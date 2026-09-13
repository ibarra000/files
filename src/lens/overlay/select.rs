//! The interaction: modifier, pointer, drag, selection.
//!
//! Pure, and deliberately so. Point 1 of the specification - the overlay
//! ignores the mouse until a modifier is held, and catches it while it is - is
//! the entire mode mechanism of the feature, and it is two booleans and a
//! window style bit. Keeping the decision here rather than in the window means
//! it can be exercised without one, which is the same arrangement
//! [`crate::app::state`] makes against the terminal.
//!
//! Nothing here calls Windows and nothing here paints. [`Selection::catches_mouse`]
//! is read by the window and turned into `WS_EX_TRANSPARENT`; [`Selection::cursor`]
//! is read by the window and turned into a cursor. Neither decision is taken twice.

use crate::lens::overlay::hit::{BoxMap, Caret};
use crate::lens::px::{Point, Rect, Screen};

/// What the pointer should look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// Whatever the application underneath asked for. The overlay is not
    /// catching the mouse, so this is not really our cursor at all.
    Arrow,
    /// There is text here and it can be selected.
    IBeam,
}

/// The interaction state of the overlay.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    map: BoxMap,
    /// Whether the modifier is held. The whole mode mechanism.
    armed: bool,
    pointer: Option<Point<Screen>>,
    /// Anchor and head, while the button is down.
    drag: Option<(Caret, Caret)>,
    /// Anchor and head of a finished drag, which is what copy and search act
    /// on. Kept after the button comes up so the user can reach for a chip.
    done: Option<(Caret, Caret)>,
}

impl Selection {
    pub fn new(map: BoxMap) -> Self {
        Self {
            map,
            ..Default::default()
        }
    }

    pub fn map(&self) -> &BoxMap {
        &self.map
    }

    /// Replaces what is readable on screen.
    ///
    /// Point 9: scrolling changes a window's content without telling anybody,
    /// so the map is rebuilt from a fresh capture on every invocation and
    /// arrives here. Any selection in progress refers to text that has moved
    /// and is dropped rather than reinterpreted - a highlight left over a
    /// scrolled window is pointing at the wrong words, which is worse than no
    /// highlight at all.
    pub fn replace_map(&mut self, map: BoxMap) {
        self.map = map;
        self.drag = None;
        self.done = None;
    }

    /// The modifier went down or came up. This is the mode.
    ///
    /// A release always ends the interaction: letting go of the modifier with
    /// the button still down would otherwise leave a drag that the overlay can
    /// no longer see the mouse to finish.
    pub fn modifier(&mut self, down: bool) {
        if self.armed == down {
            return;
        }
        self.armed = down;
        if !down {
            self.drag = None;
            self.done = None;
            self.pointer = None;
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Whether the overlay window should be catching the mouse.
    ///
    /// Read straight into `WS_EX_TRANSPARENT`. False by default, and false is
    /// the important half: an overlay that catches the mouse when it was not
    /// asked to is an overlay that has taken the desktop away.
    pub fn catches_mouse(&self) -> bool {
        self.armed
    }

    pub fn pointer_moved(&mut self, p: Point<Screen>) {
        self.pointer = Some(p);
        if let Some((_, head)) = &mut self.drag {
            // Never `caret_at`: once a drag is under way the head has to
            // resolve wherever the pointer is, including off the end of the
            // text and off the bottom of the screen.
            if let Some(found) = self.map.nearest_caret(p) {
                *head = found;
            }
        }
    }

    /// The button went down. Answers whether that began a selection.
    ///
    /// A press on empty space begins nothing, and the `false` matters: the
    /// window uses it to decide the click was not for us. Clicks that select
    /// nothing should reach the application underneath rather than being
    /// swallowed by a sheet of glass.
    pub fn press(&mut self, p: Point<Screen>) -> bool {
        self.pointer = Some(p);
        self.done = None;
        if !self.armed {
            return false;
        }
        match self.map.caret_at(p) {
            Some(caret) => {
                self.drag = Some((caret, caret));
                true
            }
            None => {
                self.drag = None;
                false
            }
        }
    }

    /// The button came up. The selection stays, so it can be acted on.
    pub fn release(&mut self) {
        if let Some((anchor, head)) = self.drag.take()
            && anchor != head
        {
            self.done = Some((anchor, head));
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The carets currently worth drawing: the live drag, else the finished one.
    fn current(&self) -> Option<(Caret, Caret)> {
        self.drag.or(self.done)
    }

    /// What the cursor should be, right now.
    ///
    /// **Point 3.** This answers from the box map alone. A box is in the map as
    /// soon as detection has found it, long before anything has been read out
    /// of it, and the I-beam must never wait on recognition - the user's own
    /// reaction time between seeing the cursor change and finishing a drag is
    /// where recognition is meant to happen.
    pub fn cursor(&self) -> Cursor {
        if !self.armed {
            return Cursor::Arrow;
        }
        if self.drag.is_some() {
            return Cursor::IBeam;
        }
        match self.pointer {
            Some(p) if self.map.is_text_at(p) => Cursor::IBeam,
            _ => Cursor::Arrow,
        }
    }

    /// The rectangles to paint over the selection.
    pub fn highlight(&self) -> Vec<Rect<Screen>> {
        match self.current() {
            Some((a, b)) => self.map.highlight(a, b),
            None => Vec::new(),
        }
    }

    /// The selected text, for the clipboard and for the search box.
    pub fn text(&self) -> String {
        match self.current() {
            Some((a, b)) => self.map.run_between(a, b),
            None => String::new(),
        }
    }

    /// Whether there is a finished selection worth offering actions for.
    pub fn has_selection(&self) -> bool {
        self.done.is_some() && !self.text().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens::fixture;

    fn sel() -> Selection {
        Selection::new(fixture::title_block(Point::new(100, 100)))
    }

    /// Where on screen the first line's text starts and ends.
    fn first_line(s: &Selection) -> (Point<Screen>, Point<Screen>) {
        let line = &s.map().lines[0];
        let y = line.rect.top + 8;
        (
            Point::new(line.rect.left, y),
            Point::new(line.rect.right, y),
        )
    }

    /// Point 1, and the half that matters: by default the overlay is not there
    /// as far as the mouse is concerned.
    #[test]
    fn the_overlay_ignores_the_mouse_until_the_modifier_is_held() {
        let mut s = sel();
        assert!(!s.catches_mouse(), "idle overlays are click-through");
        s.modifier(true);
        assert!(s.catches_mouse(), "and catch the mouse while held");
        s.modifier(false);
        assert!(!s.catches_mouse(), "and let go again");
    }

    /// Point 3. The cursor is decided by detection alone, so this is true with
    /// a map that has never been near a recogniser.
    #[test]
    fn the_i_beam_appears_over_text_only_while_armed() {
        let mut s = sel();
        let (start, _) = first_line(&s);

        s.pointer_moved(start);
        assert_eq!(s.cursor(), Cursor::Arrow, "not armed, so not our cursor");

        s.modifier(true);
        s.pointer_moved(start);
        assert_eq!(s.cursor(), Cursor::IBeam);

        s.pointer_moved(Point::new(5000, 5000));
        assert_eq!(s.cursor(), Cursor::Arrow, "away from any text");
    }

    #[test]
    fn a_drag_across_a_line_selects_it() {
        let mut s = sel();
        let (start, end) = first_line(&s);
        s.modifier(true);
        assert!(s.press(start));
        s.pointer_moved(end);
        s.release();
        assert_eq!(s.text(), "DRAWING No. 11-D-0704");
        assert!(s.has_selection());
        assert_eq!(s.highlight().len(), 1);
    }

    /// The selection survives the button coming up, or there would be nothing
    /// left to copy by the time the user reached for the chip.
    #[test]
    fn the_selection_outlives_the_button() {
        let mut s = sel();
        let (start, end) = first_line(&s);
        s.modifier(true);
        s.press(start);
        s.pointer_moved(end);
        assert!(s.is_dragging());
        s.release();
        assert!(!s.is_dragging());
        assert!(!s.text().is_empty());
    }

    /// A press that selects nothing must be reported as such, so the click can
    /// go to the window underneath instead of into a sheet of glass.
    #[test]
    fn a_press_on_empty_space_is_not_ours() {
        let mut s = sel();
        s.modifier(true);
        assert!(!s.press(Point::new(5000, 5000)));
        assert!(!s.is_dragging());
        assert_eq!(s.text(), "");
    }

    /// And a press with no modifier is never ours, wherever it lands.
    #[test]
    fn a_press_without_the_modifier_is_never_ours() {
        let mut s = sel();
        let (start, _) = first_line(&s);
        assert!(!s.press(start));
        assert!(!s.is_dragging());
    }

    /// Letting go of the modifier mid-drag has to end the drag: the overlay
    /// stops seeing the mouse at that instant, so a drag left open could never
    /// be finished.
    #[test]
    fn releasing_the_modifier_abandons_a_drag_in_progress() {
        let mut s = sel();
        let (start, end) = first_line(&s);
        s.modifier(true);
        s.press(start);
        s.pointer_moved(end);
        s.modifier(false);
        assert!(!s.is_dragging());
        assert_eq!(s.text(), "");
        assert_eq!(s.cursor(), Cursor::Arrow);
        assert!(s.highlight().is_empty());
    }

    /// Point 9. The window scrolled, so the capture was re-hashed and a new map
    /// arrived. The old carets point at text that has moved, and a highlight
    /// over the wrong words is worse than none.
    #[test]
    fn a_new_box_map_drops_the_selection_rather_than_moving_it() {
        let mut s = sel();
        let (start, end) = first_line(&s);
        s.modifier(true);
        s.press(start);
        s.pointer_moved(end);
        s.release();
        assert!(s.has_selection());

        s.replace_map(fixture::title_block(Point::new(100, 40)));
        assert!(!s.has_selection());
        assert_eq!(s.text(), "");
        assert!(s.highlight().is_empty());
    }

    /// A click with no movement selects nothing, the way a click in any other
    /// text control does - it must not leave a zero-width selection that the
    /// action chips would then offer to copy.
    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        let mut s = sel();
        let (start, _) = first_line(&s);
        s.modifier(true);
        s.press(start);
        s.release();
        assert!(!s.has_selection());
        assert_eq!(s.text(), "");
    }

    /// Dragging backwards is the same selection, which falls out of the caret
    /// model rather than needing a case here.
    #[test]
    fn dragging_right_to_left_selects_the_same_text() {
        let (mut a, mut b) = (sel(), sel());
        let (start, end) = first_line(&a);

        a.modifier(true);
        a.press(start);
        a.pointer_moved(end);
        a.release();

        b.modifier(true);
        b.press(end);
        b.pointer_moved(start);
        b.release();

        assert_eq!(a.text(), b.text());
        assert!(!a.text().is_empty());
    }

    /// The gesture the feature exists for, end to end and with no screen.
    #[test]
    fn a_job_code_can_be_dragged_out_of_a_title_block() {
        let mut s = sel();
        let line = &s.map().lines[0];
        let y = line.rect.top + 8;
        let from = Point::new(line.x_of("DRAWING No. ".chars().count()), y);
        let to = Point::new(line.rect.right, y);

        s.modifier(true);
        assert!(s.press(from));
        s.pointer_moved(to);
        assert_eq!(s.cursor(), Cursor::IBeam);
        s.release();
        assert_eq!(s.text(), "11-D-0704");
    }
}
