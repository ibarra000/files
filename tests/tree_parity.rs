//! The tree index must never lose a file the regex router found.
//!
//! This is the one property that decides whether the routing layer can be
//! deleted. Everything else about the rewrite is a performance or ergonomics
//! question; this is correctness, and it is the whole justification for the
//! change - so it is checked against the same corpus of codes
//! `tests/routing_parity.rs` froze from the original implementation.
//!
//! The direction matters. The tree is allowed to find *more* than the router
//! did - that is the entire point, since the router could only ever look in
//! the one folder its pattern predicted. It is not allowed to find less.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use files::index::fake_source::FakeDirSource;
use files::index::tree::SegmentSink;
use files::index::walk::{WalkOpts, walk_tree};
use files::paths::{MappingKind, Routes};
use files::search::matcher;
use files::util::cancel::CancelToken;

const ROOT: &str = "R:\\";

/// The rules the shipped configuration used to carry.
///
/// Stated here rather than read from `default_routes()`, because the shipped
/// configuration no longer routes by pattern at all - that is the change this
/// file exists to justify. Comparing the tree against a table that had
/// already been emptied would be comparing it against nothing.
const LEGACY_RULES: &str = "version = 1

[[mapping]]
name    = 'jobs'
path    = 'R:\\'
kind    = \"job-folder\"
case    = \"lower\"

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}${2}'

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$'
  folder  = '${1}${2}'

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z0-9]{2,})-([A-Z0-9]+)$'
  folder  = '${1}'

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}'
";

fn legacy_routes() -> Routes {
    files::config::file::parse(
        LEGACY_RULES,
        Path::new("tree_parity"),
        files::paths::ConfigSource::BuiltIn,
    )
    .expect("the frozen rules parse")
    .routes
}

/// Every code from the frozen routing corpus that resolves to a job folder,
/// plus the shapes that motivated the rewrite.
const CORPUS: &[&str] = &[
    "11-D-0704-A2",
    "AB12-X-99-ZZ",
    "11-D-0704",
    "ab-q-1",
    "11-d-0704",
    "Ab12-X-1",
    "11-DX-0704",
    "AB12-999-7",
    "11-33-0704",
    "AB12-0704",
    // The documented gap: no rule routes this one anywhere at all.
    "11-3-0704",
];

/// Builds a share containing, for every code, a file inside the folder the
/// router would have looked in - plus files the router could never reach.
fn share(routes: &Routes) -> (FakeDirSource, Vec<(String, String)>) {
    let mut rels: Vec<String> = Vec::new();
    // `(code, relative path)` the router would have found.
    let mut reachable: Vec<(String, String)> = Vec::new();

    for code in CORPUS {
        for target in routes.classify(code) {
            if target.kind != MappingKind::JobFolder {
                continue;
            }
            let folder = target
                .dir
                .strip_prefix(Path::new(ROOT))
                .unwrap_or(&target.dir)
                .to_string_lossy()
                .into_owned();
            let rel = format!("{folder}\\{code} drawing.pdf");
            rels.push(rel.clone());
            reachable.push(((*code).to_string(), rel));
        }
    }

    // Files in folders no pattern predicts. The router cannot see these at
    // all; the tree must.
    for code in CORPUS {
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
    matcher::search_tree(index, code, &CancelToken::never())
        .map(|o| o.hits.iter().map(|h| h.path.to_string()).collect())
        .unwrap_or_default()
}

/// The load-bearing assertion.
#[test]
fn the_tree_finds_everything_the_router_would_have() {
    let routes = legacy_routes();
    let (src, reachable) = share(&routes);
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
    let routes = legacy_routes();
    let (src, _) = share(&routes);
    let index = indexed(&src);

    for code in CORPUS {
        let want = format!("{ROOT}archive\\2019\\odd name\\{code} survey.pdf");
        assert!(
            found(&index, code).contains(&want),
            "the tree did not find {want:?}"
        );
    }
}

/// The code the shipped rules deliberately route nowhere is the clearest case:
/// under the router it was not "no results", it was unreachable.
#[test]
fn a_code_no_rule_routes_is_now_reachable() {
    let routes = legacy_routes();
    assert!(
        routes.classify("11-3-0704").is_empty(),
        "this test is only meaningful while no rule routes this code"
    );

    let (src, _) = share(&routes);
    let index = indexed(&src);
    assert!(
        found(&index, "11-3-0704")
            .iter()
            .any(|p| p.contains("odd name")),
        "the one code the router could never reach is still unreachable"
    );
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
