//! Tests the overlay's summon and dismiss behaviour and which searches reach the
//! recall list.
//!
//! The state machine is a pure function of (event, time), so nothing here
//! touches Windows. Whether the overlay is up is reported by the window thread
//! as `HotkeyMsg` events rather than decided by the state machine.

use std::sync::Arc;
use std::time::Instant;

use files::app::event::{AppEvent, Cmd, HotkeyMsg, Response, SearchMsg, VerifyMsg};
use files::app::key::{Key, KeyEvent, KeyPhase, Mods};
use files::app::state::pointer::Intent;
use files::app::state::{AppState, QueryPhase};
use files::config::{REMEMBER_DEBOUNCE, Settings, VERIFY_DEBOUNCE};
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;
use files::search::verify::{SkipReason, VerifyOutcome};

const TERM: &str = "AB-C-0704";

/// The default settings plus one configured viewer, `sketch`.
fn with_sketch() -> Settings {
    Settings {
        viewers: std::sync::Arc::new(files::config::Viewers::new(vec![
            files::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
        ])),
        ..Settings::fixture()
    }
}

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    let s = AppState::new(with_sketch(), now);
    (s, now)
}

fn unconfigured_state() -> (AppState, Instant) {
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

/// Delivers the local matcher's answer for whatever was last typed.
fn deliver_hits(s: &mut AppState, hits: Vec<Hit>, now: Instant) {
    let matched = hits.len() as u32;
    s.update(
        AppEvent::Search(SearchMsg {
            epoch: s.query_epoch(),
            query: Query::parse(s.input.text()),
            elapsed: std::time::Duration::ZERO,
            result: Ok(SearchOutcome {
                hits,
                matched,
                total: matched,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );
}

/// Types a search and ticks past both `VERIFY_DEBOUNCE` and `REMEMBER_DEBOUNCE`.
///
/// No `VerifyMsg` is delivered, so callers show the history commit does not
/// depend on the server. Returns the settle time and the response of the
/// final tick, which carries the history write.
fn settle(s: &mut AppState, term: &str, now: Instant) -> (Instant, Response) {
    type_in(s, term, now);
    quiet(s, now)
}

/// Ticks past both debounces for a search already typed.
fn quiet(s: &mut AppState, typed_at: Instant) -> (Instant, Response) {
    s.update(AppEvent::Tick, typed_at + VERIFY_DEBOUNCE);
    let later = typed_at + REMEMBER_DEBOUNCE;
    let response = s.update(AppEvent::Tick, later);
    (later, response)
}

fn summon(s: &mut AppState, now: Instant) -> Response {
    s.update(AppEvent::Hotkey(HotkeyMsg::Summoned), now)
}

fn saved(r: &Response) -> Option<Vec<String>> {
    r.cmds.iter().find_map(|c| match c {
        Cmd::SaveHistory(entries) => Some(entries.as_ref().clone()),
        _ => None,
    })
}

fn saves(r: &Response) -> usize {
    r.cmds
        .iter()
        .filter(|c| matches!(c, Cmd::SaveHistory(_)))
        .count()
}

fn has(r: &Response, want: &Cmd) -> bool {
    r.cmds.iter().any(|c| c == want)
}

// --- summoning --------------------------------------------------------

#[test]
fn the_hotkey_shrinks_the_program_to_the_overlay() {
    let (mut s, now) = state();
    assert!(!s.overlay_up);
    summon(&mut s, now);
    assert!(s.overlay_up);
}

/// Keeps the search and its results; selecting it lets the next keystroke
/// replace it.
#[test]
fn the_hotkey_keeps_the_search_already_typed_and_selects_it() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);
    summon(&mut s, now);
    assert_eq!(s.input.text(), TERM);
    assert!(s.input.has_selection(), "the search should be selected");
}

#[test]
fn the_first_keystroke_in_the_overlay_replaces_the_search_that_was_there() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);
    summon(&mut s, now);
    type_in(&mut s, "P", now);
    assert_eq!(s.input.text(), "P");
}

/// Summoning leaves the query epoch unchanged, so no debounce or server check
/// is re-armed.
#[test]
fn summoning_the_overlay_does_not_re_arm_the_server_check() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    let epoch = s.query_epoch();
    assert!(
        s.verify_due_at().is_none(),
        "the debounce should have fired"
    );

    let r = summon(&mut s, settled);

    assert_eq!(s.query_epoch(), epoch, "the generation moved");
    assert!(s.verify_due_at().is_none(), "the debounce was re-armed");
    assert!(
        !r.cmds
            .iter()
            .any(|c| matches!(c, Cmd::Search { .. } | Cmd::Verify { .. })),
        "summoning dispatched work: {:?}",
        r.cmds
    );
}

#[test]
fn summoning_the_overlay_stops_whatever_was_being_browsed() {
    let (mut s, now) = state();
    s.seed_history(vec!["older".into(), "newer".into()]);
    s.update(press(Key::Up), now);
    assert!(s.history.is_browsing());

    summon(&mut s, now);
    assert!(s.history.cursor().is_none());
}

// --- dismissing -------------------------------------------------------

#[test]
fn a_second_hotkey_press_asks_for_the_window_back() {
    let (mut s, now) = state();
    summon(&mut s, now);
    // The window thread toggles; a repeated summon is a no-op here and the
    // dismiss arrives as its own message.
    s.update(AppEvent::Hotkey(HotkeyMsg::Summoned), now);
    assert!(s.overlay_up);
    s.update(AppEvent::Hotkey(HotkeyMsg::Dismissed), now);
    assert!(!s.overlay_up);
}

/// Escape closes in one press and keeps the search.
///
/// Clearing first would run `on_input_changed`, dropping the debounce and
/// resetting the phase before the search could be committed.
#[test]
fn escape_in_the_overlay_closes_it_in_one_press_and_keeps_the_search() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    summon(&mut s, settled);

    let r = s.update(press(Key::Esc), settled);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
    assert_eq!(s.input.text(), TERM, "the line was cleared");
}

/// The selection made by summoning does not absorb the first Escape.
#[test]
fn escape_in_the_overlay_closes_even_with_a_selection_on_the_line() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);
    summon(&mut s, now);
    assert!(s.input.has_selection(), "precondition: summoning selects");

    let r = s.update(press(Key::Esc), now);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
}

#[test]
fn escape_in_the_overlay_closes_from_the_results_too() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("a.pdf"), hit("b.pdf")], now);
    summon(&mut s, now);
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(1), "precondition: in the list");

    let r = s.update(press(Key::Esc), now);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
}

/// While browsing recent searches, the first Escape leaves the list and the
/// second closes the overlay, as with the drive picker.
#[test]
fn escape_leaves_the_recent_searches_before_it_closes_the_overlay() {
    let (mut s, now) = state();
    s.seed_history(vec!["older".into()]);
    summon(&mut s, now);
    s.update(press(Key::Up), now);
    assert!(s.history.is_browsing());

    let first = s.update(press(Key::Esc), now);
    assert!(
        !has(&first, &Cmd::DismissOverlay),
        "the first Escape took the panel with it: {:?}",
        first.cmds
    );
    assert!(!s.history.is_browsing(), "it did not leave the list");
    assert_eq!(s.input.text(), "", "and it did not put the field back");

    let second = s.update(press(Key::Esc), now);
    assert!(
        has(&second, &Cmd::DismissOverlay),
        "no dismiss: {:?}",
        second.cmds
    );
}

#[test]
fn opening_a_result_from_the_overlay_opens_it() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::Open(_))),
        "nothing was opened: {:?}",
        r.cmds
    );
}

/// The opened search is selected so the next keystroke replaces it.
#[test]
fn opening_a_result_selects_the_search_it_opened() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);
    summon(&mut s, settled);

    s.update(press(Key::Enter), settled);

    assert_eq!(
        s.input.selected_text(),
        Some(TERM),
        "the search was left unselected, so a second search types into the first"
    );
}

/// Opening a file hides the overlay by default.
///
/// Failures reported after hiding are covered by
/// `an_open_that_fails_after_the_panel_has_gone_is_still_reported`.
#[test]
fn opening_something_closes_the_overlay() {
    let now = Instant::now();
    let (mut s, _) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(has(&r, &Cmd::DismissOverlay), "the overlay stayed up");
}

/// With `hide_after_opening` off, the overlay stays up and the search is
/// selected.
#[test]
fn the_overlay_can_be_told_to_stay_up_after_an_open() {
    let now = Instant::now();
    let mut s = AppState::new(
        Settings {
            hide_after_opening: false,
            ..Settings::fixture()
        },
        now,
    );
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(!has(&r, &Cmd::DismissOverlay), "the overlay went anyway");
    assert_eq!(
        s.input.selected_text(),
        Some(TERM),
        "and the search is selected, so the next keystroke replaces it"
    );
}

/// Copy actions never hide the panel, because their only feedback is a toast.
#[test]
fn copying_leaves_the_panel_up_to_be_read() {
    let now = Instant::now();
    let (mut s, _) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);
    summon(&mut s, settled);

    for action in [
        files::view::actions::ActionId::CopyPath,
        files::view::actions::ActionId::CopyName,
    ] {
        let r = s.update(AppEvent::Intent(Intent::Act(action)), settled);
        assert!(
            r.cmds.iter().any(|c| matches!(c, Cmd::Copy(_))),
            "{action:?} copied nothing"
        );
        assert!(
            !has(&r, &Cmd::DismissOverlay),
            "{action:?} took the panel away with its own message"
        );
    }
}

/// Each action's `hides` flag matches whether it actually dismisses the
/// overlay.
///
/// The renderer reads `Action::hides`, while the state machine hides inside
/// `open_selection` and `reveal_selection`; this keeps the two in sync.
#[test]
fn every_hiding_action_hides() {
    use files::view::actions::ActionId;
    let now = Instant::now();

    for action in [
        ActionId::Open,
        ActionId::OpenAsDocument,
        ActionId::OpenWith(0),
        ActionId::OpenWithWindows,
        ActionId::Reveal,
        ActionId::CopyPath,
        ActionId::CopyName,
        ActionId::Refresh,
        ActionId::Settings,
    ] {
        let (mut s, _) = state();
        let (settled, _) = settle(&mut s, TERM, now);
        deliver_hits(&mut s, vec![hit("report.pdf")], settled);
        summon(&mut s, settled);

        // Skip actions the menu does not offer; one of the three opens is
        // always omitted because it duplicates the default.
        let Some(claimed) = files::view::actions::actions(&s)
            .into_iter()
            .find(|a| a.id == action)
            .map(|a| a.hides)
        else {
            continue;
        };
        let r = s.update(AppEvent::Intent(Intent::Act(action)), settled);
        assert_eq!(
            has(&r, &Cmd::DismissOverlay),
            claimed,
            "{action:?} claims hides: {claimed}"
        );
    }
}

/// An open that fails after the panel is hidden is announced, so the failure
/// is not lost.
#[test]
fn an_open_that_fails_after_the_panel_has_gone_is_still_reported() {
    let now = Instant::now();
    let (mut s, _) = state();
    assert!(!s.overlay_up, "the fixture has a panel to report on");

    let r = s.update(
        AppEvent::Open(files::app::event::OpenMsg::Failed {
            path: std::sync::Arc::from(r"R:\projects\AB-C-0704\GA.pdf"),
            detail: "the file no longer exists".into(),
        }),
        now,
    );
    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::Announce { .. })),
        "the failure went nowhere: {:?}",
        r.cmds
    );
}

/// With the panel up, an open failure is a toast rather than an announcement.
#[test]
fn an_open_that_fails_with_the_panel_up_is_a_toast() {
    let now = Instant::now();
    let (mut s, _) = state();
    summon(&mut s, now);

    let r = s.update(
        AppEvent::Open(files::app::event::OpenMsg::Failed {
            path: std::sync::Arc::from(r"R:\projects\AB-C-0704\GA.pdf"),
            detail: "the file no longer exists".into(),
        }),
        now,
    );
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Announce { .. })));
    assert!(s.toast.is_some(), "and nothing was said at all");
}

#[test]
fn losing_focus_puts_the_panel_away() {
    let now = Instant::now();
    let (mut s, _) = state();
    summon(&mut s, now);

    let r = s.update(AppEvent::WindowFocus(false), now);
    assert!(has(&r, &Cmd::DismissOverlay), "the panel stayed up");
}

/// Losing focus does not dismiss when `hide_on_blur` is off or the panel is
/// not up, and gaining focus never dismisses.
#[test]
fn losing_focus_does_nothing_when_it_should_not() {
    let now = Instant::now();

    let mut off = AppState::new(
        Settings {
            hide_on_blur: false,
            ..Settings::fixture()
        },
        now,
    );
    summon(&mut off, now);
    let r = off.update(AppEvent::WindowFocus(false), now);
    assert!(!has(&r, &Cmd::DismissOverlay), "the setting was ignored");

    // Never summoned: there is no overlay to dismiss.
    let (mut down, _) = state();
    let r = down.update(AppEvent::WindowFocus(false), now);
    assert!(!has(&r, &Cmd::DismissOverlay));

    let (mut up, _) = state();
    summon(&mut up, now);
    let r = up.update(AppEvent::WindowFocus(true), now);
    assert!(!has(&r, &Cmd::DismissOverlay));
}

/// With `hide_on_escape` off, Escape clears instead of closing.
#[test]
fn escape_can_be_told_to_clear_rather_than_close() {
    let now = Instant::now();
    let mut s = AppState::new(
        Settings {
            hide_on_escape: false,
            ..Settings::fixture()
        },
        now,
    );
    let (settled, _) = settle(&mut s, TERM, now);
    summon(&mut s, settled);

    let r = s.update(press(Key::Esc), settled);
    assert!(!has(&r, &Cmd::DismissOverlay), "Escape closed the panel");
    // Summoning selects the whole search, so the first Escape drops the
    // selection and the second clears the box.
    s.update(press(Key::Esc), settled);
    assert_eq!(s.input.text(), "", "the search was not cleared");
}

/// The overlay stays up so the "nothing to open" message can be read.
#[test]
fn enter_with_nothing_to_open_keeps_the_overlay_up() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(!has(&r, &Cmd::DismissOverlay), "the overlay closed");
    assert!(s.overlay_up);
}

#[test]
fn escape_outside_the_overlay_clears_the_line_and_touches_no_window() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);

    let r = s.update(press(Key::Esc), now);

    assert!(!has(&r, &Cmd::DismissOverlay));
    assert!(s.input.is_empty());
}

#[test]
fn ctrl_q_still_quits_from_inside_the_overlay() {
    let (mut s, now) = state();
    summon(&mut s, now);
    let r = s.update(ctrl(Key::Char('q')), now);
    assert!(s.should_quit);
    assert!(has(&r, &Cmd::Quit));
}

// --- what reaches the recall list -------------------------------------

/// A prefix that has local results but has not settled is not remembered.
///
/// `SEARCH_DEBOUNCE` is shorter than a typing pause, so a prefix can reach
/// `QueryPhase::Local` mid-typing; gating on the phase alone would record it.
#[test]
fn a_search_abandoned_mid_typing_is_not_remembered_when_the_overlay_closes() {
    let (mut s, now) = state();
    summon(&mut s, now);
    type_in(&mut s, "inv", now);
    deliver_hits(&mut s, vec![hit("invoice.pdf")], now);
    assert_eq!(s.phase, QueryPhase::Local, "precondition");
    assert!(s.verify_due_at().is_some(), "the debounce should be armed");

    let r = s.update(press(Key::Esc), now);

    assert!(
        s.history.is_empty(),
        "a prefix was remembered: {:?}",
        s.history.entries()
    );
    assert_eq!(saves(&r), 0);
}

/// A settled search is remembered by the quiet period alone, without a keypress
/// or a server answer.
///
/// `run_verify` skips the server entirely with two shares configured, so the
/// commit must not depend on verification.
#[test]
fn a_search_that_settled_is_remembered_without_anybody_pressing_anything() {
    let (mut s, now) = state();
    summon(&mut s, now);
    let (settled, r) = settle(&mut s, TERM, now);
    assert!(
        s.verify_due_at().is_none(),
        "the debounce should have fired"
    );

    assert_eq!(s.history.entries(), [TERM]);
    assert_eq!(saved(&r).as_deref(), Some(&[TERM.to_string()][..]));

    // The commit on close is a backstop and writes nothing more.
    assert_eq!(saves(&s.update(press(Key::Esc), settled)), 0);
}

/// A settled search with no results is remembered so it can be recalled and
/// corrected.
#[test]
fn a_search_that_found_nothing_is_remembered_too() {
    let (mut s, now) = state();
    type_in(&mut s, TERM, now);
    deliver_hits(&mut s, vec![], now);
    assert!(s.hits.is_empty(), "precondition");

    let (settled, r) = quiet(&mut s, now);

    assert_eq!(s.history.entries(), [TERM]);
    assert_eq!(saved(&r).as_deref(), Some(&[TERM.to_string()][..]));

    // Enter opens nothing and writes no second entry.
    let enter = s.update(press(Key::Enter), settled);
    assert!(!enter.cmds.iter().any(|c| matches!(c, Cmd::Open(_))));
    assert_eq!(saves(&enter), 0);
}

#[test]
fn enter_on_a_half_typed_search_with_nothing_to_open_remembers_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "inv", now);
    deliver_hits(&mut s, vec![], now);

    let r = s.update(press(Key::Enter), now);

    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// Four commit points produce one entry and one write.
///
/// `History::record` refuses a duplicate head entry and `remember` then emits
/// no command.
#[test]
fn a_settled_search_is_written_once_however_many_triggers_fire() {
    let (mut s, now) = state();
    // 1. The quiet period expiring.
    let (settled, quiet) = settle(&mut s, TERM, now);
    deliver_hits(&mut s, vec![hit("report.pdf")], settled);

    // 2. Verification answering, skipped as with two shares configured.
    let verify = s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::contains(TERM),
            elapsed: std::time::Duration::ZERO,
            outcome: VerifyOutcome::Skipped(SkipReason::SeveralShares),
        }),
        settled,
    );
    // 3. Opening from the overlay.
    summon(&mut s, settled);
    let enter = s.update(press(Key::Enter), settled);
    // 4. The resulting dismiss.
    let dismissed = s.update(AppEvent::Hotkey(HotkeyMsg::Dismissed), settled);

    let total = saves(&quiet) + saves(&verify) + saves(&enter) + saves(&dismissed);
    assert_eq!(total, 1, "the search was written {total} times");
    assert_eq!(s.history.entries(), [TERM]);
}

/// Typing one search in bursts with real pauses leaves one history entry.
///
/// Other helpers use one `Instant` for every character, so no deadline can
/// expire mid-search. Here the search is typed in three bursts, with gaps longer
/// than the quiet period. `REMEMBER_DEBOUNCE` absorbs short gaps and
/// `History::record` collapses a remembered prefix.
#[test]
fn typing_one_search_with_pauses_leaves_exactly_one_entry() {
    let (mut s, mut now) = state();
    summon(&mut s, now);

    let mut written: Vec<Vec<String>> = Vec::new();
    // The two-second pauses exceed the quiet period.
    for (burst, pause) in [
        ("AB", std::time::Duration::from_millis(700)),
        ("-C", std::time::Duration::from_millis(2_000)),
        ("-0704", std::time::Duration::from_millis(2_000)),
    ] {
        for c in burst.chars() {
            let r = s.update(key(c), now);
            written.extend(saved(&r));
            now += std::time::Duration::from_millis(120);
        }
        // Ticks through the pause at a fixed step. `next_deadline` is not
        // used because some of its terms depend on the last drawn frame.
        let until = now + pause;
        while now < until {
            now += std::time::Duration::from_millis(50);
            written.extend(saved(&s.update(AppEvent::Tick, now)));
        }
    }

    assert_eq!(
        s.history.entries(),
        [TERM],
        "the prefixes typed on the way to it were kept"
    );

    // `AB-C` settles during the long pause and is written, then replaced when
    // typing resumes. Every write must hold exactly one entry.
    assert!(
        written.iter().all(|entries| entries.len() == 1),
        "a write left a prefix behind: {written:?}"
    );
    assert_eq!(
        written.last().map(Vec::as_slice),
        Some(&[TERM.to_string()][..]),
        "the last write is the finished search"
    );
}

#[test]
fn three_searches_looked_up_in_turn_are_three_entries() {
    let (mut s, mut now) = state();
    for term in ["AB-C-0704", "P12345-001", "XY-Z-0001"] {
        let (settled, _) = settle(&mut s, term, now);
        now = settled + std::time::Duration::from_secs(1);
        // Clear the line before the next search.
        s.update(ctrl(Key::Char('u')), now);
    }
    assert_eq!(
        s.history.entries(),
        ["XY-Z-0001", "P12345-001", "AB-C-0704"]
    );
}

/// Browsing and then closing does not reorder history.
///
/// The commit in `request_dismiss` runs before browsing ends, so a recalled
/// search on the line would otherwise be recorded.
#[test]
fn browsing_recall_and_then_closing_the_overlay_does_not_reorder_the_list() {
    let (mut s, now) = state();
    s.seed_history(vec!["newest".into(), "older".into()]);
    summon(&mut s, now);
    s.update(press(Key::Up), now);
    s.update(press(Key::Up), now);
    assert!(s.history.is_browsing());
    assert_eq!(s.input.text(), "older");

    let r = s.update(AppEvent::Hotkey(HotkeyMsg::Summoned), now);
    let r2 = s.update(press(Key::Esc), now);

    assert_eq!(s.history.entries(), ["newest", "older"]);
    assert_eq!(saves(&r) + saves(&r2), 0);
}

/// Queries shorter than `MIN_REMEMBERED_LEN` are searched but never
/// remembered.
#[test]
fn a_query_below_the_remembering_length_is_never_remembered() {
    let (mut s, now) = state();
    summon(&mut s, now);
    type_in(&mut s, "in", now);
    let later = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, later);

    let r = s.update(press(Key::Esc), later);

    assert!("in".chars().count() < files::config::MIN_REMEMBERED_LEN);
    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// With no share configured nothing is searched, so nothing is remembered.
#[test]
fn a_search_typed_where_no_share_is_configured_is_not_remembered() {
    let (mut s, now) = unconfigured_state();
    summon(&mut s, now);
    type_in(&mut s, TERM, now);
    let later = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, later);

    let r = s.update(press(Key::Esc), later);

    assert_eq!(s.phase, QueryPhase::NoShares);
    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// An unavailable hotkey window is reported as a toast.
#[test]
fn an_unreachable_window_is_reported_rather_than_left_mysterious() {
    let (mut s, now) = state();
    s.update(
        AppEvent::Hotkey(HotkeyMsg::Unavailable {
            reason: "there is no console window to move".into(),
        }),
        now,
    );
    let toast = s.toast.as_ref().expect("no toast");
    assert!(toast.text.contains("console window"), "{}", toast.text);
}

/// Key releases are ignored; Windows sends one per press, and acting on both
/// would toggle twice.
#[test]
fn a_key_release_in_the_overlay_does_nothing() {
    let (mut s, now) = state();
    summon(&mut s, now);
    let release = AppEvent::Key(KeyEvent {
        key: Key::Esc,
        mods: Mods::NONE,
        phase: KeyPhase::Release,
    });
    let r = s.update(release, now);
    assert!(!has(&r, &Cmd::DismissOverlay));
    assert!(s.overlay_up);
}

/// `Ctrl+,` requests the separate settings window and leaves the panel up.
#[test]
fn ctrl_comma_asks_for_the_settings_without_disturbing_the_panel() {
    let (mut s, now) = state();
    summon(&mut s, now);

    let opened = s.update(
        AppEvent::Key(KeyEvent::new(Key::Char(','), Mods::CTRL)),
        now,
    );
    assert!(
        has(&opened, &Cmd::ToggleSettings),
        "Ctrl+, should ask for the settings window: {:?}",
        opened.cmds
    );
    assert!(s.overlay_up, "and must not disturb the panel");

    // Escape still dismisses the overlay directly.
    let dismissed = s.update(press(Key::Esc), now);
    assert!(
        has(&dismissed, &Cmd::DismissOverlay),
        "{:?}",
        dismissed.cmds
    );
}
