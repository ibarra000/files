//! A keyboard shortcut, drawn the way Ueli draws one.
//!
//! `Ctrl+D` is not one chip reading "Ctrl+D". It is two chips, `^` and `D`,
//! two points apart, with no plus drawn between them - because that is what
//! the keyboard looks like, and because a chip wide enough to hold `Ctrl+Shift`
//! is a chip that pushes the label it belongs to off the row.
//!
//! The substitutions are the two that matter on this panel: `Ctrl` is `^` and
//! `Enter` is `↵`. Everything else is drawn as it is written, which is why
//! [`crate::view::actions`] writes shortcuts the way somebody would say them
//! rather than in any symbolic form of its own.

use eframe::egui::{Align2, Painter, Pos2, Rect, pos2, vec2};

use crate::gui::theme::{self, Theme, Weight};

/// The gap between two parts of one shortcut.
const PART_GAP: f32 = 2.0;

/// The air inside a chip, left and right of its glyph.
const PAD_X: f32 = 5.0;

/// How tall a chip is.
///
/// Sixteen. Fluent's `<kbd>` sets a font size and horizontal padding and
/// nothing else, so the height is the line box: a twelve-point caption at
/// `line_h` is sixteen, and the five points of padding are all horizontal.
pub const HEIGHT: f32 = theme::line_h(theme::SIZE_CAPTION);

/// What one part of a shortcut is drawn as.
///
/// `Ctrl` and `Enter` have marks everybody on this operating system already
/// reads; nothing else on this panel's keys does, so nothing else is
/// substituted. A vocabulary of two is one nobody has to be taught.
fn glyph(part: &str) -> &str {
    match part {
        "Ctrl" => "\u{5E}",
        "Enter" => "\u{21B5}",
        other => other,
    }
}

/// How wide `shortcut` will be.
pub fn width(painter: &Painter, shortcut: &str) -> f32 {
    let font = theme::font(theme::SIZE_CAPTION, Weight::Regular);
    let mut total = 0.0;
    for (i, part) in shortcut.split('+').enumerate() {
        if i > 0 {
            total += PART_GAP;
        }
        total += painter
            .layout_no_wrap(glyph(part).to_owned(), font.clone(), theme::tint(0, 255))
            .rect
            .width()
            + PAD_X * 2.0;
    }
    total
}

/// Draws `shortcut` with its left edge at `at`, vertically centred on it.
pub fn draw(painter: &Painter, theme: &Theme, shortcut: &str, at: Pos2) {
    let font = theme::font(theme::SIZE_CAPTION, Weight::Regular);
    let mut x = at.x;
    for part in shortcut.split('+') {
        let text = glyph(part);
        let w = painter
            .layout_no_wrap(text.to_owned(), font.clone(), theme.key_fg)
            .rect
            .width()
            + PAD_X * 2.0;
        let chip = Rect::from_min_size(pos2(x, at.y - HEIGHT / 2.0), vec2(w, HEIGHT));
        painter.rect_filled(chip, theme::radius(theme::RADIUS_MEDIUM), theme.key_bg);
        painter.text(
            chip.center(),
            Align2::CENTER_CENTER,
            text,
            font.clone(),
            theme.key_fg,
        );
        x += w + PART_GAP;
    }
}

/// How a shortcut is read out, for the accessibility tree.
///
/// Spelled rather than drawn: `^ D` is a pair of pictures, and a screen
/// reader handed those would say "circumflex D".
pub fn spoken(shortcut: &str) -> String {
    shortcut.replace('+', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_keys_with_marks_get_them() {
        assert_eq!(glyph("Ctrl"), "\u{5E}");
        assert_eq!(glyph("Enter"), "\u{21B5}");
    }

    /// Everything else is drawn as written, because nothing else on this
    /// panel's keys has a mark anybody reads without being told.
    #[test]
    fn everything_else_is_left_alone() {
        for part in ["D", "E", "O", "K", "F5", ",", "Shift"] {
            assert_eq!(glyph(part), part);
        }
    }

    /// A reader gets the words, not the pictures.
    #[test]
    fn a_shortcut_is_read_out_in_words() {
        assert_eq!(spoken("Ctrl+D"), "Ctrl D");
        assert_eq!(spoken("Enter"), "Enter");
        assert_eq!(spoken("F5"), "F5");
    }
}
