//! Which files are the pages of one job code's document.
//!
//! The documents on these shares are stored one file per page:
//!
//! ```text
//! 11-D-0704.pdf   11-D-0704_Page1.pdf   ...   11-D-0704Page12.pdf
//! ```
//!
//! so "open `11-D-0704`" means thirteen files, not one. This module answers
//! which thirteen, and in what order.
//!
//! # Why this is not built on `matcher::search`
//!
//! The matcher answers a different question. It ranks the *best fifteen*
//! substring matches by match position (`topk::K`, `crate::search::topk`), and
//! both halves of that are wrong here: a twenty-page drawing set comes back
//! truncated at fifteen, and `_Page10` sorts ahead of `_Page2` because its
//! name is one byte longer. Neither is a bug in the matcher - it is simply
//! being asked something it was not built to answer.
//!
//! # Why an uncapped sweep is affordable
//!
//! The membership rule is an *equality*, not a containment: a member's name is
//! exactly `code + marker? + ".pdf"`. That pins its length to
//! `code.len() + marker.len() + 4`, with the marker bounded by
//! [`MAX_MARKER_LEN`]. [`Snapshot::name_len`] is O(1) from the offsets array
//! and never touches the name arena, so a three-character query rejects
//! essentially all 1.3 million entries of the flat index on an integer
//! compare, before a single byte is read. That is what makes scanning
//! everything cheaper than ranking anything.
//!
//! Serial rather than rayon, which is the opposite trade from `matcher::sweep`
//! and deliberately so: this runs once when Enter is pressed, not on every
//! keystroke, and a parallel merge would have to be re-sorted into page order
//! anyway.

use std::sync::Arc;

use crate::config::MATCH_CHUNK_ENTRIES;
use crate::index::snapshot::Snapshot;
use crate::util::cancel::CancelToken;
use crate::util::fold;

/// The extension a page must have.
///
/// Membership requires it, so a stray `11-D-0704_Page2.tif` beside the PDFs is
/// simply not a page of this document and is passed over in silence. The
/// alternative - admitting it and then reporting that it could not be merged -
/// would put a warning on screen every single time anyone opened that job.
const PAGE_EXT: &str = ".pdf";

/// Longest trailing page marker the length pre-filter will let through.
///
/// It has to be at least as long as anything [`marker_page`] accepts, which is
/// a separator, `page`, *another* separator and up to ten digits - `u32::MAX`
/// is ten digits long. Being generous here costs one integer comparison; being
/// short silently drops pages the membership rule would have accepted, with
/// `capped` unset and nothing on screen to say a page went missing.
///
/// The earlier value of 9 did exactly that: it forgot the second separator, so
/// `11-D-0704 page 1234.pdf` was rejected by the filter and accepted by the
/// parser.
const MAX_MARKER_LEN: usize = 1 + 4 + 1 + 10;

/// Hard ceiling on one document's page set.
///
/// Not a display cap, and not a judgement about how long a drawing set can be.
/// It is the bound that stops a pathological set of names turning one keypress
/// into an unbounded allocation and a merge that never finishes.
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
    /// [`MAX_PAGES`] was reached, so this is not the whole document. Said out
    /// loud rather than silently truncated.
    pub capped: bool,
    /// The sweep was abandoned; the contents mean nothing.
    pub cancelled: bool,
}

impl PageGroup {
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether `path` is one of these pages.
    ///
    /// Used to decide whether Enter on a given row means "open this document"
    /// or "open the one file you are pointing at".
    pub fn contains_path(&self, path: &str) -> bool {
        self.pages.iter().any(|p| eq_fold(&p.path, path))
    }
}

/// Whether `name` is a page of `code`'s document, and which page it is.
///
/// `Some(None)` is the bare, unsuffixed file. `None` means not a member.
///
/// Pure and total: `name` is a file name rather than a path, nothing here
/// touches the filesystem, and no input panics. The entire membership rule
/// lives in this function, which is what makes it testable on a machine with
/// no shares mounted.
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

    // Otherwise the code must be a prefix, and everything after it must be a
    // page marker and nothing else. `11-D-0704 revision notes` and
    // `11-D-0704-A2` both fail here, which is the point: they are different
    // documents that happen to share a prefix.
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
/// The letter is required. A bare trailing number is deliberately **not** a
/// page marker: `11-D-0704-2` is a different code far more often than it is
/// page two of this one, and guessing wrong staples an unrelated drawing into
/// the middle of someone's document without saying anything.
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
    // `.ok()?` rather than a panic: `Page99999999999` is someone else's naming
    // scheme, not a page of ours.
    digits.parse::<u32>().ok()
}

/// Collects `code`'s pages from `snap`, in merge order.
pub fn collect(snap: &Snapshot, code: &str, cancel: &CancelToken) -> PageGroup {
    let mut out = PageGroup::default();
    let code = code.trim();
    if code.is_empty() {
        return out;
    }

    // Folded once, compared against the folded arena, so this agrees with the
    // matcher byte for byte. See `util::fold` for why there is only one fold.
    let needle = fold::fold_query(code);
    let min_len = needle.len() + PAGE_EXT.len();
    let max_len = min_len + MAX_MARKER_LEN;

    for i in 0..snap.len() as u32 {
        // One check per chunk, matching `matcher::naive`: enough to abandon a
        // superseded sweep promptly without a branch per entry.
        if (i as usize).is_multiple_of(MATCH_CHUNK_ENTRIES) && cancel.is_cancelled() {
            out.cancelled = true;
            return out;
        }

        // The whole reason this is affordable: an integer compare against the
        // offsets table, with the arena untouched.
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

    // Truncated *after* sorting, deliberately. Cutting during the scan kept
    // the first `MAX_PAGES` the enumerator happened to return, and directory
    // order is not page order on SMB - so an over-long set came back as an
    // arbitrary subset with holes in it, and could lose the bare cover sheet
    // entirely. Cutting here keeps the lowest-numbered pages, which is the
    // front of the document.
    if out.pages.len() > MAX_PAGES {
        out.pages.truncate(MAX_PAGES);
        out.capped = true;
    }
    out
}

/// Puts a group into merge order.
///
/// A **strict total order**, for the reason spelled out in
/// [`crate::search::topk`]: ties that are broken arbitrarily reshuffle between
/// runs, and here that would silently reorder the pages of a document. The
/// bare file first, then the page number as an integer so `Page2` precedes
/// `Page10`, then the name so `_Page01` and `_Page1` cannot swap.
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
    // The fold is byte-length preserving (see `util::fold`), so a prefix of
    // the folded string is a prefix of the original at the same offset.
    //
    // `split_at_checked` rather than `split_at`: the latter *panics* when the
    // offset falls inside a multi-byte character, and these names come off a
    // share where `Rapport-Été.pdf` is an ordinary thing to find. An offset
    // that is not a character boundary means the text there is not the code,
    // so `None` - not a member - is the right answer.
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
        // and this one is reached first, on every candidate name, before
        // anything has established that the tail is even an extension.
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

    /// The rule is equality against the code, not containment, so a file that
    /// merely starts with the code is a different document.
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

    /// A sibling in another format is simply not a page, and says nothing.
    /// Admitting it and then reporting that it could not be merged would warn
    /// on every open of that job, forever.
    #[test]
    fn a_non_pdf_sibling_is_not_a_member() {
        assert_eq!(page_of("11-d-1234", "11-d-1234_Page2.tif"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234.txt"), None);
        assert_eq!(page_of("11-d-1234", "11-d-1234"), None);
    }

    /// `11-d-1234-2` is a different code far more often than it is page two.
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

    /// `self.input` is never trimmed on the way in, so a pasted code can
    /// arrive with a trailing space. Without this the group comes back empty
    /// and nothing explains why.
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

    /// `split_at` panics off a character boundary, and `strip_suffix_fold`
    /// reaches it before anything has checked the tail is an extension. Names
    /// like these are ordinary on a share holding French or CJK documents, and
    /// the panic killed the open worker for the rest of the session.
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

    /// The bug this pins: sorted as text, `_Page10` lands between `_Page1` and
    /// `_Page2`, and the merged document is silently out of order.
    #[test]
    fn pages_merge_in_numeric_order_not_alphabetical() {
        let names: Vec<String> = (1..=12).map(|i| format!("11-d-1234_Page{i}.pdf")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let g = collect(&snap(&refs), "11-d-1234", &CancelToken::never());

        let pages: Vec<u32> = g.pages.iter().filter_map(|p| p.page).collect();
        assert_eq!(pages, (1..=12).collect::<Vec<_>>());
    }

    /// Two spellings of the same page must not swap between runs, or the
    /// merged document reshuffles for no visible reason.
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

    /// The pre-filter and the parser have to agree. When the filter was the
    /// shorter of the two it rejected names the rule accepts, and did it
    /// silently - the page simply was not in the document.
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

    /// The length pre-filter is the whole performance argument, so it is worth
    /// proving it actually rejects rather than merely being present.
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

        // Truncating during the scan kept whatever the enumerator happened to
        // return first, and directory order is not page order on SMB - so an
        // over-long set came back as an arbitrary subset with holes in it, and
        // could lose the bare cover sheet entirely. Cutting after the sort
        // keeps the front of the document.
        let kept: Vec<u32> = g.pages.iter().filter_map(|p| p.page).collect();
        assert_eq!(kept, (1..=MAX_PAGES as u32).collect::<Vec<_>>());
    }

    /// The membership rule is pure and total, and the only way to be sure of
    /// that against arbitrary Unicode is to throw arbitrary Unicode at it.
    /// `tests/matcher_parity.rs` establishes this idiom for the same class of
    /// bug - one that does not crash in review, only in production.
    mod properties {
        use super::snap;
        use crate::search::pages::{collect, page_of};
        use crate::util::cancel::CancelToken;
        use proptest::prelude::*;

        /// An alphabet chosen to land split offsets inside characters: one
        /// ASCII byte, two, three and four. Built from `prop_oneof!` rather
        /// than a regex for the same reason `tests/matcher_parity.rs` does -
        /// a small alphabet means the generated names actually collide with
        /// the generated codes instead of being uniformly unrelated.
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

            /// `page_of` documents itself as total. Against arbitrary Unicode
            /// that is a claim, not a fact, until it is fuzzed - the
            /// `split_at` panic it once had was invisible to every
            /// hand-written case.
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
