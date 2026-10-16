//! Tests every key binding against a hand-written table.
//!
//! The table is written by hand rather than generated from `keys.rs`, so it
//! can disagree with the code. It catches two silent failures:
//!
//! - A shadowed arm. Arms in `keys.rs` match in order, so a duplicate compiles
//!   and the later binding is dead.
//! - A stale shortcut. A row in the actions menu that names a key which no
//!   longer does anything.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::{AppEvent, Cmd, Redraw, SearchMsg};
use files::app::key::{Key, KeyEvent, KeyPhase, Mods};
use files::app::state::AppState;
use files::config::Settings;
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;
use files::view::actions;

/// The panel mode a key is pressed in.
///
/// The text field always has focus, so a key's meaning depends only on what
/// the body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    /// A code typed and results on screen.
    Searching,
    /// Browsing recent codes.
    Recent,
    /// `F5` has been pressed.
    Picking,
}

/// One binding, and where it applies.
struct Binding {
    code: Key,
    mods: Mods,
    at: Where,
    /// A short description of what the key does.
    what: &'static str,
    /// Whether the binding fires regardless of modifiers.
    ///
    /// Only function keys do this, since some sources report them with SHIFT
    /// set. A bare character is text, so character bindings require their
    /// modifier.
    any_modifier: bool,
}

const fn b(code: Key, mods: Mods, at: Where, what: &'static str) -> Binding {
    Binding {
        code,
        mods,
        at,
        what,
        any_modifier: false,
    }
}

const fn anymod(code: Key, at: Where, what: &'static str) -> Binding {
    Binding {
        code,
        mods: Mods::NONE,
        at,
        what,
        any_modifier: true,
    }
}

const CTRL: Mods = Mods::CTRL;
const NONE: Mods = Mods::NONE;

fn table() -> Vec<Binding> {
    use Where::{Picking, Recent, Searching};
    vec![
        // Everywhere.
        b(Key::Char('q'), CTRL, Searching, "quit"),
        b(Key::Char('c'), CTRL, Searching, "copy the selected text"),
        b(Key::Char('x'), CTRL, Searching, "cut the selected text"),
        b(Key::Char('v'), CTRL, Searching, "paste"),
        b(Key::Char('a'), CTRL, Searching, "select the whole code"),
        b(Key::Char('u'), CTRL, Searching, "clear the line"),
        b(Key::Char('w'), CTRL, Searching, "delete the previous field"),
        b(
            Key::Char(','),
            CTRL,
            Searching,
            "show or hide the settings window",
        ),
        b(
            Key::Char('d'),
            CTRL,
            Searching,
            "open the selection as one document",
        ),
        b(
            Key::Char('e'),
            CTRL,
            Searching,
            "open the selection with the first configured viewer",
        ),
        b(
            Key::Char('o'),
            CTRL,
            Searching,
            "show the selection in Explorer",
        ),
        b(
            Key::Char('k'),
            CTRL,
            Searching,
            "show or hide the actions menu",
        ),
        anymod(Key::F(2), Searching, "switch viewer"),
        anymod(
            Key::F(3),
            Searching,
            "narrow to the start or the end of the name",
        ),
        anymod(Key::F(4), Searching, "narrow to one kind of file"),
        anymod(Key::F(5), Searching, "choose a drive to update"),
        // The drive picker.
        anymod(Key::F(5), Picking, "close the drive list"),
        b(Key::Up, NONE, Picking, "previous drive"),
        b(Key::Down, NONE, Picking, "next drive"),
        b(Key::Enter, NONE, Picking, "update the chosen drive"),
        b(Key::Esc, NONE, Picking, "back to typing"),
        // Editing.
        b(Key::Char('1'), NONE, Searching, "type a character"),
        b(Key::Backspace, NONE, Searching, "delete backwards"),
        b(Key::Delete, NONE, Searching, "delete forwards"),
        b(Key::Left, NONE, Searching, "move the caret left"),
        b(Key::Right, NONE, Searching, "move the caret right"),
        b(Key::Home, NONE, Searching, "caret to the start"),
        b(Key::End, NONE, Searching, "caret to the end"),
        // Moving through results.
        b(Key::Up, NONE, Searching, "up a row"),
        b(Key::Down, NONE, Searching, "down a row"),
        b(Key::PageDown, NONE, Searching, "a listful down"),
        b(Key::PageUp, NONE, Searching, "a listful up"),
        b(Key::Enter, NONE, Searching, "open the selection"),
        b(Key::Esc, NONE, Searching, "clear the code"),
        // Browsing recent codes.
        b(Key::Up, NONE, Recent, "an older code"),
        b(Key::Down, NONE, Recent, "a newer code"),
        b(Key::Enter, NONE, Recent, "use this code"),
        anymod(Key::F(2), Recent, "switch viewer without taking the code"),
    ]
}

// --- fixtures ----------------------------------------------------------

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("V:\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

fn deliver(s: &mut AppState, n: usize, now: Instant) {
    let hits: Vec<Hit> = (0..n).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect();
    s.update(
        AppEvent::Search(SearchMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_micros(200),
            result: Ok(SearchOutcome {
                hits,
                matched: n as u32,
                total: 9_000,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );
}

/// The default settings plus one configured viewer, `avwin`.
fn with_avwin() -> Settings {
    Settings {
        viewers: std::sync::Arc::new(files::config::Viewers::new(vec![
            files::config::ViewerApp::new("avwin", "avwin.exe", &["dwg"]),
        ])),
        ..Settings::fixture()
    }
}

/// Returns a state in mode `at`, reached through key events rather than by
/// setting fields, so it is always a reachable state.
///
/// The panel is not summoned; dismissal behaviour is tested in
/// `tests/overlay.rs`.
fn fixture(at: Where) -> (AppState, Instant) {
    let now = Instant::now();
    let mut s = AppState::new(with_avwin(), now);
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);
    for c in "11-D-0704".chars() {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), NONE)), now);
    }
    deliver(&mut s, 60, now);
    // Park the caret mid-code so Backspace and Delete both have an effect;
    // a legitimate no-op would be indistinguishable from a dead binding.
    s.update(AppEvent::Key(KeyEvent::new(Key::Left, NONE)), now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Left, NONE)), now);

    match at {
        Where::Searching => {
            // Move off the first row so Up has an effect.
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
        }
        Where::Recent => {
            // Clear the line, then step onto the newest entry.
            s.update(AppEvent::Key(KeyEvent::new(Key::Char('u'), CTRL)), now);
            s.update(AppEvent::Key(KeyEvent::new(Key::Up, NONE)), now);
        }
        Where::Picking => {
            s.update(AppEvent::Key(KeyEvent::new(Key::F(5), NONE)), now);
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
        }
    }
    assert_eq!(where_of(&s), at, "the fixture did not reach {at:?}");
    (s, now)
}

/// Returns the mode the state is in.
fn where_of(s: &AppState) -> Where {
    if s.picking_share {
        Where::Picking
    } else if s.showing_recent() {
        Where::Recent
    } else {
        Where::Searching
    }
}

/// The state a keypress can visibly change.
#[derive(Debug, PartialEq)]
struct Snapshot {
    text: String,
    caret: usize,
    selection: Option<(usize, usize)>,
    at: Where,
    selected: Option<String>,
    viewer: files::config::ViewerKind,
    toast: Option<String>,
    quit: bool,
    epoch: u64,
    menu: bool,
}

fn snapshot(s: &AppState) -> Snapshot {
    Snapshot {
        text: s.input.text().to_string(),
        caret: s.input.caret(),
        selection: s.input.selection(),
        at: where_of(s),
        selected: s.selected_hit().map(|h| h.name.to_string()),
        viewer: s.viewer,
        toast: s.toast.as_ref().map(|t| t.text.clone()),
        quit: s.should_quit,
        epoch: s.query_epoch(),
        menu: s.actions_open,
    }
}

// --- the tests ----------------------------------------------------------

#[test]
fn no_key_is_bound_twice_in_the_same_mode() {
    let all = table();
    for (i, a) in all.iter().enumerate() {
        for bnd in all.iter().skip(i + 1) {
            let same = a.code == bnd.code && a.at == bnd.at && a.mods == bnd.mods;
            assert!(
                !same,
                "{:?}+{:?} in {:?} is bound to both {:?} and {:?}",
                a.mods, a.code, a.at, a.what, bnd.what
            );
        }
    }
}

/// Every binding produces a redraw, a command, or an observable change.
///
/// What each key does is asserted elsewhere; this only detects dead arms.
#[test]
fn every_binding_in_the_table_still_answers() {
    for binding in table() {
        let (mut s, now) = fixture(binding.at);
        let before = snapshot(&s);
        let r = s.update(
            AppEvent::Key(KeyEvent::new(binding.code, binding.mods)),
            now,
        );
        let after = snapshot(&s);
        assert!(
            r.redraw == Redraw::Yes || !r.cmds.is_empty() || after != before,
            "{} ({:?}+{:?} in {:?}) did nothing at all",
            binding.what,
            binding.mods,
            binding.code,
            binding.at
        );
    }
}

/// Every Ctrl chord in the table survives `gui::input::translate`.
///
/// The other tests build `KeyEvent`s directly, so they cannot detect a chord
/// that the input layer drops. `pressed_with_ctrl` lists the `egui::Key` for
/// each character by hand so it can disagree with `gui::input::chord`.
#[test]
fn every_chord_in_the_table_survives_the_input_layer() {
    for binding in table() {
        let Key::Char(c) = binding.code else {
            continue;
        };
        if binding.mods != CTRL {
            continue;
        }
        let Some(event) = pressed_with_ctrl(c) else {
            // egui delivers copy, cut and paste as their own events; the
            // `the_clipboard_keys_*` tests in `gui::input` cover them.
            continue;
        };

        let produced = translated(vec![event]);
        assert!(
            produced.contains(&KeyEvent::new(binding.code, CTRL)),
            "Ctrl+{c} ({}) is in the binding table and the input layer drops it: \
             `gui::input::chord` has no arm for it, so no press can ever reach \
             the state machine.\ngot: {produced:?}",
            binding.what
        );
    }
}

/// A bare comma is typed once, not once from `Event::Text` and again from
/// `Event::Key`.
#[test]
fn a_bare_comma_is_typed_exactly_once() {
    let typed = translated(vec![
        eframe::egui::Event::Text(",".into()),
        raw_press(eframe::egui::Key::Comma, eframe::egui::Modifiers::NONE),
    ]);
    assert_eq!(typed, vec![KeyEvent::new(Key::Char(','), Mods::NONE)]);
}

/// Returns the egui Ctrl+key press for `c`.
///
/// Returns `None` for copy, cut and paste, which egui delivers as their own
/// events.
fn pressed_with_ctrl(c: char) -> Option<eframe::egui::Event> {
    use eframe::egui::Key as E;
    let key = match c {
        'a' => E::A,
        'q' => E::Q,
        'u' => E::U,
        'w' => E::W,
        'd' => E::D,
        'e' => E::E,
        'o' => E::O,
        'k' => E::K,
        'p' => E::P,
        'n' => E::N,
        'l' => E::L,
        ',' => E::Comma,
        'c' | 'x' | 'v' => return None,
        other => panic!("no egui key written down for Ctrl+{other}; add one"),
    };
    Some(raw_press(key, eframe::egui::Modifiers::CTRL))
}

fn raw_press(key: eframe::egui::Key, modifiers: eframe::egui::Modifiers) -> eframe::egui::Event {
    eframe::egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Runs one frame of egui events through `gui::input::translate`.
fn translated(events: Vec<eframe::egui::Event>) -> Vec<KeyEvent> {
    let mut input = eframe::egui::InputState::default();
    input.events = events;
    files::gui::input::translate(&input)
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::Key(key) => Some(key),
            _ => None,
        })
        .collect()
}

/// Every shortcut shown in the actions menu has an effect.
#[test]
fn every_advertised_key_is_a_live_binding() {
    for at in [Where::Searching, Where::Recent, Where::Picking] {
        let (s, now) = fixture(at);
        for action in actions::actions(&s) {
            let Some(shortcut) = action.shortcut else {
                continue;
            };
            let (code, mods) = key_of(shortcut);
            let mut probe = fixture(at).0;
            let before = snapshot(&probe);
            let r = probe.update(AppEvent::Key(KeyEvent::new(code, mods)), now);
            assert!(
                r.redraw == Redraw::Yes || !r.cmds.is_empty() || snapshot(&probe) != before,
                "{at:?} advertises {shortcut} ({}), which does nothing",
                action.description
            );
        }
    }
}

/// Every shortcut shown in the actions menu is in the binding table.
#[test]
fn nothing_is_advertised_that_the_table_has_never_heard_of() {
    let all = table();
    for at in [Where::Searching, Where::Recent, Where::Picking] {
        let (s, _) = fixture(at);
        for action in actions::actions(&s) {
            let Some(shortcut) = action.shortcut else {
                continue;
            };
            let (code, _) = key_of(shortcut);
            assert!(
                all.iter().any(|b| b.code == code),
                "{at:?} advertises {shortcut}, which is in no binding"
            );
        }
    }
}

#[test]
fn escape_shuts_the_actions_menu_rather_than_the_panel() {
    let (mut s, now) = fixture(Where::Searching);
    s.update(AppEvent::Key(KeyEvent::new(Key::Char('k'), CTRL)), now);
    assert!(s.actions_open, "Ctrl+K did not open the menu");

    let r = s.update(AppEvent::Key(KeyEvent::new(Key::Esc, NONE)), now);
    assert!(!s.actions_open, "Escape left the menu up");
    assert!(
        r.cmds.is_empty(),
        "Escape took the panel with it: {:?}",
        r.cmds
    );
}

#[test]
fn the_actions_key_is_a_toggle() {
    let (mut s, now) = fixture(Where::Searching);
    s.update(AppEvent::Key(KeyEvent::new(Key::Char('k'), CTRL)), now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Char('k'), CTRL)), now);
    assert!(!s.actions_open);
}

/// A shortcut pressed while the menu is open runs and closes the menu.
#[test]
fn a_shortcut_pressed_in_the_menu_closes_it() {
    let (mut s, now) = fixture(Where::Searching);
    s.update(AppEvent::Key(KeyEvent::new(Key::Char('k'), CTRL)), now);
    let r = s.update(AppEvent::Key(KeyEvent::new(Key::Char('o'), CTRL)), now);
    assert!(!s.actions_open, "the menu stayed up");
    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::Reveal(_))),
        "and the key did not run: {:?}",
        r.cmds
    );
}

/// Only function keys ignore modifiers, and they still work with SHIFT set.
///
/// A character binding that ignored modifiers would consume typed text.
#[test]
fn only_the_function_keys_ignore_their_modifiers() {
    for binding in table().iter().filter(|b| b.any_modifier) {
        assert!(
            matches!(binding.code, Key::F(_)),
            "{:?} claims to ignore modifiers but is not a function key",
            binding.code
        );
        let (mut s, now) = fixture(binding.at);
        let before = snapshot(&s);
        let r = s.update(AppEvent::Key(KeyEvent::new(binding.code, Mods::SHIFT)), now);
        // A command counts: some keys only ask the shell to act.
        assert!(
            !r.cmds.is_empty() || snapshot(&s) != before,
            "{:?} was dead with SHIFT set, which some terminals always do",
            binding.code
        );
    }
}

/// Browsing handles only its own keys and passes the rest to editing.
#[test]
fn browsing_hands_back_every_key_it_does_not_own() {
    // A character ends browsing and edits the previewed code.
    let (mut s, now) = fixture(Where::Recent);
    let before = s.input.text().to_string();
    s.update(AppEvent::Key(KeyEvent::new(Key::Char('X'), NONE)), now);
    assert_eq!(
        where_of(&s),
        Where::Searching,
        "a letter should end the browsing"
    );
    assert_eq!(
        s.input.text(),
        format!("{before}X"),
        "the previewed code should be kept and edited, not thrown away"
    );

    // The fixture is on the newest entry, so Down leaves browsing with an
    // empty field.
    let (mut s, now) = fixture(Where::Recent);
    s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
    assert_eq!(s.input.text(), "", "Down past the newest empties the field");
    assert_eq!(
        where_of(&s),
        Where::Searching,
        "and leaves the list rather than staying in it"
    );

    // Further presses of Down keep the field empty; Down never starts
    // browsing.
    for _ in 0..4 {
        s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
        assert_eq!(s.input.text(), "", "Down put the cleared code back");
        assert_eq!(where_of(&s), Where::Searching);
    }

    // Up steps further back, and F2 changes the viewer without leaving the
    // list.
    for code in [Key::Up, Key::F(2)] {
        let (mut s, now) = fixture(Where::Recent);
        s.update(AppEvent::Key(KeyEvent::new(code, NONE)), now);
        assert_eq!(
            where_of(&s),
            Where::Recent,
            "{code:?} should keep the recent list up"
        );
    }
}

/// AltGr characters are typed rather than treated as Ctrl bindings.
///
/// Windows reports AltGr as Ctrl+Alt, which is how `@`, `{`, `[` and accented
/// letters are typed on German, Polish and French layouts.
#[test]
fn altgr_types_a_character_rather_than_being_swallowed() {
    let (mut s, now) = fixture(Where::Searching);
    let before = s.input.text().to_string();

    for c in ['@', '{', '[', '\u{e9}'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALTGR)), now);
    }

    // Inserted at the mid-code caret, so only presence is checked.
    assert!(
        s.input.text().contains("@{[é"),
        "AltGr characters were swallowed as bindings: {:?}",
        s.input.text()
    );
    assert_eq!(
        s.input.text().chars().count(),
        before.chars().count() + 4,
        "some of them were dropped"
    );
}

/// An unbound Ctrl or Alt combination does not type its letter.
#[test]
fn a_control_combination_this_program_does_not_have_types_nothing() {
    let (mut s, now) = fixture(Where::Searching);
    let before = s.input.text().to_string();
    for c in ['x', 'y', 'z', 'b'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), CTRL)), now);
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALT)), now);
    }
    assert_eq!(
        s.input.text(),
        before,
        "an unbound combination typed a letter"
    );
}

/// Key releases are ignored, so characters are not doubled.
#[test]
fn a_key_release_is_not_a_second_keystroke() {
    let now = Instant::now();
    let mut s = AppState::new(Settings::fixture(), now);
    for c in "11-D".chars() {
        let key = KeyEvent::new(Key::Char(c), NONE);
        s.update(AppEvent::Key(key), now);
        s.update(
            AppEvent::Key(KeyEvent {
                phase: KeyPhase::Release,
                ..key
            }),
            now,
        );
    }
    assert_eq!(s.input.text(), "11-D");
}

/// Every action has a descriptive label, not a bare key name.
#[test]
fn every_action_says_what_its_key_does() {
    for at in [Where::Searching, Where::Recent, Where::Picking] {
        let (s, _) = fixture(at);
        for action in actions::actions(&s) {
            assert!(
                action.description.len() >= 4 && action.description.contains(char::is_alphabetic),
                "{at:?} offers {:?}, which says nothing",
                action.description
            );
        }
    }
}

/// Maps a shortcut string back to its key.
///
/// Written out by hand so it disagrees with `view::actions` if a shortcut
/// changes only there.
fn key_of(shortcut: &str) -> (Key, Mods) {
    match shortcut {
        "Enter" => (Key::Enter, NONE),
        "F5" => (Key::F(5), NONE),
        "Ctrl+C" => (Key::Char('c'), CTRL),
        "Ctrl+D" => (Key::Char('d'), CTRL),
        "Ctrl+E" => (Key::Char('e'), CTRL),
        "Ctrl+O" => (Key::Char('o'), CTRL),
        "Ctrl+," => (Key::Char(','), CTRL),
        other => panic!("no key written down for {other:?}; add one"),
    }
}
