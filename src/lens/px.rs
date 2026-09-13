//! Points and rectangles that know which space they are in.
//!
//! Point 4 of the specification is the reason this file exists rather than a
//! plain `RectPx`. Screen pixels, capture pixels and egui's logical points are
//! three different spaces, they hold the same numbers on a single 100% monitor,
//! and a confusion between them is invisible until somebody drags a window
//! between a 150% laptop panel and a 100% external one. Testing for that is
//! possible but has to be remembered; making it a type error does not.
//!
//! So the space is a type parameter carried by a zero-sized marker. The
//! representation is unchanged: `Rect<Screen>` is four `i32`s and nothing else.
//! The only way between two spaces is `capture::geom::Mapping`, which is the
//! single place the scale factor and the origin are ever applied.
//!
//! The guarantee, which is the whole point of the file. A rectangle from one
//! space is not a rectangle from another:
//!
//! ```compile_fail
//! use files::lens::px::{Capture, Rect, Screen};
//! fn wants_screen(_: Rect<Screen>) {}
//! wants_screen(Rect::<Capture>::new(0, 0, 1, 1));
//! ```
//!
//! and a point from one space cannot be tested against a rectangle in another:
//!
//! ```compile_fail
//! use files::lens::px::{Capture, Point, Rect, Screen};
//! Rect::<Screen>::new(0, 0, 10, 10).contains(Point::<Capture>::new(1, 1));
//! ```
//!
//! while within a single space everything is ordinary:
//!
//! ```
//! use files::lens::px::{Point, Rect, Screen};
//! assert!(Rect::<Screen>::new(0, 0, 10, 10).contains(Point::new(5, 5)));
//! ```
//!
//! Those three blocks are the test. They are in the module documentation rather
//! than in the test module below because `rustdoc` does not collect doctests
//! from `#[cfg(test)]` code - written there they compile as part of nothing,
//! run as part of nothing, and prove exactly nothing while appearing to.
//!
//! Deliberately *not* a reuse of [`crate::hotkey::geometry::RectPx`]. That type
//! is the right one for what it does - one window, one monitor, one space, no
//! ambiguity to protect against - and giving it a type parameter would churn
//! working code to buy it a guarantee it has no use for.

use std::marker::PhantomData;

/// Physical pixels on the virtual desktop, origin at the top-left of the
/// primary monitor. Negative coordinates are ordinary: a monitor placed to the
/// left of the primary one has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Screen;

/// Physical pixels within one captured window, origin at its top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Capture;

/// A point in the space `S`.
pub struct Point<S> {
    pub x: i32,
    pub y: i32,
    space: PhantomData<S>,
}

/// A rectangle in the space `S`, half-open: `left <= x < right`.
///
/// Half-open because adjacent line boxes share an edge, and a closed rectangle
/// would make the shared pixel column belong to both - so a click on it would
/// select whichever happened to be tested first.
pub struct Rect<S> {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    space: PhantomData<S>,
}

// `derive` would demand `S: Clone`, `S: Debug` and so on, which the markers
// have no reason to satisfy and which would infect every signature that
// mentions one. Written out, the bounds are on the coordinates alone.
impl<S> Clone for Point<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Point<S> {}
impl<S> Clone for Rect<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Rect<S> {}

impl<S> PartialEq for Point<S> {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y
    }
}
impl<S> Eq for Point<S> {}

impl<S> PartialEq for Rect<S> {
    fn eq(&self, other: &Self) -> bool {
        self.left == other.left
            && self.top == other.top
            && self.right == other.right
            && self.bottom == other.bottom
    }
}
impl<S> Eq for Rect<S> {}

impl<S> std::fmt::Debug for Point<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

impl<S> std::fmt::Debug for Rect<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}, {} .. {}, {}]",
            self.left, self.top, self.right, self.bottom
        )
    }
}

impl<S> Point<S> {
    pub const fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            space: PhantomData,
        }
    }
}

impl<S> Rect<S> {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            space: PhantomData,
        }
    }

    /// From a corner and a size, which is the shape every Windows API hands
    /// back.
    pub const fn at(origin: Point<S>, width: i32, height: i32) -> Self {
        Self::new(origin.x, origin.y, origin.x + width, origin.y + height)
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }

    /// True when the rectangle encloses no pixels.
    ///
    /// Worth having by name: a detector that reports a zero-width box is not a
    /// crash, and a caller that treats it as selectable produces an empty
    /// selection the user can neither see nor dismiss.
    pub const fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub const fn contains(self, p: Point<S>) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    pub const fn center(self) -> Point<S> {
        Point::new(self.left + self.width() / 2, self.top + self.height() / 2)
    }

    /// The squared distance from `p` to the nearest pixel of this rectangle,
    /// and zero when it is inside.
    ///
    /// Squared, so the cursor-outward ordering of point 7 never takes a square
    /// root. `i64` because a span of the virtual desktop squared overflows an
    /// `i32` beyond about 46_000 pixels, which four 4K monitors side by side
    /// reach.
    pub const fn distance2(self, p: Point<S>) -> i64 {
        let dx = if p.x < self.left {
            self.left - p.x
        } else if p.x >= self.right {
            p.x - self.right + 1
        } else {
            0
        } as i64;
        let dy = if p.y < self.top {
            self.top - p.y
        } else if p.y >= self.bottom {
            p.y - self.bottom + 1
        } else {
            0
        } as i64;
        dx * dx + dy * dy
    }

    /// The smallest rectangle holding both. Used to draw one highlight over a
    /// run of words rather than one highlight per word.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half-open rule, which is what stops a shared edge belonging to two
    /// adjacent line boxes at once.
    #[test]
    fn the_right_and_bottom_edges_are_outside() {
        let r = Rect::<Screen>::new(10, 20, 30, 40);
        assert!(r.contains(Point::new(10, 20)), "the top-left is inside");
        assert!(!r.contains(Point::new(30, 30)), "the right edge is not");
        assert!(!r.contains(Point::new(20, 40)), "nor the bottom edge");
        assert!(r.contains(Point::new(29, 39)), "the last pixel is");
    }

    /// Two boxes laid side by side must not both claim the column between them.
    #[test]
    fn adjacent_boxes_never_both_contain_the_shared_column() {
        let a = Rect::<Screen>::new(0, 0, 10, 10);
        let b = Rect::<Screen>::new(10, 0, 20, 10);
        for y in 0..10 {
            for x in 0..20 {
                let p = Point::new(x, y);
                assert!(!(a.contains(p) && b.contains(p)), "{p:?} is in both boxes");
            }
        }
    }

    #[test]
    fn a_point_inside_is_at_no_distance() {
        let r = Rect::<Screen>::new(0, 0, 10, 10);
        assert_eq!(r.distance2(Point::new(5, 5)), 0);
        assert_eq!(r.distance2(Point::new(0, 0)), 0);
        assert_eq!(r.distance2(Point::new(9, 9)), 0);
    }

    /// The pixel just outside an edge is one away, not zero - which is the
    /// off-by-one that would make the cursor-outward order of point 7 start in
    /// the wrong place.
    #[test]
    fn the_pixel_beyond_an_edge_is_one_away() {
        let r = Rect::<Screen>::new(0, 0, 10, 10);
        assert_eq!(r.distance2(Point::new(10, 5)), 1);
        assert_eq!(r.distance2(Point::new(-1, 5)), 1);
        assert_eq!(r.distance2(Point::new(5, 10)), 1);
        assert_eq!(r.distance2(Point::new(5, -1)), 1);
        // And the diagonal is the sum of the two.
        assert_eq!(r.distance2(Point::new(10, 10)), 2);
    }

    /// Four 4K monitors side by side is some 15_000 pixels, and the square of
    /// that does not fit in an `i32`. This is the arithmetic that must not wrap.
    #[test]
    fn a_distance_across_the_whole_desktop_does_not_overflow() {
        let r = Rect::<Screen>::new(0, 0, 10, 10);
        let far = Point::new(60_000, 60_000);
        assert!(r.distance2(far) > i32::MAX as i64);
    }

    #[test]
    fn a_union_covers_both_and_ignores_empty_ones() {
        let a = Rect::<Screen>::new(0, 0, 10, 10);
        let b = Rect::<Screen>::new(20, 5, 30, 15);
        assert_eq!(a.union(b), Rect::new(0, 0, 30, 15));
        let nothing = Rect::<Screen>::new(0, 0, 0, 0);
        assert_eq!(a.union(nothing), a);
        assert_eq!(nothing.union(a), a);
    }

    #[test]
    fn a_rectangle_with_no_area_is_empty() {
        assert!(Rect::<Screen>::new(5, 5, 5, 10).is_empty());
        assert!(Rect::<Screen>::new(5, 5, 10, 5).is_empty());
        assert!(!Rect::<Screen>::new(5, 5, 6, 6).is_empty());
    }

    #[test]
    fn at_builds_from_a_corner_and_a_size() {
        let r = Rect::<Capture>::at(Point::new(3, 7), 10, 20);
        assert_eq!(r, Rect::new(3, 7, 13, 27));
        assert_eq!(r.width(), 10);
        assert_eq!(r.height(), 20);
    }
}
