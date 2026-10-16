//! Overlay placement arithmetic, in device pixels.
//!
//! Kept free of Win32 calls (see [`super::win`]) so it can be unit-tested.

/// A rectangle in device pixels, the unit `SetWindowPos` and
/// `GetMonitorInfoW` use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectPx {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// Minimum window size, so a bad size never yields an unusable window.
pub const MIN_PX: (i32, i32) = (320, 120);

/// Fraction of the spare work-area height above the overlay's top edge.
///
/// Keeps the overlay clear of a camera notch and of maximised title bars.
pub const TOP_FRACTION: f32 = 0.12;

/// Returns the default overlay rectangle: centred horizontally, in the upper
/// part of `work`.
///
/// `work` must be the work area of the window's own monitor, not the primary
/// monitor's (`SPI_GETWORKAREA`). Clamp order is size, then the work area as a
/// ceiling, then position, so a bad size yields at worst a full-screen window,
/// never an off-screen one.
pub fn place(work: RectPx, want: (i32, i32)) -> RectPx {
    let w = want
        .0
        .clamp(MIN_PX.0.min(work.width()), work.width().max(1));
    let h = want
        .1
        .clamp(MIN_PX.1.min(work.height()), work.height().max(1));

    let left = work.left + (work.width() - w) / 2;
    let down = (work.height() - h) as f32 * TOP_FRACTION;
    let top = work.top + if down.is_finite() { down as i32 } else { 0 };

    // Nudge back inside rather than clip, so the whole overlay stays visible.
    let left = left.clamp(work.left, (work.right - w).max(work.left));
    let top = top.clamp(work.top, (work.bottom - h).max(work.top));

    RectPx::new(left, top, left + w, top + h)
}

/// Returns the overlay rectangle at a remembered top-left corner `at`.
///
/// Uses the same clamp order as [`place`]; a position outside `work` (for
/// example from an unplugged monitor) is nudged inside rather than discarded,
/// so restoring is always predictable.
pub fn place_at(work: RectPx, want: (i32, i32), at: (i32, i32)) -> RectPx {
    let w = want
        .0
        .clamp(MIN_PX.0.min(work.width()), work.width().max(1));
    let h = want
        .1
        .clamp(MIN_PX.1.min(work.height()), work.height().max(1));

    let left = at.0.clamp(work.left, (work.right - w).max(work.left));
    let top = at.1.clamp(work.top, (work.bottom - h).max(work.top));

    RectPx::new(left, top, left + w, top + h)
}

/// Which edge of the work area a docked panel is pinned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

/// Returns a docked panel rectangle: full work-area width, `height` tall,
/// flush against `edge`.
///
/// Uses the work area so a bottom dock sits above the taskbar. The height is
/// clamped as in [`place`].
pub fn place_docked(work: RectPx, height: i32, edge: Edge) -> RectPx {
    let h = height.clamp(MIN_PX.1.min(work.height()), work.height().max(1));
    let top = match edge {
        Edge::Top => work.top,
        Edge::Bottom => work.bottom - h,
    };
    RectPx::new(work.left, top, work.right, top + h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RectPx = RectPx::new(0, 0, 1920, 1040);

    #[test]
    fn the_overlay_is_centred_horizontally() {
        let r = place(WORK, (720, 300));
        assert_eq!(r.width(), 720);
        assert_eq!(r.left, (1920 - 720) / 2);
        assert_eq!(r.right, r.left + 720);
    }

    #[test]
    fn the_overlay_sits_in_the_upper_part_of_the_work_area() {
        let r = place(WORK, (720, 300));
        let expected = ((1040 - 300) as f32 * TOP_FRACTION) as i32;
        assert_eq!(r.top, expected);
        assert!(r.top > 0, "flush with the top edge");
        assert!(r.top < 1040 / 3, "too far down to be glanced at");
    }

    #[test]
    fn the_overlay_is_placed_within_the_work_area_it_was_given() {
        let work = RectPx::new(-1920, 200, 0, 1000);
        let r = place(work, (720, 300));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    #[test]
    fn a_size_larger_than_the_screen_fills_it_rather_than_leaving_it() {
        let r = place(WORK, (99_999, 99_999));
        assert_eq!(r, RectPx::new(0, 0, 1920, 1040));
    }

    #[test]
    fn a_size_of_nothing_is_raised_to_something_that_can_be_seen() {
        let r = place(WORK, (0, 0));
        assert_eq!((r.width(), r.height()), MIN_PX);
        assert!(r.left >= WORK.left && r.right <= WORK.right);
    }

    #[test]
    fn a_nonsensical_size_is_still_placed_on_the_screen() {
        let r = place(WORK, (-500, -500));
        assert!(r.width() >= MIN_PX.0.min(WORK.width()));
        assert!(r.height() >= MIN_PX.1.min(WORK.height()));
        assert!(r.left >= WORK.left && r.top >= WORK.top);
    }

    /// A work area smaller than `MIN_PX` (e.g. a small remote session).
    #[test]
    fn a_tiny_screen_gets_a_window_that_fits_it() {
        let work = RectPx::new(0, 0, 200, 100);
        let r = place(work, (720, 300));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    #[test]
    fn the_top_edge_is_decided_by_the_height_it_was_given() {
        let short = place(WORK, (720, 164));
        let tall = place(WORK, (720, 444));
        assert!(
            short.top > tall.top,
            "a taller panel should start higher, so both stay centred-ish"
        );
        assert_eq!(
            short.left, tall.left,
            "the centre does not move with height"
        );
    }

    // --- remembered position ------------------------------------------------

    #[test]
    fn a_remembered_position_is_used_as_it_stands() {
        let r = place_at(WORK, (720, 300), (40, 700));
        assert_eq!(r, RectPx::new(40, 700, 760, 1000));
    }

    /// Negative coordinates are valid for a monitor left of the primary.
    #[test]
    fn a_position_on_a_monitor_left_of_the_primary_one_is_honoured() {
        let work = RectPx::new(-1920, 0, 0, 1040);
        let r = place_at(work, (720, 300), (-1500, 120));
        assert_eq!(r, RectPx::new(-1500, 120, -780, 420));
    }

    #[test]
    fn a_position_off_the_work_area_is_nudged_fully_inside_it() {
        let r = place_at(WORK, (720, 300), (5000, 5000));
        assert_eq!(r, RectPx::new(1920 - 720, 1040 - 300, 1920, 1040));

        let r = place_at(WORK, (720, 300), (-4000, -4000));
        assert_eq!(r, RectPx::new(0, 0, 720, 300));
    }

    #[test]
    fn a_size_larger_than_the_screen_still_fills_it_rather_than_leaving_it() {
        let r = place_at(WORK, (99_999, 99_999), (600, 600));
        assert_eq!(r, RectPx::new(0, 0, 1920, 1040));
    }

    #[test]
    fn a_size_of_nothing_is_still_raised_to_something_that_can_be_seen() {
        let r = place_at(WORK, (0, 0), (100, 100));
        assert_eq!((r.width(), r.height()), MIN_PX);
    }

    #[test]
    fn a_tiny_screen_gets_a_remembered_window_that_fits_it() {
        let work = RectPx::new(0, 0, 200, 100);
        let r = place_at(work, (720, 300), (150, 90));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    /// Restoring the default placement is a fixed point, so the panel does not
    /// creep between summons.
    #[test]
    fn remembering_where_the_default_placement_put_it_changes_nothing() {
        let placed = place(WORK, (720, 300));
        let again = place_at(WORK, (720, 300), (placed.left, placed.top));
        assert_eq!(placed, again);
    }

    // --- docked against an edge ---------------------------------------------

    #[test]
    fn a_panel_docked_to_the_top_spans_the_work_area_along_its_top_edge() {
        let r = place_docked(WORK, 400, Edge::Top);
        assert_eq!(r, RectPx::new(0, 0, 1920, 400));
    }

    #[test]
    fn a_panel_docked_to_the_bottom_sits_on_the_taskbar_not_under_it() {
        let r = place_docked(WORK, 400, Edge::Bottom);
        assert_eq!(r, RectPx::new(0, 640, 1920, 1040));
    }

    #[test]
    fn a_taskbar_on_the_left_narrows_the_bar_rather_than_covering_it() {
        let work = RectPx::new(62, 0, 1920, 1080);
        let r = place_docked(work, 400, Edge::Top);
        assert_eq!((r.left, r.right), (62, 1920));
    }

    #[test]
    fn a_docked_panel_follows_the_monitor_it_was_given() {
        let work = RectPx::new(-1920, 0, 0, 1040);
        let r = place_docked(work, 400, Edge::Bottom);
        assert_eq!(r, RectPx::new(-1920, 640, 0, 1040));
    }

    #[test]
    fn a_docked_height_is_kept_between_nothing_and_the_whole_screen() {
        assert_eq!(place_docked(WORK, 99_999, Edge::Bottom), WORK);
        assert_eq!(place_docked(WORK, 0, Edge::Top).height(), MIN_PX.1);
    }
}
