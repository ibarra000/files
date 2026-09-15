//! Every key this program answers, written out as data.
//!
//! By hand, and deliberately *not* generated from `keys.rs`. A table derived
//! from the code agrees with the code by construction and proves nothing. This
//! is the list of what the program is supposed to do; the tests below are the
//! argument that it does.
//!
//! Two failures this is here to catch, both silent:
//!
//! - An arm that stops being reached. The arms in `keys.rs` are matched in
//!   order, so a duplicate does not fail to compile - it shadows, and the
//!   binding that lost is simply dead. That is how `Ctrl+W` once came to type
//!   a literal `w`.
//! - A hint that outlives the key it names. The hint bar is the only
//!   documentation most people will read, and a chip for a binding that has
//!   been renamed is worse than no chip: it teaches a key that does nothing,
//!   and the reader concludes the program is broken rather than the line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::{AppEvent, HotkeyMsg, Redraw, SearchMsg};
use files::app::key::{Key, KeyEvent, KeyPhase, Mods};
use files::app::state::AppState;
use files::config::Settings;
use files::search::matcher::{Hit, SearchOutcome};
use files::view::hints;

/// Where the panel is when a key is pressed.
///
/// Two, where there used to be five. The text field always has the keyboard, so
/// the only thing a key's meaning still depends on is whether the body is the
/// result list or the drive picker - and the picker is reached from one key and
/// left by three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    /// Something typed, results on screen. The ordinary case.
    Searching,
    /// Nothing typed, so the body is the codes used before.
    Recent,
    /// `F5` has been pressed.
    Picking,
}

/// One binding, and where it applies.
struct Binding {
    code: Key,
    mods: Mods,
    at: Where,
    /// What it does, in the words the help panel would use.
    what: &'static str,
    /// Deliberately answered whatever modifiers happen to be set.
    ///
    /// Some terminals report a function key with SHIFT, and requiring NONE
    /// makes such a key intermittently dead - which is a bug report nobody can
    /// reproduce. The character bindings are the opposite: a bare letter is
    /// text, and only the modified form is a command.
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
        anymod(Key::F(1), Searching, "show or hide the shortcuts window"),
        anymod(Key::F(2), Searching, "switch viewer"),
        anymod(Key::F(5), Searching, "choose a drive to update"),
        // The drive picker, which is the one thing that borrows the body.
        anymod(Key::F(5), Picking, "close the drive list"),
        b(Key::Up, NONE, Picking, "previous drive"),
        b(Key::Down, NONE, Picking, "next drive"),
        b(Key::Enter, NONE, Picking, "update the chosen drive"),
        b(Key::Char('a'), NONE, Picking, "update every drive"),
        b(Key::Esc, NONE, Picking, "back to typing"),
        // Editing. The caret keys are caret keys everywhere, which is the
        // whole of what collapsing the focus bought.
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
        // The recent codes, which is what an empty field shows.
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
            query: s.input.text().to_string(),
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

/// A state already at `at`, reached the way the program reaches it rather than
/// by assigning a field - so the fixture cannot describe a state the program
/// cannot get into.
fn fixture(at: Where) -> (AppState, Instant) {
    fixture_in(at, false)
}

/// The same, with the panel summoned - which is the *shipped* configuration
/// and the one the hint bar calls `compact`.
///
/// It had no fixture until now, so `Context::of` always saw `compact: false`
/// and the short set the real panel draws was never contract-tested. That is
/// how a panel shipped whose only route to the shortcut window was a key it
/// advertised nowhere.
///
/// Summoned before anything is typed, because `enter_overlay` stands down
/// whatever was being browsed and selects the field - so summoning afterwards
/// would unwind the state `at` just reached.
fn fixture_in(at: Where, compact: bool) -> (AppState, Instant) {
    let now = Instant::now();
    let mut s = AppState::new(Settings::default(), now);
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);
    if compact {
        s.update(AppEvent::Hotkey(HotkeyMsg::Summoned), now);
    }
    for c in "11-D-0704".chars() {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), NONE)), now);
    }
    deliver(&mut s, 60, now);
    // The caret goes into the middle of the code rather than staying at the
    // end of it, so that Backspace and Delete both have something to do. A
    // fixture where one of them is legitimately a no-op cannot tell a dead
    // binding from a live one.
    s.update(AppEvent::Key(KeyEvent::new(Key::Left, NONE)), now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Left, NONE)), now);

    match at {
        Where::Searching => {
            // Off the first row, so Up has somewhere to go. A fixture where a
            // binding is legitimately a no-op cannot tell a dead one from a
            // live one - the same reason the caret above sits mid-code.
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
        }
        Where::Recent => {
            // The recent codes are what an empty field shows, so the code goes
            // first - and then one step onto an entry.
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

/// Where the fixture actually got to, read back off the state.
fn where_of(s: &AppState) -> Where {
    if s.picking_share {
        Where::Picking
    } else if s.showing_recent() {
        Where::Recent
    } else {
        Where::Searching
    }
}

/// Everything a keypress could visibly change.
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
        menu: false,
    }
}

// --- the tests ----------------------------------------------------------

/// No key means two things in one mode.
///
/// The arms match in order, so a duplicate does not fail to compile: it
/// shadows, and the loser is dead code that still has a hint pointing at it.
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

/// Every binding in the table still answers.
///
/// "Answers" is deliberately weak - a redraw, a command, or any observable
/// change - because asserting *what* each key does is the job of the tests that
/// name it. What this catches is an arm falling off the match, which is
/// otherwise completely silent.
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

/// Every chip in the hint bar names a key that does something.
///
/// The bar is the only documentation most people will read. A chip left
/// pointing at a renamed binding teaches a key that does nothing, and the
/// reader concludes the program is broken rather than the line.
#[test]
fn every_advertised_key_is_a_live_binding() {
    for compact in [false, true] {
        for at in [Where::Searching, Where::Recent, Where::Picking] {
            let (s, now) = fixture_in(at, compact);
            for chip in hints::hints(hints::Context::of(&s)) {
                let Some(code) = key_of(chip.key) else {
                    // Arrow clusters like `↑↓` are covered by the table above,
                    // which names each direction separately.
                    continue;
                };
                let mut probe = fixture_in(at, compact).0;
                let before = snapshot(&probe);
                let r = probe.update(AppEvent::Key(KeyEvent::new(code, mods_of(chip.key))), now);
                assert!(
                    r.redraw == Redraw::Yes || !r.cmds.is_empty() || snapshot(&probe) != before,
                    "{at:?} (compact {compact}) advertises {:?} ({}), which does nothing",
                    chip.key,
                    chip.label
                );
            }
        }
    }
}

/// And nothing is advertised that the table does not know about, which is the
/// other direction of the same contract.
#[test]
fn nothing_is_advertised_that_the_table_has_never_heard_of() {
    let all = table();
    for (compact, at) in [false, true]
        .into_iter()
        .flat_map(|c| [Where::Searching, Where::Recent, Where::Picking].map(|a| (c, a)))
    {
        let (s, _) = fixture_in(at, compact);
        for chip in hints::hints(hints::Context::of(&s)) {
            let Some(code) = key_of(chip.key) else {
                continue;
            };
            assert!(
                all.iter().any(|b| b.code == code),
                "{at:?} advertises {:?}, which is in no binding",
                chip.key
            );
        }
    }
}

/// Only the function keys ignore their modifiers, and that is a decision.
///
/// Some terminals report a function key with SHIFT set; requiring NONE there
/// would make F2 and F5 intermittently dead. Every *character* binding is the
/// opposite - a bare letter is text, and only the modified form is a command -
/// so a character binding that answered any modifier would be eating input.
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
        // A command counts as answering, and has to: `F1` opens a window, which
        // is something the shell does and the state machine only asks for.
        assert!(
            !r.cmds.is_empty() || snapshot(&s) != before,
            "{:?} was dead with SHIFT set, which some terminals always do",
            binding.code
        );
    }
}

/// Browsing the recent codes owns exactly the keys the table says it owns, and
/// hands every other one back to editing rather than swallowing it.
#[test]
fn browsing_hands_back_every_key_it_does_not_own() {
    // A character is the clearest case: it must reach the search box, keeping
    // the previewed code rather than discarding it.
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

    // The fixture steps onto the newest entry, so Down is already past the end
    // of the list - and past the end leaves recall with the field empty.
    let (mut s, now) = fixture(Where::Recent);
    s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
    assert_eq!(s.input.text(), "", "Down past the newest empties the field");
    assert_eq!(
        where_of(&s),
        Where::Searching,
        "and leaves the list rather than staying in it"
    );

    // And it *stays* empty however many times it is pressed. Down used to
    // start recall exactly as Up does, so on an empty field the first press
    // put the code just cleared straight back, the second cleared it and the
    // third put it back - one key, two states, forever.
    for _ in 0..4 {
        s.update(AppEvent::Key(KeyEvent::new(Key::Down, NONE)), now);
        assert_eq!(s.input.text(), "", "Down put the cleared code back");
        assert_eq!(where_of(&s), Where::Searching);
    }

    // Up steps further back, and F2 is deliberately not a way out of it:
    // which program opens a file has nothing to do with which code is being
    // looked at.
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

/// AltGr types a character rather than being swallowed as a binding.
///
/// Windows reports AltGr as Ctrl+Alt, and on a German, Polish or French layout
/// that is how `@`, `{`, `[` and the accented letters are typed. The guard that
/// keeps `Ctrl+W` from typing a literal `w` was making a whole class of
/// characters untypable, with nothing on screen to say why.
///
/// This has now been broken twice by two different toolkits - once by
/// crossterm's key translation and once by egui suppressing `Event::Text`
/// whenever ctrl is set - so it is asserted here, against the state machine,
/// where neither of them can reach it.
#[test]
fn altgr_types_a_character_rather_than_being_swallowed() {
    let (mut s, now) = fixture(Where::Searching);
    let before = s.input.text().to_string();

    for c in ['@', '{', '[', '\u{e9}'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALTGR)), now);
    }

    // At the caret, which the fixture parks mid-code on purpose - so this
    // checks they were typed, not where.
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

/// A control combination this program does not have is not text.
///
/// Without the guard, the catch-all that handles typing turned `Ctrl+W` into a
/// literal `w` in the search box.
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

/// Windows reports a press and a release for every keystroke. Without the
/// guard every one of them is handled twice, which for a search box means
/// every character is doubled.
#[test]
fn a_key_release_is_not_a_second_keystroke() {
    let now = Instant::now();
    let mut s = AppState::new(Settings::default(), now);
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

/// The chip labels are written for somebody who does not use a terminal by
/// choice, so none of them may be a bare key name with no verb in it.
#[test]
fn every_chip_says_what_its_key_does() {
    for (compact, at) in [false, true]
        .into_iter()
        .flat_map(|c| [Where::Searching, Where::Recent, Where::Picking].map(|a| (c, a)))
    {
        let (s, _) = fixture_in(at, compact);
        for chip in hints::hints(hints::Context::of(&s)) {
            assert!(
                chip.label.len() >= 4 && chip.label.contains(char::is_alphabetic),
                "{at:?} advertises {:?} with the unhelpful label {:?}",
                chip.key,
                chip.label
            );
        }
    }
}

/// Maps a chip's key name back to the key it stands for.
/// How the help panel writes the key this binding answers.
///
/// `None` for the ones the panel deliberately does not list as keys of their
/// own: a typed character, and `Delete`, which the Backspace row covers.
fn help_name(binding: &Binding) -> Option<&'static str> {
    let ctrl = binding.mods == CTRL;
    Some(match binding.code {
        Key::Char('1') => return None,
        Key::Delete => return None,
        Key::Char(c) if ctrl => match c {
            'q' => "Ctrl+Q",
            'c' => "Ctrl+C",
            'x' => "Ctrl+X",
            'v' => "Ctrl+V",
            'a' => "Ctrl+A",
            'u' => "Ctrl+U",
            'w' => "Ctrl+W",
            _ => return None,
        },
        // `A` in the drive picker, which the panel writes bare.
        Key::Char('a') => "A",
        Key::F(1) => "F1",
        Key::F(2) => "F2",
        Key::F(5) => "F5",
        Key::Enter => "Enter",
        Key::Esc => "Esc",
        Key::Backspace => "Backspace",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        _ => return None,
    })
}

/// The help panel is the program's only documentation, so every key in the
/// table has to appear in it.
///
/// Driven from the table rather than from a list written out by hand. The
/// panel's own copy of this test checks thirteen key names that somebody typed
/// into it, which is exactly why `Ctrl+A`, `Ctrl+U`, `Home` and `End` were
/// live bindings documented nowhere while it passed.
#[test]
fn every_key_in_the_table_is_in_the_help_panel() {
    let panel = help_text();
    for binding in table() {
        let Some(name) = help_name(&binding) else {
            continue;
        };
        assert!(
            panel.contains(name),
            "{name} ({}) is a live binding and is not in the help panel:
{panel}",
            binding.what
        );
    }
}

fn help_text() -> String {
    use files::view::help::Row;
    files::view::help::rows(files::config::ViewerKind::Pdf)
        .iter()
        .map(|row| match row {
            Row::Blank => String::new(),
            Row::Heading(text) => (*text).to_string(),
            Row::Entry(key, what) => format!("{key} {what}"),
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

fn key_of(name: &str) -> Option<Key> {
    match name {
        "Enter" => Some(Key::Enter),
        "Esc" => Some(Key::Esc),
        "F1" => Some(Key::F(1)),
        "F2" => Some(Key::F(2)),
        "F5" => Some(Key::F(5)),
        "Ctrl+Q" => Some(Key::Char('q')),
        "Ctrl+C" => Some(Key::Char('c')),
        "\u{2191}" => Some(Key::Up),
        "\u{2193}" => Some(Key::Down),
        // A cluster like `↑↓` or `←→` names two keys at once; the table covers
        // each of them separately.
        _ => None,
    }
}

fn mods_of(name: &str) -> Mods {
    if name.starts_with("Ctrl+") {
        CTRL
    } else {
        NONE
    }
}

/// The panel you actually get says how to find every key.
///
/// F1 has opened the shortcut window since the window existed, and the bar the
/// shipped panel draws advertised it nowhere - so the only way to discover it
/// was to already know. The compact set had no fixture either, which is how
/// that survived: every contract test above ran against the full bar.
#[test]
fn the_compact_bar_offers_a_way_to_see_every_key() {
    let (s, _) = fixture_in(Where::Searching, false);
    assert!(
        !hints::Context::of(&s).compact,
        "the plain fixture should not be compact, or this proves nothing"
    );

    let (s, _) = fixture_in(Where::Searching, true);
    let cx = hints::Context::of(&s);
    assert!(cx.compact, "the summoned fixture is not the compact bar");

    let help = hints::hints(cx)
        .into_iter()
        .find(|h| h.key == "F1")
        .expect("the compact bar does not mention F1");
    assert_eq!(
        help.action,
        Some(hints::Action::Help),
        "the F1 chip is not clickable"
    );
}
