//! Searching a share that is never read in full.
//!
//! # Why this is a kind of mapping rather than a fallback
//!
//! The rejected design was a runtime escape hatch: index everything, and when
//! a share turns out to be too large, quietly stop walking it and start
//! querying instead. That fails the way the old routing rules failed - the
//! share's behaviour changes under the user with no symptom, and "no matches"
//! comes to mean two different things on two different days. A configured kind
//! is a promise somebody made and can read back.
//!
//! # What one pass costs, exactly
//!
//! `FindFirstFileExW` evaluates the pattern on the server but within **one**
//! folder, so reach has to be bought a round trip at a time:
//!
//! * **One round trip** for the root's filtered query. It returns the matching
//!   files *and the matching subfolder names*, and the second half matters more
//!   than the first - a job code names a folder at least as often as a file, so
//!   at the shipped depth of one this single round trip answers the common
//!   question outright.
//! * **One more per matching subfolder**, to list what is inside it. This
//!   mirrors [`crate::search::matcher`]'s folder pass exactly, so a live share
//!   and a walked tree rank a folder match the same way.
//! * **One more per folder expanded, and only below depth one**, to learn the
//!   folders whose names *do not* match - which no filtered query can reveal.
//!   That is the expensive half, and the reason depth is per-mapping and
//!   defaults to one.
//!
//! # Why the coverage travels with the hits
//!
//! Everything here answers a bounded fraction of the question. A caller that
//! cannot tell "nothing matched on the share" from "nothing matched in the
//! fourteen folders I could afford" will render the second as the first, which
//! is the one lie this program is built not to tell. So [`LiveOutcome`] has no
//! variant that reports hits without also reporting reach.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::matcher::Hit;
use super::pattern::{self, PatternReject};
use super::query::Query;
use crate::config::hidden::Hidden;
use crate::config::{
    LIVE_DEADLINE, LIVE_FAILURE_LIMIT, LIVE_MIN_SPACING, LIVE_ROUND_TRIP_BUDGET,
    MAX_FILES_PER_FOLDER, MAX_SERVER_HITS,
};
use crate::index::EnumError;
use crate::index::enumerate::{DirSource, EntryMeta, EntrySink, ListOpts, is_listable_file};
use crate::paths::MappingId;
use crate::util::cancel::CancelToken;

/// Splits an enumeration into the files and the folders it returned.
///
/// The two are wanted separately and a single `Vec` of names cannot say which
/// is which, because the attribute is only in hand while the directory entry
/// is.
#[derive(Debug, Default)]
struct SplitSink {
    files: Vec<String>,
    dirs: Vec<String>,
    cap: usize,
}

impl SplitSink {
    fn with_cap(cap: usize) -> Self {
        Self {
            cap,
            ..Self::default()
        }
    }

    fn len(&self) -> usize {
        self.files.len() + self.dirs.len()
    }
}

impl EntrySink for SplitSink {
    fn push_wide(&mut self, name: &[u16], meta: EntryMeta) -> bool {
        self.push_str(&String::from_utf16_lossy(name), meta)
    }

    fn push_str(&mut self, name: &str, meta: EntryMeta) -> bool {
        if crate::index::enumerate::is_dot_entry(name) {
            return true;
        }
        if is_listable_file(meta.attributes) {
            self.files.push(name.to_string());
        } else if crate::index::enumerate::is_walkable_dir(meta.attributes) {
            self.dirs.push(name.to_string());
        }
        self.len() < self.cap
    }

    fn accepted(&self) -> usize {
        self.len()
    }
}

/// Why a pass stopped before it had finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveStop {
    /// Spent its round trips.
    Budget,
    /// Ran out of time.
    Deadline,
    /// The server returned more matches than are worth carrying.
    Capped,
    /// A later keystroke superseded it.
    Cancelled,
}

impl LiveStop {
    pub fn label(self) -> &'static str {
        match self {
            Self::Budget => "it had asked as much as it may",
            Self::Deadline => "it ran out of time",
            Self::Capped => "there were too many matches",
            Self::Cancelled => "you kept typing",
        }
    }
}

/// What a pass reached, and what it did not.
///
/// Deliberately not folded into `SearchOutcome`: that type describes a sweep
/// over an index this process owns, where "how much of the share did you look
/// at" has one answer and it is "all of it".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveCoverage {
    pub dirs_queried: u32,
    /// Folders known to exist and never asked about.
    ///
    /// The number that matters: it is the size of the hole in the answer.
    pub dirs_skipped: u32,
    pub round_trips: u32,
    pub deepest: u16,
    pub stopped: Option<LiveStop>,
    pub elapsed: Duration,
}

impl LiveCoverage {
    /// True when everything the configured depth names was reached.
    pub fn complete(&self) -> bool {
        self.dirs_skipped == 0 && self.stopped.is_none()
    }
}

/// Why a share was not asked at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSkip {
    /// The query cannot be expressed as a literal pattern, so the server
    /// cannot be asked. Named rather than silent: on a live share this is the
    /// difference between "no matches" and "not searched".
    Pattern(PatternReject),
    /// Asked too recently.
    ///
    /// Carries how long until it may be asked again, so the footer can count
    /// down rather than going quiet.
    Throttled { retry_in: Duration },
    /// The source will not push a filter down, so every query would be a full
    /// enumeration wearing a filter.
    Unsupported,
    /// Switched off for the rest of the process after repeated refusals.
    Disabled { failures: u32 },
}

impl LiveSkip {
    pub fn label(&self) -> String {
        match self {
            Self::Pattern(r) => r.label(),
            Self::Throttled { .. } => "it was just asked".into(),
            Self::Unsupported => "this drive cannot search for you".into(),
            Self::Disabled { .. } => "searching this drive is switched off".into(),
        }
    }
}

/// What a pass produced.
#[derive(Debug, Clone)]
pub enum LiveOutcome {
    Answered {
        hits: Vec<Hit>,
        matched: u32,
        coverage: LiveCoverage,
    },
    Skipped(LiveSkip),
    Failed(EnumError),
}

/// Sentinel for a share nothing has asked about yet.
const NEVER_ASKED: u64 = u64::MAX;

/// One share that is searched by asking the file server.
pub struct LiveShare {
    id: MappingId,
    root: PathBuf,
    depth: u16,
    /// Roots of other enabled mappings beneath this one.
    ///
    /// They are already indexed, so querying them as well would return every
    /// file under them twice. This is what makes a walked tree *inside* a live
    /// share a legal - and useful - configuration.
    excluded: Box<[PathBuf]>,
    source: Arc<dyn DirSource>,
    /// Nanoseconds since `started` at the last query, or [`NEVER_ASKED`].
    ///
    /// The sentinel is not zero, and that is not fastidiousness: two `Instant`s
    /// taken microseconds apart can be equal on Windows, so a share asked at
    /// the moment it was constructed would store zero and then read it back as
    /// "never asked" - and the floor would let the next query straight
    /// through. A test caught exactly that.
    ///
    /// An atomic rather than a field on the state machine, because it is a
    /// property of the share rather than of the interaction: a paste, a held
    /// key and a snapshot-change re-run all have to be bounded by the same
    /// number, and only one of them goes through the keyboard.
    started: Instant,
    last_query_nanos: AtomicU64,
    failures: AtomicU32,
    enabled: AtomicBool,
}

impl LiveShare {
    pub fn new(
        id: MappingId,
        root: PathBuf,
        depth: u16,
        excluded: Vec<PathBuf>,
        source: Arc<dyn DirSource>,
    ) -> Self {
        Self {
            id,
            root,
            depth: depth.max(1),
            excluded: excluded.into_boxed_slice(),
            source,
            started: Instant::now(),
            last_query_nanos: AtomicU64::new(NEVER_ASKED),
            failures: AtomicU32::new(0),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn id(&self) -> MappingId {
        self.id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn depth(&self) -> u16 {
        self.depth
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn failures(&self) -> u32 {
        self.failures.load(Ordering::Relaxed)
    }

    /// Whether this share may be asked now, and if not how long until it may.
    fn admission(&self, now: Instant) -> Result<(), LiveSkip> {
        if !self.is_enabled() {
            return Err(LiveSkip::Disabled {
                failures: self.failures(),
            });
        }
        let last = self.last_query_nanos.load(Ordering::Relaxed);
        if last != NEVER_ASKED {
            let since = now.saturating_duration_since(self.started);
            let elapsed = since.saturating_sub(Duration::from_nanos(last));
            if elapsed < LIVE_MIN_SPACING {
                return Err(LiveSkip::Throttled {
                    retry_in: LIVE_MIN_SPACING - elapsed,
                });
            }
        }
        Ok(())
    }

    fn note_query(&self, now: Instant) {
        let since = now.saturating_duration_since(self.started);
        self.last_query_nanos
            .store(since.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Records a refusal, switching the share off after enough of them.
    ///
    /// Mirrors the verifier's self-disabling for the same reason: a mechanism
    /// that cannot be tested against the real server has to be able to take
    /// itself out of service, and say that it has.
    fn note_failure(&self) -> u32 {
        let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= LIVE_FAILURE_LIMIT {
            self.enabled.store(false, Ordering::Relaxed);
        }
        n
    }

    fn note_success(&self) {
        self.failures.store(0, Ordering::Relaxed);
    }

    /// True when `dir` is the root of some other enabled mapping.
    fn is_excluded(&self, dir: &Path) -> bool {
        self.excluded
            .iter()
            .any(|e| crate::util::winpath::same_dir(e, dir))
    }

    /// Asks the server about this share.
    pub fn search(
        &self,
        query: &Query,
        hidden: &Hidden,
        now: Instant,
        cancel: &CancelToken,
    ) -> LiveOutcome {
        if let Err(skip) = self.admission(now) {
            return LiveOutcome::Skipped(skip);
        }
        // Folders as well as files, so the extension stays off the pattern.
        // See `wildcard_for_entries`.
        let wildcard = match pattern::wildcard_for_entries(query) {
            Ok(w) => w,
            Err(reject) => return LiveOutcome::Skipped(LiveSkip::Pattern(reject)),
        };
        self.note_query(now);

        let started = Instant::now();
        let deadline = started + LIVE_DEADLINE;
        let mut pass = Pass {
            share: self,
            query,
            hidden,
            wildcard: &wildcard,
            deadline,
            budget: LIVE_ROUND_TRIP_BUDGET,
            hits: Vec::new(),
            matched: 0,
            coverage: LiveCoverage::default(),
            cancel,
        };

        match pass.run() {
            Err(err) => {
                // An unreachable share is not a refusal to filter, so it does
                // not count against the limit: the share may simply be down,
                // and disabling the search for the rest of the process would
                // outlive the outage.
                if matches!(err, EnumError::Unsupported(_)) {
                    let failures = self.note_failure();
                    return LiveOutcome::Skipped(if self.is_enabled() {
                        LiveSkip::Unsupported
                    } else {
                        LiveSkip::Disabled { failures }
                    });
                }
                LiveOutcome::Failed(err)
            }
            Ok(()) => {
                self.note_success();
                let mut coverage = pass.coverage;
                coverage.elapsed = started.elapsed();
                LiveOutcome::Answered {
                    hits: pass.hits,
                    matched: pass.matched,
                    coverage,
                }
            }
        }
    }
}

/// One pass over one share. Exists so the bookkeeping is not eleven arguments.
struct Pass<'a> {
    share: &'a LiveShare,
    query: &'a Query,
    hidden: &'a Hidden,
    wildcard: &'a str,
    deadline: Instant,
    budget: u32,
    hits: Vec<Hit>,
    matched: u32,
    coverage: LiveCoverage,
    cancel: &'a CancelToken,
}

impl Pass<'_> {
    /// Spends one round trip, or reports why it cannot.
    fn spend(&mut self) -> Result<(), LiveStop> {
        if self.cancel.is_cancelled() {
            return Err(LiveStop::Cancelled);
        }
        if self.budget == 0 {
            return Err(LiveStop::Budget);
        }
        if Instant::now() >= self.deadline {
            return Err(LiveStop::Deadline);
        }
        self.budget -= 1;
        self.coverage.round_trips += 1;
        Ok(())
    }

    fn opts(&self) -> ListOpts {
        ListOpts {
            // Folders matter as much as files here: a job code names a folder
            // at least as often as it names a file, and the filtered query is
            // the only thing that will ever reveal one.
            files_only: false,
            ..ListOpts::default()
        }
        .with_deadline(self.deadline)
        .with_max_entries(MAX_SERVER_HITS)
    }

    fn run(&mut self) -> Result<(), EnumError> {
        // Breadth-first, so that a pass which runs out of budget has spent it
        // on the shallowest folders - the ones most likely to hold what was
        // asked for, and the ones a person would have looked in first.
        let mut frontier: Vec<(PathBuf, u16)> = vec![(self.share.root.clone(), 0)];

        while let Some((dir, level)) = frontier.pop() {
            self.coverage.deepest = self.coverage.deepest.max(level);
            match self.visit(&dir, level, &mut frontier) {
                Ok(()) => {}
                Err(Halt::Stop(stop)) => {
                    self.coverage.stopped = Some(stop);
                    // Whatever is still queued is a hole in the answer, and
                    // saying how big it is is the whole point of this type.
                    self.coverage.dirs_skipped += frontier.len() as u32;
                    return Ok(());
                }
                Err(Halt::Enum(err)) => {
                    // The root failing is the share failing. A folder deeper
                    // down failing is one hole, not an outage - the same
                    // distinction the walker draws.
                    if level == 0 {
                        return Err(err);
                    }
                    self.coverage.dirs_skipped += 1;
                }
            }
        }
        Ok(())
    }

    fn visit(
        &mut self,
        dir: &Path,
        level: u16,
        frontier: &mut Vec<(PathBuf, u16)>,
    ) -> Result<(), Halt> {
        self.spend().map_err(Halt::Stop)?;

        let mut sink = SplitSink::with_cap(MAX_SERVER_HITS);
        match self
            .share
            .source
            .query(dir, self.wildcard, &mut sink, &self.opts(), self.cancel)
        {
            Ok(_) => {}
            // `FindFirstFileExW` reports "nothing matched the pattern" as
            // `ERROR_FILE_NOT_FOUND`, which is an answer rather than an
            // outage. Reading it as a failure would turn every unsuccessful
            // search on a live share into "the drive did not answer".
            Err(EnumError::Empty) => {}
            Err(err) => return Err(Halt::Enum(err)),
        }
        self.coverage.dirs_queried += 1;

        for name in &sink.files {
            self.take(dir, name, false);
        }

        // A folder whose own name matched brings its files with it, exactly as
        // the tree matcher does.
        for name in &sink.dirs {
            let child = dir.join(name);
            if self.share.is_excluded(&child) {
                continue;
            }
            self.expand(&child)?;
        }

        // Below the configured depth, the folders that did *not* match still
        // have to be visited - and only a full listing can name them.
        if level + 1 < self.share.depth {
            self.spend().map_err(Halt::Stop)?;
            let mut all = SplitSink::with_cap(MAX_SERVER_HITS);
            let opts = ListOpts {
                files_only: false,
                ..ListOpts::default()
            }
            .with_deadline(self.deadline);
            self.share
                .source
                .list(dir, &mut all, &opts, self.cancel)
                .map_err(Halt::Enum)?;
            for name in all.dirs {
                let child = dir.join(&name);
                if !self.share.is_excluded(&child) {
                    frontier.push((child, level + 1));
                }
            }
        }
        Ok(())
    }

    /// Lists a matching folder and takes what is in it.
    fn expand(&mut self, dir: &Path) -> Result<(), Halt> {
        match self.spend() {
            Ok(()) => {}
            Err(stop) => return Err(Halt::Stop(stop)),
        }
        let mut sink = SplitSink::with_cap(MAX_SERVER_HITS);
        let opts = ListOpts::default().with_deadline(self.deadline);
        match self.share.source.list(dir, &mut sink, &opts, self.cancel) {
            Ok(_) => {}
            Err(_) => {
                // One unreadable folder is a hole, not a failure: the rest of
                // the answer is still worth showing, and the count says one is
                // missing.
                self.coverage.dirs_skipped += 1;
                return Ok(());
            }
        }
        let mut shown = 0usize;
        for name in &sink.files {
            if self.take(dir, name, true) {
                shown += 1;
                // The same bound the tree matcher uses, and for the same
                // reason: one enormous folder must not fill every slot and
                // hide every other folder that matched.
                if shown >= MAX_FILES_PER_FOLDER {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Admits one name, if the query really wants it.
    ///
    /// The server pattern is deliberately a superset - see
    /// [`pattern::wildcard_for`] - so this is where a trailing match, and a
    /// filter naming several types, are narrowed back to what was asked for.
    fn take(&mut self, dir: &Path, name: &str, inherited: bool) -> bool {
        if self.hidden.hides(name) {
            return false;
        }
        if inherited {
            // The file's own name did not match; its folder did. Only the type
            // filter applies, for the reason the tree matcher gives.
            if !self
                .query
                .types()
                .admits_name(name.to_ascii_lowercase().as_bytes())
            {
                return false;
            }
        } else if !pattern::confirms(name, self.query) {
            return false;
        }

        self.matched = self.matched.saturating_add(1);
        if self.hits.len() < crate::config::MAX_RESULTS {
            let path = dir.join(name);
            self.hits.push(Hit {
                path: Arc::from(path.to_string_lossy().as_ref()),
                name: Arc::from(name),
                match_pos: if inherited {
                    u32::MAX
                } else {
                    position_of(name, self.query)
                },
                index: 0,
            });
        }
        true
    }
}

/// Where the query sits in a name the server returned, for the underline.
fn position_of(name: &str, query: &Query) -> u32 {
    let folded = crate::util::fold::fold_query(name);
    let needle = crate::util::fold::fold_query(query.term());
    match memchr::memmem::find(&folded, &needle) {
        Some(pos) => {
            crate::search::query::admits(&folded, &needle, pos as u32, query.mode(), query.types())
                .unwrap_or(pos as u32)
        }
        None => 0,
    }
}

/// How a visit ended badly.
enum Halt {
    Stop(LiveStop),
    Enum(EnumError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fake_source::{Call, FakeDirSource};

    const ROOT: &str = "V:\\archive";

    fn share(src: &Arc<FakeDirSource>, depth: u16) -> LiveShare {
        LiveShare::new(
            MappingId(0),
            PathBuf::from(ROOT),
            depth,
            Vec::new(),
            Arc::clone(src) as Arc<dyn DirSource>,
        )
    }

    fn ask(s: &LiveShare, line: &str) -> LiveOutcome {
        s.search(
            &Query::parse(line),
            &Hidden::none(),
            Instant::now(),
            &CancelToken::never(),
        )
    }

    fn answered(o: LiveOutcome) -> (Vec<String>, LiveCoverage) {
        match o {
            LiveOutcome::Answered { hits, coverage, .. } => {
                (hits.iter().map(|h| h.path.to_string()).collect(), coverage)
            }
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn a_matching_file_in_the_root_is_found_in_one_round_trip() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf", "other.pdf"]));
        let (paths, cover) = answered(ask(&share(&src, 1), "p12345"));
        assert_eq!(paths, ["V:\\archive\\p12345.pdf"]);
        assert_eq!(cover.round_trips, 1);
        assert!(cover.complete());
    }

    /// A job code names a folder at least as often as a file, which is what
    /// makes the shipped depth of one worth having at all.
    #[test]
    fn a_matching_folder_brings_its_files_with_it() {
        let src = Arc::new(
            FakeDirSource::new()
                .with_tree(ROOT, &["p12345\\a.pdf", "p12345\\b.dwg", "other\\c.pdf"]),
        );
        let (mut paths, _) = answered(ask(&share(&src, 1), "p12345"));
        paths.sort();
        assert_eq!(
            paths,
            ["V:\\archive\\p12345\\a.pdf", "V:\\archive\\p12345\\b.dwg"]
        );
    }

    #[test]
    fn a_type_filter_still_applies_to_what_a_folder_brings_in() {
        let src =
            Arc::new(FakeDirSource::new().with_tree(ROOT, &["p12345\\a.pdf", "p12345\\b.dwg"]));
        let (paths, _) = answered(ask(&share(&src, 1), "p12345 ext:pdf"));
        assert_eq!(paths, ["V:\\archive\\p12345\\a.pdf"]);
    }

    /// The server pattern is deliberately a superset, so the narrowing has to
    /// happen here or a trailing match would return the leading ones too.
    #[test]
    fn a_trailing_match_is_narrowed_back_from_the_loose_server_pattern() {
        let src =
            Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345-rev.pdf", "job-p12345.pdf"]));
        let (paths, _) = answered(ask(&share(&src, 1), "*p12345"));
        assert_eq!(paths, ["V:\\archive\\job-p12345.pdf"]);
    }

    /// `FindFirstFileExW` matches within one folder, so depth one cannot see a
    /// file two levels down. Saying how many folders were left unasked is the
    /// difference between "there is no such file" and "I did not look there".
    #[test]
    fn depth_one_reports_the_folders_it_did_not_reach() {
        let src = Arc::new(
            FakeDirSource::new().with_tree(ROOT, &["alpha\\beta\\p12345.pdf", "alpha\\x.txt"]),
        );
        let (paths, cover) = answered(ask(&share(&src, 1), "p12345"));
        assert!(paths.is_empty(), "depth one cannot reach it");
        assert_eq!(cover.deepest, 0);
        assert_eq!(cover.round_trips, 1);
    }

    #[test]
    fn a_greater_depth_reaches_further_and_costs_more_round_trips() {
        let src = Arc::new(
            FakeDirSource::new().with_tree(ROOT, &["alpha\\beta\\p12345.pdf", "alpha\\x.txt"]),
        );
        let (paths, cover) = answered(ask(&share(&src, 3), "p12345"));
        assert_eq!(paths, ["V:\\archive\\alpha\\beta\\p12345.pdf"]);
        assert!(cover.round_trips > 1, "{cover:?}");
    }

    /// The whole feature defeated in one line: a source that cannot push the
    /// filter down would answer every query with a full enumeration wearing a
    /// filter, which is the cost configuring the share this way avoids.
    #[test]
    fn a_source_that_cannot_filter_is_refused_rather_than_enumerated() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf"]));
        src.set_query_unsupported(true);
        let s = share(&src, 1);
        assert!(matches!(
            ask(&s, "p12345"),
            LiveOutcome::Skipped(LiveSkip::Unsupported)
        ));
        assert!(
            !src.calls()
                .iter()
                .any(|c| matches!(c, Call::List(d) if d == Path::new(ROOT))),
            "the root was enumerated after the filter was refused"
        );
    }

    #[test]
    fn repeated_refusals_switch_the_share_off_for_the_process() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf"]));
        src.set_query_unsupported(true);
        let s = share(&src, 1);
        for _ in 0..LIVE_FAILURE_LIMIT {
            // Each attempt has to clear the per-share floor.
            s.last_query_nanos.store(NEVER_ASKED, Ordering::Relaxed);
            let _ = ask(&s, "p12345");
        }
        assert!(!s.is_enabled());
        s.last_query_nanos.store(NEVER_ASKED, Ordering::Relaxed);
        assert!(matches!(
            ask(&s, "p12345"),
            LiveOutcome::Skipped(LiveSkip::Disabled { .. })
        ));
    }

    /// One client must cost a share at most one query per second however
    /// pathological its input - a held key, a paste loop, a burst of index
    /// updates each re-running the match.
    #[test]
    fn a_share_asked_twice_in_a_moment_is_only_asked_once() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf"]));
        let s = share(&src, 1);
        let now = Instant::now();
        let q = Query::parse("p12345");

        assert!(matches!(
            s.search(&q, &Hidden::none(), now, &CancelToken::never()),
            LiveOutcome::Answered { .. }
        ));
        match s.search(&q, &Hidden::none(), now, &CancelToken::never()) {
            LiveOutcome::Skipped(LiveSkip::Throttled { retry_in }) => {
                assert!(retry_in <= LIVE_MIN_SPACING);
            }
            other => panic!("expected a throttle, got {other:?}"),
        }
        assert_eq!(
            src.calls()
                .iter()
                .filter(|c| matches!(c, Call::Query(..)))
                .count(),
            1
        );
    }

    /// On a live share this is the difference between "no matches" and "not
    /// searched", so it is named rather than reported as an empty answer.
    #[test]
    fn a_query_that_cannot_be_a_pattern_is_skipped_with_a_reason() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf"]));
        match ask(&share(&src, 1), "\u{c9}coles") {
            LiveOutcome::Skipped(LiveSkip::Pattern(_)) => {}
            other => panic!("expected a pattern skip, got {other:?}"),
        }
    }

    /// A walked tree inside a live share is a legal configuration, and this is
    /// what stops it returning every file under it twice.
    #[test]
    fn a_folder_another_mapping_already_covers_is_never_queried() {
        let src = Arc::new(
            FakeDirSource::new().with_tree(ROOT, &["p12345\\a.pdf", "current\\p12345.pdf"]),
        );
        let s = LiveShare::new(
            MappingId(0),
            PathBuf::from(ROOT),
            2,
            vec![PathBuf::from("V:\\archive\\current")],
            Arc::clone(&src) as Arc<dyn DirSource>,
        );
        let (paths, _) = answered(ask(&s, "p12345"));
        assert_eq!(paths, ["V:\\archive\\p12345\\a.pdf"]);
        assert!(
            !src.calls()
                .iter()
                .any(|c| matches!(c, Call::List(d) | Call::Query(d, _) if d == Path::new("V:\\archive\\current"))),
            "the excluded folder was asked about anyway"
        );
    }

    #[test]
    fn an_unreachable_root_is_reported_as_a_failure_rather_than_an_empty_answer() {
        let src = Arc::new(FakeDirSource::new());
        match ask(&share(&src, 1), "p12345") {
            LiveOutcome::Failed(_) => {}
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_superseded_pass_stops_and_says_so() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf"]));
        let s = share(&src, 1);
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let token = CancelToken::from_flag(flag);
        let out = s.search(
            &Query::parse("p12345"),
            &Hidden::none(),
            Instant::now(),
            &token,
        );
        match out {
            LiveOutcome::Answered { coverage, .. } => {
                assert_eq!(coverage.stopped, Some(LiveStop::Cancelled));
                assert!(!coverage.complete());
            }
            other => panic!("expected a stopped answer, got {other:?}"),
        }
    }

    #[test]
    fn the_hidden_list_still_applies_to_what_the_server_returned() {
        let src = Arc::new(FakeDirSource::new().with_dir(ROOT, &["p12345.pdf", "p12345.db"]));
        let s = share(&src, 1);
        let out = s.search(
            &Query::parse("p12345"),
            &Hidden::new(&["db"], false),
            Instant::now(),
            &CancelToken::never(),
        );
        let (paths, _) = answered(out);
        assert_eq!(paths, ["V:\\archive\\p12345.pdf"]);
    }
}
