//! The recall panel.
//!
//! Shown in place of the results while the Up arrow is walking back through
//! previous codes. Deliberately not an overlay: this program has no modal or
//! z-order concept, and inventing one for a list would mean a `Clear` widget,
//! a floating rect, and a second set of rules about where the caret goes.
//! Swapping what the results pane draws costs none of that, and the results
//! underneath are stale during recall anyway - they belong to the code that
//! was being typed before the arrow was pressed.

use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::theme;
use crate::history::History;

/// One row per remembered code, newest first.
pub fn rows(history: &History) -> Vec<ListItem<'static>> {
    history
        .entries()
        .iter()
        .map(|entry| ListItem::new(Line::from(vec![Span::raw("  "), Span::raw(entry.clone())])))
        .collect()
}

/// Title for the panel, carrying the position so the list does not feel
/// bottomless while stepping through it.
pub fn title(history: &History) -> String {
    match history.cursor() {
        Some(at) => format!(" History ({} of {}) ", at + 1, history.len()),
        None => format!(" History ({}) ", history.len()),
    }
}

/// Shown when nothing has been searched for yet.
pub fn empty_message() -> &'static str {
    "  No previous codes yet."
}

/// The hint that replaces the usual help line during recall.
pub fn help_line() -> &'static str {
    "Up/Down browse · Enter use this code · Esc keep what you were typing"
}

/// Style for the highlighted entry. Shared with the results list on purpose:
/// one "this is the row you are on" appearance across the program.
pub fn highlight() -> ratatui::style::Style {
    theme::selection()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[&str]) -> History {
        History::from_entries(entries.iter().copied())
    }

    #[test]
    fn every_remembered_code_gets_a_row() {
        let h = history(&["c", "b", "a"]);
        assert_eq!(rows(&h).len(), 3);
    }

    #[test]
    fn the_title_reports_the_position_while_browsing() {
        let mut h = history(&["c", "b", "a"]);
        assert_eq!(title(&h), " History (3) ");
        h.begin("draft");
        assert_eq!(title(&h), " History (1 of 3) ");
        h.older();
        assert_eq!(title(&h), " History (2 of 3) ");
    }

    #[test]
    fn an_empty_history_still_has_something_to_say() {
        assert!(!empty_message().trim().is_empty());
        assert!(rows(&History::new()).is_empty());
    }

    #[test]
    fn the_recall_help_names_the_way_out() {
        // Esc restoring the draft is the non-obvious part, so it must be on
        // screen rather than discoverable only by losing a half-typed code.
        assert!(help_line().contains("Esc"));
        assert!(help_line().contains("Enter"));
    }
}
