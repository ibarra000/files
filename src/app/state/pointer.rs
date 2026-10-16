//! Pointer input, already resolved to intent by the toolkit.
//!
//! The toolkit hit-tests its own widgets, so [`Intent`] carries decisions and
//! no coordinates. Policies kept here:
//!
//! * **Select before opening.** What opens is the row drawn under the pointer,
//!   not whatever the arrows last selected.
//! * **A hover is never a selection.** Enter opens the selection, so a hover
//!   must not be mistakable for one.
//! * **A control does exactly what its key does**, by going through the key
//!   handler rather than reimplementing it.

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
    /// A double-click on a row: select and open it.
    Activate(usize),
    /// A click in the search field, at this byte offset into the text.
    Caret { byte: usize, extend: bool },
    /// A remembered query was clicked: use that entry.
    ///
    /// Separate from [`Intent::Activate`], which indexes the result list, so
    /// the state machine never has to know which body is on screen.
    Recall(usize),
    /// A configured alias was clicked: put it in the box.
    ///
    /// Separate from [`Intent::Recall`] for the same reason: each indexes a
    /// different list.
    UseAlias(usize),
    /// A row of the actions menu, or the button that names the default one.
    ///
    /// Named by action rather than key: two actions have no key, and Ctrl+C
    /// copies the text selection when there is one. See
    /// [`crate::view::actions`].
    Act(crate::view::actions::ActionId),
    /// The `⋮` button, or a press outside the open menu.
    ///
    /// Carries the target state rather than "toggle", so a press outside cannot
    /// reopen a menu the button just closed on the same press.
    ShowActions(bool),
}

impl AppState {
    pub(super) fn on_intent(&mut self, intent: Intent, now: Instant) -> Response {
        match intent {
            Intent::Hover(row) => self.hover(row),
            Intent::Select(row) => self.select_row(row),
            Intent::Activate(row) => {
                // Select first: the list can be replaced between the two
                // clicks, and what opens must be the row under the pointer now.
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
            Intent::UseAlias(rank) => self.accept_alias(rank, now),
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

    /// Uses a remembered query on a single click, as Enter does.
    ///
    /// A rank past the end is ignored; see [`Self::select_row`].
    fn recall_row(&mut self, rank: usize, now: Instant) -> Response {
        let Some(entry) = self.history.select(rank).map(str::to_string) else {
            return Response::none();
        };
        self.input.set_text(entry);
        self.accept_recall(now)
    }

    /// Runs one entry of the actions menu.
    ///
    /// Goes through the real key where the action has one, so a control and
    /// its shortcut cannot diverge. Copy path, copy name and open with Windows
    /// are direct: the last two have no key, and Ctrl+C copies the text
    /// selection when there is one.
    pub(super) fn run_action(&mut self, action: ActionId, now: Instant) -> Response {
        // Close the menu before the action runs, so a
        // `Cmd::DismissOverlay` cannot leave it open behind a hidden panel.
        let closing = std::mem::replace(&mut self.actions_open, false);
        let mut response = match action {
            ActionId::Open => self.on_key(KeyEvent::new(Key::Enter, Mods::NONE), now),
            ActionId::OpenAsDocument => self.open_with(ViewerKind::Pdf, now),
            ActionId::OpenWith(i) => self.open_with(ViewerKind::App(i), now),
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

    /// Moves the highlight to the row the pointer named.
    ///
    /// A rank past the end is ignored: the results can be replaced between the
    /// clicked frame and the event arriving.
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
        let mut state = AppState::new(Settings::fixture(), Instant::now());
        state.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\projects\AB-C-070{i}.pdf").as_str()),
                name: Arc::from(format!("AB-C-070{i}.pdf").as_str()),
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

    #[test]
    fn hovering_a_row_never_selects_it() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Hover(Some(2)), Instant::now());

        assert_eq!(state.hovered(), Some(2));
        assert_eq!(state.selected_row(), None, "a hover became a selection");
    }

    #[test]
    fn the_pointer_leaving_clears_the_hover() {
        let mut state = state_with_hits(4);
        state.on_intent(Intent::Hover(Some(2)), Instant::now());

        let response = state.on_intent(Intent::Hover(None), Instant::now());
        assert!(response.redraw.is_yes());
        assert_eq!(state.hovered(), None);
    }

    /// The toolkit reports the pointer every frame; only changes redraw.
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

    #[test]
    fn a_hover_past_the_end_of_the_list_is_no_hover_at_all() {
        let mut state = state_with_hits(2);
        state.on_intent(Intent::Hover(Some(7)), Instant::now());
        assert_eq!(state.hovered(), None);
    }

    #[test]
    fn clicking_in_the_search_field_moves_the_caret() {
        let mut state = state_with_hits(0);
        state.input.set_text("AB-C-0704");

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

    /// Every menu action that advertises a shortcut does what that key does.
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

    /// The key a shortcut string names.
    ///
    /// Kept as a separate hand-written list so it disagrees with
    /// `view::actions` when a shortcut changes there only.
    fn key_of(shortcut: &str) -> Option<KeyEvent> {
        Some(match shortcut {
            "Enter" => KeyEvent::new(Key::Enter, Mods::NONE),
            "F5" => KeyEvent::new(Key::F(5), Mods::NONE),
            "Ctrl+D" => KeyEvent::new(Key::Char('d'), Mods::CTRL),
            "Ctrl+E" => KeyEvent::new(Key::Char('e'), Mods::CTRL),
            "Ctrl+O" => KeyEvent::new(Key::Char('o'), Mods::CTRL),
            "Ctrl+," => KeyEvent::new(Key::Char(','), Mods::CTRL),
            // Ctrl+C copies the text selection when there is one, so the menu
            // row does not go through it. `view::actions` hides the shortcut
            // then; see `the_copy_key_is_advertised_only_when_it_copies_the_path`.
            "Ctrl+C" => return None,
            other => panic!("no key written down for {other:?}; add one"),
        })
    }

    #[test]
    fn running_an_action_puts_the_menu_away() {
        let now = Instant::now();
        let mut state = state_with_hits(3);
        state.actions_open = true;
        state.on_intent(Intent::Act(crate::view::actions::ActionId::CopyName), now);
        assert!(!state.actions_open, "the menu stayed up");
    }

    #[test]
    fn clicking_a_remembered_search_takes_that_one() {
        let now = Instant::now();
        let mut state = AppState::new(Settings::fixture(), now);
        state.seed_history(vec!["a".into(), "b".into(), "c".into()]);
        state.on_key(crate::app::key::KeyEvent::new(Key::Up, Mods::NONE), now);
        assert_eq!(state.input.text(), "a", "the fixture is not browsing");

        state.on_intent(Intent::Recall(2), now);
        assert_eq!(state.input.text(), "c", "it took a neighbour instead");
        assert!(
            !state.history.is_browsing(),
            "a click uses the search, it does not preview it"
        );
    }

    #[test]
    fn clicking_a_remembered_search_that_is_gone_does_nothing() {
        let now = Instant::now();
        let mut state = AppState::new(Settings::fixture(), now);
        state.seed_history(vec!["a".into()]);
        assert!(state.on_intent(Intent::Recall(9), now).cmds.is_empty());
        assert_eq!(state.input.text(), "");
    }
}
