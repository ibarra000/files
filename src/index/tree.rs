//! A walked directory tree, as something searchable.
//!
//! The flat index answers "which files are in this one directory". A tree has
//! to answer "which files are anywhere under this root, and which folder is
//! each one in" - for five million files across three hundred thousand
//! folders, on a machine that also has to stay responsive while someone types.
//!
//! # Why this wraps [`Snapshot`] instead of changing it
//!
//! [`Snapshot`] is an arena of NUL-separated names with one offsets table,
//! and `search::matcher` sweeps it with a single vectorised pass. That is the
//! part of this program that has to stay fast, and it is proven against a
//! frozen copy of the original implementation in `tests/matcher_parity.rs`.
//! So the directory dimension is added *around* it: a segment holds two
//! ordinary snapshots and a small table joining them. Nothing the matcher
//! touches changes, and a flat mapping keeps exactly the code path it has now.
//!
//! # Why the folder names are a `Snapshot` too
//!
//! A job code names a folder far more often than it names a file, so "does
//! this code match a directory" is a question that has to be answered on every
//! keystroke - and it is the *same* substring question, over a different
//! arena. Storing the folders as their own snapshot means it is answered by
//! the same matcher, with the same fold rules and the same NUL guarantees,
//! rather than by a second implementation written to a lower standard because
//! it is "only for folders". At three hundred thousand directories against
//! five million files it is a 2% pass on top of the one already being made.
//!
//! # Why runs rather than an owner per file
//!
//! The walk hands over one whole directory at a time, so a segment's files
//! arrive grouped and a directory's files are always contiguous. Recording
//! `(first_file, dir)` once per directory is therefore exact, and costs 2.4 MB
//! at this scale where a `u32` owner per file would cost 20 MB - for a lookup
//! that only ever happens for the few hundred rows actually on screen.

use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;

use super::Snapshot;
use super::builder::SnapshotBuilder;
use super::walk::TreeSink;
use crate::config::{SEGMENT_MAX_BYTES, SEGMENT_MIN_BYTES};

/// Where one directory's files begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    /// Index of this directory's first file within the segment.
    first_file: u32,
    /// Index of the directory within the segment's directory snapshot.
    dir: u32,
}

/// A contiguous slice of a walked tree.
///
/// Sealed and published as the walk proceeds, so results appear about a second
/// in rather than after the whole share has been read.
#[derive(Debug)]
pub struct TreeSegment {
    /// File names. Its prefix is empty: a file's path is composed from the
    /// root and the owning directory, not carried per entry.
    files: Snapshot,
    /// The relative directory paths this segment covers, `\`-separated, with
    /// the root itself spelled as the empty string.
    dirs: Snapshot,
    /// Ascending by both fields. Only directories that hold at least one file
    /// appear; an empty folder is still in `dirs`, so it can be matched by
    /// name, but it owns no files to point at.
    runs: Box<[Run]>,
}

impl TreeSegment {
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

    /// Which directory file `i` belongs to.
    ///
    /// `None` only when `i` is out of range, which `check_invariants` makes
    /// unreachable for a segment that was built rather than decoded.
    pub fn dir_of(&self, i: u32) -> Option<u32> {
        if i >= self.file_count() {
            return None;
        }
        // The last run that starts at or before `i`. Runs are ascending, so
        // this is the directory whose range contains it.
        let at = self.runs.partition_point(|r| r.first_file <= i);
        self.runs.get(at.checked_sub(1)?).map(|r| r.dir)
    }

    /// The half-open range of file indices owned by directory `d`.
    ///
    /// Empty when the directory holds no files, which is what makes a folder
    /// match expand to nothing rather than to someone else's files.
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

    /// The directory path of `d`, relative to the root.
    pub fn dir_path(&self, d: u32) -> std::borrow::Cow<'_, str> {
        self.dirs.display_name(d)
    }

    pub fn memory_bytes(&self) -> usize {
        self.files.memory_bytes() + self.dirs.memory_bytes() + std::mem::size_of_val(&*self.runs)
    }

    /// Checks the structural promises this type makes about itself.
    ///
    /// Cheap and O(directories), so it runs on every sealed segment rather
    /// than only on decoded ones. A run pointing at the wrong directory does
    /// not crash: it reports a real file under a path it is not at, which is
    /// a wrong answer nobody can tell is wrong - the exact class of bug this
    /// index exists to remove.
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
/// Cheap to clone: publishing one more segment copies a vector of refcounts,
/// not a byte of the hundreds of megabytes behind them. That is what lets a
/// walk publish as it goes without the cost growing with what it has already
/// found.
#[derive(Debug, Clone)]
pub struct TreeIndex {
    root: Arc<str>,
    segments: Vec<Arc<TreeSegment>>,
    /// Prefix sums of segment file counts, `segments.len() + 1` long. Turns a
    /// global ordinal into `(segment, local)` with one `partition_point` over
    /// a couple of dozen `u32`s - one cache line - which is what lets the
    /// ranking key keep its 32-bit index field unchanged.
    bases: Vec<u32>,
    dirs: usize,
    captured_at: SystemTime,
    volume_serial: u32,
    /// The walk that produced this reached the end of the tree. A partial
    /// index is still worth serving, but the difference has to be visible:
    /// results missing because a subtree was unreadable look exactly like
    /// results that do not exist.
    complete: bool,
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
        }
    }

    /// This index plus one more segment.
    ///
    /// Returns a new value rather than mutating, because publication is an
    /// `ArcSwap` of an immutable whole: a reader holding the previous one must
    /// keep seeing exactly what it had.
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

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn segments(&self) -> &[Arc<TreeSegment>] {
        &self.segments
    }

    /// Files across every segment.
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

    /// First global ordinal of a segment.
    pub fn base(&self, segment: usize) -> u32 {
        self.bases.get(segment).copied().unwrap_or(0)
    }

    /// Splits a global ordinal into the segment holding it and its index
    /// within that segment.
    pub fn locate(&self, ordinal: u32) -> Option<(usize, u32)> {
        if ordinal >= self.len() as u32 {
            return None;
        }
        // `bases` is ascending and starts at zero, so the segment is the last
        // base at or below the ordinal.
        let at = self
            .bases
            .partition_point(|&b| b <= ordinal)
            .checked_sub(1)?;
        Some((at, ordinal - self.bases[at]))
    }

    /// The full path of a file, by global ordinal.
    ///
    /// The only place a tree path is assembled, so the separator rules live in
    /// exactly one function. An entry directly in the root has an empty
    /// directory component and must not gain a doubled separator from it.
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
/// Deliberately not `Send`-shared: the sink that owns it serialises access, so
/// this stays a plain single-threaded builder and the locking question has one
/// answer in one place.
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
            // Empty prefixes: a tree composes paths from the root and the
            // owning directory, so a per-snapshot prefix would be a second,
            // redundant copy of the root on every segment.
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

    /// Bytes held so far, for deciding when to seal.
    pub fn arena_bytes(&self) -> usize {
        self.files.arena_bytes() + self.dirs.arena_bytes()
    }

    /// Adds one completed directory and its files.
    ///
    /// Returns false when an arena is full, which the walk reports as the
    /// index being unable to take more. Pushing the run *before* the names
    /// keeps the ordering promise the run table makes: its `first_file` is
    /// where this directory's files begin, so it must be read before any of
    /// them are added.
    pub fn push_dir(&mut self, rel: &str, files: &[String]) -> bool {
        let dir = self.dirs.len() as u32;
        if !self.dirs.push_str(rel) {
            return false;
        }
        if !files.is_empty() {
            self.runs.push(Run {
                first_file: self.files.len() as u32,
                dir,
            });
            for name in files {
                if !self.files.push_str(name) {
                    return false;
                }
            }
        }
        true
    }

    /// Seals the segment. The builder is left empty and reusable.
    pub fn seal(&mut self) -> TreeSegment {
        let files = std::mem::replace(&mut self.files, SnapshotBuilder::new(""));
        let dirs = std::mem::replace(&mut self.dirs, SnapshotBuilder::new(""));
        TreeSegment {
            // The timestamps and serial belong to the index as a whole, not to
            // a slice of it; a segment carries placeholders.
            files: files.finish(SystemTime::UNIX_EPOCH, 0, None),
            dirs: dirs.finish(SystemTime::UNIX_EPOCH, 0, None),
            runs: std::mem::take(&mut self.runs).into_boxed_slice(),
        }
    }
}

/// The directory part of a relative path, as the walk and the watcher both
/// spell it.
///
/// `ReadDirectoryChangesW` reports the *file* that changed, and the unit this
/// index stores is a directory listing - so the two have to agree on exactly
/// one string format, and it is defined here rather than twice.
pub fn parent_rel(rel: &str) -> &str {
    rel.rsplit_once('\\').map_or("", |(parent, _)| parent)
}

/// Builds a [`TreeIndex`] from a walk, sealing segments as it goes.
///
/// # Why everything mutable is behind one lock
///
/// `TreeSink::push_dir` takes `&self` and is called from every walk worker at
/// once. The store's rule is that the index actor thread is the only writer,
/// and a walk appears to break it - so it is worth being exact about why it
/// does not.
///
/// `walk_tree` is built on `std::thread::scope`: it does not return until
/// every worker has joined. For the whole of a walk the actor thread is
/// therefore *inside* one step, mutating nothing, and the workers are a scoped
/// sub-computation of it. One mutex serialises them against each other, so
/// there is still exactly one writer at any instant and it is still causally
/// the actor. That is also why the walk runs on the actor's own thread rather
/// than being supervised from it: under a supervising actor the argument
/// evaporates, and two writers would race a load-modify-store.
///
/// Contention is not a concern, and the arithmetic says why: appending
/// seventeen names to a vector is well under a microsecond, while the
/// directory read that produced them cost a millisecond or so of SMB. Eight
/// workers contend for about a thousandth of their time.
pub struct SegmentSink {
    staging: Mutex<Staging>,
    root: Box<str>,
}

struct Staging {
    builder: SegmentBuilder,
    sealed: Vec<Arc<TreeSegment>>,
    /// Arena bytes that seal the segment being built. Doubles each time, so
    /// the first results arrive quickly and the segment count stays bounded.
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

    /// The index built so far, including whatever is still unsealed.
    ///
    /// Callable mid-walk, which is what lets a partially walked share be
    /// searched: a snapshot of progress rather than a promise of completeness.
    pub fn index(&self) -> TreeIndex {
        let mut s = self.staging.lock();
        // Sealing the remainder rather than reading round it: a segment is the
        // only searchable form, and leaving the tail in the builder would make
        // the most recently walked folders - the ones someone watching the
        // progress line is waiting for - the ones that never appear.
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

    /// Segments sealed so far. Test and diagnostic use.
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
            // An arena ceiling. Reported back so the walk stops here rather
            // than spending another two minutes reading directories that
            // cannot be stored.
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
            ("11d", &["notes.txt"]),
            ("11d\\0704", &["quote.pdf", "drawing.pdf"]),
            ("empty", &[]),
            ("ab12", &["spec.pdf"]),
        ])
    }

    #[test]
    fn every_file_knows_which_directory_it_is_in() {
        let s = sample();
        assert_eq!(s.file_count(), 5);
        assert_eq!(s.dir_of(0), Some(0), "loose.pdf is in the root");
        assert_eq!(s.dir_of(1), Some(1), "notes.txt is in 11d");
        assert_eq!(s.dir_of(2), Some(2));
        assert_eq!(s.dir_of(3), Some(2), "both of 0704's files");
        assert_eq!(s.dir_of(4), Some(4), "ab12, skipping the empty folder");
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
        assert_eq!(index.full_path(1).unwrap(), "R:\\11d\\notes.txt");
        assert_eq!(index.full_path(2).unwrap(), "R:\\11d\\0704\\quote.pdf");
        assert_eq!(index.full_path(4).unwrap(), "R:\\ab12\\spec.pdf");
        assert_eq!(index.full_path(5), None);
    }

    /// A root that does not already end in a separator must gain exactly one,
    /// and a root that does must not gain a second.
    #[test]
    fn a_root_without_a_trailing_separator_still_composes_cleanly() {
        let index = TreeIndex::empty("R:\\Jobs").appended(Arc::new(sample()));
        assert_eq!(index.full_path(1).unwrap(), "R:\\Jobs\\11d\\notes.txt");
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

    /// Appending shares the existing segments rather than copying them, which
    /// is what makes publishing during a walk affordable.
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
        assert_eq!(parent_rel("11d\\0704\\quote.pdf"), "11d\\0704");
        assert_eq!(parent_rel("loose.pdf"), "", "a file in the root");
        assert_eq!(parent_rel(""), "");
    }
}
