//! The one place screen pixels become egui points, and back.
//!
//! The overlay is a single window spanning the whole virtual desktop, and that
//! is what makes this delicate. egui hands out positions in *logical points*
//! relative to the window's top-left, scaled by one factor for the whole
//! window - but a virtual desktop made of a 150% laptop panel and a 100%
//! external monitor has no single factor, so that number is right on at most
//! one of them.
//!
//! The rule that follows, and the reason everything in [`super::hit`] is in
//! [`Screen`] pixels: **hit-testing never happens in logical points.** A
//! position arriving from egui is converted here, once, at the edge; a
//! rectangle going back to egui to be painted is converted here, once, on the
//! way out. Nothing in between knows egui exists.
//!
//! This is a different conversion from the one `capture::geom` makes, and they
//! are kept apart on purpose. That one maps a captured window's pixels onto the
//! screen; this one maps the screen onto the sheet of glass drawn over it.
//! Folding them together would produce a single function with two scale factors
//! in it, which is precisely the shape point 4 warns about.

use crate::lens::px::{Point, Rect, Screen};

/// Where the overlay window is, and what egui is scaling by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// The window's top-left in physical screen pixels. Negative on a desktop
    /// with a monitor left of or above the primary one, which is ordinary.
    pub origin: Point<Screen>,
    /// egui's points-to-pixels factor for this window.
    pub scale: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            origin: Point::new(0, 0),
            scale: 1.0,
        }
    }
}

impl Viewport {
    pub fn new(origin: Point<Screen>, scale: f32) -> Self {
        // A scale of zero or worse would put every coordinate at the origin or
        // at NaN, and the symptom would be a selection that silently never
        // matches anything. Refused here rather than propagated.
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self { origin, scale }
    }

    /// A position from egui, in physical screen pixels.
    pub fn to_screen(self, x: f32, y: f32) -> Point<Screen> {
        Point::new(
            self.origin.x + (x * self.scale).round() as i32,
            self.origin.y + (y * self.scale).round() as i32,
        )
    }

    /// A screen rectangle, in the logical points egui paints with.
    pub fn to_points(self, r: Rect<Screen>) -> (f32, f32, f32, f32) {
        let x = |v: i32| (v - self.origin.x) as f32 / self.scale;
        let y = |v: i32| (v - self.origin.y) as f32 / self.scale;
        (x(r.left), y(r.top), x(r.right), y(r.bottom))
    }

    /// The window size in logical points needed to span `span` physical pixels.
    pub fn size_in_points(self, width: i32, height: i32) -> (f32, f32) {
        (width as f32 / self.scale, height as f32 / self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_one_hundred_percent_a_point_is_a_pixel() {
        let v = Viewport::new(Point::new(0, 0), 1.0);
        assert_eq!(v.to_screen(10.0, 20.0), Point::new(10, 20));
    }

    /// The case the whole file exists for. A window whose origin is not the
    /// primary monitor's, at a scale that is not one.
    #[test]
    fn an_offset_origin_and_a_scale_are_both_applied() {
        let v = Viewport::new(Point::new(-1920, -200), 1.5);
        assert_eq!(v.to_screen(0.0, 0.0), Point::new(-1920, -200));
        assert_eq!(v.to_screen(100.0, 100.0), Point::new(-1770, -50));
    }

    /// A monitor to the left of the primary one has negative coordinates, and
    /// they are not a bug to be clamped away.
    #[test]
    fn a_monitor_left_of_the_primary_one_keeps_its_negative_coordinates() {
        let v = Viewport::new(Point::new(-1920, 0), 1.0);
        let p = v.to_screen(10.0, 10.0);
        assert!(p.x < 0, "{p:?} should still be on the left-hand monitor");
    }

    /// The round trip a selection depends on: a rectangle handed to egui to be
    /// painted, read back, is the rectangle that was hit-tested.
    #[test]
    fn a_rectangle_survives_the_trip_to_points_and_back() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let v = Viewport::new(Point::new(-1920, -1080), scale);
            let r = Rect::<Screen>::new(-1800, -1000, -1700, -980);
            let (l, t, right, b) = v.to_points(r);
            assert_eq!(v.to_screen(l, t), Point::new(r.left, r.top));
            assert_eq!(v.to_screen(right, b), Point::new(r.right, r.bottom));
        }
    }

    /// A scale that is zero, negative or NaN would put every coordinate at the
    /// origin, and the only symptom would be selection silently never matching.
    #[test]
    fn a_nonsense_scale_falls_back_to_one_rather_than_poisoning_every_point() {
        for bad in [0.0, -1.5, f32::NAN, f32::INFINITY] {
            let v = Viewport::new(Point::new(0, 0), bad);
            assert_eq!(v.scale, 1.0, "{bad} should not have been accepted");
            assert_eq!(v.to_screen(10.0, 10.0), Point::new(10, 10));
        }
    }

    #[test]
    fn the_window_is_sized_in_points_but_spans_pixels() {
        let v = Viewport::new(Point::new(0, 0), 2.0);
        assert_eq!(v.size_in_points(3840, 2160), (1920.0, 1080.0));
    }
}
