//! The single line the results pane shows when it has no results.
//!
//! The line distinguishes a search that found nothing from one that could not
//! be made. Diagnostics are shown verbatim; full detail is on the Diagnostics
//! page. Indentation and centring are left to the renderer.

use crate::app::state::EmptyReason;
use crate::view::status::Tone;
use crate::view::{Block, Run};

/// Returns a block of plain body text.
fn say(text: impl Into<String>) -> Block {
    vec![Run::body(text.into())]
}

/// Returns a dimmed secondary clause.
fn aside(text: impl Into<String>) -> Run {
    Run::dim(text.into())
}

/// Returns a diagnostic run, shown verbatim.
fn detail(text: impl Into<String>) -> Run {
    Run::toned(text.into(), Tone::Bad)
}

/// Returns the empty-state line, or `None` when nothing should be shown.
pub fn view(reason: &EmptyReason, query: &str) -> Option<Block> {
    /// Returns `Nothing matched "x"`, or `Nothing matched` for an empty query.
    fn nothing_matched(query: &str) -> String {
        if query.is_empty() {
            "Nothing matched".to_owned()
        } else {
            format!("Nothing matched \u{201c}{query}\u{201d}")
        }
    }

    match reason {
        // The panel has no body before anything is typed.
        EmptyReason::NoQuery => None,

        // The share answered incompletely; retrying may find more.
        EmptyReason::LiveIncomplete { name, .. } => Some(vec![
            Run::body(nothing_matched(query)),
            aside(format!(" on {name} yet \u{b7} F5 to retry")),
        ]),

        EmptyReason::LiveUnavailable { name, detail: why } => Some(vec![
            Run::body(format!("{name} could not be searched \u{b7} ")),
            detail(crate::view::sentence(why)),
        ]),

        EmptyReason::BadQuery { detail: why } => Some(vec![
            Run::body("Invalid search \u{b7} "),
            aside(crate::view::sentence(why)),
        ]),

        EmptyReason::NoSharesConfigured => Some(vec![
            Run::body("No drives set up \u{b7} "),
            Run::accent("Ctrl+,"),
            aside(" to add one"),
        ]),

        EmptyReason::NoMatches { .. } => Some(say(nothing_matched(query))),

        EmptyReason::IndexUnavailable { detail: what } => Some(vec![
            Run::body("Cannot search right now \u{b7} "),
            detail(what.clone()),
        ]),

        EmptyReason::PathNotFound { dir } => Some(vec![
            Run::body("No folder for that search \u{b7} "),
            aside(format!("looked in {}", dir.display())),
        ]),

        EmptyReason::AccessDenied { dir } => Some(vec![
            Run::body("No permission to read "),
            aside(format!("{}", dir.display())),
        ]),

        // The only on-screen indication that a search is in flight.
        EmptyReason::NotSearchedYet => Some(vec![Run::toned("Searching\u{2026}", Tone::Busy)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn text(reason: &EmptyReason, query: &str) -> String {
        view(reason, query)
            .as_ref()
            .map(crate::view::plain)
            .unwrap_or_default()
    }

    fn every_reason() -> Vec<EmptyReason> {
        vec![
            EmptyReason::NoQuery,
            EmptyReason::NoSharesConfigured,
            EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            EmptyReason::IndexUnavailable {
                detail: "V:\\shared\\files unreachable (os error 53)".into(),
            },
            EmptyReason::PathNotFound {
                dir: PathBuf::from("R:\\abc"),
            },
            EmptyReason::AccessDenied {
                dir: PathBuf::from("R:\\abc"),
            },
            EmptyReason::NotSearchedYet,
        ]
    }

    #[test]
    fn every_block_keeps_the_house_style() {
        // Check the joined block: runs carry the spaces between them, so a
        // single run may legitimately end in a space.
        let mut lines = Vec::new();
        for reason in every_reason() {
            for query in ["", "AB-C-0704"] {
                if let Some(block) = view(&reason, query) {
                    lines.push(crate::view::plain(&block));
                }
            }
        }
        crate::view::style::check_all(
            "the empty-state blocks",
            lines.iter().map(String::as_str),
            crate::view::style::Slot::Body,
        );
    }

    /// `NoQuery` is exempt because an untouched panel draws no pane.
    #[test]
    fn every_reason_says_something() {
        for reason in every_reason() {
            if reason == EmptyReason::NoQuery {
                continue;
            }
            let t = text(&reason, "AB-C-0704");
            assert!(
                t.lines().any(|l| l.trim().len() > 8),
                "{reason:?} says nothing worth reading:\n{t}"
            );
        }
    }

    #[test]
    fn the_first_screen_says_nothing_at_all() {
        assert!(
            view(&EmptyReason::NoQuery, "").is_none(),
            "the untouched panel drew a body"
        );
    }

    #[test]
    fn a_no_match_names_the_search_and_says_nothing_else() {
        let t = text(
            &EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            "AB-C-0704",
        );
        assert!(t.contains("AB-C-0704"), "{t}");
        assert!(!t.contains("1,284,551"), "{t}");
        assert!(!t.contains('\n'), "more than one line:\n{t}");
    }

    #[test]
    fn a_failure_keeps_the_diagnostic_that_makes_it_solvable() {
        let t = text(
            &EmptyReason::IndexUnavailable {
                detail: "V:\\shared\\files unreachable (os error 53)".into(),
            },
            "AB-C-0704",
        );
        assert!(t.contains("unreachable"), "{t}");
        assert!(t.contains("os error 53"), "{t}");
    }

    #[test]
    fn an_unconfigured_install_names_the_key_that_fixes_it() {
        let t = text(&EmptyReason::NoSharesConfigured, "");
        assert!(t.contains("Ctrl+,"), "{t}");
    }

    #[test]
    fn every_message_is_one_line() {
        for reason in every_reason() {
            for query in ["", "AB-C-0704"] {
                let Some(block) = view(&reason, query) else {
                    continue;
                };
                let line = crate::view::plain(&block);
                assert!(!line.contains('\n'), "{reason:?} is more than a line");
                // Fits a 600 pt panel at 14 pt text.
                assert!(line.chars().count() < 90, "{reason:?}: {line:?}");
            }
        }
    }
}
