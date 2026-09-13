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

use std::borrow::Cow;

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
