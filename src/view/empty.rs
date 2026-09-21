//! What the results pane says when it has no results.
//!
//! One line, centred, or nothing at all. That is the whole of it.
//!
//! # It used to be five
//!
//! Every variant had the same three-part shape - what happened, a fact about
//! it, then the thing to do - separated by blank rows, on the argument that
//! the eye learns where to look once and then knows. Read in a screenshot
//! that argument is sound. Read on a launcher it is not: this pane appears
//! for a second or two while somebody is still typing, and a paragraph that
//! appears and disappears under a search box is not read, it is a flicker
//! that makes the window feel unfinished.
//!
//! Ueli's equivalent is one centred line: `No results found for "x"`. That
//! is what this is now, and the detail the other four lines carried has
//! gone to the place somebody can read it at leisure - the Diagnostics page,
//! which says which drives were searched, how many files each holds, and
//! what went wrong with the ones that failed.
//!
//! What is *kept* is the distinction between a search that found nothing and
//! a search that could not be made: "nothing matched" and "that drive could
//! not be reached" are different facts and the second one is not the user's
//! fault. Where a line names a diagnostic it is kept verbatim, because that
//! is what makes a support call solvable.
//!
//! # `Option`, not a list
//!
//! `view` returns at most one block, and the type says so. It used to return
//! a `Vec<Block>` and the renderer had to centre a stack of them, measure
//! whether they fitted the pane, and fall back if they did not - three
//! pieces of machinery for a case that cannot arise once there is only ever
//! one line. `height`, `fits` and the stacking in `centred_blocks` all go
//! with it.
//!
//! A `Block` is still a `Vec<Run>`, because two of the messages mix tones
//! within one line - a diagnostic in the warning colour with ordinary words
//! either side of it.
//!
//! Indentation and centring are deliberately *not* here. Where a block sits
//! is layout, and the two renderers place it differently.

use crate::app::state::EmptyReason;
use crate::view::status::Tone;
use crate::view::{Block, Run};

/// Plain body text.
fn say(text: impl Into<String>) -> Block {
    vec![Run::body(text.into())]
}

/// A secondary clause on the same line.
fn aside(text: impl Into<String>) -> Run {
    Run::dim(text.into())
}

/// A diagnostic, kept verbatim because it is what makes a support call
/// solvable.
fn detail(text: impl Into<String>) -> Run {
    Run::toned(text.into(), Tone::Bad)
}

/// The message, as one block, or nothing.
pub fn view(reason: &EmptyReason, query: &str) -> Option<Block> {
    /// `Nothing matched "x"` or `Nothing matched`, depending.
    fn nothing_matched(query: &str) -> String {
        if query.is_empty() {
            "Nothing matched".to_owned()
        } else {
            format!("Nothing matched \u{201c}{query}\u{201d}")
        }
    }

    match reason {
        // Nothing. The panel has no body at all before anything is typed,
        // and a reason for an empty list is not something to say when nobody
        // has asked for a list yet.
        EmptyReason::NoQuery => None,

        // The share answered, incompletely. The counts are gone - they are
        // in the diagnostics - and what is left is the part that changes
        // what to do, which is that looking again may find more.
        EmptyReason::LiveIncomplete { name, .. } => Some(vec![
            Run::body(nothing_matched(query)),
            aside(format!(" on {name} yet \u{b7} F5 looks again")),
        ]),

        EmptyReason::LiveUnavailable { name, detail: why } => Some(vec![
            Run::body(format!("{name} could not be searched \u{b7} ")),
            detail(crate::view::sentence(why)),
        ]),

        EmptyReason::BadQuery { detail: why } => Some(vec![
            Run::body("That is not something this can look for \u{b7} "),
            aside(crate::view::sentence(why)),
        ]),

        EmptyReason::NoSharesConfigured => Some(vec![
            Run::body("No drives are set up yet \u{b7} "),
            Run::accent("files --check-config"),
            aside(" says why"),
        ]),

        EmptyReason::NoMatches { .. } => Some(say(nothing_matched(query))),

        EmptyReason::IndexUnavailable { detail: what } => Some(vec![
            Run::body("Cannot search right now \u{b7} "),
            detail(what.clone()),
        ]),

        EmptyReason::PathNotFound { dir } => Some(vec![
            Run::body("No folder for that job code \u{b7} "),
            aside(format!("looked in {}", dir.display())),
        ]),

        EmptyReason::AccessDenied { dir } => Some(vec![
            Run::body("You do not have permission to read "),
            aside(format!("{}", dir.display())),
        ]),

        // Not "Searching...", which is what the status line used to say at
        // the same moment - and does not any more, so this is the only thing
        // on screen that says a search is in flight.
        EmptyReason::NotSearchedYet => Some(vec![Run::toned("Looking for it\u{2026}", Tone::Busy)]),
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
            view(&EmptyReason::NoQuery, "").is_none(),
            "the untouched panel drew a body"
        );
    }

    /// A no-match names the code that did not match, and nothing else.
    ///
    /// "No matches" alone leaves somebody who has typed two codes in a row
    /// unsure which one this is about, so the code stays. The file count and
    /// the "press F5" that used to sit under it are gone: how many files
    /// were searched is a fact about the index, which is a page in the
    /// settings window, and the footer already names what F5 does.
    #[test]
    fn a_no_match_names_the_code_and_says_nothing_else() {
        let t = text(
            &EmptyReason::NoMatches {
                searched: 1_284_551,
            },
            "11-D-0704",
        );
        assert!(t.contains("11-D-0704"), "{t}");
        assert!(!t.contains("1,284,551"), "{t}");
        assert!(!t.contains('\n'), "more than one line:\n{t}");
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
    }

    /// Whoever installed this needs the command, exactly as it is typed.
    #[test]
    fn an_unconfigured_install_names_the_command_that_explains_it() {
        let t = text(&EmptyReason::NoSharesConfigured, "");
        assert!(t.contains("files --check-config"), "{t}");
    }

    /// Every message is one line, which replaces a test that checked they
    /// were short enough to fit.
    ///
    /// That test compared a block count against `VISIBLE_ROWS`, because the
    /// renderer drew a stack into the same band the result rows use and a
    /// block past the bottom was dropped rather than scrolled to - and the
    /// lines dropped first were the ones saying what to do. Structurally
    /// impossible beats bounded: with one block there is nothing to drop,
    /// and `height` and `fits` are gone with the stacking.
    #[test]
    fn every_message_is_one_line() {
        for reason in every_reason() {
            for query in ["", "11-D-0704"] {
                let Some(block) = view(&reason, query) else {
                    continue;
                };
                let line = crate::view::plain(&block);
                assert!(!line.contains('\n'), "{reason:?} is more than a line");
                // Wide enough to be worth saying, narrow enough to fit a
                // six-hundred-point panel at fourteen points.
                assert!(line.chars().count() < 90, "{reason:?}: {line:?}");
            }
        }
    }
}
