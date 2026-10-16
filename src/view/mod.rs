//! On-screen text, independent of how it is drawn.
//!
//! Each module is a pure function from application state to the words and
//! emphasis that belong on screen, with no colours, rectangles or widgets. The
//! exact strings are pinned by tests here so every renderer shares them.
//!
//! # House style
//!
//! Checked by [`style::violations`] and applied by tests in every module below.
//!
//! * **Sentence case.** Key names, program names, acronyms and configured
//!   names keep their own spelling: `Enter`, `avwin.exe`, `PDF`, `jobs`.
//! * **Fragments or sentences by slot.** The status slot (status line and
//!   toasts) takes no full stop; the body pane takes whole sentences.
//! * **`…` (U+2026), never `...`.**
//! * **` · ` joins independent facts.** ` - ` is not a connector; at footer
//!   width it is indistinguishable from the hyphen in `11-D-0704`.
//! * **Only the first `·`-joined clause is capitalised.** Shared `label()`
//!   fragments in [`crate::index::store`] stay lowercase and are capitalised
//!   at the point of use by [`sentence`].
//! * **No run of two spaces.** Spacing belongs to the renderer.
//! * **"Drive" on screen, "share" in the code.**

use std::borrow::Cow;

pub mod style;

pub mod actions;
pub mod empty;
pub mod row;
pub mod settings;
pub mod shares;
pub mod status;

/// The semantic role of a run of text; the renderer maps it to a colour.
///
/// `Tone` reuses the status line's tones so there is one vocabulary to keep
/// in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Body,
    Dim,
    Strong,
    Accent,
    Tone(status::Tone),
}

/// A run of text with one emphasis.
///
/// `Cow` because most text is `&'static str` and only diagnostics are
/// formatted.
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

    /// An empty line. Blank rows are content and must not be dropped by a
    /// renderer.
    pub fn blank() -> Self {
        Self::body("")
    }
}

/// A line of runs.
pub type Block = Vec<Run>;

/// Returns the block as plain text, without emphasis.
pub fn plain(block: &Block) -> String {
    block.iter().map(|r| r.text.as_ref()).collect()
}

/// Capitalises a shared lowercase fragment for the head of a line.
///
/// Only a leading lowercase letter changes; fragments starting with a path,
/// switch or code are returned unchanged.
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

    /// Lowercase program names such as `avwin.exe` would still be changed, so
    /// callers must not pass them.
    #[test]
    fn a_fragment_that_does_not_start_with_a_word_keeps_its_spelling() {
        for text in ["--check-config", "11-D-0704", "PDF is in use", ""] {
            assert_eq!(sentence(text), text, "{text} was rewritten");
        }
    }
}
