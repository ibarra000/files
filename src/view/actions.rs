//! What can be done with the result the cursor is on.
//!
//! Ueli's model, and the thing its footer is built around: one *default
//! action* named on a button beside an `↵` chip, and everything else behind a
//! `⋮` that opens a small menu. The footer stops being a row of key hints and
//! becomes a sentence - "Enter will do this" - with the rest one press away.
//!
//! # Why this replaces the hint bar
//!
//! The hint bar answered a different question. It listed the keys that were
//! live *somewhere on the panel*, sorted by how hard each fought for its
//! place, and dropped the losers when the line ran out - so what it said
//! depended on how wide the window was, and the one thing it could never say
//! was what would happen if you pressed Enter right now. It said `Enter open`,
//! which is true of every launcher ever written.
//!
//! This says "Open as one document", or "Open with avwin", depending on the
//! mode `F2` has left the program in - which is the fact the viewer chip
//! existed to carry and could not, because at the shipped width it did not
//! fit.
//!
//! # Two kinds of entry
//!
//! Everything above the last two is about the *selected file* and disappears
//! when there is not one. The last two - reading a drive again, and the
//! settings - are about the program, are always there, and are how `F5` and
//! `Ctrl+,` stay discoverable now that nothing advertises them along the
//! bottom.

use crate::app::state::AppState;
use crate::config::ViewerKind;

/// One thing that can be done.
///
/// Named by id rather than carried as a closure or a `Cmd`, for the reason
/// every other decision in `view` is: this module has to be checkable without
/// a window, and the state machine has to stay the only thing that knows what
/// a key means. See [`crate::app::state::pointer::Intent::Act`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    /// Whatever `Enter` does, which depends on the viewer `F2` chose.
    Open,
    /// Every page of the code, merged, whatever the mode is.
    OpenAsDocument,
    /// This one file, handed to `avwin.exe`.
    OpenInAvwin,
    /// This one file, handed to whatever Windows has registered for it.
    OpenWithWindows,
    /// Explorer, with the file already picked out.
    Reveal,
    CopyPath,
    CopyName,
    /// Re-read the drive this result came off.
    Refresh,
    Settings,
}

/// One entry, as the footer and the menu will draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    /// What the button says, and what the menu row says.
    ///
    /// A verb phrase rather than a noun: this is read as the completion of
    /// "Enter will…", and a button labelled `Viewer: PDF` answers a question
    /// nobody asked at the moment they are about to press it.
    pub description: &'static str,
    /// The key that does the same thing, where there is one.
    ///
    /// Written the way somebody would say it. The renderer splits it on `+`
    /// and draws each part as its own chip, which is Ueli's treatment and the
    /// reason there is no `+` in any of these strings on screen.
    pub shortcut: Option<&'static str>,
    /// Whether running it puts the panel away.
    ///
    /// Per action, as Ueli has it, and the distinction is the whole of why
    /// hide-after-opening is safe to switch on by default: an open is
    /// finished the moment the file is on its way, and a copy has a toast to
    /// read. A panel that vanished and took "copied 47 characters" with it
    /// would be a panel that never confirmed anything.
    pub hides: bool,
}

const fn act(
    id: ActionId,
    description: &'static str,
    shortcut: Option<&'static str>,
    hides: bool,
) -> Action {
    Action {
        id,
        description,
        shortcut,
        hides,
    }
}

/// What `Enter` will do, said in words.
///
/// The one thing the hint bar could not say. `F2` decides whether Enter
/// assembles a document, launches avwin or hands the file to Windows, and the
/// chip that named the mode was `Priority::Normal` - so at six hundred points
/// it was the first thing dropped, and the key whose whole job is to change a
/// mode gave no sign of which mode it was in.
pub const fn open_label(viewer: ViewerKind) -> &'static str {
    match viewer {
        // Not "Open with whatever Windows uses", which is what this mode
        // technically means: on the default setting the button is read dozens
        // of times an hour and "Open" is what it does.
        ViewerKind::Auto => "Open",
        ViewerKind::Pdf => "Open as one document",
        ViewerKind::Avwin => "Open with avwin",
    }
}

/// Everything on offer right now, the default first.
///
/// The order is the menu's order and the first entry is what `Enter` does, so
/// this is one list rather than a default plus a menu: two lists would be two
/// places for the default to be decided, and they would disagree the first
/// time somebody added an action.
pub fn actions(state: &AppState) -> Vec<Action> {
    let mut out = Vec::new();

    if has_something_to_open(state) {
        out.push(act(
            ActionId::Open,
            open_label(state.viewer),
            Some("Enter"),
            true,
        ));
        // The two modes that are *not* the current one, so the menu never
        // offers the same thing twice under two names.
        for alternative in alternatives(state.viewer) {
            out.push(alternative);
        }
        out.push(act(
            ActionId::Reveal,
            "Show it in Explorer",
            Some("Ctrl+O"),
            true,
        ));
        out.push(act(
            ActionId::CopyPath,
            "Copy the path",
            // Only when `Ctrl+C` really would do this. It copies the text
            // selected in the search box when there is one, and a chip that
            // named a key doing something else is worse than no chip.
            state.input.selection().is_none().then_some("Ctrl+C"),
            false,
        ));
        out.push(act(ActionId::CopyName, "Copy the name", None, false));
    }

    // Always, and this is what replaces the hint bar for the two keys that
    // are about the program rather than about a file. Nothing else advertises
    // them now.
    out.push(act(
        ActionId::Refresh,
        "Read a drive again",
        Some("F5"),
        false,
    ));
    out.push(act(ActionId::Settings, "Settings", Some("Ctrl+,"), false));
    out
}

/// What `Enter` would do, or nothing when there is nothing under it.
///
/// Read off [`actions`] rather than computed again, so the button and the
/// first row of the menu cannot come to disagree.
pub fn default_action(state: &AppState) -> Option<Action> {
    actions(state).into_iter().find(|a| a.id == ActionId::Open)
}

/// Whether `Enter` has a file to take.
///
/// The selection, or the first row where nothing is selected - which is what
/// `open_selection` falls back to, so this has to agree with it.
fn has_something_to_open(state: &AppState) -> bool {
    !state.hits.is_empty() && !state.history.is_browsing() && !state.picking_share
}

/// The two viewers the current mode is not.
fn alternatives(viewer: ViewerKind) -> Vec<Action> {
    const DOCUMENT: Action = act(
        ActionId::OpenAsDocument,
        "Open as one document",
        Some("Ctrl+D"),
        true,
    );
    const AVWIN: Action = act(
        ActionId::OpenInAvwin,
        "Open with avwin",
        Some("Ctrl+E"),
        true,
    );
    // No key, and that is not an oversight: this is what `Enter` already does
    // on the default setting, so it is here for somebody who has switched
    // away from it and wants one file back the ordinary way. A third chord
    // for that would be a key nobody presses twice.
    const WINDOWS: Action = act(
        ActionId::OpenWithWindows,
        "Open with whatever Windows uses",
        None,
        true,
    );

    match viewer {
        ViewerKind::Auto => vec![DOCUMENT, AVWIN],
        ViewerKind::Pdf => vec![AVWIN, WINDOWS],
        ViewerKind::Avwin => vec![DOCUMENT, WINDOWS],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::AppEvent;
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::config::Settings;
    use crate::search::matcher::Hit;
    use std::sync::Arc;
    use std::time::Instant;

    fn state() -> AppState {
        AppState::new(Settings::default(), Instant::now())
    }

    fn with_hits(n: usize) -> AppState {
        let mut s = state();
        s.input.set_text("11-D");
        s.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\jobs\11-D-070{i}.pdf").as_str()),
                name: Arc::from(format!("11-D-070{i}.pdf").as_str()),
                match_pos: 0,
                index: i as u32,
            })
            .collect();
        s
    }

    /// The default is the first entry, and it is what `Enter` does.
    #[test]
    fn the_first_action_is_the_one_enter_runs() {
        let s = with_hits(3);
        let all = actions(&s);
        assert_eq!(all[0].id, ActionId::Open);
        assert_eq!(all[0].shortcut, Some("Enter"));
        assert_eq!(default_action(&s), Some(all[0]));
    }

    /// With nothing to open there is no default action, so the footer button
    /// has nothing to say and says nothing.
    #[test]
    fn nothing_to_open_offers_no_way_to_open_it() {
        let s = state();
        assert_eq!(default_action(&s), None);
        let all = actions(&s);
        assert!(
            all.iter().all(|a| !a.hides),
            "an action that puts the panel away with nothing selected: {all:?}"
        );
        // But the program's own two are still there, because nothing else
        // advertises them any more.
        assert!(all.iter().any(|a| a.id == ActionId::Refresh));
        assert!(all.iter().any(|a| a.id == ActionId::Settings));
    }

    /// The button says which viewer `Enter` will use, which is the fact the
    /// hint bar dropped first whenever the line got tight.
    #[test]
    fn the_default_action_names_the_viewer_it_will_use() {
        for (viewer, said) in [
            (ViewerKind::Auto, "Open"),
            (ViewerKind::Pdf, "Open as one document"),
            (ViewerKind::Avwin, "Open with avwin"),
        ] {
            let mut s = with_hits(3);
            s.viewer = viewer;
            assert_eq!(default_action(&s).unwrap().description, said, "{viewer:?}");
        }
    }

    /// And the menu offers the two it will not, never the same one twice.
    #[test]
    fn the_menu_offers_the_other_two_viewers_and_not_the_current_one() {
        for viewer in [ViewerKind::Auto, ViewerKind::Pdf, ViewerKind::Avwin] {
            let mut s = with_hits(3);
            s.viewer = viewer;
            let all = actions(&s);

            let mut said: Vec<&str> = all.iter().map(|a| a.description).collect();
            let before = said.len();
            said.sort_unstable();
            said.dedup();
            assert_eq!(said.len(), before, "{viewer:?} offers the same thing twice");

            let opens = all
                .iter()
                .filter(|a| {
                    matches!(
                        a.id,
                        ActionId::Open
                            | ActionId::OpenAsDocument
                            | ActionId::OpenInAvwin
                            | ActionId::OpenWithWindows
                    )
                })
                .count();
            assert_eq!(opens, 3, "{viewer:?}: the default and two alternatives");
        }
    }

    /// `Ctrl+C` is advertised on "Copy the path" only while it would really
    /// do that. With a run selected in the search box it copies the run, and
    /// a chip naming a key that does something else teaches the wrong key.
    #[test]
    fn the_copy_key_is_advertised_only_when_it_copies_the_path() {
        let s = with_hits(3);
        let path = actions(&s)
            .into_iter()
            .find(|a| a.id == ActionId::CopyPath)
            .expect("no copy action");
        assert_eq!(path.shortcut, Some("Ctrl+C"));

        let mut s = with_hits(3);
        s.input.select_all();
        assert!(
            s.input.selection().is_some(),
            "the fixture selected nothing"
        );
        let path = actions(&s)
            .into_iter()
            .find(|a| a.id == ActionId::CopyPath)
            .expect("no copy action");
        assert_eq!(path.shortcut, None, "Ctrl+C would copy the selected text");
    }

    /// A copy leaves the panel up so its toast can be read; an open does not.
    #[test]
    fn only_the_opens_put_the_panel_away() {
        for action in actions(&with_hits(3)) {
            let expected = matches!(
                action.id,
                ActionId::Open
                    | ActionId::OpenAsDocument
                    | ActionId::OpenInAvwin
                    | ActionId::OpenWithWindows
                    | ActionId::Reveal
            );
            assert_eq!(
                action.hides, expected,
                "{:?} hides: {}",
                action.id, action.hides
            );
        }
    }

    /// Browsing the recent codes is not a file, so nothing that acts on one
    /// is offered.
    #[test]
    fn browsing_a_code_offers_nothing_to_do_with_a_file() {
        let mut s = with_hits(3);
        s.history.record("11-D-0704");
        s.input.clear();
        s.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            Instant::now(),
        );
        assert!(s.history.is_browsing(), "the fixture is not browsing");
        assert_eq!(default_action(&s), None);
    }

    /// Every description is written for somebody who does not use a terminal
    /// by choice, so none of them may be a bare key name with no verb in it.
    #[test]
    fn every_action_says_what_it_does() {
        for action in actions(&with_hits(3)) {
            assert!(
                action.description.len() >= 4 && action.description.contains(char::is_alphabetic),
                "{:?} is described as {:?}",
                action.id,
                action.description
            );
        }
    }
}
