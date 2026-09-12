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

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use files::app::event::{AppEvent, Cmd, IndexMsg, OpenMsg, Redraw, Response, SearchMsg, VerifyMsg};
use files::app::state::{AppState, EmptyReason, Focus, QueryPhase, Severity, TOAST_LIFETIME};
use files::config::{MIN_QUERY_LEN, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG, ViewerKind};
use files::index::errors::EnumError;
use files::index::store::{Activity, Health, IndexStatus};
use files::search::matcher::{Hit, SearchOutcome};
use files::search::verify::{AuditVerdict, VerifyOutcome};
use ratatui::layout::Rect;

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
    AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn press(code: KeyCode) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(code: KeyCode) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
}

fn shift(code: KeyCode) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, KeyModifiers::SHIFT))
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
fn every_keystroke_past_the_minimum_dispatches_a_local_search() {
    let (mut s, now) = state();
    let r = type_in(&mut s, "11-D-0704", now);
    let searches = r
        .cmds
        .iter()
        .filter(|c| matches!(c, Cmd::Search { .. }))
        .count();
    assert!(
        searches >= 5,
        "local matching is cheap enough to run per keystroke"
    );
    assert_eq!(s.phase, QueryPhase::LocalPending);
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
    let r = type_in(&mut s, "!!!", now);
    assert_eq!(s.phase, QueryPhase::LocalPending);
    assert_ne!(s.empty_reason, Some(EmptyReason::NoSharesConfigured));
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
        s.update(press(KeyCode::Backspace), now);
    }
    assert_eq!(s.phase, QueryPhase::Idle);
    assert_eq!(s.empty_reason, Some(EmptyReason::NoQuery));
}

#[test]
fn key_releases_are_ignored() {
    // Windows delivers both press and release; handling both doubles
    // every keystroke.
    let (mut s, now) = state();
    let mut ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    ev.kind = KeyEventKind::Release;
    s.update(AppEvent::Key(ev), now);
    assert_eq!(s.input, "");
}

#[test]
fn typing_bumps_the_epoch_so_stale_results_are_discarded() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-07", now);
    let stale = AppEvent::Search(SearchMsg {
        epoch: s.query_epoch() - 1,
        query: "old".into(),
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
        query: s.input.text().to_string(),
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

    s.update(press(KeyCode::Esc), now);
    assert_eq!(s.input, "");
    assert!(!s.should_quit, "the first Esc must not end the session");

    s.update(press(KeyCode::Esc), now);
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
    s.update(shift(KeyCode::Left), now);
    s.update(shift(KeyCode::Left), now);

    let r = s.update(ctrl(KeyCode::Char('c')), now);
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

    s.update(ctrl(KeyCode::Char('c')), now);
    assert!(!s.should_quit);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("nothing selected")),
        "{:?}",
        s.toast
    );
    assert_eq!(s.input, "11-D-0704", "the code must survive");
}

/// Ctrl+Q is the *only* way out, and the bindings most likely to be hit by
/// accident are the ones this checks hardest. Esc in particular: two taps of
/// it used to end the session, which is what put a deliberate quit key here.
#[test]
fn nothing_except_ctrl_q_quits_the_application() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let codes = [
        KeyCode::Esc,
        KeyCode::Enter,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Tab,
        KeyCode::F(5),
        KeyCode::Char('c'),
        KeyCode::Char('d'),
        KeyCode::Char('z'),
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
    s.update(press(KeyCode::Char('q')), now);
    assert!(!s.should_quit, "typing q must not end the session");
    s.update(shift(KeyCode::Char('q')), now);
    assert!(!s.should_quit, "Shift+Q must not end the session");
}

#[test]
fn ctrl_q_quits_and_asks_exactly_once() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let r = s.update(ctrl(KeyCode::Char('q')), now);
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

    let r = s.update(ctrl(KeyCode::Char('c')), now);
    assert!(!s.should_quit);
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Quit)));
}

/// Ctrl and Alt combinations this program has no binding for used to fall
/// through to the "it is a character, type it" arm: Ctrl+W typed a `w`.
#[test]
fn an_unbound_control_combination_does_not_type_a_letter() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D", now);
    for c in ['q', 'x', 'z', 'n', 'p'] {
        s.update(ctrl(KeyCode::Char(c)), now);
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

    let r = s.update(press(KeyCode::Enter), now);
    // The request carries the typed code as well as the row, because the page
    // set is rebuilt from the code rather than from the fifteen rows on screen.
    assert!(matches!(
        &r.cmds[0],
        Cmd::Open(req)
            if &*req.path == "V:\\a.pdf"
                && req.query == "11-D-0704"
                && req.viewer == ViewerKind::Pdf
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
    let r = s.update(press(KeyCode::Enter), now);
    assert!(r.cmds.is_empty());
    assert!(s.toast.is_some());
}

#[test]
fn f5_forces_a_refresh_and_an_immediate_verify() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let r = s.update(press(KeyCode::F(5)), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::RefreshIndex { force: true }))
    );
    assert_eq!(s.verify_due_at(), Some(now), "F5 bypasses the debounce");
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

/// Down still wraps at the bottom, but Up no longer wraps at the top: it
/// hands focus back to the search box, which is what makes a further Up reach
/// the recalled codes. Wrapping would also throw the eye from the row someone
/// was reading to the far end of the list.
#[test]
fn down_wraps_at_the_bottom_but_up_leaves_the_list_at_the_top() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        search_result(&view(&s), vec![hit("a"), hit("b"), hit("c")], 3, 3),
        now,
    );

    s.update(press(KeyCode::Down), now);
    assert_eq!(s.selected_row(), Some(1));
    s.update(press(KeyCode::Down), now);
    assert_eq!(s.selected_row(), Some(2));
    s.update(press(KeyCode::Down), now);
    assert_eq!(s.selected_row(), Some(0), "down from the bottom wraps");

    s.update(press(KeyCode::Up), now);
    assert_eq!(
        s.focus,
        Focus::Input,
        "up from the top returns to the search box"
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
    s.update(press(KeyCode::Down), now);
    s.update(press(KeyCode::Down), now);
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
    s.update(press(KeyCode::Down), now); // row 1, "b"

    s.update(
        search_result(&view(&s), vec![hit("a"), hit("c")], 2, 2),
        now,
    );
    assert!(s.selection_lost, "the user should be told the list shifted");
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
    s.update(press(KeyCode::Down), now);
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

#[test]
fn a_verifying_state_asks_for_animation_frames() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.wants_animation());
    assert!(s.next_deadline().is_some());
}

#[test]
fn animation_stops_when_the_work_finishes() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(AppEvent::Tick, now + VERIFY_DEBOUNCE);
    assert!(s.wants_animation());

    s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: s.input.text().to_string(),
            elapsed: Duration::from_millis(40),
            outcome: VerifyOutcome::IndexAuthoritative { stamp: None },
        }),
        now,
    );
    assert!(
        !s.wants_animation(),
        "derived from state, never a sticky flag"
    );
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
            query: s.input.text().to_string(),
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
            query: s.input.text().to_string(),
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
            query: s.input.text().to_string(),
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
            query: s.input.text().to_string(),
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
    assert!(toast.text.contains("server filter"));
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
    s.update(AppEvent::Index(IndexMsg::Status(Arc::new(status))), now);

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
    assert_eq!(s.viewer, ViewerKind::Pdf);

    let r = s.update(press(KeyCode::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Avwin);
    assert_eq!(r.redraw, Redraw::Yes, "the help line changes");
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::SaveViewer(ViewerKind::Avwin))),
        "{:?}",
        r.cmds
    );

    s.update(press(KeyCode::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Pdf, "it cycles back");
}

/// A doubled character is visible; a doubled toggle is a silent no-op that
/// looks exactly like the key being broken.
#[test]
fn f2_on_key_release_does_not_toggle_twice() {
    let (mut s, now) = state();
    let mut ev = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
    ev.kind = KeyEventKind::Release;
    s.update(AppEvent::Key(ev), now);
    assert_eq!(s.viewer, ViewerKind::Pdf, "a release must change nothing");
}

/// With FILES_VIEWER or --viewer in play the file value is ignored at the next
/// start, so writing it would report a save that does nothing.
#[test]
fn f2_says_so_when_the_choice_cannot_be_persisted() {
    let (mut s, now) = state();
    s.settings.viewer_persistable = false;

    let r = s.update(press(KeyCode::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Avwin, "it still applies");
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

    s.update(press(KeyCode::Up), now);
    assert_eq!(s.focus, Focus::History);
    let recalled = s.input.text().to_string();
    let epoch = s.query_epoch();

    let r = s.update(press(KeyCode::F(2)), now);
    assert_eq!(s.viewer, ViewerKind::Avwin, "the viewer still toggles");
    assert_eq!(s.focus, Focus::History, "recall stays open");
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

    let r = s.update(press(KeyCode::F(2)), now);
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
    s.update(press(KeyCode::F(2)), now);

    let r = s.update(press(KeyCode::Enter), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Open(req) if req.viewer == ViewerKind::Avwin)),
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
    s.update(AppEvent::Index(IndexMsg::Status(Arc::new(status))), now);
    assert!(s.wants_animation());
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
        query: view.input.clone(),
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

// --- the results grid -------------------------------------------------

/// A terminal wide enough for all three columns, with a known column height.
///
/// The geometry is asserted rather than assumed: the whole point of deriving
/// the grid from `ui::layout` is that navigation and rendering agree, so a
/// test that guessed the column height would be testing its own arithmetic.
fn grid_state(rows: u16) -> (AppState, Instant, usize) {
    let (mut s, now) = state();
    // 2 margin + 3 input + 1 status + 1 toast + 1 help + 2 result borders.
    s.set_area(Rect::new(0, 0, 120, rows + 10));
    let g = s.grid();
    assert_eq!(g.columns(), 3, "120 columns is wide enough for three");
    assert_eq!(g.rows(), rows as usize);
    (s, now, g.rows())
}

fn many_hits(n: usize) -> Vec<Hit> {
    (0..n).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect()
}

fn with_results(s: &mut AppState, now: Instant, n: usize) {
    type_in(s, "11-D-0704", now);
    let v = view(s);
    s.update(search_result(&v, many_hits(n), n as u32, 9_000), now);
}

#[test]
fn right_moves_the_selection_a_whole_column() {
    let (mut s, now, rows) = grid_state(10);
    with_results(&mut s, now, 200);

    s.update(press(KeyCode::Down), now); // into the results
    let before = s.selected_row().unwrap();
    s.update(press(KeyCode::Right), now);
    assert_eq!(s.selected_row(), Some(before + rows));

    s.update(press(KeyCode::Left), now);
    assert_eq!(s.selected_row(), Some(before), "and back again");
}

/// Horizontal movement clamps where vertical movement wraps. Wrapping
/// sideways would land on an arbitrary rank, since the result count is not a
/// multiple of the column height.
#[test]
fn right_at_the_last_column_clamps_rather_than_wrapping() {
    let (mut s, now, _) = grid_state(10);
    with_results(&mut s, now, 200);

    s.update(press(KeyCode::Down), now);
    s.update(press(KeyCode::End), now);
    let last = s.selected_row().unwrap();
    assert_eq!(last, 199);

    let r = s.update(press(KeyCode::Right), now);
    assert_eq!(s.selected_row(), Some(last), "stays put");
    assert_eq!(
        r.redraw,
        Redraw::No,
        "and does not redraw an identical frame"
    );
}

#[test]
fn left_from_the_first_column_clamps_to_the_top() {
    let (mut s, now, _) = grid_state(10);
    with_results(&mut s, now, 200);

    s.update(press(KeyCode::Down), now);
    s.update(press(KeyCode::Left), now);
    assert_eq!(s.selected_row(), Some(0));
}

/// While typing, the arrows still belong to the caret.
#[test]
fn left_and_right_move_the_caret_when_the_input_has_focus() {
    let (mut s, now, _) = grid_state(10);
    with_results(&mut s, now, 200);
    assert_eq!(s.focus, Focus::Input);

    s.update(press(KeyCode::Left), now);
    assert_eq!(s.focus, Focus::Input, "must not step into the results");
    s.update(key('X'), now);
    assert_eq!(s.input.text(), "11-D-070X4", "the caret moved, not the row");
}

/// Down walks rank by rank and wraps from the foot of one column to the head
/// of the next, which is what makes the newspaper flow readable.
#[test]
fn down_crosses_from_one_column_to_the_next() {
    let (mut s, now, rows) = grid_state(6);
    with_results(&mut s, now, 100);

    s.update(press(KeyCode::Down), now);
    for _ in 1..rows {
        s.update(press(KeyCode::Down), now);
    }
    assert_eq!(
        s.selected_row(),
        Some(rows),
        "one past the foot of column one is the head of column two"
    );
}

#[test]
fn the_visible_page_follows_the_selection() {
    let (mut s, now, rows) = grid_state(10);
    with_results(&mut s, now, 200);
    let page = rows * 3;

    assert_eq!(s.visible_range(), 0..page, "starts on the first page");

    s.update(press(KeyCode::Down), now);
    s.update(press(KeyCode::End), now);
    let last = s.visible_range();
    assert!(last.contains(&199), "the last page holds the last result");
    assert_eq!(last.end, 200, "and is clipped to what exists");
}

#[test]
fn page_down_advances_a_whole_screen_and_page_up_returns() {
    let (mut s, now, rows) = grid_state(10);
    with_results(&mut s, now, 200);
    let page = rows * 3;

    s.update(press(KeyCode::PageDown), now);
    assert_eq!(s.focus, Focus::Results, "paging steps into the results");
    assert_eq!(s.selected_row(), Some(page));
    assert_eq!(s.visible_range(), page..page * 2);

    s.update(press(KeyCode::PageUp), now);
    assert_eq!(s.selected_row(), Some(0));
    assert_eq!(s.visible_range(), 0..page);
}

/// The cap is what makes the grid worth having; 15 was one column.
#[test]
fn far_more_than_one_screen_of_results_is_reachable() {
    let (mut s, now, rows) = grid_state(10);
    with_results(&mut s, now, files::config::MAX_RESULTS);

    s.update(press(KeyCode::Down), now);
    s.update(press(KeyCode::End), now);
    assert_eq!(
        s.selected_row(),
        Some(files::config::MAX_RESULTS - 1),
        "the last of {} results is reachable",
        files::config::MAX_RESULTS
    );
    assert!(
        files::config::MAX_RESULTS > rows * 3,
        "the cap has to exceed one screen or none of this matters"
    );
}
