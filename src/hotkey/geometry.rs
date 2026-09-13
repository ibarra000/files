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
/// The height is the panel's height *at the moment it is summoned*, and the top
/// edge is fixed from it. The panel then grows downward as results arrive,
/// which is both the stable choice and the legible one: a list that grows by
/// pushing its own search box up the screen is a list nobody can read while it
/// is arriving.
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
}
