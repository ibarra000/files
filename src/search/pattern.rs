//! Decides when a query can be evaluated by the file server.
//!
//! `FindFirstFileExW("V:\*p12345*")` evaluates the pattern on the server
//! (MS-SMB2 2.2.33 `QUERY_DIRECTORY` carries a `FileName` field), so a cold
//! search costs one round trip regardless of directory size or link bandwidth.
//! This is only equivalent to the local substring match when the query has
//! nothing the NT expression evaluator treats specially.
//!
//! # A whitelist, not escaping
//!
//! NT expressions have no escape mechanism (`\` cannot escape `*`), so queries
//! are accepted only when provably literal; everything else falls back to the
//! local index. Three ANSI characters are aliases for DOS wildcards:
//!
//! | Char | Acts as |
//! |------|---------|
//! | `"` (0x22) | `DOS_DOT` |
//! | `<` (0x3C) | `DOS_STAR` |
//! | `>` (0x3E) | `DOS_QM` |
//!
//! # Superset, not exact
//!
//! Win32 also matches the 8.3 short name, so the server may return names that
//! do not contain the query. These false positives are removed locally by
//! [`confirms`]. False negatives are the real hazard, which is why
//! [`crate::search::verify`] audits server results against the local index and
//! disables the mechanism after a few disagreements.

use crate::config::{MAX_SERVER_QUERY_LEN, MIN_SERVER_QUERY_LEN};
use crate::search::query::{self, MatchMode, Query};
use crate::util::fold;

/// Why a query cannot be pushed to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternReject {
    TooShort,
    TooLong,
    /// Contains an NT expression metacharacter, a path separator, or another
    /// character with no literal interpretation.
    NotLiteral(char),
    /// Non-ASCII. The server's NLS upcase table disagrees with Rust's
    /// `to_lowercase` in cases like the Turkish dotless i.
    NonAscii,
    /// Only punctuation; would match implausibly broadly.
    NoAlphanumeric,
    /// Ends in a space or dot, which Windows path canonicalisation strips before
    /// the pattern reaches the kernel.
    TrailingSpaceOrDot,
    /// kernel32 rewrites `*.*` to `*`, which matches everything.
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
/// They occur in real filenames and have no meaning to the NT expression
/// evaluator. `*`, `?`, `<`, `>`, `"`, `\`, `/`, `:` and `|` are excluded.
const EXTRA_LITERAL: &[char] = &['-', '_', ' ', '.', '(', ')', '&', '+', '#', '\''];

/// Returns true when `c` is safe to embed in a search pattern verbatim.
pub fn is_literal_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || EXTRA_LITERAL.contains(&c)
}

/// Builds the pattern to send to the server, when the query is provably literal.
///
/// Each mode emits the tightest pattern that is still a provable superset of
/// the local answer, and [`confirms`] narrows it back locally. A suffix match
/// cannot use `*term.*`: NT `*` matches any run including dots, so that
/// pattern requires a dot and misses a file named plainly `term`, a false
/// negative.
///
/// | mode | pattern | why it is a superset |
/// |------|---------|----------------------|
/// | contains | `*term*` | exact |
/// | prefix | `term*` | exact: a name starts with the term iff it matches |
/// | suffix | `*term*` | a stem ending in the term contains it |
///
/// A single extension is appended because it is exactly expressible. Several
/// are not: NT expressions have no alternation and `FindFirstFileExW` takes
/// one pattern.
pub fn wildcard_for(query: &Query) -> Result<String, PatternReject> {
    let term = query.term();
    let body = literal_body(term)?;

    // One extension is exact; several are left to the local narrowing.
    let ext = match query.types().iter().next() {
        Some(e) if query.types().len() == 1 => format!(".{}", e.as_str()),
        _ => String::new(),
    };

    let pattern = match query.mode() {
        MatchMode::Prefix => format!("{body}*{ext}"),
        // Loose on purpose; see the doc comment above.
        MatchMode::Contains | MatchMode::Suffix => format!("*{body}*{ext}"),
    };
    if pattern.eq_ignore_ascii_case("*.*") {
        return Err(PatternReject::MatchesEverything);
    }
    Ok(pattern)
}

/// The same pattern, for an enumeration that returns folders as well as files.
///
/// The extension is not appended: a folder has no extension, so
/// `*p12345*.pdf` cannot match a folder named `p12345`, and on a share a
/// folder is the common hit. [`confirms`] applies the type filter locally;
/// [`wildcard_for`] keeps the extension for the files-only enumeration in
/// [`crate::search::verify`].
pub fn wildcard_for_entries(query: &Query) -> Result<String, PatternReject> {
    let body = literal_body(query.term())?;
    let pattern = match query.mode() {
        MatchMode::Prefix => format!("{body}*"),
        MatchMode::Contains | MatchMode::Suffix => format!("*{body}*"),
    };
    if pattern.eq_ignore_ascii_case("*.*") {
        return Err(PatternReject::MatchesEverything);
    }
    Ok(pattern)
}

/// Returns the term once it is proven safe to embed in a pattern verbatim.
fn literal_body(query: &str) -> Result<&str, PatternReject> {
    if !query.is_ascii() {
        return Err(PatternReject::NonAscii);
    }
    // The server's floor, independent of the panel's. A very short filter
    // matches most of a share, so the round trip saves nothing.
    if query.len() < MIN_SERVER_QUERY_LEN {
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

    Ok(query)
}

/// Returns whether a name the server returned is one the local index would
/// return.
///
/// Removes 8.3 short-name false positives and narrows the loose patterns from
/// [`wildcard_for`] (trailing match, several extensions) back to the query.
/// Required because `VerifyOutcome::Server` replaces the list on screen.
/// Delegates to the matcher's own predicate so both agree by construction.
pub fn confirms(name: &str, query: &Query) -> bool {
    let folded = fold::fold_query(name);
    let needle = fold::fold_query(query.term());
    let Some(pos) = memchr::memmem::find(&folded, &needle) else {
        return false;
    };
    query::admits(&folded, &needle, pos as u32, query.mode(), query.types()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skips the length check so the `*.*` case can be exercised directly;
    /// `.` alone is rejected as too short first.
    fn wildcard_for_unchecked(query: &str) -> Result<String, PatternReject> {
        let pattern = format!("*{query}*");
        if pattern.eq_ignore_ascii_case("*.*") {
            return Err(PatternReject::MatchesEverything);
        }
        Ok(pattern)
    }

    #[test]
    fn accepts_a_plain_job_code() {
        assert_eq!(
            wildcard_for(&Query::contains("p12345")).unwrap(),
            "*p12345*"
        );
        assert_eq!(
            wildcard_for(&Query::contains("11-D-0704")).unwrap(),
            "*11-D-0704*"
        );
        assert_eq!(
            wildcard_for(&Query::contains("report_a1")).unwrap(),
            "*report_a1*"
        );
    }

    #[test]
    fn accepts_the_punctuation_that_appears_in_real_filenames() {
        for q in [
            "a-b", "a_b", "a b", "a.b", "a(b)", "a&b", "a+b", "a#b", "o'brien1",
        ] {
            assert!(
                wildcard_for(&Query::contains(q)).is_ok(),
                "{q:?} should be accepted"
            );
        }
    }

    /// ANSI aliases for DOS wildcards.
    #[test]
    fn rejects_the_dos_wildcard_aliases() {
        assert_eq!(
            wildcard_for(&Query::contains("ab\"c")),
            Err(PatternReject::NotLiteral('"'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab<c")),
            Err(PatternReject::NotLiteral('<'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab>c")),
            Err(PatternReject::NotLiteral('>'))
        );
    }

    #[test]
    fn rejects_the_obvious_wildcards() {
        assert_eq!(
            wildcard_for(&Query::contains("ab*c")),
            Err(PatternReject::NotLiteral('*'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab?c")),
            Err(PatternReject::NotLiteral('?'))
        );
    }

    #[test]
    fn rejects_path_separators_and_drive_syntax() {
        assert_eq!(
            wildcard_for(&Query::contains("ab\\c")),
            Err(PatternReject::NotLiteral('\\'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab/c")),
            Err(PatternReject::NotLiteral('/'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab:c")),
            Err(PatternReject::NotLiteral(':'))
        );
        assert_eq!(
            wildcard_for(&Query::contains("ab|c")),
            Err(PatternReject::NotLiteral('|'))
        );
    }

    #[test]
    fn rejects_non_ascii_rather_than_guessing_at_nls_case_folding() {
        assert_eq!(
            wildcard_for(&Query::contains("Écoles")),
            Err(PatternReject::NonAscii)
        );
        assert_eq!(
            wildcard_for(&Query::contains("привет")),
            Err(PatternReject::NonAscii)
        );
    }

    #[test]
    fn rejects_queries_outside_the_length_band() {
        assert_eq!(
            wildcard_for(&Query::contains("ab")),
            Err(PatternReject::TooShort)
        );
        assert_eq!(
            wildcard_for(&Query::contains("a".repeat(MAX_SERVER_QUERY_LEN + 1))),
            Err(PatternReject::TooLong)
        );
        assert!(wildcard_for(&Query::contains("a".repeat(MAX_SERVER_QUERY_LEN))).is_ok());
    }

    /// Path canonicalisation strips these before the kernel sees them.
    #[test]
    fn rejects_a_trailing_space_or_dot() {
        assert_eq!(
            wildcard_for(&Query::contains("abc ")),
            Err(PatternReject::TrailingSpaceOrDot)
        );
        assert_eq!(
            wildcard_for(&Query::contains("abc.")),
            Err(PatternReject::TrailingSpaceOrDot)
        );
        assert!(
            wildcard_for(&Query::contains(" abc")).is_ok(),
            "a leading space is harmless"
        );
    }

    #[test]
    fn rejects_pure_punctuation() {
        assert_eq!(
            wildcard_for(&Query::contains("---")),
            Err(PatternReject::NoAlphanumeric)
        );
        // Checked before the trailing-space rule, so this is the reason given.
        assert_eq!(
            wildcard_for(&Query::contains("   ")),
            Err(PatternReject::NoAlphanumeric)
        );
    }

    /// kernel32 special-cases this pattern into "match everything".
    #[test]
    fn rejects_the_pattern_that_kernel32_rewrites() {
        // "." alone is too short, so construct the collision directly.
        assert_eq!(
            wildcard_for(&Query::contains(".")),
            Err(PatternReject::TooShort)
        );
        assert_eq!(
            wildcard_for_unchecked("."),
            Err(PatternReject::MatchesEverything)
        );
    }

    // --- narrowed queries ---------------------------------------------------

    fn wc(line: &str) -> String {
        wildcard_for(&Query::parse(line)).unwrap()
    }

    #[test]
    fn a_leading_anchor_is_pushed_down_as_a_prefix_pattern() {
        assert_eq!(wc("p12345*"), "p12345*");
    }

    /// `*p12345.*` is unsound: NT `*` matches dots, so it requires a dot and
    /// misses a file named plainly `p12345`.
    #[test]
    fn a_trailing_anchor_is_pushed_down_loosely_because_a_dotless_name_has_no_dot() {
        assert_eq!(wc("*p12345"), "*p12345*");
    }

    #[test]
    fn one_extension_is_pushed_down_and_several_are_not() {
        assert_eq!(wc("p12345 ext:pdf"), "*p12345*.pdf");
        assert_eq!(wc("p12345* ext:pdf"), "p12345*.pdf");
        // No alternation in an NT expression, and one pattern per call.
        assert_eq!(wc("p12345 ext:dwg,pdf"), "*p12345*");
    }

    /// A folder has no extension, so a pattern for folders must not carry one.
    #[test]
    fn a_pattern_that_must_match_folders_never_carries_the_extension() {
        let q = Query::parse("p12345 ext:pdf");
        assert_eq!(wildcard_for(&q).unwrap(), "*p12345*.pdf");
        assert_eq!(wildcard_for_entries(&q).unwrap(), "*p12345*");
        assert!(glob(&wildcard_for_entries(&q).unwrap(), "p12345"));
        assert!(!glob(&wildcard_for(&q).unwrap(), "p12345"));
    }

    #[test]
    fn confirms_narrows_the_loose_pattern_back_to_what_was_asked_for() {
        let trailing = Query::parse("*p12345");
        assert!(confirms("job-p12345.pdf", &trailing));
        assert!(
            !confirms("p12345-job.pdf", &trailing),
            "not a trailing match"
        );

        let several = Query::parse("p12345 ext:dwg,pdf");
        assert!(confirms("p12345.dwg", &several));
        assert!(!confirms("p12345.txt", &several), "not an asked-for type");
    }

    /// Whatever pattern is emitted, the server's answer is a superset of the
    /// local one. Checked against a glob evaluator.
    #[test]
    fn every_pattern_emitted_is_a_superset_of_what_the_index_would_return() {
        let names = [
            "p12345",
            "p12345.pdf",
            "p12345.dwg",
            "job-p12345.pdf",
            "p12345-job.pdf",
            "p12345.rev2.pdf",
            "unrelated.pdf",
        ];
        for line in [
            "p12345",
            "p12345*",
            "*p12345",
            "p12345 ext:pdf",
            "p12345* ext:pdf",
            "*p12345 ext:pdf",
            "p12345 ext:dwg,pdf",
        ] {
            let q = Query::parse(line);
            let pattern = wildcard_for(&q).unwrap();
            for name in names {
                let local = confirms(name, &q);
                let server = glob(&pattern, name);
                assert!(
                    !local || server,
                    "{line:?}: the index keeps {name:?} and the pattern {pattern:?} drops it"
                );
            }
        }
    }

    /// Two-cursor glob with backtracking on the last star. Only `*` and literals
    /// are emitted, so nothing else is handled.
    fn glob(pattern: &str, name: &str) -> bool {
        let p: Vec<char> = pattern.to_lowercase().chars().collect();
        let n: Vec<char> = name.to_lowercase().chars().collect();
        let (mut pi, mut ni) = (0usize, 0usize);
        let (mut star, mut resume) = (None, 0usize);
        while ni < n.len() {
            if pi < p.len() && p[pi] == n[ni] {
                pi += 1;
                ni += 1;
            } else if pi < p.len() && p[pi] == '*' {
                star = Some(pi);
                resume = ni;
                pi += 1;
            } else if let Some(s) = star {
                pi = s + 1;
                resume += 1;
                ni = resume;
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == '*' {
            pi += 1;
        }
        pi == p.len()
    }

    #[test]
    fn confirms_filters_out_short_name_false_positives() {
        // The server can match on the 8.3 name; the long name is what counts.
        assert!(confirms(
            "Project Alpha Report.docx",
            &Query::contains("alpha")
        ));
        assert!(confirms("PROJECT.DOCX", &Query::contains("project")));
        assert!(!confirms("Something Else.docx", &Query::contains("alpha")));
    }

    #[test]
    fn confirms_is_case_insensitive_both_ways() {
        assert!(confirms("REPORT_ABC.pdf", &Query::contains("abc")));
        assert!(confirms("report_abc.pdf", &Query::contains("ABC")));
    }

    #[test]
    fn every_accepted_query_produces_a_pattern_with_exactly_two_stars() {
        for q in ["p12345", "11-D-0704", "a b c1", "o'brien1"] {
            let p = wildcard_for(&Query::contains(q)).unwrap();
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
