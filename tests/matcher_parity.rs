//! Proof that the new matcher ranks exactly like the old one.
//!
//! This is the highest-value test in the project. The rewrite replaced a
//! per-entry `str::find` plus a full sort with a SIMD sweep over a contiguous
//! arena, a galloping byte-to-entry mapping, parallel chunking and a bounded
//! top-K merge. Every one of those steps is an opportunity for an
//! off-by-one - and an off-by-one here does not crash, it quietly displays
//! *plausible-looking wrong filenames shifted by one row*, which a user may
//! never recognise as a bug.
//!
//! [`reference`] below is a direct transcription of the original
//! `rank_matches`, kept as an oracle. The new implementation is asserted to
//! produce byte-identical output: same order, same match count, same total.
//!
//! None of this needs the network drives, which is precisely why it carries
//! so much of the verification burden.

use std::path::Path;

use files::config::MatcherKind;
use files::config::hidden::Hidden;
use files::index::builder::SnapshotBuilder;
use files::index::snapshot::Snapshot;
use files::search::matcher::{self, SearchOutcome};
use files::search::query::Query;
use files::util::cancel::CancelToken;
use proptest::prelude::*;

/// The oracle's own copy of the display cap.
///
/// Deliberately a separate constant, because the oracle is a frozen
/// transcription and should not quietly follow the implementation it is
/// checking. The assertion below is what keeps that honest: if the real cap
/// moves and this does not, the oracle truncates at a different point from
/// the matcher and every parity assertion here silently becomes a tautology.
/// Failing to compile is the only safe way to notice.
const MAX_RESULTS: usize = 300;
const _: () = assert!(
    MAX_RESULTS == files::config::MAX_RESULTS,
    "the parity oracle's cap has drifted from config::MAX_RESULTS"
);

const PREFIX: &str = "V:\\";

/// The original implementation, transcribed verbatim.
///
/// Entries are `(lowercased name, full path)`, exactly as the previous
/// `get_dir_listing` produced them.
fn reference(names: &[String], query: &str) -> (Vec<String>, usize, usize) {
    let entries: Vec<(String, String)> = names
        .iter()
        .map(|n| {
            (
                n.to_lowercase(),
                Path::new(PREFIX).join(n).to_string_lossy().into_owned(),
            )
        })
        .collect();

    let query_lower = query.to_lowercase();

    let mut scored: Vec<(usize, &str, &str)> = entries
        .iter()
        .filter_map(|(lower_name, full_path)| {
            lower_name
                .find(&query_lower)
                .map(|pos| (pos, full_path.as_str(), lower_name.as_str()))
        })
        .collect();

    let matched_count = scored.len();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.len().cmp(&b.2.len())));

    let matches = scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, full_path, _)| full_path.to_string())
        .collect();

    (matches, entries.len(), matched_count)
}

fn snapshot(names: &[String]) -> Snapshot {
    let mut b = SnapshotBuilder::new(PREFIX);
    for n in names {
        b.push_str(n);
    }
    b.finish(std::time::SystemTime::UNIX_EPOCH, 0, None)
}

fn run(names: &[String], query: &str, kind: MatcherKind) -> SearchOutcome {
    matcher::search(
        &snapshot(names),
        &Query::contains(query),
        kind,
        &Hidden::none(),
        &CancelToken::never(),
    )
    .expect("query should be acceptable")
}

/// Asserts both implementations agree, in every field.
fn assert_parity(names: &[String], query: &str) {
    let (want_paths, want_total, want_matched) = reference(names, query);

    for kind in [MatcherKind::Simd, MatcherKind::Naive] {
        let got = run(names, query, kind);
        let got_paths: Vec<String> = got.hits.iter().map(|h| h.path.to_string()).collect();

        assert_eq!(
            got.total,
            want_total as u32,
            "{kind:?}: total differs for {query:?} over {} names",
            names.len()
        );
        assert_eq!(
            got.matched, want_matched as u32,
            "{kind:?}: match count differs for {query:?}"
        );
        assert_eq!(
            got_paths, want_paths,
            "{kind:?}: result order differs for {query:?}"
        );
    }
}

// --- fixed cases -----------------------------------------------------------

#[test]
fn parity_on_a_simple_listing() {
    let names = ["Report_ABC123.pdf", "other.txt", "abc123_more.pdf"].map(String::from);
    assert_parity(&names, "abc123");
}

#[test]
fn parity_when_ranking_by_match_position() {
    let names = ["xxabc.txt", "abc.txt", "xabc.txt"].map(String::from);
    assert_parity(&names, "abc");
}

#[test]
fn parity_when_breaking_ties_by_name_length() {
    let names = ["abc_long_name.txt", "abc.txt", "abc_mid.txt", "abc_x.txt"].map(String::from);
    assert_parity(&names, "abc");
}

/// The original sort was *stable*, so identical keys kept directory order.
/// The new key includes the entry index precisely so an unstable parallel
/// merge reproduces that.
#[test]
fn parity_when_every_key_is_identical() {
    let names: Vec<String> = (0..50).map(|_| "abc.txt".to_string()).collect();
    assert_parity(&names, "abc");
}

#[test]
fn parity_when_more_match_than_can_be_displayed() {
    let names: Vec<String> = (0..500).map(|i| format!("abc{i:04}.pdf")).collect();
    assert_parity(&names, "abc");
}

#[test]
fn parity_when_nothing_matches() {
    let names = ["alpha.txt", "beta.txt"].map(String::from);
    assert_parity(&names, "zzz");
}

#[test]
fn parity_on_an_empty_listing() {
    assert_parity(&[], "abc");
}

#[test]
fn parity_with_repeated_occurrences_in_one_name() {
    // The count must be of entries, not occurrences.
    let names = ["abcabcabc.txt", "abc.txt"].map(String::from);
    assert_parity(&names, "abc");
}

#[test]
fn parity_with_names_that_are_prefixes_of_each_other() {
    let names = ["abc", "abcd", "abcde", "ab", "abcdef"].map(String::from);
    assert_parity(&names, "abc");
}

#[test]
fn parity_with_one_byte_names() {
    let names = ["a", "b", "aaa", "aa"].map(String::from);
    assert_parity(&names, "aaa");
}

#[test]
fn parity_with_mixed_case() {
    let names = ["ABC.TXT", "abc.txt", "AbC.TxT"].map(String::from);
    assert_parity(&names, "AbC");
}

#[test]
fn parity_when_the_query_is_the_whole_name() {
    let names = ["abc", "xabc"].map(String::from);
    assert_parity(&names, "abc");
}

#[test]
fn parity_across_many_rayon_chunks() {
    // Comfortably more than one chunk, so the parallel merge is exercised
    // against the serial oracle.
    let names: Vec<String> = (0..40_000)
        .map(|i| format!("job_{i:06}_report.pdf"))
        .collect();
    for query in ["job", "report", "_012", "0999", ".pdf", "zzz"] {
        assert_parity(&names, query);
    }
}

#[test]
fn parity_with_a_query_longer_than_every_name() {
    let names = ["ab.txt", "cd.txt"].map(String::from);
    assert_parity(&names, &"x".repeat(200));
}

#[test]
fn parity_with_punctuation_heavy_names() {
    let names = [
        "11-D-0704 (rev 2).pdf",
        "11-D-0704.pdf",
        "copy of 11-D-0704 [old].pdf",
        "P12345-001.dwg",
    ]
    .map(String::from);
    assert_parity(&names, "11-d-0704");
    assert_parity(&names, "-00");
}

// --- non-ASCII -------------------------------------------------------------

/// The length-preserving fold matches the original for every case that
/// folds to an equal-length form, which is all of Latin-1, Latin Extended-A,
/// Cyrillic and Greek.
#[test]
fn parity_on_ordinary_non_ascii_names() {
    let names = ["Écoles-Été.pdf", "ПРИВЕТ.txt", "ΣΙΓΜΑ.doc", "Ünïcödé.dat"].map(String::from);
    for query in ["été", "привет", "σιγμα", "ödé"] {
        assert_parity(&names, query);
    }
}

/// The documented divergence, asserted so it stays deliberate rather than
/// becoming a surprise.
///
/// U+212A KELVIN SIGN lowercases to `k`, which changes byte length, so the
/// fold declines it. The fast path therefore misses - and the Unicode
/// fallback exists to rescue exactly this, which means the observable
/// behaviour still matches the original.
#[test]
fn length_changing_folds_are_rescued_by_the_unicode_fallback() {
    let names = ["kelvin_report.pdf".to_string()];
    let (_, _, want_matched) = reference(&names, "\u{212A}elvin");

    let got = run(&names, "\u{212A}elvin", MatcherKind::Simd);
    assert_eq!(got.matched, want_matched as u32);
    assert!(got.unicode_fallback, "the slow path should have been used");
}

// --- properties ------------------------------------------------------------

/// Names built from a small alphabet, so queries actually hit.
fn name_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![Just('a'), Just('b'), Just('c'), Just('-'), Just('1')],
        0..12,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

fn query_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![Just('a'), Just('b'), Just('c'), Just('-'), Just('1')],
        3..6,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// The core property: whatever the listing and whatever the query, the
    /// new matcher agrees with the original in every observable respect.
    #[test]
    fn matcher_agrees_with_the_original_implementation(
        names in proptest::collection::vec(name_strategy(), 0..60),
        query in query_strategy(),
    ) {
        let (want_paths, want_total, want_matched) = reference(&names, &query);
        let got = run(&names, &query, MatcherKind::Simd);
        let got_paths: Vec<String> = got.hits.iter().map(|h| h.path.to_string()).collect();

        prop_assert_eq!(got.total, want_total as u32);
        prop_assert_eq!(got.matched, want_matched as u32);
        prop_assert_eq!(got_paths, want_paths);
    }

    /// The parallel path and the straightforward path must never disagree.
    #[test]
    fn the_simd_and_naive_matchers_agree(
        names in proptest::collection::vec(name_strategy(), 0..80),
        query in query_strategy(),
    ) {
        let simd = run(&names, &query, MatcherKind::Simd);
        let naive = run(&names, &query, MatcherKind::Naive);
        prop_assert_eq!(simd.matched, naive.matched);
        prop_assert_eq!(simd.total, naive.total);
        prop_assert_eq!(simd.hits, naive.hits);
    }

    /// Structural invariants must hold for any listing at all, including
    /// empty names and duplicates.
    #[test]
    fn snapshots_are_always_structurally_sound(
        names in proptest::collection::vec(name_strategy(), 0..120),
    ) {
        let snap = snapshot(&names);
        prop_assert!(snap.check_invariants().is_ok());
        prop_assert!(snap.sample_separators(1).is_ok());
        prop_assert_eq!(snap.len(), names.len());
        for (i, name) in names.iter().enumerate() {
            let shown = snap.display_name(i as u32).into_owned();
            prop_assert_eq!(shown.as_str(), name.as_str());
            prop_assert_eq!(snap.name_len(i as u32) as usize, name.len());
        }
    }

    /// Folding must never change the byte length, since one offsets table
    /// addresses both arenas.
    #[test]
    fn folding_preserves_byte_length(text in ".{0,64}") {
        let mut out = Vec::new();
        files::util::fold::fold_into(&text, &mut out);
        prop_assert_eq!(out.len(), text.len());
    }

    /// A folded haystack contains a folded needle exactly when the original
    /// contains it case-insensitively. Divergence here would be a silent
    /// zero-match that is impossible to reproduce on a developer machine.
    #[test]
    fn folded_containment_matches_case_insensitive_containment(
        haystack in "[a-zA-Z0-9 _.-]{0,40}",
        needle in "[a-zA-Z0-9 _.-]{1,8}",
    ) {
        let mut h = Vec::new();
        files::util::fold::fold_into(&haystack, &mut h);
        let n = files::util::fold::fold_query(&needle);

        let folded_hit = h.windows(n.len()).any(|w| w == n.as_slice());
        let expected = haystack.to_lowercase().contains(&needle.to_lowercase());
        prop_assert_eq!(folded_hit, expected, "{} vs {}", haystack, needle);
    }
}
