//! Caret movement, selection, recall and the mouse, driven through the real
//! state machine with a fake clock.
//!
//! `AppState::update` is a pure function of (event, time), so all of this is
//! reachable without a terminal - which matters more here than usual, because
//! the behaviour being tested is almost entirely about where a caret is and
//! what is highlighted, and neither is visible from outside a running program.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use files::app::event::{AppEvent, Cmd, Response, VerifyMsg};
use files::app::state::{AppState, Focus};
use files::config::{MULTI_CLICK_WINDOW, Settings, VERIFY_DEBOUNCE};
use files::search::matcher::Hit;
use files::search::verify::VerifyOutcome;
use files::ui;

const COLS: u16 = 100;
const ROWS: u16 = 30;

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    let mut s = AppState::new(Settings::default(), now);
    s.set_area(ratatui::layout::Rect::new(0, 0, COLS, ROWS));
    (s, now)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, modifiers))
}

fn press(code: KeyCode) -> AppEvent {
    key(code, KeyModifiers::NONE)
}

fn shift(code: KeyCode) -> AppEvent {
    key(code, KeyModifiers::SHIFT)
}

fn ctrl(code: KeyCode) -> AppEvent {
    key(code, KeyModifiers::CONTROL)
}

fn type_in(s: &mut AppState, text: &str, now: Instant) {
    for c in text.chars() {
        s.update(key(KeyCode::Char(c), KeyModifiers::NONE), now);
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

/// Drives a code all the way to a completed verification, which is what makes
/// it eligible for recall.
fn search_and_verify(s: &mut AppState, code: &str, now: Instant) -> Response {
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
    )
}

// --- editing ----------------------------------------------------------

#[test]
fn a_character_can_be_fixed_in_the_middle_of_a_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-074", now);
    s.update(press(KeyCode::Left), now);
    type_in(&mut s, "0", now);
    assert_eq!(s.input, "11-D-0704");
}

#[test]
fn delete_removes_forwards_and_backspace_removes_backwards() {
    let (mut s, now) = state();
    type_in(&mut s, "abcd", now);
    s.update(press(KeyCode::Home), now);
    s.update(press(KeyCode::Delete), now);
    assert_eq!(s.input, "bcd");
    s.update(press(KeyCode::End), now);
    s.update(press(KeyCode::Backspace), now);
    assert_eq!(s.input, "bc");
}

#[test]
fn ctrl_w_deletes_the_previous_field_of_the_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(KeyCode::Char('w')), now);
    assert_eq!(s.input, "11-D-");
}

#[test]
fn ctrl_a_selects_the_whole_code_and_typing_replaces_it() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(KeyCode::Char('a')), now);
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
        shift(KeyCode::Left),
        shift(KeyCode::Left),
        press(KeyCode::Left),
        press(KeyCode::Right),
        shift(KeyCode::Home),
        shift(KeyCode::End),
        press(KeyCode::Home),
        press(KeyCode::End),
        ctrl(KeyCode::Left),
        ctrl(KeyCode::Right),
        ctrl(KeyCode::Char('a')),
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
    s.update(ctrl(KeyCode::Char('a')), now);
    s.update(AppEvent::Paste("P12345-001".into()), now);
    assert_eq!(s.input, "P12345-001");
}

// --- escape -----------------------------------------------------------

#[test]
fn escape_drops_the_selection_before_it_clears_the_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(ctrl(KeyCode::Char('a')), now);

    s.update(press(KeyCode::Esc), now);
    assert_eq!(
        s.input, "11-D-0704",
        "the first Esc only drops the highlight"
    );

    s.update(press(KeyCode::Esc), now);
    assert_eq!(s.input, "");
    assert!(!s.should_quit);
}

// --- focus ------------------------------------------------------------

#[test]
fn down_steps_into_the_results_and_up_from_the_top_steps_back_out() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        AppEvent::Search(files::app::event::SearchMsg {
            epoch: s.query_epoch(),
            query: s.input.text().to_string(),
            elapsed: Duration::from_micros(200),
            result: Ok(files::search::matcher::SearchOutcome {
                hits: vec![hit("a.pdf"), hit("b.pdf")],
                matched: 2,
                total: 2,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );

    // The top row is already highlighted before any key is pressed, so the
    // first Down moves off it rather than onto it.
    assert_eq!(s.focus, Focus::Input);
    assert_eq!(s.selected_row(), Some(0));

    s.update(press(KeyCode::Down), now);
    assert_eq!(s.focus, Focus::Results);
    assert_eq!(s.selected_row(), Some(1));

    s.update(press(KeyCode::Up), now);
    assert_eq!(s.selected_row(), Some(0));

    // Off the top of the list is a return to typing, not a wrap to the
    // bottom, which would throw the eye to the far end of the list.
    s.update(press(KeyCode::Up), now);
    assert_eq!(s.focus, Focus::Input);
}

// --- recall -----------------------------------------------------------

#[test]
fn a_verified_code_becomes_recallable_and_is_stored() {
    let (mut s, now) = state();
    let r = search_and_verify(&mut s, "11-D-0704", now);

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

/// The prefixes typed on the way to a code must not fill the list. Nothing
/// filters them explicitly - they simply never reach a verification.
#[test]
fn the_prefixes_typed_on_the_way_to_a_code_are_not_remembered() {
    let (mut s, now) = state();
    search_and_verify(&mut s, "11-D-0704", now);
    assert_eq!(s.history.len(), 1);
    assert_eq!(s.history.entries(), ["11-D-0704"]);
}

#[test]
fn up_recalls_the_previous_code_and_down_brings_the_draft_back() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);

    type_in(&mut s, "AB1", now);
    s.update(press(KeyCode::Up), now);
    assert_eq!(s.focus, Focus::History);
    assert_eq!(s.input, "P12345-001");

    s.update(press(KeyCode::Up), now);
    assert_eq!(s.input, "11-D-0704");

    s.update(press(KeyCode::Down), now);
    assert_eq!(s.input, "P12345-001");

    s.update(press(KeyCode::Down), now);
    assert_eq!(
        s.input, "AB1",
        "past the newest, the half-typed code returns"
    );
    assert_eq!(s.focus, Focus::Input);
}

#[test]
fn escape_during_recall_restores_what_was_being_typed() {
    let (mut s, now) = state();
    s.seed_history(vec!["11-D-0704".into()]);
    type_in(&mut s, "AB1", now);

    s.update(press(KeyCode::Up), now);
    assert_eq!(s.input, "11-D-0704");

    s.update(press(KeyCode::Esc), now);
    assert_eq!(s.input, "AB1");
    assert_eq!(s.focus, Focus::Input);
}

/// Stepping through recall must not touch the network: twenty codes would be
/// twenty round trips for codes nobody has chosen.
#[test]
fn browsing_recall_dispatches_no_work_until_an_entry_is_taken() {
    let (mut s, now) = state();
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);

    let open = s.update(press(KeyCode::Up), now);
    let step = s.update(press(KeyCode::Up), now);
    for r in [&open, &step] {
        assert!(
            r.cmds.iter().all(|c| !matches!(
                c,
                Cmd::Search { .. } | Cmd::Verify { .. } | Cmd::Prefetch { .. }
            )),
            "recall must not search: {:?}",
            r.cmds
        );
    }
    assert!(
        s.verify_due_at().is_none(),
        "a pending verify would fire for a code nobody chose"
    );

    let taken = s.update(press(KeyCode::Enter), now);
    assert!(
        taken.cmds.iter().any(|c| matches!(c, Cmd::Search { .. })),
        "taking an entry searches for it: {:?}",
        taken.cmds
    );
    assert_eq!(s.focus, Focus::Input);
}

#[test]
fn typing_during_recall_keeps_the_recalled_code_and_edits_it() {
    let (mut s, now) = state();
    s.seed_history(vec!["11-D-0704".into()]);
    s.update(press(KeyCode::Up), now);
    type_in(&mut s, "X", now);

    assert_eq!(s.focus, Focus::Input);
    assert_eq!(s.input, "11-D-0704X");
}

#[test]
fn recall_with_nothing_remembered_says_so_rather_than_doing_nothing() {
    let (mut s, now) = state();
    s.update(press(KeyCode::Up), now);
    assert_eq!(s.focus, Focus::Input);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("previous")),
        "{:?}",
        s.toast
    );
}

// --- mouse ------------------------------------------------------------

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn input_cell(s: &AppState, column_in_text: u16) -> (u16, u16) {
    let chunks = ui::layout(s.area);
    (
        chunks.input_text_x() + column_in_text,
        chunks.input_text_y(),
    )
}

#[test]
fn clicking_in_the_box_puts_the_caret_where_it_was_clicked() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let (x, y) = input_cell(&s, 2);
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y), now);
    // Typing is the observable proof of where the caret landed.
    type_in(&mut s, "X", now);
    assert_eq!(s.input, "11X-D-0704");
}

#[test]
fn dragging_across_the_box_selects_the_run_dragged_over() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let (x0, y) = input_cell(&s, 0);
    let (x1, _) = input_cell(&s, 4);
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x0, y), now);
    s.update(mouse(MouseEventKind::Drag(MouseButton::Left), x1, y), now);
    s.update(mouse(MouseEventKind::Up(MouseButton::Left), x1, y), now);

    let r = s.update(ctrl(KeyCode::Char('c')), now);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Copy(text) if text == "11-D")),
        "{:?}",
        r.cmds
    );
}

/// A drag that began outside the search box must not select text in it.
#[test]
fn dragging_without_a_press_in_the_box_selects_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    let (x, y) = input_cell(&s, 4);
    s.update(mouse(MouseEventKind::Drag(MouseButton::Left), x, y), now);

    s.update(ctrl(KeyCode::Char('c')), now);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("nothing selected")),
        "{:?}",
        s.toast
    );
}

#[test]
fn a_double_click_selects_the_whole_code() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let (x, y) = input_cell(&s, 4);
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y), now);
    let second = now + MULTI_CLICK_WINDOW / 2;
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y), second);

    let r = s.update(ctrl(KeyCode::Char('c')), second);
    assert!(
        r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Copy(text) if text == "11-D-0704")),
        "a double-click must take the whole code, not one dash-delimited \
         field: {:?}",
        r.cmds
    );
}

#[test]
fn two_slow_clicks_are_two_single_clicks() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);

    let (x, y) = input_cell(&s, 4);
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y), now);
    let late = now + MULTI_CLICK_WINDOW * 2;
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y), late);

    s.update(ctrl(KeyCode::Char('c')), late);
    assert!(
        s.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("nothing selected")),
        "a slow second click must not select: {:?}",
        s.toast
    );
}

#[test]
fn clicking_a_result_row_selects_it() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        AppEvent::Search(files::app::event::SearchMsg {
            epoch: s.query_epoch(),
            query: s.input.text().to_string(),
            elapsed: Duration::from_micros(200),
            result: Ok(files::search::matcher::SearchOutcome {
                hits: vec![hit("a.pdf"), hit("b.pdf"), hit("c.pdf")],
                matched: 3,
                total: 3,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );

    let chunks = ui::layout(s.area);
    let row = chunks.first_row_y() + 2;
    s.update(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            chunks.results.x + 4,
            row,
        ),
        now,
    );
    assert_eq!(s.focus, Focus::Results);
    assert_eq!(s.selected_row(), Some(2));
}

#[test]
fn the_wheel_moves_the_result_selection() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(
        AppEvent::Search(files::app::event::SearchMsg {
            epoch: s.query_epoch(),
            query: s.input.text().to_string(),
            elapsed: Duration::from_micros(200),
            result: Ok(files::search::matcher::SearchOutcome {
                hits: vec![hit("a.pdf"), hit("b.pdf")],
                matched: 2,
                total: 2,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );

    let chunks = ui::layout(s.area);
    let (x, y) = (chunks.results.x + 4, chunks.first_row_y());
    s.update(mouse(MouseEventKind::ScrollDown, x, y), now);
    assert_eq!(s.focus, Focus::Results);
    assert_eq!(s.selected_row(), Some(1));
}

#[test]
fn a_click_outside_every_pane_changes_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "11-D-0704", now);
    s.update(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), now);
    assert_eq!(s.input, "11-D-0704");
    assert_eq!(s.focus, Focus::Input);
}
