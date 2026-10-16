//! The actions available for the selected result.
//!
//! The footer names one default action (what `Enter` does, which depends on
//! the `F2` viewer mode) beside an `↵` chip; the rest sit behind a `⋮` menu.
//! File actions appear only when a result is selected. The last two entries,
//! refresh and settings, are always present and are where `F5` and `Ctrl+,`
//! are advertised.

use std::borrow::Cow;

use crate::app::state::AppState;
use crate::config::{ViewerKind, Viewers};

/// Identifies an action.
///
/// An id rather than a closure or `Cmd`, so this module is testable without a
/// window and only the state machine interprets keys. See
/// [`crate::app::state::pointer::Intent::Act`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    /// Whatever `Enter` does, which depends on the viewer `F2` chose.
    Open,
    /// Opens every page of the base name as one merged PDF, regardless of mode.
    OpenAsDocument,
    /// Opens the file with the configured viewer at this index.
    OpenWith(usize),
    /// Opens the file with the Windows default app.
    OpenWithWindows,
    /// Opens Explorer with the file selected.
    Reveal,
    CopyPath,
    CopyName,
    /// Re-reads the drive this result came from.
    Refresh,
    Settings,
}

/// One entry as the footer and the menu draw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    /// The button and menu label, written as a verb phrase. Owned when it
    /// names a configured viewer.
    pub description: Cow<'static, str>,
    /// The equivalent key, if any. The renderer splits it on `+` and draws
    /// each part as a separate chip.
    pub shortcut: Option<&'static str>,
    /// Whether running it hides the panel. Opens hide; copies do not, so
    /// their confirmation toast stays visible.
    pub hides: bool,
}

fn act(
    id: ActionId,
    description: impl Into<Cow<'static, str>>,
    shortcut: Option<&'static str>,
    hides: bool,
) -> Action {
    Action {
        id,
        description: description.into(),
        shortcut,
        hides,
    }
}

/// Returns the label for what `Enter` does in the current `F2` viewer mode.
pub fn open_label(viewer: ViewerKind, viewers: &Viewers) -> Cow<'static, str> {
    match viewer.within(viewers) {
        // The default mode; plain "Open" rather than "Open with default app".
        ViewerKind::Auto => Cow::Borrowed("Open"),
        ViewerKind::Pdf => Cow::Borrowed("Open as combined PDF"),
        ViewerKind::App(i) => Cow::Owned(open_with_label(viewers, i)),
    }
}

/// Returns "Open with" followed by the configured viewer's name.
fn open_with_label(viewers: &Viewers, index: usize) -> String {
    let name = viewers.get(index).map_or("", |v| v.name.as_ref());
    format!("Open with {name}")
}

/// Returns every available action in menu order, the default first.
///
/// A single list, so the default and the menu cannot disagree.
pub fn actions(state: &AppState) -> Vec<Action> {
    let mut out = Vec::new();

    if has_something_to_open(state) {
        out.push(act(
            ActionId::Open,
            open_label(state.viewer, &state.settings.viewers),
            Some("Enter"),
            true,
        ));
        // Only the modes that are not current, so nothing appears twice.
        out.extend(alternatives(state.viewer, &state.settings.viewers));
        out.push(act(
            ActionId::Reveal,
            "Show in Explorer",
            Some("Ctrl+O"),
            true,
        ));
        out.push(act(
            ActionId::CopyPath,
            "Copy path",
            // `Ctrl+C` copies the search-box selection when there is one, so
            // the chip is shown only when it would copy the path.
            state.input.selection().is_none().then_some("Ctrl+C"),
            false,
        ));
        out.push(act(ActionId::CopyName, "Copy name", None, false));
    }

    // Always present: the only place `F5` and `Ctrl+,` are advertised.
    out.push(act(ActionId::Refresh, "Refresh drive", Some("F5"), false));
    out.push(act(ActionId::Settings, "Settings", Some("Ctrl+,"), false));
    out
}

/// Returns what `Enter` would do, or `None` when there is nothing to open.
///
/// Derived from [`actions`] so the button and the menu cannot disagree.
pub fn default_action(state: &AppState) -> Option<Action> {
    actions(state).into_iter().find(|a| a.id == ActionId::Open)
}

/// Returns whether `Enter` has a file to open.
///
/// Must agree with `open_selection`, which falls back to the first row when
/// nothing is selected.
fn has_something_to_open(state: &AppState) -> bool {
    !state.hits.is_empty() && !state.history.is_browsing() && !state.picking_share
}

/// Returns the open actions other than the current mode: the two built-in
/// ones, then each configured viewer.
fn alternatives(viewer: ViewerKind, viewers: &Viewers) -> Vec<Action> {
    let document = act(
        ActionId::OpenAsDocument,
        "Open as combined PDF",
        Some("Ctrl+D"),
        true,
    );
    // No shortcut: this is what `Enter` does in the default mode.
    let windows = act(
        ActionId::OpenWithWindows,
        "Open with default app",
        None,
        true,
    );
    // Only the first viewer gets `Ctrl+E`.
    let app = |i: usize| {
        act(
            ActionId::OpenWith(i),
            open_with_label(viewers, i),
            (i == 0).then_some("Ctrl+E"),
            true,
        )
    };

    let current = viewer.within(viewers);
    let apps = (0..viewers.len())
        .filter(|&i| current != ViewerKind::App(i))
        .map(app);
    match current {
        // `Enter` is the system's choice, so the system is not offered again.
        ViewerKind::Auto => std::iter::once(document).chain(apps).collect(),
        ViewerKind::Pdf => apps.chain(std::iter::once(windows)).collect(),
        ViewerKind::App(_) => std::iter::once(document)
            .chain(apps)
            .chain(std::iter::once(windows))
            .collect(),
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
        let settings = Settings {
            viewers: Arc::new(crate::config::Viewers::new(vec![
                crate::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
            ])),
            ..Settings::fixture()
        };
        AppState::new(settings, Instant::now())
    }

    const SKETCH: ViewerKind = ViewerKind::App(0);

    fn with_hits(n: usize) -> AppState {
        let mut s = state();
        s.input.set_text("AB-C");
        s.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\projects\AB-C-070{i}.pdf").as_str()),
                name: Arc::from(format!("AB-C-070{i}.pdf").as_str()),
                match_pos: 0,
                index: i as u32,
            })
            .collect();
        s
    }

    #[test]
    fn the_first_action_is_the_one_enter_runs() {
        let s = with_hits(3);
        let all = actions(&s);
        assert_eq!(all[0].id, ActionId::Open);
        assert_eq!(all[0].shortcut, Some("Enter"));
        assert_eq!(default_action(&s).as_ref(), Some(&all[0]));
    }

    #[test]
    fn nothing_to_open_offers_no_way_to_open_it() {
        let s = state();
        assert_eq!(default_action(&s), None);
        let all = actions(&s);
        assert!(
            all.iter().all(|a| !a.hides),
            "an action that puts the panel away with nothing selected: {all:?}"
        );
        // The program-level actions are always present.
        assert!(all.iter().any(|a| a.id == ActionId::Refresh));
        assert!(all.iter().any(|a| a.id == ActionId::Settings));
    }

    #[test]
    fn the_default_action_names_the_viewer_it_will_use() {
        for (viewer, said) in [
            (ViewerKind::Auto, "Open"),
            (ViewerKind::Pdf, "Open as combined PDF"),
            (SKETCH, "Open with sketch"),
        ] {
            let mut s = with_hits(3);
            s.viewer = viewer;
            assert_eq!(default_action(&s).unwrap().description, said, "{viewer:?}");
        }
    }

    #[test]
    fn the_menu_offers_the_other_two_viewers_and_not_the_current_one() {
        for viewer in [ViewerKind::Auto, ViewerKind::Pdf, SKETCH] {
            let mut s = with_hits(3);
            s.viewer = viewer;
            let all = actions(&s);

            let mut said: Vec<&str> = all.iter().map(|a| a.description.as_ref()).collect();
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
                            | ActionId::OpenWith(_)
                            | ActionId::OpenWithWindows
                    )
                })
                .count();
            assert_eq!(opens, 3, "{viewer:?}: the default and two alternatives");
        }
    }

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

    #[test]
    fn only_the_opens_put_the_panel_away() {
        for action in actions(&with_hits(3)) {
            let expected = matches!(
                action.id,
                ActionId::Open
                    | ActionId::OpenAsDocument
                    | ActionId::OpenWith(_)
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

    #[test]
    fn browsing_a_past_search_offers_nothing_to_do_with_a_file() {
        let mut s = with_hits(3);
        s.history.record("AB-C-0704");
        s.input.clear();
        s.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            Instant::now(),
        );
        assert!(s.history.is_browsing(), "the fixture is not browsing");
        assert_eq!(default_action(&s), None);
    }

    #[test]
    fn with_no_viewers_none_is_offered() {
        let mut s = with_hits(3);
        s.settings.viewers = Arc::new(crate::config::Viewers::default());
        for viewer in [ViewerKind::Auto, ViewerKind::Pdf] {
            s.viewer = viewer;
            let all = actions(&s);
            assert!(
                all.iter().all(|a| !matches!(a.id, ActionId::OpenWith(_))),
                "{viewer:?}: {all:?}"
            );
            assert!(all.iter().all(|a| a.shortcut != Some("Ctrl+E")));
        }
    }

    #[test]
    fn every_configured_viewer_is_offered_and_the_first_has_the_key() {
        let mut s = with_hits(3);
        s.settings.viewers = Arc::new(crate::config::Viewers::new(vec![
            crate::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
            crate::config::ViewerApp::new("Notes", "notepad.exe", &[]),
        ]));
        let all = actions(&s);
        let first = all.iter().find(|a| a.id == ActionId::OpenWith(0)).unwrap();
        let second = all.iter().find(|a| a.id == ActionId::OpenWith(1)).unwrap();
        assert_eq!(first.description, "Open with sketch");
        assert_eq!(first.shortcut, Some("Ctrl+E"));
        assert_eq!(second.description, "Open with Notes");
        assert_eq!(second.shortcut, None);

        // With the second in force, the menu offers the first and not it.
        s.viewer = ViewerKind::App(1);
        let all = actions(&s);
        assert_eq!(default_action(&s).unwrap().description, "Open with Notes");
        assert!(all.iter().any(|a| a.id == ActionId::OpenWith(0)));
        assert!(!all.iter().any(|a| a.id == ActionId::OpenWith(1)));
    }

    /// No description may be a bare key name.
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
