//! Deciding when a query can safely be handed to the file server.
//!
//! `FindFirstFileExW("V:\\*p12345*")` evaluates the pattern **on the server**
//! (MS-SMB2 2.2.33 `QUERY_DIRECTORY` carries a `FileName` field), so a cold
//! search costs one round trip and a few kilobytes no matter whether the
//! directory holds a thousand files or ten million. That is two orders of
//! magnitude better than any client-side buffering trick, and unlike them it
//! is independent of link bandwidth.
//!
//! The catch is that it is only *equivalent* to the local substring match when
//! the query contains nothing the NT expression evaluator treats specially.
//!
//! # A whitelist, not escaping
//!
//! NT expressions have **no escape mechanism** - `\` cannot escape `*` - so
//! attempting to sanitise a query is the wrong model. The only sound approach
//! is to recognise queries that are provably literal and refuse everything
//! else, falling back to the local index.
//!
//! Three ANSI characters are the ones people forget, because they are aliases
//! for DOS wildcards rather than obvious metacharacters:
//!
//! | Char | Acts as |
//! |------|---------|
//! | `"` (0x22) | `DOS_DOT` |
//! | `<` (0x3C) | `DOS_STAR` |
//! | `>` (0x3E) | `DOS_QM` |
//!
//! # What this deliberately does not try to guarantee
//!
//! Win32 matches the pattern against the 8.3 short name as well as the long
//! name, so the server may return entries whose long name does not contain
//! the query. Those false positives are harmless and free to remove: the
//! caller re-ranks the returned long names locally. The server-side filter is
//! therefore a **superset filter**.
//!
//! False *negatives* are the real hazard, and the reason
//! [`crate::search::verify`] audits every server result against what the
//! local index would have returned, and disables the whole mechanism after a
//! few disagreements.

use crate::config::{MAX_SERVER_QUERY_LEN, MIN_QUERY_LEN};

/// Why a query cannot be pushed to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternReject {
    TooShort,
    TooLong,
    /// Contains an NT expression metacharacter, a path separator, or another
    /// character with no literal interpretation.
    NotLiteral(char),
    /// Non-ASCII. The NLS upcase table used by the server does not agree with
    /// Rust's `to_lowercase` for cases like the Turkish dotless i, so rather
    /// than reason about the divergence the query falls back to local
    /// matching.
    NonAscii,
    /// Nothing but punctuation; would match implausibly broadly.
    NoAlphanumeric,
    /// Windows path canonicalisation strips these before the pattern reaches
    /// the kernel, silently changing the query.
    TrailingSpaceOrDot,
    /// kernel32 rewrites `*.*` to `*`, i.e. "match everything".
    MatchesEverything,
}

impl PatternReject {
    pub fn label(self) -> String {
        match self {
            Self::TooShort => "query too short for a server-side filter".into(),
            Self::TooLong => "query too long for a server-side filter".into(),
            Self::NotLiteral(c) => format!("{c:?} is not a literal character"),
            Self::NonAscii => "non-ASCII query".into(),
            Self::NoAlphanumeric => "query has no letters or digits".into(),
            Self::TrailingSpaceOrDot => "query ends in a space or dot".into(),
            Self::MatchesEverything => "query would match every file".into(),
        }
    }
}

/// Characters accepted in addition to ASCII alphanumerics.
///
/// Chosen because they occur in real filenames and have no meaning to the NT
/// expression evaluator. Note the absence of `*`, `?`, `<`, `>`, `"`, `\`,
/// `/`, `:` and `|`.
const EXTRA_LITERAL: &[char] = &['-', '_', ' ', '.', '(', ')', '&', '+', '#', '\''];

/// True when `c` is safe to embed in a search pattern verbatim.
pub fn is_literal_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || EXTRA_LITERAL.contains(&c)
}

/// Builds `*query*` when the query is provably a literal.
///
/// A `Some` result means the set of names the server matches is exactly the
/// set of names containing `query`, up to 8.3 short-name false positives
/// which the caller filters out.
pub fn wildcard_for(query: &str) -> Result<String, PatternReject> {
    if !query.is_ascii() {
        return Err(PatternReject::NonAscii);
    }
    if query.len() < MIN_QUERY_LEN {
        return Err(PatternReject::TooShort);
    }
    if query.len() > MAX_SERVER_QUERY_LEN {
        return Err(PatternReject::TooLong);
    }
    if let Some(c) = query.chars().find(|&c| !is_literal_char(c)) {
        return Err(PatternReject::NotLiteral(c));
    }
    if !query.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(PatternReject::NoAlphanumeric);
    }
    if query.ends_with(' ') || query.ends_with('.') {
        return Err(PatternReject::TrailingSpaceOrDot);
    }

    let pattern = format!("*{query}*");
    if pattern.eq_ignore_ascii_case("*.*") {
        return Err(PatternReject::MatchesEverything);
    }
    Ok(pattern)
}

/// Whether a name the server returned genuinely contains the query.
///
/// Removes 8.3 short-name false positives. Cheap, because only the matches
/// cross the wire.
pub fn confirms(name: &str, query: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let q = query.to_ascii_lowercase();
    n.contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skips the length check, so the `*.*` special case can be exercised
    /// directly - `.` on its own is rejected as too short first.
    fn wildcard_for_unchecked(query: &str) -> Result<String, PatternReject> {
        let pattern = format!("*{query}*");
        if pattern.eq_ignore_ascii_case("*.*") {
            return Err(PatternReject::MatchesEverything);
        }
        Ok(pattern)
    }

    #[test]
    fn accepts_a_plain_job_code() {
        assert_eq!(wildcard_for("p12345").unwrap(), "*p12345*");
        assert_eq!(wildcard_for("11-D-0704").unwrap(), "*11-D-0704*");
        assert_eq!(wildcard_for("report_a1").unwrap(), "*report_a1*");
    }

    #[test]
    fn accepts_the_punctuation_that_appears_in_real_filenames() {
        for q in [
            "a-b", "a_b", "a b", "a.b", "a(b)", "a&b", "a+b", "a#b", "o'brien1",
        ] {
            assert!(wildcard_for(q).is_ok(), "{q:?} should be accepted");
        }
    }

    /// The three that are easy to miss: ANSI aliases for DOS wildcards.
    #[test]
    fn rejects_the_dos_wildcard_aliases() {
        assert_eq!(wildcard_for("ab\"c"), Err(PatternReject::NotLiteral('"')));
        assert_eq!(wildcard_for("ab<c"), Err(PatternReject::NotLiteral('<')));
        assert_eq!(wildcard_for("ab>c"), Err(PatternReject::NotLiteral('>')));
    }

    #[test]
    fn rejects_the_obvious_wildcards() {
        assert_eq!(wildcard_for("ab*c"), Err(PatternReject::NotLiteral('*')));
        assert_eq!(wildcard_for("ab?c"), Err(PatternReject::NotLiteral('?')));
    }

    #[test]
    fn rejects_path_separators_and_drive_syntax() {
        assert_eq!(wildcard_for("ab\\c"), Err(PatternReject::NotLiteral('\\')));
        assert_eq!(wildcard_for("ab/c"), Err(PatternReject::NotLiteral('/')));
        assert_eq!(wildcard_for("ab:c"), Err(PatternReject::NotLiteral(':')));
        assert_eq!(wildcard_for("ab|c"), Err(PatternReject::NotLiteral('|')));
    }

    #[test]
    fn rejects_non_ascii_rather_than_guessing_at_nls_case_folding() {
        assert_eq!(wildcard_for("Écoles"), Err(PatternReject::NonAscii));
        assert_eq!(wildcard_for("привет"), Err(PatternReject::NonAscii));
    }

    #[test]
    fn rejects_queries_outside_the_length_band() {
        assert_eq!(wildcard_for("ab"), Err(PatternReject::TooShort));
        assert_eq!(
            wildcard_for(&"a".repeat(MAX_SERVER_QUERY_LEN + 1)),
            Err(PatternReject::TooLong)
        );
        assert!(wildcard_for(&"a".repeat(MAX_SERVER_QUERY_LEN)).is_ok());
    }

    /// Path canonicalisation strips these before the kernel sees them, which
    /// would silently change what was searched for.
    #[test]
    fn rejects_a_trailing_space_or_dot() {
        assert_eq!(wildcard_for("abc "), Err(PatternReject::TrailingSpaceOrDot));
        assert_eq!(wildcard_for("abc."), Err(PatternReject::TrailingSpaceOrDot));
        assert!(wildcard_for(" abc").is_ok(), "a leading space is harmless");
    }

    #[test]
    fn rejects_pure_punctuation() {
        assert_eq!(wildcard_for("---"), Err(PatternReject::NoAlphanumeric));
        // Checked before the trailing-space rule, so this is the reason given.
        assert_eq!(wildcard_for("   "), Err(PatternReject::NoAlphanumeric));
    }

    /// kernel32 special-cases this one pattern into "match everything".
    #[test]
    fn rejects_the_pattern_that_kernel32_rewrites() {
        // "." on its own is too short, so construct the collision directly.
        assert_eq!(wildcard_for("."), Err(PatternReject::TooShort));
        assert_eq!(
            wildcard_for_unchecked("."),
            Err(PatternReject::MatchesEverything)
        );
    }

    #[test]
    fn confirms_filters_out_short_name_false_positives() {
        // The server can match on the 8.3 name; the long name is what counts.
        assert!(confirms("Project Alpha Report.docx", "alpha"));
        assert!(confirms("PROJECT.DOCX", "project"));
        assert!(!confirms("Something Else.docx", "alpha"));
    }

    #[test]
    fn confirms_is_case_insensitive_both_ways() {
        assert!(confirms("REPORT_ABC.pdf", "abc"));
        assert!(confirms("report_abc.pdf", "ABC"));
    }

    #[test]
    fn every_accepted_query_produces_a_pattern_with_exactly_two_stars() {
        for q in ["p12345", "11-D-0704", "a b c1", "o'brien1"] {
            let p = wildcard_for(q).unwrap();
            assert_eq!(p.matches('*').count(), 2);
            assert!(p.starts_with('*') && p.ends_with('*'));
            assert_eq!(&p[1..p.len() - 1], q);
        }
    }

    #[test]
    fn the_literal_set_excludes_every_metacharacter() {
        for c in ['*', '?', '<', '>', '"', '\\', '/', ':', '|', '\0'] {
            assert!(!is_literal_char(c), "{c:?} must not be treated as literal");
        }
    }
}
