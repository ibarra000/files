//! The header: ten points of air, a search box, ten more.
//!
//! Ueli's header is a single Fluent `Input` at size `large` with a search icon
//! before it and a placeholder in it, and this is that box. What it does is
//! unchanged from the panel it replaces - the caret scrolls the code, a click
//! puts the caret where the pointer is, a drag selects a run, and an alias
//! that fired says what it stood for at the right-hand end.
//!
//! # Why the band is a drag handle and the box is not
//!
//! The header carries `-webkit-app-region: drag` in Ueli and the input inside
//! it does not, which is what lets somebody move the window by the air around
//! the box without the box losing its clicks. The same thing happens here for
//! a duller reason: [`super::handles`] registers the whole band *before* this
//! runs, and the box is registered on top of it, so the box wins every press
//! that lands on it and the air wins the rest. Nothing computes where the air
//! is.

use eframe::egui::{Align2, Color32, Id, Rect, Sense, Ui, pos2, vec2};

use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};

/// How much room the caret keeps between itself and the right-hand edge of the
/// box once the code is long enough to scroll.
const CARET_MARGIN: f32 = 12.0;

/// The air between the box's left edge and the search icon.
const ICON_PAD: f32 = 12.0;

/// The air between the icon and the first character of the code.
const ICON_GAP: f32 = 10.0;

/// The air between the last character and the box's right edge.
const TAIL_PAD: f32 = 12.0;

/// Draws the header and reports what the pointer did to the box.
pub fn show(ui: &mut Ui, state: &AppState, theme: &Theme, band: Rect) -> Vec<Intent> {
    let rect = box_of(band);
    let painter = ui.painter().clone();
    let font = theme::font(theme::SIZE_INPUT, Weight::Regular);

    // The box reads as pressed into the panel rather than drawn on it: a
    // trough is what a search box looks like in this idiom, and it is the one
    // element here somebody puts something *into*. Fluent calls the same thing
    // `filled-darker`.
    theme::press(&painter, theme, rect, theme::ROW_RADIUS);

    // A magnifier, which is what every search field on this operating system
    // has, so nobody has to be told what the box is for.
    //
    // From the icon font the settings window already uses, with the width it
    // would have taken kept back either way - a machine with no Segoe Fluent
    // Icons loses the mark and keeps the alignment, rather than shuffling the
    // whole box left by twenty points.
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
        // One word. This used to be the whole instruction - "Type a job code,
        // for example 11-D-0704" - on the argument that a placeholder saying
        // "Search" is decoration beside a magnifier that already says it. The
        // argument was sound and the result was still wrong: a box that
        // explains itself is a box somebody reads, and this one is summoned
        // dozens of times an hour by people who learned what it was for on the
        // first day.
        painter.text(
            pos2(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            "Search",
            theme::font(theme::SIZE_ROW, Weight::Regular),
            theme.dim,
        );
    }

    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), theme.input);

    // What an alias stood for, at the right-hand end of the box.
    //
    // Here rather than on the status line because this is where the eye
    // already is, and because that line is a slot other things legitimately
    // claim - a toast, a drive that cannot be reached - which would leave an
    // alias firing with nothing on screen to say so. An alias that fires
    // silently is the program searching for something nobody typed. See
    // [`crate::alias`].
    let expansion = state
        .expansion()
        .map(|alias| format!("\u{2192} {}", alias.code));
    let expansion_galley = expansion.as_ref().map(|shown| {
        painter.layout_no_wrap(
            shown.clone(),
            theme::font(theme::SIZE_ROW, Weight::Regular),
            theme.dim,
        )
    });
    // The code gives up the room the expansion takes, rather than running
    // under it: two overlapping strings in a search box is worse than a code
    // that scrolls slightly sooner.
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

    // How much of the code there is room for.
    let window = Rect::from_min_max(
        pos2(text_left, rect.top()),
        pos2(rect.right() - TAIL_PAD - reserved, rect.bottom()),
    );

    // How far the code is scrolled under that window.
    //
    // Derived from where the caret is, never stored. A remembered offset is
    // how a text field ends up scrolled somewhere the caret is not - the same
    // rule the terminal build's `input_line` followed, and for the same
    // reason.
    //
    // Without any of this a pasted code simply kept going: `layout_no_wrap`
    // has no width to fit into, so thirty characters ran off the edge of the
    // panel, taking the caret with them. There was no way to see what you had
    // pasted and no way to get back to it.
    let caret_at = x_of(&galley, state.input.caret());
    let overflow = (galley.rect.width() - window.width()).max(0.0);
    let offset = if overflow <= 0.0 {
        0.0
    } else {
        // Enough of a margin that the caret is never *on* the edge it is
        // keeping itself inside.
        (caret_at - window.width() + CARET_MARGIN).clamp(0.0, overflow)
    };
    let origin = text_left - offset;
    let baseline = pos2(origin, rect.center().y - galley.rect.height() / 2.0);

    // Everything that moves with the text is clipped to the window, so a code
    // too long for the box stops at the box's edge rather than at the panel's.
    let painter = painter.with_clip_rect(window);

    // The run about to be copied, drawn under the text rather than over it.
    if let Some((from, to)) = state.input.selection() {
        let x0 = origin + x_of(&galley, from);
        let x1 = origin + x_of(&galley, to);
        painter.rect_filled(
            Rect::from_min_max(
                pos2(x0, baseline.y),
                pos2(x1, baseline.y + galley.rect.height()),
            ),
            theme::radius(theme::CHIP_RADIUS),
            theme.text_selection,
        );
    }

    let caret_x = origin + caret_at;
    painter.galley(baseline, galley, theme.input);

    // The box is the one thing on the panel somebody is *editing*, so it is
    // announced whether or not there is anything in it - a search box that
    // reads as empty is still a search box.
    //
    // The expansion goes in with it. A screen reader hearing `pw` and being
    // read a list of drawings for a code it never said is the same failure as
    // the silent one, reached by the ear instead of the eye.
    let spoken = match (&expansion, text.is_empty()) {
        (Some(shown), _) => format!("{text} {shown}"),
        (None, true) => "Search".to_string(),
        (None, false) => text.to_string(),
    };
    super::announce(ui, rect, "field", &spoken);

    // Solid rather than blinking. A caret that blinks is a repaint twice a
    // second for as long as the panel is up, and this one is never the only
    // thing on screen that says where the keyboard is going.
    painter.vline(
        caret_x,
        (rect.center().y - theme::SIZE_INPUT * 0.6)..=(rect.center().y + theme::SIZE_INPUT * 0.6),
        eframe::egui::Stroke::new(2.0, theme.caret),
    );

    click_to_caret(ui, rect, origin, text)
}

/// The search box inside the header band.
///
/// Ueli's header is `padding: 10` around one input at Fluent's `large` size,
/// which is forty points. Both numbers are the theme's, so the band and the
/// box cannot drift apart.
fn box_of(band: Rect) -> Rect {
    Rect::from_min_size(
        pos2(
            band.left() + theme::BAND_PAD,
            band.center().y - theme::INPUT_H / 2.0,
        ),
        vec2(band.width() - theme::BAND_PAD * 2.0, theme::INPUT_H),
    )
}

/// Where a byte offset falls, in points from the start of the text.
fn x_of(galley: &eframe::egui::Galley, byte: usize) -> f32 {
    let chars = galley.text()[..byte.min(galley.text().len())]
        .chars()
        .count();
    galley
        .pos_from_cursor(eframe::egui::text::CCursor::new(chars))
        .min
        .x
}

/// Clicking in the box puts the caret where the pointer is, which is what
/// every other text box on this machine does.
fn click_to_caret(ui: &mut Ui, rect: Rect, text_left: f32, text: &str) -> Vec<Intent> {
    let response = ui.interact(rect, Id::new("files-field"), Sense::click_and_drag());
    let Some(pos) = response.interact_pointer_pos() else {
        return Vec::new();
    };
    if !response.clicked() && !response.dragged() {
        return Vec::new();
    }

    let font = theme::font(theme::SIZE_INPUT, Weight::Regular);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let cursor = galley.cursor_from_pos(vec2(pos.x - text_left, 0.0));
    // `cursor_from_pos` counts characters; `Input` indexes bytes, and a share
    // name is not always ASCII.
    let byte = text
        .char_indices()
        .nth(cursor.index.into())
        .map_or(text.len(), |(offset, _)| offset);

    vec![Intent::Caret {
        byte,
        // A drag extends what a press started, which is how a run gets
        // selected for copying.
        extend: response.dragged(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::HEADER_H))
    }

    /// Ten points of air on every side of the box, which is the header's
    /// whole composition.
    #[test]
    fn the_box_sits_inside_the_bands_padding() {
        let rect = box_of(band());
        assert_eq!(rect.left(), theme::BAND_PAD);
        assert_eq!(rect.right(), theme::PANEL_W - theme::BAND_PAD);
        assert_eq!(rect.top(), theme::BAND_PAD);
        assert_eq!(rect.bottom(), theme::HEADER_H - theme::BAND_PAD);
        assert_eq!(rect.height(), theme::INPUT_H);
    }

    /// And there is air left for the window to be dragged by, which is the
    /// only reason the box is not the whole band.
    #[test]
    fn the_band_keeps_room_around_the_box_to_be_grabbed_by() {
        let band = band();
        let rect = box_of(band);
        assert!(band.contains(pos2(band.center().x, 2.0)));
        assert!(!rect.contains(pos2(band.center().x, 2.0)));
    }
}
