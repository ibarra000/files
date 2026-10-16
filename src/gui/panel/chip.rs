//! Keyboard shortcut chips.
//!
//! `Ctrl+D` is drawn as two chips, `^` and `D`, with no plus between them.
//! Only `Ctrl` (`^`) and `Enter` (`↵`) are substituted; other parts are drawn as
//! written, so [`crate::view::actions`] spells shortcuts in plain words.

use eframe::egui::{Align2, Painter, Pos2, Rect, pos2, vec2};

use crate::gui::theme::{self, Theme, Weight};

/// Gap between the chips of one shortcut.
const PART_GAP: f32 = 2.0;

/// Horizontal padding inside a chip.
const PAD_X: f32 = 5.0;

/// Chip height: the line box of a caption (16 points).
///
/// Fluent's `<kbd>` sets only font size and horizontal padding, so the height
/// is the line height.
pub const HEIGHT: f32 = theme::line_h(theme::SIZE_CAPTION);

/// Returns the glyph drawn for one part of a shortcut.
fn glyph(part: &str) -> &str {
    match part {
        "Ctrl" => "\u{5E}",
        "Enter" => "\u{21B5}",
        other => other,
    }
}

/// Returns the drawn width of `shortcut`.
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

/// Returns the shortcut as text for the accessibility tree.
///
/// Uses key names rather than glyphs, which a screen reader would read as
/// "circumflex D".
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

    #[test]
    fn everything_else_is_left_alone() {
        for part in ["D", "E", "O", "K", "F5", ",", "Shift"] {
            assert_eq!(glyph(part), part);
        }
    }

    #[test]
    fn a_shortcut_is_read_out_in_words() {
        assert_eq!(spoken("Ctrl+D"), "Ctrl D");
        assert_eq!(spoken("Enter"), "Enter");
        assert_eq!(spoken("F5"), "F5");
    }
}
