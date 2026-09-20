//! Where the parts of a setting row go.
//!
//! Arithmetic and nothing else: no `Ui`, no painter, no font. Split out for
//! the reason [`crate::view`] is split out one level up - a rule that can only
//! be checked by looking at it is a rule nobody checks - and because the one
//! thing this layout has to get right is invisible in a screenshot.
//!
//! # What it has to get right
//!
//! A setting row is a label, a sentence under the label, and a control to the
//! right of both. The control is centred against **the whole of the prose**,
//! not against its first line. With a one-line description the two are the
//! same thing and any arrangement looks correct; with a three-line
//! description an implementation that centres on the first line puts the
//! control near the top of the tile and looks like a mistake nobody can name.
//!
//! # Why egui's own `Sides` is not enough
//!
//! [`egui::containers::Sides`] is otherwise exactly this: `shrink_left`
//! measures the right-hand side first and gives the left whatever remains,
//! which is `flex: 1 1 0` and `flex: 0 0 auto` in the two places they are
//! wanted. But it builds both children inside a rect whose height is
//! `interact_size.y`, and centres the right-hand child in *that* - so the
//! control stays pinned near the top as the prose grows. That is the one
//! property above, and it is the one property `Sides` cannot express.
//!
//! So the row is measured and then painted. `Sides` is still the right tool
//! for a row of buttons, where both sides are one line tall.

use eframe::egui::{Rect, Vec2, pos2};

/// The padding inside a tile, on every side. Fluent's `Card` default.
pub const CARD_PAD: f32 = 12.0;

/// Between the prose and the control beside it.
///
/// Wider than the gap between two controls, deliberately: this is the gutter
/// that tells the eye the row has two columns, and at the width a card
/// actually gets it is the only thing that does.
pub const GAP: f32 = 16.0;

/// The narrowest the prose column may be squeezed to.
///
/// At this width a description wraps every three or four words, which is
/// unpleasant and still readable. Below it the column stops being prose and
/// becomes a stack of fragments, so the tile is allowed to overflow the card
/// instead - a row wider than the window can be scrolled to, and a row of
/// single-word lines cannot be unscrambled.
pub const MIN_PROSE_W: f32 = 140.0;

/// The gap between a label and the sentence under it.
///
/// Small. Ueli's `Setting` puts them in two bare `div`s with nothing between,
/// so the only separation is the difference in line height; at this program's
/// sizes that reads as slightly too tight, and two points is the whole
/// correction.
pub const LABEL_GAP: f32 = 2.0;

/// How wide the label and the sentence under it may be.
///
/// The control's width is spent first, which is what makes it `flex: 0 0
/// auto`: a drop-down does not shrink because a description is long.
pub fn prose_width(avail_w: f32, control_w: f32) -> f32 {
    let inner = avail_w - CARD_PAD * 2.0;
    let left = inner - control_w - GAP;
    left.max(MIN_PROSE_W)
}

/// How tall the whole tile is, given how tall the prose came out.
///
/// Never shorter than the control, or a row holding nothing but a switch
/// would be a different height from the row above it and the column would
/// stop being a column.
pub fn tile_height(prose_h: f32, control_h: f32) -> f32 {
    prose_h.max(control_h) + CARD_PAD * 2.0
}

/// Where the control goes: hard against the right padding, and centred on the
/// tile rather than on the first line of the prose.
///
/// See the module note. This one line is the whole reason this module is not
/// three calls to `Sides`.
pub fn control_slot(tile: Rect, control_w: f32, control_h: f32) -> Rect {
    Rect::from_min_size(
        pos2(
            tile.right() - CARD_PAD - control_w,
            tile.center().y - control_h / 2.0,
        ),
        Vec2::new(control_w, control_h),
    )
}

/// Where the prose starts.
pub fn prose_origin(tile: Rect) -> eframe::egui::Pos2 {
    pos2(tile.left() + CARD_PAD, tile.top() + CARD_PAD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    fn tile(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(100.0, 200.0), vec2(w, h))
    }

    /// The control is spent first, so a long description costs the prose
    /// column and never the drop-down.
    #[test]
    fn the_control_keeps_its_width_and_the_prose_gets_the_rest() {
        let w = prose_width(600.0, 220.0);
        assert_eq!(w, 600.0 - 24.0 - 220.0 - GAP);
    }

    /// Below the floor the row overflows rather than shredding the sentence.
    /// A row too wide can be scrolled to; a column of one-word lines cannot be
    /// unscrambled.
    #[test]
    fn a_narrow_window_stops_squeezing_the_prose_at_the_floor() {
        assert_eq!(prose_width(300.0, 220.0), MIN_PROSE_W);
        assert_eq!(prose_width(0.0, 220.0), MIN_PROSE_W);
    }

    /// A row holding one switch must be the same height as the row above it,
    /// or the column stops reading as a column.
    #[test]
    fn a_short_row_is_still_as_tall_as_the_control_in_it() {
        assert_eq!(tile_height(6.0, 28.0), 28.0 + CARD_PAD * 2.0);
    }

    #[test]
    fn a_tall_row_is_as_tall_as_its_prose() {
        assert_eq!(tile_height(90.0, 28.0), 90.0 + CARD_PAD * 2.0);
    }

    /// The property the module note is about, and the one `Sides` could not
    /// express: as the prose grows, the control stays in the middle.
    #[test]
    fn the_control_is_centred_on_the_whole_tile_and_not_on_its_first_line() {
        for prose_h in [18.0, 40.0, 96.0] {
            let h = tile_height(prose_h, 28.0);
            let tile = tile(600.0, h);
            let slot = control_slot(tile, 220.0, 28.0);
            assert!(
                (slot.center().y - tile.center().y).abs() < 0.01,
                "a {prose_h}pt description put the control {:.1}pt off centre",
                slot.center().y - tile.center().y
            );
        }
    }

    /// And it stays hard against the right padding whatever else moves, so a
    /// column of controls lines up down the page.
    #[test]
    fn every_control_ends_on_the_same_rule() {
        let tall = control_slot(tile(600.0, 120.0), 220.0, 28.0);
        let short = control_slot(tile(600.0, 52.0), 40.0, 20.0);
        assert_eq!(tall.right(), short.right());
        assert_eq!(tall.right(), 100.0 + 600.0 - CARD_PAD);
    }

    /// The prose and the control are laid inside the same padding, so nothing
    /// touches the edge of the tile.
    #[test]
    fn nothing_is_laid_outside_the_padding() {
        let tile = tile(600.0, 80.0);
        let origin = prose_origin(tile);
        assert_eq!(origin.x, tile.left() + CARD_PAD);
        assert_eq!(origin.y, tile.top() + CARD_PAD);

        let slot = control_slot(tile, 220.0, 28.0);
        assert!(slot.right() <= tile.right() - CARD_PAD + 0.01);
        assert!(slot.left() > origin.x + prose_width(600.0, 220.0));
    }
}
