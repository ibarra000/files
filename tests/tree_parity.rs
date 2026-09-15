//! The tree index must never lose a file the regex router found.
//!
//! This is the one property that decided whether the routing layer could be
//! deleted. Everything else about the rewrite was a performance or ergonomics
//! question; this is correctness, and it was the whole justification for the
//! change.
//!
//! The direction matters. The tree is allowed to find *more* than the router
//! did - that is the entire point, since the router could only ever look in
//! the one folder its pattern predicted. It is not allowed to find less.
//!
//! # Why the answers are frozen rather than computed
//!
//! The router is gone. While it existed this file ran the shipped corpus
//! through it and compared; the table below is what it actually answered,
//! recorded before the engine was removed. Freezing it is the point - a gate
//! that disappears along with the thing it was guarding proves nothing about
//! the code that replaced it, and this is the only remaining evidence that the
//! replacement covers everything it replaced.

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

/// Every code from the frozen routing corpus, and the folder under `R:\` the
/// shipped rules resolved it to.
///
/// Recorded from the rule engine itself, against the patterns
/// `assets/default_config.toml` used to carry:
///
/// ```text
/// ^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)-([A-Z0-9]+)$  ->  ${1}${2}
/// ^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$              ->  ${1}${2}
/// ^([A-Z0-9]+)-([A-Z0-9]{2,})-([A-Z0-9]+)$       ->  ${1}
/// ^([A-Z0-9]+)-([A-Z0-9]+)$                      ->  ${1}
/// ```
///
/// An empty folder means no rule routed it anywhere at all - which under the
/// router was not "no results", it was unreachable.
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
    // The documented gap: no rule routed this one anywhere.
    ("11-3-0704", ""),
];

/// Builds a share containing, for every code, a file inside the folder the
/// router would have looked in - plus files the router could never reach.
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

    // Files in folders no pattern predicts. The router could not see these at
    // all; the tree must.
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

/// The load-bearing assertion.
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

/// And finds what the router never could, which is the reason for the change.
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

/// The code the shipped rules deliberately routed nowhere is the clearest
/// case: under the router it was not "no results", it was unreachable.
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

/// A folder that matches brings its contents, which is how a job code usually
/// behaves - and is what the router did by construction.
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
