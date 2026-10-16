//! The tree index must find every file the regex router found.
//!
//! The tree may find more, since the router only looked in the one folder its
//! pattern predicted, but never less. The router's answers are frozen in
//! `FROZEN`, recorded from the rule engine, so the check does not depend on it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use files::config::hidden::Hidden;
use files::index::fake_source::FakeDirSource;
use files::index::tree::SegmentSink;
use files::index::walk::{WalkOpts, walk_tree};
use files::search::matcher;
use files::util::cancel::CancelToken;

const ROOT: &str = "R:\\";

/// Every code in the routing corpus and the folder under `R:\` the rules
/// resolved it to, recorded against these patterns:
///
/// ```text
/// ^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)-([A-Z0-9]+)$  ->  ${1}${2}
/// ^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$              ->  ${1}${2}
/// ^([A-Z0-9]+)-([A-Z0-9]{2,})-([A-Z0-9]+)$       ->  ${1}
/// ^([A-Z0-9]+)-([A-Z0-9]+)$                      ->  ${1}
/// ```
///
/// An empty folder means no rule matched, so the router could not reach it.
const FROZEN: &[(&str, &str)] = &[
    ("11-D-0704-A2", "11d"),
    ("AB12-X-99-ZZ", "ab12x"),
    ("11-D-0704", "11d"),
    ("ab-q-1", "abq"),
    ("11-d-0704", "11d"),
    ("Ab12-X-1", "ab12x"),
    ("11-DX-0704", "11"),
    ("AB12-999-7", "ab12"),
    ("11-33-0704", "11"),
    ("AB12-0704", "ab12"),
    // No rule routes this code.
    ("11-3-0704", ""),
];

/// Builds a share with, for every code, a file in its routed folder plus a file
/// in a folder no rule predicts.
fn share() -> (FakeDirSource, Vec<(String, String)>) {
    let mut rels: Vec<String> = Vec::new();
    // `(code, relative path)` the router would have found.
    let mut reachable: Vec<(String, String)> = Vec::new();

    for (code, folder) in FROZEN {
        if folder.is_empty() {
            continue;
        }
        let rel = format!("{folder}\\{code} drawing.pdf");
        rels.push(rel.clone());
        reachable.push(((*code).to_string(), rel));
    }

    // Folders no pattern predicts; only the tree can find these.
    for (code, _) in FROZEN {
        rels.push(format!("archive\\2019\\odd name\\{code} survey.pdf"));
    }

    let refs: Vec<&str> = rels.iter().map(|s| s.as_str()).collect();
    (FakeDirSource::new().with_tree(ROOT, &refs), reachable)
}

/// Walks the share and returns a searchable index of it.
fn indexed(src: &FakeDirSource) -> Arc<files::index::tree::TreeIndex> {
    let sink = SegmentSink::new(ROOT);
    let report = walk_tree(
        src,
        Path::new(ROOT),
        &WalkOpts::default(),
        &sink,
        &CancelToken::never(),
    );
    assert!(report.complete(), "the fixture walk failed: {report:?}");
    Arc::new(sink.index())
}

fn found(index: &files::index::tree::TreeIndex, code: &str) -> HashSet<String> {
    matcher::search_tree(
        index,
        &files::search::query::Query::contains(code),
        &Hidden::none(),
        &CancelToken::never(),
    )
    .map(|o| o.hits.iter().map(|h| h.path.to_string()).collect())
    .unwrap_or_default()
}

#[test]
fn the_tree_finds_everything_the_router_would_have() {
    let (src, reachable) = share();
    let index = indexed(&src);

    assert!(
        !reachable.is_empty(),
        "the fixture proves nothing if the router reached nothing"
    );

    for (code, rel) in &reachable {
        let want = format!("{ROOT}{rel}");
        let got = found(&index, code);
        assert!(
            got.contains(&want),
            "the tree lost {want:?}, which routing {code:?} would have found.\n\
             It returned: {got:?}"
        );
    }
}

#[test]
fn the_tree_also_finds_what_no_rule_could_reach() {
    let (src, _) = share();
    let index = indexed(&src);

    for (code, _) in FROZEN {
        let want = format!("{ROOT}archive\\2019\\odd name\\{code} survey.pdf");
        assert!(
            found(&index, code).contains(&want),
            "the tree did not find {want:?}"
        );
    }
}

/// A code no rule routes is still found by the tree.
#[test]
fn a_code_no_rule_routed_is_now_reachable() {
    let unroutable: Vec<&str> = FROZEN
        .iter()
        .filter(|(_, folder)| folder.is_empty())
        .map(|(code, _)| *code)
        .collect();
    assert!(
        !unroutable.is_empty(),
        "this test is only meaningful while the corpus holds one"
    );

    let (src, _) = share();
    let index = indexed(&src);
    for code in unroutable {
        assert!(
            found(&index, code).iter().any(|p| p.contains("odd name")),
            "the one code the router could never reach is still unreachable"
        );
    }
}

/// A folder whose name matches returns the files inside it.
#[test]
fn a_code_naming_a_folder_still_returns_that_folders_files() {
    let src = FakeDirSource::new().with_tree(
        ROOT,
        &[
            "11d\\quote.pdf",
            "11d\\drawing.pdf",
            "11d\\notes.txt",
            "other\\unrelated.pdf",
        ],
    );
    let index = indexed(&src);

    let got = found(&index, "11d");
    assert_eq!(got.len(), 3, "{got:?}");
    for name in ["quote.pdf", "drawing.pdf", "notes.txt"] {
        assert!(
            got.iter().any(|p| p.ends_with(name)),
            "{name} missing from {got:?}"
        );
    }
}
