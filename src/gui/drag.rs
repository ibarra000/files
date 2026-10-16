//! Moving the panel with the pointer.
//!
//! Pure arithmetic, separate from the toolkit so it can be tested, as
//! [`crate::hotkey::geometry`] is.
//!
//! # Accumulated position
//!
//! `ViewportInfo::outer_rect` lags one frame behind the commanded position, so
//! adding the pointer delta to it double-counts and the panel judders. Instead
//! this stores the position it last commanded. The pointer is window-local, so
//! `outer + pointer` is its desktop position and holding that fixed gives
//!
//! ```text
//! outer' = outer + (pointer - grab)
//! ```
//!
//! which settles exactly: once the window catches up, `pointer == grab`.
//!
//! # Not `ViewportCommand::StartDrag`
//!
//! `StartDrag` enters the modal `WM_NCLBUTTONDOWN` move loop, which blocks the
//! event loop that draws the panel, and is not testable.

use eframe::egui::{Pos2, Vec2};

/// Distance in points the pointer must travel before a press becomes a move.
///
/// Prevents a slightly wobbling click from being saved to `%APPDATA%` as a
/// custom position, which would stop automatic placement.
pub const SLOP: f32 = 2.0;

/// Where the gesture started.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    /// The pointer's position within the window at the press, in points.
    grab: Pos2,
    /// The last commanded outer position, in points. See the module docs.
    outer: Pos2,
}

/// A panel drag, in progress or not.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Drag {
    anchor: Option<Anchor>,
    /// Whether the pointer has passed [`SLOP`].
    moved: bool,
}

impl Drag {
    pub const fn new() -> Self {
        Self {
            anchor: None,
            moved: false,
        }
    }

    /// Starts a gesture.
    ///
    /// `outer` is the window's current position and `grab` the pointer's
    /// position within it, both in points (egui's unit and the unit
    /// `ViewportCommand::OuterPosition` expects).
    pub fn begin(&mut self, grab: Pos2, outer: Pos2) {
        self.anchor = Some(Anchor { grab, outer });
        self.moved = false;
    }

    /// Returns the position to command this frame.
    ///
    /// `None` when no gesture is active or the pointer is still within
    /// [`SLOP`].
    pub fn update(&mut self, pointer: Pos2) -> Option<Pos2> {
        let anchor = self.anchor.as_mut()?;
        let delta: Vec2 = pointer - anchor.grab;
        if !self.moved {
            if delta.length() < SLOP {
                return None;
            }
            self.moved = true;
        }
        anchor.outer += delta;
        Some(anchor.outer)
    }

    /// Ends the gesture and returns the final position.
    ///
    /// `None` when the gesture never passed [`SLOP`].
    pub fn end(&mut self) -> Option<Pos2> {
        let anchor = self.anchor.take()?;
        let moved = self.moved;
        self.moved = false;
        moved.then_some(anchor.outer)
    }

    /// Whether a drag is active and has passed [`SLOP`].
    ///
    /// Lets callers suppress hover highlights during a drag but not a click.
    pub fn is_dragging(&self) -> bool {
        self.anchor.is_some() && self.moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn the_window_follows_the_pointer_exactly() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        let at = drag.update(pos2(110.0, 20.0)).expect("past the slop");
        assert_eq!(at, pos2(510.0, 300.0));
    }

    /// Once the window catches up the pointer is back at the grab point and the
    /// position must stay put.
    #[test]
    fn a_window_that_has_caught_up_stops_moving() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        assert_eq!(
            drag.update(pos2(140.0, 20.0)),
            Some(pos2(540.0, 300.0)),
            "the first frame moves by the pointer delta"
        );
        assert_eq!(
            drag.update(pos2(100.0, 20.0)),
            Some(pos2(540.0, 300.0)),
            "the window caught up, so the panel must stand still"
        );
        assert_eq!(drag.update(pos2(100.0, 20.0)), Some(pos2(540.0, 300.0)));
    }

    #[test]
    fn successive_movements_accumulate() {
        let mut drag = Drag::new();
        drag.begin(pos2(0.0, 0.0), pos2(0.0, 0.0));

        drag.update(pos2(10.0, 0.0));
        drag.update(pos2(0.0, 10.0));
        let at = drag.update(pos2(0.0, 0.0));
        assert_eq!(at, Some(pos2(10.0, 10.0)));
    }

    #[test]
    fn a_press_that_barely_moves_is_not_a_drag() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        assert_eq!(drag.update(pos2(101.0, 20.5)), None);
        assert!(!drag.is_dragging());
        assert_eq!(drag.end(), None, "a wobble was reported as a move");
    }

    #[test]
    fn a_drag_that_returns_to_where_it_started_is_still_a_drag() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        drag.update(pos2(200.0, 20.0));
        assert!(drag.is_dragging());
        drag.update(pos2(0.0, 20.0));

        assert_eq!(
            drag.end(),
            Some(pos2(500.0, 300.0)),
            "the panel is back where it began, but the gesture happened"
        );
    }

    #[test]
    fn nothing_happens_without_a_grab() {
        let mut drag = Drag::new();
        assert_eq!(drag.update(pos2(10.0, 10.0)), None);
        assert_eq!(drag.end(), None);
        assert!(!drag.is_dragging());
    }

    #[test]
    fn letting_go_clears_the_gesture() {
        let mut drag = Drag::new();
        drag.begin(pos2(0.0, 0.0), pos2(100.0, 100.0));
        drag.update(pos2(50.0, 0.0));
        assert_eq!(drag.end(), Some(pos2(150.0, 100.0)));

        assert!(!drag.is_dragging());
        drag.begin(pos2(0.0, 0.0), pos2(150.0, 100.0));
        assert!(!drag.is_dragging(), "the new gesture is not a move yet");
        assert_eq!(drag.update(pos2(10.0, 0.0)), Some(pos2(160.0, 100.0)));
    }
}
