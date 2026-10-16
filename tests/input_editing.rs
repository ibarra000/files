//! Caret movement, selection and recall, driven through the real state machine
//! with a fake clock. `AppState::update` is a pure function of (event, time), so
//! no window is needed.

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
    let s = AppState::new(Settings::fixture(), now);
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

/// Drives a search through verification and on to `REMEMBER_DEBOUNCE`, which
/// makes it eligible for recall.
fn search_and_settle(s: &mut AppState, term: &str, now: Instant) -> Response {
    type_in(s, term, now);
    let at = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, at);
    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: files::search::query::Query::contains(term),
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
fn a_character_can_be_fixed_in_the_middle_of_a_search() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-074", now);
    s.update(press(Key::Left), now);
    type_in(&mut s, "0", now);
    assert_eq!(s.input, "AB-C-0704");
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
fn ctrl_w_deletes_the_previous_field_of_the_search() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-0704", now);
    s.update(ctrl(Key::Char('w')), now);
    assert_eq!(s.input, "AB-C-");
}

#[test]
fn ctrl_a_selects_the_whole_search_and_typing_replaces_it() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-0704", now);
    s.update(ctrl(Key::Char('a')), now);
    type_in(&mut s, "P", now);
    assert_eq!(s.input, "P");
}

/// Caret and selection changes leave the query unchanged, so they must not bump
/// the epoch, discard results or re-arm server-side verification.
#[test]
fn moving_the_caret_or_selecting_never_re_runs_the_search() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-0704", now);
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
    assert_eq!(s.input, "AB-C-0704");
}

#[test]
fn a_paste_lands_at_the_caret_as_a_single_edit() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-", now);
    s.update(AppEvent::Paste("C-0704\r\n".into()), now);
    assert_eq!(s.input, "AB-C-0704", "the newline must not reach the query");
}

#[test]
fn a_paste_replaces_the_selection() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-0704", now);
    s.update(ctrl(Key::Char('a')), now);
    s.update(AppEvent::Paste("P12345-001".into()), now);
    assert_eq!(s.input, "P12345-001");
}

// --- escape -----------------------------------------------------------

#[test]
fn escape_drops_the_selection_before_it_clears_the_search() {
    let (mut s, now) = state();
    type_in(&mut s, "AB-C-0704", now);
    s.update(ctrl(Key::Char('a')), now);

    s.update(press(Key::Esc), now);
    assert_eq!(
        s.input, "AB-C-0704",
        "the first Esc only drops the highlight"
    );

    s.update(press(Key::Esc), now);
    assert_eq!(s.input, "");
    assert!(!s.should_quit);
}

// --- recall -----------------------------------------------------------

#[test]
fn a_settled_search_becomes_recallable_and_is_stored() {
    let (mut s, now) = state();
    let r = search_and_settle(&mut s, "AB-C-0704", now);

    assert_eq!(s.history.entries(), ["AB-C-0704"]);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveHistory(entries) if entries.as_slice() == ["AB-C-0704"])),
        "the list must be written as it is recorded, because there is no \
         clean shutdown to flush it at: {:?}",
        r.cmds
    );
}

/// Prefixes are never on screen long enough for the quiet period to expire.
/// `tests/overlay.rs` covers a prefix that survives a pause mid-typing.
#[test]
fn the_prefixes_typed_on_the_way_to_a_search_are_not_remembered() {
    let (mut s, now) = state();
    search_and_settle(&mut s, "AB-C-0704", now);
    assert_eq!(s.history.len(), 1);
    assert_eq!(s.history.entries(), ["AB-C-0704"]);
}

/// With nothing typed, the arrows walk the recent searches.
#[test]
fn the_arrows_walk_the_recent_searches_when_nothing_is_typed() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "AB-C-0704".into()]);

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "P12345-001");

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "AB-C-0704");

    s.update(press(Key::Down), now);
    assert_eq!(s.input, "P12345-001");

    s.update(press(Key::Down), now);
    assert_eq!(s.input, "", "past the newest, the field is empty again");
}

/// Down never restores a search it cleared.
#[test]
fn down_never_puts_a_cleared_search_back() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "AB-C-0704".into()]);

    // Down on an empty field must not open the list.
    s.update(press(Key::Down), now);
    assert_eq!(s.input, "", "Down opened the recall list");
    assert!(!s.history.is_browsing(), "Down started browsing");

    // After stepping past the newest entry, the field stays empty.
    s.update(press(Key::Up), now);
    assert_eq!(s.input, "P12345-001", "Up did not open the list");
    s.update(press(Key::Down), now);
    assert_eq!(s.input, "", "past the newest, the field is empty");
    for _ in 0..4 {
        s.update(press(Key::Down), now);
        assert_eq!(s.input, "", "Down put the cleared search back");
    }
    assert!(!s.history.is_browsing());
}

/// Once something is typed, the arrows move in the results, not in recall.
#[test]
fn the_arrows_do_not_reach_the_recent_searches_once_something_is_typed() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into()]);
    type_in(&mut s, "AB1", now);

    s.update(press(Key::Up), now);
    assert_eq!(s.input, "AB1", "recall stole a half-typed search");
}

/// Stepping through recall must not issue searches or verifications.
#[test]
fn browsing_recall_dispatches_no_work_until_an_entry_is_taken() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "AB-C-0704".into()]);

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
        "a pending verify would fire for a search nobody chose"
    );

    let taken = s.update(press(Key::Enter), now);
    assert!(
        taken.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "taking an entry searches for it: {:?}",
        taken.cmds
    );
}

#[test]
fn typing_after_recalling_a_search_keeps_it_and_edits_it() {
    let (mut s, now) = state();
    s.seed_history(vec!["AB-C-0704".into()]);
    s.update(press(Key::Up), now);
    type_in(&mut s, "X", now);

    assert_eq!(s.input, "AB-C-0704X");
}

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
