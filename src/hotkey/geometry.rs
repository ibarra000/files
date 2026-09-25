//! Where the overlay goes, in pixels.
//!
//! Pure arithmetic, deliberately separated from [`super::win`] for the same
//! reason `index::watch` is separated from `index::win_watch`: none of the
//! Windows calls can be exercised on the machine this was written on, so the
//! part that *can* be tested has to be somewhere a test can reach it.
//!
//! # What used to be here
//!
//! A two-point fit of `client_px = cell_px × cells + chrome_px`, because the
//! thing being placed was a *terminal* and a terminal's client area is not a
//! whole number of character cells. Under Windows Terminal the tab row and its
//! padding live inside the client rect, so one measurement overestimated the
//! cell height by about five per cent and asking for twelve rows delivered ten.
//! The second sample came from the grid the terminal reported after the snap.
//!
//! All of that was machinery for finding out how big somebody else's window
//! was. The window is ours now: its size is a number this program chose, in
//! points it also chose, and the only thing left to work out is where to put
//! it. Two hundred lines and twelve tests went with the model, and none of the
//! behaviour did.

/// A rectangle in device pixels.
///
/// Device pixels rather than points, because this is the coordinate system
/// `SetWindowPos` and `GetMonitorInfoW` speak, and converting at the boundary
/// is one conversion rather than one per field.
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

/// Never smaller than this, whatever the arithmetic says.
///
/// A window that came out one pixel across cannot be found again or dragged,
/// and the panel is summoned by a shortcut somebody may not remember pressing.
pub const MIN_PX: (i32, i32) = (320, 120);

/// How far down the work area the top edge sits.
///
/// Not flush with the top: on a laptop that puts the overlay under the camera
/// notch, and over the band where a maximised window's title bar lives.
pub const TOP_FRACTION: f32 = 0.12;

/// Where the overlay goes.
///
/// `work` is the work area of the monitor the window is actually on - not the
/// primary monitor's, which is what `SPI_GETWORKAREA` would have given and is
/// how an overlay ends up off-screen for anyone with two displays.
///
/// The clamp order is the specification: size first, then the work area as a
/// hard ceiling, then the position. Doing it that way means a size that went
/// badly wrong produces at worst a full-screen window, and never one positioned
/// outside the desktop where it cannot be reached.
///
/// The height is the panel's height, which no longer changes once it is up:
/// the list scrolls inside a fixed window rather than growing it.
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

    // Nudge back inside rather than clipping, so the whole overlay is visible
    // even when the arithmetic above wanted it a little too low or too far
    // right.
    let left = left.clamp(work.left, (work.right - w).max(work.left));
    let top = top.clamp(work.top, (work.bottom - h).max(work.top));

    RectPx::new(left, top, left + w, top + h)
}

/// Where the overlay goes when somebody has already put it somewhere.
///
/// The same clamp order as [`place`], and the whole of the safety argument is
/// that it is the same: size first, then the work area as a hard ceiling, then
/// the position. `at` is the remembered top-left corner, and it is a *request*
/// rather than an instruction - a rectangle saved against a monitor that has
/// since been unplugged, or one saved on a machine with a taller taskbar, is
/// nudged back inside the work area rather than honoured off the edge.
///
/// Note what is deliberately *not* here: no check for whether the position is
/// "close enough" to be worth restoring, and no fallback to [`place`]. Either
/// would mean a panel that sometimes returns to where it was left and sometimes
/// does not, with the difference decided by arithmetic the user cannot see.
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

/// Where a docked panel goes: the full width of the work area, `height` tall,
/// flush against `edge`.
///
/// The work area rather than the monitor, for the reason [`place`] gives -
/// and here it is the whole feature, because the bottom of the monitor is
/// where the taskbar is. Same clamp as the floating placements, so a height
/// that went wrong fills the screen at worst.
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

    /// Not flush with the top, and not in the middle either: a search box in
    /// the vertical centre is one the eye has to travel to.
    #[test]
    fn the_overlay_sits_in_the_upper_part_of_the_work_area() {
        let r = place(WORK, (720, 300));
        let expected = ((1040 - 300) as f32 * TOP_FRACTION) as i32;
        assert_eq!(r.top, expected);
        assert!(r.top > 0, "flush with the top edge");
        assert!(r.top < 1040 / 3, "too far down to be glanced at");
    }

    /// The work area, not the monitor: a taskbar on the left moves the centre.
    #[test]
    fn the_overlay_is_placed_within_the_work_area_it_was_given() {
        let work = RectPx::new(-1920, 200, 0, 1000);
        let r = place(work, (720, 300));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    /// The clamp order is the whole safety argument: a size that went wrong
    /// gives at worst a full-screen window, never an unreachable one.
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

    /// A negative size is not reachable from the panel, which is why the clamp
    /// has to be the thing that says so rather than a comment upstream.
    #[test]
    fn a_nonsensical_size_is_still_placed_on_the_screen() {
        let r = place(WORK, (-500, -500));
        assert!(r.width() >= MIN_PX.0.min(WORK.width()));
        assert!(r.height() >= MIN_PX.1.min(WORK.height()));
        assert!(r.left >= WORK.left && r.top >= WORK.top);
    }

    /// A work area smaller than the minimum is a real configuration - a
    /// 320-pixel-wide remote session - and the panel has to land inside it
    /// rather than hanging off the edge because a constant said so.
    #[test]
    fn a_tiny_screen_gets_a_window_that_fits_it() {
        let work = RectPx::new(0, 0, 200, 100);
        let r = place(work, (720, 300));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    /// The panel grows downward, so the top edge must not move when it does.
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

    // --- a position somebody chose ------------------------------------------

    /// The point of the whole feature: what was asked for is what is used.
    #[test]
    fn a_remembered_position_is_used_as_it_stands() {
        let r = place_at(WORK, (720, 300), (40, 700));
        assert_eq!(r, RectPx::new(40, 700, 760, 1000));
    }

    /// A second monitor to the left of the primary one has negative
    /// coordinates, and that is an ordinary layout rather than a damaged value.
    #[test]
    fn a_position_on_a_monitor_left_of_the_primary_one_is_honoured() {
        let work = RectPx::new(-1920, 0, 0, 1040);
        let r = place_at(work, (720, 300), (-1500, 120));
        assert_eq!(r, RectPx::new(-1500, 120, -780, 420));
    }

    /// The monitor it was saved against is gone, so the work area it is being
    /// placed into no longer contains it. It must land on screen.
    #[test]
    fn a_position_off_the_work_area_is_nudged_fully_inside_it() {
        let r = place_at(WORK, (720, 300), (5000, 5000));
        assert_eq!(r, RectPx::new(1920 - 720, 1040 - 300, 1920, 1040));

        let r = place_at(WORK, (720, 300), (-4000, -4000));
        assert_eq!(r, RectPx::new(0, 0, 720, 300));
    }

    /// The clamp order is the same argument [`place`] makes: a size that went
    /// wrong gives at worst a full-screen window, never an unreachable one, and
    /// a remembered position cannot talk it out of that.
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

    /// A work area narrower than the panel - a small remote session - must
    /// still produce a rectangle that starts inside it.
    #[test]
    fn a_tiny_screen_gets_a_remembered_window_that_fits_it() {
        let work = RectPx::new(0, 0, 200, 100);
        let r = place_at(work, (720, 300), (150, 90));
        assert!(r.left >= work.left && r.right <= work.right, "{r:?}");
        assert!(r.top >= work.top && r.bottom <= work.bottom, "{r:?}");
    }

    /// Placing a panel and then remembering exactly where it landed must be a
    /// fixed point, or the panel would creep a little on every summon.
    #[test]
    fn remembering_where_the_default_placement_put_it_changes_nothing() {
        let placed = place(WORK, (720, 300));
        let again = place_at(WORK, (720, 300), (placed.left, placed.top));
        assert_eq!(placed, again);
    }

    // --- docked against an edge ---------------------------------------------

    /// A bar the width of the screen, and the height it was asked for.
    #[test]
    fn a_panel_docked_to_the_top_spans_the_work_area_along_its_top_edge() {
        let r = place_docked(WORK, 400, Edge::Top);
        assert_eq!(r, RectPx::new(0, 0, 1920, 400));
    }

    /// The work area's bottom, which is the top of the taskbar - so a bottom
    /// bar sits on the taskbar rather than behind it.
    #[test]
    fn a_panel_docked_to_the_bottom_sits_on_the_taskbar_not_under_it() {
        let r = place_docked(WORK, 400, Edge::Bottom);
        assert_eq!(r, RectPx::new(0, 640, 1920, 1040));
    }

    /// A taskbar down the left side moves the work area's left edge, and the
    /// bar has to start there rather than at the monitor's.
    #[test]
    fn a_taskbar_on_the_left_narrows_the_bar_rather_than_covering_it() {
        let work = RectPx::new(62, 0, 1920, 1080);
        let r = place_docked(work, 400, Edge::Top);
        assert_eq!((r.left, r.right), (62, 1920));
    }

    /// And a second monitor is its own work area, negative coordinates and all.
    #[test]
    fn a_docked_panel_follows_the_monitor_it_was_given() {
        let work = RectPx::new(-1920, 0, 0, 1040);
        let r = place_docked(work, 400, Edge::Bottom);
        assert_eq!(r, RectPx::new(-1920, 640, 0, 1040));
    }

    /// The same clamp as the floating placement: never taller than the screen,
    /// and never so short it cannot be seen.
    #[test]
    fn a_docked_height_is_kept_between_nothing_and_the_whole_screen() {
        assert_eq!(place_docked(WORK, 99_999, Edge::Bottom), WORK);
        assert_eq!(place_docked(WORK, 0, Edge::Top).height(), MIN_PX.1);
    }
}
