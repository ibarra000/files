//! The interaction model, driven end to end with a fake clock.
//!
//! `AppState::update` is a pure function of (event, time), which is what
//! makes the whole debounce, staleness, selection and error model testable
//! on a machine where neither network drive exists. These are the cases that
//! would otherwise only be discoverable in production.
//!
//! Everything here goes through the public API, so it also serves as a check
//! that the surface is usable from outside the crate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::LiveMsg;
use files::app::event::{
    AppEvent, ClipboardMsg, Cmd, IndexMsg, OpenMsg, Redraw, RefreshTarget, Response, SearchMsg,
    VerifyMsg,
};
use files::app::key::{Key, KeyEvent, KeyPhase, Mods};
use files::app::state::{AppState, EmptyReason, QueryPhase, Severity, TOAST_LIFETIME};
use files::config::LIVE_DEBOUNCE;
use files::config::{
    ENTER_WATCHDOG, MIN_QUERY_LEN, SEARCH_DEBOUNCE, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG,
    VISIBLE_ROWS, ViewerKind,
};
use files::index::errors::EnumError;
use files::index::store::{Activity, Health, IndexStatus};
use files::paths::MappingId;
use files::search::live::{LiveCoverage, LiveOutcome};
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;
use files::search::verify::{AuditVerdict, VerifyOutcome};

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    (AppState::new(Settings::default(), now), now)
}

/// A state with no share configured at all.
///
/// The only way a query now reaches nothing: every share is indexed, so there
/// is no pattern left to fail to match.
fn unconfigured_state() -> (AppState, Instant) {
    // Built directly rather than parsed: the config layer refuses a file with
    // no enabled mapping in it, which is why this phase is a defensive branch
    // rather than something a user can reach by editing the file.
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

#[test]
fn a_short_query_is_not_dispatched() {
    let (mut s, now) = state();
    let r = type_in(&mut s, "ab", now);
    assert_eq!(
        s.phase,
        QueryPhase::TooShort {
            need: MIN_QUERY_LEN
        }
    );
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

    // And the pause produces exactly one, carrying the whole code rather than
    // any of the eight prefixes on the way to it.
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
///
/// This asserted the opposite: a pattern had to recognise a code before
/// anything would look anywhere, so a string matching none of them never
/// reached the index at all. Both shares are indexed now, so there is nothing
/// to recognise - the query runs and finds nothing, which is a different
/// answer from declining to look, and the two used to be indistinguishable.
#[test]
fn an_unusual_code_is_searched_for_rather_than_refused() {
    let (mut s, now) = state();
    type_in(&mut s, "!!!", now);
    assert_eq!(s.phase, QueryPhase::LocalPending);
    assert_ne!(s.empty_reason, Some(EmptyReason::NoSharesConfigured));
    // Behind the pause now, like any other typed code.
    let r = s.update(AppEvent::Tick, now + SEARCH_DEBOUNCE);
    assert!(r.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })));
}

/// With no share enabled there is nowhere to look, and saying so beats
/// searching nothing and reporting no matches.
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
    // Windows delivers both press and release; handling both doubles
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

/// Esc clears and never exits. Two taps used to be enough to end the session
/// by accident, which is the whole reason this changed - so the second tap is
/// the interesting half of this test, not the first.
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

    // Still usable afterwards: clearing is not a terminal state.
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

/// With nothing selected it says so. Silence would read as the program
/// ignoring the key, to anyone who remembers when it quit.
#[test]
fn ctrl_c_with_no_selection_explains_itself_and_stays_running() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    s.update(ctrl(Key::Char('c')), now);
    assert!(!s.should_quit);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Nothing selected")),
        "{:?}",
        s.toast
    );
    assert_eq!(s.input, "11-D-0704", "the code must survive");
}

/// Ctrl+X takes the selection with it.
///
/// Reaches the state machine as a `Char('x')` with Ctrl, which no keyboard
/// ever sends: `gui::input` reconstructs it from the toolkit's `Event::Cut`,
/// which is also what Shift+Delete arrives as.
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

/// With nothing selected there is nothing to remove, and saying so beats a
/// key that silently does nothing.
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

/// Ctrl+Q is the *only* way out, and the bindings most likely to be hit by
/// accident are the ones this checks hardest. Esc in particular: two taps of
/// it used to end the session, which is what put a deliberate quit key here.
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

/// Ctrl+C copies; it must not also be a way out. This is the binding someone
/// reaching for a quit key is most likely to try first.
#[test]
fn ctrl_c_copies_rather_than_quitting() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let r = s.update(ctrl(Key::Char('c')), now);
    assert!(!s.should_quit);
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Quit)));
}

/// AltGr is not a modifier this program has a binding for; it is how a
/// European keyboard types a character.
///
/// Windows reports AltGr as `CONTROL | ALT`, so on a German, Polish or French
/// layout it is how `@`, `{`, `[` and every accented letter are produced. The
/// guard above exists to stop `Ctrl+W` inserting a literal `w`, and it was
/// swallowing those too - which makes a whole class of characters untypable in
/// the search box with no indication of why.
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

/// And the guard still does the job it was written for.
#[test]
fn a_control_combination_this_program_does_not_have_still_types_nothing() {
    let (mut s, now) = state();
    for c in ['w', 'x', 'z'] {
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::CTRL)), now);
        s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALT)), now);
    }
    assert_eq!(s.input, "", "a lone Ctrl or Alt combination is not text");
}

/// Ctrl and Alt combinations this program has no binding for used to fall
/// through to the "it is a character, type it" arm: Ctrl+W typed a `w`.
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
    // The request carries the typed code as well as the row, because the page
    // set is rebuilt from the code rather than from the fifteen rows on screen.
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
    // Opening is also the strongest signal that this was the code meant, so
    // it is remembered at the same time.
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
    // Enter inside the pause asks the matcher rather than answering from a
    // list that belongs to the previous code. The report comes when it does.
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

/// `F5` asks which drive rather than re-reading all of them.
///
/// It used to mean "re-read every share, now". Almost every press wanted one
/// of them, and across a few hundred people the difference is between a
/// handful of passes over a three-hundred-thousand-folder share and several
/// hundred simultaneous ones.
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
    assert!(
        matches!(target, Some(RefreshTarget::One(_))),
        "one drive, not all of them: {:?}",
        r.cmds
    );
    assert!(!s.picking_share, "and the list closes behind it");
    assert!(
        s.verify_due_at().is_some(),
        "an update bypasses the debounce"
    );
}

#[test]
fn a_in_the_drive_list_still_updates_everything() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::F(5)), now);
    let r = s.update(press(Key::Char('a')), now);
    assert!(
        r.cmds.iter().any(|c| matches!(
            c,
            Cmd::RefreshIndex {
                target: RefreshTarget::All,
                force: true
            }
        )),
        "{:?}",
        r.cmds
    );
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

/// Neither end of the list wraps. Wrapping would throw the eye from the row
/// someone was reading to the far end of the list, and the way back is the way
/// they came.
///
/// Up at the top used to hand focus back to the search box, which was what made
/// a further Up reach the recalled codes. The field never gives up the keyboard
/// now, so the top simply holds.
#[test]
fn neither_end_of_the_list_wraps() {
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
    let r = s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(2), "down from the bottom stays put");
    assert_eq!(
        r.redraw,
        Redraw::No,
        "and does not redraw an identical frame"
    );

    // The way back is the way they came.
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(1));
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(0));

    let r = s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(0), "up from the top stays put");
    assert_eq!(
        r.redraw,
        Redraw::No,
        "and does not redraw an identical frame"
    );
    assert!(!s.should_quit);
}

/// The specific annoyance this design exists to avoid.
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
    // And is: the flag was maintained and read by nothing but this assertion
    // for the whole of the rewrite, so it is checked here where it comes out.
    let line = files::view::status::render(&s, now, std::time::SystemTime::now());
    assert!(
        line.text.contains("The list changed"),
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
// The reported symptom was that the age readout froze and then jumped: the
// loop parked in an unbounded receive whenever nothing else was pending, so
// time-derived text was only recomputed when a keystroke happened to arrive.

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

/// The ages are only on screen while the drive picker is open, so the wakeups
/// they cost are only owed while it is.
///
/// They used to lead the status line in every healthy phase, which meant an
/// idle panel with a loaded index ticked for as long as it was up - once a
/// second, then once a minute, for a number nobody had asked to see. The
/// picker is where somebody goes when they suspect the list is stale, and that
/// is where the clock now runs.
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

/// The assertion that distinguishes an aligned tick from a flat 1 Hz one: at
/// ten minutes old the readout says `10m` and only changes on the minute.
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

/// The most important test here. A deadline with no matching redraw arm is a
/// busy spin: the loop wakes, gets `Response::none()`, leaves `dirty` false
/// and re-blocks on a deadline already in the past.
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

/// Guards against "fixing" the freeze by redrawing on every tick, which would
/// restore the fifty-frames-a-second poll through the back door.
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

/// `next_retry_at` is an absolute instant, unlike every other deadline here.
/// Once it passes, an ungated term is permanently due - a full-speed redraw
/// loop rather than a countdown.
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

/// The regression test for the reported flicker. Two actors wrote one field,
/// so the status line described whichever published last.
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

/// A fresh share must not vouch for a stale one.
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

/// The spinner must not stop while two of three shares are still walking.
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
    let (mut s, now) = state();
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
    let (mut s, now) = state();
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
    s.settings.viewer_persistable = true;
    assert_eq!(s.viewer, ViewerKind::Auto, "the default opens by file type");

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf);
    assert_eq!(r.redraw, Redraw::Yes, "the help line changes");
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveViewer(ViewerKind::Pdf))),
        "{:?}",
        r.cmds
    );

    // All the way round, so a mode that F2 cannot reach is a failure here
    // rather than something nobody notices.
    s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Avwin);
    s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Auto, "it cycles back");
}

/// A doubled character is visible; a doubled toggle is a silent no-op that
/// looks exactly like the key being broken.
#[test]
fn f2_on_key_release_does_not_toggle_twice() {
    let (mut s, now) = state();
    let mut ev = KeyEvent::new(Key::F(2), Mods::NONE);
    ev.phase = KeyPhase::Release;
    s.update(AppEvent::Key(ev), now);
    assert_eq!(s.viewer, ViewerKind::Auto, "a release must change nothing");
}

/// With FILES_VIEWER or --viewer in play the file value is ignored at the next
/// start, so writing it would report a save that does nothing.
#[test]
fn f2_says_so_when_the_choice_cannot_be_persisted() {
    let (mut s, now) = state();
    s.settings.viewer_persistable = false;

    let r = s.update(press(Key::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf, "it still applies");
    assert!(
        !r.cmds.iter().any(|c| matches!(c, Cmd::SaveViewer(_))),
        "nothing should be written"
    );
    let t = s.toast.as_ref().unwrap();
    assert!(t.text.contains("session only"), "{}", t.text);
}

/// Recall owns the arrows, Enter and Esc; every *other* key means "back to
/// editing" and commits the highlighted entry. F2 is not editing - it changes
/// which program opens a file - so it must not drag a code into the search box
/// and run a query for it.
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

/// Choosing a different program to open a file with is not a reason to run
/// the search again.
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

/// A drawing set silently missing page seven is the worst outcome here,
/// because nothing on screen would ever reveal it.
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

/// A drawing set longer than the ceiling opens without its tail. Saying
/// nothing would leave the user believing they had seen the whole thing.
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
            viewer: ViewerKind::Avwin,
        }),
        now,
    );
    let t = s.toast.as_ref().unwrap();
    assert_eq!(t.severity, Severity::Info);
    assert!(t.text.contains("avwin"), "{}", t.text);
}

/// The toggle still applies, so this is a warning about persistence rather
/// than a failure of the keypress.
#[test]
fn a_failed_viewer_save_warns_rather_than_ending_the_session() {
    let (mut s, now) = state();
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

/// Several assertions need an immutable view of the state while also
/// mutating it.
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
//
// This section used to be about a *grid*: results flowed down each column and
// wrapped into the next, the way a newspaper does, and Left and Right walked
// between columns. That earned its complexity in a terminal, where the pane was
// as wide as somebody's window and a single column wasted most of it.
//
// A window shows one scrolling list, which is both the native idiom and one
// fewer thing to explain - and with it went `Grid`, `visible_range`,
// `move_columns`, `move_pages` and the four tests that covered the flow. What
// is kept below is every invariant that was never about columns.

fn with_results(s: &mut AppState, now: Instant, n: usize) {
    type_in(s, "11-D-0704", now);
    let v = view(s);
    s.update(search_result(&v, many_hits(n), n as u32, 9_000), now);
}

fn many_hits(n: usize) -> Vec<Hit> {
    (0..n).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect()
}

/// Down walks the list rank by rank and stops at the end rather than wrapping.
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

/// And the head of the list holds, because there is nowhere to hand the
/// keyboard back to: the search field never gave it up.
#[test]
fn up_at_the_top_of_the_list_holds_rather_than_leaving_it() {
    let (mut s, now) = state();
    with_results(&mut s, now, 4);

    s.update(press(Key::Down), now);
    s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(0));

    let again = s.update(press(Key::Up), now);
    assert_eq!(s.selected_row(), Some(0), "the top row must hold");
    assert_eq!(
        again.redraw,
        Redraw::No,
        "and holding still draws no new frame"
    );
}

/// The arrows move the selection; the caret keys move the caret. There is no
/// mode in which one becomes the other, which is the whole of what collapsing
/// `Focus` bought.
#[test]
fn left_and_right_always_move_the_caret() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);

    // Even with the selection deep in the list.
    for _ in 0..5 {
        s.update(press(Key::Down), now);
    }
    let row = s.selected_row();

    s.update(press(Key::Left), now);
    let before = s.query_epoch();
    let typed = s.update(key('X'), now);

    assert_eq!(s.input.text(), "11-D-070X4", "the caret moved, not the row");
    // Asserted on the deadline rather than on the dispatch, and on the
    // dispatch rather than on the selection going away. Typing used to empty
    // the list, so "the row changed" was a usable proxy for "the search
    // re-ran"; then the results began surviving the keystroke, and now the
    // search itself waits for a pause. What must still be true is that the
    // keystroke *re-armed* it. See
    // `typing_keeps_the_results_until_the_new_ones_arrive` below.
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

/// What is on screen stays on screen until there is something to put in its
/// place.
///
/// `Cmd::Search` is dispatched at the end of the turn and answered on another
/// thread, so the frame that asks the question is always drawn before the
/// answer arrives. Emptying the list here meant every frame drawn in that gap
/// showed no results - which is not a shorter list, it is a different body
/// three hundred points shorter, reached through a cross-fade. Once per
/// character typed.
///
/// Keeping them is safe because `on_search` is epoch-guarded and `apply_hits`
/// replaces wholesale: the worst a superseded set can do is survive one frame.
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

    // And they are replaced, not merged, when the answer does arrive.
    let v = view(&s);
    s.update(search_result(&v, many_hits(2), 2, 9_000), now);
    assert_eq!(s.hits.len(), 2);
}

/// The branches that have *decided* there is nothing to search for still
/// clear the list. A code deleted back to nothing must not leave the previous
/// code's results sitting under an empty search box.
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

/// A whole listful at a time, and the ends still hold.
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

/// The bug that made holding an arrow key look like the program had hung.
///
/// The selection walked the whole list while the panel drew the first
/// screenful and pinned the highlight to its last row, so every frame past
/// that was pixel-identical to the one before it. Frames were still being
/// produced - each auto-repeat woke the loop - the panel simply had nothing
/// new to say, and `Enter` would have opened a file that was not on screen.
#[test]
fn holding_down_scrolls_the_list_rather_than_freezing_on_its_last_row() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);
    assert_eq!(s.scroll_top(), 0, "it starts at the top");

    // Down to the foot of the window: still no scrolling needed.
    for _ in 0..VISIBLE_ROWS - 1 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(VISIBLE_ROWS - 1));
    assert_eq!(s.scroll_top(), 0, "the window has not had to move yet");

    // One more, and the window follows by exactly one row.
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(VISIBLE_ROWS));
    assert_eq!(s.scroll_top(), 1, "the list scrolls under the cursor");
    assert!(s.visible_rows().contains(&VISIBLE_ROWS));

    // And all the way to the end, where the window stops rather than running
    // off it.
    for _ in 0..300 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.selected_row(), Some(199));
    assert_eq!(s.scroll_top(), 200 - VISIBLE_ROWS);
    assert_eq!(s.visible_rows(), (200 - VISIBLE_ROWS)..200);

    // Back up, and it comes with you.
    for _ in 0..300 {
        s.update(press(Key::Up), now);
    }
    assert_eq!(s.selected_row(), Some(0));
    assert_eq!(s.scroll_top(), 0);
}

/// A list that shrinks under a window near its end must not leave the window
/// pointing past it - half a screen of rows with nothing below them.
#[test]
fn a_shorter_result_set_pulls_the_window_back() {
    let (mut s, now) = state();
    with_results(&mut s, now, 200);
    for _ in 0..199 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(s.scroll_top(), 200 - VISIBLE_ROWS, "precondition");

    // The server answers with far fewer.
    let v = view(&s);
    s.update(
        search_result(&v, many_hits(VISIBLE_ROWS + 2), 14, 9_000),
        now,
    );

    assert_eq!(s.scroll_top(), 2);
    assert_eq!(s.visible_rows().end, VISIBLE_ROWS + 2);
    assert!(
        s.visible_rows().contains(&s.selected_row().unwrap()),
        "the cursor is off screen"
    );
}

/// The cap is what makes a long list worth having at all.
#[test]
fn far_more_than_one_screenful_of_results_is_reachable() {
    let (mut s, now) = state();
    with_results(&mut s, now, files::config::MAX_RESULTS);

    for _ in 0..files::config::MAX_RESULTS + 10 {
        s.update(press(Key::Down), now);
    }
    assert_eq!(
        s.selected_row(),
        Some(files::config::MAX_RESULTS - 1),
        "the last of {} results is reachable",
        files::config::MAX_RESULTS
    );
}

// --- the pause, and what must not go wrong inside it --------------------

/// Enter typed before the pause elapses must not open the row on screen: that
/// row answers the *previous* code. Opening it would open the wrong file
/// without saying so, which is the worst thing this program could do.
#[test]
fn enter_before_the_search_fires_opens_the_new_code_not_the_old_one() {
    let (mut s, now) = state();

    // A settled query, with results for the code as it stands.
    type_in(&mut s, "11-D-070", now);
    let v = view(&s);
    s.update(
        search_result(&v, vec![hit("11-D-070-OLD.pdf")], 1, 9_000),
        now,
    );
    assert_eq!(s.hits.len(), 1);

    // One more character, then Enter straight away.
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

    // And when the answer lands, that is what gets opened.
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

/// An edit between Enter and the answer cancels the open. The keystroke was
/// about a code the user has since moved on from.
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

/// A matcher that never answers must not leave Enter dead.
#[test]
fn a_matcher_that_never_answers_an_enter_gives_the_keystroke_back() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(press(Key::Enter), now);
    assert!(s.toast.is_none());

    s.update(AppEvent::Tick, now + ENTER_WATCHDOG);
    assert!(s.toast.is_some(), "the keystroke was swallowed");
}

/// The local answer may not walk the phase back out of the verification.
///
/// Both deadlines are 300ms, so both fall due on one tick and either can
/// answer first. A local answer is never news about the server.
#[test]
fn the_local_answer_does_not_walk_the_phase_back_from_verifying() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let due = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, due);
    assert!(s.phase.is_verifying(), "the verification did not start");

    // The matcher answers a frame later, as it always does.
    let v = view(&s);
    s.update(search_result(&v, vec![hit("a.pdf")], 1, 9_000), due);
    assert!(
        s.phase.is_verifying(),
        "the local answer blinked the spinner off: {:?}",
        s.phase
    );
    assert_eq!(s.hits.len(), 1, "and it still applied the results");
}

/// A cleared field must leave nothing armed. This one is not caught by the
/// epoch guard: the edit bumps the epoch on its way past, so a late dispatch
/// carries the current one and its answer would be accepted.
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

/// A pasted code arrives whole, so it is searched for at once. Nobody pastes
/// half a job number.
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

/// Every toast this state machine can raise, held to the house style.
///
/// Toasts are written here and drawn by `view::status` in the same 13 pt run
/// as the status line, which is what makes them status text however far from
/// `view` they live. For the whole of the rewrite that was invisible, because
/// they were computed and thrown away - so by the time anything drew them the
/// footer alternated between `Searching...` and `nothing to open` seconds
/// apart, in two different conventions, and nobody had ever seen it happen.
///
/// A toast this cannot reach is a toast nothing here is checking, so a new
/// `set_toast` belongs in this list.
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

    // Nothing selected, and nothing to open.
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(Key::Char('c')), now);
    raised(&s);

    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(search_result(&view(&s), Vec::new(), 0, 9_000), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    // And something to open, which says so while it is being assembled.
    let (mut s, now) = state();
    with_results(&mut s, now, 4);
    s.update(press(Key::Down), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    // The clipboard, in all three of its answers.
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

    // Opening, in every way it can end other than cleanly.
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

    // Every viewer, saved and unsaveable.
    for viewer in [ViewerKind::Auto, ViewerKind::Pdf, ViewerKind::Avwin] {
        let (mut s, now) = state();
        s.update(AppEvent::Open(OpenMsg::ViewerSaved { viewer }), now);
        raised(&s);

        let mut unsaveable = AppState::new(
            Settings {
                viewer_persistable: false,
                ..Settings::default()
            },
            now,
        );
        unsaveable.viewer = viewer;
        unsaveable.update(press(Key::F(2)), now);
        raised(&unsaveable);
    }

    // An actor that stopped.
    let (mut s, now) = state();
    s.update(
        AppEvent::ActorDied {
            actor: "index",
            detail: "attempt to subtract with overflow".into(),
        },
        now,
    );
    raised(&s);

    // The hotkey that could not be claimed.
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

    // A refresh, both ways.
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

    // The drive picker, one drive and all of them.
    let (mut s, now) = state();
    s.update(press(Key::F(5)), now);
    s.update(press(Key::Enter), now);
    raised(&s);

    let (mut s, now) = state();
    s.update(press(Key::F(5)), now);
    s.update(key('a'), now);
    raised(&s);

    // A search that never came back. Enter has to land while one is still
    // due, which is what arms the watchdog - pressing it over a list that has
    // already arrived opens a file instead, and pressing it after the debounce
    // has fired opens nothing and says so.
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

/// A toast starts the way a line starts.
///
/// The exception is the actor name in `<actor> stopped unexpectedly`, which is
/// an internal identifier printed verbatim beside the same name in the log.
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

/// F3 and F4 edit the line rather than holding a filter beside it. That is
/// what keeps the query in one place: recalled with the line, copied with the
/// line, and visible without a chip anybody has to notice.
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

/// A deliberate press is not a character on the way to a longer code, so
/// there is nothing to wait for - the same argument the paste path makes.
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

/// A hand-typed filter the key could not have produced cycles back to no
/// filter. Guessing where `ext:sldprt` sits in a list that does not hold it
/// would be inventing an answer; clearing it is the one step that is always
/// what it looks like.
#[test]
fn a_hand_typed_type_the_key_does_not_know_is_cleared_rather_than_guessed_at() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704 ext:sldprt", now);

    s.update(press(Key::F(4)), now);
    assert_eq!(s.input.text(), "11-D-0704");
}

/// The line is the query, so a narrowed search is what gets remembered and
/// what comes back on Up.
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

/// `*` cannot occur in a Windows filename, so reading it literally would be a
/// guaranteed empty list with nothing on screen to explain it.
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

/// The minimum exists to bound the arena sweep, so it is counted on the thing
/// that is swept for. `ab ext:pdf` is a ten-character line and a
/// two-character needle.
#[test]
fn the_minimum_length_is_judged_on_the_code_and_not_on_the_whole_line() {
    let (mut s, now) = state();
    type_in(&mut s, "ab ext:pdf", now);
    assert!(matches!(s.phase, QueryPhase::TooShort { .. }));
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

/// Typing must not put a round trip on somebody elses file server per
/// keystroke, so the live shares wait longer than the local sweep does.
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

/// Phase two arrives a second after phase one. Replacing the list would empty
/// it and refill it, which is the flinch `on_input_changed` avoids during
/// typing, except this one lands after the typing stopped.
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

/// The keystroke that superseded it already bumped the epoch, so a late answer
/// has to be dropped rather than folded into a different querys results.
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

/// The note on `stand_down`: a command armed on a field that has since been
/// cleared is not caught by the epoch guard, because the epoch was already
/// bumped on the way past.
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

/// The claim this program exists not to let somebody act on by mistake. A live
/// share answers for the folders one query reached, so an empty list is only
/// honestly "no matches" when everything was reached.
///
/// Note what the list never passes *through*: not "no matches" before the
/// drive is asked, and not "no matches" after it came back short. There is no
/// moment in the sequence where the panel claims the file does not exist.
#[test]
fn an_empty_list_from_a_partly_searched_share_never_reads_as_no_matches() {
    let (mut s, now) = live_state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + LIVE_DEBOUNCE);

    // The local sweep answers first and finds nothing. That is *not* an
    // answer about the share: the drive has not been asked yet, and on a live
    // share whose whole index is what past searches found, an empty first
    // sweep is the ordinary case rather than the interesting one.
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

/// A share that could not be asked at all is a different sentence again, and
/// the one most likely to be acted on wrongly.
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

/// A configuration holding nothing but live shares is searchable, and the gate
/// that decides whether to dispatch at all has to agree.
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
