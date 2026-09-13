//! Caret movement, selection and recall, driven through the real state machine
//! with a fake clock.
//!
//! `AppState::update` is a pure function of (event, time), so all of this is
//! reachable without a window - which matters more here than usual, because the
//! behaviour being tested is almost entirely about where a caret is and what is
//! highlighted, and neither is visible from outside a running program.
//!
//! The mouse half of this file is gone. It drove `AppEvent::Mouse` with cell
//! coordinates and re-derived the layout to work out what had been clicked;
//! what replaced it is `app::state::pointer`, where a click arrives already
//! resolved and there are no coordinates left to be wrong about.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::{AppEvent, Cmd, Response, VerifyMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::config::{REMEMBER_DEBOUNCE, Settings, VERIFY_DEBOUNCE};
use files::search::matcher::Hit;
use files::search::verify::VerifyOutcome;

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    let s = AppState::new(Settings::default(), now);
    (s, now)
}

fn key(code: Key, modifiers: Mods) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, modifiers))
}

fn press(code: Key) -> AppEvent {
    key(code, Mods::NONE)
}

fn shift(code: Key) -> AppEvent {
    key(code, Mods::SHIFT)
}

fn ctrl(code: Key) -> AppEvent {
    key(code, Mods::CTRL)
}

fn type_in(s: &mut AppState, text: &str, now: Instant) {
    for c in text.chars() {
        s.update(key(Key::Char(c), Mods::NONE), now);
    }
}

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("V:\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

/// Drives a code through a verification and then all the way to rest, which is
/// what makes it eligible for recall.
///
/// The two used to be the same event. Recording rode on the verification
/// coming back, which is `VERIFY_DEBOUNCE` after the last keystroke - a third
/// of a second, which is an ordinary mid-word pause rather than the end of
/// anything. It is its own, much longer, deadline now, so a helper that wants
/// a code remembered has to say so by letting the typing stop.
fn search_and_settle(s: &mut AppState, code: &str, now: Instant) -> Response {
    type_in(s, code, now);
    let at = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, at);
    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: code.to_string(),
            elapsed: Duration::from_millis(5),
            outcome: VerifyOutcome::Server {
                hits: vec![hit("a.pdf")],
                matched: 1,
                capped: false,
                audit: files::search::verify::AuditVerdict::NotChecked,
            },
        }),
        at,
    );
    s.update(AppEvent::Tick, now + REMEMBER_DEBOUNCE)
}

// --- editing ----------------------------------------------------------

#[test]
fn a_character_can_be_fixed_in_the_middle_of_a_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-074", now);
    s.update(press(Key::Left), now);
    type_in(&mut s, "0", now);
    assert_eq!(s.input, "11-D-0704");
}

#[test]
fn delete_removes_forwards_and_backspace_removes_backwards() {
    let (mut s, now) = state();
    type_in(&mut s, "abcd", now);
    s.update(press(Key::Home), now);
    s.update(press(Key::Delete), now);
    assert_eq!(s.input, "bcd");
    s.update(press(Key::End), now);
    s.update(press(Key::Backspace), now);
    assert_eq!(s.input, "bc");
}

#[test]
fn ctrl_w_deletes_the_previous_field_of_the_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('w')), now);
    assert_eq!(s.input, "11-D-");
}

#[test]
fn ctrl_a_selects_the_whole_code_and_typing_replaces_it() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('a')), now);
    type_in(&mut s, "P", now);
    assert_eq!(s.input, "P");
}

/// The one that matters most for a network-bound tool: extending a selection
/// changes nothing about the query, so it must not bump the epoch, discard
/// the results or re-arm the server-side verification. Every Shift+Left
/// costing a round trip would be unusable over a VPN.
#[test]
fn moving_the_caret_or_selecting_never_re_runs_the_search() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let epoch = s.query_epoch();

    let moves = [
        shift(Key::Left),
        shift(Key::Left),
        press(Key::Left),
        press(Key::Right),
        shift(Key::Home),
        shift(Key::End),
        press(Key::Home),
        press(Key::End),
        ctrl(Key::Left),
        ctrl(Key::Right),
        ctrl(Key::Char('a')),
    ];
    for event in moves {
        let r = s.update(event, now);
        assert!(
            r.cmds.iter().all(|c| !matches!(c, Cmd::Search { .. })),
            "a caret move dispatched a search"
        );
        assert!(
            r.cmds.iter().all(|c| !matches!(c, Cmd::Verify { .. })),
            "a caret move dispatched a verify"
        );
    }
    assert_eq!(s.query_epoch(), epoch, "the query never changed");
    assert_eq!(s.input, "11-D-0704");
}

#[test]
fn a_paste_lands_at_the_caret_as_a_single_edit() {
    let (mut s, now) = state();
    type_in(&mut s, "11-", now);
    s.update(AppEvent::Paste("D-0704\r\n".into()), now);
    assert_eq!(s.input, "11-D-0704", "the newline must not reach the query");
}

#[test]
fn a_paste_replaces_the_selection() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('a')), now);
    s.update(AppEvent::Paste("P12345-001".into()), now);
    assert_eq!(s.input, "P12345-001");
}

// --- escape -----------------------------------------------------------

#[test]
fn escape_drops_the_selection_before_it_clears_the_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('a')), now);

    s.update(press(Key::Esc), now);
    assert_eq!(
        s.input, "11-D-0704",
        "the first Esc only drops the highlight"
    );

    s.update(press(Key::Esc), now);
    assert_eq!(s.input, "");
    assert!(!s.should_quit);
}

// --- recall -----------------------------------------------------------

#[test]
fn a_settled_code_becomes_recallable_and_is_stored() {
    let (mut s, now) = state();
    let r = search_and_settle(&mut s, "11-D-0704", now);

    assert_eq!(s.history.entries(), ["11-D-0704"]);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveHistory(entries) if entries.as_slice() == ["11-D-0704"])),
        "the list must be written as it is recorded, because there is no \
         clean shutdown to flush it at: {:?}",
        r.cmds
    );
}

/// The prefixes typed on the way to a code must not fill the list.
///
/// Two things stop them, and this asserts the first: a prefix is only ever
/// on screen while the typing is still going, so the quiet period never
/// expires for one. See `tests/overlay.rs` for the second, which is what
/// catches the prefix that gets through anyway because somebody stopped in
/// the middle to read the next four digits off a drawing.
#[test]
fn the_prefixes_typed_on_the_way_to_a_code_are_not_remembered() {
    let (mut s, now) = state();
    search_and_settle(&mut s, "11-D-0704", now);
    assert_eq!(s.history.len(), 1);
    assert_eq!(s.history.entries(), ["11-D-0704"]);
}

/// With nothing typed the list is the codes used before, and the arrows walk
/// it. This used to be a mode entered from a half-typed code, with a draft
/// preserved underneath; it is the empty state now, so there is no underneath.
#[test]
fn the_arrows_walk_the_recent_codes_when_nothing_is_typed() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "P12345-001");

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "11-D-0704");

    s.update(press(Key::Down), now);
    assert_eq!(s.input, "P12345-001");

    s.update(press(Key::Down), now);
    assert_eq!(s.input, "", "past the newest, the field is empty again");
}

/// Something typed means the arrows belong to the results, not to recall.
/// That is the rule that replaced the mode: the list you can see is the list
/// the arrows move in.
#[test]
fn the_arrows_do_not_reach_the_recent_codes_once_something_is_typed() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into()]);
    type_in(&mut s, "AB1", now);

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "AB1", "recall stole a half-typed code");
}

/// Stepping through recall must not touch the network: twenty codes would be
/// twenty round trips for codes nobody has chosen.
#[test]
fn browsing_recall_dispatches_no_work_until_an_entry_is_taken() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);

    let open = s.update(press(Key::Up), now);
    let step = s.update(press(Key::Up), now);
    for r in [&open, &step] {
        assert!(
            r.cmds
                .iter()
                .all(|c| !matches!(c, Cmd::Search { .. } | Cmd::Verify { .. })),
            "recall must not search: {:?}",
            r.cmds
        );
    }
    assert!(
        s.verify_due_at().is_none(),
        "a pending verify would fire for a code nobody chose"
    );

    let taken = s.update(press(Key::Enter), now);
    assert!(
        taken.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "taking an entry searches for it: {:?}",
        taken.cmds
    );
}

#[test]
fn typing_after_recalling_a_code_keeps_it_and_edits_it() {
    let (mut s, now) = state();
    s.seed_history(vec!["11-D-0704".into()]);
    s.update(press(Key::Up), now);
    type_in(&mut s, "X", now);

    assert_eq!(s.input, "11-D-0704X");
}

/// Nothing remembered and nothing typed is the first screen after an install,
/// and Up there has nowhere to go. It must do nothing rather than something
/// surprising.
#[test]
fn the_arrows_do_nothing_when_there_is_nothing_to_recall() {
    let (mut s, now) = state();
    let r = s.update(press(Key::Up), now);

    assert_eq!(s.input, "");
    assert!(
        !r.redraw.is_yes(),
        "an arrow with nowhere to go drew a frame"
    );
}
