//! Recursive walking, driven against a fake tree.
//!
//! The share this is for holds millions of files and does not exist on the
//! machine the code is written on, so every property that matters here has to
//! be provable in memory: that the walk finds everything, that it finds it
//! regardless of how many threads read it, that one unreadable folder does not
//! cost the rest, and that nothing it does can fail to terminate.
//!
//! The first test is the whole reason the walk exists. Routing by regex meant
//! a file whose folder did not follow the naming rule was not "no results" but
//! silently absent, and no amount of testing the router could have found it.

use std::collections::HashSet;
use std::path::Path;

use files::config::hidden::Hidden;
use files::index::errors::EnumError;
use files::index::fake_source::FakeDirSource;
use files::index::walk::{TreeSink, WalkOpts, WalkReport, walk_tree};
use files::util::cancel::{CancelToken, Epoch};
use parking_lot::Mutex;

/// Records everything, so a walk is compared against the tree it was handed
/// rather than against a count of it.
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

    /// Every file as a path relative to the root, which is what an index built
    /// on this walk would store.
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

/// A job share: codes at the top, per-job subfolders below, one loose file.
fn tree() -> FakeDirSource {
    FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "11d\\0704\\quote.pdf",
            "11d\\0704\\drawing.pdf",
            "11d\\0705\\quote.pdf",
            "11d\\notes.txt",
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
    assert_eq!(report.dirs_visited, 5, "root, 11d, 0704, 0705, ab12");
    assert!(report.complete(), "{report:?}");
    assert_eq!(
        sink.paths(),
        HashSet::from(
            [
                "11d\\0704\\quote.pdf",
                "11d\\0704\\drawing.pdf",
                "11d\\0705\\quote.pdf",
                "11d\\notes.txt",
                "ab12\\spec.pdf",
                "loose.pdf",
            ]
            .map(String::from)
        )
    );
}

/// The bug the walk exists to remove. No routing rule would guess this folder,
/// so the file used to be unreachable rather than merely unranked.
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

/// Same tree, however many threads read it. Without this the walk could be
/// correct only on the machine it was timed on.
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

/// The concurrency limit is a promise to someone else's production file
/// server, so it is asserted directly rather than inferred from timing.
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

/// An unreadable folder in the middle of a share must not cost the other
/// 299,999.
#[test]
fn an_unreadable_directory_is_recorded_and_its_siblings_still_walked() {
    let src = tree();
    src.fail_dir("R:\\11d\\0704", EnumError::AccessDenied(5));

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
        sink.paths().contains("11d\\0705\\quote.pdf"),
        "including the failed folder's own sibling"
    );
    assert_eq!(report.errors.recorded[0].0, "11d\\0704");
}

#[test]
fn a_junction_is_counted_but_not_followed() {
    let src = tree().with_junction("R:\\", "mirror", "R:\\11d");
    let (report, sink) = walk(&src, &WalkOpts::default());

    assert_eq!(report.skipped_reparse, 1);
    assert!(
        !sink.dirs().contains("mirror"),
        "descended into a junction: {:?}",
        sink.dirs()
    );
    assert_eq!(report.files, 6, "and so found nothing twice");
}

/// Following one is opt-in, and the depth limit is what stops the cycle - the
/// file-id guard that would do it properly is unavailable on many SMB servers.
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

/// A folder deleted while the walk is running is ordinary churn on a share
/// people are working on. Counting it as a coverage hole would leave a
/// perfectly healthy share permanently degraded, and the warnings that do
/// matter would be ignored along with it.
#[test]
fn a_folder_that_vanishes_mid_walk_is_not_a_coverage_hole() {
    let src = tree();
    src.fail_dir("R:\\11d\\0704", EnumError::PathNotFound(3));

    let (report, sink) = walk(&src, &WalkOpts::default().with_concurrency(1));

    assert_eq!(report.errors.vanished, 1);
    assert_eq!(report.errors.holes(), 0, "nothing is unreachable");
    assert!(
        report.complete(),
        "the walk read everything that was still there: {report:?}"
    );
    assert!(sink.paths().contains("ab12\\spec.pdf"));
}

/// Where an unreadable one is exactly that, and must be reported.
#[test]
fn an_unreadable_folder_is_a_coverage_hole() {
    let src = tree();
    src.fail_dir("R:\\11d\\0704", EnumError::AccessDenied(5));

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

/// A sink reporting itself full stops the walk. That is how an index that has
/// run out of room says so without the walk needing to know what an index is.
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

/// A wide, deep tree, to show the frontier terminates and nothing is lost or
/// double-counted at scale.
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
    matcher::search_tree(index, query, &Hidden::none(), &CancelToken::never())
        .unwrap()
        .hits
        .iter()
        .map(|h| h.path.to_string())
        .collect()
}

/// The whole project, in one assertion: a file whose folder no routing rule
/// would have guessed is walked, indexed, and found by its code.
#[test]
fn a_walked_tree_can_be_searched_for_a_code_no_rule_would_have_routed() {
    let src = FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "archive\\2019\\odd name\\11-3-0704 survey.pdf",
            "11d\\0704\\quote.pdf",
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

/// And the folder case, which is how a job code usually appears.
#[test]
fn a_code_that_names_a_folder_finds_what_is_in_it() {
    let src = FakeDirSource::new().with_tree(
        "R:\\",
        &[
            "11d\\0704\\quote.pdf",
            "11d\\0704\\drawing.pdf",
            "11d\\0705\\other.pdf",
        ],
    );
    let (index, _) = indexed(&src, &WalkOpts::default());

    let mut got = found(&index, "0704");
    got.sort();
    assert_eq!(
        got,
        vec!["R:\\11d\\0704\\drawing.pdf", "R:\\11d\\0704\\quote.pdf"]
    );
}

/// The index is searchable while the walk is still running, which is what
/// makes a three-minute walk of a real share usable rather than a blank
/// screen.
#[test]
fn a_partially_walked_tree_is_already_searchable() {
    let paths: Vec<String> = (0..400)
        .map(|i| format!("d{i:03}\\0704 file.pdf"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree("R:\\", &refs);

    let sink = SegmentSink::new("R:\\");
    // Stopped early on purpose, exactly as reading the index mid-walk would
    // see it.
    let mut opts = WalkOpts::default().with_concurrency(1);
    opts.max_dirs = 20;
    let report = walk_tree(&src, Path::new("R:\\"), &opts, &sink, &CancelToken::never());

    assert!(report.truncated, "the walk was stopped short");
    let index = sink.index();
    assert!(!index.is_empty(), "yet something is already searchable");
    assert!(!found(&index, "0704").is_empty());
}

/// Segments are an implementation detail; the answers must not depend on
/// where the boundaries fell.
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

    let hits =
        matcher::search_tree(&index, "file123", &Hidden::none(), &CancelToken::never()).unwrap();
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

/// A large tree crosses several segment boundaries, and every one of its
/// files must still resolve to the right path.
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

    // Every path is distinct and well formed, which is what the run table has
    // to get right across a boundary.
    let all: std::collections::HashSet<String> = (0..index.len() as u32)
        .map(|i| index.full_path(i).unwrap())
        .collect();
    assert_eq!(all.len(), 1_600, "a path was duplicated or lost");
    assert!(all.contains("R:\\a17\\b23\\report.pdf"));
}

// --- seeded walks ----------------------------------------------------------

/// A walk seeded with the dirty folders is what turns a change notification
/// into work proportional to what changed rather than to the share.
mod subtrees {
    use super::*;
    use files::index::walk::walk_subtrees;

    fn share() -> FakeDirSource {
        FakeDirSource::new().with_tree(
            "R:\\",
            &[
                "top.txt",
                "11d\\quote.pdf",
                "11d\\0704\\drawing.dwg",
                "11d\\0704\\deep\\note.txt",
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
        let (sink, report) = seeded(&["11d"]);
        assert!(report.complete(), "{report:?}");
        assert_eq!(
            dirs(&sink),
            vec![
                "11d".to_string(),
                "11d\\0704".to_string(),
                "11d\\0704\\deep".to_string(),
            ]
        );
    }

    /// A prefix is not a parent. Without the separator check, refreshing
    /// `ab12` would read `ab12x` as well and then the caller would replace a
    /// subtree it never walked.
    #[test]
    fn a_seed_does_not_pick_up_a_sibling_that_merely_starts_the_same() {
        let (sink, _) = seeded(&["ab12"]);
        assert_eq!(dirs(&sink), vec!["ab12".to_string()]);
    }

    /// Reading the same directory twice would hand the sink two directories
    /// with one name, and a folder match would return its contents doubled.
    #[test]
    fn a_seed_already_covered_by_another_is_not_walked_twice() {
        let (sink, _) = seeded(&["11d", "11d\\0704", "11d"]);
        assert_eq!(
            dirs(&sink),
            vec![
                "11d".to_string(),
                "11d\\0704".to_string(),
                "11d\\0704\\deep".to_string(),
            ]
        );
    }

    #[test]
    fn several_disjoint_seeds_are_all_walked() {
        let (sink, _) = seeded(&["ab12", "ab12x"]);
        assert_eq!(dirs(&sink), vec!["ab12".to_string(), "ab12x".to_string()]);
    }

    /// A dirty root degenerates to a full walk, which is the honest answer.
    #[test]
    fn the_root_as_a_seed_walks_everything() {
        let (sink, _) = seeded(&["", "ab12"]);
        assert_eq!(dirs(&sink).len(), 6);
    }

    /// A folder that is gone is ordinary churn, and the caller needs to be
    /// able to tell it from a folder it simply could not read - one means
    /// "delete it from the index", the other means "leave it alone".
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
