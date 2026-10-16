//! Recursive directory walking.
//!
//! Walks the whole share so files are found regardless of folder naming.
//!
//! # Cost
//!
//! On SMB the cost is round trips, dominated by directory count: each
//! directory takes about three (open, query, end-of-data) regardless of size.
//! 300,000 directories is about 900,000 round trips, roughly seven minutes at
//! 0.5 ms RTT if serial, so directories are read concurrently.
//!
//! `walkdir` and `jwalk` use `std::fs::read_dir`, whose ~4 KB buffer needs
//! ~45,000 round trips per million entries versus ~150 for
//! [`super::win_enum`], so the walk is built on [`DirSource`] instead.
//!
//! # Shape
//!
//! A shared LIFO frontier and N worker threads. Depth-first keeps the frontier
//! at a few thousand entries; breadth-first would hold the widest level of
//! the tree. A failed directory is recorded and never fatal.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::enumerate::{
    DirSource, EntryMeta, EntrySink, ListOpts, is_listable_file, is_listable_file_with,
    is_walkable_dir,
};
use super::errors::EnumError;
use crate::config::{WALK_CONCURRENCY, WALK_DIR_BUFFER_BYTES};
use crate::util::cancel::CancelToken;

/// Consecutive transient failures before the walk concludes the share is gone.
///
/// Prevents an unreachable share from costing one SMB timeout (about twenty
/// seconds) per directory.
const ABORT_AFTER_CONSECUTIVE_TRANSIENT: u32 = 32;

/// Maximum per-directory failures kept verbatim, so a broken share cannot
/// grow the list with its directory count.
const MAX_RECORDED_ERRORS: usize = 64;

/// Limits and options for a walk.
#[derive(Debug, Clone)]
pub struct WalkOpts {
    pub concurrency: usize,
    /// Maximum depth below the root. Also bounds a cycle through a followed
    /// junction.
    pub max_depth: u16,
    pub max_dirs: usize,
    pub max_entries: usize,
    /// Off by default. See [`is_walkable_dir`].
    pub follow_reparse: bool,
    pub buffer_bytes: usize,
    /// Drop files Windows marks hidden or system, never folders. See
    /// [`crate::index::enumerate::is_listable_file_with`].
    pub hide_system: bool,
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
            hide_system: false,
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
    /// Folders that could not be read. These are coverage holes: their files
    /// exist but are absent from the index.
    pub denied: u32,
    /// Folders deleted between being queued and being read.
    ///
    /// Not a coverage hole: this is normal churn on a busy share, and counting
    /// it as a failure would leave a healthy share permanently "degraded".
    pub vanished: u32,
    pub transient: u32,
    pub other: u32,
}

/// Returns whether this failure leaves part of the share unsearchable.
///
/// False for a folder that has gone. An incremental update uses this to delete
/// vanished folders from the index while leaving unreadable ones untouched.
pub fn is_hole(err: EnumError) -> bool {
    !matches!(
        err,
        EnumError::PathNotFound(_) | EnumError::NotADirectory(_)
    )
}

impl WalkErrors {
    /// Returns the failures that leave part of the share unsearchable.
    /// Excludes [`Self::vanished`].
    pub fn holes(&self) -> u32 {
        self.denied + self.transient + self.other
    }

    /// Returns every per-directory failure, including vanished folders.
    pub fn total(&self) -> u32 {
        self.holes() + self.vanished
    }

    fn record(&mut self, rel: &str, err: EnumError) {
        match err {
            EnumError::AccessDenied(_) => self.denied += 1,
            EnumError::PathNotFound(_) | EnumError::NotADirectory(_) => self.vanished += 1,
            EnumError::Transient(_) | EnumError::TimedOut => self.transient += 1,
            _ => self.other += 1,
        }
        // Vanished folders do not take example slots; there is nothing to
        // inspect and they would crowd out actionable errors.
        if self.recorded.len() < MAX_RECORDED_ERRORS && !matches!(err, EnumError::PathNotFound(_)) {
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
    /// The walk gave up because the share stopped answering.
    pub aborted: Option<EnumError>,
    pub errors: WalkErrors,
}

impl WalkReport {
    /// Returns true when the walk reached the end of the tree and read every
    /// folder that still existed.
    ///
    /// Otherwise the index is partial and the user must be told. Vanished
    /// folders do not count; see [`WalkErrors::vanished`].
    pub fn complete(&self) -> bool {
        !self.cancelled && !self.truncated && self.aborted.is_none() && self.errors.holes() == 0
    }
}

/// Receives one completed directory at a time.
///
/// Called from worker threads, so implementations must be cheap and must not
/// touch the network.
pub trait TreeSink: Send + Sync {
    /// `rel` is relative to the walk root, `\`-separated, with no leading or
    /// trailing separator; the root itself is `""`. Returning false stops the
    /// walk (for example when an index is full).
    fn push_dir(&self, rel: &str, files: &[String]) -> bool;

    /// Reports a directory that could not be read. Never fatal.
    fn push_error(&self, _rel: &str, _err: &EnumError) {}

    /// Reports rate-limited progress, for the status line.
    fn progress(&self, _dirs_done: usize, _queued: usize, _files: usize) {}
}

/// Counts a walk without keeping it, for `--bench --walk`.
///
/// Unlike `enumerate::CountingSink`, accumulates across a whole tree from
/// several threads.
#[derive(Debug, Default)]
pub struct WalkCounts {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    pub name_bytes: AtomicU64,
    /// Longest relative directory path, which sizes the directory table a
    /// real index would build.
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
/// LIFO so N depth-first spines keep it at O(depth x fanout). The walk ends
/// when `stack` is empty and `in_flight == 0`, the only state in which no
/// worker can produce more work.
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
/// Reused across directories by each worker to avoid per-folder allocation.
#[derive(Debug, Default)]
struct DirEntries {
    files: Vec<String>,
    subdirs: Vec<String>,
    reparse: u32,
    /// Descend into junctions as well. They are counted either way.
    follow_reparse: bool,
    hide_system: bool,
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
            // Checked inside the file arm so a hidden file is dropped here
            // rather than falling through to the junction arm and being
            // counted as a skipped subtree.
            if is_listable_file_with(meta.attributes, self.hide_system) {
                self.files.push(name.to_string());
            }
        } else if is_walkable_dir(meta.attributes) {
            self.subdirs.push(name.to_string());
        } else {
            // A junction. Always counted so a skipped subtree is visible;
            // descended only when asked, with `max_depth` as the cycle guard.
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
/// the share stops answering. Unreadable directories are recorded and skipped.
pub fn walk_tree(
    source: &dyn DirSource,
    root: &Path,
    opts: &WalkOpts,
    sink: &dyn TreeSink,
    cancel: &CancelToken,
) -> WalkReport {
    walk_from(source, root, &[String::new()], opts, sink, cancel)
}

/// Walks only the named subtrees of `root`.
///
/// Turns a change notification into work proportional to the change. Each
/// seed's whole subtree is walked, since a new folder arrives as one event on
/// its parent.
///
/// `seeds` are relative to `root` in the index's spelling; `""` is the root
/// itself, which makes this a full walk.
///
/// Depth restarts at zero for each seed, because `max_depth` is a runaway
/// guard and measuring from the share root would make deep folders
/// unrefreshable.
pub fn walk_subtrees(
    source: &dyn DirSource,
    root: &Path,
    seeds: &[String],
    opts: &WalkOpts,
    sink: &dyn TreeSink,
    cancel: &CancelToken,
) -> WalkReport {
    walk_from(source, root, &prune_seeds(seeds), opts, sink, cancel)
}

/// Drops seeds covered by another seed, collapsing to the root if present.
///
/// Otherwise overlapping seeds would read a directory twice and the sink
/// would record it twice, doubling a folder match's results.
fn prune_seeds(seeds: &[String]) -> Vec<String> {
    if seeds.iter().any(|s| s.is_empty()) {
        return vec![String::new()];
    }
    let mut sorted: Vec<String> = seeds.to_vec();
    sorted.sort();
    sorted.dedup();

    let mut out: Vec<String> = Vec::with_capacity(sorted.len());
    for seed in sorted {
        // Sorted, so an ancestor always precedes its descendants.
        if out.last().is_some_and(|kept| is_within(&seed, kept)) {
            continue;
        }
        out.push(seed);
    }
    out
}

/// Returns whether `rel` is `ancestor` or beneath it.
///
/// The separator check stops `abc2` from being treated as a child of `abc`.
pub fn is_within(rel: &str, ancestor: &str) -> bool {
    if ancestor.is_empty() || rel == ancestor {
        return true;
    }
    rel.len() > ancestor.len()
        && rel.as_bytes()[ancestor.len()] == b'\\'
        && rel.starts_with(ancestor)
}

fn walk_from(
    source: &dyn DirSource,
    root: &Path,
    seeds: &[String],
    opts: &WalkOpts,
    sink: &dyn TreeSink,
    cancel: &CancelToken,
) -> WalkReport {
    let started = Instant::now();
    let frontier = Arc::new(Frontier::default());
    let shared = Arc::new(Shared::default());
    for rel in seeds {
        frontier.push(Pending {
            rel: rel.clone(),
            depth: 0,
        });
    }

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
        // Subdirectories must be returned so the walk can recurse.
        files_only: false,
        buffer_bytes: opts.buffer_bytes,
        max_entries: usize::MAX,
        deadline: None,
        force: None,
        // Filtering happens in `DirEntries`, which must see hidden
        // directories in order to descend into them.
        hide_system: false,
    };
    let mut entries = DirEntries {
        follow_reparse: opts.follow_reparse,
        hide_system: opts.hide_system,
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
                    // A failed root means there is no share; a walk that read
                    // nothing must never be reported as complete.
                    if pending.rel.is_empty() {
                        s.aborted = Some(err);
                        shared.stop.store(true, Ordering::Relaxed);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: u32 = 0x0080;
    const DIRECTORY: u32 = 0x0010;
    const REPARSE: u32 = 0x0400;
    const HIDDEN: u32 = 0x0002;
    const SYSTEM: u32 = 0x0004;

    fn sorted(hide_system: bool, entries: &[(&str, u32)]) -> (Vec<String>, Vec<String>, u32) {
        let mut sink = DirEntries {
            hide_system,
            ..Default::default()
        };
        for (name, attributes) in entries {
            assert!(sink.push_str(
                name,
                EntryMeta {
                    attributes: *attributes
                }
            ));
        }
        (sink.files.clone(), sink.subdirs.clone(), sink.reparse)
    }

    #[test]
    fn a_marked_file_is_kept_until_the_walk_is_asked_to_drop_it() {
        let entries = [("sheet.pdf", FILE), ("Thumbs.db", FILE | HIDDEN)];
        let (files, _, _) = sorted(false, &entries);
        assert_eq!(files, vec!["sheet.pdf", "Thumbs.db"]);

        let (files, _, _) = sorted(true, &entries);
        assert_eq!(files, vec!["sheet.pdf"]);
    }

    /// A dropped file is not reclassified as a junction, which would make
    /// `WalkReport::skipped_reparse` report unsearched folders that do not
    /// exist.
    #[test]
    fn a_dropped_file_is_not_counted_as_a_skipped_subtree() {
        let (files, subdirs, reparse) = sorted(
            true,
            &[
                ("desktop.ini", FILE | SYSTEM),
                ("junction", DIRECTORY | REPARSE),
            ],
        );
        assert!(files.is_empty());
        assert!(subdirs.is_empty());
        assert_eq!(reparse, 1, "the hidden file was counted as a junction");
    }

    /// A folder marked system or hidden is still walked, so a share marked
    /// system does not disappear.
    #[test]
    fn a_marked_folder_is_still_descended_into() {
        let (_, subdirs, _) = sorted(
            true,
            &[
                ("archive", DIRECTORY | SYSTEM),
                ("private", DIRECTORY | HIDDEN),
            ],
        );
        assert_eq!(subdirs, vec!["archive", "private"]);
    }
}
