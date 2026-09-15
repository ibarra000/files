//! An in-memory [`DirSource`] for tests.
//!
//! The valuable behaviours here are not "returns data". They are **fails with
//! a network error** and **blocks for a minute**, because those are the
//! conditions the real drives will produce and that no test on this machine
//! can otherwise reach. The call log additionally makes prefetch de-duplication
//! and cache-bypass behaviour assertable without any I/O at all.
//!
//! # The stamp seams, and why they are separate from the listing ones
//!
//! [`FakeDirSource::set_stamp_error`] and
//! [`FakeDirSource::set_stamp_supported`] fail the freshness probe *without*
//! failing enumeration. Until they existed there was no way to reach the
//! "this share will not tell me whether it changed" path from a test - the old
//! test for it said so out loud and asserted against `IndexStore` directly
//! instead. That path was where the index quietly re-enumerated a
//! million-entry share every sixty seconds, so the missing seam was the bug.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::DirStamp;
use super::enumerate::{DirSource, EntryMeta, EntrySink, ListOpts, ListStats};
use super::errors::EnumError;
use crate::util::cancel::CancelToken;

/// Case-insensitive wildcard match, mirroring the semantics the file server
/// applies to a search pattern.
///
/// `*` matches zero or more characters (including `.`, unlike the legacy DOS
/// star) and `?` matches **exactly one**. Getting `?` right matters: the
/// completeness oracle in `--bench` partitions a directory by name length
/// using `?`, `??`, ... and asserts the shards sum to the whole. A fake that
/// ignored `?` would make that check report a false failure.
pub fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();

    // Classic two-cursor glob match with backtracking on the last star.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);

    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// One recorded call, so tests can assert on access patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Stamp(PathBuf),
    List(PathBuf),
    Query(PathBuf, String),
    Prewarm(PathBuf),
}

#[derive(Debug)]
struct Behaviour {
    latency: Duration,
    fail_with: Option<EnumError>,
    /// Blocks until cancelled or the deadline passes, to prove the UI stays
    /// responsive when the network does not.
    hang: bool,
    stamp: Option<DirStamp>,
    /// Fails `probe_stamp` only, leaving `list` and `query` working.
    stamp_error: Option<EnumError>,
    /// When false, `probe_stamp` reports `Unsupported`, as a share that will
    /// not answer `FileBasicInfo` does.
    stamp_supported: bool,
    /// Makes `query` report `Unsupported`, as a non-Windows source would.
    query_unsupported: bool,
    /// Drops matches whose index is in this set, simulating a server-side
    /// pattern matcher that under-returns - the case the audit must catch.
    drop_from_query: Vec<usize>,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            latency: Duration::ZERO,
            fail_with: None,
            hang: false,
            stamp: None,
            stamp_error: None,
            stamp_supported: true,
            query_unsupported: false,
            drop_from_query: Vec::new(),
        }
    }
}

/// Decrements the in-flight count however the call leaves, including the early
/// returns for cancellation and scripted failures.
struct InFlight<'a>(&'a Mutex<(usize, usize)>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.lock().0 -= 1;
    }
}

/// One entry in a fake directory.
///
/// Carries the attribute word because that is exactly what a tree walk reads:
/// whether to list an entry as a file, and whether to descend into it. The
/// fake could previously only produce files, which made a walker untestable on
/// a machine with no network drives - that is, untestable at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeEntry {
    pub name: String,
    pub attributes: u32,
}

impl FakeEntry {
    pub const FILE: u32 = 0x0000_0080;
    pub const DIRECTORY: u32 = 0x0000_0010;
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    pub const HIDDEN: u32 = 0x0000_0002;
    pub const SYSTEM: u32 = 0x0000_0004;

    /// A file Windows marks hidden - `Thumbs.db`, a `~$` Office lock file.
    pub fn hidden_file(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Self::FILE | Self::HIDDEN,
        }
    }

    /// A folder a file server marked system. Listed and walked regardless,
    /// which is the behaviour worth holding the walker to.
    pub fn system_dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Self::DIRECTORY | Self::SYSTEM,
        }
    }

    pub fn file(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Self::FILE,
        }
    }

    pub fn dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Self::DIRECTORY,
        }
    }

    /// A junction or directory symlink. Listed as neither a file nor something
    /// to descend into, which is the behaviour a walker has to be held to.
    pub fn junction(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Self::DIRECTORY | Self::REPARSE_POINT,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.attributes & Self::DIRECTORY != 0
    }
}

/// A scriptable directory source.
#[derive(Debug, Clone, Default)]
pub struct FakeDirSource {
    dirs: Arc<Mutex<HashMap<PathBuf, Vec<FakeEntry>>>>,
    behaviour: Arc<Mutex<Behaviour>>,
    calls: Arc<Mutex<Vec<Call>>>,
    /// Per-directory failures, for the access-denied-subtree case that cannot
    /// be reproduced any other way here.
    dir_errors: Arc<Mutex<HashMap<PathBuf, EnumError>>>,
    /// Junction path rewrites: link -> target, applied as a prefix.
    junctions: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    /// Concurrent `list` calls, now and at their peak. A walker's concurrency
    /// limit is otherwise only observable by timing, which is no way to write
    /// a test.
    in_flight: Arc<Mutex<(usize, usize)>>,
}

impl FakeDirSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a directory and its files.
    pub fn with_dir(self, dir: impl Into<PathBuf>, names: &[&str]) -> Self {
        self.dirs.lock().insert(
            dir.into(),
            names.iter().map(|s| FakeEntry::file(*s)).collect(),
        );
        self
    }

    /// Registers a directory whose entries carry attributes of their own.
    ///
    /// [`Self::with_dir`] makes every name an ordinary file, which is what
    /// almost every test wants. This is for the ones about what an attribute
    /// means - hidden, system, a junction.
    pub fn with_entries(self, dir: impl Into<PathBuf>, entries: Vec<FakeEntry>) -> Self {
        self.dirs.lock().insert(dir.into(), entries);
        self
    }

    /// Registers a directory holding `n` generated files.
    pub fn with_synthetic(self, dir: impl Into<PathBuf>, n: usize) -> Self {
        let names = (0..n)
            .map(|i| FakeEntry::file(format!("job_{i:07}_report.pdf")))
            .collect();
        self.dirs.lock().insert(dir.into(), names);
        self
    }

    /// Declares a file at `path`, creating every ancestor directory and the
    /// directory entry each one needs inside its parent.
    ///
    /// The only tree-building primitive, deliberately: declaring a directory's
    /// entry in its parent and its own listing as two separate steps is how a
    /// fake ends up internally inconsistent, and a walker proved correct
    /// against a tree that could not exist has been proved nothing.
    pub fn with_file(self, path: impl AsRef<Path>) -> Self {
        self.add_path(path.as_ref());
        self
    }

    /// Declares many files at once, each relative to `root`.
    pub fn with_tree(self, root: impl AsRef<Path>, rel_paths: &[&str]) -> Self {
        let root = root.as_ref();
        // The root exists even if it holds nothing, or a walk of an empty
        // share would report "no such directory".
        self.dirs.lock().entry(root.to_path_buf()).or_default();
        for rel in rel_paths {
            self.add_path(&root.join(rel));
        }
        self
    }

    /// Inserts `entry` into `dir`, replacing any entry of the same name.
    fn put(&self, dir: &Path, entry: FakeEntry) {
        let mut dirs = self.dirs.lock();
        let listing = dirs.entry(dir.to_path_buf()).or_default();
        if let Some(slot) = listing.iter_mut().find(|e| e.name == entry.name) {
            *slot = entry;
        } else {
            listing.push(entry);
        }
    }

    /// Registers a file and every directory above it.
    fn add_path(&self, path: &Path) {
        let Some(parent) = path.parent() else { return };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        self.add_dirs(parent);
        self.put(parent, FakeEntry::file(name));
    }

    /// Makes `dir` exist, along with every directory above it.
    fn add_dirs(&self, dir: &Path) {
        self.dirs.lock().entry(dir.to_path_buf()).or_default();
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        // A volume root's parent is itself in spirit; stop rather than
        // inventing an entry for `V:\` inside `V:\`.
        if parent.as_os_str().is_empty() || parent == dir {
            return;
        }
        self.add_dirs(parent);
        self.put(parent, FakeEntry::dir(name));
    }

    /// Declares `name` inside `dir` as a junction pointing at `target`, and
    /// registers the target too - so a walker that wrongly descends actually
    /// loops and the test notices, instead of the fake quietly making the bug
    /// unreachable.
    pub fn with_junction(
        self,
        dir: impl AsRef<Path>,
        name: &str,
        target: impl AsRef<Path>,
    ) -> Self {
        self.add_dirs(dir.as_ref());
        self.put(dir.as_ref(), FakeEntry::junction(name));
        // Recorded as a path rewrite rather than a copy of the target's
        // listing. A copy aliases one level only, so a junction pointing at an
        // ancestor would dead-end instead of looping - and a cycle that cannot
        // happen is a cycle the walker cannot be held to surviving.
        self.junctions
            .lock()
            .insert(dir.as_ref().join(name), target.as_ref().to_path_buf());
        self
    }

    /// Rewrites any junction prefix of `dir`, as the server would.
    ///
    /// Bounded rather than looped to a fixed point: a junction chain that
    /// never settles is a real thing to model, and hanging the fake is not the
    /// way to model it.
    fn resolve(&self, dir: &Path) -> PathBuf {
        let junctions = self.junctions.lock();
        if junctions.is_empty() {
            return dir.to_path_buf();
        }
        let mut cur = dir.to_path_buf();
        for _ in 0..64 {
            let mut rewritten = false;
            for (link, target) in junctions.iter() {
                if let Ok(rest) = cur.strip_prefix(link) {
                    cur = target.join(rest);
                    rewritten = true;
                    break;
                }
            }
            if !rewritten {
                break;
            }
        }
        cur
    }

    /// Fails one directory, leaving the rest of the tree readable.
    pub fn fail_dir(&self, dir: impl Into<PathBuf>, err: EnumError) {
        self.dir_errors.lock().insert(dir.into(), err);
    }

    /// The most `list` calls that were ever in flight at once.
    pub fn peak_in_flight(&self) -> usize {
        self.in_flight.lock().1
    }

    pub fn set_dir(&self, dir: impl Into<PathBuf>, names: &[&str]) {
        self.dirs.lock().insert(
            dir.into(),
            names.iter().map(|s| FakeEntry::file(*s)).collect(),
        );
    }

    pub fn add_file(&self, dir: impl Into<PathBuf>, name: &str) {
        self.dirs
            .lock()
            .entry(dir.into())
            .or_default()
            .push(FakeEntry::file(name));
    }

    pub fn remove_file(&self, dir: impl AsRef<Path>, name: &str) {
        if let Some(v) = self.dirs.lock().get_mut(dir.as_ref()) {
            v.retain(|e| e.name != name);
        }
    }

    pub fn set_latency(&self, d: Duration) {
        self.behaviour.lock().latency = d;
    }

    pub fn set_error(&self, e: Option<EnumError>) {
        self.behaviour.lock().fail_with = e;
    }

    pub fn set_hang(&self, hang: bool) {
        self.behaviour.lock().hang = hang;
    }

    pub fn set_stamp(&self, stamp: Option<DirStamp>) {
        self.behaviour.lock().stamp = stamp;
    }

    /// Advances the stamp, as adding or removing an entry would.
    ///
    /// Shorthand for the common case, so a test that means "the directory
    /// changed" does not have to invent timestamp numbers.
    pub fn bump_stamp(&self) {
        let mut b = self.behaviour.lock();
        let next = b.stamp.map_or(2, |s| s.last_write.saturating_add(1));
        b.stamp = Some(DirStamp::new(next, next));
    }

    /// Fails the freshness probe while leaving enumeration working.
    pub fn set_stamp_error(&self, err: Option<EnumError>) {
        self.behaviour.lock().stamp_error = err;
    }

    /// Makes the share refuse the probe outright, the way one that will not
    /// answer `FileBasicInfo` does.
    pub fn set_stamp_supported(&self, supported: bool) {
        self.behaviour.lock().stamp_supported = supported;
    }

    pub fn set_query_unsupported(&self, unsupported: bool) {
        self.behaviour.lock().query_unsupported = unsupported;
    }

    /// Makes the server-side filter silently omit the given match positions.
    pub fn set_query_drops(&self, drops: Vec<usize>) {
        self.behaviour.lock().drop_from_query = drops;
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().len()
    }

    pub fn clear_calls(&self) {
        self.calls.lock().clear();
    }

    /// How many times `dir` was enumerated. The de-duplication assertion, and
    /// the closest thing the crate has to a rebuild counter.
    pub fn list_count(&self, dir: impl AsRef<Path>) -> usize {
        let dir = dir.as_ref();
        self.calls
            .lock()
            .iter()
            .filter(|c| matches!(c, Call::List(p) if p == dir))
            .count()
    }

    /// How many times `dir`'s stamp was probed.
    ///
    /// The counterpart to `list_count`: a healthy session should show many of
    /// these and almost none of those.
    pub fn stamp_count(&self, dir: impl AsRef<Path>) -> usize {
        let dir = dir.as_ref();
        self.calls
            .lock()
            .iter()
            .filter(|c| matches!(c, Call::Stamp(p) if p == dir))
            .count()
    }

    fn record(&self, call: Call) {
        self.calls.lock().push(call);
    }

    /// Counts one call in, and out again when the guard drops.
    fn enter(&self) -> InFlight<'_> {
        let mut c = self.in_flight.lock();
        c.0 += 1;
        c.1 = c.1.max(c.0);
        InFlight(&self.in_flight)
    }

    /// Applies scripted latency, hanging, and failure.
    fn gate(&self, opts: &ListOpts, cancel: &CancelToken) -> Result<(), EnumError> {
        let (latency, fail, hang) = {
            let b = self.behaviour.lock();
            (b.latency, b.fail_with, b.hang)
        };

        if hang {
            // A real blocked SMB call cannot be interrupted either; the honest
            // model is that it returns only when someone stops caring.
            let started = Instant::now();
            loop {
                if cancel.is_cancelled() {
                    return Err(EnumError::Cancelled);
                }
                if opts.expired() {
                    return Err(EnumError::TimedOut);
                }
                if started.elapsed() > Duration::from_secs(60) {
                    return Err(EnumError::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        if !latency.is_zero() {
            std::thread::sleep(latency);
        }
        if cancel.is_cancelled() {
            return Err(EnumError::Cancelled);
        }
        if let Some(e) = fail {
            return Err(e);
        }
        Ok(())
    }
}

impl DirSource for FakeDirSource {
    fn probe_stamp(&self, dir: &Path) -> Result<DirStamp, EnumError> {
        self.record(Call::Stamp(dir.to_path_buf()));
        // Checked before `gate`, so a share can be scripted to answer
        // enumerations while refusing the probe - the combination the real
        // failure took.
        {
            let b = self.behaviour.lock();
            if !b.stamp_supported {
                return Err(EnumError::Unsupported(50));
            }
            if let Some(err) = b.stamp_error {
                return Err(err);
            }
        }
        self.gate(&ListOpts::default(), &CancelToken::never())?;
        if !self.dirs.lock().contains_key(dir) {
            return Err(EnumError::PathNotFound(3));
        }
        Ok(self.behaviour.lock().stamp.unwrap_or(DirStamp::new(1, 1)))
    }

    fn list(
        &self,
        dir: &Path,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        self.record(Call::List(dir.to_path_buf()));
        let started = Instant::now();

        // Counted around everything below, so a walker's concurrency limit is
        // observable directly rather than inferred from timing.
        let _guard = self.enter();

        // Before the global gate: a scripted per-directory failure is about
        // this directory, not about the share.
        if let Some(err) = self.dir_errors.lock().get(dir).cloned() {
            return Err(err);
        }
        self.gate(opts, cancel)?;

        let names = self.dirs.lock().get(&self.resolve(dir)).cloned();
        let Some(names) = names else {
            return Err(EnumError::PathNotFound(3));
        };

        let mut entries = 0usize;
        let mut complete = true;
        for entry in &names {
            if cancel.is_cancelled() {
                return Err(EnumError::Cancelled);
            }
            // The real enumerators filter on attributes before the sink sees
            // anything; a fake that skipped this would let a walker pass here
            // and miss every subdirectory against a real share.
            if opts.files_only
                && !super::enumerate::is_listable_file_with(entry.attributes, opts.hide_system)
            {
                continue;
            }
            if entries >= opts.max_entries {
                complete = false;
                break;
            }
            let meta = EntryMeta {
                attributes: entry.attributes,
            };
            if !sink.push_str(&entry.name, meta) {
                complete = false;
                break;
            }
            entries += 1;
        }

        Ok(ListStats {
            entries,
            round_trips: 1,
            elapsed: started.elapsed(),
            strategy: None,
            complete,
        })
    }

    fn query(
        &self,
        dir: &Path,
        wildcard: &str,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        self.record(Call::Query(dir.to_path_buf(), wildcard.to_string()));
        if self.behaviour.lock().query_unsupported {
            return Err(EnumError::Unsupported(50));
        }
        let started = Instant::now();
        self.gate(opts, cancel)?;

        let names = self.dirs.lock().get(dir).cloned();
        let Some(names) = names else {
            return Err(EnumError::PathNotFound(3));
        };

        let drops = self.behaviour.lock().drop_from_query.clone();

        let mut entries = 0usize;
        let mut complete = true;
        let mut seen = 0usize;
        for entry in &names {
            // A server-side wildcard returns directories too; the caller
            // filters. Matching the real thing keeps `--bench`'s completeness
            // oracle honest about what it is comparing against.
            if opts.files_only
                && !super::enumerate::is_listable_file_with(entry.attributes, opts.hide_system)
            {
                continue;
            }
            if !wildcard_matches(wildcard, &entry.name) {
                continue;
            }
            if drops.contains(&seen) {
                seen += 1;
                continue;
            }
            seen += 1;
            if entries >= opts.max_entries {
                complete = false;
                break;
            }
            let meta = EntryMeta {
                attributes: entry.attributes,
            };
            if !sink.push_str(&entry.name, meta) {
                complete = false;
                break;
            }
            entries += 1;
        }

        if entries == 0 && complete {
            // Matches Win32: a wildcard that matches nothing reports
            // ERROR_FILE_NOT_FOUND, which is an answer rather than a failure.
            return Err(EnumError::Empty);
        }

        Ok(ListStats {
            entries,
            round_trips: 1,
            elapsed: started.elapsed(),
            strategy: None,
            complete,
        })
    }

    fn prewarm(&self, root: &Path) {
        self.record(Call::Prewarm(root.to_path_buf()));
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::enumerate::VecSink;

    fn src() -> FakeDirSource {
        FakeDirSource::new().with_dir("V:\\", &["Alpha.pdf", "beta.txt", "alpha_two.pdf"])
    }

    #[test]
    fn lists_the_registered_files() {
        let s = src();
        let mut sink = VecSink::default();
        let stats = s
            .list(
                Path::new("V:\\"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(stats.entries, 3);
        assert_eq!(sink.names.len(), 3);
    }

    #[test]
    fn an_unregistered_directory_reports_path_not_found() {
        let s = src();
        let mut sink = VecSink::default();
        assert!(matches!(
            s.list(
                Path::new("Q:\\"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never()
            ),
            Err(EnumError::PathNotFound(_))
        ));
    }

    /// The fake has to filter exactly as the real enumerators do, or a walker
    /// proved correct against it has been proved nothing - which is the same
    /// reason it filters on the directory bit.
    #[test]
    fn marked_files_are_listed_only_when_the_caller_allows_them() {
        let s = FakeDirSource::new().with_entries(
            "V:\\",
            vec![
                FakeEntry::file("sheet.pdf"),
                FakeEntry::hidden_file("Thumbs.db"),
                FakeEntry::system_dir("archive"),
            ],
        );

        let listed = |hide_system: bool| {
            let mut sink = VecSink::default();
            s.list(
                Path::new("V:\\"),
                &mut sink,
                &ListOpts::default().hiding_system(hide_system),
                &CancelToken::never(),
            )
            .unwrap();
            sink.names.sort();
            sink.names
        };

        assert_eq!(listed(false), vec!["Thumbs.db", "sheet.pdf"]);
        assert_eq!(listed(true), vec!["sheet.pdf"]);
    }

    #[test]
    fn injected_errors_surface_to_the_caller() {
        let s = src();
        s.set_error(Some(EnumError::Transient(53)));
        let mut sink = VecSink::default();
        assert_eq!(
            s.list(
                Path::new("V:\\"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never()
            ),
            Err(EnumError::Transient(53))
        );
    }

    #[test]
    fn a_hang_is_interrupted_by_cancellation() {
        let s = src();
        s.set_hang(true);
        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.current());

        let handle = {
            let s = s.clone();
            std::thread::spawn(move || {
                let mut sink = VecSink::default();
                s.list(Path::new("V:\\"), &mut sink, &ListOpts::default(), &token)
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        epoch.bump();
        assert_eq!(handle.join().unwrap(), Err(EnumError::Cancelled));
    }

    #[test]
    fn a_hang_is_bounded_by_the_deadline() {
        let s = src();
        s.set_hang(true);
        let mut sink = VecSink::default();
        let opts = ListOpts::default().with_deadline(Instant::now() + Duration::from_millis(30));
        assert_eq!(
            s.list(Path::new("V:\\"), &mut sink, &opts, &CancelToken::never()),
            Err(EnumError::TimedOut)
        );
    }

    #[test]
    fn star_matches_zero_or_more_characters() {
        assert!(wildcard_matches("*", "anything.txt"));
        assert!(wildcard_matches("*alpha*", "xx_ALPHA_yy"));
        assert!(wildcard_matches("*alpha*", "alpha"));
        assert!(!wildcard_matches("*alpha*", "beta"));
        // Unlike the legacy DOS star, `*` crosses a dot.
        assert!(wildcard_matches("*a*", "x.a"));
    }

    /// The property the `--bench` completeness oracle depends on.
    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(wildcard_matches("?", "a"));
        assert!(!wildcard_matches("?", ""));
        assert!(!wildcard_matches("?", "ab"));
        assert!(wildcard_matches("???", "abc"));
        assert!(!wildcard_matches("???", "abcd"));
        assert!(wildcard_matches("????????*", "abcdefgh"));
        assert!(wildcard_matches("????????*", "abcdefghij"));
        assert!(!wildcard_matches("????????*", "abcdefg"));
    }

    /// Partitioning by name length must be disjoint and complete, which is
    /// exactly what the oracle asserts against the real server.
    #[test]
    fn a_length_partition_covers_every_name_exactly_once() {
        let names = ["a", "bb", "ccc", "dddd", "eeeeeeee", "ffffffffffff"];
        for name in names {
            let hits = (1..=7usize)
                .map(|k| "?".repeat(k))
                .chain(std::iter::once("????????*".to_string()))
                .filter(|p| wildcard_matches(p, name))
                .count();
            assert_eq!(
                hits, 1,
                "{name} matched {hits} shards, expected exactly one"
            );
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(wildcard_matches("*ALPHA*", "my_alpha_file"));
        assert!(wildcard_matches("*alpha*", "MY_ALPHA_FILE"));
    }

    #[test]
    fn query_filters_server_side() {
        let s = src();
        let mut sink = VecSink::default();
        s.query(
            Path::new("V:\\"),
            "*alpha*",
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(sink.names, vec!["Alpha.pdf", "alpha_two.pdf"]);
    }

    #[test]
    fn a_query_matching_nothing_is_empty_not_a_failure() {
        let s = src();
        let mut sink = VecSink::default();
        assert_eq!(
            s.query(
                Path::new("V:\\"),
                "*zzz*",
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never()
            ),
            Err(EnumError::Empty)
        );
    }

    #[test]
    fn a_source_can_pretend_it_cannot_push_filters_down() {
        let s = src();
        s.set_query_unsupported(true);
        let mut sink = VecSink::default();
        assert!(matches!(
            s.query(
                Path::new("V:\\"),
                "*a*",
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never()
            ),
            Err(EnumError::Unsupported(_))
        ));
    }

    #[test]
    fn a_query_can_be_scripted_to_under_return() {
        // The exact failure the server-filter audit exists to detect.
        let s = src();
        s.set_query_drops(vec![0]);
        let mut sink = VecSink::default();
        s.query(
            Path::new("V:\\"),
            "*alpha*",
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(sink.names, vec!["alpha_two.pdf"], "one match was dropped");
    }

    /// The seam whose absence hid the reload bug: a share that enumerates
    /// fine but will not answer the freshness probe.
    #[test]
    fn the_probe_can_fail_while_enumeration_still_works() {
        let s = src();
        s.set_stamp_supported(false);

        assert_eq!(
            s.probe_stamp(Path::new("V:\\")),
            Err(EnumError::Unsupported(50))
        );

        let mut sink = VecSink::default();
        let stats = s
            .list(
                Path::new("V:\\"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(stats.entries, 3, "the listing must be unaffected");
    }

    #[test]
    fn an_injected_probe_error_is_reported_verbatim() {
        let s = src();
        s.set_stamp_error(Some(EnumError::AccessDenied(5)));
        assert_eq!(
            s.probe_stamp(Path::new("V:\\")),
            Err(EnumError::AccessDenied(5))
        );
        s.set_stamp_error(None);
        assert!(s.probe_stamp(Path::new("V:\\")).is_ok());
    }

    #[test]
    fn bumping_the_stamp_models_an_entry_being_added() {
        let s = src();
        let before = s.probe_stamp(Path::new("V:\\")).unwrap();
        s.bump_stamp();
        let after = s.probe_stamp(Path::new("V:\\")).unwrap();
        assert_ne!(before, after, "a changed directory must look changed");
    }

    #[test]
    fn probes_and_listings_are_counted_separately() {
        let s = src();
        let mut sink = VecSink::default();
        let _ = s.probe_stamp(Path::new("V:\\"));
        let _ = s.probe_stamp(Path::new("V:\\"));
        let _ = s.list(
            Path::new("V:\\"),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        );
        assert_eq!(s.stamp_count("V:\\"), 2);
        assert_eq!(s.list_count("V:\\"), 1);
    }

    #[test]
    fn records_every_call_for_dedup_assertions() {
        let s = src();
        let mut sink = VecSink::default();
        s.prewarm(Path::new("V:\\"));
        let _ = s.probe_stamp(Path::new("V:\\"));
        let _ = s.list(
            Path::new("V:\\"),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        );
        assert_eq!(
            s.calls(),
            vec![
                Call::Prewarm(PathBuf::from("V:\\")),
                Call::Stamp(PathBuf::from("V:\\")),
                Call::List(PathBuf::from("V:\\")),
            ]
        );
        assert_eq!(s.list_count("V:\\"), 1);
    }

    #[test]
    fn synthetic_directories_scale_without_ceremony() {
        let s = FakeDirSource::new().with_synthetic("V:\\", 10_000);
        let mut sink = crate::index::enumerate::CountingSink::default();
        let stats = s
            .list(
                Path::new("V:\\"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(stats.entries, 10_000);
    }
}
