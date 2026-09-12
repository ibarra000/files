//! Proof that config-driven routing sends every job code exactly where the
//! original hardcoded implementation did.
//!
//! Routing was six regexes and a two-variant enum compiled into the binary.
//! It is now a list of mappings, each with its own ordered rules, loaded from
//! a file. That is a change to the one thing in this program that must not
//! change: which share a typed code is looked up in. Getting it wrong does not
//! crash - it quietly searches the wrong place, or probes a share that should
//! never have been touched.
//!
//! [`legacy_classify`] below is a verbatim transcription of the original, kept
//! frozen as an oracle. It is deliberately disposable: once this has held
//! through a few releases it can be deleted along with this file's parity
//! half.
//!
//! Needs no network drives, which is exactly why it carries so much weight.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use files::config::default_routes;
use files::paths::{MappingKind, Routes};
use proptest::prelude::*;
use regex::{Regex, RegexBuilder};

// ---------------------------------------------------------------------------
// The frozen original. Do not edit; it is the thing being compared against.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Legacy {
    FlatRoot,
    JobDir(PathBuf),
}

struct LegacyPatterns {
    re3: Regex,
    re2a: Regex,
    re2b: Regex,
    re1: Regex,
    re_pp: Regex,
    re_p_num: Regex,
}

fn legacy_patterns() -> &'static LegacyPatterns {
    use std::sync::OnceLock;
    static P: OnceLock<LegacyPatterns> = OnceLock::new();
    P.get_or_init(|| {
        let ci = |pat: &str| {
            RegexBuilder::new(pat)
                .case_insensitive(true)
                .build()
                .unwrap()
        };
        LegacyPatterns {
            re3: ci(r"^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)-([A-Z0-9]+)$"),
            re2a: ci(r"^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$"),
            re2b: ci(r"^([A-Z0-9]+)-([A-Z0-9]{2,})-([A-Z0-9]+)$"),
            re1: ci(r"^([A-Z0-9]+)-([A-Z0-9]+)$"),
            re_pp: ci(r"^PP"),
            re_p_num: ci(r"^P[0-9]+"),
        }
    })
}

fn legacy_join_lower(a: &str, b: &str) -> String {
    let mut s = String::with_capacity(a.len() + b.len());
    s.push_str(a);
    s.push_str(b);
    s.make_ascii_lowercase();
    if !s.is_ascii() { s.to_lowercase() } else { s }
}

fn legacy_classify(code: &str) -> Option<Legacy> {
    let p = legacy_patterns();

    if p.re_pp.is_match(code) || p.re_p_num.is_match(code) {
        return Some(Legacy::FlatRoot);
    }
    if let Some(c) = p.re3.captures(code) {
        return Some(Legacy::JobDir(PathBuf::from(legacy_join_lower(
            &c[1], &c[2],
        ))));
    }
    if let Some(c) = p.re2a.captures(code) {
        return Some(Legacy::JobDir(PathBuf::from(legacy_join_lower(
            &c[1], &c[2],
        ))));
    }
    if let Some(c) = p.re2b.captures(code) {
        return Some(Legacy::JobDir(PathBuf::from(c[1].to_lowercase())));
    }
    if let Some(c) = p.re1.captures(code) {
        return Some(Legacy::JobDir(PathBuf::from(c[1].to_lowercase())));
    }
    None
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

const LEGACY_FLAT: &str = r"V:\";
const LEGACY_BASE: &str = r"R:\";

/// The shipped defaults with the *old* CustomPro path, so this compares
/// routing alone and not the `V:\` -> `V:\Documents\custpro` correction.
/// The rules the shipped configuration used to carry, restated here.
///
/// They used to be read from `default_routes()`. The shipped configuration
/// indexes both shares now and carries no patterns at all, so reading them
/// from it would test nothing - but the rule engine still exists and is still
/// reachable by anyone who configures `kind = "job-folder"`, so the guarantee
/// it reproduces the original implementation is still worth holding. Pinning
/// the fixture here is what lets the shipped defaults move on without
/// quietly turning this whole file into a tautology.
const LEGACY_RULES: &str = "version = 1

[[mapping]]
name    = 'custompro'
path    = 'V:\\'
kind    = \"flat\"
stop    = true

  [[mapping.rules]]
  pattern = '^PP'

  [[mapping.rules]]
  pattern = '^P[0-9]+'

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

fn parity_routes() -> Arc<Routes> {
    let parsed = files::config::file::parse(
        LEGACY_RULES,
        Path::new("routing_parity"),
        files::paths::ConfigSource::BuiltIn,
    )
    .expect("the frozen rules parse");
    Arc::new(parsed.routes)
}

#[track_caller]
fn assert_same(routes: &Routes, code: &str) {
    let targets = routes.classify(code);
    match legacy_classify(code) {
        None => assert!(
            targets.is_empty(),
            "{code:?}: the original routed nowhere, the config routed {targets:?}"
        ),
        Some(Legacy::FlatRoot) => {
            assert_eq!(
                targets.len(),
                1,
                "{code:?}: expected one target, got {targets:?}"
            );
            assert_eq!(targets[0].kind, MappingKind::Flat, "{code:?}");
            assert_eq!(targets[0].dir, Path::new(LEGACY_FLAT), "{code:?}");
        }
        Some(Legacy::JobDir(folder)) => {
            assert_eq!(
                targets.len(),
                1,
                "{code:?}: expected one target, got {targets:?}"
            );
            assert_eq!(targets[0].kind, MappingKind::JobFolder, "{code:?}");
            assert_eq!(
                targets[0].dir,
                Path::new(LEGACY_BASE).join(&folder),
                "{code:?}"
            );
        }
    }
}

/// Every literal code from the original `paths.rs` suite.
const CORPUS: &[&str] = &[
    // re3 - four fields, single letter second
    "11-D-0704-A2",
    "AB12-X-99-ZZ",
    // re2a - three fields, single letter second
    "11-D-0704",
    "ab-q-1",
    "11-d-0704",
    "Ab12-X-1",
    // re2b - three fields, multi-character second
    "11-DX-0704",
    "AB12-999-7",
    "11-33-0704",
    // re1 - two fields
    "AB12-0704",
    // the gap between re2a and re2b
    "11-3-0704",
    // CustomPro
    "PP987",
    "pp987",
    "PP987-A",
    "P12345",
    "p12345",
    "P12345-001",
    "P1-D-0704",
    "P99-XY-1",
    "PX-1234",
    // rejected
    "",
    "abc",
    "-",
    "-abc",
    "abc-",
    "a--b",
    "a-b-c-d-e",
    "ab_12",
    "ab 12",
    "ab-12.pdf",
    "11-D-0704-",
    "-11-D-0704",
    // partial input, as it is typed
    "11-D-0",
    "11-D-07",
];

#[test]
fn the_shipped_defaults_route_the_whole_corpus_identically() {
    let routes = parity_routes();
    for code in CORPUS {
        assert_same(&routes, code);
    }
}

/// The precedence fix, restated for the config engine. Two targets here would
/// mean a nonexistent `R:\p12345` is probed on every keystroke.
#[test]
fn a_dashed_custompro_code_still_resolves_to_exactly_one_share() {
    let routes = parity_routes();
    for code in ["P12345-001", "PP987-A", "P1-D-0704", "P99-XY-1"] {
        let targets = routes.classify(code);
        assert_eq!(targets.len(), 1, "{code} routed to {targets:?}");
        assert_eq!(targets[0].kind, MappingKind::Flat, "{code}");
    }
}

/// Field two of the first two job rules is `[A-Z]` - letters only. Widening it
/// would silently reroute this code.
#[test]
fn a_single_digit_middle_field_still_routes_nowhere() {
    let routes = parity_routes();
    assert!(routes.classify("11-3-0704").is_empty());
    assert_eq!(legacy_classify("11-3-0704"), None);
}

#[test]
fn the_shipped_defaults_use_the_corrected_custompro_directory() {
    let routes = default_routes();
    let flat: Vec<&Path> = routes.flat().map(|m| m.path.as_path()).collect();
    assert_eq!(flat, vec![Path::new(r"V:\Documents\custpro")]);
}

/// The shipped defaults index both shares and route neither.
///
/// This asserted one flat and one *job-folder* mapping, which was the shape
/// that made a file in an unpredicted folder unreachable. Both are indexed
/// now: the flat share because it is one directory, the job share because it
/// is a tree - and neither carries a pattern.
#[test]
fn the_shipped_defaults_index_both_shares_and_route_neither() {
    let routes = default_routes();
    assert_eq!(routes.enabled().count(), 2);
    assert_eq!(routes.flat().count(), 1);
    assert_eq!(
        routes
            .enabled()
            .filter(|m| m.kind == MappingKind::Tree)
            .count(),
        1
    );
    assert_eq!(
        routes
            .enabled()
            .filter(|m| m.kind == MappingKind::JobFolder)
            .count(),
        0,
        "nothing is routed by pattern any more"
    );
}

/// Every shipped folder template must reference only groups its own pattern
/// provides. An unsatisfiable reference expands to nothing, and the search
/// would fall back to the entire share.
#[test]
fn every_shipped_template_is_satisfiable() {
    for mapping in default_routes().all() {
        for rule in mapping.rules.iter() {
            assert!(
                rule.unsatisfiable_refs().is_empty(),
                "mapping {:?} rule {} references {:?}",
                mapping.name,
                rule.index(),
                rule.unsatisfiable_refs()
            );
        }
    }
}

/// Codes are built from the alphabet the real patterns use, weighted toward
/// dash-delimited shapes: uniform random strings essentially never reach the
/// three- and four-field rules.
fn code_strategy() -> impl Strategy<Value = String> {
    let field = "[A-Za-z0-9]{1,4}";
    prop_oneof![
        4 => (field, field).prop_map(|(a, b)| format!("{a}-{b}")),
        4 => (field, field, field).prop_map(|(a, b, c)| format!("{a}-{b}-{c}")),
        3 => (field, field, field, field).prop_map(|(a, b, c, d)| format!("{a}-{b}-{c}-{d}")),
        2 => "[A-Za-z0-9-]{0,12}".prop_map(|s| s),
        1 => (field, field).prop_map(|(a, b)| format!("P{a}-{b}")),
        1 => (field, field).prop_map(|(a, b)| format!("PP{a}-{b}")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// The core property: whatever the code, the configured routing agrees
    /// with the original.
    #[test]
    fn routing_agrees_with_the_original_for_any_code(code in code_strategy()) {
        assert_same(&parity_routes(), &code);
    }

    /// A code can never be routed outside the directory its mapping names.
    #[test]
    fn a_target_never_escapes_its_mapping_root(code in "\\PC{0,24}") {
        let routes = default_routes();
        for target in routes.classify(&code) {
            let root = routes.dir(target.mapping).expect("target names a mapping");
            prop_assert!(
                target.dir.starts_with(root),
                "{:?} escaped {:?}",
                target.dir,
                root
            );
        }
    }

    /// Classification is a pure function of the code.
    #[test]
    fn classification_is_stable(code in code_strategy()) {
        let routes = parity_routes();
        prop_assert_eq!(routes.classify(&code), routes.classify(&code));
    }
}
