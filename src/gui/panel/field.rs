//! The header: a padded search box with an icon, placeholder, caret scrolling,
//! click-to-caret, drag selection and the active alias expansion.
//!
//! The band around the box is a window drag handle. [`super::handles`] registers
//! the whole band before this runs and the box is registered on top, so presses
//! on the box go to the box and the rest go to the handle.

use eframe::egui::{Align2, Color32, CursorIcon, Id, Rect, Sense, Ui, pos2, vec2};

use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};

/// Gap kept between the caret and the box's right edge once the text scrolls.
const CARET_MARGIN: f32 = 12.0;

/// Padding between the box's left edge and the search icon.
const ICON_PAD: f32 = 12.0;

/// Gap between the icon and the text.
const ICON_GAP: f32 = 10.0;

/// Padding between the text and the box's right edge.
const TAIL_PAD: f32 = 12.0;

/// Draws the header and reports what the pointer did to the box.
pub fn show(ui: &mut Ui, state: &AppState, theme: &Theme, band: Rect) -> Vec<Intent> {
    let rect = box_of(band);
    let painter = ui.painter().clone();
    let font = theme::font(theme::SIZE_LARGE, Weight::Regular);

    // Flat fill, Fluent's `filled-darker` appearance.
    painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.well);
    // Always focused: nothing else on the panel takes keyboard focus.
    theme::focus_rule(&painter, theme, rect, true);

    // The icon's width is reserved even when Segoe Fluent Icons is missing,
    // so the text alignment does not shift.
    let icon_w = theme::ICON_NAV;
    if theme::has_icon(ui.ctx(), Icon::Search) {
        painter.text(
            pos2(rect.left() + ICON_PAD, rect.center().y),
            Align2::LEFT_CENTER,
            Icon::Search.text(),
            theme::icon_font(theme::ICON_NAV),
            theme.dim,
        );
    }

    let text_left = rect.left() + ICON_PAD + icon_w + ICON_GAP;
    let text = state.input.text();

    if text.is_empty() {
        painter.text(
            pos2(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            "Search",
            theme::font(theme::SIZE_LARGE, Weight::Regular),
            // Placeholder colour, Fluent's `NeutralForeground4`.
            theme.caption,
        );
    }

    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), theme.input);

    // The active alias expansion, at the right-hand end of the box. Shown here
    // rather than on the status line, which toasts and warnings can occupy.
    // See [`crate::alias`].
    let expansion = state
        .expansion()
        .map(|alias| format!("\u{2192} {}", alias.code));
    let expansion_galley = expansion.as_ref().map(|shown| {
        painter.layout_no_wrap(
            shown.clone(),
            theme::font(theme::SIZE_BODY, Weight::Regular),
            theme.dim,
        )
    });
    // The text area shrinks by the expansion's width so the two never overlap.
    let reserved = expansion_galley
        .as_ref()
        .map(|g| g.rect.width() + TAIL_PAD)
        .unwrap_or(0.0);
    if let Some(shown) = expansion_galley {
        painter.galley(
            pos2(
                rect.right() - TAIL_PAD - shown.rect.width(),
                rect.center().y - shown.rect.height() / 2.0,
            ),
            shown,
            theme.dim,
        );
    }

    // Visible text area.
    let window = Rect::from_min_max(
        pos2(text_left, rect.top()),
        pos2(rect.right() - TAIL_PAD - reserved, rect.bottom()),
    );

    // Horizontal scroll offset, derived from the caret each frame rather than
    // stored, so the caret is always visible.
    let caret_at = x_of(&galley, state.input.caret());
    let overflow = (galley.rect.width() - window.width()).max(0.0);
    let offset = if overflow <= 0.0 {
        0.0
    } else {
        (caret_at - window.width() + CARET_MARGIN).clamp(0.0, overflow)
    };
    let origin = text_left - offset;
    let baseline = pos2(origin, rect.center().y - galley.rect.height() / 2.0);

    // Clip scrolled content to the text area.
    let painter = painter.with_clip_rect(window);

    // Selection highlight, drawn under the text.
    if let Some((from, to)) = state.input.selection() {
        let x0 = origin + x_of(&galley, from);
        let x1 = origin + x_of(&galley, to);
        painter.rect_filled(
            Rect::from_min_max(
                pos2(x0, baseline.y),
                pos2(x1, baseline.y + galley.rect.height()),
            ),
            theme::radius(theme::RADIUS_MEDIUM),
            theme.text_selection,
        );
    }

    let caret_x = origin + caret_at;
    painter.galley(baseline, galley, theme.input);

    // Always announced, even when empty, and includes the alias expansion so a
    // screen reader reports what is actually being searched.
    let spoken = match (&expansion, text.is_empty()) {
        (Some(shown), _) => format!("{text} {shown}"),
        (None, true) => "Search".to_string(),
        (None, false) => text.to_string(),
    };
    super::announce(ui, rect, "field", &spoken);

    // Solid caret: blinking would force continuous repaints.
    painter.vline(
        caret_x,
        (rect.center().y - theme::line_h(theme::SIZE_LARGE) / 2.0)
            ..=(rect.center().y + theme::line_h(theme::SIZE_LARGE) / 2.0),
        eframe::egui::Stroke::new(2.0, theme.caret),
    );

    click_to_caret(ui, rect, origin, text)
}

/// Returns the search box rect: inset `BAND_PAD` within the band, `INPUT_H` tall.
fn box_of(band: Rect) -> Rect {
    Rect::from_min_size(
        pos2(
            band.left() + theme::BAND_PAD,
            band.center().y - theme::INPUT_H / 2.0,
        ),
        vec2(band.width() - theme::BAND_PAD * 2.0, theme::INPUT_H),
    )
}

/// Returns the x position of a byte offset, in points from the text start.
fn x_of(galley: &eframe::egui::Galley, byte: usize) -> f32 {
    let chars = galley.text()[..byte.min(galley.text().len())]
        .chars()
        .count();
    galley
        .pos_from_cursor(eframe::egui::text::CCursor::new(chars))
        .min
        .x
}

/// Moves the caret to a click, or extends the selection on drag.
fn click_to_caret(ui: &mut Ui, rect: Rect, text_left: f32, text: &str) -> Vec<Intent> {
    // With Alt held the panel-wide drag handle is on top and takes the hover,
    // so the I-beam disappears when a window drag would begin.
    let response = ui
        .interact(rect, Id::new("files-field"), Sense::click_and_drag())
        .on_hover_cursor(CursorIcon::Text);
    let Some(pos) = response.interact_pointer_pos() else {
        return Vec::new();
    };
    if !response.clicked() && !response.dragged() {
        return Vec::new();
    }

    let font = theme::font(theme::SIZE_LARGE, Weight::Regular);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let cursor = galley.cursor_from_pos(vec2(pos.x - text_left, 0.0));
    // `cursor_from_pos` counts characters; `Input` indexes bytes.
    let byte = text
        .char_indices()
        .nth(cursor.index.into())
        .map_or(text.len(), |(offset, _)| offset);

    vec![Intent::Caret {
        byte,
        extend: response.dragged(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::HEADER_H))
    }

    #[test]
    fn the_box_sits_inside_the_bands_padding() {
        let rect = box_of(band());
        assert_eq!(rect.left(), theme::BAND_PAD);
        assert_eq!(rect.right(), theme::PANEL_W - theme::BAND_PAD);
        assert_eq!(rect.top(), theme::BAND_PAD);
        assert_eq!(rect.bottom(), theme::HEADER_H - theme::BAND_PAD);
        assert_eq!(rect.height(), theme::INPUT_H);
    }

    #[test]
    fn the_band_keeps_room_around_the_box_to_be_grabbed_by() {
        let band = band();
        let rect = box_of(band);
        assert!(band.contains(pos2(band.center().x, 2.0)));
        assert!(!rect.contains(pos2(band.center().x, 2.0)));
    }
}
