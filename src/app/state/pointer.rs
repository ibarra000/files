//! What the pointer meant, rather than where it was.
//!
//! The terminal build had to do its own hit-testing: a click arrived as a cell
//! coordinate, and 470 lines of [`mouse`](super::mouse) turned it back into a
//! row, a caret position or a key hint by re-deriving the layout the frame had
//! just been drawn with. A window toolkit hit-tests its own widgets, so what
//! crosses this boundary is the decision and never the pixel.
//!
//! That is not only less code. It is the difference between a state machine
//! that can be wrong about where a row was and one that cannot: [`Intent`] has
//! no coordinates in it to disagree about.
//!
//! # What survived the move
//!
//! Roughly half of `mouse.rs` was policy rather than arithmetic, and policy
//! does not stop being true because the renderer changed:
//!
//! * **Select before opening.** What opens is the row that was drawn under the
//!   pointer, not whatever the arrows had last left selected.
//! * **A hover is never a selection.** Enter opens the selection; a highlight
//!   that could be mistaken for it is a highlight that gets a file opened by
//!   accident.
//! * **A control does exactly what its key does**, by going through the key
//!   handler rather than by reimplementing it. The chips this was written
//!   about are gone; the actions menu is what keeps the rule.
//!
//! Two things genuinely improve. The old `hovered` field carried this comment:
//! *"a terminal reports no 'the pointer left the window' event, and a hover
//! that outlives the pointer is a lie"* - `Hover(None)` is now reachable, so
//! the lie is impossible rather than merely mitigated. And this program's own
//! click counting is gone: a terminal reports presses, so `mouse.rs` had to
//! time them itself and rule that a third click inside the window was a hand
//! that had not lifted. A toolkit reports double-clicks, and
//! [`Intent::Activate`] is simply what one is.

use std::time::Instant;

use crate::app::event::Response;
use crate::app::key::{Key, KeyEvent, Mods};
use crate::app::state::AppState;
use crate::config::ViewerKind;
use crate::view::actions::ActionId;

/// What the pointer did, already resolved against the layout that drew it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// The pointer is over this row, or over none of them.
    Hover(Option<usize>),
    /// One click on a row: move the selection there.
    Select(usize),
    /// A double-click, or a single one on a remembered code: take the row.
    Activate(usize),
    /// A click in the search field, at this byte offset into the text.
    Caret { byte: usize, extend: bool },
    /// A remembered code was clicked: take that one.
    ///
    /// Its own variant rather than a reuse of [`Intent::Activate`], which
    /// indexes the result list. Making that one mean two things depending on
    /// which body is up would put a "which body is on screen" decision back
    /// inside the state machine, which is what `Intent` exists to keep out.
    ///
    /// The rank is the thing that matters and the thing a click on the old
    /// recall *chip* threw away - it was the Up arrow with the rank dropped,
    /// so clicking the fifth code stepped one entry older.
    Recall(usize),
    /// A row of the actions menu, or the button that names the default one.
    ///
    /// Named by what it *is* rather than by the key it happens to share,
    /// because two of these have no key at all and one - copying the path -
    /// has a key that means something else while there is a text selection.
    /// See [`crate::view::actions`].
    Act(crate::view::actions::ActionId),
    /// The `⋮` button, or a press anywhere the open menu is not.
    ///
    /// Carries the state it is asking for rather than saying "toggle",
    /// because the two callers know different things: the button knows it is
    /// a toggle, and a press outside knows only that the menu should be shut.
    /// A toggle from the second would reopen the menu the first had just
    /// closed, on the same press.
    ShowActions(bool),
}

impl AppState {
    pub(super) fn on_intent(&mut self, intent: Intent, now: Instant) -> Response {
        match intent {
            Intent::Hover(row) => self.hover(row),
            Intent::Select(row) => self.select_row(row),
            Intent::Activate(row) => {
                // Selection first, and unconditionally: what opens is the row
                // drawn under the pointer *now*. Results can land between the
                // two clicks - a verification replaces the list wholesale - so
                // opening whatever the first click chose would open something
                // that is no longer on screen.
                if row >= self.hits.len() {
                    return Response::none();
                }
                let mut response = self.jump_selection(row);
                response.merge(self.on_enter(now));
                response
            }
            Intent::Caret { byte, extend } => {
                self.input.set_caret(byte, extend);
                Response::redraw()
            }
            Intent::Recall(rank) => self.recall_row(rank, now),
            Intent::Act(action) => self.run_action(action, now),
            Intent::ShowActions(open) => {
                if self.actions_open == open {
                    return Response::none();
                }
                self.actions_open = open;
                Response::redraw()
            }
        }
    }

    /// One click on a remembered code uses it, exactly as Enter on it does.
    ///
    /// One click and not two: a remembered code is a shortcut, and asking for a
    /// double-click to use a shortcut defeats the point of having one.
    ///
    /// A rank past the end is not an error worth reporting, for the reason
    /// [`Self::select_row`] gives: the list can be replaced between the frame
    /// that was clicked and the event arriving.
    fn recall_row(&mut self, rank: usize, now: Instant) -> Response {
        let Some(entry) = self.history.select(rank).map(str::to_string) else {
            return Response::none();
        };
        self.input.set_text(entry);
        self.accept_recall(now)
    }

    /// Runs one entry of the actions menu.
    ///
    /// Through the real key where the action has one, so that a control and
    /// its shortcut cannot come to mean different things. That invariant is
    /// the reason this dispatches rather than each caller building its own
    /// `Cmd`: a menu row that assembled a command by hand would be a second
    /// copy of what the key does, and the two would part company the first
    /// time one of them was corrected.
    ///
    /// Three are direct, and each for a reason rather than for convenience.
    /// Copying the path has a key, `Ctrl+C`, whose meaning depends on whether
    /// there is a text selection - so a synthetic press would copy the
    /// selection and not the path, which is not what the row says. Copying
    /// the name and opening with Windows have no key at all.
    pub(super) fn run_action(&mut self, action: ActionId, now: Instant) -> Response {
        // Whatever it was, the menu has served its purpose. Before the action
        // runs, so a `Cmd::DismissOverlay` cannot leave a menu open behind a
        // hidden panel.
        let closing = std::mem::replace(&mut self.actions_open, false);
        let mut response = match action {
            ActionId::Open => self.on_key(KeyEvent::new(Key::Enter, Mods::NONE), now),
            ActionId::OpenAsDocument => self.open_with(ViewerKind::Pdf, now),
            ActionId::OpenInAvwin => self.open_with(ViewerKind::Avwin, now),
            ActionId::OpenWithWindows => self.open_with(ViewerKind::Auto, now),
            ActionId::Reveal => self.reveal_selection(now),
            ActionId::CopyPath => self.copy_path(now),
            ActionId::CopyName => self.copy_name(now),
            ActionId::Refresh => self.on_key(KeyEvent::new(Key::F(5), Mods::NONE), now),
            ActionId::Settings => self.on_key(KeyEvent::new(Key::Char(','), Mods::CTRL), now),
        };
        if closing {
            response.redraw = crate::app::event::Redraw::Yes;
        }
        response
    }

    /// Moves the highlight to a row the pointer named.
    ///
    /// A rank past the end of the list is not an error worth reporting: the
    /// results can be replaced between the frame that was clicked and the
    /// event arriving, and the honest answer to "select row 9 of 4" is to do
    /// nothing.
    fn select_row(&mut self, row: usize) -> Response {
        if row >= self.hits.len() {
            return Response::none();
        }
        self.jump_selection(row)
    }

    fn hover(&mut self, row: Option<usize>) -> Response {
        let row = row.filter(|&r| r < self.hits.len());
        if self.hovered == row {
            return Response::none();
        }
        self.hovered = row;
        Response::redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::search::matcher::Hit;
    use std::sync::Arc;

    fn state_with_hits(n: usize) -> AppState {
        let mut state = AppState::new(Settings::default(), Instant::now());
        state.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\jobs\11-D-070{i}.pdf").as_str()),
                name: Arc::from(format!("11-D-070{i}.pdf").as_str()),
                match_pos: 0,
                index: i as u32,
            })
            .collect();
        state
    }

    #[test]
    fn a_click_moves_the_selection_to_the_row_that_was_clicked() {
        let mut state = state_with_hits(4);
        let response = state.on_intent(Intent::Select(2), Instant::now());

        assert!(response.redraw.is_yes());
        assert_eq!(state.selected_row(), Some(2));
    }

    /// The results can be replaced between the frame that was drawn and the
    /// click arriving. Opening row nine of four is not a thing to do.
    #[test]
    fn a_click_past_the_end_of_the_list_does_nothing() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Select(1), Instant::now());

        let response = state.on_intent(Intent::Select(9), Instant::now());
        assert!(!response.redraw.is_yes());
        assert_eq!(
            state.selected_row(),
            Some(1),
            "an impossible row moved the selection anyway"
        );

        let opened = state.on_intent(Intent::Activate(9), Instant::now());
        assert!(
            opened.cmds.is_empty(),
            "an impossible row opened something: {:?}",
            opened.cmds
        );
    }

    /// What opens is the row under the pointer *now*, not whatever the arrows
    /// had last left selected.
    #[test]
    fn activating_a_row_selects_it_before_opening_it() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Select(0), Instant::now());

        let response = state.on_intent(Intent::Activate(3), Instant::now());
        assert_eq!(state.selected_row(), Some(3));
        assert!(
            !response.cmds.is_empty(),
            "activating a row should ask for something to be opened"
        );
    }

    /// Enter opens the selection. A hover that could be mistaken for one is a
    /// hover that gets a file opened by accident.
    #[test]
    fn hovering_a_row_never_selects_it() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Hover(Some(2)), Instant::now());

        assert_eq!(state.hovered(), Some(2));
        assert_eq!(state.selected_row(), None, "a hover became a selection");
    }

    /// The terminal could not report this and the old code said so. A window
    /// can, so a hover no longer outlives the pointer.
    #[test]
    fn the_pointer_leaving_clears_the_hover() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Hover(Some(2)), Instant::now());

        let response = state.on_intent(Intent::Hover(None), Instant::now());
        assert!(response.redraw.is_yes());
        assert_eq!(state.hovered(), None);
    }

    /// Every frame reports where the pointer is, whether or not it moved. A
    /// redraw for each of them is sixty repaints a second for a mouse that is
    /// sitting still.
    #[test]
    fn a_hover_that_changes_nothing_asks_for_no_frame() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Hover(Some(1)), Instant::now());

        let again = state.on_intent(Intent::Hover(Some(1)), Instant::now());
        assert!(!again.redraw.is_yes(), "an unchanged hover claimed a frame");

        let none = state.on_intent(Intent::Hover(None), Instant::now());
        assert!(none.redraw.is_yes());
        let still_none = state.on_intent(Intent::Hover(None), Instant::now());
        assert!(!still_none.redraw.is_yes());
    }

    /// A hover of a row that no longer exists is the same lie as a hover that
    /// outlived the pointer.
    #[test]
    fn a_hover_past_the_end_of_the_list_is_no_hover_at_all() {
        let mut state = state_with_hits(2);
        state.on_intent(Intent::Hover(Some(7)), Instant::now());
        assert_eq!(state.hovered(), None);
    }

    #[test]
    fn clicking_in_the_search_field_moves_the_caret() {
        let mut state = state_with_hits(0);
        state.input.set_text("11-D-0704");

        state.on_intent(
            Intent::Caret {
                byte: 3,
                extend: false,
            },
            Instant::now(),
        );
        assert_eq!(state.input.caret(), 3);
        assert!(!state.input.has_selection());

        state.on_intent(
            Intent::Caret {
                byte: 7,
                extend: true,
            },
            Instant::now(),
        );
        assert_eq!(state.input.selection(), Some((3, 7)));
    }

    /// A menu row and the key it names must do the same thing, or the menu
    /// is lying about the keyboard.
    ///
    /// Through the whole list rather than one entry: every action that
    /// advertises a shortcut is a claim, and a claim that only holds for the
    /// one somebody remembered to test is not an invariant.
    #[test]
    fn every_advertised_action_does_what_its_key_does() {
        let now = Instant::now();
        for action in crate::view::actions::actions(&state_with_hits(3)) {
            let Some(shortcut) = action.shortcut else {
                continue;
            };
            let Some(key) = key_of(shortcut) else {
                continue;
            };
            let mut clicked = state_with_hits(3);
            let mut pressed = state_with_hits(3);
            let from_menu = clicked.on_intent(Intent::Act(action.id), now);
            let from_key = pressed.on_key(key, now);

            assert_eq!(
                from_menu.cmds, from_key.cmds,
                "{:?} ({shortcut}) does something other than its key",
                action.id
            );
            assert_eq!(clicked.selected_row(), pressed.selected_row());
        }
    }

    /// The key a shortcut string names, for the test above.
    ///
    /// Written out rather than parsed, and deliberately a second list: its
    /// only purpose is to disagree with `view::actions` when a shortcut is
    /// changed there and nowhere else.
    fn key_of(shortcut: &str) -> Option<KeyEvent> {
        Some(match shortcut {
            "Enter" => KeyEvent::new(Key::Enter, Mods::NONE),
            "F5" => KeyEvent::new(Key::F(5), Mods::NONE),
            "Ctrl+D" => KeyEvent::new(Key::Char('d'), Mods::CTRL),
            "Ctrl+E" => KeyEvent::new(Key::Char('e'), Mods::CTRL),
            "Ctrl+O" => KeyEvent::new(Key::Char('o'), Mods::CTRL),
            "Ctrl+," => KeyEvent::new(Key::Char(','), Mods::CTRL),
            // Copies the text selection when there is one, so the menu row
            // deliberately does *not* go through it. `view::actions` stops
            // advertising the key in that case, which is the claim
            // `the_copy_key_is_advertised_only_when_it_copies_the_path`
            // makes over there.
            "Ctrl+C" => return None,
            other => panic!("no key written down for {other:?}; add one"),
        })
    }

    /// The menu shuts when something on it is chosen, whatever that was.
    #[test]
    fn running_an_action_puts_the_menu_away() {
        let now = Instant::now();
        let mut state = state_with_hits(3);
        state.actions_open = true;
        state.on_intent(Intent::Act(crate::view::actions::ActionId::CopyName), now);
        assert!(!state.actions_open, "the menu stayed up");
    }

    /// A click takes the code that was clicked, not the one next to it.
    ///
    /// This used to push a hint-chip intent, which is the Up arrow with the
    /// rank thrown away - so clicking the fifth remembered code stepped one
    /// entry older than wherever the cursor already was.
    #[test]
    fn clicking_a_remembered_code_takes_that_one() {
        let now = Instant::now();
        let mut state = AppState::new(Settings::default(), now);
        state.seed_history(vec!["a".into(), "b".into(), "c".into()]);
        state.on_key(crate::app::key::KeyEvent::new(Key::Up, Mods::NONE), now);
        assert_eq!(state.input.text(), "a", "the fixture is not browsing");

        state.on_intent(Intent::Recall(2), now);
        assert_eq!(state.input.text(), "c", "it took a neighbour instead");
        assert!(
            !state.history.is_browsing(),
            "a click uses the code, it does not preview it"
        );
    }

    /// A rank past the end of a list that has been replaced underneath the
    /// click does nothing, rather than indexing past it.
    #[test]
    fn clicking_a_remembered_code_that_is_gone_does_nothing() {
        let now = Instant::now();
        let mut state = AppState::new(Settings::default(), now);
        state.seed_history(vec!["a".into()]);
        assert!(state.on_intent(Intent::Recall(9), now).cmds.is_empty());
        assert_eq!(state.input.text(), "");
    }
}
