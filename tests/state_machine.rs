//! End-to-end tests of the interaction model, driven with a fake clock.
//!
//! `AppState::update` is a pure function of (event, time), so debounce,
//! staleness, selection and error handling are testable without the network
//! drives. Everything goes through the public API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::LiveMsg;
use files::app::event::{
    AppEvent, ClipboardMsg, Cmd, IndexMsg, OpenMsg, Redraw, Response, SearchMsg, VerifyMsg,
};
use files::app::key::{Key, KeyEvent, KeyPhase, Mods};
use files::app::state::{AppState, EmptyReason, QueryPhase, Severity, TOAST_LIFETIME};
use files::config::LIVE_DEBOUNCE;
use files::config::{
    ENTER_WATCHDOG, SEARCH_DEBOUNCE, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG, VISIBLE_ROWS,
    ViewerKind,
};
use files::index::errors::EnumError;
use files::index::store::{Activity, Health, IndexStatus};
use files::paths::MappingId;
use files::search::live::{LiveCoverage, LiveOutcome};
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;
use files::search::verify::{AuditVerdict, VerifyOutcome};

/// The base settings for these tests: the defaults plus an `avwin` viewer for
/// `.dwg` files.
fn with_avwin() -> Settings {
    Settings {
        viewers: std::sync::Arc::new(files::config::Viewers::new(vec![
            files::config::ViewerApp::new("avwin", "avwin.exe", &["dwg"]),
        ])),
        ..Settings::fixture()
    }
}

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    (AppState::new(with_avwin(), now), now)
}

/// A state with no share configured.
fn unconfigured_state() -> (AppState, Instant) {
    // Built directly because the config layer rejects a file with no enabled
    // mapping; this state is a defensive branch, not reachable from config.
    let routes = files::paths::Routes::new(Vec::new(), files::paths::ConfigSource::BuiltIn);
    let settings = Settings::with_routes(Arc::new(routes), |s| s);
    let now = Instant::now();
    (AppState::new(settings, now), now)
}

fn key(c: char) -> AppEvent {
    AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE))
}

fn press(code: Key) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, Mods::NONE))
}

fn ctrl(code: Key) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, Mods::CTRL))
}

fn shift(code: Key) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, Mods::SHIFT))
}

fn type_in(s: &mut AppState, text: &str, now: Instant) -> Response {
    let mut r = Response::none();
    for c in text.chars() {
        r.merge(s.update(key(c), now));
    }
    r
}

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("V:\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

// --- typing -----------------------------------------------------------

/// Any non-empty term is dispatched; only a line with no term is not.
#[test]
fn a_two_character_query_is_dispatched() {
    let (mut s, now) = state();
    type_in(&mut s, "ab", now);
    assert_eq!(s.phase, QueryPhase::LocalPending);
    assert!(s.search_due_at().is_some(), "nothing was armed");
}

#[test]
fn a_line_with_no_term_on_it_is_not_dispatched() {
    let (mut s, now) = state();
    let r = type_in(&mut s, "ext:pdf", now);
    assert_eq!(s.phase, QueryPhase::Idle);
    assert!(r.cmds.iter().all(|c| !matches!(c, Cmd::Search { .. })));
}

#[test]
fn typing_arms_one_search_rather_than_one_per_keystroke() {
    let (mut s, now) = state();
    let r = type_in(&mut s, "11-D-0704", now);
    let searches = r
        .cmds
        .iter()
        .filter(|c| matches!(c, Cmd::Search { .. }))
        .count();
    assert_eq!(
        searches, 0,
        "typing dispatched a search rather than arming one"
    );
    assert!(s.search_due_at().is_some(), "nothing was armed");
    assert_eq!(s.phase, QueryPhase::LocalPending);

    // The pause dispatches exactly one search, for the whole code.
    let tick = s.update(AppEvent::Tick, now + SEARCH_DEBOUNCE);
    let dispatched: Vec<_> = tick
        .cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Search { query, .. } => Some(query.term().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(dispatched, vec!["11-D-0704".to_string()]);
    assert!(s.search_due_at().is_none(), "the deadline was not retired");
}

/// An unusual query is searched for rather than refused.
#[test]
fn an_unusual_code_is_searched_for_rather_than_refused() {
    let (mut s, now) = state();
    type_in(&mut s, "!!!", now);
    assert_eq!(s.phase, QueryPhase::LocalPending);
    assert_ne!(s.empty_reason, Some(EmptyReason::NoSharesConfigured));
    let r = s.update(AppEvent::Tick, now + SEARCH_DEBOUNCE);
    assert!(r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })));
}

/// With no share enabled the state reports that instead of searching nothing.
#[test]
fn a_configuration_with_no_enabled_share_says_so() {
    let (mut s, now) = unconfigured_state();
    let r = type_in(&mut s, "11-D-0704", now);
    assert_eq!(s.phase, QueryPhase::NoShares);
    assert_eq!(s.empty_reason, Some(EmptyReason::NoSharesConfigured));
    assert!(r.cmds.iter().all(|c| !matches!(c, Cmd::Search { .. })));
}

#[test]
fn backspacing_to_empty_returns_to_idle() {
    let (mut s, now) = state();
    type_in(&mut s, "abc", now);
    for _ in 0..3 {
        s.update(press(Key::Backspace), now);
    }
    assert_eq!(s.phase, QueryPhase::Idle);
    assert_eq!(s.empty_reason, Some(EmptyReason::NoQuery));
}

#[test]
fn key_releases_are_ignored() {
    // Windows delivers both press and release; handling both would double
    // every keystroke.
    let (mut s, now) = state();
    let mut ev = KeyEvent::new(Key::Char('a'), Mods::NONE);
    ev.phase = KeyPhase::Release;
    s.update(AppEvent::Key(ev), now);
    assert_eq!(s.input, "");
}

#[test]
fn typing_bumps_the_epoch_so_stale_results_are_discarded() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-07", now);
    let stale = AppEvent::Search(SearchMsg {
        epoch: s.query_epoch() - 1,
        query: Query::contains("old"),
        elapsed: Duration::ZERO,
        result: Ok(SearchOutcome {
            hits: vec![hit("ghost.txt")],
            matched: 1,
            total: 1,
            cancelled: false,
            unicode_fallback: false,
        }),
    });
    s.update(stale, now);
    assert!(
        s.hits.is_empty(),
        "a superseded result must not be displayed"
    );
}

#[test]
fn a_cancelled_result_changes_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let ev = AppEvent::Search(SearchMsg {
        epoch: s.query_epoch(),
        query: Query::parse(s.input.text()),
        elapsed: Duration::ZERO,
        result: Ok(SearchOutcome {
            hits: vec![],
            matched: 0,
            total: 0,
            cancelled: true,
            unicode_fallback: false,
        }),
    });
    assert_eq!(s.update(ev, now).redraw, Redraw::No);
    assert_eq!(s.phase, QueryPhase::LocalPending);
}

// --- keys -------------------------------------------------------------

/// Esc clears the input and never exits, including on an empty input.
#[test]
fn escape_clears_the_input_and_never_quits() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(press(Key::Esc), now);
    assert_eq!(s.input, "");
    assert!(!s.should_quit, "the first Esc must not end the session");

    s.update(press(Key::Esc), now);
    assert!(
        !s.should_quit,
        "Esc on an empty input must not end it either"
    );

    // Clearing is not a terminal state.
    type_in(&mut s, "11-D-0704", now);
    assert_eq!(s.input, "11-D-0704");
    assert!(!s.should_quit);
}

/// Ctrl+C copies the selection rather than ending the session.
#[test]
fn ctrl_c_copies_the_selected_text() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(shift(Key::Left), now);
    s.update(shift(Key::Left), now);

    let r = s.update(ctrl(Key::Char('c')), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Copy(text) if text == "04")),
        "{:?}",
        r.cmds
    );
    assert!(!s.should_quit);
}

/// With no text selected, Ctrl+C copies the selected row's path.
///
/// `view::actions` advertises `Ctrl+C` on "Copy the path" only while there is
/// no text selection, so the label always matches what the key does.
#[test]
fn ctrl_c_with_no_text_selected_copies_the_path() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let v = view(&s);
    s.update(search_result(&v, many_hits(3), 3, 9_000), now);
    assert!(s.input.selection().is_none(), "the fixture selected text");

    let r = s.update(ctrl(Key::Char('c')), now);
    assert!(!s.should_quit);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Copy(text) if text.contains(".pdf"))),
        "{:?}",
        r.cmds
    );
    assert_eq!(s.input, "11-D-0704", "the code must survive");
}

/// With nothing to copy, Ctrl+C shows a toast and the session continues.
#[test]
fn ctrl_c_with_nothing_to_copy_explains_itself_and_stays_running() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(ctrl(Key::Char('c')), now);
    assert!(!s.should_quit);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Nothing to copy")),
        "{:?}",
        s.toast
    );
    assert_eq!(s.input, "11-D-0704", "the code must survive");
}

/// Ctrl+X copies and removes the selection.
///
/// `gui::input` synthesizes Ctrl+`Char('x')` from the toolkit's `Event::Cut`,
/// which Shift+Delete also produces.
#[test]
fn ctrl_x_cuts_the_selected_text() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(shift(Key::Left), now);
    s.update(shift(Key::Left), now);

    let r = s.update(ctrl(Key::Char('x')), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Copy(text) if text == "04")),
        "{:?}",
        r.cmds
    );
    assert_eq!(
        s.input, "11-D-07",
        "the selection was copied but not removed"
    );
    assert!(!s.should_quit);
}

/// With nothing selected, Ctrl+X leaves the input alone and shows a toast.
#[test]
fn ctrl_x_with_no_selection_leaves_the_code_alone() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let r = s.update(ctrl(Key::Char('x')), now);
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::Copy(_))),
        "{:?}",
        r.cmds
    );
    assert_eq!(s.input, "11-D-0704", "the code must survive");
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Nothing selected")),
        "{:?}",
        s.toast
    );
}

/// Ctrl+Q is the only binding that quits; common keys and their Ctrl and
/// Shift forms do not.
#[test]
fn nothing_except_ctrl_q_quits_the_application() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let codes = [
        Key::Esc,
        Key::Enter,
        Key::Backspace,
        Key::Delete,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
        Key::Tab,
        Key::F(5),
        Key::Char('c'),
        Key::Char('d'),
        Key::Char('z'),
    ];
    for code in codes {
        for event in [press(code), ctrl(code), shift(code)] {
            let r = s.update(event, now);
            assert!(!s.should_quit, "{code:?} ended the session");
            assert!(
                !r.cmds.iter().any(|c| matches!(c, Cmd::Quit)),
                "{code:?} asked to quit"
            );
        }
    }

    // A bare `q` is text, not a command. Only the modified form leaves.
    s.update(press(Key::Char('q')), now);
    assert!(!s.should_quit, "typing q must not end the session");
    s.update(shift(Key::Char('q')), now);
    assert!(!s.should_quit, "Shift+Q must not end the session");
}

#[test]
fn ctrl_q_quits_and_asks_exactly_once() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let r = s.update(ctrl(Key::Char('q')), now);
    assert!(s.should_quit);
    assert_eq!(
        r.cmds.iter().filter(|c| matches!(c, Cmd::Quit)).count(),
        1,
        "one Quit command, not several"
    );
}

#[test]
fn ctrl_c_copies_rather_than_quitting() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let r = s.update(ctrl(Key::Char('c')), now);
    assert!(!s.should_quit);
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Quit)));
}

/// AltGr combinations type their character.
///
/// Windows reports AltGr as `CONTROL | ALT`; on German, Polish or French
/// layouts it produces `@`, `{`, `[` and accented letters, so the guard that
/// drops unbound Ctrl/Alt combinations must not catch it.
#[test]
fn altgr_types_a_character_rather_than_being_swallowed() {
    let (mut s, now) = state();
    let altgr = Mods::ALTGR;
    for c in ['@', '{', '[', 'é'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), altgr)), now);
    }
    assert_eq!(
        s.input, "@{[é",
        "AltGr characters were swallowed by the Ctrl/Alt guard"
    );
}

#[test]
fn a_control_combination_this_program_does_not_have_still_types_nothing() {
    let (mut s, now) = state();
    for c in ['w', 'x', 'z'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::CTRL)), now);
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALT)), now);
    }
    assert_eq!(s.input, "", "a lone Ctrl or Alt combination is not text");
}

#[test]
fn an_unbound_control_combination_does_not_type_a_letter() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D", now);
    for c in ['q', 'x', 'z', 'n', 'p'] {
        s.update(ctrl(Key::Char(c)), now);
    }
    assert_eq!(s.input, "11-D");
}

#[test]
fn enter_opens_the_selection() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a.pdf"), hit("b.pdf")], 2, 9),
        now,
    );

    let r = s.update(press(Key::Enter), now);
    // The request carries the typed code because the page set is rebuilt from
    // the code, not from the rows on screen.
    assert!(matches!(
        &r.cmds[0],
        Cmd::Open(req)
            if &*req.path == "V:\\a.pdf"
                && req.query == "11-D-0704"
                && req.viewer == ViewerKind::Auto
    ));
    assert_eq!(
        r.cmds.iter().filter(|c| matches!(c, Cmd::Open(_))).count(),
        1,
        "exactly one file is opened"
    );
    // Opening a result also saves the query to history.
    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::SaveHistory(_))),
        "{:?}",
        r.cmds
    );
}

#[test]
fn enter_with_no_results_reports_rather_than_opening() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    // Enter during the debounce flushes the search instead of acting on the
    // previous query's results; the report follows the result.
    let asked = s.update(press(Key::Enter), now);
    assert!(
        asked.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "Enter did not flush the pending search: {:?}",
        asked.cmds
    );
    assert!(s.toast.is_none(), "it reported before it had looked");

    let r = s.update(
        AppEvent::Search(SearchMsg {
            epoch: s.query_epoch(),
            query: Query::contains("11-D-0704"),
            elapsed: Duration::ZERO,
            result: Ok(SearchOutcome::default()),
        }),
        now,
    );
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Open(_))));
    assert!(s.toast.is_some());
}

/// `F5` opens the drive list instead of refreshing anything.
///
/// Refreshing every share at once from many machines would load the file
/// servers heavily, so refreshes are per drive.
#[test]
fn f5_opens_the_drive_list_rather_than_refreshing_everything() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let r = s.update(press(Key::F(5)), now);
    assert!(s.picking_share);
    assert!(
        r.cmds.is_empty(),
        "asking the question must not also answer it: {:?}",
        r.cmds
    );
}

#[test]
fn enter_in_the_drive_list_updates_only_that_drive() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::F(5)), now);
    let r = s.update(press(Key::Enter), now);

    let target = r.cmds.iter().find_map(|c| match c {
        Cmd::RefreshIndex {
            target,
            force: true,
        } => Some(*target),
        _ => None,
    });
    assert!(target.is_some(), "one drive, not all of them: {:?}", r.cmds);
    assert!(!s.picking_share, "and the list closes behind it");
    assert!(
        s.verify_due_at().is_some(),
        "an update bypasses the debounce"
    );
}

/// `A` in the drive list refreshes nothing; drives are refreshed one at a time.
#[test]
fn a_in_the_drive_list_refreshes_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::F(5)), now);
    let r = s.update(press(Key::Char('a')), now);

    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::RefreshIndex { .. })),
        "{:?}",
        r.cmds
    );
    assert!(!s.picking_share, "an unbound key still closes the list");
}

#[test]
fn escape_leaves_the_drive_list_without_updating_anything() {
    let (mut s, now) = state();
    s.update(press(Key::F(5)), now);
    let r = s.update(press(Key::Esc), now);
    assert!(!s.picking_share);
    assert!(r.cmds.is_empty(), "{:?}", r.cmds);
}

// --- selection --------------------------------------------------------

#[test]
fn the_first_result_is_selected_by_default() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a.pdf"), hit("b.pdf")], 2, 9),
        now,
    );
    assert_eq!(s.selected_row(), Some(0));
}

/// Up and Down wrap at both ends of the list.
#[test]
fn both_ends_of_the_list_wrap() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a"), hit("b"), hit("c")], 3, 3),
        now,
    );

    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(1));
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(2));
    s.update(press(Key::Down), now);
    assert_eq!(
        s.selected_row(),
        Some(0),
        "down from the bottom comes round"
    );

    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(2), "and up from the top goes back");
    assert!(!s.should_quit);
}

/// PageUp and PageDown stop at the ends rather than wrapping.
///
/// A wrapped page would land at an arbitrary row near the other end, which
/// is hard to tell apart from not moving.
#[test]
fn a_page_stops_at_the_ends_rather_than_wrapping() {
    let (mut s, now) = state();
    with_results(&mut s, now, 40);

    for _ in 0..20 {
        s.update(press(Key::PageDown), now);
    }
    assert_eq!(s.selected_row(), Some(39), "PageDown wrapped");

    for _ in 0..20 {
        s.update(press(Key::PageUp), now);
    }
    assert_eq!(s.selected_row(), Some(0), "PageUp wrapped");
}

/// A pinned selection follows its file when the results are reordered.
#[test]
fn a_pinned_selection_survives_a_result_update() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a"), hit("b"), hit("c")], 3, 3),
        now,
    );
    s.update(press(Key::Down), now);
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_hit().unwrap().name.as_ref(), "c");

    // The server reorders the same files.
    s.update(
        search_result(&view(&s), vec![hit("c"), hit("a"), hit("b")], 3, 3),
        now,
    );
    assert_eq!(
        s.selected_hit().unwrap().name.as_ref(),
        "c",
        "selection follows the file, not the row"
    );
    assert!(!s.selection_lost);
}

#[test]
fn a_pinned_selection_that_disappears_clamps_and_reports() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a"), hit("b"), hit("c")], 3, 3),
        now,
    );
    s.update(press(Key::Down), now); // row 1, "b"

    s.update(
        search_result(&view(&s), vec![hit("a"), hit("c")], 2, 2),
        now,
    );
    assert!(s.selection_lost, "the user should be told the list shifted");
    // The flag must also reach the status line.
    let line = files::view::status::render(&s, std::time::SystemTime::now());
    assert!(
        line.text.contains("File list changed"),
        "nothing on screen says so: {line:?}"
    );
    assert_eq!(
        s.selected_row(),
        Some(1),
        "clamped to the same row, not teleported"
    );
}

#[test]
fn typing_unpins_the_selection() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a"), hit("b")], 2, 2),
        now,
    );
    s.update(press(Key::Down), now);
    assert!(s.selection_pinned);
    s.update(key('x'), now);
    assert!(!s.selection_pinned);
}

// --- debounce and timers ----------------------------------------------

#[test]
fn verification_waits_for_a_pause_in_typing() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let early = s.update(
        AppEvent::Tick,
        now + VERIFY_DEBOUNCE - Duration::from_millis(1),
    );
    assert!(early.cmds.iter().all(|c| !matches!(c, Cmd::Verify { .. })));

    let due = s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(due.cmds.iter().any(|c| matches!(c, Cmd::Verify { .. })));
    assert!(s.phase.is_verifying());
}

#[test]
fn continued_typing_pushes_the_verify_deadline_out() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-070", now);
    let first = s.verify_due_at().unwrap();
    let later = now + Duration::from_millis(100);
    s.update(key('4'), later);
    assert!(s.verify_due_at().unwrap() > first);
}

#[test]
fn a_wedged_verification_is_broken_by_the_watchdog() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.phase.is_verifying());

    let later = now + VERIFY_DEBOUNCE + VERIFY_WATCHDOG;
    s.update(AppEvent::Tick, later);
    assert!(
        matches!(s.phase, QueryPhase::VerifyFailed { .. }),
        "the spinner must not run forever"
    );
}

#[test]
fn an_idle_state_has_no_deadline_so_the_loop_can_block() {
    let (s, _now) = state();
    assert_eq!(s.next_deadline(), None, "idle must cost zero wakeups");
}

// --- the clock ----------------------------------------------------------
//
// Time-derived text needs its own deadlines; otherwise the loop blocks
// indefinitely and the readout only updates when other input arrives.

fn publish(s: &mut AppState, id: MappingId, now: Instant, f: impl FnOnce(&mut IndexStatus)) {
    let mut status = IndexStatus::default();
    f(&mut status);
    s.update(
        AppEvent::Index(IndexMsg::Status {
            id,
            status: Arc::new(status),
        }),
        now,
    );
}

const EPOCH: std::time::SystemTime = std::time::SystemTime::UNIX_EPOCH;

#[test]
fn a_session_with_no_index_still_costs_zero_wakeups() {
    let (s, _now) = state();
    assert!(!s.shows_elapsed_text());
    assert_eq!(s.next_deadline(), None, "idle must stay free");
}

/// Index ages are shown only in the drive picker, so they schedule wakeups
/// only while it is open.
#[test]
fn a_loaded_index_costs_nothing_until_the_drive_list_is_open() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH + Duration::from_secs(10));
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
        st.entries = 5;
    });
    assert!(!s.shows_elapsed_text(), "an idle panel owes no frames");
    assert_eq!(s.next_deadline(), None);

    s.update(press(Key::F(5)), now);
    assert!(s.picking_share, "precondition");
    assert!(s.shows_elapsed_text());
    assert_eq!(
        s.next_deadline(),
        Some(now + Duration::from_secs(1)),
        "a ten-second-old index re-renders every second"
    );
}

/// At ten minutes old the readout says `10m`, so the next wakeup is aligned to
/// the minute rather than a flat 1 Hz tick.
#[test]
fn an_aged_index_wakes_on_the_minute_rather_than_every_second() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH + Duration::from_secs(617));
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
    });
    s.update(press(Key::F(5)), now);
    assert_eq!(s.next_deadline(), Some(now + Duration::from_secs(43)));
}

/// A deadline without a matching redraw is a busy spin: the loop wakes, gets
/// `Response::none()`, leaves `dirty` false and re-blocks on a past deadline.
#[test]
fn redrawing_advances_the_anchor_so_the_deadline_moves_forward() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH + Duration::from_secs(10));
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
    });
    s.update(press(Key::F(5)), now);

    let due = s.next_deadline().unwrap();
    assert_eq!(
        s.update(AppEvent::Tick, due).redraw,
        Redraw::Yes,
        "a deadline with no redraw arm is a busy spin"
    );
    s.note_frame(due, EPOCH + Duration::from_secs(11));
    assert!(
        s.next_deadline().unwrap() > due,
        "or the loop runs at 100% CPU"
    );
}

/// A tick before the readout changes must not redraw; redrawing on every tick
/// would amount to constant polling.
#[test]
fn a_tick_before_the_readout_changes_asks_for_nothing() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH + Duration::from_secs(10));
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
    });
    s.update(press(Key::F(5)), now);
    let due = s.next_deadline().unwrap();
    assert_eq!(
        s.update(AppEvent::Tick, due - Duration::from_millis(1))
            .redraw,
        Redraw::No
    );
}

#[test]
fn an_unreachable_share_counts_down_without_any_input() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH);
    publish(&mut s, MappingId(0), now, |st| {
        st.health = Health::Unreachable {
            err: EnumError::Transient(53),
            since: now,
            attempt: 1,
            next_retry_at: now + Duration::from_secs(42),
        };
    });
    assert!(s.shows_elapsed_text());
    assert_eq!(s.next_deadline(), Some(now + Duration::from_secs(1)));
}

#[test]
fn the_retry_moment_itself_is_a_deadline() {
    let (mut s, now) = state();
    s.note_frame(now, EPOCH);
    publish(&mut s, MappingId(0), now, |st| {
        st.health = Health::Unreachable {
            err: EnumError::Transient(53),
            since: now,
            attempt: 1,
            next_retry_at: now + Duration::from_millis(400),
        };
    });
    assert_eq!(s.next_deadline(), Some(now + Duration::from_millis(400)));
}

/// `next_retry_at` is an absolute instant, unlike the other deadlines. Once it
/// passes it must stop scheduling wakeups, or the loop redraws at full speed.
#[test]
fn an_overdue_retry_stops_costing_wakeups() {
    let (mut s, now) = state();
    publish(&mut s, MappingId(0), now, |st| {
        st.health = Health::Unreachable {
            err: EnumError::Transient(53),
            since: now,
            attempt: 1,
            next_retry_at: now + Duration::from_secs(42),
        };
    });
    s.note_frame(now + Duration::from_secs(43), EPOCH);
    assert_eq!(
        s.next_deadline(),
        None,
        "an absolute deadline in the past is a spin, not a countdown"
    );
}

// --- one status per share ------------------------------------------------

/// Each share keeps its own status; one share's update does not replace
/// another's.
#[test]
fn two_shares_publishing_statuses_do_not_overwrite_each_other() {
    let (mut s, now) = state();
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
        st.entries = 1_000_000;
    });
    publish(&mut s, MappingId(1), now, |st| {
        st.built_at = Some(EPOCH);
        st.entries = 284_551;
    });
    assert_eq!(s.index.entries, 1_284_551, "both shares are counted");
}

/// The aggregate age is that of the oldest share.
#[test]
fn the_aggregate_ages_from_the_oldest_share_not_the_newest() {
    let (mut s, now) = state();
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
    });
    publish(&mut s, MappingId(1), now, |st| {
        st.built_at = Some(EPOCH + Duration::from_secs(3600));
    });
    assert_eq!(s.index.built_at, Some(EPOCH));
    assert_eq!(s.index.oldest, Some(MappingId(0)));
}

#[test]
fn an_unreachable_share_is_named_even_when_another_is_healthy() {
    let (mut s, now) = state();
    publish(&mut s, MappingId(0), now, |st| {
        st.built_at = Some(EPOCH);
    });
    publish(&mut s, MappingId(1), now, |st| {
        st.health = Health::Unreachable {
            err: EnumError::PathNotFound(3),
            since: now,
            attempt: 1,
            next_retry_at: now + Duration::from_secs(30),
        };
    });
    assert_eq!(
        s.index.worst.as_ref().map(|(id, _)| *id),
        Some(MappingId(1))
    );
}

#[test]
fn a_status_for_an_unknown_mapping_is_dropped_rather_than_panicking() {
    let (mut s, now) = state();
    let r = s.update(
        AppEvent::Index(IndexMsg::Status {
            id: MappingId(99),
            status: Arc::new(IndexStatus::default()),
        }),
        now,
    );
    assert_eq!(r.redraw, Redraw::No);
}

/// The state is busy while any share is busy.
#[test]
fn busy_means_any_share_is_busy() {
    let (mut s, now) = state();
    publish(&mut s, MappingId(1), now, |st| {
        st.activity = Activity::Scanning { seen: 10 };
    });
    assert!(s.is_busy());
    publish(&mut s, MappingId(1), now, |st| {
        st.activity = Activity::Idle;
    });
    assert!(!s.is_busy());
}

#[test]
fn a_verifying_state_asks_for_animation_frames() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.is_busy());
    assert!(s.next_deadline().is_some());
}

#[test]
fn animation_stops_when_the_work_finishes() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.is_busy());

    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_millis(40),
            outcome: VerifyOutcome::IndexAuthoritative { stamp: None },
        }),
        now,
    );
    assert!(!s.is_busy(), "derived from state, never a sticky flag");
}

// --- verification outcomes --------------------------------------------

#[test]
fn an_unchanged_directory_verifies_without_touching_the_results() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), vec![hit("a.pdf")], 1, 5), now);

    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_millis(2),
            outcome: VerifyOutcome::IndexAuthoritative { stamp: None },
        }),
        now,
    );
    assert!(matches!(
        s.phase,
        QueryPhase::Verified { by_stamp: true, .. }
    ));
    assert_eq!(s.hits.len(), 1);
}

#[test]
fn a_failed_verification_keeps_the_local_results_on_screen() {
    let (mut s, now) = dev_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), vec![hit("a.pdf")], 1, 5), now);

    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_millis(20),
            outcome: VerifyOutcome::Failed(EnumError::Transient(53)),
        }),
        now,
    );
    assert_eq!(s.hits.len(), 1, "stale results beat an empty screen");
    match &s.phase {
        QueryPhase::VerifyFailed { detail } => assert!(detail.contains("53")),
        other => panic!("expected a reported failure, got {other:?}"),
    }
}

#[test]
fn a_server_answer_replaces_rather_than_unions() {
    // A union would keep showing files that have been deleted.
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("old.pdf"), hit("kept.pdf")], 2, 9),
        now,
    );

    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_millis(30),
            outcome: VerifyOutcome::Server {
                hits: vec![hit("kept.pdf")],
                matched: 1,
                capped: false,
                audit: AuditVerdict::Consistent,
            },
        }),
        now,
    );
    assert_eq!(s.hits.len(), 1);
    assert_eq!(s.hits[0].name.as_ref(), "kept.pdf");
}

#[test]
fn an_audit_failure_warns_the_user() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: Duration::from_millis(30),
            outcome: VerifyOutcome::Server {
                hits: vec![],
                matched: 0,
                capped: false,
                audit: AuditVerdict::ServerUnderReturned {
                    missing: vec!["a.pdf".into()],
                },
            },
        }),
        now,
    );
    let toast = s.toast.as_ref().expect("a silent downgrade would be worse");
    assert_eq!(toast.severity, Severity::Warn);
    assert!(toast.text.contains("Server filter"));
}

// --- honest emptiness --------------------------------------------------

#[test]
fn an_unreachable_drive_explains_itself_instead_of_showing_nothing() {
    let (mut s, now) = dev_state();
    let status = IndexStatus {
        health: Health::Unreachable {
            err: EnumError::Transient(53),
            since: now,
            attempt: 1,
            next_retry_at: now + Duration::from_secs(10),
        },
        ..Default::default()
    };
    s.update(
        AppEvent::Index(IndexMsg::Status {
            id: MappingId(0),
            status: Arc::new(status),
        }),
        now,
    );

    type_in(&mut s, "P12345", now);
    s.update(search_result(&view(&s), vec![], 0, 0), now);

    match &s.empty_reason {
        Some(EmptyReason::IndexUnavailable { detail }) => assert!(detail.contains("53")),
        other => panic!("expected an explanation, got {other:?}"),
    }
}

#[test]
fn a_genuine_no_match_says_how_much_was_searched() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), vec![], 0, 4321), now);
    assert_eq!(
        s.empty_reason,
        Some(EmptyReason::NoMatches { searched: 4321 })
    );
}

// --- background messages ----------------------------------------------

#[test]
fn a_new_snapshot_reruns_the_local_search_but_does_not_re_arm_verification() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    // Let the pending verification fire and settle first, so the
    // assertion below is about the snapshot event and nothing else.
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    let before = s.verify_due_at();

    let r = s.update(AppEvent::Index(IndexMsg::SnapshotChanged), now);
    assert!(r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })));
    assert_eq!(s.verify_due_at(), before, "re-arming here would livelock");
}

#[test]
fn a_refresh_report_is_shown_to_the_user() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Index(IndexMsg::RefreshReport {
            id: MappingId(0),
            entries: 1_284_551,
            elapsed: Duration::from_millis(8400),
            error: None,
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert!(t.text.contains("1,284,551"));
    assert_eq!(t.severity, Severity::Info);
}

#[test]
fn a_failed_open_is_reported_rather_than_swallowed() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::Failed {
            path: Arc::from(r"V:\a.pdf"),
            detail: "avwin.exe not found".into(),
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Error);
    assert!(t.text.contains("a.pdf"));
}

// --- the viewer -------------------------------------------------------

#[test]
fn f2_toggles_the_viewer_and_asks_for_it_to_be_saved() {
    let (mut s, now) = state();
    s.settings.have_file = true;
    assert_eq!(s.viewer, ViewerKind::Auto, "the default opens by file type");

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf);
    assert_eq!(r.redraw, Redraw::Yes, "the help line changes");
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveViewer(name) if name.as_ref() == "pdf")),
        "{:?}",
        r.cmds
    );

    // Cycle through every mode so an unreachable one fails here.
    s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::App(0));
    assert!(
        s.update(press(Key::F(2)), now)
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveViewer(name) if name.as_ref() == "auto"))
    );
    assert_eq!(s.viewer, ViewerKind::Auto, "it cycles back");
}

#[test]
fn f2_without_viewers_toggles_between_the_built_in_two() {
    let now = Instant::now();
    let mut s = AppState::new(Settings::fixture(), now);
    s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf);
    s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Auto);
}

/// `Ctrl+E` opens with the first configured viewer, and does nothing when
/// none is configured.
#[test]
fn ctrl_e_opens_with_the_first_viewer_and_only_if_there_is_one() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), vec![hit("a.dwg")], 1, 9), now);
    let r = s.update(ctrl(Key::Char('e')), now);
    let opened = r.cmds.iter().find_map(|c| match c {
        Cmd::Open(req) => Some(req.viewer),
        _ => None,
    });
    assert_eq!(opened, Some(ViewerKind::App(0)), "{:?}", r.cmds);

    let mut none = AppState::new(Settings::fixture(), now);
    type_in(&mut none, "11-D-0704", now);
    none.update(search_result(&view(&none), vec![hit("a.dwg")], 1, 9), now);
    let r = none.update(ctrl(Key::Char('e')), now);
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::Open(_))),
        "{:?}",
        r.cmds
    );
}

/// A key release must not toggle; a double toggle looks like a broken key.
#[test]
fn f2_on_key_release_does_not_toggle_twice() {
    let (mut s, now) = state();
    let mut ev = KeyEvent::new(Key::F(2), Mods::NONE);
    ev.phase = KeyPhase::Release;
    s.update(AppEvent::Key(ev), now);
    assert_eq!(s.viewer, ViewerKind::Auto, "a release must change nothing");
}

/// With `FILES_VIEWER` or `--viewer` set, the config value is ignored at the
/// next start, so F2 applies for the session only and saves nothing.
#[test]
fn f2_says_so_when_the_choice_cannot_be_persisted() {
    let (mut s, now) = state();
    s.settings.have_file = false;

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf, "it still applies");
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::SaveViewer(_))),
        "nothing should be written"
    );
    let t = s.toast.as_ref().unwrap();
    assert!(t.text.contains("session only"), "{}", t.text);
}

/// During history recall, any key other than the arrows, Enter and Esc commits
/// the highlighted entry. F2 is the exception: it changes the viewer without
/// committing the entry or running a search.
#[test]
fn f2_during_recall_changes_the_viewer_without_accepting_the_code() {
    let (mut s, now) = state();
    s.seed_history(vec!["11-D-0704".into(), "AB12-0704".into()]);
    type_in(&mut s, "99-", now);

    s.update(press(Key::Up), now);
    let recalled = s.input.text().to_string();
    let epoch = s.query_epoch();

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf, "the viewer still toggles");
    assert_eq!(s.input.text(), recalled, "the entry is not committed");
    assert_eq!(s.query_epoch(), epoch, "and nothing is searched for");
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })));
}

#[test]
fn f2_does_not_disturb_the_query_or_the_results() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a.pdf"), hit("b.pdf")], 2, 9),
        now,
    );
    let epoch = s.query_epoch();
    let selected = s.selected_path.clone();

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.query_epoch(), epoch);
    assert_eq!(s.hits.len(), 2);
    assert_eq!(s.selected_path, selected);
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })));
}

#[test]
fn enter_carries_the_viewer_that_is_active_now_not_the_one_configured_at_startup() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), vec![hit("a.pdf")], 1, 9), now);
    s.update(press(Key::F(2)), now);

    let r = s.update(press(Key::Enter), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Open(req) if req.viewer == ViewerKind::Pdf)),
        "{:?}",
        r.cmds
    );
}

/// A partially assembled drawing set warns and names the skipped pages.
#[test]
fn a_partial_assembly_warns_and_names_what_was_skipped() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::Launched {
            path: Arc::from(r"C:\cache\pdf\abc.pdf"),
            pages: 12,
            skipped: vec!["11-D-0704_Page7.pdf (could not be read)".into()],
            truncated: false,
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Warn);
    assert!(t.text.contains("12 of 13"), "{}", t.text);
    assert!(t.text.contains("Page7"), "{}", t.text);
}

#[test]
fn a_complete_assembly_says_nothing() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::Launched {
            path: Arc::from(r"C:\cache\pdf\abc.pdf"),
            pages: 13,
            skipped: Vec::new(),
            truncated: false,
        }),
        now,
    );
    assert!(s.toast.is_none(), "{:?}", s.toast);
}

/// A drawing set longer than the page limit opens truncated, with a warning.
#[test]
fn a_truncated_document_says_the_set_is_longer() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::Launched {
            path: Arc::from(r"C:\cache\pdf\merged.pdf"),
            pages: 512,
            skipped: Vec::new(),
            truncated: true,
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Warn);
    assert!(t.text.contains("512"), "{}", t.text);
    assert!(t.text.contains("longer"), "{}", t.text);
}

#[test]
fn a_saved_viewer_is_confirmed_on_screen() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::ViewerSaved {
            name: "avwin".into(),
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Info);
    assert!(t.text.contains("avwin"), "{}", t.text);
}

/// The toggle still applies, so a failed save is a warning, not an error.
#[test]
fn a_failed_viewer_save_warns_rather_than_ending_the_session() {
    let (mut s, now) = dev_state();
    s.update(
        AppEvent::Open(OpenMsg::ViewerSaveFailed {
            detail: "access is denied".into(),
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Warn);
    assert!(t.text.contains("session only"), "{}", t.text);
    assert!(t.text.contains("access is denied"), "{}", t.text);
}

#[test]
fn a_dead_worker_clears_the_spinner_and_says_so() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.phase.is_verifying());

    s.update(
        AppEvent::ActorDied {
            actor: "verify",
            detail: "panicked".into(),
        },
        now,
    );
    assert!(!s.phase.is_verifying());
    assert_eq!(s.toast.as_ref().unwrap().severity, Severity::Error);
}

#[test]
fn toasts_expire() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Open(OpenMsg::Failed {
            path: Arc::from(r"V:\a.pdf"),
            detail: "boom".into(),
        }),
        now,
    );
    assert!(s.toast.is_some());
    s.update(AppEvent::Tick, now + TOAST_LIFETIME);
    assert!(s.toast.is_none());
}

#[test]
fn a_busy_index_keeps_the_frame_animating() {
    let (mut s, now) = state();
    let status = IndexStatus {
        activity: Activity::Scanning { seen: 1000 },
        ..Default::default()
    };
    s.update(
        AppEvent::Index(IndexMsg::Status {
            id: MappingId(0),
            status: Arc::new(status),
        }),
        now,
    );
    assert!(s.is_busy());
}

/// A snapshot of the fields `search_result` needs, so a result can be built
/// while the state is mutably borrowed.
fn view(s: &AppState) -> AppStateView {
    AppStateView {
        query_epoch: s.query_epoch(),
        input: s.input.text().to_string(),
    }
}

struct AppStateView {
    query_epoch: u64,
    input: String,
}

fn search_result(view: &AppStateView, hits: Vec<Hit>, matched: u32, total: u32) -> AppEvent {
    AppEvent::Search(SearchMsg {
        epoch: view.query_epoch,
        query: Query::parse(&view.input),
        elapsed: Duration::from_micros(400),
        result: Ok(SearchOutcome {
            hits,
            matched,
            total,
            cancelled: false,
            unicode_fallback: false,
        }),
    })
}

// --- the results list --------------------------------------------------

fn with_results(s: &mut AppState, now: Instant, n: usize) {
    type_in(s, "11-D-0704", now);
    let v = view(s);
    s.update(search_result(&v, many_hits(n), n as u32, 9_000), now);
}

fn many_hits(n: usize) -> Vec<Hit> {
    (0..n).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect()
}

/// Down walks the list one rank at a time.
#[test]
fn down_walks_the_list_and_stops_at_the_end() {
    let (mut s, now) = state();
    with_results(&mut s, now, 4);

    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(1), "the top row was already on");
    for _ in 0..10 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(3), "walked off the end");
}

/// Up from the first row wraps to the last.
#[test]
fn up_at_the_top_of_the_list_comes_round_to_the_end() {
    let (mut s, now) = state();
    with_results(&mut s, now, 4);

    s.update(press(Key::Down), now);
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(0));

    let again = s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(3), "the top must come round");
    assert_eq!(again.redraw, Redraw::Yes, "and it is a new frame");
}

/// On an empty line, Down steps into the shortcut list (Up steps into history),
/// and Enter takes the highlighted shortcut.
#[test]
fn down_steps_into_the_shortcuts_and_enter_takes_one() {
    let now = Instant::now();
    let mut s = AppState::new(with_aliases(), now);
    assert!(s.showing_aliases(), "the fixture shows no shortcuts");
    assert_eq!(s.alias_cursor(), None, "and nobody has stepped onto one");

    s.update(press(Key::Down), now);
    assert_eq!(s.alias_cursor(), Some(0));
    s.update(press(Key::Down), now);
    assert_eq!(s.alias_cursor(), Some(1));
    s.update(press(Key::Down), now);
    assert_eq!(s.alias_cursor(), Some(0), "the list did not come round");
    s.update(press(Key::Up), now);
    assert_eq!(s.alias_cursor(), Some(1));

    // Enter inserts the shortcut's name, so it expands the same way as typed
    // text.
    s.update(press(Key::Enter), now);
    assert_eq!(s.input.text(), "gd");
    assert_eq!(s.alias_cursor(), None, "the list stayed up under the code");
}

/// The first Escape leaves the shortcut list; only the second dismisses the
/// panel, as with the drive picker and the recall list.
#[test]
fn escape_steps_off_the_shortcuts_first() {
    let now = Instant::now();
    let mut s = AppState::new(with_aliases(), now);
    s.update(
        AppEvent::Hotkey(files::app::event::HotkeyMsg::Summoned),
        now,
    );
    s.update(press(Key::Down), now);
    assert_eq!(s.alias_cursor(), Some(0));

    let r = s.update(press(Key::Esc), now);
    assert_eq!(s.alias_cursor(), None);
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::DismissOverlay)),
        "Escape took the panel with it: {:?}",
        r.cmds
    );

    let r = s.update(press(Key::Esc), now);
    assert!(r.cmds.iter().any(|c| matches!(c, Cmd::DismissOverlay)));
}

#[test]
fn typing_steps_off_the_shortcuts() {
    let now = Instant::now();
    let mut s = AppState::new(with_aliases(), now);
    s.update(press(Key::Down), now);
    assert_eq!(s.alias_cursor(), Some(0));

    s.update(press(Key::Char('1')), now);
    assert_eq!(s.alias_cursor(), None);
    assert!(!s.showing_aliases(), "the box is not empty any more");
}

/// Settings with two shortcuts.
fn with_aliases() -> Settings {
    Settings {
        aliases: std::sync::Arc::new(files::alias::Aliases::new(vec![
            files::alias::Alias {
                name: "pw".into(),
                code: "11-D-0704".into(),
                note: Some("the pump house".into()),
            },
            files::alias::Alias {
                name: "gd".into(),
                code: "22-A-1234".into(),
                note: None,
            },
        ])),
        ..Settings::fixture()
    }
}

/// Ctrl+P and Ctrl+N behave exactly like Up and Down.
#[test]
fn the_home_row_arrows_are_the_arrows() {
    for (chord, arrow) in [(Key::Char('n'), Key::Down), (Key::Char('p'), Key::Up)] {
        let (mut chorded, now) = state();
        let (mut arrowed, _) = state();
        with_results(&mut chorded, now, 6);
        with_results(&mut arrowed, now, 6);

        for _ in 0..8 {
            chorded.update(ctrl(chord), now);
            arrowed.update(press(arrow), now);
        }
        assert_eq!(
            chorded.selected_row(),
            arrowed.selected_row(),
            "{chord:?} is not {arrow:?}"
        );
    }
}

/// Ctrl+L selects the whole input, as focusing a search box does elsewhere.
#[test]
fn ctrl_l_selects_the_whole_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    assert!(s.input.selection().is_none(), "the fixture selected text");

    s.update(ctrl(Key::Char('l')), now);
    assert_eq!(s.input.selected_text(), Some("11-D-0704"));
}

/// Up and Down move the selection; Left and Right always move the caret.
#[test]
fn left_and_right_always_move_the_caret() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);

    for _ in 0..5 {
        s.update(press(Key::Down), now);
    }
    let row = s.selected_row();

    s.update(press(Key::Left), now);
    let before = s.query_epoch();
    let typed = s.update(key('X'), now);

    assert_eq!(s.input.text(), "11-D-070X4", "the caret moved, not the row");
    // Typing keeps the results and debounces the search, so the check is that
    // the keystroke re-armed it. See
    // `typing_keeps_the_results_until_the_new_ones_arrive`.
    assert!(
        typed.cmds.is_empty(),
        "typing dispatched rather than arming: {:?}",
        typed.cmds
    );
    assert!(
        s.search_due_at().is_some(),
        "typing did not re-arm the search, as it must"
    );
    assert_ne!(
        s.query_epoch(),
        before,
        "and superseded whatever was still in flight"
    );
    let _ = row;
}

/// Results stay on screen until their replacement arrives.
///
/// The search is answered on another thread, so clearing the list on each
/// keystroke would collapse the panel between frames. This is safe because
/// `on_search` is epoch-guarded and `apply_hits` replaces wholesale.
#[test]
fn typing_keeps_the_results_until_the_new_ones_arrive() {
    let (mut s, now) = state();
    with_results(&mut s, now, 6);
    let showing = s.hits.len();
    assert_eq!(showing, 6, "precondition");

    s.update(key('4'), now);

    assert_eq!(s.phase, QueryPhase::LocalPending, "a search is in flight");
    assert_eq!(
        s.hits.len(),
        showing,
        "the results were thrown away before their replacement existed"
    );
    assert!(
        s.selected_row().is_some(),
        "and the selection went with them"
    );

    // The answer replaces them rather than merging.
    let v = view(&s);
    s.update(search_result(&v, many_hits(2), 2, 9_000), now);
    assert_eq!(s.hits.len(), 2);
}

/// A line with nothing to search for clears the results.
#[test]
fn a_query_that_stops_being_one_does_clear_the_results() {
    let (mut s, now) = state();
    with_results(&mut s, now, 6);

    for _ in 0..9 {
        s.update(press(Key::Backspace), now);
    }

    assert_eq!(s.input.text(), "");
    assert_eq!(s.phase, QueryPhase::Idle);
    assert!(s.hits.is_empty(), "the last code's results outlived it");
    assert_eq!(s.selected_row(), None);
}

#[test]
fn paging_reaches_the_ends_of_the_list() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);

    s.update(press(Key::PageDown), now);
    let after = s.selected_row().expect("paging selects something");
    assert!(after > 0, "PageDown moved nowhere");

    for _ in 0..200 {
        s.update(press(Key::PageUp), now);
    }
    assert_eq!(s.selected_row(), Some(0), "PageUp did not reach the top");
}

/// Holding Down walks the whole list, past the first screenful, and wraps.
#[test]
fn holding_down_walks_the_whole_list_rather_than_freezing_on_one_row() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);

    // To the last row of the first screenful.
    for _ in 0..VISIBLE_ROWS - 1 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(VISIBLE_ROWS - 1));

    // And one past it.
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(VISIBLE_ROWS));

    // Exactly to the end; an overshoot would wrap.
    for _ in 0..(199 - VISIBLE_ROWS) {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(199));

    // One more wraps to the top, and Up wraps back.
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(0), "the foot did not come round");
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(199));
}

/// When the list shrinks, the cursor is clamped into range.
#[test]
fn a_shorter_result_set_pulls_the_cursor_back() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);
    for _ in 0..199 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(199), "precondition");

    // The server answers with far fewer.
    let v = view(&s);
    s.update(
        search_result(&v, many_hits(VISIBLE_ROWS + 2), 14, 9_000),
        now,
    );

    assert_eq!(s.hits.len(), VISIBLE_ROWS + 2);
    let row = s.selected_row().expect("nothing is selected at all");
    assert!(
        row < s.hits.len(),
        "the cursor is on rank {row} of a list of {}",
        s.hits.len()
    );
}

/// The last of `MAX_RESULTS` results is reachable.
#[test]
fn far_more_than_one_screenful_of_results_is_reachable() {
    let (mut s, now) = state();
    with_results(&mut s, now, files::config::MAX_RESULTS);

    // Up from the first row wraps to the last.
    s.update(press(Key::Up), now);
    assert_eq!(
        s.selected_row(),
        Some(files::config::MAX_RESULTS - 1),
        "the last of {} results is reachable",
        files::config::MAX_RESULTS
    );
}

// --- more than one column -----------------------------------------------

/// A state whose results are laid out `columns` to a line.
fn in_columns(columns: usize) -> (AppState, Instant) {
    let now = Instant::now();
    let settings = Settings {
        columns,
        ..Settings::fixture()
    };
    (AppState::new(settings, now), now)
}

/// Down moves a line and stays in its column, and Up comes back.
///
/// ```text
///  0  1  2
///  3  4  5
///  ...
/// ```
#[test]
fn in_columns_down_and_up_stay_in_the_column() {
    let (mut s, now) = in_columns(3);
    with_results(&mut s, now, 30);
    assert_eq!(s.selected_row(), Some(0));

    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(3));
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(6));
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(3));
}

/// Tab moves one column right on the same line; Up and Down then stay in the
/// new column.
#[test]
fn tab_moves_to_the_same_line_of_the_next_column() {
    let (mut s, now) = in_columns(3);
    with_results(&mut s, now, 30);
    s.update(press(Key::Down), now);
    s.update(press(Key::Down), now);
    assert_eq!(
        s.selected_row(),
        Some(6),
        "precondition: third line, first column"
    );

    s.update(press(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(7), "the same line, one column over");

    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(10), "Down stayed in the new column");
}

#[test]
fn shift_tab_moves_to_the_same_line_of_the_previous_column() {
    let (mut s, now) = in_columns(3);
    with_results(&mut s, now, 30);
    s.update(press(Key::Down), now);
    s.update(press(Key::Tab), now);
    s.update(press(Key::Tab), now);
    assert_eq!(
        s.selected_row(),
        Some(5),
        "precondition: second line, last column"
    );

    s.update(shift(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(4));
    s.update(shift(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(3));
}

/// Tab and Shift+Tab stop at the ends of a line rather than wrapping.
#[test]
fn tab_holds_at_the_ends_of_the_line() {
    let (mut s, now) = in_columns(2);
    with_results(&mut s, now, 10);

    s.update(shift(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(0), "Shift+Tab left the first column");

    s.update(press(Key::Tab), now);
    s.update(press(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(1), "Tab left the last column");
}

#[test]
fn tab_leaves_a_single_column_alone() {
    let (mut s, now) = state();
    with_results(&mut s, now, 10);
    s.update(press(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(0));
}

/// Up and Down wrap within the current column, never into another one.
#[test]
fn in_columns_the_wrap_keeps_to_the_column() {
    let (mut s, now) = in_columns(3);
    // Eight results: the last line is two short of full.
    with_results(&mut s, now, 8);
    s.update(press(Key::Tab), now);
    s.update(press(Key::Tab), now);
    assert_eq!(s.selected_row(), Some(2), "precondition: the third column");

    // The column's last row is on the second-to-last line.
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(5));
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(2));
}

/// A page moves `VISIBLE_ROWS` lines, which is `VISIBLE_ROWS * columns`
/// results, and keeps the column.
#[test]
fn in_columns_a_page_moves_a_screen_of_lines_and_keeps_the_column() {
    let (mut s, now) = in_columns(2);
    with_results(&mut s, now, 100);
    s.update(press(Key::Tab), now);

    s.update(press(Key::PageDown), now);
    assert_eq!(s.selected_row(), Some(1 + VISIBLE_ROWS * 2));
}

// --- the debounce window -------------------------------------------------

/// Enter before the debounce elapses must not open the row on screen, which
/// belongs to the previous query; it waits for the new result instead.
#[test]
fn enter_before_the_search_fires_opens_the_new_code_not_the_old_one() {
    let (mut s, now) = state();

    type_in(&mut s, "11-D-070", now);
    let v = view(&s);
    s.update(
        search_result(&v, vec![hit("11-D-070-OLD.pdf")], 1, 9_000),
        now,
    );
    assert_eq!(s.hits.len(), 1);

    s.update(key('4'), now);
    assert!(s.search_due_at().is_some());
    let pressed = s.update(press(Key::Enter), now);

    assert!(
        !pressed.cmds.iter().any(|c| matches!(c, Cmd::Open(_))),
        "Enter opened a row belonging to the previous code"
    );
    assert!(
        pressed.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "Enter did not ask the matcher: {:?}",
        pressed.cmds
    );

    // The new result is what gets opened.
    let v = view(&s);
    let r = s.update(
        search_result(&v, vec![hit("11-D-0704-NEW.pdf")], 1, 9_000),
        now,
    );
    let opened = r.cmds.iter().find_map(|c| match c {
        Cmd::Open(req) => Some(format!("{req:?}")),
        _ => None,
    });
    let opened = opened.expect("the answer did not open anything");
    assert!(
        opened.contains("11-D-0704-NEW"),
        "opened the wrong file: {opened}"
    );
}

#[test]
fn an_edit_between_enter_and_the_answer_cancels_the_open() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::Enter), now);

    let stale = view(&s);
    s.update(key('9'), now);
    let r = s.update(search_result(&stale, vec![hit("gone.pdf")], 1, 9_000), now);
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::Open(_))),
        "a superseded answer opened a file"
    );
}

/// If the matcher never answers a pending Enter, the watchdog reports it.
#[test]
fn a_matcher_that_never_answers_an_enter_gives_the_keystroke_back() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::Enter), now);
    assert!(s.toast.is_none());

    s.update(AppEvent::Tick, now + ENTER_WATCHDOG);
    assert!(s.toast.is_some(), "the keystroke was swallowed");
}

/// A local result does not move the phase back out of verifying.
///
/// Both debounces are 300 ms, so they fall due on the same tick and either
/// can answer first.
#[test]
fn the_local_answer_does_not_walk_the_phase_back_from_verifying() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let due = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, due);
    assert!(s.phase.is_verifying(), "the verification did not start");

    let v = view(&s);
    s.update(search_result(&v, vec![hit("a.pdf")], 1, 9_000), due);
    assert!(
        s.phase.is_verifying(),
        "the local answer blinked the spinner off: {:?}",
        s.phase
    );
    assert_eq!(s.hits.len(), 1, "and it still applied the results");
}

/// Clearing the field disarms every deadline.
///
/// The epoch guard does not cover this: a late dispatch would carry the
/// current epoch, so its answer would be accepted.
#[test]
fn clearing_the_field_stands_every_clock_down() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    assert!(s.search_due_at().is_some());

    for _ in 0..9 {
        s.update(press(Key::Backspace), now);
    }
    assert!(s.input.is_empty());
    assert!(s.search_due_at().is_none(), "a search was left armed");
    assert!(s.verify_due_at().is_none());
    assert!(s.next_deadline().is_none(), "the loop cannot block");
}

/// A paste is complete input, so it is searched immediately without debounce.
#[test]
fn a_paste_is_searched_for_at_once_rather_than_debounced() {
    let (mut s, now) = state();
    let r = s.update(AppEvent::Paste("11-D-0704".into()), now);
    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "a paste waited out the pause: {:?}",
        r.cmds
    );
    assert!(s.search_due_at().is_none());
}

// --- the house style ----------------------------------------------------

/// Raises every toast the state machine can produce and returns their text.
///
/// Toasts are drawn by `view::status` as status text, so they follow the same
/// style. Add each new `set_toast` call site here.
fn every_toast() -> Vec<String> {
    let mut out = Vec::new();
    let mut raised = |s: &AppState| {
        out.push(
            s.toast
                .as_ref()
                .expect("this step raised no toast, so it checks nothing")
                .text
                .clone(),
        )
    };

    // Nothing to copy, and nothing to open.
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('c')), now);
    raised(&s);

    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), Vec::new(), 0, 9_000), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    // An open in progress. Uses `Pdf` because it is the only mode that
    // assembles a document; `Auto` hands off to the system immediately.
    let (mut s, now) = state();
    s.viewer = ViewerKind::Pdf;
    with_results(&mut s, now, 4);
    s.update(press(Key::Down), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    // Clipboard outcomes.
    for msg in [
        ClipboardMsg::Copied { chars: 1 },
        ClipboardMsg::Copied { chars: 9 },
        ClipboardMsg::Read { text: "   ".into() },
        ClipboardMsg::Failed {
            detail: "the clipboard was held by another program".into(),
        },
    ] {
        let (mut s, now) = state();
        s.update(AppEvent::Clipboard(msg), now);
        raised(&s);
    }

    // Open outcomes other than a clean launch.
    for msg in [
        OpenMsg::Launched {
            path: "R:\\11d\\a.pdf".into(),
            pages: 8,
            skipped: vec!["page 3 is not a PDF".into()],
            truncated: false,
        },
        OpenMsg::Launched {
            path: "R:\\11d\\a.pdf".into(),
            pages: 64,
            skipped: Vec::new(),
            truncated: true,
        },
        OpenMsg::Failed {
            path: "R:\\11d\\a.pdf".into(),
            detail: "the file no longer exists".into(),
        },
        OpenMsg::ViewerSaveFailed {
            detail: "the configuration file is read-only".into(),
        },
    ] {
        let (mut s, now) = state();
        s.update(AppEvent::Open(msg), now);
        raised(&s);
    }

    // Every viewer, saved and not saveable.
    for viewer in [ViewerKind::Auto, ViewerKind::Pdf, ViewerKind::App(0)] {
        let (mut s, now) = state();
        let name = viewer.name(&s.settings.viewers).into();
        s.update(AppEvent::Open(OpenMsg::ViewerSaved { name }), now);
        raised(&s);

        let mut unsaveable = AppState::new(
            Settings {
                have_file: false,
                ..with_avwin()
            },
            now,
        );
        unsaveable.viewer = viewer;
        unsaveable.update(press(Key::F(2)), now);
        raised(&unsaveable);
    }

    let (mut s, now) = state();
    s.update(
        AppEvent::ActorDied {
            actor: "index",
            detail: "attempt to subtract with overflow".into(),
        },
        now,
    );
    raised(&s);

    // The hotkey could not be registered.
    let (mut s, now) = state();
    s.update(
        AppEvent::Hotkey(files::app::event::HotkeyMsg::Unavailable {
            reason: "Ctrl+Shift+Space is already claimed \u{b7} set hotkey in config.toml to \
                     another combination, or to \"off\""
                .into(),
        }),
        now,
    );
    raised(&s);

    // A refresh, succeeding and failing.
    for error in [None, Some(EnumError::Transient(53))] {
        let (mut s, now) = state();
        s.update(
            AppEvent::Index(IndexMsg::RefreshReport {
                id: MappingId(0),
                entries: 812_000,
                elapsed: Duration::from_millis(1_200),
                error,
            }),
            now,
        );
        raised(&s);
    }

    // The drive picker refreshing one drive.
    let (mut s, now) = state();
    s.update(press(Key::F(5)), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    // A search that never returned. Enter must arrive while a search is still
    // pending, since that is what arms the watchdog.
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::Enter), now);
    s.update(
        AppEvent::Tick,
        now + ENTER_WATCHDOG + Duration::from_secs(1),
    );
    let watchdog = s.toast.as_ref().expect("the watchdog said nothing");
    assert!(
        watchdog.text.contains("did not answer"),
        "{:?}",
        watchdog.text
    );
    raised(&s);

    out
}

#[test]
fn every_toast_keeps_the_house_style() {
    let toasts = every_toast();
    files::view::style::check_all(
        "the toasts",
        toasts.iter().map(String::as_str),
        files::view::style::Slot::Status,
    );
}

/// Every toast starts with a capital letter.
///
/// The exception is `<actor> stopped unexpectedly`, where the actor name is an
/// internal identifier printed verbatim, matching the log.
#[test]
fn every_toast_starts_the_way_a_line_should() {
    for toast in every_toast() {
        if toast.contains("stopped unexpectedly") {
            continue;
        }
        assert!(
            files::view::style::starts_capitalised(&toast),
            "the toast {toast:?} does not start a sentence"
        );
    }
}

// --- narrowing what is already on screen ------------------------------------

/// F3 and F4 rewrite the input text rather than holding a separate filter, so
/// the whole query is visible, recalled and copied with the line.
#[test]
fn f3_writes_the_narrowing_into_the_search_box() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(press(Key::F(3)), now);
    assert_eq!(s.input.text(), "11-D-0704*");
    s.update(press(Key::F(3)), now);
    assert_eq!(s.input.text(), "*11-D-0704");
    s.update(press(Key::F(3)), now);
    assert_eq!(s.input.text(), "11-D-0704", "the cycle has to come back");
}

#[test]
fn f4_steps_through_the_file_types_and_back_to_all_of_them() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(press(Key::F(4)), now);
    assert_eq!(s.input.text(), "11-D-0704 ext:pdf");
    s.update(press(Key::F(4)), now);
    assert_eq!(s.input.text(), "11-D-0704 ext:dwg");
    s.update(press(Key::F(4)), now);
    assert_eq!(s.input.text(), "11-D-0704");
}

#[test]
fn the_two_narrowings_compose_without_either_dropping_the_other() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(press(Key::F(4)), now);
    s.update(press(Key::F(3)), now);
    assert_eq!(s.input.text(), "11-D-0704* ext:pdf");
}

/// Narrowing is a complete edit, so the search is dispatched without debounce,
/// as with a paste.
#[test]
fn narrowing_dispatches_the_search_at_once_rather_than_waiting_out_the_pause() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let response = s.update(press(Key::F(3)), now);
    assert!(
        response
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Search { .. })),
        "F3 left the search sitting on the debounce"
    );
    assert!(s.search_due_at().is_none());
}

/// F4 clears a typed `ext:` filter that is not in its cycle.
#[test]
fn a_hand_typed_type_the_key_does_not_know_is_cleared_rather_than_guessed_at() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704 ext:sldprt", now);

    s.update(press(Key::F(4)), now);
    assert_eq!(s.input.text(), "11-D-0704");
}

/// The dispatched query carries the type filter added by F4.
#[test]
fn a_narrowed_search_is_dispatched_with_its_filter_intact() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let response = s.update(press(Key::F(4)), now);

    let dispatched: Vec<Query> = response
        .cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Search { query, .. } => Some(query.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].term(), "11-D-0704");
    assert_eq!(dispatched[0].types().len(), 1);
}

/// A `*` inside a term is reported as a bad query; it cannot occur in a
/// Windows filename, so a literal search could never match.
#[test]
fn a_star_in_the_middle_of_a_code_is_explained_rather_than_searched_for() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D*0704", now);

    assert!(
        matches!(s.phase, QueryPhase::BadQuery { .. }),
        "got {:?}",
        s.phase
    );
    assert!(matches!(s.empty_reason, Some(EmptyReason::BadQuery { .. })));
}

/// The minimum length applies to the search term, not the whole line:
/// `  ext:pdf` has nine characters but no term.
#[test]
fn the_minimum_length_is_judged_on_the_code_and_not_on_the_whole_line() {
    let (mut s, now) = state();
    type_in(&mut s, "  ext:pdf", now);
    assert_eq!(s.phase, QueryPhase::Idle);
}

// --- shares that are asked rather than indexed -------------------------------

/// A state with one indexed share and one live one.
fn live_state() -> (AppState, Instant) {
    use files::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};
    let mappings = vec![
        Mapping {
            id: MappingId(0),
            name: "custompro".into(),
            path: std::path::PathBuf::from("V:\\Documents\\custpro"),
            kind: MappingKind::Flat,
            enabled: true,
            refresh: RefreshPolicy::Auto,
            depth: files::config::DEFAULT_LIVE_DEPTH,
        },
        Mapping {
            id: MappingId(1),
            name: "archive".into(),
            path: std::path::PathBuf::from("\\\\nas\\archive"),
            kind: MappingKind::Live,
            enabled: true,
            refresh: RefreshPolicy::Manual,
            depth: 1,
        },
    ];
    let routes = Routes::new(mappings, ConfigSource::BuiltIn);
    let settings = Settings::with_routes(Arc::new(routes), |s| s);
    let now = Instant::now();
    (AppState::new(settings, now), now)
}

fn live_hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("\\\\nas\\archive\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

fn answered(hits: Vec<Hit>, skipped: u32) -> LiveOutcome {
    let matched = hits.len() as u32;
    LiveOutcome::Answered {
        hits,
        matched,
        coverage: LiveCoverage {
            dirs_queried: 1,
            dirs_skipped: skipped,
            round_trips: 1,
            ..LiveCoverage::default()
        },
    }
}

fn live_msg(s: &AppState, outcome: LiveOutcome) -> AppEvent {
    AppEvent::Live(LiveMsg {
        epoch: s.query_epoch(),
        query: Query::parse(s.input.text()),
        mapping: MappingId(1),
        elapsed: Duration::ZERO,
        outcome: Box::new(outcome),
    })
}

fn local_answer(s: &AppState, hits: Vec<Hit>) -> AppEvent {
    let matched = hits.len() as u32;
    AppEvent::Search(SearchMsg {
        epoch: s.query_epoch(),
        query: Query::parse(s.input.text()),
        elapsed: Duration::ZERO,
        result: Ok(SearchOutcome {
            hits,
            matched,
            total: 10,
            cancelled: false,
            unicode_fallback: false,
        }),
    })
}

/// Live shares use a longer debounce than the local search, so typing does not
/// cost a file-server round trip per keystroke.
#[test]
fn the_live_shares_are_asked_only_after_a_longer_pause_than_the_indexes() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);

    let early = s.update(AppEvent::Tick, now + SEARCH_DEBOUNCE);
    assert!(
        !early.cmds.iter().any(|c| matches!(c, Cmd::Live { .. })),
        "the drive was asked on the local debounce"
    );

    let later = s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);
    assert!(
        later.cmds.iter().any(|c| matches!(c, Cmd::Live { .. })),
        "the drive was never asked"
    );
    assert!(s.live.as_ref().is_some_and(|l| l.is_asking()));
}

/// A live answer is merged into the local results; replacing them would empty
/// and refill the list after typing has stopped.
#[test]
fn a_live_answer_merges_into_the_local_results_rather_than_replacing_them() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);

    let local = local_answer(&s, vec![hit("11-D-0704.pdf")]);
    s.update(local, now);
    assert_eq!(s.hits.len(), 1);

    let msg = live_msg(&s, answered(vec![live_hit("11-D-0704-rev.pdf")], 0));
    s.update(msg, now);

    assert_eq!(s.hits.len(), 2, "the local row was dropped");
    assert_eq!(s.matched, 2);
    assert!(s.live.as_ref().is_some_and(|l| !l.is_asking()));
}

/// A live answer with a superseded epoch is dropped rather than merged into
/// another query's results.
#[test]
fn a_live_answer_for_a_superseded_keystroke_is_dropped() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);

    let stale = AppEvent::Live(LiveMsg {
        epoch: s.query_epoch().saturating_sub(1),
        query: Query::contains("old"),
        mapping: MappingId(1),
        elapsed: Duration::ZERO,
        outcome: Box::new(answered(vec![live_hit("old.pdf")], 0)),
    });
    s.update(stale, now);
    assert!(s.hits.is_empty(), "a superseded answer reached the list");
}

/// Clearing the field also disarms the live deadline; see `stand_down`. The
/// epoch guard cannot catch it because the epoch was already bumped.
#[test]
fn clearing_the_field_retires_the_live_deadline_too() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('u')), now);

    let after = s.update(AppEvent::Tick, now + LIVE_DEBOUNCE * 2);
    assert!(
        !after.cmds.iter().any(|c| matches!(c, Cmd::Live { .. })),
        "a cleared field still asked the drive"
    );
    assert!(s.live.is_none());
}

/// An empty list from a partly searched live share never reads as "no
/// matches", neither before the share answers nor after a partial answer.
#[test]
fn an_empty_list_from_a_partly_searched_share_never_reads_as_no_matches() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);

    // The local search answers first and finds nothing. That says nothing
    // about the live share, which has not been asked yet.
    let local = local_answer(&s, Vec::new());
    s.update(local, now);
    assert!(
        matches!(s.empty_reason, Some(EmptyReason::NotSearchedYet)),
        "got {:?}",
        s.empty_reason
    );

    // One folder reached, three not.
    let msg = live_msg(&s, answered(Vec::new(), 3));
    s.update(msg, now);

    match &s.empty_reason {
        Some(EmptyReason::LiveIncomplete { name, skipped, .. }) => {
            assert_eq!(name, "archive");
            assert_eq!(*skipped, 3);
        }
        other => panic!("expected an incomplete-coverage reason, got {other:?}"),
    }
}

/// A live share that could not be asked is reported as unavailable.
#[test]
fn a_share_that_could_not_be_asked_says_so_rather_than_reporting_nothing() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);

    let msg = live_msg(
        &s,
        LiveOutcome::Skipped(files::search::live::LiveSkip::Unsupported),
    );
    s.update(msg, now);

    match &s.empty_reason {
        Some(EmptyReason::LiveUnavailable { name, .. }) => assert_eq!(name, "archive"),
        other => panic!("expected an unavailable reason, got {other:?}"),
    }
}

#[test]
fn a_configuration_of_only_live_shares_still_accepts_a_query() {
    use files::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};
    let routes = Routes::new(
        vec![Mapping {
            id: MappingId(0),
            name: "archive".into(),
            path: std::path::PathBuf::from("\\\\nas\\archive"),
            kind: MappingKind::Live,
            enabled: true,
            refresh: RefreshPolicy::Manual,
            depth: 1,
        }],
        ConfigSource::BuiltIn,
    );
    let settings = Settings::with_routes(Arc::new(routes), |s| s);
    let now = Instant::now();
    let mut s = AppState::new(settings, now);

    type_in(&mut s, "11-D-0704", now);
    assert!(
        !matches!(s.phase, QueryPhase::NoShares),
        "a live-only configuration was treated as having no drives"
    );
}

// --- aliases ---------------------------------------------------------------

/// A state whose configuration carries one alias.
fn aliased_state() -> (AppState, Instant) {
    let aliases = files::alias::Aliases::new(vec![files::alias::Alias {
        name: "pw".into(),
        code: "11-D-0704".into(),
        note: None,
    }]);
    let settings = Settings {
        aliases: Arc::new(aliases),
        ..Settings::fixture()
    };
    let now = Instant::now();
    (AppState::new(settings, now), now)
}

fn searched_for(r: &Response) -> Option<String> {
    r.cmds.iter().find_map(|c| match c {
        Cmd::Search { query, .. } => Some(query.term().to_string()),
        _ => None,
    })
}

/// An alias searches for its code immediately, without the typing debounce.
#[test]
fn an_alias_searches_for_the_code_it_stands_for_without_a_pause() {
    let (mut s, now) = aliased_state();
    let r = type_in(&mut s, "pw", now);

    assert_eq!(searched_for(&r).as_deref(), Some("11-D-0704"));
    assert_eq!(s.query().term(), "11-D-0704");
}

#[test]
fn an_alias_is_searched_for_as_its_expansion() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pw", now);
    assert_eq!(s.query().term(), "11-D-0704");
}

#[test]
fn a_short_line_that_is_not_an_alias_is_searched_for_as_written() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "zz", now);

    assert_eq!(s.query().term(), "zz");
    assert!(s.expansion().is_none());
}

/// The input keeps the typed text; rewriting it mid-edit would disturb the
/// caret and selection.
#[test]
fn an_alias_does_not_rewrite_what_was_typed() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pw", now);

    assert_eq!(s.input.text(), "pw");
    assert_eq!(s.expansion().map(|a| a.code.as_ref()), Some("11-D-0704"));
}

/// The active expansion is exposed so the UI can show what the alias stands
/// for.
#[test]
fn an_alias_reports_what_it_stood_for() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pw", now);

    let alias = s.expansion().expect("the expansion must be readable");
    assert_eq!(alias.name.as_ref(), "pw");
    assert_eq!(alias.describe(), "pw \u{b7} 11-D-0704");
}

/// Typing past an alias searches for the literal line, so longer codes that
/// start with an alias name stay reachable.
#[test]
fn typing_past_an_alias_goes_back_to_searching_for_what_is_on_the_line() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pw", now);
    assert!(s.expansion().is_some());

    let r = type_in(&mut s, "xyz", now);
    assert!(s.expansion().is_none(), "the expansion outlived the alias");
    assert_eq!(s.query().term(), "pwxyz");
    assert!(searched_for(&r).is_none_or(|q| q == "pwxyz"));
}

#[test]
fn an_alias_resolves_again_when_the_line_becomes_one() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pwx", now);
    assert!(s.expansion().is_none());

    s.update(press(Key::Backspace), now);
    assert_eq!(s.expansion().map(|a| a.code.as_ref()), Some("11-D-0704"));
}

/// Alias names are case-insensitive.
#[test]
fn an_alias_resolves_whatever_case_it_is_typed_in() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "PW", now);
    assert_eq!(s.query().term(), "11-D-0704");
}

#[test]
fn emptying_the_line_forgets_the_alias() {
    let (mut s, now) = aliased_state();
    type_in(&mut s, "pw", now);
    s.update(ctrl(Key::Char('u')), now);

    assert!(s.expansion().is_none());
    assert!(matches!(s.phase, QueryPhase::Idle));
}

#[test]
fn a_configuration_without_aliases_is_unchanged() {
    let (mut s, now) = state();
    type_in(&mut s, "pw", now);

    assert!(s.expansion().is_none());
    assert_eq!(s.query().term(), "pw");
}

// --- settings changed in another window -------------------------------------

use files::config::write::SettingKey;

/// Builds an alias with no note.
fn alias(name: &str, code: &str) -> files::alias::Alias {
    files::alias::Alias {
        name: name.into(),
        code: code.into(),
        note: None,
    }
}

fn saveable() -> (AppState, Instant) {
    let settings = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    let now = Instant::now();
    (AppState::new(settings, now), now)
}

/// The same as `state()` with developer mode on, so messages include their
/// technical detail.
fn dev_state() -> (AppState, Instant) {
    let (mut s, now) = state();
    s.settings.dev_mode = true;
    (s, now)
}

/// Delivers a new `Settings` to the panel, as after the settings window
/// writes the file.
fn adopt(s: &mut AppState, fresh: Settings, now: Instant) -> Response {
    s.update(AppEvent::Adopt(Box::new(fresh)), now)
}

/// The panel adopts the rewritten settings whole and shows a toast.
///
/// The settings window is a separate process that writes the file itself;
/// the panel then reloads all of it.
#[test]
fn a_file_rewritten_elsewhere_is_taken_whole() {
    let (mut s, now) = saveable();
    assert!(s.settings.history);

    let fresh = Settings {
        have_file: true,
        history: false,
        result_layout: files::config::ResultLayout::Detailed,
        ..Settings::fixture()
    };
    adopt(&mut s, fresh, now);

    assert!(!s.settings.history);
    assert_eq!(
        s.settings.result_layout,
        files::config::ResultLayout::Detailed
    );
    let toast = s
        .toast
        .as_ref()
        .expect("a change nobody watched must say so");
    assert!(toast.text.contains("Settings"), "{}", toast.text);
}

/// Turning history off also clears the in-memory entries, so Up no longer
/// recalls them.
#[test]
fn turning_the_history_off_elsewhere_forgets_what_it_held() {
    let (mut s, now) = saveable();
    s.history = files::history::History::from_entries(["11-D-0704"]);
    assert!(!s.history.is_empty());

    adopt(
        &mut s,
        Settings {
            have_file: true,
            history: false,
            ..Settings::fixture()
        },
        now,
    );
    assert!(s.history.is_empty(), "the codes outlived the setting");
}

/// An alias added in the settings window applies to the line already typed.
///
/// The line is re-resolved through the ordinary input path so the expansion,
/// query and results stay consistent.
#[test]
fn an_alias_added_elsewhere_expands_the_line_already_on_the_panel() {
    let (mut s, now) = saveable();
    type_in(&mut s, "pw", now);
    assert!(s.expansion().is_none());

    let mut fresh = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    fresh.aliases = std::sync::Arc::new(files::alias::Aliases::new(vec![alias("pw", "11-D-0704")]));
    adopt(&mut s, fresh, now);

    assert!(s.expansion().is_some(), "the new alias did not fire");
    assert_eq!(s.query().term(), "11-D-0704");
}

#[test]
fn an_alias_removed_elsewhere_stops_expanding_the_line() {
    let (mut s, now) = saveable();
    let mut with = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    with.aliases = std::sync::Arc::new(files::alias::Aliases::new(vec![alias("pw", "11-D-0704")]));
    adopt(&mut s, with, now);
    type_in(&mut s, "pw", now);
    assert!(s.expansion().is_some());

    adopt(
        &mut s,
        Settings {
            have_file: true,
            ..Settings::fixture()
        },
        now,
    );
    assert!(s.expansion().is_none(), "the expansion outlived the alias");
    assert_eq!(s.query().term(), "pw");
}

/// The status slots are resized with the drive list, so an actor's report
/// never targets a missing slot.
#[test]
fn the_status_slots_follow_the_drive_list() {
    use files::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};

    fn drive(id: u16, name: &str, path: &str) -> Mapping {
        Mapping {
            id: MappingId(id),
            name: name.into(),
            path: std::path::PathBuf::from(path),
            kind: MappingKind::Flat,
            enabled: true,
            refresh: RefreshPolicy::Auto,
            depth: files::config::DEFAULT_LIVE_DEPTH,
        }
    }

    let (mut s, now) = saveable();
    let three = vec![
        drive(0, "one", r"R:\"),
        drive(1, "two", r"S:\"),
        drive(2, "three", r"T:\"),
    ];
    let mut fresh = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    fresh.routes = std::sync::Arc::new(Routes::new(three, ConfigSource::BuiltIn));
    adopt(&mut s, fresh, now);
    assert_eq!(s.settings.routes.all().len(), 3);

    let mut fewer = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    fewer.routes = std::sync::Arc::new(Routes::new(
        vec![drive(0, "only", r"Z:\")],
        ConfigSource::BuiltIn,
    ));
    adopt(&mut s, fewer, now);
    assert_eq!(s.settings.routes.all().len(), 1);

    // The slot count is private, so this checks the routing table, which
    // `resize_statuses` in `adopt` keeps in step with the statuses.
}

/// A failed save warns that the named setting applies for this session only.
#[test]
fn a_failed_save_says_the_change_applies_for_this_session() {
    let (mut s, now) = saveable();
    s.update(
        AppEvent::Open(OpenMsg::SettingSaveFailed {
            label: "Colours",
            detail: "the drive is full".into(),
        }),
        now,
    );

    let toast = s.toast.as_ref().expect("a failed save must say so");
    assert!(toast.text.contains("Colours"), "{}", toast.text);
    assert!(toast.text.contains("this session only"), "{}", toast.text);
    assert_eq!(toast.severity, Severity::Warn);
}

/// What `applies_at_once` claims about each key matches what `adopt` does.
///
/// Runs over every `SettingKey` with a non-default value, so a key added
/// without handling in `adopt_one` fails here.
#[test]
fn what_a_key_claims_about_applying_at_once_is_what_it_does() {
    /// A `Settings` differing from the default in exactly one key.
    fn differing(key: SettingKey) -> Settings {
        let mut s = Settings {
            have_file: true,
            ..Settings::fixture()
        };
        match key {
            SettingKey::Theme => s.theme = files::config::ThemeChoice::Dark,
            SettingKey::Viewer => {
                s.viewers = with_avwin().viewers;
                s.viewer = files::config::ViewerKind::App(0);
            }
            SettingKey::Hotkey => s.hotkey = files::hotkey::spec::HotkeySpec::Off,
            SettingKey::PdfViewer => s.pdf_viewer = Some(r"C:\viewer.exe".into()),
            SettingKey::UpdateFrom => s.update_from = Some(r"\\server\share".into()),
            SettingKey::IndexLog => s.index_log = Some(r"C:\index.log".into()),
            SettingKey::Backdrop => s.backdrop = files::gui::window::Material::Mica,
            SettingKey::ResultLayout => s.result_layout = files::config::ResultLayout::Detailed,
            SettingKey::Dock => s.dock = files::config::Dock::Bottom,
            SettingKey::Columns => s.columns = 2,
            SettingKey::CheckForUpdates => s.check_for_updates = false,
            SettingKey::HideExtensions => {
                s.hidden = std::sync::Arc::new(files::config::hidden::Hidden::new(
                    &["zzz"],
                    s.hidden.hides_system(),
                ))
            }
            // Defaults to off, so `true` is the differing value.
            SettingKey::DevMode => s.dev_mode = true,
            SettingKey::History => s.history = false,
            SettingKey::StaleNotices => s.stale_notices = false,
            SettingKey::LiveUpdates => s.live_updates = false,
            SettingKey::PdfReadOnly => s.pdf_read_only = false,
            SettingKey::HideOnBlur => s.hide_on_blur = false,
            SettingKey::HideAfterOpening => s.hide_after_opening = false,
            SettingKey::HideOnEscape => s.hide_on_escape = false,
            SettingKey::HideSystemFiles => {
                let kept: Vec<String> = s.hidden.suffixes().map(str::to_owned).collect();
                s.hidden = std::sync::Arc::new(files::config::hidden::Hidden::new(
                    &kept,
                    !s.hidden.hides_system(),
                ))
            }
        }
        s
    }

    /// The state a running panel holds outside `settings`.
    fn live(s: &AppState) -> String {
        format!("{:?}|{}", s.viewer, s.history.is_empty())
    }

    for key in SettingKey::ALL {
        let (mut s, now) = saveable();
        s.history = files::history::History::from_entries(["11-D-0704"]);
        let before = live(&s);
        adopt(&mut s, differing(key), now);

        // `settings` is always replaced whole, so the claim concerns the
        // derived state held separately from it.
        let moved = live(&s) != before;
        assert!(
            !moved || key.applies_at_once(),
            "{} moved something live but says it waits for a restart",
            key.name()
        );
    }
}

// --- being told about a new version ----------------------------------------

use files::app::event::UpdateMsg;
use files::update::{Found, Manifest, Version};

fn looked(found: Found) -> AppEvent {
    AppEvent::Update(UpdateMsg::Looked(Box::new(found)))
}

fn available(version: &str) -> Found {
    Found::Available {
        manifest: Manifest {
            version: Version::parse(version).unwrap(),
            msi: "files.msi".into(),
            sha256: None,
            notes: None,
        },
        msi: std::path::PathBuf::from("files.msi"),
        msi_present: false,
    }
}

#[test]
fn a_new_version_is_announced_once_it_is_known_about() {
    let (mut s, now) = state();
    s.update(looked(available("0.3.0")), now);

    let toast = s.toast.as_ref().expect("a new version must be announced");
    assert!(toast.text.contains("0.3.0"), "{}", toast.text);
    assert_eq!(toast.severity, Severity::Info);
}

/// The periodic check announces each version only once.
#[test]
fn the_same_version_is_not_announced_twice() {
    let (mut s, now) = state();
    s.update(looked(available("0.3.0")), now);
    s.toast = None;

    s.update(looked(available("0.3.0")), now);
    assert!(s.toast.is_none(), "it nagged about a version already seen");
}

#[test]
fn a_further_version_is_announced_again() {
    let (mut s, now) = state();
    s.update(looked(available("0.3.0")), now);
    s.toast = None;

    s.update(looked(available("0.4.0")), now);
    let toast = s.toast.as_ref().expect("a further version is news");
    assert!(toast.text.contains("0.4.0"), "{}", toast.text);
}

/// Up-to-date and unavailable results are stored for the settings window but
/// raise no toast.
#[test]
fn a_dull_answer_is_remembered_without_being_announced() {
    for found in [
        Found::UpToDate,
        Found::Unavailable {
            detail: "the share is not there".into(),
        },
    ] {
        let (mut s, now) = state();
        s.update(looked(found.clone()), now);

        assert_eq!(s.update.as_ref(), Some(&found));
        assert!(s.toast.is_none(), "it interrupted to say nothing happened");
    }
}

/// The update state stays unknown until the first check answers, so the
/// window does not claim to be up to date.
#[test]
fn nothing_is_known_about_updates_until_a_look_answers() {
    let (s, _now) = state();
    assert_eq!(s.update, None);
}

// --- what an ordinary user is shown -----------------------------------------

/// Outside developer mode, a drive failure names the drive without OS error
/// codes or paths.
#[test]
fn a_drive_failure_is_a_sentence_until_somebody_asks() {
    fn reported(dev: bool) -> String {
        let (mut s, now) = state();
        s.settings.dev_mode = dev;
        s.update(
            AppEvent::Verify(VerifyMsg {
                epoch: s.query_epoch(),
                query: Query::parse(""),
                elapsed: Duration::from_millis(20),
                outcome: VerifyOutcome::Failed(EnumError::Transient(53)),
            }),
            now,
        );
        match &s.phase {
            QueryPhase::VerifyFailed { detail } => detail.clone(),
            other => panic!("expected a reported failure, got {other:?}"),
        }
    }

    let plain = reported(false);
    assert!(
        !plain.contains("53"),
        "an os error code reached an ordinary user: {plain}"
    );
    assert!(
        !plain.contains(r"\"),
        "a filesystem path reached an ordinary user: {plain}"
    );
    assert!(
        plain.contains("custompro") || plain.contains("jobs"),
        "the drive is not named at all: {plain}"
    );

    // Developer mode includes the technical detail.
    let technical = reported(true);
    assert!(technical.contains("53"), "{technical}");
}

#[test]
fn a_toast_carries_its_detail_only_in_developer_mode() {
    fn toast(dev: bool) -> String {
        let (mut s, now) = state();
        s.settings.dev_mode = dev;
        s.update(
            AppEvent::Open(OpenMsg::Failed {
                path: Arc::from(r"R:\11d\drawing.pdf"),
                detail: "the handle is invalid (os error 6)".into(),
            }),
            now,
        );
        s.toast
            .as_ref()
            .expect("a failure must say so")
            .text
            .clone()
    }

    let plain = toast(false);
    assert!(plain.contains("drawing.pdf"), "{plain}");
    assert!(
        !plain.contains("os error"),
        "an os error code reached an ordinary user: {plain}"
    );

    assert!(toast(true).contains("os error 6"));
}

/// Developer mode applies immediately; it only affects rendering, and no
/// worker holds a copy.
#[test]
fn developer_mode_changes_the_next_message_drawn() {
    let (mut s, now) = saveable();
    assert!(!s.settings.dev_mode);

    adopt(
        &mut s,
        Settings {
            have_file: true,
            dev_mode: true,
            ..Settings::fixture()
        },
        now,
    );
    assert!(s.settings.dev_mode, "it waited for a restart");
}
