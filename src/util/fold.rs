//! The single canonical case-folding function.
//!
//! Both the indexer and the query path MUST fold through here. If the two
//! sides ever disagree by even one byte the result is a silent zero-match that
//! is essentially impossible to reproduce on a developer machine, so this is
//! deliberately the only implementation in the crate.
//!
//! # The length-preserving contract
//!
//! Folding is *byte-length preserving*: `fold_into` always appends exactly as
//! many bytes as the input occupied. That is what lets a single offsets table
//! in [`crate::index::snapshot::Snapshot`] address both the folded arena and
//! the original-case arena.
//!
//! ASCII takes the obvious path. For non-ASCII we apply `char::to_lowercase`
//! only when it yields exactly one `char` whose UTF-8 encoding is the same
//! length as the input's; otherwise the character is copied through unchanged.
//! That covers Latin-1 Supplement, Latin Extended-A, Cyrillic and Greek - i.e.
//! effectively all real filenames. The cases it declines are length-changing or
//! multi-char folds:
//!
//! | Char | Would fold to | Declined because |
//! |------|---------------|------------------|
//! | `İ` U+0130 | `i` + U+0307 | multi-char |
//! | `ẞ` U+1E9E (3 B) | `ß` U+00DF (2 B) | length change |
//! | `K` U+212A KELVIN (3 B) | `k` (1 B) | length change |
//! | `Å` U+212B ANGSTROM (3 B) | `å` (2 B) | length change |
//!
//! [`needs_unicode_fallback`] reports when a query might be affected, so the
//! matcher can fall back to a slow full-Unicode comparison rather than being
//! silently wrong. See `crate::search::matcher`.

use smallvec::SmallVec;

/// Inline capacity for a folded query. Job codes are far shorter than this.
pub type FoldedQuery = SmallVec<[u8; 64]>;

/// Folds `name` to lowercase, appending to `out`.
///
/// Returns the number of bytes appended, which is always exactly
/// `name.len()`.
pub fn fold_into(name: &str, out: &mut Vec<u8>) -> usize {
    let before = out.len();
    let bytes = name.as_bytes();

    if bytes.is_ascii() {
        // Fast path: ~100% of real filenames. `make_ascii_lowercase` is
        // auto-vectorized by LLVM.
        out.extend_from_slice(bytes);
        out[before..].make_ascii_lowercase();
    } else {
        fold_non_ascii(name, out);
    }

    debug_assert_eq!(
        out.len() - before,
        name.len(),
        "fold must preserve byte length"
    );
    out.len() - before
}

#[cold]
fn fold_non_ascii(name: &str, out: &mut Vec<u8>) {
    let mut buf = [0u8; 4];
    for c in name.chars() {
        if c.is_ascii() {
            out.push((c as u8).to_ascii_lowercase());
            continue;
        }
        let mut lower = c.to_lowercase();
        let folded = match (lower.next(), lower.next()) {
            (Some(l), None) if l.len_utf8() == c.len_utf8() => l,
            _ => c,
        };
        out.extend_from_slice(folded.encode_utf8(&mut buf).as_bytes());
    }
}

/// Folds a query for matching. Stack-allocated for typical inputs.
pub fn fold_query(query: &str) -> FoldedQuery {
    let mut v: Vec<u8> = Vec::with_capacity(query.len());
    fold_into(query, &mut v);
    FoldedQuery::from_vec(v)
}

/// True when `query` contains a character whose correct lowercase form this
/// module declines to apply, so a fast-path miss might be a false negative.
///
/// Any non-ASCII character is treated as suspect. Being conservative costs
/// nothing: the caller only consults this after the fast path already returned
/// zero matches, which for an ASCII query (the overwhelming majority) can never
/// happen because `is_ascii()` short-circuits first.
pub fn needs_unicode_fallback(query: &str) -> bool {
    !query.is_ascii()
}

/// Slow, allocating, fully Unicode-correct containment check.
///
/// Only reachable from the fallback path described above.
pub fn contains_unicode_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_lowercase().find(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(s: &str) -> Vec<u8> {
        let mut v = Vec::new();
        fold_into(s, &mut v);
        v
    }

    #[test]
    fn folds_ascii_to_lowercase() {
        assert_eq!(fold("ABC-123.PDF"), b"abc-123.pdf".to_vec());
    }

    #[test]
    fn leaves_already_lowercase_unchanged() {
        assert_eq!(fold("abc-123.pdf"), b"abc-123.pdf".to_vec());
    }

    #[test]
    fn preserves_byte_length_for_ascii() {
        for s in ["", "a", "Z", "11-D-0704", "MiXeD CaSe .Txt"] {
            assert_eq!(fold(s).len(), s.len(), "length changed for {s:?}");
        }
    }

    #[test]
    fn preserves_byte_length_for_latin1_and_cyrillic_and_greek() {
        // Every one of these folds to a same-length form.
        for s in ["ÉCOLE.pdf", "ÀÈÌÒÙ", "ПРИВЕТ", "ΣΙΓΜΑ", "Ünïcödé"] {
            assert_eq!(fold(s).len(), s.len(), "length changed for {s:?}");
        }
    }

    #[test]
    fn folds_latin1_supplement() {
        assert_eq!(fold("ÉCOLE"), "école".as_bytes().to_vec());
    }

    #[test]
    fn folds_cyrillic() {
        assert_eq!(fold("ПРИВЕТ"), "привет".as_bytes().to_vec());
    }

    #[test]
    fn declines_length_changing_folds_but_preserves_length() {
        // U+212A KELVIN would lowercase to 'k' (1 byte) - declined.
        let s = "\u{212A}";
        assert_eq!(fold(s).len(), s.len());
        assert_eq!(fold(s), s.as_bytes().to_vec());

        // U+1E9E CAPITAL SHARP S would lowercase to U+00DF (2 bytes) - declined.
        let s = "\u{1E9E}";
        assert_eq!(fold(s).len(), s.len());
        assert_eq!(fold(s), s.as_bytes().to_vec());
    }

    #[test]
    fn declines_multi_char_folds_but_preserves_length() {
        // U+0130 lowercases to two chars - declined.
        let s = "\u{0130}";
        assert_eq!(fold(s).len(), s.len());
        assert_eq!(fold(s), s.as_bytes().to_vec());
    }

    #[test]
    fn mixed_ascii_and_non_ascii_preserves_length() {
        let s = "Rapport-Éte\u{0301}-2024_\u{212A}.PDF";
        assert_eq!(fold(s).len(), s.len());
    }

    #[test]
    fn fold_query_matches_fold_into() {
        for s in ["ABC", "ÉCOLE", "11-D-0704"] {
            assert_eq!(fold_query(s).as_slice(), fold(s).as_slice());
        }
    }

    #[test]
    fn ascii_queries_never_need_the_unicode_fallback() {
        assert!(!needs_unicode_fallback("11-D-0704"));
        assert!(needs_unicode_fallback("École"));
    }

    #[test]
    fn folded_containment_agrees_with_case_insensitive_containment_for_ascii() {
        let cases = [
            ("Report_ABC123.pdf", "abc123", true),
            ("Report_ABC123.pdf", "ABC123", true),
            ("Report_ABC123.pdf", "xyz", false),
            ("aaa", "aaaa", false),
            ("aaaa", "aa", true),
        ];
        for (hay, needle, expected) in cases {
            let h = fold(hay);
            let n = fold(needle);
            let got = h.windows(n.len().max(1)).any(|w| w == n.as_slice());
            assert_eq!(got, expected, "{hay:?} contains {needle:?}");
        }
    }
}
