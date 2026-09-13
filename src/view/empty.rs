//! What the results pane says when it has no results.
//!
//! This is the first screen anybody sees, and for somebody whose code found
//! nothing it is the only screen that can say what to do next. It gets the
//! whole pane rather than the top corner of it: a sixteen-row box holding one
//! grey sentence reads as a program that has broken.
//!
//! Every variant has the same three-part shape - what happened, a fact about
//! it, then the thing to do - so the eye learns where to look once and then
//! knows. The diagnostics that make a support call solvable are kept verbatim
//! and given their own line; the sentence around them is the part rewritten
//! for somebody who does not use a terminal by choice.
//!
//! Indentation and centring are deliberately *not* here. Where a block sits is
//! layout, and the two renderers place it differently - but what it says, and
//! the blank rows that give it its shape, are the same in both.

use crate::app::state::EmptyReason;
use crate::config::MIN_QUERY_LEN;
use crate::util::humanize;
use crate::view::status::Tone;
use crate::view::{Block, Run};

/// Plain body text: the headline of each block.
fn say(text: impl Into<String>) -> Block {
    vec![Run::body(text.into())]
}

/// A secondary line: a fact, or what to do about it.
fn aside(text: impl Into<String>) -> Block {
    vec![Run::dim(text.into())]
}

/// A diagnostic, kept verbatim because it is what makes a support call
/// solvable.
fn detail(text: impl Into<String>) -> Block {
    vec![Run::toned(text.into(), Tone::Bad)]
}

fn blank() -> Block {
    vec![Run::blank()]
}

/// The message, as blocks.
pub fn view(reason: &EmptyReason, query: &str) -> Vec<Block> {
    match reason {
        EmptyReason::NoQuery => vec![
            say("Type a job code to see its files."),
            blank(),
            vec![Run::dim("For example:   "), Run::accent("11-D-0704")],
            blank(),
            aside("Press \u{2191} for codes you used before, or F1 for every key."),
        ],

        EmptyReason::QueryTooShort { .. } => vec![
            say("Keep typing."),
            blank(),
            aside(format!(
                "A job code needs at least {MIN_QUERY_LEN} characters."
            )),
        ],

        EmptyReason::NoSharesConfigured => vec![
            say("No file shares are set up yet."),
            blank(),
            // The command is kept exactly as it is typed: it is the next thing
            // whoever installed this has to run.
            aside("Run  files --check-config  in a command prompt to see why."),
        ],

        EmptyReason::NoMatches { searched } => vec![
            say(if query.is_empty() {
                "Nothing matched.".to_string()
            } else {
                format!("Nothing matched \"{query}\".")
            }),
            blank(),
            aside(format!(
                "Searched {} files.",
                humanize::count(*searched as usize)
            )),
            aside("Check the code, or press F5 to look again."),
        ],

        EmptyReason::IndexUnavailable { detail: what } => vec![
            say("Cannot search right now."),
            blank(),
            detail(what.clone()),
            blank(),
            aside("Press F5 to try again."),
        ],

        EmptyReason::PathNotFound { dir } => vec![
            say("No folder for that job code."),
            blank(),
            aside(format!("Looked in {}", dir.display())),
        ],

        EmptyReason::AccessDenied { dir } => vec![
            say("You do not have permission to read that folder."),
            blank(),
            aside(format!("{}", dir.display())),
            blank(),
            aside("Ask IT for access to it."),
        ],

        EmptyReason::NotSearchedYet => {
            vec![vec![Run::toned("Searching\u{2026}", Tone::Busy)]]
        }
    }
}

/// Height of the block, for the centring above and for tests.
pub fn height(reason: &EmptyReason, query: &str) -> u16 {
    view(reason, query).len() as u16
}

/// Whether the pane has room to say anything at all.
/// Whether a pane this many rows tall has room to say it.
///
/// In rows rather than a rectangle, because it is not really about a
/// terminal: it is a cap on how verbose a message may become, and every one
/// of these is read by somebody whose search has just failed.
pub fn fits(reason: &EmptyReason, query: &str, rows: u16) -> bool {
    rows >= height(reason, query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn text(reason: &EmptyReason, query: &str) -> String {
        view(reason, query)
            .iter()
            .map(crate::view::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn every_reason() -> Vec<EmptyReason> {
        vec![
            EmptyReason::NoQuery,
            EmptyReason::QueryTooShort { need: 3 },
            EmptyReason::NoSharesConfigured,
            EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            EmptyReason::IndexUnavailable {
                detail: "V:\\Documents\\custpro unreachable (os error 53)".into(),
            },
            EmptyReason::PathNotFound {
                dir: PathBuf::from("R:\\11d"),
            },
            EmptyReason::AccessDenied {
                dir: PathBuf::from("R:\\11d"),
            },
            EmptyReason::NotSearchedYet,
        ]
    }

    /// A blank pane with no explanation is the state this whole type exists to
    /// make unreachable.
    #[test]
    fn every_reason_says_something() {
        for reason in every_reason() {
            let t = text(&reason, "11-D-0704");
            assert!(
                t.lines().any(|l| l.trim().len() > 8),
                "{reason:?} says nothing worth reading:\n{t}"
            );
        }
    }

    /// The screen the office sees every morning has to teach three things:
    /// what to type, what one looks like, and that there is more to find.
    #[test]
    fn the_first_screen_shows_what_to_type_and_an_example_of_it() {
        let t = text(&EmptyReason::NoQuery, "");
        assert!(t.contains("Type a job code"), "{t}");
        assert!(t.contains("11-D-0704"), "the example is missing:\n{t}");
        assert!(
            t.contains("F1"),
            "nothing points at the rest of the keys:\n{t}"
        );
    }

    /// A no-match names the code that did not match. "No matches" alone leaves
    /// somebody who has typed two codes in a row unsure which one this is
    /// about.
    #[test]
    fn a_no_match_names_the_code_and_says_how_much_was_searched() {
        let t = text(
            &EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            "11-D-0704",
        );
        assert!(t.contains("11-D-0704"), "{t}");
        assert!(t.contains("1,284,551"), "{t}");
        assert!(t.contains("F5"), "nothing says what to try:\n{t}");
    }

    /// The diagnostics stay verbatim. `os error 53` is what turns a support
    /// call into a fix, and rewording it into something friendlier would be
    /// throwing that away to sound nicer.
    #[test]
    fn a_failure_keeps_the_diagnostic_that_makes_it_solvable() {
        let t = text(
            &EmptyReason::IndexUnavailable {
                detail: "V:\\Documents\\custpro unreachable (os error 53)".into(),
            },
            "11-D-0704",
        );
        assert!(t.contains("unreachable"), "{t}");
        assert!(t.contains("os error 53"), "{t}");
        assert!(t.contains("F5"), "{t}");
    }

    /// Whoever installed this needs the command, exactly as it is typed.
    #[test]
    fn an_unconfigured_install_names_the_command_that_explains_it() {
        let t = text(&EmptyReason::NoSharesConfigured, "");
        assert!(t.contains("files --check-config"), "{t}");
    }

    /// Every variant is short enough to fit the pane an eighty-by-twenty-four
    /// terminal gives it, which is the smallest anybody actually runs.
    #[test]
    fn every_message_fits_the_smallest_terminal_people_use() {
        let rows = 16u16;
        for reason in every_reason() {
            assert!(
                fits(&reason, "11-D-0704", rows),
                "{reason:?} is {} lines, taller than the pane",
                height(&reason, "11-D-0704")
            );
        }
    }
}
