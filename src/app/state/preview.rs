//! Which file the pane is describing, and when to go and ask.
//!
//! # The target is the hover, then the selection
//!
//! `pointer.rs` holds the rule that a hover is never a selection: Enter opens
//! the selection, so a highlight that could be mistaken for one gets a file
//! opened by accident. That rule is about *what Enter does* and this is not -
//! nothing here opens anything - so the pane is free to follow the pointer,
//! which is what somebody sweeping a mouse down a list of near-identical page
//! names is asking it to do.
//!
//! With the pointer elsewhere it falls back to the selection, so the pane still
//! says something while the list is being driven from the keyboard.
//!
//! # Why this is derived once rather than armed at each site
//!
//! The target can move on a keystroke, a click, a hover, a scroll, or a batch
//! of results landing and taking the selected row with it. Arming a request at
//! each of those is five call sites that have to agree, and the sixth that gets
//! added later is the one that leaves the pane describing the previous file
//! under the current file's name. So [`AppState::follow_preview`] recomputes
//! the target after *every* event and compares it against what was last asked
//! about, which cannot be forgotten by a site that does not know it exists.
//!
//! # Why it is debounced at all, for a local answer
//!
//! Because the answer is not local. `metadata` is a round trip to an SMB share
//! reached over a VPN as often as over the LAN, and holding Down through
//! twelve rows would be twelve of them. `PREVIEW_DEBOUNCE` is short enough that
//! stopping on a row feels immediate and long enough that passing over one
//! costs nothing.

use std::sync::Arc;
use std::time::Instant;

use crate::app::event::{Cmd, PreviewMsg, Redraw, Response};
use crate::app::state::AppState;
use crate::config::PREVIEW_DEBOUNCE;
use crate::preview::{Preview, Request};

impl AppState {
    /// The file the pane should be describing, as of right now.
    ///
    /// `None` when there is no list, which is every empty state and the drive
    /// picker - all of which draw a body of their own, and none of which have a
    /// file for the pane to talk about.
    fn preview_target(&self) -> Option<&crate::search::matcher::Hit> {
        if self.picking_share || self.history.is_browsing() {
            return None;
        }
        let row = self.hovered().or_else(|| self.selected_row())?;
        self.hits.get(row)
    }

    /// Notices that the target moved, and arms a request for the new one.
    ///
    /// Called once per event from `dispatch`, so no caller has to remember to.
    pub(super) fn follow_preview(&mut self, now: Instant) -> Response {
        let target = self.preview_target().map(|hit| Arc::clone(&hit.path));
        if target == self.preview_target_path {
            return Response::none();
        }

        // Dropped rather than left up. The facts of the previous file under the
        // name of this one is the one wrong thing this pane can say, and it is
        // worse than saying nothing for the 120 ms it takes to find out.
        let had_something = self.preview.is_some();
        self.preview = None;
        self.preview_target_path = target.clone();

        self.preview_due_at = target.is_some().then(|| now + PREVIEW_DEBOUNCE);

        // A frame is owed only if something on screen actually changed. Moving
        // the pointer between two rows of an empty list changes nothing, and
        // sixty repaints a second for that is what `pointer::hover` already
        // refuses to ask for.
        let changed = had_something || target.is_some();
        Response::none().with_redraw(if changed { Redraw::Yes } else { Redraw::No })
    }

    /// The debounce expired: go and ask.
    pub(super) fn preview_due(&mut self, now: Instant) -> Response {
        let Some(due) = self.preview_due_at else {
            return Response::none();
        };
        if now < due {
            return Response::none();
        }
        self.preview_due_at = None;

        let Some(hit) = self.preview_target() else {
            return Response::none();
        };
        Response::none().with(Cmd::Preview(Request {
            path: Arc::clone(&hit.path),
            name: Arc::clone(&hit.name),
            // The line as typed, because a page set is defined against the
            // code somebody searched for rather than against the file they are
            // pointing at. See `preview::pages_of`.
            query: self.query.term().to_string(),
        }))
    }

    /// An answer came back.
    pub(super) fn on_preview(&mut self, msg: PreviewMsg) -> Response {
        let PreviewMsg::Ready(preview) = msg;
        self.take_preview(preview)
    }

    /// Keeps the answer, unless it is about a file nobody is looking at.
    ///
    /// The guard is by path rather than by epoch, which is the opposite of
    /// `on_search`, and the difference is what is being addressed: a search
    /// answers a *query*, so the query's epoch identifies it, while this
    /// answers a *file*, and a file that is still the target is still the right
    /// answer however long it took to arrive.
    fn take_preview(&mut self, preview: Arc<Preview>) -> Response {
        if self.preview_target_path.as_deref() != Some(&*preview.path) {
            return Response::none();
        }
        self.preview = Some(preview);
        Response::redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::AppEvent;
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::app::state::pointer::Intent;
    use crate::config::Settings;
    use crate::preview::Facts;
    use crate::search::matcher::Hit;

    fn hit(name: &str) -> Hit {
        Hit {
            path: Arc::from(format!(r"R:\11d\11-D-0704\{name}").as_str()),
            name: Arc::from(name),
            match_pos: 0,
            index: 0,
        }
    }

    fn state_with_hits(n: usize) -> (AppState, Instant) {
        let now = Instant::now();
        let mut state = AppState::new(Settings::default(), now);
        state.hits = (0..n)
            .map(|i| hit(&format!("11-D-0704_Page{i}.pdf")))
            .collect();
        (state, now)
    }

    fn ready(path: &str) -> PreviewMsg {
        PreviewMsg::Ready(Arc::new(Preview {
            path: Arc::from(path),
            name: Arc::from("whatever.pdf"),
            facts: Facts {
                bytes: Some(2_400_000),
                ..Facts::default()
            },
            pages: None,
        }))
    }

    /// Nothing is asked for until the pointer or the selection settles.
    #[test]
    fn moving_onto_a_row_asks_about_it_only_once_the_pause_has_passed() {
        let (mut state, now) = state_with_hits(4);

        let armed = state.follow_preview(now);
        assert!(armed.cmds.is_empty(), "asked before anything was selected");

        state.update(AppEvent::Intent(Intent::Hover(Some(2))), now);
        assert!(
            state.preview_due_at.is_some(),
            "a hover did not arm a request"
        );

        let early = state.preview_due(now + PREVIEW_DEBOUNCE / 2);
        assert!(early.cmds.is_empty(), "asked before the pause was over");

        let due = state.preview_due(now + PREVIEW_DEBOUNCE);
        match due.cmds.as_slice() {
            [Cmd::Preview(request)] => {
                assert!(request.path.ends_with("11-D-0704_Page2.pdf"), "{request:?}");
            }
            other => panic!("expected one preview request, got {other:?}"),
        }
    }

    /// The pointer wins over the selection, so sweeping a mouse down the list
    /// describes what is under it rather than what Enter would open.
    #[test]
    fn a_hover_describes_the_row_under_the_pointer_not_the_selection() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Select(0)), now);
        state.update(AppEvent::Intent(Intent::Hover(Some(3))), now);

        let due = state.preview_due(now + PREVIEW_DEBOUNCE);
        match due.cmds.as_slice() {
            [Cmd::Preview(request)] => {
                assert!(request.path.ends_with("Page3.pdf"), "{request:?}");
            }
            other => panic!("expected the hovered row, got {other:?}"),
        }
    }

    /// And the pointer leaving hands the pane back to the selection rather than
    /// blanking it.
    #[test]
    fn the_pointer_leaving_falls_back_to_the_selection() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Select(1)), now);
        state.update(AppEvent::Intent(Intent::Hover(Some(3))), now);
        state.update(AppEvent::Intent(Intent::Hover(None)), now);

        let due = state.preview_due(now + PREVIEW_DEBOUNCE);
        match due.cmds.as_slice() {
            [Cmd::Preview(request)] => {
                assert!(request.path.ends_with("Page1.pdf"), "{request:?}");
            }
            other => panic!("expected the selected row, got {other:?}"),
        }
    }

    /// An answer for a file the pointer has already left is dropped. Without
    /// this the pane shows one file's size under another file's name, which is
    /// the single most misleading thing it could do.
    #[test]
    fn an_answer_about_a_file_nobody_is_looking_at_is_dropped() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Hover(Some(1))), now);

        let stale = state.update(AppEvent::Preview(ready(r"R:\somewhere\else.pdf")), now);
        assert!(!stale.redraw.is_yes(), "a stale answer claimed a frame");
        assert!(state.preview.is_none(), "a stale answer was kept");
    }

    #[test]
    fn an_answer_about_the_file_under_the_pointer_is_kept() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Hover(Some(1))), now);
        let path = state.preview_target_path.clone().expect("a target");

        state.update(AppEvent::Preview(ready(&path)), now);
        assert_eq!(
            state.preview.as_ref().map(|p| p.facts.bytes),
            Some(Some(2_400_000))
        );
    }

    /// Moving on drops what was on screen immediately, rather than leaving the
    /// previous file's facts up until the next answer arrives.
    #[test]
    fn moving_to_another_row_clears_what_was_being_shown() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Hover(Some(1))), now);
        let path = state.preview_target_path.clone().expect("a target");
        state.update(AppEvent::Preview(ready(&path)), now);
        assert!(state.preview.is_some(), "the fixture has nothing to clear");

        state.update(AppEvent::Intent(Intent::Hover(Some(2))), now);
        assert!(
            state.preview.is_none(),
            "the previous file's facts stayed up under the new file's name"
        );
    }

    /// Staying put asks once, not once per event.
    #[test]
    fn an_unchanged_target_does_not_ask_again() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Hover(Some(1))), now);
        state.preview_due(now + PREVIEW_DEBOUNCE);
        assert!(
            state.preview_due_at.is_none(),
            "the request was not retired"
        );

        let again = state.update(AppEvent::Intent(Intent::Hover(Some(1))), now);
        assert!(again.cmds.is_empty(), "an unchanged hover asked again");
        assert!(state.preview_due_at.is_none());
    }

    /// The drive picker draws a body of its own and has no file to describe.
    #[test]
    fn the_drive_picker_has_nothing_to_preview() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Select(1)), now);
        state.update(
            AppEvent::Preview(ready(&state.preview_target_path.clone().expect("a target"))),
            now,
        );
        assert!(state.preview.is_some(), "the fixture has nothing to clear");

        state.update(AppEvent::Key(KeyEvent::new(Key::F(5), Mods::NONE)), now);
        assert!(state.picking_share, "the fixture did not open the picker");
        assert!(state.preview.is_none(), "the picker kept a file preview up");
        assert_eq!(state.preview_due_at, None);
    }

    /// The deadline has to be folded in, or the loop parks and the request is
    /// never sent at all.
    #[test]
    fn the_pause_is_one_of_the_deadlines_the_loop_waits_on() {
        let (mut state, now) = state_with_hits(4);
        state.update(AppEvent::Intent(Intent::Hover(Some(2))), now);

        assert_eq!(
            state.next_deadline(),
            Some(now + PREVIEW_DEBOUNCE),
            "the loop would sleep through the request"
        );
    }
}
