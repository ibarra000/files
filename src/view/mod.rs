//! What the screen says, with no opinion about how it looks.
//!
//! Every module here is a pure function from application state to the words,
//! marks and key hints that belong on screen - and to nothing else. There is no
//! colour, no rectangle and no widget: a caller renders these with whatever it
//! draws with.
//!
//! That split is not tidiness. This program spent its life as a terminal
//! application and is becoming a desktop one, and the part worth keeping across
//! that change is not the drawing - it is the wording. What to say when a drive
//! cannot be reached, which key to advertise when the window is narrow, how to
//! explain an empty result list to somebody who has typed a code that does not
//! exist: all of that was argued over once, is covered by tests that assert the
//! exact strings, and has nothing to do with cells or pixels.
//!
//! So it lives here, where both renderers can reach it, and the tests that pin
//! it come with it.
//!
//! # House style
//!
//! One rule set, written down once, checked by [`style::violations`] and
//! applied by tests in every module below. It exists because the panel used to
//! break all of these at once - a footer that read `Searching...` one second
//! and `nothing to open` the next, with `Building the file list - 812,000
//! files so far` in between.
//!
//! * **Sentence case.** Not Title Case and not SHOUTING. Key names, program
//!   names, acronyms and anything read out of the configuration keep their own
//!   spelling: `Enter`, `avwin.exe`, `PDF`, and a drive called `jobs`.
//! * **Fragments or sentences by slot, not by module.** The status slot - the
//!   status line and the toasts that share its 13 pt run - takes no full stop.
//!   The body pane takes whole sentences and keeps them.
//! * **`…` (U+2026), never `...`.** Three periods are three glyphs that kern
//!   badly and read as a pause rather than as work in progress.
//! * **` · ` joins independent facts.** ` - ` as a sentence connector is
//!   banned: at the width the footer actually gets it is indistinguishable
//!   from the hyphen inside `11-D-0704`.
//! * **Only the first `·`-joined clause is capitalised.** That is what lets
//!   the shared `label()` fragments in [`crate::index::store`] stay lowercase
//!   and be capitalised at the point of use by [`sentence`].
//! * **No run of two spaces.** Spacing is the renderer's business. A line that
//!   pads itself is a line that has been laid out twice, and the padding is
//!   what gets ellipsised and what a screen reader reads out.
//! * **Say "drive" on screen and "share" in the code.** Nobody outside this
//!   repository calls `R:\` a share.

use std::borrow::Cow;

pub mod style;

pub mod empty;
pub mod help;
pub mod hints;
pub mod row;
pub mod shares;
pub mod status;

/// The role a run of text plays.
///
/// Meanings, never colours. A terminal maps these to an SGR pair and a window
/// to a `Color32`, and neither is named here - which is the whole reason this
/// module can be read by both. `Tone` is carried through rather than flattened
/// into a fourth variant because the status line's five tones are already a
/// tested vocabulary, and a second one beside it would be two things to keep
/// in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Body,
    Dim,
    Strong,
    Accent,
    Tone(status::Tone),
}

/// A run of text with one emphasis. What a `Span` was, minus the styling.
///
/// `Cow` because most of this content is written out in full as `&'static str`
/// and only the diagnostics are formatted - which is exactly the split
/// `Span<'static>` already had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: Cow<'static, str>,
    pub emphasis: Emphasis,
}

impl Run {
    pub fn body(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            emphasis: Emphasis::Body,
        }
    }
    pub fn dim(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            emphasis: Emphasis::Dim,
        }
    }
    pub fn strong(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            emphasis: Emphasis::Strong,
        }
    }
    pub fn accent(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            emphasis: Emphasis::Accent,
        }
    }
    pub fn toned(text: impl Into<Cow<'static, str>>, tone: status::Tone) -> Self {
        Self {
            text: text.into(),
            emphasis: Emphasis::Tone(tone),
        }
    }

    /// An empty line. Spacing is content here: the blank rows between the
    /// headline, the fact and the thing to do are what make the three-part
    /// shape readable, and a renderer that dropped them would be rendering
    /// something else.
    pub fn blank() -> Self {
        Self::body("")
    }
}

/// A line of runs. What a `Line` was.
pub type Block = Vec<Run>;

/// Everything in one block as plain text, for tests and for anything that has
/// no styling to apply.
pub fn plain(block: &Block) -> String {
    block.iter().map(|r| r.text.as_ref()).collect()
}

/// Capitalises a shared fragment for the head of a line.
///
/// The `label()` fragments are written lowercase because most of their uses
/// are the second clause of a ` · ` join, where a capital would read as a new
/// sentence. This is for the uses that are not.
///
/// First character only, and only where it is alphabetic: a fragment that
/// begins with a path, a key name or a drive name read out of the
/// configuration keeps the spelling it was given.
pub fn sentence(fragment: &str) -> String {
    let mut chars = fragment.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => fragment.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fragment_is_capitalised_for_the_head_of_a_line() {
        assert_eq!(sentence("no live updates"), "No live updates");
    }

    /// A fragment that does not begin with a lowercase letter is left exactly
    /// as it was written: a switch, a code, an acronym.
    ///
    /// This is a floor and not a spell-checker. `avwin.exe` begins with a
    /// lowercase letter like any other word, so the only thing that keeps it
    /// spelled correctly is not calling this on it - which is why the two
    /// places that would are written out longhand.
    #[test]
    fn a_fragment_that_does_not_start_with_a_word_keeps_its_spelling() {
        for text in ["--check-config", "11-D-0704", "PDF is in use", ""] {
            assert_eq!(sentence(text), text, "{text} was rewritten");
        }
    }
}
