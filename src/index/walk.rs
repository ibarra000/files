//! Recursive directory walking.
//!
//! The index knows where a file is because a regex said which folder to look
//! in. Any file whose folder does not follow the naming rule is therefore
//! invisible - not "no results", silently absent - and that is the bug this
//! exists to remove: walk the share and *know*.
//!
//! # What it costs
//!
//! On SMB the bill is round trips, and for a recursive walk the **directory
//! count** dominates, not the file count. A directory costs roughly three
//! round trips - open, query, end-of-data - however few files it holds, so a
//! tree of 300,000 directories is around 900,000 round trips: some seven
//! minutes at a 0.5 ms RTT if done one at a time. Walking several directories
//! at once is therefore not an optimisation, it is the difference between
//! usable and not.
//!
//! # Why not `walkdir` or `jwalk`
//!
//! Both sit on `std::fs::read_dir`, whose ~4 KB buffer costs ~45,000 round
//! trips per million entries against ~150 for the handle-based enumerator this
//! crate already has (see [`super::win_enum`]). On SMB that is a ~300x
//! regression, which is why the walk is built on [`DirSource`] instead.
//!
//! # Shape
//!
//! A shared LIFO frontier and N worker threads. Depth-first because breadth
//! -first over a wide tree holds the whole of its widest level in memory:
//! eight independent depth-first spines keep the frontier at a few thousand
//! entries rather than a hundred thousand.
//!
//! Failure of one directory is recorded and never fatal. An unreadable folder
//! in the middle of a share must not cost the other 299,999.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::enumerate::{
    DirSource, EntryMeta, EntrySink, ListOpts, is_listable_file, is_walkable_dir,
};
use super::errors::EnumError;
use crate::config::{WALK_CONCURRENCY, WALK_DIR_BUFFER_BYTES};
use crate::util::cancel::CancelToken;

/// Consecutive transient failures before the walk concludes the share is gone.
///
/// Without it, an unplugged VPN turns a 300,000-directory walk into 300,000
/// serialised twenty-second SMB timeouts - around twenty hours, which is not
/// something anyone can wait out or interpret. Thirty-two in a row is not
/// ambiguous.
const ABORT_AFTER_CONSECUTIVE_TRANSIENT: u32 = 32;

/// Per-directory failures kept verbatim. Bounded, because a broken share must
/// not be able to allocate a list proportional to its directory count.
const MAX_RECORDED_ERRORS: usize = 64;

/// How the walk is bounded.
#[derive(Debug, Clone)]
pub struct WalkOpts {
    pub concurrency: usize,
    /// Depth below the root. A cycle through a followed junction cannot
    /// outrun this even if the id-based guard is unavailable.
    pub max_depth: u16,
    pub max_dirs: usize,
    pub max_entries: usize,
    /// Off by default. See [`is_walkable_dir`].
    pub follow_reparse: bool,
    pub buffer_bytes: usize,
}

impl Default for WalkOpts {
    fn default() -> Self {
        Self {
            concurrency: WALK_CONCURRENCY,
            max_depth: 32,
            max_dirs: 1_000_000,
            max_entries: 8_000_000,
            follow_reparse: false,
            buffer_bytes: WALK_DIR_BUFFER_BYTES,
        }
    }
}

impl WalkOpts {
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    pub fn with_max_depth(mut self, d: u16) -> Self {
        self.max_depth = d;
        self
    }
}

/// Per-directory failures, counted in full and kept in part.
#[derive(Debug, Clone, Default)]
pub struct WalkErrors {
    pub recorded: Vec<(String, EnumError)>,
    pub denied: u32,
    pub missing: u32,
    pub transient: u32,
    pub other: u32,
}

impl WalkErrors {
    pub fn total(&self) -> u32 {
        self.denied + self.missing + self.transient + self.other
    }

    fn record(&mut self, rel: &str, err: EnumError) {
        match err {
            EnumError::AccessDenied(_) => self.denied += 1,
            EnumError::PathNotFound(_) | EnumError::NotADirectory(_) => self.missing += 1,
            EnumError::Transient(_) | EnumError::TimedOut => self.transient += 1,
            _ => self.other += 1,
        }
        if self.recorded.len() < MAX_RECORDED_ERRORS {
            self.recorded.push((rel.to_string(), err));
        }
    }
}

/// What a walk found, and what it could not reach.
#[derive(Debug, Clone, Default)]
pub struct WalkReport {
    pub dirs_visited: usize,
    pub files: usize,
    pub max_depth_seen: u16,
    /// Junctions and directory symlinks not descended into.
    pub skipped_reparse: u32,
    /// Subtrees cut off by `max_depth`.
    pub depth_clipped: u32,
    pub round_trips: u64,
    pub elapsed: Duration,
    pub cancelled: bool,
    /// A limit stopped the walk, so this is a partial view of the tree.
    pub truncated: bool,
    /// The walk gave up because the share stopped answering at all.
    pub aborted: Option<EnumError>,
    pub errors: WalkErrors,
}

impl WalkReport {
    /// True when the walk reached the end of the tree and read every folder.
    ///
    /// Anything else is a partial index, and the difference has to reach the
    /// user: results missing because a subtree was unreadable look exactly
    /// like results that do not exist.
    pub fn complete(&self) -> bool {
        !self.cancelled && !self.truncated && self.aborted.is_none() && self.errors.total() == 0
    }
}

/// Receives one completed directory at a time.
///
/// Called from a worker thread, so an implementation must be cheap and must
/// not touch the network.
pub trait TreeSink: Send + Sync {
    /// `rel` is relative to the walk root, `\`-separated, with no leading or
    /// trailing separator; the root itself is `""`. Returning false stops the
    /// walk, which is how an index reports that it is full.
    fn push_dir(&self, rel: &str, files: &[String]) -> bool;

    /// A directory could not be read. Never fatal.
    fn push_error(&self, _rel: &str, _err: &EnumError) {}

    /// Rate-limited progress, for the status line.
    fn progress(&self, _dirs_done: usize, _queued: usize, _files: usize) {}
}

/// Counts a walk without keeping any of it, for `--bench --walk`.
///
/// Distinct from `enumerate::CountingSink`, which counts one directory; this
/// accumulates across a whole tree and from several threads at once.
#[derive(Debug, Default)]
pub struct WalkCounts {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    pub name_bytes: AtomicU64,
    /// Longest relative directory path, which sizes the interned directory
    /// table a real index would build.
    pub max_rel_len: AtomicU64,
}

impl TreeSink for WalkCounts {
    fn push_dir(&self, rel: &str, files: &[String]) -> bool {
        self.dirs.fetch_add(1, Ordering::Relaxed);
        self.files.fetch_add(files.len() as u64, Ordering::Relaxed);
        let bytes: usize = files.iter().map(|f| f.len()).sum();
        self.name_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.max_rel_len
            .fetch_max(rel.len() as u64, Ordering::Relaxed);
        true
    }
}

/// A directory waiting to be read.
#[derive(Debug)]
struct Pending {
    rel: String,
    depth: u16,
}

/// The shared frontier.
///
/// LIFO on purpose: breadth-first over a wide tree holds its widest level in
/// the queue at once, whereas N depth-first spines keep it at O(depth x
/// fanout). Termination is `stack empty && in_flight == 0`, which is the only
/// condition under which no worker can still produce more work.
#[derive(Debug, Default)]
struct FrontierInner {
    stack: Vec<Pending>,
    in_flight: usize,
    closed: bool,
}

#[derive(Debug, Default)]
struct Frontier {
    inner: Mutex<FrontierInner>,
    wake: Condvar,
}

impl Frontier {
    fn push(&self, pending: Pending) {
        self.inner.lock().stack.push(pending);
        self.wake.notify_one();
    }

    fn push_many(&self, items: Vec<Pending>) {
        if items.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        inner.stack.extend(items);
        self.wake.notify_all();
    }

    /// Takes the next directory, or `None` once the walk is over.
    fn pop(&self) -> Option<Pending> {
        let mut inner = self.inner.lock();
        loop {
            if inner.closed {
                return None;
            }
            if let Some(next) = inner.stack.pop() {
                inner.in_flight += 1;
                return Some(next);
            }
            if inner.in_flight == 0 {
                // Nothing queued and nobody working: no more work can appear.
                inner.closed = true;
                self.wake.notify_all();
                return None;
            }
            self.wake.wait(&mut inner);
        }
    }

    fn finish(&self) {
        let mut inner = self.inner.lock();
        inner.in_flight -= 1;
        if inner.in_flight == 0 && inner.stack.is_empty() {
            inner.closed = true;
            self.wake.notify_all();
        }
    }

    fn close(&self) {
        let mut inner = self.inner.lock();
        inner.closed = true;
        self.wake.notify_all();
    }

    fn queued(&self) -> usize {
        self.inner.lock().stack.len()
    }
}

/// One directory's entries, split into files kept and subdirectories queued.
///
/// Reused across directories by each worker, so a large walk does not allocate
/// a fresh vector per folder.
#[derive(Debug, Default)]
struct DirEntries {
    files: Vec<String>,
    subdirs: Vec<String>,
    reparse: u32,
    /// Descend into junctions as well. Still counted, so the report says how
    /// many were crossed either way.
    follow_reparse: bool,
}

impl DirEntries {
    fn clear(&mut self) {
        self.files.clear();
        self.subdirs.clear();
        self.reparse = 0;
    }
}

impl EntrySink for DirEntries {
    fn push_wide(&mut self, name: &[u16], meta: EntryMeta) -> bool {
        self.push_str(&String::from_utf16_lossy(name), meta)
    }

    fn push_str(&mut self, name: &str, meta: EntryMeta) -> bool {
        if is_listable_file(meta.attributes) {
            self.files.push(name.to_string());
        } else if is_walkable_dir(meta.attributes) {
            self.subdirs.push(name.to_string());
        } else {
            // A junction. Counted either way, so a skipped subtree is visible
            // rather than silent, and only descended into when asked for -
            // where `max_depth` is the only thing standing between a cycle and
            // a walk that never ends.
            self.reparse += 1;
            if self.follow_reparse {
                self.subdirs.push(name.to_string());
            }
        }
        true
    }

    fn accepted(&self) -> usize {
        self.files.len() + self.subdirs.len()
    }
}

/// Totals accumulated across the worker threads.
#[derive(Debug, Default)]
struct Shared {
    stats: Mutex<WalkReport>,
    stop: AtomicBool,
    consecutive_transient: AtomicU64,
}

/// Joins a relative path with a child name, `\`-separated.
fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}\\{name}")
    }
}

/// Walks `root` recursively, handing each completed directory to `sink`.
///
/// Returns when the tree is exhausted, the token fires, a limit is reached, or
/// the share stops answering. A directory that cannot be read is recorded and
/// the walk carries on.
pub fn walk_tree(
    source: &dyn DirSource,
    root: &Path,
    opts: &WalkOpts,
    sink: &dyn TreeSink,
    cancel: &CancelToken,
) -> WalkReport {
    let started = Instant::now();
    let frontier = Arc::new(Frontier::default());
    let shared = Arc::new(Shared::default());
    frontier.push(Pending {
        rel: String::new(),
        depth: 0,
    });

    let workers = opts.concurrency.max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                worker(source, root, opts, sink, cancel, &frontier, &shared);
            });
        }
    });

    let mut report = std::mem::take(&mut *shared.stats.lock());
    report.elapsed = started.elapsed();
    report.cancelled |= cancel.is_cancelled();
    report
}

#[allow(clippy::too_many_arguments)]
fn worker(
    source: &dyn DirSource,
    root: &Path,
    opts: &WalkOpts,
    sink: &dyn TreeSink,
    cancel: &CancelToken,
    frontier: &Frontier,
    shared: &Shared,
) {
    let list_opts = ListOpts {
        // The whole point: subdirectories have to come back, or there is
        // nothing to recurse into.
        files_only: false,
        buffer_bytes: opts.buffer_bytes,
        max_entries: usize::MAX,
        deadline: None,
        force: None,
    };
    let mut entries = DirEntries {
        follow_reparse: opts.follow_reparse,
        ..Default::default()
    };

    while let Some(pending) = frontier.pop() {
        if cancel.is_cancelled() || shared.stop.load(Ordering::Relaxed) {
            frontier.finish();
            frontier.close();
            return;
        }

        entries.clear();
        let dir = if pending.rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&pending.rel)
        };

        let result = source.list(&dir, &mut entries, &list_opts, cancel);
        let mut queue: Vec<Pending> = Vec::new();

        match result {
            Ok(stats) => {
                shared.consecutive_transient.store(0, Ordering::Relaxed);

                let depth = pending.depth;
                if depth < opts.max_depth {
                    queue.extend(entries.subdirs.iter().map(|name| Pending {
                        rel: join_rel(&pending.rel, name),
                        depth: depth + 1,
                    }));
                } else if !entries.subdirs.is_empty() {
                    let mut s = shared.stats.lock();
                    s.depth_clipped += entries.subdirs.len() as u32;
                    s.truncated = true;
                }

                let full = !sink.push_dir(&pending.rel, &entries.files);

                let (dirs_done, files_total) = {
                    let mut s = shared.stats.lock();
                    s.dirs_visited += 1;
                    s.files += entries.files.len();
                    s.round_trips += stats.round_trips as u64;
                    s.skipped_reparse += entries.reparse;
                    s.max_depth_seen = s.max_depth_seen.max(depth);
                    if !stats.complete {
                        s.truncated = true;
                    }
                    if full || s.dirs_visited >= opts.max_dirs || s.files >= opts.max_entries {
                        s.truncated = true;
                        shared.stop.store(true, Ordering::Relaxed);
                    }
                    (s.dirs_visited, s.files)
                };
                sink.progress(dirs_done, frontier.queued(), files_total);
            }
            Err(EnumError::Cancelled) => {
                shared.stats.lock().cancelled = true;
                shared.stop.store(true, Ordering::Relaxed);
            }
            Err(err) => {
                let transient = matches!(err, EnumError::Transient(_) | EnumError::TimedOut);
                sink.push_error(&pending.rel, &err);
                {
                    let mut s = shared.stats.lock();
                    s.errors.record(&pending.rel, err);
                }
                if transient {
                    let n = shared.consecutive_transient.fetch_add(1, Ordering::Relaxed) + 1;
                    if n >= ABORT_AFTER_CONSECUTIVE_TRANSIENT as u64 {
                        shared.stats.lock().aborted = Some(err);
                        shared.stop.store(true, Ordering::Relaxed);
                    }
                } else {
                    shared.consecutive_transient.store(0, Ordering::Relaxed);
                }
            }
        }

        frontier.push_many(queue);
        frontier.finish();

        if shared.stop.load(Ordering::Relaxed) {
            frontier.close();
            return;
        }
    }
}
