//! A walked directory tree as a searchable index: which files are under a
//! root, and which folder holds each one.
//!
//! # Design
//!
//! A segment wraps two ordinary [`Snapshot`]s (file names and relative folder
//! paths) plus a run table joining them, so `search::matcher` and its parity
//! tests (`tests/matcher_parity.rs`) are unchanged and folder names are
//! matched by the same code with the same fold rules.
//!
//! A walk delivers one whole directory at a time, so each directory's files
//! are contiguous. One `(first_file, dir)` run per directory is therefore
//! exact and far smaller than a per-file owner index; it is only consulted
//! for displayed rows.

use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;

use super::Snapshot;
use super::builder::SnapshotBuilder;
use super::walk::TreeSink;
use crate::config::{SEGMENT_MAX_BYTES, SEGMENT_MIN_BYTES};

/// Where one directory's files begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    /// Index of this directory's first file within the segment.
    first_file: u32,
    /// Index of the directory within the segment's directory snapshot.
    dir: u32,
}

impl Run {
    pub(crate) fn new(first_file: u32, dir: u32) -> Self {
        Self { first_file, dir }
    }

    pub(crate) fn first_file(self) -> u32 {
        self.first_file
    }

    pub(crate) fn dir(self) -> u32 {
        self.dir
    }
}

/// A contiguous slice of a walked tree.
///
/// Sealed and published as the walk proceeds, so results appear before the
/// whole share has been read.
#[derive(Debug)]
pub struct TreeSegment {
    /// File names. The prefix is empty: paths are composed from the root and
    /// the owning directory.
    files: Snapshot,
    /// Relative directory paths, `\`-separated, with the root as `""`.
    dirs: Snapshot,
    /// Ascending by both fields. Only directories with at least one file
    /// appear; an empty folder is in `dirs` but owns no files.
    runs: Box<[Run]>,
}

impl TreeSegment {
    /// Assembles a segment from decoded parts (the persisted index),
    /// validating with [`Self::check_invariants`].
    pub(crate) fn from_parts(
        files: Snapshot,
        dirs: Snapshot,
        runs: Box<[Run]>,
    ) -> Result<Self, &'static str> {
        let seg = Self { files, dirs, runs };
        seg.check_invariants()?;
        Ok(seg)
    }

    /// The run table, for the persistence layer. Other readers use
    /// [`Self::dir_of`] or [`Self::files_of`].
    pub(crate) fn runs(&self) -> &[Run] {
        &self.runs
    }

    pub fn files(&self) -> &Snapshot {
        &self.files
    }

    pub fn dirs(&self) -> &Snapshot {
        &self.dirs
    }

    pub fn file_count(&self) -> u32 {
        self.files.len() as u32
    }

    pub fn dir_count(&self) -> u32 {
        self.dirs.len() as u32
    }

    /// Returns the directory that file `i` belongs to.
    ///
    /// `None` only when `i` is out of range.
    pub fn dir_of(&self, i: u32) -> Option<u32> {
        if i >= self.file_count() {
            return None;
        }
        // The last run starting at or before `i` is the owning directory.
        let at = self.runs.partition_point(|r| r.first_file <= i);
        self.runs.get(at.checked_sub(1)?).map(|r| r.dir)
    }

    /// Returns the half-open range of file indices owned by directory `d`.
    ///
    /// Empty when the directory holds no files.
    pub fn files_of(&self, d: u32) -> std::ops::Range<u32> {
        let Ok(at) = self.runs.binary_search_by_key(&d, |r| r.dir) else {
            return 0..0;
        };
        let start = self.runs[at].first_file;
        let end = self
            .runs
            .get(at + 1)
            .map_or(self.file_count(), |r| r.first_file);
        start..end
    }

    /// Returns the directory path of `d`, relative to the root.
    pub fn dir_path(&self, d: u32) -> std::borrow::Cow<'_, str> {
        self.dirs.display_name(d)
    }

    pub fn memory_bytes(&self) -> usize {
        self.files.memory_bytes() + self.dirs.memory_bytes() + std::mem::size_of_val(&*self.runs)
    }

    /// Checks the segment's structural invariants.
    ///
    /// O(directories), so it runs on every sealed segment. A wrong run would
    /// not crash but would silently report files under the wrong folder.
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        self.files.check_invariants()?;
        self.dirs.check_invariants()?;

        if self.runs.is_empty() {
            return if self.file_count() == 0 {
                Ok(())
            } else {
                Err("files with no directory to belong to")
            };
        }
        if self.runs[0].first_file != 0 {
            return Err("the first run must start at the first file");
        }
        for w in self.runs.windows(2) {
            if w[1].first_file <= w[0].first_file {
                return Err("runs must start strictly after one another");
            }
            if w[1].dir <= w[0].dir {
                return Err("runs must be ascending by directory");
            }
        }
        if self
            .runs
            .last()
            .is_some_and(|r| r.first_file >= self.file_count())
        {
            return Err("a run starts past the end of the files");
        }
        if self.runs.iter().any(|r| r.dir >= self.dir_count()) {
            return Err("a run names a directory that does not exist");
        }
        Ok(())
    }
}

/// One mapping's whole index, as an ordered list of sealed segments.
///
/// Cheap to clone: publishing one more segment copies a vector of `Arc`s, not
/// the data, so publishing during a walk stays cheap.
#[derive(Debug, Clone)]
pub struct TreeIndex {
    root: Arc<str>,
    segments: Vec<Arc<TreeSegment>>,
    /// Prefix sums of segment file counts, `segments.len() + 1` long. Maps a
    /// global ordinal to `(segment, local)` with one `partition_point`, so
    /// the ranking key keeps its 32-bit index field.
    bases: Vec<u32>,
    dirs: usize,
    captured_at: SystemTime,
    volume_serial: u32,
    /// The walk reached the end of the tree. A partial index is still served,
    /// but the UI must say it is partial.
    complete: bool,
    /// Every directory here was seen through a search rather than read.
    ///
    /// True only for a share configured `kind = "live"`, which is never
    /// walked, so an absent name says nothing about the share. Stronger than
    /// not [`Self::complete`], and shown differently. Live shares are never
    /// walked and walked shares are never queried live, so this is one bit for
    /// the whole index.
    observed: bool,
}

impl TreeIndex {
    pub fn empty(root: &str) -> Self {
        Self {
            root: Arc::from(root),
            segments: Vec::new(),
            bases: vec![0],
            dirs: 0,
            captured_at: SystemTime::UNIX_EPOCH,
            volume_serial: 0,
            complete: false,
            observed: false,
        }
    }

    /// Returns this index plus one more segment.
    ///
    /// Does not mutate, because publication swaps an immutable whole via
    /// `ArcSwap` and existing readers must keep their view.
    pub fn appended(&self, segment: Arc<TreeSegment>) -> Self {
        let mut next = self.clone();
        let base = next.bases.last().copied().unwrap_or(0);
        next.bases.push(base + segment.file_count());
        next.dirs += segment.dir_count() as usize;
        next.segments.push(segment);
        next
    }

    pub fn with_metadata(mut self, captured_at: SystemTime, volume_serial: u32) -> Self {
        self.captured_at = captured_at;
        self.volume_serial = volume_serial;
        self
    }

    pub fn with_complete(mut self, complete: bool) -> Self {
        self.complete = complete;
        self
    }

    pub fn with_observed(mut self, observed: bool) -> Self {
        self.observed = observed;
        self
    }

    /// Returns true when this index holds only what searches have found.
    pub fn observed(&self) -> bool {
        self.observed
    }

    /// Returns whether this index already holds `file` in `rel`.
    ///
    /// Linear in the directory count; called once per directory a search
    /// touched, on an index that holds only searched-for names.
    pub fn holds(&self, rel: &str, file: &str) -> bool {
        self.segments.iter().any(|seg| {
            (0..seg.dir_count()).any(|d| {
                seg.dir_path(d) == rel
                    && seg
                        .files_of(d)
                        .any(|i| seg.files().display_name(i).eq_ignore_ascii_case(file))
            })
        })
    }

    /// Returns this index plus files a search saw, skipping those already
    /// held.
    ///
    /// Appends and never replaces, unlike [`Self::with_subtrees_replaced`]: a
    /// search result names only the files that matched, so replacing a folder
    /// would drop the files that did not.
    ///
    /// `None` when nothing was new, so a repeated search causes no
    /// publication. `captured_at` is unchanged, as in `confirm_fresh`.
    pub fn with_observed_files(&self, seen: &[(String, Vec<String>)]) -> Option<Self> {
        let mut builder = SegmentBuilder::new();
        let mut added = false;
        for (rel, files) in seen {
            let fresh: Vec<String> = files
                .iter()
                .filter(|f| !self.holds(rel, f))
                .cloned()
                .collect();
            if fresh.is_empty() {
                continue;
            }
            if !builder.push_dir(rel, &fresh) {
                break;
            }
            added = true;
        }
        if !added {
            return None;
        }
        let mut next = self.appended(Arc::new(builder.seal()));
        next.observed = true;
        next.complete = false;
        next.segments = coalesce(std::mem::take(&mut next.segments));
        next.rebuild_bases();
        Some(next)
    }

    /// Recomputes the prefix sums after the segment list has been rewritten.
    fn rebuild_bases(&mut self) {
        self.bases = Vec::with_capacity(self.segments.len() + 1);
        self.bases.push(0);
        let mut running = 0u32;
        self.dirs = 0;
        for seg in &self.segments {
            running += seg.file_count();
            self.dirs += seg.dir_count() as usize;
            self.bases.push(running);
        }
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn segments(&self) -> &[Arc<TreeSegment>] {
        &self.segments
    }

    /// Returns the number of files across every segment.
    pub fn len(&self) -> usize {
        self.bases.last().copied().unwrap_or(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dir_count(&self) -> usize {
        self.dirs
    }

    pub fn captured_at(&self) -> SystemTime {
        self.captured_at
    }

    pub fn volume_serial(&self) -> u32 {
        self.volume_serial
    }

    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Returns the first global ordinal of a segment.
    pub fn base(&self, segment: usize) -> u32 {
        self.bases.get(segment).copied().unwrap_or(0)
    }

    /// Splits a global ordinal into the segment holding it and its index
    /// within that segment.
    pub fn locate(&self, ordinal: u32) -> Option<(usize, u32)> {
        if ordinal >= self.len() as u32 {
            return None;
        }
        // `bases` is ascending from zero, so the segment is the last base at
        // or below the ordinal.
        let at = self
            .bases
            .partition_point(|&b| b <= ordinal)
            .checked_sub(1)?;
        Some((at, ordinal - self.bases[at]))
    }

    /// Returns the full path of a file, by global ordinal.
    ///
    /// The only place a tree path is assembled. A file directly in the root
    /// has an empty directory component and must not get a doubled separator.
    pub fn full_path(&self, ordinal: u32) -> Option<String> {
        let (s, local) = self.locate(ordinal)?;
        let segment = &self.segments[s];
        let dir = segment.dir_of(local)?;
        let rel_dir = segment.dir_path(dir);
        let name = segment.files.display_name(local);

        let mut out = String::with_capacity(self.root.len() + rel_dir.len() + name.len() + 2);
        out.push_str(&self.root);
        if !out.ends_with('\\') {
            out.push('\\');
        }
        if !rel_dir.is_empty() {
            out.push_str(&rel_dir);
            out.push('\\');
        }
        out.push_str(&name);
        Some(out)
    }

    pub fn memory_bytes(&self) -> usize {
        self.segments
            .iter()
            .map(|s| s.memory_bytes())
            .sum::<usize>()
            + self.bases.len() * std::mem::size_of::<u32>()
    }
}

/// Accumulates one segment's worth of a walk.
///
/// Single-threaded; the owning sink serialises access.
pub struct SegmentBuilder {
    files: SnapshotBuilder,
    dirs: SnapshotBuilder,
    runs: Vec<Run>,
}

impl Default for SegmentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SegmentBuilder {
    pub fn new() -> Self {
        Self {
            // Empty prefixes: tree paths are composed from the root and the
            // owning directory.
            files: SnapshotBuilder::new(""),
            dirs: SnapshotBuilder::new(""),
            runs: Vec::new(),
        }
    }

    pub fn file_count(&self) -> u32 {
        self.files.len() as u32
    }

    pub fn dir_count(&self) -> u32 {
        self.dirs.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.len() == 0
    }

    /// Returns the bytes held so far, for deciding when to seal.
    pub fn arena_bytes(&self) -> usize {
        self.files.arena_bytes() + self.dirs.arena_bytes()
    }

    /// Adds one completed directory and its files.
    ///
    /// Returns false when an arena is full. `first_file` is read before any
    /// name is added, but the run is pushed only once a file has landed: a run
    /// recorded before a failed push would start past the end of the segment,
    /// and `dir_of` would attribute the next directory's files to this one.
    pub fn push_dir(&mut self, rel: &str, files: &[String]) -> bool {
        let dir = self.dirs.len() as u32;
        if !self.dirs.push_str(rel) {
            return false;
        }
        let first_file = self.files.len() as u32;
        let mut pushed = 0usize;
        let mut full = false;
        for name in files {
            if !self.files.push_str(name) {
                full = true;
                break;
            }
            pushed += 1;
        }
        if pushed > 0 {
            self.runs.push(Run { first_file, dir });
        }
        !full
    }

    /// Copies one directory and its files verbatim from `seg`.
    ///
    /// Copies raw bytes instead of re-folding, so a rebuild cannot change how
    /// an existing name is folded and make it unmatchable.
    pub(crate) fn push_dir_raw(&mut self, seg: &TreeSegment, d: u32) -> bool {
        let dir = self.dirs.len() as u32;
        if !self
            .dirs
            .push_raw(seg.dirs.name_orig(d), seg.dirs.name_lower(d))
        {
            return false;
        }
        let first_file = self.files.len() as u32;
        let mut pushed = 0usize;
        let mut full = false;
        for i in seg.files_of(d) {
            if !self
                .files
                .push_raw(seg.files.name_orig(i), seg.files.name_lower(i))
            {
                full = true;
                break;
            }
            pushed += 1;
        }
        if pushed > 0 {
            self.runs.push(Run { first_file, dir });
        }
        !full
    }

    /// Seals the segment. The builder is left empty and reusable.
    pub fn seal(&mut self) -> TreeSegment {
        let files = std::mem::replace(&mut self.files, SnapshotBuilder::new(""));
        let dirs = std::mem::replace(&mut self.dirs, SnapshotBuilder::new(""));
        TreeSegment {
            // Timestamps and serial belong to the whole index; a segment
            // carries placeholders.
            files: files.finish(SystemTime::UNIX_EPOCH, 0, None),
            dirs: dirs.finish(SystemTime::UNIX_EPOCH, 0, None),
            runs: std::mem::take(&mut self.runs).into_boxed_slice(),
        }
    }
}

/// Returns the directory part of a relative path.
///
/// `ReadDirectoryChangesW` reports the changed file, while the index stores
/// directory listings; the walk and the watcher both use this function so
/// they agree on one string format.
pub fn parent_rel(rel: &str) -> &str {
    rel.rsplit_once('\\').map_or("", |(parent, _)| parent)
}

/// Builds a [`TreeIndex`] from a walk, sealing segments as it goes.
///
/// # Locking
///
/// `TreeSink::push_dir` takes `&self` and is called from every walk worker.
/// This does not break the store's single-writer rule because `walk_tree`
/// uses `std::thread::scope`: the actor thread is blocked inside one step
/// until every worker joins, and one mutex serialises the workers. The walk
/// must therefore run on the actor's own thread, not be supervised from it.
///
/// Contention is negligible: appending a directory's names takes well under a
/// microsecond, against about a millisecond of SMB to read it.
pub struct SegmentSink {
    staging: Mutex<Staging>,
    root: Box<str>,
}

struct Staging {
    builder: SegmentBuilder,
    sealed: Vec<Arc<TreeSegment>>,
    /// Arena bytes at which the current segment is sealed. Doubles each time,
    /// so first results arrive quickly and the segment count stays bounded.
    target: usize,
    full: bool,
}

impl SegmentSink {
    pub fn new(root: &str) -> Self {
        Self {
            staging: Mutex::new(Staging {
                builder: SegmentBuilder::new(),
                sealed: Vec::new(),
                target: SEGMENT_MIN_BYTES,
                full: false,
            }),
            root: root.into(),
        }
    }

    /// Returns the index built so far, including the unsealed remainder.
    ///
    /// Callable mid-walk, so a partially walked share can be searched.
    pub fn index(&self) -> TreeIndex {
        let mut s = self.staging.lock();
        // Seal the remainder: only segments are searchable, and the most
        // recently walked folders would otherwise never appear.
        let tail = (!s.builder.is_empty()).then(|| Arc::new(s.builder.seal()));
        if let Some(tail) = tail {
            s.sealed.push(tail);
        }
        s.sealed
            .iter()
            .fold(TreeIndex::empty(&self.root), |ix, seg| {
                ix.appended(Arc::clone(seg))
            })
    }

    /// Returns the number of segments sealed so far. Test and diagnostic use.
    pub fn segment_count(&self) -> usize {
        self.staging.lock().sealed.len()
    }
}

impl TreeSink for SegmentSink {
    fn push_dir(&self, rel: &str, files: &[String]) -> bool {
        let mut s = self.staging.lock();
        if s.full {
            return false;
        }
        if !s.builder.push_dir(rel, files) {
            // Arena ceiling reached. Stop the walk rather than read
            // directories that cannot be stored.
            s.full = true;
            return false;
        }
        if s.builder.arena_bytes() >= s.target {
            let segment = Arc::new(s.builder.seal());
            debug_assert!(segment.check_invariants().is_ok(), "sealed a bad segment");
            s.sealed.push(segment);
            s.target = (s.target * 2).min(SEGMENT_MAX_BYTES);
        }
        true
    }
}
// --- incremental update ----------------------------------------------------

/// What was left of a segment after dropping some of its directories.
enum Retained {
    /// Nothing was dropped, so the original can be shared by refcount.
    All,
    /// Everything was dropped.
    None,
    /// Boxed to keep the enum small, since `All` is the common case.
    Some(Box<TreeSegment>),
}

impl TreeSegment {
    /// Returns the bytes in this segment's arenas, counted as
    /// [`SegmentBuilder::arena_bytes`] counts them.
    pub fn arena_bytes(&self) -> usize {
        self.files.lower().len() + self.dirs.lower().len()
    }

    /// Returns this segment without the directories `drop` selects.
    ///
    /// Returns [`Retained::All`] rather than a copy when nothing matched, so a
    /// change to one folder does not rebuild unrelated segments.
    fn retaining(&self, drop: &dyn Fn(&str) -> bool) -> Retained {
        let dropped = (0..self.dir_count())
            .filter(|d| drop(&self.dir_path(*d)))
            .count() as u32;
        if dropped == 0 {
            return Retained::All;
        }
        if dropped == self.dir_count() {
            return Retained::None;
        }

        let mut b = SegmentBuilder::new();
        for d in 0..self.dir_count() {
            if drop(&self.dir_path(d)) {
                continue;
            }
            if !b.push_dir_raw(self, d) {
                // Arena ceiling while shrinking a segment that already fit.
                // Not expected; keep the original rather than publish a partial
                // copy. The next full walk collects the deletion.
                return Retained::All;
            }
        }
        Retained::Some(Box::new(b.seal()))
    }
}

impl TreeIndex {
    /// Assembles an index from segments that are already sealed.
    fn from_segments(root: Arc<str>, segments: Vec<Arc<TreeSegment>>) -> Self {
        let mut bases = Vec::with_capacity(segments.len() + 1);
        bases.push(0u32);
        let mut dirs = 0usize;
        for seg in &segments {
            let base = bases.last().copied().unwrap_or(0);
            bases.push(base + seg.file_count());
            dirs += seg.dir_count() as usize;
        }
        Self {
            root,
            segments,
            bases,
            dirs,
            captured_at: SystemTime::UNIX_EPOCH,
            volume_serial: 0,
            complete: false,
            observed: false,
        }
    }

    /// Returns this index with the subtrees named by `replaced` rebuilt from
    /// `fresh`.
    ///
    /// Drops every directory that is in `replaced` or beneath one, then
    /// appends everything in `fresh`. Whole subtrees are replaced because a
    /// new folder arrives as one notification on its parent.
    ///
    /// `captured_at` is unchanged: only some folders are newer, and advancing
    /// it would claim the whole index was verified current.
    pub fn with_subtrees_replaced(&self, replaced: &[String], fresh: &TreeIndex) -> Self {
        let covered = |rel: &str| {
            replaced
                .iter()
                .any(|r| crate::index::walk::is_within(rel, r))
        };

        let mut segments: Vec<Arc<TreeSegment>> = Vec::with_capacity(self.segments.len() + 1);
        for seg in &self.segments {
            match seg.retaining(&covered) {
                Retained::All => segments.push(Arc::clone(seg)),
                Retained::None => {}
                Retained::Some(rebuilt) => segments.push(Arc::new(*rebuilt)),
            }
        }
        segments.extend(fresh.segments.iter().map(Arc::clone));

        Self::from_segments(Arc::clone(&self.root), coalesce(segments))
            .with_metadata(self.captured_at, self.volume_serial)
            .with_complete(self.complete)
            .with_observed(self.observed)
    }
}

/// Merges adjacent segments that fit together within one segment's budget.
///
/// Without this, every update appends a segment and the count grows between
/// full walks, each segment adding a sweep per keystroke. Update segments are
/// small and land at the end, so they merge into one growing tail;
/// full-size segments never pair. Work per update is bounded by one segment's
/// worth of copying.
fn coalesce(segments: Vec<Arc<TreeSegment>>) -> Vec<Arc<TreeSegment>> {
    let mut out: Vec<Arc<TreeSegment>> = Vec::with_capacity(segments.len());
    for seg in segments {
        let fits = out
            .last()
            .is_some_and(|prev| prev.arena_bytes() + seg.arena_bytes() <= SEGMENT_MAX_BYTES);
        if fits && let Some(merged) = merge(out.last().expect("checked by fits"), &seg) {
            out.pop();
            out.push(Arc::new(merged));
            continue;
        }
        out.push(seg);
    }
    out
}

/// Concatenates two segments, or `None` if the arenas will not take it.
fn merge(a: &TreeSegment, b: &TreeSegment) -> Option<TreeSegment> {
    let mut out = SegmentBuilder::new();
    for seg in [a, b] {
        for d in 0..seg.dir_count() {
            if !out.push_dir_raw(seg, d) {
                return None;
            }
        }
    }
    Some(out.seal())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(dirs: &[(&str, &[&str])]) -> TreeSegment {
        let mut b = SegmentBuilder::new();
        for (rel, files) in dirs {
            let owned: Vec<String> = files.iter().map(|f| f.to_string()).collect();
            assert!(b.push_dir(rel, &owned));
        }
        let s = b.seal();
        s.check_invariants()
            .expect("a built segment is well formed");
        s
    }

    fn sample() -> TreeSegment {
        seg(&[
            ("", &["loose.pdf"]),
            ("abc", &["notes.txt"]),
            ("abc\\0704", &["quote.pdf", "photo.pdf"]),
            ("empty", &[]),
            ("abd", &["spec.pdf"]),
        ])
    }

    // --- remembering what a search found ------------------------------------

    fn observed(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(d, fs)| {
                (
                    d.to_string(),
                    fs.iter().map(|f| f.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    fn files_in(ix: &TreeIndex, rel: &str) -> Vec<String> {
        let mut out = Vec::new();
        for seg in ix.segments() {
            for d in 0..seg.dir_count() {
                if seg.dir_path(d) == rel {
                    for i in seg.files_of(d) {
                        out.push(seg.files().display_name(i).into_owned());
                    }
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn an_empty_index_takes_what_a_search_found() {
        let ix = TreeIndex::empty("V:\\archive");
        let next = ix
            .with_observed_files(&observed(&[("p12345", &["a.pdf", "b.tif"])]))
            .expect("something was new");
        assert_eq!(next.len(), 2);
        assert!(next.observed());
        assert!(!next.complete(), "an observed index is never complete");
        assert_eq!(files_in(&next, "p12345"), ["a.pdf", "b.tif"]);
    }

    /// A repeated search must not publish a new index.
    #[test]
    fn a_search_that_found_only_what_was_already_held_changes_nothing() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("p12345", &["a.pdf"])]))
            .unwrap();
        assert!(
            ix.with_observed_files(&observed(&[("p12345", &["a.pdf"])]))
                .is_none()
        );
    }

    #[test]
    fn a_name_already_held_is_not_stored_twice_beside_a_new_one() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("p12345", &["a.pdf"])]))
            .unwrap();
        let next = ix
            .with_observed_files(&observed(&[("p12345", &["a.pdf", "b.pdf"])]))
            .expect("b.pdf was new");
        assert_eq!(files_in(&next, "p12345"), ["a.pdf", "b.pdf"]);
        assert_eq!(next.len(), 2, "a.pdf was stored twice");
    }

    /// A search result says nothing about files that did not match, so a
    /// second search must not drop what the first found.
    #[test]
    fn remembering_a_second_search_never_drops_what_the_first_one_found() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("projects", &["0704.pdf"])]))
            .unwrap();
        let next = ix
            .with_observed_files(&observed(&[("projects", &["0801.pdf"])]))
            .unwrap();
        assert_eq!(files_in(&next, "projects"), ["0704.pdf", "0801.pdf"]);
    }

    #[test]
    fn a_name_is_matched_for_holding_however_it_is_cased() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("projects", &["A.PDF"])]))
            .unwrap();
        assert!(ix.holds("projects", "a.pdf"));
        assert!(!ix.holds("other", "a.pdf"), "the folder has to match too");
    }

    /// Coalescing keeps the segment count bounded across many observations.
    #[test]
    fn many_small_observations_do_not_grow_the_segment_count_without_bound() {
        let mut ix = TreeIndex::empty("V:\\archive");
        for i in 0..40 {
            ix = ix
                .with_observed_files(&observed(&[("projects", &[&format!("f{i:03}.pdf")])]))
                .expect("each name is new");
        }
        assert_eq!(ix.len(), 40);
        assert!(
            ix.segments().len() <= 2,
            "{} segments for 40 observations",
            ix.segments().len()
        );
    }

    #[test]
    fn the_ordinals_still_address_every_file_after_coalescing() {
        let mut ix = TreeIndex::empty("V:\\archive");
        for i in 0..8 {
            ix = ix
                .with_observed_files(&observed(&[(&format!("d{i}"), &[&format!("f{i}.pdf")])]))
                .unwrap();
        }
        for ordinal in 0..ix.len() as u32 {
            assert!(
                ix.full_path(ordinal).is_some(),
                "ordinal {ordinal} of {} does not resolve",
                ix.len()
            );
        }
    }

    #[test]
    fn every_file_knows_which_directory_it_is_in() {
        let s = sample();
        assert_eq!(s.file_count(), 5);
        assert_eq!(s.dir_of(0), Some(0), "loose.pdf is in the root");
        assert_eq!(s.dir_of(1), Some(1), "notes.txt is in abc");
        assert_eq!(s.dir_of(2), Some(2));
        assert_eq!(s.dir_of(3), Some(2), "both of 0704's files");
        assert_eq!(s.dir_of(4), Some(4), "abd, skipping the empty folder");
        assert_eq!(s.dir_of(5), None);
    }

    #[test]
    fn a_directory_expands_to_exactly_its_own_files() {
        let s = sample();
        assert_eq!(s.files_of(0), 0..1);
        assert_eq!(s.files_of(1), 1..2);
        assert_eq!(s.files_of(2), 2..4, "0704 holds two");
        assert_eq!(s.files_of(4), 4..5);
    }

    /// An empty folder is still matchable by name, and expands to nothing
    /// rather than to the next directory's files.
    #[test]
    fn an_empty_directory_owns_no_files() {
        let s = sample();
        assert_eq!(s.dir_path(3), "empty");
        assert_eq!(s.files_of(3), 0..0);
    }

    #[test]
    fn a_segment_with_no_files_at_all_is_well_formed() {
        let s = seg(&[("", &[]), ("a", &[])]);
        assert_eq!(s.file_count(), 0);
        assert_eq!(s.dir_count(), 2);
        assert_eq!(s.dir_of(0), None);
    }

    #[test]
    fn paths_are_composed_from_the_root_and_the_directory() {
        let index = TreeIndex::empty("R:\\").appended(Arc::new(sample()));
        assert_eq!(index.full_path(0).unwrap(), "R:\\loose.pdf");
        assert_eq!(index.full_path(1).unwrap(), "R:\\abc\\notes.txt");
        assert_eq!(index.full_path(2).unwrap(), "R:\\abc\\0704\\quote.pdf");
        assert_eq!(index.full_path(4).unwrap(), "R:\\abd\\spec.pdf");
        assert_eq!(index.full_path(5), None);
    }

    /// A root without a trailing separator gains exactly one.
    #[test]
    fn a_root_without_a_trailing_separator_still_composes_cleanly() {
        let index = TreeIndex::empty("R:\\Projects").appended(Arc::new(sample()));
        assert_eq!(index.full_path(1).unwrap(), "R:\\Projects\\abc\\notes.txt");
    }

    #[test]
    fn ordinals_span_segments_in_order() {
        let index = TreeIndex::empty("R:\\")
            .appended(Arc::new(seg(&[("a", &["one.pdf", "two.pdf"])])))
            .appended(Arc::new(seg(&[("b", &["three.pdf"])])));

        assert_eq!(index.len(), 3);
        assert_eq!(index.dir_count(), 2);
        assert_eq!(index.locate(0), Some((0, 0)));
        assert_eq!(index.locate(1), Some((0, 1)));
        assert_eq!(index.locate(2), Some((1, 0)), "over the seam");
        assert_eq!(index.locate(3), None);
        assert_eq!(index.full_path(2).unwrap(), "R:\\b\\three.pdf");
    }

    /// Appending shares the existing segments rather than copying them.
    #[test]
    fn appending_leaves_the_previous_index_untouched() {
        let first = TreeIndex::empty("R:\\").appended(Arc::new(seg(&[("a", &["one.pdf"])])));
        let second = first.appended(Arc::new(seg(&[("b", &["two.pdf"])])));

        assert_eq!(first.len(), 1, "the earlier value is unchanged");
        assert_eq!(second.len(), 2);
        assert!(
            Arc::ptr_eq(&first.segments()[0], &second.segments()[0]),
            "the shared segment is shared, not cloned"
        );
    }

    #[test]
    fn an_empty_index_is_coherent() {
        let index = TreeIndex::empty("R:\\");
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.locate(0), None);
        assert_eq!(index.full_path(0), None);
    }

    #[test]
    fn a_sealed_builder_can_be_used_again() {
        let mut b = SegmentBuilder::new();
        assert!(b.push_dir("a", &["one.pdf".into()]));
        let first = b.seal();
        assert!(b.is_empty(), "sealing leaves it ready for the next segment");
        assert!(b.push_dir("b", &["two.pdf".into()]));
        let second = b.seal();

        assert_eq!(first.file_count(), 1);
        assert_eq!(second.file_count(), 1);
        assert_eq!(second.dir_path(0), "b");
        second.check_invariants().unwrap();
    }

    #[test]
    fn the_parent_of_a_relative_path_is_its_directory() {
        assert_eq!(parent_rel("abc\\0704\\quote.pdf"), "abc\\0704");
        assert_eq!(parent_rel("loose.pdf"), "", "a file in the root");
        assert_eq!(parent_rel(""), "");
    }
}

/// Replacing subtrees in place for incremental updates.
#[cfg(test)]
mod apply_tests {
    use std::time::Duration;

    use super::*;
    use crate::index::builder::MAX_NAME_BYTES;

    const ROOT: &str = "R:\\";

    /// Builds one segment per group, so tests control which segment holds a
    /// directory.
    fn tree_of(groups: &[&[(&str, &[&str])]]) -> TreeIndex {
        let mut index = TreeIndex::empty(ROOT);
        for group in groups {
            let mut b = SegmentBuilder::new();
            for (dir, files) in *group {
                let owned: Vec<String> = files.iter().map(|f| (*f).to_string()).collect();
                assert!(b.push_dir(dir, &owned));
            }
            index = index.appended(Arc::new(b.seal()));
        }
        index
            .with_metadata(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000), 7)
            .with_complete(true)
    }

    fn paths(index: &TreeIndex) -> Vec<String> {
        let mut out: Vec<String> = (0..index.len() as u32)
            .map(|i| index.full_path(i).expect("every ordinal has a path"))
            .collect();
        out.sort();
        out
    }

    fn base() -> TreeIndex {
        tree_of(&[
            &[
                ("", &["readme.txt"] as &[&str]),
                ("abc", &["quote.pdf", "old.pdf"]),
            ],
            &[
                ("abc\\0704", &["photo.tif"]),
                ("abd", &["spec.pdf"]),
                ("abdx", &["decoy.pdf"]),
            ],
        ])
    }

    #[test]
    fn a_replaced_subtree_takes_the_fresh_contents() {
        let fresh = tree_of(&[&[
            ("abc", &["quote.pdf", "new.pdf"] as &[&str]),
            ("abc\\0704", &["photo.tif", "revised.tif"]),
        ]]);
        let after = base().with_subtrees_replaced(&["abc".to_string()], &fresh);

        assert_eq!(
            paths(&after),
            vec![
                "R:\\abc\\0704\\photo.tif",
                "R:\\abc\\0704\\revised.tif",
                "R:\\abc\\new.pdf",
                "R:\\abc\\quote.pdf",
                "R:\\abd\\spec.pdf",
                "R:\\abdx\\decoy.pdf",
                "R:\\readme.txt",
            ]
        );
    }

    /// A new folder arrives as one notification on its parent, so replacing
    /// the parent's subtree must pick it up.
    #[test]
    fn a_folder_created_under_a_dirty_parent_is_picked_up() {
        let fresh = tree_of(&[&[
            ("abc", &["quote.pdf", "old.pdf"] as &[&str]),
            ("abc\\0704", &["photo.tif"]),
            ("abc\\0999", &["brand new.pdf"]),
        ]]);
        let after = base().with_subtrees_replaced(&["abc".to_string()], &fresh);
        assert!(paths(&after).contains(&"R:\\abc\\0999\\brand new.pdf".to_string()));
    }

    #[test]
    fn a_deleted_folder_leaves_the_index() {
        let fresh = tree_of(&[&[("abc", &["quote.pdf"] as &[&str])]]);
        let after = base().with_subtrees_replaced(&["abc".to_string()], &fresh);

        assert!(!paths(&after).iter().any(|p| p.contains("0704")));
        assert!(!paths(&after).iter().any(|p| p.contains("old.pdf")));
        assert!(paths(&after).contains(&"R:\\abd\\spec.pdf".to_string()));
    }

    /// A subtree that is gone entirely is replaced by nothing.
    #[test]
    fn replacing_a_subtree_with_nothing_removes_it() {
        let after = base().with_subtrees_replaced(&["abd".to_string()], &TreeIndex::empty(ROOT));
        let got = paths(&after);
        assert!(!got.iter().any(|p| p.contains("\\abd\\")));
        assert!(
            got.contains(&"R:\\abdx\\decoy.pdf".to_string()),
            "a prefix match is not a subtree: {got:?}"
        );
    }

    /// The separator check. Without it `abd` would swallow `abdx`.
    #[test]
    fn a_name_that_merely_starts_the_same_is_not_within_the_subtree() {
        assert!(crate::index::walk::is_within("abd\\sub", "abd"));
        assert!(crate::index::walk::is_within("abd", "abd"));
        assert!(!crate::index::walk::is_within("abdx", "abd"));
        assert!(crate::index::walk::is_within("anything", ""));
    }

    /// A dirty root is a full replacement.
    #[test]
    fn replacing_the_root_replaces_everything() {
        let fresh = tree_of(&[&[("", &["only.txt"] as &[&str])]]);
        let after = base().with_subtrees_replaced(&[String::new()], &fresh);
        assert_eq!(paths(&after), vec!["R:\\only.txt"]);
    }

    /// A change to one folder must not rebuild the segments it is not in.
    #[test]
    fn untouched_segments_are_shared_rather_than_copied() {
        let before = base();
        let untouched = Arc::clone(&before.segments()[1]);
        let fresh = tree_of(&[&[("abc", &["quote.pdf"] as &[&str])]]);

        let after = before.with_subtrees_replaced(&["abc\\0704".to_string()], &fresh);
        assert!(
            after
                .segments()
                .iter()
                .any(|s| Arc::ptr_eq(s, &untouched) || Arc::strong_count(&untouched) > 1),
            "the segment holding no dirty directory should have been reused"
        );
    }

    /// Refreshing some folders does not advance the index's metadata.
    #[test]
    fn an_incremental_update_does_not_claim_the_index_is_newer() {
        let before = base();
        let fresh = tree_of(&[&[("abc", &["quote.pdf"] as &[&str])]]);
        let after = before.with_subtrees_replaced(&["abc".to_string()], &fresh);

        assert_eq!(after.captured_at(), before.captured_at());
        assert_eq!(after.volume_serial(), before.volume_serial());
        assert_eq!(after.complete(), before.complete());
        assert_eq!(after.root(), before.root());
    }

    #[test]
    fn the_directory_count_follows_the_update() {
        let before = base();
        assert_eq!(before.dir_count(), 5);
        let after = before.with_subtrees_replaced(&["abc".to_string()], &TreeIndex::empty(ROOT));
        assert_eq!(after.dir_count(), 3, "`abc` and `abc\\0704` both left");
    }

    /// Repeated updates do not grow the segment count without bound.
    #[test]
    fn repeated_updates_do_not_grow_the_segment_count_without_bound() {
        let mut index = base();
        for i in 0..50 {
            let fresh = tree_of(&[&[(
                "abd",
                &[Box::leak(format!("v{i}.pdf").into_boxed_str()) as &str] as &[&str],
            )]]);
            index = index.with_subtrees_replaced(&["abd".to_string()], &fresh);
        }
        assert!(
            index.segments().len() <= 2,
            "fifty updates left {} segments",
            index.segments().len()
        );
        assert_eq!(
            paths(&index).iter().filter(|p| p.contains("abd\\")).count(),
            1,
            "each update should supersede the last, not stack on it"
        );
    }

    /// Coalescing must not disturb what the index reports.
    #[test]
    fn coalescing_preserves_every_path_and_its_folder() {
        let before = base();
        let fresh = tree_of(&[&[("abd", &["spec.pdf", "extra.pdf"] as &[&str])]]);
        let after = before.with_subtrees_replaced(&["abd".to_string()], &fresh);

        for seg in after.segments() {
            seg.check_invariants()
                .expect("a coalesced segment is sound");
        }
        assert!(paths(&after).contains(&"R:\\abd\\extra.pdf".to_string()));
        assert!(paths(&after).contains(&"R:\\abc\\0704\\photo.tif".to_string()));
    }

    /// Replacing nothing is a no-op. The actor reaches this when a batch names
    /// only unreadable folders.
    #[test]
    fn replacing_nothing_changes_nothing() {
        let before = base();
        let after = before.with_subtrees_replaced(&[], &TreeIndex::empty(ROOT));
        assert_eq!(paths(&after), paths(&before));
        assert_eq!(after.dir_count(), before.dir_count());
    }

    /// A run recorded before a failed push would start past the end of the
    /// segment and misattribute files in `dir_of`.
    #[test]
    fn a_directory_whose_files_did_not_fit_leaves_a_sound_segment() {
        let mut b = SegmentBuilder::new();
        let huge = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(b.push_dir("a", &["real.pdf".to_string()]));
        assert!(!b.push_dir("b", &[huge]));
        assert!(b.seal().check_invariants().is_ok());
    }
}
