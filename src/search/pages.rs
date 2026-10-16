//! Finds the files that are the pages of one job code's document.
//!
//! Documents on these shares are stored one file per page:
//!
//! ```text
//! 11-D-0704.pdf   11-D-0704_Page1.pdf   ...   11-D-0704Page12.pdf
//! ```
//!
//! so opening `11-D-0704` means thirteen files. This module finds them and
//! orders them.
//!
//! # Not built on `matcher::search`
//!
//! The matcher keeps the best `topk::K` matches ranked by position and length,
//! which would truncate long sets and sort `_Page10` before `_Page2`.
//!
//! # Why an uncapped sweep is affordable
//!
//! Membership is equality: a member's name is exactly
//! `code + marker? + ".pdf"`, so its length is `code.len() + marker.len() + 4`
//! with the marker bounded by [`MAX_MARKER_LEN`]. [`Snapshot::name_len`] is
//! O(1) and never touches the name arena, so nearly every entry is rejected by
//! an integer compare. The sweep is serial because it runs once per Enter and
//! the result is re-sorted into page order anyway.

use std::sync::Arc;

use crate::config::MATCH_CHUNK_ENTRIES;
use crate::index::snapshot::Snapshot;
use crate::util::cancel::CancelToken;
use crate::util::fold;

/// The extension a page must have.
///
/// A sibling such as `11-D-0704_Page2.tif` is not a member and is skipped
/// without a warning.
const PAGE_EXT: &str = ".pdf";

/// Longest trailing page marker the length pre-filter lets through.
///
/// Must be at least as long as anything [`marker_page`] accepts: separator,
/// `page`, separator and up to ten digits (`u32::MAX`). A shorter value
/// silently drops pages the membership rule would accept.
const MAX_MARKER_LEN: usize = 1 + 4 + 1 + 10;

/// Hard ceiling on one document's page set.
///
/// Bounds allocation and merge time for pathological name sets; it is not a
/// display cap.
pub const MAX_PAGES: usize = 512;

/// One file in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub path: Arc<str>,
    pub name: Arc<str>,
    /// `None` for the bare, unsuffixed file, which merges first.
    pub page: Option<u32>,
}

/// A code's pages, in merge order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageGroup {
    /// Bare file first, then ascending page number.
    pub pages: Vec<Page>,
    /// [`MAX_PAGES`] was reached, so this is not the whole document.
    pub capped: bool,
    /// The sweep was abandoned; the contents are meaningless.
    pub cancelled: bool,
}

impl PageGroup {
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Returns whether `path` is one of these pages.
    ///
    /// Decides whether Enter on a row opens the document or only that file.
    pub fn contains_path(&self, path: &str) -> bool {
        self.pages.iter().any(|p| eq_fold(&p.path, path))
    }
}

/// Returns whether `name` is a page of `code`'s document, and which page.
///
/// `Some(None)` is the bare, unsuffixed file. `None` means not a member.
///
/// Pure and total: `name` is a file name, nothing touches the filesystem, and
/// no input panics. The whole membership rule lives here.
pub fn page_of(code: &str, name: &str) -> Option<Option<u32>> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    let stem = name.strip_suffix_fold(PAGE_EXT)?;

    // Exactly the code: the bare file, which merges first.
    if eq_fold(stem, code) {
        return Some(None);
    }

    // Otherwise the code must be a prefix followed only by a page marker.
    // `11-D-0704 revision notes` and `11-D-0704-A2` are different documents.
    let rest = strip_prefix_fold(stem, code)?;
    marker_page(rest).map(Some)
}

/// Parses a trailing page marker.
///
/// Recognised, case-insensitively:
///
/// ```text
/// _Page1    Page12    _P3    -p4    " page 5"
/// ```
///
/// The letter is required. A bare trailing number such as `11-D-0704-2` is
/// usually a different code, and treating it as a page would merge an
/// unrelated drawing into the document.
fn marker_page(rest: &str) -> Option<u32> {
    let b = rest.as_bytes();
    let mut i = 0;

    // Separator, optional.
    if i < b.len() && matches!(b[i], b'_' | b'-' | b' ' | b'.') {
        i += 1;
    }

    // `p`, `pg` or `page`, required.
    if i >= b.len() || !b[i].eq_ignore_ascii_case(&b'p') {
        return None;
    }
    i += 1;
    for word in [b"age".as_slice(), b"g".as_slice()] {
        if b.len() >= i + word.len() && b[i..i + word.len()].eq_ignore_ascii_case(word) {
            i += word.len();
            break;
        }
    }

    // Separator again, optional, so `page 4` and `page_4` both work.
    if i < b.len() && matches!(b[i], b'_' | b'-' | b' ') {
        i += 1;
    }

    // Digits to the end, and nothing after them.
    let digits = &rest[i..];
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // `.ok()?` rather than a panic: an overflowing number is not a page.
    digits.parse::<u32>().ok()
}

/// Collects `code`'s pages from `snap`, in merge order.
pub fn collect(snap: &Snapshot, code: &str, cancel: &CancelToken) -> PageGroup {
    let mut out = PageGroup::default();
    let code = code.trim();
    if code.is_empty() {
        return out;
    }

    // Folded once and compared against the folded arena, so this agrees with
    // the matcher byte for byte. See `util::fold`.
    let needle = fold::fold_query(code);
    let min_len = needle.len() + PAGE_EXT.len();
    let max_len = min_len + MAX_MARKER_LEN;

    for i in 0..snap.len() as u32 {
        // One check per chunk, as in `matcher::naive`: prompt cancellation
        // without a branch per entry.
        if (i as usize).is_multiple_of(MATCH_CHUNK_ENTRIES) && cancel.is_cancelled() {
            out.cancelled = true;
            return out;
        }

        // Integer compare against the offsets table; the arena is untouched.
        let len = snap.name_len(i) as usize;
        if len < min_len || len > max_len {
            continue;
        }

        let lower = snap.name_lower(i);
        if !lower.starts_with(&needle[..]) {
            continue;
        }
        let Ok(name) = std::str::from_utf8(lower) else {
            continue;
        };
        let Some(page) = page_of(code, name) else {
            continue;
        };

        out.pages.push(Page {
            path: Arc::from(snap.full_path(i).as_str()),
            name: Arc::from(snap.display_name(i).as_ref()),
            page,
        });
    }

    sort_pages(&mut out.pages);

    // Truncate after sorting: SMB directory order is not page order, so
    // cutting during the scan would keep an arbitrary subset. This keeps the
    // lowest-numbered pages and the bare cover sheet.
    if out.pages.len() > MAX_PAGES {
        out.pages.truncate(MAX_PAGES);
        out.capped = true;
    }
    out
}

/// Puts a group into merge order.
///
/// A strict total order (see [`crate::search::topk`]) so pages never reorder
/// between runs: bare file first, then the page number as an integer so
/// `Page2` precedes `Page10`, then the name so `_Page01` and `_Page1` cannot
/// swap.
fn sort_pages(pages: &mut [Page]) {
    pages.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Case-insensitive equality over the crate's canonical fold.
fn eq_fold(a: &str, b: &str) -> bool {
    a.len() == b.len() && fold::fold_query(a) == fold::fold_query(b)
}

fn strip_prefix_fold<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    // The fold preserves byte length (see `util::fold`), so a prefix of the
    // folded string is a prefix of the original at the same offset.
    //
    // `split_at_checked` because `split_at` panics inside a multi-byte
    // character, and share names such as `Rapport-Été.pdf` are common. An
    // offset off a character boundary means the text is not the code.
    let (head, rest) = s.split_at_checked(prefix.len())?;
    eq_fold(head, prefix).then_some(rest)
}

trait StripSuffixFold {
    fn strip_suffix_fold(&self, suffix: &str) -> Option<&str>;
}

impl StripSuffixFold for str {
    fn strip_suffix_fold(&self, suffix: &str) -> Option<&str> {
        if self.len() <= suffix.len() {
            return None;
        }
        // See `strip_prefix_fold`: `split_at` panics off a character boundary,
        // and this runs on every candidate before the tail is known to be an
        // extension.
        let (head, tail) = self.split_at_checked(self.len() - suffix.len())?;
        eq_fold(tail, suffix).then_some(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;
    use std::time::SystemTime;

    fn snap(names: &[&str]) -> Snapshot {
        let mut b = SnapshotBuilder::with_capacity(r"R:\11d", names.len(), 32);
        for n in names {
            b.push_str(n);
        }
        b.finish(SystemTime::UNIX_EPOCH, 0, None)
    }

    fn order(group: &PageGroup) -> Vec<&str> {
        group.pages.iter().map(|p| p.name.as_ref()).collect()
    }

    // --- the membership rule ------------------------------------------------

    #[test]
    fn the_bare_file_and_its_pages_form_one_group() {
        assert_eq!(page_of("11-d-1234", "11-d-1234.pdf"), Some(None));
        assert_eq!(page_of("11-d-1234", "11-d-1234_Page1.pdf"), Some(Some(1)));
        assert_eq!(page_of("11-d-1234", "11-d-1234Page12.pdf"), Some(Some(12)));
    }

    #[test]
    fn every_page_marker_spelling_is_recognised() {
        for (name, want) in [
            ("11-d-1234_Page1.pdf", 1u32),
            ("11-d-1234Page2.pdf", 2),
            ("11-d-1234_P3.pdf", 3),
            ("11-d-1234-p4.pdf", 4),
            ("11-d-1234 page 5.pdf", 5),
            ("11-d-1234_pg6.pdf", 6),
            ("11-d-1234_PAGE007.pdf", 7),
        ] {
            assert_eq!(
                page_of("11-d-1234", name),
                Some(Some(want)),
                "{name} should be page {want}"
            );
        }
    }

    /// Membership is equality against the code, so a file that merely starts
    /// with the code is a different document.
    #[test]
    fn a_name_with_extra_text_is_not_a_page() {
        assert_eq!(page_of("11-d-1234", "11-d-1234 revision notes.pdf"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234 sketch.pdf"), None);
    }

    #[test]
    fn a_different_code_is_not_a_page() {
        assert_eq!(page_of("11-d-1234", "11-d-1234-A2.pdf"), None);
        assert_eq!(page_of("11-d-1234", "11-d-12345.pdf"), None);
        assert_eq!(page_of("11-d-1234", "99-d-1234.pdf"), None);
    }

    /// A sibling in another format is not a page and produces no warning.
    #[test]
    fn a_non_pdf_sibling_is_not_a_member() {
        assert_eq!(page_of("11-d-1234", "11-d-1234_Page2.tif"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234.txt"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234"), None);
    }

    /// `11-d-1234-2` is usually a different code, not page two.
    #[test]
    fn a_trailing_number_without_a_letter_is_not_a_page_marker() {
        assert_eq!(page_of("11-d-1234", "11-d-1234-2.pdf"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234_2.pdf"), None);
        assert_eq!(page_of("11-d-1234", "11-d-12342.pdf"), None);
    }

    #[test]
    fn the_code_is_matched_case_insensitively() {
        assert_eq!(page_of("11-d-1234", "11-D-1234.PDF"), Some(None));
        assert_eq!(page_of("11-D-1234", "11-d-1234_page3.pdf"), Some(Some(3)));
    }

    /// `self.input` is not trimmed, so a pasted code may carry a trailing space.
    #[test]
    fn a_query_with_surrounding_whitespace_still_forms_its_group() {
        assert_eq!(
            page_of("  11-d-1234 ", "11-d-1234_Page1.pdf"),
            Some(Some(1))
        );
        let g = collect(
            &snap(&["11-d-1234_Page1.pdf"]),
            " 11-d-1234 ",
            &CancelToken::never(),
        );
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn an_absurd_page_number_is_not_a_page_rather_than_a_panic() {
        assert_eq!(page_of("11-d-1234", "11-d-1234_Page99999999999.pdf"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234_Page.pdf"), None);
    }

    /// `strip_suffix_fold` must not panic on names whose split offset falls
    /// inside a multi-byte character.
    #[test]
    fn a_multi_byte_name_is_not_a_page_rather_than_a_panic() {
        for name in [
            "11-d-1234 rev\u{2019} 2",
            "11-d-1234\u{2013}v2",
            "11-d-1234\u{4e2d}\u{6587}",
            "\u{20ac}ab",
            "abc\u{20ac}abc",
            "11-d-1234.p\u{e9}f",
        ] {
            let _ = page_of("11-d-1234", name);
            let _ = page_of("abc", name);
            let _ = page_of("a", name);
        }
    }

    #[test]
    fn a_multi_byte_name_in_the_listing_does_not_panic_the_sweep() {
        let g = collect(
            &snap(&["abc\u{20ac}abc", "abc\u{20ac}.pdf", "abc.pdf"]),
            "abc",
            &CancelToken::never(),
        );
        assert_eq!(order(&g), ["abc.pdf"], "only the real page is a member");
    }

    #[test]
    fn an_empty_code_matches_nothing() {
        assert_eq!(page_of("", "11-d-1234.pdf"), None);
        assert!(collect(&snap(&["a.pdf"]), "   ", &CancelToken::never()).is_empty());
    }

    // --- ordering -----------------------------------------------------------

    #[test]
    fn the_unsuffixed_file_merges_first() {
        let g = collect(
            &snap(&[
                "11-d-1234_Page2.pdf",
                "11-d-1234.pdf",
                "11-d-1234_Page1.pdf",
            ]),
            "11-d-1234",
            &CancelToken::never(),
        );
        assert_eq!(
            order(&g),
            [
                "11-d-1234.pdf",
                "11-d-1234_Page1.pdf",
                "11-d-1234_Page2.pdf"
            ]
        );
    }

    /// Sorted as text, `_Page10` would land between `_Page1` and `_Page2`.
    #[test]
    fn pages_merge_in_numeric_order_not_alphabetical() {
        let names: Vec<String> = (1..=12).map(|i| format!("11-d-1234_Page{i}.pdf")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = collect(&snap(&refs), "11-d-1234", &CancelToken::never());

        let pages: Vec<u32> = g.pages.iter().filter_map(|p| p.page).collect();
        assert_eq!(pages, (1..=12).collect::<Vec<_>>());
    }

    /// Two spellings of the same page must not swap between runs.
    #[test]
    fn duplicate_page_numbers_keep_a_stable_order() {
        let a = collect(
            &snap(&["11-d-1234_Page01.pdf", "11-d-1234_Page1.pdf"]),
            "11-d-1234",
            &CancelToken::never(),
        );
        let b = collect(
            &snap(&["11-d-1234_Page1.pdf", "11-d-1234_Page01.pdf"]),
            "11-d-1234",
            &CancelToken::never(),
        );
        assert_eq!(order(&a), order(&b));
        assert_eq!(order(&a), ["11-d-1234_Page1.pdf", "11-d-1234_Page01.pdf"]);
    }

    // --- the sweep ----------------------------------------------------------

    #[test]
    fn collecting_keeps_only_the_documents_own_pages() {
        let g = collect(
            &snap(&[
                "11-d-1234.pdf",
                "11-d-1234_Page1.pdf",
                "11-d-1234Page12.pdf",
                "11-d-1234_Page2.tif",
                "11-d-1234 revision notes.pdf",
                "11-d-1234-A2.pdf",
                "99-x-0001.pdf",
            ]),
            "11-d-1234",
            &CancelToken::never(),
        );
        assert_eq!(
            order(&g),
            [
                "11-d-1234.pdf",
                "11-d-1234_Page1.pdf",
                "11-d-1234Page12.pdf"
            ]
        );
        assert!(!g.capped);
        assert!(!g.cancelled);
    }

    /// The length pre-filter must accept every name the parser accepts.
    #[test]
    fn the_length_filter_admits_everything_the_rule_accepts() {
        for name in [
            "11-d-1234 page 1234.pdf",
            "11-d-1234_page_1234.pdf",
            "11-d-1234_Page10000.pdf",
            "11-d-1234-p-999999.pdf",
        ] {
            assert!(
                page_of("11-d-1234", name).is_some(),
                "{name} should be a page"
            );
            let g = collect(&snap(&[name]), "11-d-1234", &CancelToken::never());
            assert_eq!(g.len(), 1, "{name} was filtered out before it was parsed");
        }
    }

    #[test]
    fn the_full_path_of_each_page_is_materialised() {
        let g = collect(
            &snap(&["11-d-1234.pdf"]),
            "11-d-1234",
            &CancelToken::never(),
        );
        assert_eq!(&*g.pages[0].path, r"R:\11d\11-d-1234.pdf");
        assert!(g.contains_path(r"R:\11D\11-D-1234.PDF"), "paths fold too");
        assert!(!g.contains_path(r"R:\11d\other.pdf"));
    }

    /// The length pre-filter actually rejects candidates.
    #[test]
    fn a_broad_query_against_a_large_listing_finds_nothing_to_allocate() {
        let names: Vec<String> = (0..50_000)
            .map(|i| format!("job_{i:07}_report_{i}.pdf"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = collect(&snap(&refs), "job", &CancelToken::never());
        assert!(g.is_empty(), "a prefix is not a document");
    }

    #[test]
    fn collection_stops_when_the_work_is_superseded() {
        let names: Vec<String> = (0..MATCH_CHUNK_ENTRIES + 16)
            .map(|i| format!("11-d-1234_Page{i}.pdf"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();

        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.bump());
        epoch.bump(); // supersede it before the sweep starts

        let g = collect(&snap(&refs), "11-d-1234", &token);
        assert!(g.cancelled);
        assert!(g.is_empty());
    }

    #[test]
    fn a_group_larger_than_the_ceiling_reports_itself_capped() {
        let names: Vec<String> = (1..=MAX_PAGES + 10)
            .map(|i| format!("11-d-1234_Page{i}.pdf"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = collect(&snap(&refs), "11-d-1234", &CancelToken::never());

        assert!(
            g.capped,
            "the user must be told it is not the whole document"
        );
        assert_eq!(g.len(), MAX_PAGES);

        // Directory order is not page order on SMB, so truncation must happen
        // after the sort to keep the front of the document.
        let kept: Vec<u32> = g.pages.iter().filter_map(|p| p.page).collect();
        assert_eq!(kept, (1..=MAX_PAGES as u32).collect::<Vec<_>>());
    }

    /// Fuzzes the membership rule with arbitrary Unicode to check it is total.
    /// Follows the idiom of `tests/matcher_parity.rs`.
    mod properties {
        use super::snap;
        use crate::search::pages::{collect, page_of};
        use crate::util::cancel::CancelToken;
        use proptest::prelude::*;

        /// Alphabet of one-, two-, three- and four-byte characters, so split
        /// offsets land inside characters. Kept small, via `prop_oneof!`, so
        /// generated names collide with generated codes.
        fn piece() -> impl Strategy<Value = char> {
            prop_oneof![
                Just('a'),
                Just('b'),
                Just('-'),
                Just('1'),
                Just('.'),
                Just('p'),
                Just('\u{e9}'),    // 2 bytes
                Just('\u{20ac}'),  // 3 bytes
                Just('\u{1f600}'), // 4 bytes
            ]
        }

        fn text(len: std::ops::Range<usize>) -> impl Strategy<Value = String> {
            proptest::collection::vec(piece(), len).prop_map(|cs| cs.into_iter().collect())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// `page_of` must not panic on any input.
            #[test]
            fn the_membership_rule_never_panics_on_any_name(
                code in text(1..12),
                name in text(0..24),
            ) {
                let _ = page_of(&code, &name);
            }

            #[test]
            fn the_sweep_never_panics_on_any_listing(
                code in text(1..12),
                names in proptest::collection::vec(text(0..24), 0..16),
            ) {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let _ = collect(&snap(&refs), &code, &CancelToken::never());
            }
        }
    }

    #[test]
    fn an_empty_listing_produces_an_empty_group() {
        let g = collect(&snap(&[]), "11-d-1234", &CancelToken::never());
        assert!(g.is_empty());
        assert!(!g.cancelled);
    }
}
