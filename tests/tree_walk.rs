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
