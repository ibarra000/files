//! What the results pane says when it has no results.
//!
//! For somebody whose code found nothing this is the only screen that can say
//! what to do next. It gets the whole pane rather than the top corner of it: a
//! twelve-row box holding one grey sentence reads as a program that has
//! broken.
//!
//! It is no longer the *first* screen. An untouched panel shows the search box
//! and nothing under it - see [`crate::app::state::AppState::is_quiet`] - so
//! `NoQuery` produces no blocks at all. It used to produce five, which is a
//! page of instructions in front of somebody who summoned a search box to
//! search.
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
        // Nothing. The panel has no body at all before anything is typed, and
        // a reason for an empty list is not something to say when nobody has
        // asked for a list yet.
        EmptyReason::NoQuery => Vec::new(),

        EmptyReason::LiveIncomplete {
            name,
            searched,
            skipped,
        } => vec![
            say(format!(
                "Nothing matched \u{201c}{query}\u{201d} on {name} yet."
            )),
            blank(),
            aside(format!(
                "{} of {} folders were searched \u{b7} there may be more",
                humanize::count(*searched as usize),
                humanize::count((*searched + *skipped) as usize)
            )),
            blank(),
            aside("Press F5 to look again, or narrow the code."),
        ],

        EmptyReason::LiveUnavailable { name, detail: why } => vec![
            say(format!("{name} could not be searched.")),
            blank(),
            detail(crate::view::sentence(why)),
            blank(),
            aside("Anything found on the other drives is shown above."),
        ],

        EmptyReason::BadQuery { detail } => vec![
            say("That is not something this can look for."),
            blank(),
            aside(crate::view::sentence(detail)),
            blank(),
            aside("A * goes at the start or the end of a code, and ext:pdf narrows by type."),
        ],

        EmptyReason::QueryTooShort { .. } => vec![
            say("Keep typing."),
            blank(),
            aside(format!(
                "A job code needs at least {MIN_QUERY_LEN} characters."
            )),
        ],

        EmptyReason::NoSharesConfigured => vec![
            say("No drives are set up yet."),
            blank(),
            // The command is kept exactly as it is typed: it is the next
            // thing whoever installed this has to run. In its own run rather
            // than set off by extra spaces - padding a string is the
            // renderer's job done in the wrong place, and it is what gets
            // ellipsised and read out by a screen reader.
            vec![
                Run::dim("Run "),
                Run::accent("files --check-config"),
                Run::dim(" in a command prompt to see why."),
            ],
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

        // Not "Searching…", which is what the status line says at the same
        // moment. Two places on one screen saying the same word is how a
        // reader learns to stop reading one of them - and it is also why a
        // test asserting the pane had *not* taken over could be satisfied by
        // the footer instead.
        EmptyReason::NotSearchedYet => {
            vec![vec![Run::toned("Looking for it\u{2026}", Tone::Busy)]]
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

    /// Every line of every block, held to the house style.
    ///
    /// The body slot, so these keep their full stops - they are whole
    /// sentences with room to be. What they may not do is pad themselves,
    /// which two of them did: a "For example:" set off with three trailing
    /// spaces and a command set off with two on either side, both of which
    /// are the renderer's job done in the wrong layer.
    #[test]
    fn every_block_keeps_the_house_style() {
        // The joined block, not each run: a block is one line on screen, and
        // its runs carry the spaces between them. "Run " followed by
        // "files --check-config" is one clean line and two runs that would
        // each look like they had a stray space on the end.
        let mut lines = Vec::new();
        for reason in every_reason() {
            for query in ["", "11-D-0704"] {
                for block in view(&reason, query) {
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

    /// A blank pane with no explanation is the state this whole type exists to
    /// make unreachable.
    ///
    /// `NoQuery` is exempt and it is not an exception to the rule: there is no
    /// pane. An untouched panel draws the field and stops, so there is nothing
    /// blank to explain. Every reason that *does* get a pane still has to fill
    /// it.
    #[test]
    fn every_reason_says_something() {
        for reason in every_reason() {
            if reason == EmptyReason::NoQuery {
                continue;
            }
            let t = text(&reason, "11-D-0704");
            assert!(
                t.lines().any(|l| l.trim().len() > 8),
                "{reason:?} says nothing worth reading:\n{t}"
            );
        }
    }

    /// The screen the office sees every morning is a search box and nothing
    /// else. It used to be five lines teaching what a job code looks like,
    /// which is a page of instructions in front of somebody who summoned a
    /// search box to search.
    #[test]
    fn the_first_screen_says_nothing_at_all() {
        assert!(
            view(&EmptyReason::NoQuery, "").is_empty(),
            "the untouched panel drew a body"
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

    /// Every variant is short enough to fit the pane it is drawn into.
    ///
    /// [`crate::config::VISIBLE_ROWS`], not the sixteen a terminal used to
    /// give it: `gui::overlay` draws these blocks into the same body the
    /// result rows use and stops at the bottom of it, so a block past that
    /// count is not scrolled to, it is *dropped* - and the lines these drop
    /// first are the ones saying what to do about it.
    #[test]
    fn every_message_fits_the_pane_it_is_drawn_into() {
        let rows = crate::config::VISIBLE_ROWS as u16;
        for reason in every_reason() {
            assert!(
                fits(&reason, "11-D-0704", rows),
                "{reason:?} is {} lines, taller than the pane",
                height(&reason, "11-D-0704")
            );
        }
    }
}
