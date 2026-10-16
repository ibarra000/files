//! Recursive walking, driven against an in-memory fake tree.
//!
//! Checks that the walk finds everything regardless of thread count, that one
//! unreadable folder does not cost the rest, and that it always terminates.

use std::collections::HashSet;
use std::path::Path;

use files::config::hidden::Hidden;
use files::index::errors::EnumError;
use files::index::fake_source::FakeDirSource;
use files::index::walk::{TreeSink, WalkOpts, WalkReport, walk_tree};
use files::util::cancel::{CancelToken, Epoch};
use parking_lot::Mutex;

/// Records every entry, so a walk is compared against its tree rather than
/// against a count.
#[derive(Debug, Default)]
struct Collect {
    seen: Mutex<Vec<(String, Vec<String>)>>,
}

impl TreeSink for Collect {
    fn push_dir(&self, rel: &str, files: &[String]) -> bool {
        self.seen.lock().push((rel.to_string(), files.to_vec()));
        true
    }
}

fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}\\{name}")
    }
}

impl Collect {
    fn dirs(&self) -> HashSet<String> {
        self.seen.lock().iter().map(|(d, _)| d.clone()).collect()
    }

    /// Every file as a path relative to the root, as an index would store it.
    fn paths(&self) -> HashSet<String> {
        self.seen
            .lock()
            .iter()
            .flat_map(|(d, fs)| fs.iter().map(|f| join_rel(d, f)).collect::<Vec<_>>())
            .collect()
    }
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/// A tree share: project folders at the top, numbered subfolders below, one loose file.
fn tree() -> FakeDirSource {
    FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "abc\\0704\\quote.pdf",
            "abc\\0704\\design.pdf",
            "abc\\0705\\quote.pdf",
            "abc\\notes.txt",
            "ab12\\spec.pdf",
            "loose.pdf",
        ],
    )
}

fn walk(src: &FakeDirSource, opts: &WalkOpts) -> (WalkReport, Collect) {
    let sink = Collect::default();
    let report = walk_tree(src, Path::new("R:\\"), opts, &sink, &CancelToken::never());
    (report, sink)
}

#[test]
fn finds_every_file_in_the_tree() {
    let (report, sink) = walk(&tree(), &WalkOpts::default());

    assert_eq!(report.files, 6);
    assert_eq!(report.dirs_visited, 5, "root, abc, 0704, 0705, ab12");
    assert!(report.complete(), "{report:?}");
    assert_eq!(
        sink.paths(),
        HashSet::from(
            [
                "abc\\0704\\quote.pdf",
                "abc\\0704\\design.pdf",
                "abc\\0705\\quote.pdf",
                "abc\\notes.txt",
                "ab12\\spec.pdf",
                "loose.pdf",
            ]
            .map(String::from)
        )
    );
}

/// A file in a folder no naming rule would predict is still found.
#[test]
fn finds_a_file_whose_folder_no_routing_rule_would_have_guessed() {
    let src =
        FakeDirSource::new().with_tree("R:\\", &["archive\\2019\\odd name\\11-3-0704 survey.pdf"]);
    let (_, sink) = walk(&src, &WalkOpts::default());
    assert!(
        sink.paths()
            .contains("archive\\2019\\odd name\\11-3-0704 survey.pdf"),
        "{:?}",
        sink.paths()
    );
}

#[test]
fn the_root_itself_is_reported_as_the_empty_relative_path() {
    let (_, sink) = walk(&tree(), &WalkOpts::default());
    assert!(sink.dirs().contains(""), "{:?}", sink.dirs());
}

/// The result is the same for any thread count.
#[test]
fn the_result_does_not_depend_on_concurrency() {
    let (serial_report, serial) = walk(&tree(), &WalkOpts::default().with_concurrency(1));
    for n in [2usize, 4, 8, 16] {
        let (report, sink) = walk(&tree(), &WalkOpts::default().with_concurrency(n));
        assert_eq!(sink.paths(), serial.paths(), "at concurrency {n}");
        assert_eq!(report.files, serial_report.files, "at concurrency {n}");
        assert_eq!(
            report.dirs_visited, serial_report.dirs_visited,
            "at concurrency {n}"
        );
    }
}

/// The concurrency limit protects the file server, so it is asserted directly
/// rather than inferred from timing.
#[test]
fn never_reads_more_directories_at_once_than_it_was_allowed() {
    let paths: Vec<String> = (0..64).map(|i| format!("d{i:03}\\f.pdf")).collect();
    for limit in [1usize, 2, 4] {
        let src = FakeDirSource::new().with_tree("R:\\", &strs(&paths));
        let _ = walk(&src, &WalkOpts::default().with_concurrency(limit));
        assert!(
            src.peak_in_flight() <= limit,
            "peaked at {} against a limit of {limit}",
            src.peak_in_flight()
        );
    }
}

/// An unreadable folder is recorded and its siblings are still walked.
#[test]
fn an_unreadable_directory_is_recorded_and_its_siblings_still_walked() {
    let src = tree();
    src.fail_dir("R:\\abc\\0704", EnumError::AccessDenied(5));

    let (report, sink) = walk(&src, &WalkOpts::default().with_concurrency(1));

    assert_eq!(report.errors.denied, 1);
    assert!(
        !report.complete(),
        "a walk with a hole in it is not complete"
    );
    assert!(
        sink.paths().contains("ab12\\spec.pdf"),
        "the rest of the tree still arrived"
    );
    assert!(
        sink.paths().contains("abc\\0705\\quote.pdf"),
        "including the failed folder's own sibling"
    );
    assert_eq!(report.errors.recorded[0].0, "abc\\0704");
}

#[test]
fn a_junction_is_counted_but_not_followed() {
    let src = tree().with_junction("R:\\", "mirror", "R:\\abc");
    let (report, sink) = walk(&src, &WalkOpts::default());

    assert_eq!(report.skipped_reparse, 1);
    assert!(
        !sink.dirs().contains("mirror"),
        "descended into a junction: {:?}",
        sink.dirs()
    );
    assert_eq!(report.files, 6, "and so found nothing twice");
}

/// Following junctions is opt-in, and the depth limit stops cycles, since many
/// SMB servers do not provide the file IDs a proper cycle guard needs.
#[test]
fn a_followed_junction_cannot_outrun_the_depth_limit() {
    let src = FakeDirSource::new()
        .with_tree("R:\\", &["a\\f.pdf"])
        .with_junction("R:\\a", "loop", "R:\\");

    let mut opts = WalkOpts::default().with_max_depth(4);
    opts.follow_reparse = true;
    let (report, _) = walk(&src, &opts);

    assert!(report.max_depth_seen <= 4, "{report:?}");
    assert!(report.depth_clipped > 0, "the cycle was cut, not walked");
}

#[test]
fn the_depth_limit_clips_rather_than_failing() {
    let src = FakeDirSource::new().with_tree("R:\\", &["a\\b\\c\\d\\deep.pdf"]);
    let (report, sink) = walk(&src, &WalkOpts::default().with_max_depth(2));

    assert!(report.truncated, "a clipped walk is a partial view");
    assert!(!report.complete());
    assert!(report.depth_clipped > 0);
    assert!(!sink.paths().contains("a\\b\\c\\d\\deep.pdf"));
    assert!(sink.dirs().contains("a\\b"), "{:?}", sink.dirs());
}

#[test]
fn a_directory_cap_truncates_rather_than_running_away() {
    let paths: Vec<String> = (0..200).map(|i| format!("d{i:03}\\f.pdf")).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &strs(&paths));

    let mut opts = WalkOpts::default().with_concurrency(1);
    opts.max_dirs = 10;
    let (report, _) = walk(&src, &opts);

    assert!(report.truncated);
    assert!(!report.complete());
    assert!(
        report.dirs_visited <= 20,
        "stopped near the cap, having visited {}",
        report.dirs_visited
    );
}

#[test]
fn an_empty_share_walks_cleanly_rather_than_looking_broken() {
    let src = FakeDirSource::new().with_tree("R:\\", &[]);
    let (report, sink) = walk(&src, &WalkOpts::default());

    assert_eq!(report.files, 0);
    assert_eq!(report.dirs_visited, 1, "the root itself");
    assert!(report.complete());
    assert_eq!(sink.dirs(), HashSet::from([String::new()]));
}

#[test]
fn a_missing_root_is_reported_rather_than_hung_on() {
    let src = FakeDirSource::new().with_tree("R:\\", &["a.pdf"]);
    let sink = Collect::default();
    let report = walk_tree(
        &src,
        Path::new("Z:\\nope"),
        &WalkOpts::default(),
        &sink,
        &CancelToken::never(),
    );
    assert_eq!(report.errors.vanished, 1);
    assert_eq!(report.dirs_visited, 0);
    assert!(
        report.aborted.is_some(),
        "a missing root is the share being absent, not a folder having moved"
    );
    assert!(!report.complete());
}

/// A folder deleted mid-walk is ordinary churn, not a coverage hole; counting
/// it would mark a healthy share as degraded.
#[test]
fn a_folder_that_vanishes_mid_walk_is_not_a_coverage_hole() {
    let src = tree();
    src.fail_dir("R:\\abc\\0704", EnumError::PathNotFound(3));

    let (report, sink) = walk(&src, &WalkOpts::default().with_concurrency(1));

    assert_eq!(report.errors.vanished, 1);
    assert_eq!(report.errors.holes(), 0, "nothing is unreachable");
    assert!(
        report.complete(),
        "the walk read everything that was still there: {report:?}"
    );
    assert!(sink.paths().contains("ab12\\spec.pdf"));
}

#[test]
fn an_unreadable_folder_is_a_coverage_hole() {
    let src = tree();
    src.fail_dir("R:\\abc\\0704", EnumError::AccessDenied(5));

    let (report, _) = walk(&src, &WalkOpts::default().with_concurrency(1));

    assert_eq!(report.errors.holes(), 1);
    assert_eq!(report.errors.vanished, 0);
    assert!(!report.complete());
}

#[test]
fn cancelling_stops_the_walk() {
    let paths: Vec<String> = (0..500).map(|i| format!("d{i:03}\\f.pdf")).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &strs(&paths));

    let epoch = Epoch::new();
    let token = epoch.token(epoch.bump());
    epoch.bump(); // superseded before it starts

    let sink = Collect::default();
    let report = walk_tree(&src, Path::new("R:\\"), &WalkOpts::default(), &sink, &token);
    assert!(report.cancelled);
    assert!(!report.complete());
}

/// A sink reporting itself full stops the walk, so the walk needs no knowledge
/// of index capacity.
#[test]
fn a_full_sink_stops_the_walk() {
    #[derive(Default)]
    struct Full(Mutex<usize>);
    impl TreeSink for Full {
        fn push_dir(&self, _rel: &str, _files: &[String]) -> bool {
            let mut n = self.0.lock();
            *n += 1;
            *n < 3
        }
    }

    let paths: Vec<String> = (0..100).map(|i| format!("d{i:03}\\f.pdf")).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &strs(&paths));

    let sink = Full::default();
    let report = walk_tree(
        &src,
        Path::new("R:\\"),
        &WalkOpts::default().with_concurrency(1),
        &sink,
        &CancelToken::never(),
    );
    assert!(report.truncated);
    assert!(report.dirs_visited < 100);
}

/// A wide, deep tree terminates with nothing lost or double-counted.
#[test]
fn a_large_tree_is_walked_exactly_once_through() {
    let mut paths = Vec::new();
    for a in 0..12 {
        for b in 0..12 {
            paths.push(format!("a{a:02}\\b{b:02}\\f1.pdf"));
            paths.push(format!("a{a:02}\\b{b:02}\\f2.pdf"));
        }
    }
    let src = FakeDirSource::new().with_tree("R:\\", &strs(&paths));
    let (report, sink) = walk(&src, &WalkOpts::default());

    assert_eq!(report.files, 12 * 12 * 2);
    assert_eq!(report.dirs_visited, 1 + 12 + 12 * 12);
    assert_eq!(sink.paths().len(), paths.len(), "no file counted twice");
    assert_eq!(sink.dirs().len(), report.dirs_visited, "nor any directory");
    assert!(report.complete());
}

// --- from a walk to a searchable index --------------------------------

use files::index::tree::SegmentSink;
use files::search::matcher;

fn indexed(src: &FakeDirSource, opts: &WalkOpts) -> (files::index::tree::TreeIndex, WalkReport) {
    let sink = SegmentSink::new("R:\\");
    let report = walk_tree(src, Path::new("R:\\"), opts, &sink, &CancelToken::never());
    (sink.index(), report)
}

fn found(index: &files::index::tree::TreeIndex, query: &str) -> Vec<String> {
    matcher::search_tree(
        index,
        &files::search::query::Query::contains(query),
        &Hidden::none(),
        &CancelToken::never(),
    )
    .unwrap()
    .hits
    .iter()
    .map(|h| h.path.to_string())
    .collect()
}

/// A file in an unpredictable folder is walked, indexed and found by its name.
#[test]
fn a_walked_tree_can_be_searched_for_a_code_no_rule_would_have_routed() {
    let src = FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "archive\\2019\\odd name\\11-3-0704 survey.pdf",
            "abc\\0704\\quote.pdf",
            "ab12\\unrelated.pdf",
        ],
    );
    let (index, report) = indexed(&src, &WalkOpts::default());

    assert!(report.complete());
    assert_eq!(index.len(), 3);
    assert_eq!(
        found(&index, "11-3-0704"),
        vec!["R:\\archive\\2019\\odd name\\11-3-0704 survey.pdf"]
    );
}

#[test]
fn a_code_that_names_a_folder_finds_what_is_in_it() {
    let src = FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "abc\\0704\\quote.pdf",
            "abc\\0704\\design.pdf",
            "abc\\0705\\other.pdf",
        ],
    );
    let (index, _) = indexed(&src, &WalkOpts::default());

    let mut got = found(&index, "0704");
    got.sort();
    assert_eq!(
        got,
        vec!["R:\\abc\\0704\\design.pdf", "R:\\abc\\0704\\quote.pdf"]
    );
}

/// The index is searchable while the walk is still running.
#[test]
fn a_partially_walked_tree_is_already_searchable() {
    let paths: Vec<String> = (0..400)
        .map(|i| format!("d{i:03}\\0704 file.pdf"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &refs);

    let sink = SegmentSink::new("R:\\");
    // Stopped early, as a mid-walk read would see it.
    let mut opts = WalkOpts::default().with_concurrency(1);
    opts.max_dirs = 20;
    let report = walk_tree(&src, Path::new("R:\\"), &opts, &sink, &CancelToken::never());

    assert!(report.truncated, "the walk was stopped short");
    let index = sink.index();
    assert!(!index.is_empty(), "yet something is already searchable");
    assert!(!found(&index, "0704").is_empty());
}

/// Results do not depend on where segment boundaries fall.
#[test]
fn segmenting_does_not_change_what_is_found() {
    let paths: Vec<String> = (0..300)
        .map(|i| format!("d{i:03}\\0704 file{i:03}.pdf"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &refs);

    let (index, report) = indexed(&src, &WalkOpts::default().with_concurrency(1));
    assert!(report.complete());
    assert_eq!(index.len(), 300);

    let hits = matcher::search_tree(
        &index,
        &files::search::query::Query::contains("file123"),
        &Hidden::none(),
        &CancelToken::never(),
    )
    .unwrap();
    assert_eq!(
        hits.hits
            .iter()
            .map(|h| h.path.to_string())
            .collect::<Vec<_>>(),
        vec!["R:\\d123\\0704 file123.pdf"]
    );
}

/// Every file the walk reported is in the index, and no more.
#[test]
fn the_index_holds_exactly_what_the_walk_found() {
    let src = FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "a\\one.pdf",
            "a\\two.pdf",
            "a\\b\\three.pdf",
            "c\\four.pdf",
            "loose.pdf",
        ],
    );
    let (index, report) = indexed(&src, &WalkOpts::default());

    assert_eq!(index.len(), report.files);
    let mut all: Vec<String> = (0..index.len() as u32)
        .map(|i| index.full_path(i).unwrap())
        .collect();
    all.sort();
    assert_eq!(
        all,
        vec![
            "R:\\a\\b\\three.pdf",
            "R:\\a\\one.pdf",
            "R:\\a\\two.pdf",
            "R:\\c\\four.pdf",
            "R:\\loose.pdf",
        ]
    );
}

/// Every file in a tree spanning several segments resolves to its path.
#[test]
fn a_tree_spanning_many_segments_resolves_every_path() {
    let mut paths = Vec::new();
    for a in 0..40 {
        for b in 0..40 {
            paths.push(format!("a{a:02}\\b{b:02}\\report.pdf"));
        }
    }
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &refs);

    let (index, report) = indexed(&src, &WalkOpts::default());
    assert!(report.complete());
    assert_eq!(index.len(), 1_600);

    // Every path is distinct and well formed across segment boundaries.
    let all: std::collections::HashSet<String> = (0..index.len() as u32)
        .map(|i| index.full_path(i).unwrap())
        .collect();
    assert_eq!(all.len(), 1_600, "a path was duplicated or lost");
    assert!(all.contains("R:\\a17\\b23\\report.pdf"));
}

// --- seeded walks ----------------------------------------------------------

/// A walk seeded with dirty folders does work proportional to the change, not
/// to the share.
mod subtrees {
    use super::*;
    use files::index::walk::walk_subtrees;

    fn share() -> FakeDirSource {
        FakeDirSource::new().with_tree(
            "R:\\",
            &[
                "top.txt",
                "abc\\quote.pdf",
                "abc\\0704\\design.dwg",
                "abc\\0704\\deep\\note.txt",
                "ab12\\spec.pdf",
                "ab12x\\decoy.pdf",
            ],
        )
    }

    fn seeded(seeds: &[&str]) -> (Collect, WalkReport) {
        let sink = Collect::default();
        let owned: Vec<String> = seeds.iter().map(|s| (*s).to_string()).collect();
        let report = walk_subtrees(
            &share(),
            Path::new("R:\\"),
            &owned,
            &WalkOpts::default(),
            &sink,
            &CancelToken::never(),
        );
        (sink, report)
    }

    fn dirs(sink: &Collect) -> Vec<String> {
        let mut out: Vec<String> = sink.seen.lock().iter().map(|(d, _)| d.clone()).collect();
        out.sort();
        out
    }

    #[test]
    fn a_seed_brings_its_whole_subtree_and_nothing_else() {
        let (sink, report) = seeded(&["abc"]);
        assert!(report.complete(), "{report:?}");
        assert_eq!(
            dirs(&sink),
            vec![
                "abc".to_string(),
                "abc\\0704".to_string(),
                "abc\\0704\\deep".to_string(),
            ]
        );
    }

    /// A prefix is not a parent: refreshing `ab12` must not read `ab12x`, or the
    /// caller would replace a subtree it never walked.
    #[test]
    fn a_seed_does_not_pick_up_a_sibling_that_merely_starts_the_same() {
        let (sink, _) = seeded(&["ab12"]);
        assert_eq!(dirs(&sink), vec!["ab12".to_string()]);
    }

    /// A directory covered by two seeds is read once; otherwise a folder match
    /// would return its contents twice.
    #[test]
    fn a_seed_already_covered_by_another_is_not_walked_twice() {
        let (sink, _) = seeded(&["abc", "abc\\0704", "abc"]);
        assert_eq!(
            dirs(&sink),
            vec![
                "abc".to_string(),
                "abc\\0704".to_string(),
                "abc\\0704\\deep".to_string(),
            ]
        );
    }

    #[test]
    fn several_disjoint_seeds_are_all_walked() {
        let (sink, _) = seeded(&["ab12", "ab12x"]);
        assert_eq!(dirs(&sink), vec!["ab12".to_string(), "ab12x".to_string()]);
    }

    /// A dirty root becomes a full walk.
    #[test]
    fn the_root_as_a_seed_walks_everything() {
        let (sink, _) = seeded(&["", "ab12"]);
        assert_eq!(dirs(&sink).len(), 6);
    }

    /// A deleted seed is reported as vanished, not unreadable: the caller
    /// removes the first from the index and leaves the second alone.
    #[test]
    fn a_seed_that_has_been_deleted_is_counted_as_vanished_not_as_a_hole() {
        let sink = Collect::default();
        let report = walk_subtrees(
            &share(),
            Path::new("R:\\"),
            &["gone".to_string()],
            &WalkOpts::default(),
            &sink,
            &CancelToken::never(),
        );
        assert_eq!(report.errors.vanished, 1);
        assert_eq!(report.errors.holes(), 0);
        assert!(report.complete());
        assert!(
            report.aborted.is_none(),
            "a missing seed is not a missing share"
        );
    }

    #[test]
    fn an_unreadable_seed_is_a_hole() {
        let sink = Collect::default();
        let src = share();
        src.fail_dir("R:\\ab12", EnumError::AccessDenied(5));
        let report = walk_subtrees(
            &src,
            Path::new("R:\\"),
            &["ab12".to_string()],
            &WalkOpts::default(),
            &sink,
            &CancelToken::never(),
        );
        assert_eq!(report.errors.holes(), 1);
        assert!(!report.complete());
    }

    #[test]
    fn no_seeds_walks_nothing_and_succeeds() {
        let (sink, report) = seeded(&[]);
        assert!(dirs(&sink).is_empty());
        assert!(report.complete());
        assert_eq!(report.dirs_visited, 0);
    }
}
