//! Verifies a local result against the file server.
//!
//! The local index may be minutes old. Verification avoids a full enumeration:
//!
//! 1. **Probe the directory stamp** (three round trips). If the timestamps have
//!    not moved since the snapshot, the local index is provably current.
//! 2. **Only if it changed**, issue a server-side wildcard query, which returns
//!    just the matches in one round trip.
//!
//! # The audit
//!
//! Server-side matching is `FsRtlIsNameInExpression` on a Microsoft server, but
//! the share may run a different matcher that returns fewer files than an
//! enumeration would, hiding files silently. Every server result is therefore
//! compared with the local index: the server may return more (new files) but
//! never less. After
//! [`SERVER_FILTER_MISS_LIMIT`](crate::config::SERVER_FILTER_MISS_LIMIT)
//! disagreements the mechanism disables itself for the rest of the process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::matcher::Hit;
use super::pattern::{self, PatternReject};
use super::query::{self, Query};
use crate::config::hidden::Hidden;
use crate::config::{MAX_SERVER_HITS, SERVER_FILTER_MISS_LIMIT};
use crate::index::DirStamp;
use crate::index::enumerate::{DirSource, ListOpts, VecSink};
use crate::index::errors::EnumError;
use crate::index::snapshot::Snapshot;
use crate::paths::Routes;
use crate::util::cancel::CancelToken;
use crate::util::fold;

/// Why verification did not reach the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The query cannot be expressed as a literal pattern.
    Pattern(PatternReject),
    /// Turned off by configuration.
    Disabled,
    /// Turned off after the audit caught the server under-returning.
    AuditFailed { misses: u32 },
    /// The source cannot push filters down at all.
    Unsupported,
    /// Several flat shares are configured, so the server-side check was not
    /// attempted.
    ///
    /// The check replaces the on-screen list with the server's answer, which
    /// across several shares would require per-share merging and auditing.
    SeveralShares,
    /// There is no single directory to ask about.
    ///
    /// A walked tree spans many folders and server-side matching works within
    /// one. Freshness comes from the change watcher and re-walk instead. Reported
    /// so that "not checked" is distinguishable from "checked and fine".
    NotApplicable,
}

impl SkipReason {
    pub fn label(&self) -> String {
        match self {
            Self::Pattern(p) => p.label(),
            Self::Disabled => "server-side filtering is off".into(),
            Self::AuditFailed { misses } => {
                format!("server filter disabled after {misses} disagreements")
            }
            Self::Unsupported => "server-side filtering unavailable".into(),
            Self::SeveralShares => "several drives; not checked against the server".into(),
            Self::NotApplicable => "kept live by the watcher".into(),
        }
    }
}

/// What verification concluded.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// The directory has not changed, so the local results are authoritative.
    /// This is the most common answer.
    IndexAuthoritative {
        stamp: Option<DirStamp>,
    },
    /// The server answered with the current match set.
    Server {
        hits: Vec<Hit>,
        matched: u32,
        capped: bool,
        audit: AuditVerdict,
    },
    Skipped(SkipReason),
    Failed(EnumError),
}

/// Result of comparing a server answer against the local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditVerdict {
    /// No local snapshot to compare against.
    NotChecked,
    /// The server returned everything the local index would have.
    Consistent,
    /// The server omitted names the local index holds. Each occurrence counts
    /// against the disable threshold.
    ServerUnderReturned { missing: Vec<String> },
}

/// Compares a server result against the names the local index matched.
///
/// The server may return more (files created since the snapshot), never less.
pub fn audit(server_names: &[String], local_names: &[String]) -> AuditVerdict {
    if local_names.is_empty() {
        return AuditVerdict::Consistent;
    }
    let server: std::collections::HashSet<String> = server_names
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let missing: Vec<String> = local_names
        .iter()
        .filter(|n| !server.contains(&n.to_ascii_lowercase()))
        .cloned()
        .collect();
    if missing.is_empty() {
        AuditVerdict::Consistent
    } else {
        AuditVerdict::ServerUnderReturned { missing }
    }
}

/// Returns the names the local snapshot would return for `query`, for the
/// audit.
///
/// Uses the matcher's own predicate so that names correctly removed by the
/// query's filters are not counted as server misses.
fn local_matches(snapshot: &Snapshot, query: &Query) -> Vec<String> {
    let needle = fold::fold_query(query.term());
    (0..snapshot.len() as u32)
        .filter(|&i| {
            let name = snapshot.name_lower(i);
            memchr::memmem::find(name, &needle).is_some_and(|pos| {
                query::admits(name, &needle, pos as u32, query.mode(), query.types()).is_some()
            })
        })
        .map(|i| snapshot.display_name(i).into_owned())
        .collect()
}

/// Runs server-side verification and audits its results.
pub struct Verifier {
    source: Arc<dyn DirSource>,
    /// Every enabled flat mapping, in configuration order.
    ///
    /// Trees are excluded because they record no directory stamp.
    dirs: Box<[PathBuf]>,
    enabled: AtomicBool,
    misses: AtomicU32,
}

impl Verifier {
    pub fn new(source: Arc<dyn DirSource>, dir: PathBuf, enabled: bool) -> Self {
        Self::over(source, vec![dir], enabled)
    }

    /// Builds one over every enabled flat mapping in a routing table.
    pub fn for_routes(source: Arc<dyn DirSource>, routes: &Routes, enabled: bool) -> Self {
        let dirs = routes
            .flat()
            .map(|m| m.path.clone())
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        Self::over(source, dirs, enabled)
    }

    fn over(source: Arc<dyn DirSource>, dirs: Vec<PathBuf>, enabled: bool) -> Self {
        Self {
            source,
            dirs: dirs.into_boxed_slice(),
            enabled: AtomicBool::new(enabled),
            misses: AtomicU32::new(0),
        }
    }

    /// Returns whether any share is eligible for a server-side check.
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u32 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Permanently disables server-side filtering for this process.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// The single flat share, when there is exactly one.
    pub fn dir(&self) -> &Path {
        self.dirs
            .first()
            .map(PathBuf::as_path)
            .unwrap_or(Path::new(""))
    }

    /// Checks whether the directory has changed since `snapshot` was taken.
    ///
    /// `Some(stamp)` means unchanged and therefore authoritative.
    pub fn stamp_unchanged(&self, snapshot: Option<&Snapshot>) -> Option<DirStamp> {
        let recorded = snapshot?.stamp()?;
        let current = self.source.probe_stamp(self.dir()).ok()?;
        (current == recorded).then_some(current)
    }

    /// Verifies `query`.
    pub fn verify(
        &self,
        query: &Query,
        snapshot: Option<&Snapshot>,
        hidden: &Hidden,
        cancel: &CancelToken,
    ) -> VerifyOutcome {
        // Step 1: an unchanged stamp makes everything below unnecessary.
        if let Some(stamp) = self.stamp_unchanged(snapshot) {
            return VerifyOutcome::IndexAuthoritative { stamp: Some(stamp) };
        }
        if cancel.is_cancelled() {
            return VerifyOutcome::Failed(EnumError::Cancelled);
        }

        if !self.is_enabled() {
            let misses = self.misses();
            return VerifyOutcome::Skipped(if misses >= SERVER_FILTER_MISS_LIMIT {
                SkipReason::AuditFailed { misses }
            } else {
                SkipReason::Disabled
            });
        }

        let wildcard = match pattern::wildcard_for(query) {
            Ok(w) => w,
            Err(reject) => return VerifyOutcome::Skipped(SkipReason::Pattern(reject)),
        };

        // Step 2: one round trip, matches only.
        let mut sink = VecSink::default();
        // Hidden-system filtering is needed here as well as the extension filter
        // below: this route fires when a share was recently written to, and
        // without it hidden files would appear depending on recent activity.
        let opts = ListOpts::default()
            .with_max_entries(MAX_SERVER_HITS)
            .hiding_system(hidden.hides_system());
        let stats = match self
            .source
            .query(self.dir(), &wildcard, &mut sink, &opts, cancel)
        {
            Ok(stats) => stats,
            // Zero matches is an answer, not a failure.
            Err(EnumError::Empty) => {
                return VerifyOutcome::Server {
                    hits: Vec::new(),
                    matched: 0,
                    capped: false,
                    audit: self.run_audit(&[], snapshot, query),
                };
            }
            Err(EnumError::Unsupported(_)) => {
                self.disable();
                return VerifyOutcome::Skipped(SkipReason::Unsupported);
            }
            Err(e) => return VerifyOutcome::Failed(e),
        };

        // Win32 matches 8.3 short names too, and the pattern may be loose, so
        // narrow the server's names back to the query.
        let confirmed: Vec<String> = sink
            .names
            .into_iter()
            .filter(|n| pattern::confirms(n, query))
            .collect();

        let verdict = self.run_audit(&confirmed, snapshot, query);

        // Audit before filtering hidden files. The audit judges the server's
        // matching; auditing a pre-filtered list would count hidden files as server
        // misses and could disable the mechanism.
        let shown: Vec<String> = confirmed.into_iter().filter(|n| !hidden.hides(n)).collect();
        let matched = shown.len() as u32;
        let hits = rank_server_names(self.dir(), shown, query);

        VerifyOutcome::Server {
            hits,
            matched,
            capped: !stats.complete,
            audit: verdict,
        }
    }

    fn run_audit(
        &self,
        server_names: &[String],
        snapshot: Option<&Snapshot>,
        query: &Query,
    ) -> AuditVerdict {
        let Some(snapshot) = snapshot else {
            return AuditVerdict::NotChecked;
        };
        let verdict = audit(server_names, &local_matches(snapshot, query));
        if let AuditVerdict::ServerUnderReturned { .. } = verdict {
            let misses = self.misses.fetch_add(1, Ordering::Relaxed) + 1;
            if misses >= SERVER_FILTER_MISS_LIMIT {
                // Degrade to purely local matching rather than keep hiding
                // files.
                self.disable();
            }
        }
        verdict
    }
}

/// Ranks server-returned names with the same ordering the local matcher uses.
fn rank_server_names(dir: &Path, names: Vec<String>, query: &Query) -> Vec<Hit> {
    let needle = fold::fold_query(query.term());
    let mut scored: Vec<(u32, u32, u32, String)> = names
        .into_iter()
        .enumerate()
        .filter_map(|(i, name)| {
            // Use the justifying occurrence, not the leftmost, so a server row ranks
            // and underlines where the local matcher would have put it.
            let folded = fold::fold_query(&name);
            let pos = memchr::memmem::find(&folded, &needle)?;
            let at = query::admits(&folded, &needle, pos as u32, query.mode(), query.types())?;
            Some((at, name.len() as u32, i as u32, name))
        })
        .collect();
    scored.sort_unstable_by_key(|&(pos, len, i, _)| (pos, len, i));
    scored.truncate(crate::config::MAX_RESULTS);

    let prefix = dir.to_string_lossy();
    scored
        .into_iter()
        .map(|(pos, _, i, name)| {
            let mut path = String::with_capacity(prefix.len() + name.len() + 1);
            path.push_str(&prefix);
            if !prefix.ends_with(['\\', '/']) {
                path.push('\\');
            }
            path.push_str(&name);
            Hit {
                path: Arc::from(path.as_str()),
                name: Arc::from(name.as_str()),
                match_pos: pos,
                index: i,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;
    use crate::index::fake_source::FakeDirSource;
    use std::time::SystemTime;

    fn snap(names: &[&str], stamp: Option<DirStamp>) -> Snapshot {
        let mut b = SnapshotBuilder::new("V:\\");
        for n in names {
            b.push_str(n);
        }
        b.finish(SystemTime::UNIX_EPOCH, 0, stamp)
    }

    fn verifier(src: FakeDirSource) -> Verifier {
        Verifier::new(Arc::new(src), PathBuf::from("V:\\"), true)
    }

    // --- audit (pure) ------------------------------------------------------

    #[test]
    fn an_exact_match_is_consistent() {
        let server = vec!["a.txt".into(), "b.txt".into()];
        let local = vec!["a.txt".into(), "b.txt".into()];
        assert_eq!(audit(&server, &local), AuditVerdict::Consistent);
    }

    #[test]
    fn the_server_may_return_more_than_the_index_knows_about() {
        // Files created since the snapshot.
        let server = vec!["a.txt".into(), "b.txt".into(), "c.txt".into()];
        let local = vec!["a.txt".into()];
        assert_eq!(audit(&server, &local), AuditVerdict::Consistent);
    }

    #[test]
    fn the_server_may_never_return_less() {
        let server = vec!["a.txt".into()];
        let local = vec!["a.txt".into(), "b.txt".into()];
        assert_eq!(
            audit(&server, &local),
            AuditVerdict::ServerUnderReturned {
                missing: vec!["b.txt".into()]
            }
        );
    }

    #[test]
    fn the_audit_compares_case_insensitively() {
        let server = vec!["A.TXT".into()];
        let local = vec!["a.txt".into()];
        assert_eq!(audit(&server, &local), AuditVerdict::Consistent);
    }

    #[test]
    fn nothing_local_means_nothing_to_disagree_about() {
        assert_eq!(audit(&[], &[]), AuditVerdict::Consistent);
    }

    // --- the cheap path ----------------------------------------------------

    /// Nothing changed, so no query is issued.
    #[test]
    fn an_unchanged_stamp_short_circuits_the_whole_verification() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        src.set_stamp(Some(DirStamp::new(5, 5)));
        let v = verifier(src.clone());

        let s = snap(&["alpha.txt"], Some(DirStamp::new(5, 5)));
        let outcome = v.verify(
            &Query::contains("alpha"),
            Some(&s),
            &Hidden::none(),
            &CancelToken::never(),
        );

        assert!(matches!(outcome, VerifyOutcome::IndexAuthoritative { .. }));
        assert_eq!(
            src.calls()
                .iter()
                .filter(|c| matches!(c, crate::index::fake_source::Call::Query(..)))
                .count(),
            0,
            "an unchanged directory must not cost a server query"
        );
    }

    #[test]
    fn a_changed_stamp_falls_through_to_the_server() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt", "alpha_two.txt"]);
        src.set_stamp(Some(DirStamp::new(9, 9)));
        let v = verifier(src);

        let s = snap(&["alpha.txt"], Some(DirStamp::new(5, 5)));
        match v.verify(
            &Query::contains("alpha"),
            Some(&s),
            &Hidden::none(),
            &CancelToken::never(),
        ) {
            VerifyOutcome::Server { matched, audit, .. } => {
                assert_eq!(matched, 2);
                assert_eq!(audit, AuditVerdict::Consistent);
            }
            other => panic!("expected a server answer, got {other:?}"),
        }
    }

    #[test]
    fn a_snapshot_without_a_stamp_always_queries() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        let v = verifier(src);
        let s = snap(&["alpha.txt"], None);
        assert!(matches!(
            v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never()
            ),
            VerifyOutcome::Server { .. }
        ));
    }

    // --- hidden files ------------------------------------------------------

    /// Hidden-file filtering also applies to server results.
    #[test]
    fn a_hidden_file_the_server_returned_is_not_shown() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.pdf", "alpha.db"]);
        src.set_stamp(Some(DirStamp::new(9, 9)));
        let v = verifier(src);

        let s = snap(&["alpha.pdf"], Some(DirStamp::new(5, 5)));
        match v.verify(
            &Query::contains("alpha"),
            Some(&s),
            &Hidden::new(&["db"], false),
            &CancelToken::never(),
        ) {
            VerifyOutcome::Server { hits, matched, .. } => {
                let names: Vec<&str> = hits.iter().map(|h| h.name.as_ref()).collect();
                assert_eq!(names, vec!["alpha.pdf"]);
                assert_eq!(matched, 1, "the count has to match the list");
            }
            other => panic!("expected a server answer, got {other:?}"),
        }
    }

    /// Hidden-attribute filtering also applies to server results.
    #[test]
    fn a_marked_file_the_server_returned_is_not_shown_either() {
        use crate::index::fake_source::FakeEntry;
        let src = FakeDirSource::new().with_entries(
            "V:\\",
            vec![
                FakeEntry::file("alpha.pdf"),
                // No hidden extension, so only the attribute can catch it.
                FakeEntry::hidden_file("~$alpha.docx"),
            ],
        );
        src.set_stamp(Some(DirStamp::new(9, 9)));
        let v = verifier(src);

        let s = snap(&["alpha.pdf"], Some(DirStamp::new(5, 5)));
        let shown = |hidden: &Hidden| match v.verify(
            &Query::contains("alpha"),
            Some(&s),
            hidden,
            &CancelToken::never(),
        ) {
            VerifyOutcome::Server { hits, .. } => {
                let mut names: Vec<String> = hits.iter().map(|h| h.name.to_string()).collect();
                names.sort();
                names
            }
            other => panic!("expected a server answer, got {other:?}"),
        };

        assert_eq!(
            shown(&Hidden::new::<&str>(&[], false)),
            vec!["alpha.pdf", "~$alpha.docx"],
            "nothing was asked for, so nothing should have been dropped"
        );
        assert_eq!(shown(&Hidden::new::<&str>(&[], true)), vec!["alpha.pdf"]);
    }

    /// The audit runs before the hidden filter, so filtered names are not
    /// counted as server misses.
    #[test]
    fn hiding_a_file_does_not_make_the_server_look_wrong() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.pdf", "alpha.db"]);
        src.set_stamp(Some(DirStamp::new(9, 9)));
        let v = verifier(src);

        let s = snap(&["alpha.pdf", "alpha.db"], Some(DirStamp::new(5, 5)));
        for _ in 0..=SERVER_FILTER_MISS_LIMIT {
            match v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::new(&["db"], false),
                &CancelToken::never(),
            ) {
                VerifyOutcome::Server { audit, .. } => {
                    assert_eq!(audit, AuditVerdict::Consistent, "our filter was blamed");
                }
                other => panic!("expected a server answer, got {other:?}"),
            }
        }
        assert!(
            v.is_enabled(),
            "the filter switched itself off over our own"
        );
    }

    // --- the audit in situ -------------------------------------------------

    /// The audit catches an under-returning server and disables the mechanism.
    #[test]
    fn repeated_under_returns_disable_the_server_filter() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha_one.txt", "alpha_two.txt"]);
        src.set_query_drops(vec![0]); // silently omit the first match
        let v = verifier(src);
        let s = snap(&["alpha_one.txt", "alpha_two.txt"], None);

        for i in 1..=SERVER_FILTER_MISS_LIMIT {
            match v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never(),
            ) {
                VerifyOutcome::Server { audit, .. } => {
                    assert!(matches!(audit, AuditVerdict::ServerUnderReturned { .. }));
                }
                VerifyOutcome::Skipped(SkipReason::AuditFailed { .. }) => {}
                other => panic!("iteration {i}: unexpected {other:?}"),
            }
        }

        assert!(!v.is_enabled(), "the filter must switch itself off");
        assert!(matches!(
            v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never()
            ),
            VerifyOutcome::Skipped(SkipReason::AuditFailed { .. })
        ));
    }

    #[test]
    fn a_consistent_server_never_accrues_misses() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        let v = verifier(src);
        let s = snap(&["alpha.txt"], None);
        for _ in 0..10 {
            v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never(),
            );
        }
        assert_eq!(v.misses(), 0);
        assert!(v.is_enabled());
    }

    #[test]
    fn a_source_that_cannot_filter_disables_itself_immediately() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        src.set_query_unsupported(true);
        let v = verifier(src);
        let s = snap(&["alpha.txt"], None);
        assert!(matches!(
            v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never()
            ),
            VerifyOutcome::Skipped(SkipReason::Unsupported)
        ));
        assert!(!v.is_enabled());
    }

    // --- rejection and failure --------------------------------------------

    #[test]
    fn a_non_literal_query_is_skipped_with_a_reason() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        let v = verifier(src);
        let s = snap(&["alpha.txt"], None);
        match v.verify(
            &Query::contains("al*ha"),
            Some(&s),
            &Hidden::none(),
            &CancelToken::never(),
        ) {
            VerifyOutcome::Skipped(SkipReason::Pattern(PatternReject::NotLiteral('*'))) => {}
            other => panic!("expected a pattern rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_network_failure_is_reported_rather_than_swallowed() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        src.set_error(Some(EnumError::Transient(53)));
        let v = verifier(src);
        let s = snap(&["alpha.txt"], None);
        match v.verify(
            &Query::contains("alpha"),
            Some(&s),
            &Hidden::none(),
            &CancelToken::never(),
        ) {
            VerifyOutcome::Failed(EnumError::Transient(53)) => {}
            other => panic!("expected a transient failure, got {other:?}"),
        }
    }

    #[test]
    fn zero_server_matches_is_an_answer() {
        let src = FakeDirSource::new().with_dir("V:\\", &["beta.txt"]);
        let v = verifier(src);
        let s = snap(&["beta.txt"], None);
        match v.verify(
            &Query::contains("alpha"),
            Some(&s),
            &Hidden::none(),
            &CancelToken::never(),
        ) {
            VerifyOutcome::Server {
                matched: 0, hits, ..
            } => assert!(hits.is_empty()),
            other => panic!("expected an empty server answer, got {other:?}"),
        }
    }

    #[test]
    fn a_disabled_verifier_skips_without_touching_the_network() {
        let src = FakeDirSource::new().with_dir("V:\\", &["alpha.txt"]);
        let v = Verifier::new(Arc::new(src.clone()), PathBuf::from("V:\\"), false);
        let s = snap(&["alpha.txt"], None);
        assert!(matches!(
            v.verify(
                &Query::contains("alpha"),
                Some(&s),
                &Hidden::none(),
                &CancelToken::never()
            ),
            VerifyOutcome::Skipped(SkipReason::Disabled)
        ));
        assert_eq!(
            src.calls()
                .iter()
                .filter(|c| matches!(c, crate::index::fake_source::Call::Query(..)))
                .count(),
            0
        );
    }

    // --- ranking parity ----------------------------------------------------

    #[test]
    fn server_results_are_ranked_like_local_ones() {
        let names = vec![
            "xxalpha.txt".to_string(),
            "alpha.txt".to_string(),
            "alpha_longer.txt".to_string(),
        ];
        let hits = rank_server_names(Path::new("V:\\"), names, &Query::contains("alpha"));
        let ordered: Vec<&str> = hits.iter().map(|h| h.name.as_ref()).collect();
        // Earliest position first, then the shorter name.
        assert_eq!(
            ordered,
            vec!["alpha.txt", "alpha_longer.txt", "xxalpha.txt"]
        );
    }

    #[test]
    fn server_hits_carry_a_usable_full_path() {
        let hits = rank_server_names(
            Path::new("V:\\"),
            vec!["a_alpha.txt".into()],
            &Query::contains("alpha"),
        );
        assert_eq!(&*hits[0].path, "V:\\a_alpha.txt");
        assert_eq!(hits[0].match_pos, 2);
    }

    #[test]
    fn server_hits_are_capped_at_the_display_limit() {
        let n = crate::config::MAX_RESULTS * 2;
        let names: Vec<String> = (0..n).map(|i| format!("alpha{i:05}.txt")).collect();
        let hits = rank_server_names(Path::new("V:\\"), names, &Query::contains("alpha"));
        assert_eq!(hits.len(), crate::config::MAX_RESULTS);
    }

    #[test]
    fn local_matches_finds_what_the_matcher_would() {
        let s = snap(&["Alpha.txt", "beta.txt", "ALPHA_TWO.txt"], None);
        let mut got = local_matches(&s, &Query::contains("alpha"));
        got.sort();
        assert_eq!(got, vec!["ALPHA_TWO.txt", "Alpha.txt"]);
    }
}
