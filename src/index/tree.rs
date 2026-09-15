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
    /// Assembles a segment from parts that came from somewhere other than a
    /// walk - which today means the persisted index.
    ///
    /// Validating here rather than at the call site is what keeps
    /// [`Self::check_invariants`] the single definition of a well-formed
    /// segment: a decoder cannot accidentally accept a run table the builder
    /// could never have produced.
    pub(crate) fn from_parts(
        files: Snapshot,
        dirs: Snapshot,
        runs: Box<[Run]>,
    ) -> Result<Self, &'static str> {
        let seg = Self { files, dirs, runs };
        seg.check_invariants()?;
        Ok(seg)
    }

    /// The run table, for the persistence layer. Not public: a run is
    /// meaningless without the segment it indexes into, and every in-crate
    /// reader goes through [`Self::dir_of`] or [`Self::files_of`].
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
    /// Every directory here was seen through a *search* rather than read.
    ///
    /// True only for a share configured `kind = "live"`, which is never
    /// walked: what it holds is whatever somebody has looked for, so a name
    /// absent from it says nothing at all about the share. That is a stronger
    /// claim than [`Self::complete`] makes - an incomplete walk still read
    /// every folder it reached - and the two are kept apart because the
    /// sentence each one licenses on screen is different.
    ///
    /// A share is never both. A live mapping gets no walk, and a walked one is
    /// never asked per query, so this is a property of the whole index rather
    /// than of any segment in it - which is what keeps it a single bit in the
    /// header rather than something the persistence layer has to interleave.
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

    pub fn with_observed(mut self, observed: bool) -> Self {
        self.observed = observed;
        self
    }

    /// True when this index holds only what searches have found.
    pub fn observed(&self) -> bool {
        self.observed
    }

    /// Whether this index already holds `file` in `rel`.
    ///
    /// Linear in the directory count, and called once per directory a search
    /// touched rather than once per file on the share - a few dozen
    /// comparisons against an index that only ever holds what has been looked
    /// for.
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

    /// This index plus files a search saw, leaving out anything already held.
    ///
    /// Appends and never replaces, which is the opposite of
    /// [`Self::with_subtrees_replaced`] and deliberately so. A change
    /// notification names folders that were *re-read*, so replacing them is
    /// exact. A search result names the files that *matched*, and says nothing
    /// whatever about the files that did not - so the only sound merge is the
    /// one that adds. Replacing here would turn a partial answer into a
    /// confident, wrong, complete one, which is the silent disappearance this
    /// index exists to prevent arriving by the front door.
    ///
    /// `None` when every observation was already held, so repeating a search
    /// costs no publication and no reader a re-read.
    ///
    /// `captured_at` is left alone, for the reason `confirm_fresh` leaves
    /// `built_at` alone: the index is exactly as old as it was, and a handful
    /// of names in it are newer.
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
    /// index being unable to take more. `first_file` is read *before* any name
    /// is added, which is the ordering promise the run table makes, but the
    /// run is only pushed once a file has actually landed: a run recorded
    /// ahead of a push that then failed would name a range starting past the
    /// end of the segment, and `dir_of` would attribute the next directory's
    /// files to this one.
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

    /// Copies one directory and every file in it verbatim from `seg`.
    ///
    /// Raw byte copies rather than re-folding. Re-deriving the folded form on
    /// a rebuild would make a name that had been indexed under one fold
    /// reappear under another, so the matcher would find it before the update
    /// and not after - a file disappearing because its folder was refreshed.
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
// --- incremental update ----------------------------------------------------

/// What was left of a segment after dropping some of its directories.
enum Retained {
    /// Nothing was dropped, so the original can be shared by refcount.
    All,
    /// Everything was dropped.
    None,
    /// Boxed only to keep the enum small: a `TreeSegment` is a quarter of a
    /// kilobyte of headers, and every `Retained::All` - the common answer, one
    /// per untouched segment - would otherwise be returned in a value that
    /// size.
    Some(Box<TreeSegment>),
}

impl TreeSegment {
    /// Bytes in this segment's arenas, on the same basis
    /// [`SegmentBuilder::arena_bytes`] counts them.
    pub fn arena_bytes(&self) -> usize {
        self.files.lower().len() + self.dirs.lower().len()
    }

    /// This segment without the directories `drop` selects.
    ///
    /// Returns [`Retained::All`] rather than a copy when nothing matched,
    /// which is the case that matters: a change to one folder must not cost a
    /// rebuild of the twenty segments it is not in.
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
                // The arena ceiling, reached while *shrinking* a segment that
                // already fitted. Not reachable in practice, but "not
                // reachable" is not a reason to publish a half-copied segment:
                // keeping the original loses nothing but the deletion, which
                // the next full walk collects.
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

    /// This index with the subtrees named by `replaced` rebuilt from `fresh`.
    ///
    /// Every directory that is one of `replaced` or sits beneath one is
    /// dropped, and everything `fresh` holds is appended. That is deliberately
    /// coarser than "update these directories": a new job folder arrives as a
    /// single notification on its *parent*, so anything that only refreshed
    /// the directories it was told about would index the parent and miss every
    /// file in the folder that was actually created.
    ///
    /// `captured_at` is left alone. The index really is as old as it was; a
    /// handful of its folders are newer. Moving the timestamp forward would be
    /// the index claiming to have been proven current everywhere, which is
    /// exactly the false confidence a tree is not allowed to express.
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

/// Merges adjacent segments that fit together inside one segment's budget.
///
/// Without this, every update appends a segment and the count grows without
/// bound between full walks - a few hundred of them by the time the floor
/// comes round, each one a separate sweep per keystroke.
///
/// One rule does the whole job, and the shape of the data is why. Update
/// segments are tiny and land at the end, so consecutive ones merge into a
/// single growing tail; full-size segments never pair, because two of them
/// exceed the budget by construction. So the work per update is bounded by one
/// segment's worth of copying, and only where there was headroom for it.
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
            ("11d", &["notes.txt"]),
            ("11d\\0704", &["quote.pdf", "drawing.pdf"]),
            ("empty", &[]),
            ("ab12", &["spec.pdf"]),
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
            .with_observed_files(&observed(&[("p12345", &["a.pdf", "b.dwg"])]))
            .expect("something was new");
        assert_eq!(next.len(), 2);
        assert!(next.observed());
        assert!(!next.complete(), "an observed index is never complete");
        assert_eq!(files_in(&next, "p12345"), ["a.pdf", "b.dwg"]);
    }

    /// The common case for a code somebody searches twice. Publishing anyway
    /// would cost every reader a re-read for nothing.
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

    /// A search result names the files that *matched*, and says nothing
    /// whatever about the files that did not. Replacing the directory would
    /// turn a partial answer into a confident, wrong, complete one - the
    /// silent disappearance this index exists to prevent, arriving by the
    /// front door.
    #[test]
    fn remembering_a_second_search_never_drops_what_the_first_one_found() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("jobs", &["0704.pdf"])]))
            .unwrap();
        let next = ix
            .with_observed_files(&observed(&[("jobs", &["0801.pdf"])]))
            .unwrap();
        assert_eq!(files_in(&next, "jobs"), ["0704.pdf", "0801.pdf"]);
    }

    #[test]
    fn a_name_is_matched_for_holding_however_it_is_cased() {
        let ix = TreeIndex::empty("V:\\archive")
            .with_observed_files(&observed(&[("jobs", &["A.PDF"])]))
            .unwrap();
        assert!(ix.holds("jobs", "a.pdf"));
        assert!(!ix.holds("other", "a.pdf"), "the folder has to match too");
    }

    /// Every pass appends, so without coalescing the segment count would grow
    /// without bound and each one is a separate sweep per keystroke.
    #[test]
    fn many_small_observations_do_not_grow_the_segment_count_without_bound() {
        let mut ix = TreeIndex::empty("V:\\archive");
        for i in 0..40 {
            ix = ix
                .with_observed_files(&observed(&[("jobs", &[&format!("f{i:03}.pdf")])]))
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

/// Replacing a subtree in place, which is what turns a change notification
/// into work proportional to what changed rather than to the share.
#[cfg(test)]
mod apply_tests {
    use std::time::Duration;

    use super::*;
    use crate::index::builder::MAX_NAME_BYTES;

    const ROOT: &str = "R:\\";

    /// One segment per group, so the tests can put a directory in a chosen
    /// segment rather than hoping the size ladder puts it there.
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
                ("11d", &["quote.pdf", "old.pdf"]),
            ],
            &[
                ("11d\\0704", &["drawing.dwg"]),
                ("ab12", &["spec.pdf"]),
                ("ab12x", &["decoy.pdf"]),
            ],
        ])
    }

    #[test]
    fn a_replaced_subtree_takes_the_fresh_contents() {
        let fresh = tree_of(&[&[
            ("11d", &["quote.pdf", "new.pdf"] as &[&str]),
            ("11d\\0704", &["drawing.dwg", "revised.dwg"]),
        ]]);
        let after = base().with_subtrees_replaced(&["11d".to_string()], &fresh);

        assert_eq!(
            paths(&after),
            vec![
                "R:\\11d\\0704\\drawing.dwg",
                "R:\\11d\\0704\\revised.dwg",
                "R:\\11d\\new.pdf",
                "R:\\11d\\quote.pdf",
                "R:\\ab12\\spec.pdf",
                "R:\\ab12x\\decoy.pdf",
                "R:\\readme.txt",
            ]
        );
    }

    /// The reason a whole subtree is replaced rather than a directory. A new
    /// job folder arrives as one notification on its parent; anything that
    /// only refreshed the directory it was told about would index the parent
    /// and miss every file in the folder that was actually created.
    #[test]
    fn a_folder_created_under_a_dirty_parent_is_picked_up() {
        let fresh = tree_of(&[&[
            ("11d", &["quote.pdf", "old.pdf"] as &[&str]),
            ("11d\\0704", &["drawing.dwg"]),
            ("11d\\0999", &["brand new.pdf"]),
        ]]);
        let after = base().with_subtrees_replaced(&["11d".to_string()], &fresh);
        assert!(paths(&after).contains(&"R:\\11d\\0999\\brand new.pdf".to_string()));
    }

    #[test]
    fn a_deleted_folder_leaves_the_index() {
        let fresh = tree_of(&[&[("11d", &["quote.pdf"] as &[&str])]]);
        let after = base().with_subtrees_replaced(&["11d".to_string()], &fresh);

        assert!(!paths(&after).iter().any(|p| p.contains("0704")));
        assert!(!paths(&after).iter().any(|p| p.contains("old.pdf")));
        assert!(paths(&after).contains(&"R:\\ab12\\spec.pdf".to_string()));
    }

    /// A subtree that is gone entirely is replaced by nothing.
    #[test]
    fn replacing_a_subtree_with_nothing_removes_it() {
        let after = base().with_subtrees_replaced(&["ab12".to_string()], &TreeIndex::empty(ROOT));
        let got = paths(&after);
        assert!(!got.iter().any(|p| p.contains("\\ab12\\")));
        assert!(
            got.contains(&"R:\\ab12x\\decoy.pdf".to_string()),
            "a prefix match is not a subtree: {got:?}"
        );
    }

    /// The separator check. Without it `ab12` would swallow `ab12x`.
    #[test]
    fn a_name_that_merely_starts_the_same_is_not_within_the_subtree() {
        assert!(crate::index::walk::is_within("ab12\\sub", "ab12"));
        assert!(crate::index::walk::is_within("ab12", "ab12"));
        assert!(!crate::index::walk::is_within("ab12x", "ab12"));
        assert!(crate::index::walk::is_within("anything", ""));
    }

    /// A dirty root is a full replacement, which is the honest answer.
    #[test]
    fn replacing_the_root_replaces_everything() {
        let fresh = tree_of(&[&[("", &["only.txt"] as &[&str])]]);
        let after = base().with_subtrees_replaced(&[String::new()], &fresh);
        assert_eq!(paths(&after), vec!["R:\\only.txt"]);
    }

    /// The cost argument for the whole design: a change to one folder must not
    /// rebuild the segments it is not in.
    #[test]
    fn untouched_segments_are_shared_rather_than_copied() {
        let before = base();
        let untouched = Arc::clone(&before.segments()[1]);
        let fresh = tree_of(&[&[("11d", &["quote.pdf"] as &[&str])]]);

        let after = before.with_subtrees_replaced(&["11d\\0704".to_string()], &fresh);
        assert!(
            after
                .segments()
                .iter()
                .any(|s| Arc::ptr_eq(s, &untouched) || Arc::strong_count(&untouched) > 1),
            "the segment holding no dirty directory should have been reused"
        );
    }

    /// The metadata says what it says for a reason: a handful of refreshed
    /// folders is not the whole share having been proven current.
    #[test]
    fn an_incremental_update_does_not_claim_the_index_is_newer() {
        let before = base();
        let fresh = tree_of(&[&[("11d", &["quote.pdf"] as &[&str])]]);
        let after = before.with_subtrees_replaced(&["11d".to_string()], &fresh);

        assert_eq!(after.captured_at(), before.captured_at());
        assert_eq!(after.volume_serial(), before.volume_serial());
        assert_eq!(after.complete(), before.complete());
        assert_eq!(after.root(), before.root());
    }

    #[test]
    fn the_directory_count_follows_the_update() {
        let before = base();
        assert_eq!(before.dir_count(), 5);
        let after = before.with_subtrees_replaced(&["11d".to_string()], &TreeIndex::empty(ROOT));
        assert_eq!(after.dir_count(), 3, "`11d` and `11d\\0704` both left");
    }

    /// Every update appends a segment. Left alone the count would run to
    /// hundreds between full walks, each one a separate sweep per keystroke.
    #[test]
    fn repeated_updates_do_not_grow_the_segment_count_without_bound() {
        let mut index = base();
        for i in 0..50 {
            let fresh = tree_of(&[&[(
                "ab12",
                &[Box::leak(format!("v{i}.pdf").into_boxed_str()) as &str] as &[&str],
            )]]);
            index = index.with_subtrees_replaced(&["ab12".to_string()], &fresh);
        }
        assert!(
            index.segments().len() <= 2,
            "fifty updates left {} segments",
            index.segments().len()
        );
        assert_eq!(
            paths(&index)
                .iter()
                .filter(|p| p.contains("ab12\\"))
                .count(),
            1,
            "each update should supersede the last, not stack on it"
        );
    }

    /// Coalescing must not disturb what the index reports.
    #[test]
    fn coalescing_preserves_every_path_and_its_folder() {
        let before = base();
        let fresh = tree_of(&[&[("ab12", &["spec.pdf", "extra.pdf"] as &[&str])]]);
        let after = before.with_subtrees_replaced(&["ab12".to_string()], &fresh);

        for seg in after.segments() {
            seg.check_invariants()
                .expect("a coalesced segment is sound");
        }
        assert!(paths(&after).contains(&"R:\\ab12\\extra.pdf".to_string()));
        assert!(paths(&after).contains(&"R:\\11d\\0704\\drawing.dwg".to_string()));
    }

    /// Replacing nothing is a no-op, which matters because the actor reaches
    /// this path whenever a batch turns out to name only unreadable folders.
    #[test]
    fn replacing_nothing_changes_nothing() {
        let before = base();
        let after = before.with_subtrees_replaced(&[], &TreeIndex::empty(ROOT));
        assert_eq!(paths(&after), paths(&before));
        assert_eq!(after.dir_count(), before.dir_count());
    }

    /// A run recorded before a push that then failed would name a range
    /// starting past the end of the segment, and `dir_of` would hand this
    /// directory's files to the next one.
    #[test]
    fn a_directory_whose_files_did_not_fit_leaves_a_sound_segment() {
        let mut b = SegmentBuilder::new();
        let huge = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(b.push_dir("a", &["real.pdf".to_string()]));
        assert!(!b.push_dir("b", &[huge]));
        assert!(b.seal().check_invariants().is_ok());
    }
}
