//! Colours, in one place.
//!
//! Deliberately restrained: this is a tool people use dozens of times an hour
//! under fluorescent light, so colour carries meaning rather than decoration.
//! Tone maps to semantics - busy, good, warning, failure - and everything
//! else stays neutral.

use ratatui::style::{Color, Modifier, Style};

use super::status::Tone;

pub const ACCENT: Color = Color::Cyan;
pub const DIM: Color = Color::DarkGray;
pub const TEXT: Color = Color::Gray;

pub fn input() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn border() -> Style {
    Style::default().fg(DIM)
}

pub fn title() -> Style {
    Style::default().fg(TEXT)
}

pub fn help() -> Style {
    Style::default().fg(DIM)
}

pub fn selection() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Selected text in the search line.
///
/// Blue rather than the results list's grey: the two can be on screen at once,
/// and "the row you are on" and "the text you are about to copy" must not look
/// like the same thing. Reversed foreground so the code stays readable.
pub fn text_selection() -> Style {
    Style::default().bg(Color::Blue).fg(Color::White)
}

/// The matched substring, so the eye lands on why a row is there.
pub fn match_highlight() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::default().fg(TEXT),
        Tone::Busy => Style::default().fg(Color::Cyan),
        Tone::Good => Style::default().fg(Color::Green),
        Tone::Warn => Style::default().fg(Color::Yellow),
        Tone::Bad => Style::default().fg(Color::Red),
    }
}

pub fn toast(severity: crate::app::state::Severity) -> Style {
    use crate::app::state::Severity;
    match severity {
        Severity::Info => Style::default().fg(Color::Cyan),
        Severity::Warn => Style::default().fg(Color::Yellow),
        Severity::Error => Style::default().fg(Color::Red),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_a_distinct_colour() {
        let tones =
            [Tone::Normal, Tone::Busy, Tone::Good, Tone::Warn, Tone::Bad].map(|t| tone(t).fg);
        for (i, a) in tones.iter().enumerate() {
            for b in tones.iter().skip(i + 1) {
                assert_ne!(a, b, "tones must be visually distinguishable");
            }
        }
    }

    #[test]
    fn failure_is_red_and_success_is_green() {
        assert_eq!(tone(Tone::Bad).fg, Some(Color::Red));
        assert_eq!(tone(Tone::Good).fg, Some(Color::Green));
    }

    #[test]
    fn the_selection_is_visually_distinct_from_ordinary_rows() {
        assert!(selection().bg.is_some());
        assert!(selection().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selected_text_cannot_be_mistaken_for_the_selected_row_or_a_match() {
        // All three can share a frame. If any two agreed, the highlight would
        // stop meaning anything.
        assert!(text_selection().bg.is_some());
        assert_ne!(text_selection().bg, selection().bg);
        assert_ne!(text_selection().bg, match_highlight().bg);
        assert_ne!(text_selection().fg, match_highlight().fg);
    }
}
