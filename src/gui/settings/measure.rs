//! Pure layout arithmetic for setting and list rows (no `Ui` or fonts), so it
//! can be unit tested.
//!
//! A setting row's control is centred vertically on the whole tile, not the
//! first line of its description. `egui::containers::Sides` cannot do this:
//! it centres the right child within `interact_size.y`, pinning the control
//! near the top as the prose grows.

use eframe::egui::{Rect, Vec2, pos2};

/// Padding inside a tile on every side (Fluent's `Card` default).
pub const CARD_PAD: f32 = 12.0;

/// Gutter between the prose and the control; wider than the gap between
/// controls so the two columns read as distinct.
pub const GAP: f32 = 16.0;

/// Minimum prose column width before the control starts shrinking.
///
/// The control gives way first, down to [`MIN_CONTROL_W`]; below that the
/// prose shrinks too, so the columns never overlap.
pub const MIN_PROSE_W: f32 = 140.0;

/// Minimum control width; still shows enough of a value or path to read.
pub const MIN_CONTROL_W: f32 = 120.0;

/// Returns the tile width minus padding on both sides.
fn inner_w(avail_w: f32) -> f32 {
    (avail_w - CARD_PAD * 2.0).max(0.0)
}

/// Returns the control's width given the width it wants.
///
/// The control keeps `want` until the prose reaches [`MIN_PROSE_W`], then
/// shrinks toward [`MIN_CONTROL_W`]. Clamping `want` to the inner width
/// guarantees the columns can meet but never overlap.
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    /// Exactly this width at every row width.
    Fixed(f32),
    /// At least this width, plus an equal share of the spare space.
    Flex(f32),
}

impl Cell {
    pub const fn least(self) -> f32 {
        match self {
            Self::Fixed(w) | Self::Flex(w) => w,
        }
    }
}

/// Returns each column's width, or `None` if the minimums do not fit.
///
/// The add row uses `None` as its signal to wrap onto two lines.
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

/// Vertical gap between a label and its description.
pub const LABEL_GAP: f32 = 2.0;

/// Returns the width left for the label and description.
///
/// Call [`control_width`] first; this takes whatever the control did not.
pub fn prose_width(avail_w: f32, control_w: f32) -> f32 {
    (inner_w(avail_w) - control_w - GAP).max(0.0)
}

/// Returns the tile height, never shorter than the control plus padding.
pub fn tile_height(prose_h: f32, control_h: f32) -> f32 {
    prose_h.max(control_h) + CARD_PAD * 2.0
}

/// Returns the control's rect: flush with the right padding and centred
/// vertically on the tile (see the module note).
pub fn control_slot(tile: Rect, control_w: f32, control_h: f32) -> Rect {
    Rect::from_min_size(
        pos2(
            tile.right() - CARD_PAD - control_w,
            tile.center().y - control_h / 2.0,
        ),
        Vec2::new(control_w, control_h),
    )
}

/// Returns the top-left of the prose inside the padding.
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

    /// Measures a row as production does: control first, then prose.
    fn columns(avail_w: f32, want: f32) -> (f32, f32) {
        let control = control_width(avail_w, want);
        (prose_width(avail_w, control), control)
    }

    #[test]
    fn the_control_keeps_its_width_while_there_is_room_for_both() {
        let (prose, control) = columns(600.0, 220.0);
        assert_eq!(control, 220.0);
        assert_eq!(prose, 600.0 - CARD_PAD * 2.0 - 220.0 - GAP);
    }

    #[test]
    fn the_control_gives_way_before_the_prose_does() {
        let (prose, control) = columns(300.0, 220.0);
        assert!(control < 220.0, "the control did not give way");
        assert_eq!(prose, MIN_PROSE_W, "the prose gave way first");
    }

    #[test]
    fn a_control_stops_shrinking_at_its_own_floor() {
        assert_eq!(control_width(220.0, 220.0), MIN_CONTROL_W);
    }

    #[test]
    fn a_small_control_is_never_inflated_to_the_floor() {
        assert_eq!(control_width(200.0, 40.0), 40.0);
    }

    /// Sweeps 0 to 800 points against four control widths; below about 170
    /// points the prose is zero and the columns meet without crossing.
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

    /// Flex columns fill the width exactly; fixed columns keep their size.
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
        // Spare space is shared equally, so the 40-point difference remains.
        assert!((widths[2] - widths[1] - 40.0).abs() < 0.001);
    }

    #[test]
    fn a_list_row_that_cannot_fit_says_so() {
        let cells = [Cell::Fixed(200.0), Cell::Flex(200.0)];
        assert!(row_cells(300.0, 8.0, &cells).is_none());
        assert!(row_cells(408.0, 8.0, &cells).is_some());
    }

    #[test]
    fn fixed_columns_are_fixed() {
        let widths = row_cells(600.0, 8.0, &[Cell::Fixed(28.0), Cell::Fixed(56.0)]).unwrap();
        assert_eq!(widths, vec![28.0, 56.0]);
    }

    #[test]
    fn a_short_row_is_still_as_tall_as_the_control_in_it() {
        assert_eq!(tile_height(6.0, 28.0), 28.0 + CARD_PAD * 2.0);
    }

    #[test]
    fn a_tall_row_is_as_tall_as_its_prose() {
        assert_eq!(tile_height(90.0, 28.0), 90.0 + CARD_PAD * 2.0);
    }

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

    /// Controls align on the right padding regardless of tile size.
    #[test]
    fn every_control_ends_on_the_same_rule() {
        let tall = control_slot(tile(600.0, 120.0), 220.0, 28.0);
        let short = control_slot(tile(600.0, 52.0), 40.0, 20.0);
        assert_eq!(tall.right(), short.right());
        assert_eq!(tall.right(), 100.0 + 600.0 - CARD_PAD);
    }

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
