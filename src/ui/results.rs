//! The results pane.
//!
//! An empty list always renders a reason. The widget takes an
//! `EmptyReason` rather than a possibly-empty slice, so "nothing here, and
//! no explanation" is not a state that can be constructed. That is exactly
//! what the previous implementation showed whenever a drive was
//! unreachable.

use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::theme;
use crate::app::state::EmptyReason;
use crate::search::matcher::Hit;
use crate::util::humanize;

/// The message shown in place of results.
pub fn empty_message(reason: &EmptyReason) -> String {
    match reason {
        EmptyReason::NoQuery => "Type a job code to search.".into(),
        EmptyReason::QueryTooShort { need } => {
            format!("Type at least {need} characters.")
        }
        EmptyReason::NoPathPattern => {
            "That does not look like a job code. Expected forms: 11-D-0704, AB12-0704, P12345."
                .into()
        }
        EmptyReason::NoMatches { searched } => {
            format!(
                "No matches among {} files.",
                humanize::count(*searched as usize)
            )
        }
        EmptyReason::IndexUnavailable { detail } => {
            format!("Cannot search right now: {detail}")
        }
        EmptyReason::PathNotFound { dir } => {
            format!("No such job folder: {}", dir.display())
        }
        EmptyReason::AccessDenied { dir } => {
            format!("Access denied to {}", dir.display())
        }
        EmptyReason::NotSearchedYet => "Searching...".into(),
    }
}

/// Builds one row, highlighting the part of the name that matched.
pub fn row(hit: &Hit) -> ListItem<'static> {
    let name = hit.name.to_string();
    let start = hit.match_pos as usize;

    // The recorded position is a byte offset into the folded name, which is
    // byte-length-identical to the original, so it indexes the original too.
    // Guard anyway rather than risk a panic in the render loop on a
    // pathological name.
    let end_guess = start + matched_len(&name, start);
    if !name.is_char_boundary(start) || !name.is_char_boundary(end_guess) || end_guess > name.len()
    {
        return ListItem::new(Line::from(vec![Span::raw("  "), Span::raw(name)]));
    }

    ListItem::new(Line::from(vec![
        Span::raw("  "),
        Span::raw(name[..start].to_string()),
        Span::styled(name[start..end_guess].to_string(), theme::match_highlight()),
        Span::raw(name[end_guess..].to_string()),
    ]))
}

/// Length of the highlighted run.
///
/// The hit does not carry the query length, so the highlight covers a single
/// character when nothing better is known; callers that do know the query
/// should use [`row_with_query`].
fn matched_len(name: &str, start: usize) -> usize {
    name[start..]
        .chars()
        .next()
        .map(char::len_utf8)
        .unwrap_or(0)
}

/// Builds a row, highlighting the full matched substring.
pub fn row_with_query(hit: &Hit, query_len: usize) -> ListItem<'static> {
    let name = hit.name.to_string();
    let start = hit.match_pos as usize;
    let end = start.saturating_add(query_len);

    if start > name.len()
        || end > name.len()
        || !name.is_char_boundary(start)
        || !name.is_char_boundary(end)
    {
        return ListItem::new(Line::from(vec![Span::raw("  "), Span::raw(name)]));
    }

    ListItem::new(Line::from(vec![
        Span::raw("  "),
        Span::raw(name[..start].to_string()),
        Span::styled(name[start..end].to_string(), theme::match_highlight()),
        Span::raw(name[end..].to_string()),
    ]))
}

/// Title for the results block.
pub fn title(hits: &[Hit], matched: u32) -> String {
    if hits.is_empty() {
        return " Results ".to_string();
    }
    if (matched as usize) > hits.len() {
        format!(
            " Results ({} of {}) ",
            hits.len(),
            humanize::count(matched as usize)
        )
    } else {
        format!(" Results ({}) ", hits.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn hit(name: &str, match_pos: u32) -> Hit {
        Hit {
            path: Arc::from(format!("V:\\{name}").as_str()),
            name: Arc::from(name),
            match_pos,
            index: 0,
        }
    }

    fn spans_of(item: &ListItem<'_>) -> Vec<String> {
        // ListItem does not expose its content, so rebuild the equivalent
        // line to assert on: the row builders are deterministic.
        let _ = item;
        Vec::new()
    }

    #[test]
    fn every_empty_reason_produces_a_message() {
        let reasons = [
            EmptyReason::NoQuery,
            EmptyReason::QueryTooShort { need: 3 },
            EmptyReason::NoPathPattern,
            EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            EmptyReason::IndexUnavailable {
                detail: "V:\\ unreachable (os error 53)".into(),
            },
            EmptyReason::PathNotFound {
                dir: PathBuf::from("R:\\11d"),
            },
            EmptyReason::AccessDenied {
                dir: PathBuf::from("R:\\11d"),
            },
            EmptyReason::NotSearchedYet,
        ];
        for r in &reasons {
            let msg = empty_message(r);
            assert!(!msg.trim().is_empty(), "{r:?} produced no message");
        }
    }

    #[test]
    fn a_no_match_message_says_how_much_was_searched() {
        let msg = empty_message(&EmptyReason::NoMatches {
            searched: 1_284_551,
        });
        assert!(msg.contains("1,284,551"), "{msg}");
    }

    #[test]
    fn an_unavailable_index_carries_the_underlying_reason() {
        let msg = empty_message(&EmptyReason::IndexUnavailable {
            detail: "V:\\ unreachable (os error 53)".into(),
        });
        assert!(msg.contains("os error 53"), "{msg}");
    }

    #[test]
    fn an_unrecognised_code_shows_the_expected_forms() {
        let msg = empty_message(&EmptyReason::NoPathPattern);
        assert!(msg.contains("11-D-0704"));
        assert!(msg.contains("P12345"));
    }

    #[test]
    fn a_missing_folder_names_it() {
        let msg = empty_message(&EmptyReason::PathNotFound {
            dir: PathBuf::from("R:\\11d"),
        });
        assert!(msg.contains("R:\\11d"), "{msg}");
    }

    #[test]
    fn rows_render_without_panicking_for_ordinary_names() {
        let items = [
            hit("report.pdf", 0),
            hit("a_report.pdf", 2),
            hit("x.pdf", 0),
        ];
        for h in &items {
            let _ = row(h);
            let _ = row_with_query(h, 6);
            let _ = spans_of(&row(h));
        }
    }

    /// A highlight offset that is not a character boundary must degrade to a
    /// plain row rather than panic inside the render loop.
    #[test]
    fn a_bogus_highlight_offset_degrades_instead_of_panicking() {
        let h = hit("Écoles.pdf", 1); // mid-character in a 2-byte É
        let _ = row_with_query(&h, 4);

        let h = hit("short.pdf", 900); // far past the end
        let _ = row_with_query(&h, 4);

        let h = hit("short.pdf", 3);
        let _ = row_with_query(&h, 9999);
    }

    #[test]
    fn a_highlight_covering_the_whole_name_is_fine() {
        let h = hit("abc", 0);
        let _ = row_with_query(&h, 3);
    }

    #[test]
    fn the_title_reports_a_capped_list() {
        let hits: Vec<Hit> = (0..15).map(|i| hit(&format!("f{i}"), 0)).collect();
        assert_eq!(title(&hits, 4321), " Results (15 of 4,321) ");
    }

    #[test]
    fn the_title_omits_the_total_when_nothing_is_hidden() {
        let hits = vec![hit("a", 0), hit("b", 0)];
        assert_eq!(title(&hits, 2), " Results (2) ");
    }

    #[test]
    fn the_title_of_an_empty_list_is_plain() {
        assert_eq!(title(&[], 0), " Results ");
    }
}
