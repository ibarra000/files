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
/// becomes a stack of fragments.
///
/// This note used to end differently: "so the tile is allowed to overflow
/// the card instead - a row wider than the window can be scrolled to". There
/// is no horizontal scroller in the settings window and there never was, so
/// the escape hatch it reasoned from did not exist and the overflow simply
/// ran the prose under the control. What gives way now is the control, down
/// to [`MIN_CONTROL_W`], and below *that* the prose gives way after all -
/// because two columns that overlap are worse than either being narrow.
pub const MIN_PROSE_W: f32 = 140.0;

/// And the narrowest a control may be squeezed to.
///
/// A drop-down at this width still shows a word and a half of its value
/// before the ellipsis, and a text box still shows enough of a path to tell
/// two apart. Under it a control is a decoration that reports its own state
/// in three characters.
pub const MIN_CONTROL_W: f32 = 120.0;

/// What is left of a tile once its padding is taken off both sides.
fn inner_w(avail_w: f32) -> f32 {
    (avail_w - CARD_PAD * 2.0).max(0.0)
}

/// How wide the control actually gets, having asked for `want`.
///
/// `flex: 0 0 auto` until the prose reaches its floor, and `flex: 0 1 auto`
/// from there down to [`MIN_CONTROL_W`]. The old model was 0-0-auto all the
/// way down, which is why below about 420 points the prose and the control
/// overlapped: the control kept every point it asked for, the prose was
/// handed a negative number that clamped to its floor, and the two then sat
/// in the same place.
///
/// The first clamp is the one that makes overlap impossible rather than
/// merely unlikely. Whatever else happens a control is never wider than the
/// tile it is in, so in the worst case the prose is given zero and the two
/// columns meet rather than cross.
pub fn control_width(avail_w: f32, want: f32) -> f32 {
    let inner = inner_w(avail_w);
    let want = want.min(inner);
    let room = inner - GAP - MIN_PROSE_W;
    if want <= room {
        return want;
    }
    let floor = MIN_CONTROL_W.min(want);
    room.clamp(floor, want)
}

/// One column of a list row.
///
/// A drive row is a mark, a letter, a path, a caveat and two buttons, and
/// only one of those has any business growing when the window does. Writing
/// the row as a list of these rather than as six hard numbers is what lets
/// it be checked without a font atlas - and what turns "the window got
/// narrower" into a question with one answer instead of six.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    /// Takes exactly this much, at every width, and gives none of it back.
    Fixed(f32),
    /// Takes at least this much, and an equal share of whatever is spare.
    Flex(f32),
}

impl Cell {
    pub const fn least(self) -> f32 {
        match self {
            Self::Fixed(w) | Self::Flex(w) => w,
        }
    }
}

/// How wide each column of a list row gets, or `None` if they do not fit.
///
/// `None` is a real answer rather than a failure: it is the add row's signal
/// to fall onto two lines. That is the one place in this window where
/// wrapping beats eliding, because an elided text box is a text box nobody
/// can use, and it is worth having a shape that can say so instead of a
/// width that quietly goes negative.
pub fn row_cells(avail_w: f32, gap: f32, cells: &[Cell]) -> Option<Vec<f32>> {
    let gaps = gap * cells.len().saturating_sub(1) as f32;
    let budget = avail_w - gaps;
    let least: f32 = cells.iter().map(|c| c.least()).sum();
    if least > budget {
        return None;
    }
    let flexes = cells.iter().filter(|c| matches!(c, Cell::Flex(_))).count();
    let share = if flexes == 0 {
        0.0
    } else {
        (budget - least) / flexes as f32
    };
    Some(
        cells
            .iter()
            .map(|c| match c {
                Cell::Fixed(w) => *w,
                Cell::Flex(min) => min + share,
            })
            .collect(),
    )
}

/// The gap between a label and the sentence under it.
///
/// Small. Ueli's `Setting` puts them in two bare `div`s with nothing between,
/// so the only separation is the difference in line height; at this program's
/// sizes that reads as slightly too tight, and two points is the whole
/// correction.
pub const LABEL_GAP: f32 = 2.0;

/// How wide the label and the sentence under it may be.
///
/// Whatever the control did not take, which is why [`control_width`] has to
/// run first. The floor that used to be applied here has moved there, where
/// it can be honoured by shrinking something rather than by clamping a
/// negative number and hoping.
pub fn prose_width(avail_w: f32, control_w: f32) -> f32 {
    (inner_w(avail_w) - control_w - GAP).max(0.0)
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

    /// How a row is measured, everywhere: the control first, then whatever
    /// is left over.
    fn columns(avail_w: f32, want: f32) -> (f32, f32) {
        let control = control_width(avail_w, want);
        (prose_width(avail_w, control), control)
    }

    /// With room to spare the control keeps every point it asked for and the
    /// prose gets the rest.
    #[test]
    fn the_control_keeps_its_width_while_there_is_room_for_both() {
        let (prose, control) = columns(600.0, 220.0);
        assert_eq!(control, 220.0);
        assert_eq!(prose, 600.0 - CARD_PAD * 2.0 - 220.0 - GAP);
    }

    /// And when there is not, the control is what gives way. This is the
    /// whole of the overlap fix: the old model shrank neither, handed the
    /// prose a negative width, clamped it to the floor, and drew the two on
    /// top of each other.
    #[test]
    fn the_control_gives_way_before_the_prose_does() {
        let (prose, control) = columns(300.0, 220.0);
        assert!(control < 220.0, "the control did not give way");
        assert_eq!(prose, MIN_PROSE_W, "the prose gave way first");
    }

    /// It stops giving way at a width where it is still a control.
    #[test]
    fn a_control_stops_shrinking_at_its_own_floor() {
        assert_eq!(control_width(220.0, 220.0), MIN_CONTROL_W);
    }

    /// A control that wanted less than the floor is not grown to meet it.
    #[test]
    fn a_small_control_is_never_inflated_to_the_floor() {
        assert_eq!(control_width(200.0, 40.0), 40.0);
    }

    /// The property the whole module exists for, swept rather than sampled.
    ///
    /// Every width from a sliver to a wide window, against four control
    /// widths. Under about 170 points the prose is zero and the two columns
    /// *meet*; they never cross, which is the claim.
    #[test]
    fn the_two_columns_never_overlap() {
        for tenths in 0..8000u32 {
            let avail = tenths as f32 / 10.0;
            for want in [40.0, 120.0, 220.0, 400.0] {
                let (prose, control) = columns(avail, want);
                let used = CARD_PAD * 2.0 + prose + control;
                assert!(
                    used <= avail.max(CARD_PAD * 2.0) + 0.001,
                    "at {avail}pt a {want}pt control and {prose:.1}pt of prose came to {used:.1}pt"
                );
                assert!(control >= 0.0 && prose >= 0.0);
            }
        }
    }

    /// A row of columns fills its width exactly, and the fixed ones do not
    /// move.
    #[test]
    fn a_list_row_spends_every_point_it_is_given() {
        let cells = [
            Cell::Fixed(28.0),
            Cell::Flex(80.0),
            Cell::Flex(120.0),
            Cell::Fixed(56.0),
        ];
        let widths = row_cells(600.0, 8.0, &cells).expect("these fit");
        assert_eq!(widths[0], 28.0);
        assert_eq!(widths[3], 56.0);
        let total: f32 = widths.iter().sum::<f32>() + 8.0 * 3.0;
        assert!((total - 600.0).abs() < 0.001, "{total} of 600");
        // The spare is shared equally, so two flexible columns that started
        // forty apart stay forty apart.
        assert!((widths[2] - widths[1] - 40.0).abs() < 0.001);
    }

    /// And says so when they do not, rather than handing back widths that
    /// add up to more than there is.
    #[test]
    fn a_list_row_that_cannot_fit_says_so() {
        let cells = [Cell::Fixed(200.0), Cell::Flex(200.0)];
        assert!(row_cells(300.0, 8.0, &cells).is_none());
        assert!(row_cells(408.0, 8.0, &cells).is_some());
    }

    /// A row of nothing but fixed columns does not stretch to fill.
    #[test]
    fn fixed_columns_are_fixed() {
        let widths = row_cells(600.0, 8.0, &[Cell::Fixed(28.0), Cell::Fixed(56.0)]).unwrap();
        assert_eq!(widths, vec![28.0, 56.0]);
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
        assert!(slot.left() >= origin.x + prose_width(600.0, 220.0));
    }
}
