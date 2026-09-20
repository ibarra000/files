//! The quick-search overlay, and the rule about what reaches the recall list.
//!
//! Two features that have to be tested together, because the interesting
//! question about the overlay is what it leaves behind when it closes.
//!
//! Nothing here touches Windows. The state machine is a pure function of
//! (event, time), so a summon is `AppEvent::Hotkey(HotkeyMsg::Summoned)` and
//! the window that would really have moved is somebody else's problem - which
//! is exactly why the decision about whether the overlay is up is reported by
//! the thread that owns the window rather than decided here.

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

const CODE: &str = "11-D-0704";

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    let s = AppState::new(Settings::default(), now);
    // Big enough that the results pane has room, so nothing here is an
    // accidental test of a one-row terminal.
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

/// Types a code and lets the typing settle: both deadlines expire, which is
/// what "the user stopped typing" actually means to the state machine.
///
/// Two ticks, because there are two clocks and they are deliberately far
/// apart. `VERIFY_DEBOUNCE` is the one that asks the server; `REMEMBER_DEBOUNCE`
/// is the one that decides the code was meant. They used to be the same event,
/// which is how a pause in the middle of a code ended up in the recall list.
///
/// Deliberately stops short of delivering a `VerifyMsg`, so every test using
/// this proves the commit does *not* depend on the server ever answering.
/// Returns the moment it settled and the response from the tick that settled
/// it - which is where the write now happens, so a test that cares has to be
/// able to see it.
fn settle(s: &mut AppState, code: &str, now: Instant) -> (Instant, Response) {
    type_in(s, code, now);
    quiet(s, now)
}

/// Lets the typing stop, for a code already on the line.
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

/// Clearing on summon would throw away both a code somebody may still want and
/// the results already computed for it. Selecting it means the next character
/// replaces it anyway, so nothing is lost either way round.
#[test]
fn the_hotkey_keeps_the_code_already_typed_and_selects_it() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);
    summon(&mut s, now);
    assert_eq!(s.input.text(), CODE);
    assert!(s.input.has_selection(), "the code should be selected");
}

#[test]
fn the_first_keystroke_in_the_overlay_replaces_the_code_that_was_there() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);
    summon(&mut s, now);
    type_in(&mut s, "P", now);
    assert_eq!(s.input.text(), "P");
}

/// Summoning the window is not editing the code. Bumping the generation would
/// re-arm the debounce and spend a server round trip on a code nobody retyped.
#[test]
fn summoning_the_overlay_does_not_re_arm_the_server_check() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, CODE, now);
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

/// Whatever was being browsed last time is not what this summon is about.
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
    // The thread toggles; from this side a second summon is simply the state
    // it is already in, and the dismiss arrives as its own report.
    s.update(AppEvent::Hotkey(HotkeyMsg::Summoned), now);
    assert!(s.overlay_up);
    s.update(AppEvent::Hotkey(HotkeyMsg::Dismissed), now);
    assert!(!s.overlay_up);
}

/// "Escape clears, a second Escape closes" is not merely worse here, it is
/// broken: clearing runs `on_input_changed`, which drops the debounce deadline
/// and puts the phase back to `Idle`, so the code and the fact that it had
/// settled are both gone before the second press could commit it.
#[test]
fn escape_in_the_overlay_closes_it_in_one_press_and_keeps_the_code() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    summon(&mut s, settled);

    let r = s.update(press(Key::Esc), settled);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
    assert_eq!(s.input.text(), CODE, "the line was cleared");
}

/// Escape does not peel in the overlay, and the selection is the reason it
/// must not: summoning selects the code so the next keystroke replaces it, so
/// a peeling Escape would spend its first press undoing something the user
/// never did and appear to ignore the key.
#[test]
fn escape_in_the_overlay_closes_even_with_a_selection_on_the_line() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);
    summon(&mut s, now);
    assert!(s.input.has_selection(), "precondition: summoning selects");

    let r = s.update(press(Key::Esc), now);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
}

/// A window covering somebody's work has to close without them first working
/// out which layer they are on. There are no layers left to be on, which is
/// most of why that is now true.
#[test]
fn escape_in_the_overlay_closes_from_the_results_too() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("a.pdf"), hit("b.pdf")], now);
    summon(&mut s, now);
    s.update(press(Key::Down), now);
    assert_eq!(s.selected_row(), Some(1), "precondition: in the list");

    let r = s.update(press(Key::Esc), now);

    assert!(has(&r, &Cmd::DismissOverlay), "no dismiss: {:?}", r.cmds);
}

/// ...except out of the recent codes, which peel first.
///
/// The exception the drive picker already has, and for the reason `on_escape`
/// gives for it: a list that cannot be shut without taking the panel with it is
/// a trap. The status line has always said "Esc to go back"; now something
/// keeps the promise.
///
/// The list only exists because the Up arrow opened it, so leaving it is a
/// thing the user can mean - which was not true while it was simply what an
/// empty field showed.
#[test]
fn escape_leaves_the_recent_codes_before_it_closes_the_overlay() {
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

    // The second one closes, exactly as it does from anywhere else.
    let second = s.update(press(Key::Esc), now);
    assert!(
        has(&second, &Cmd::DismissOverlay),
        "no dismiss: {:?}",
        second.cmds
    );
}

/// Enter opens the row the cursor is on, whatever else it does with the
/// panel afterwards.
#[test]
fn opening_a_result_from_the_overlay_opens_it() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(
        r.cmds.iter().any(|c| matches!(c, Cmd::Open(_))),
        "nothing was opened: {:?}",
        r.cmds
    );
}

/// The code just opened is selected, so the next one replaces it.
///
/// The counterpart to the summon behaviour: an overlay that stays up is only
/// useful for a second search if the first code is not in the way of typing
/// it.
#[test]
fn opening_a_result_selects_the_code_it_opened() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
    summon(&mut s, settled);

    s.update(press(Key::Enter), settled);

    assert_eq!(
        s.input.selected_text(),
        Some(CODE),
        "the code was left unselected, so a second search types into the first"
    );
}

/// Opening a file puts the panel away, which is what a launcher does.
///
/// This used to be off by default and the test was written the other way
/// round. The objection to switching it on was that an open is answered on
/// another thread, so everything the open had to say arrived at a window
/// that had gone - which is answered rather than overruled: see
/// `an_open_that_fails_after_the_panel_has_gone_is_still_reported` below.
#[test]
fn opening_something_closes_the_overlay() {
    let now = Instant::now();
    let (mut s, _) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(has(&r, &Cmd::DismissOverlay), "the overlay stayed up");
}

/// And it can be turned off, which is what it shipped as.
#[test]
fn the_overlay_can_be_told_to_stay_up_after_an_open() {
    let now = Instant::now();
    let mut s = AppState::new(
        Settings {
            hide_after_opening: false,
            ..Settings::default()
        },
        now,
    );
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(!has(&r, &Cmd::DismissOverlay), "the overlay went anyway");
    assert_eq!(
        s.input.selected_text(),
        Some(CODE),
        "and the code is selected, so the next keystroke replaces it"
    );
}

/// A copy leaves the panel up, whatever the setting says, because the whole
/// of what a copy reports is a toast.
#[test]
fn copying_leaves_the_panel_up_to_be_read() {
    let now = Instant::now();
    let (mut s, _) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
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

/// Every action that says it hides does, and every one that says it does not
/// does not.
///
/// `Action::hides` is a claim the renderer reads and the state machine never
/// consults - the hiding happens inside `open_selection` and
/// `reveal_selection`. Two facts in two places is two facts that drift, and
/// this is the join.
#[test]
fn every_hiding_action_hides() {
    use files::view::actions::ActionId;
    let now = Instant::now();

    for action in [
        ActionId::Open,
        ActionId::OpenAsDocument,
        ActionId::OpenInAvwin,
        ActionId::OpenWithWindows,
        ActionId::Reveal,
        ActionId::CopyPath,
        ActionId::CopyName,
        ActionId::Refresh,
        ActionId::Settings,
    ] {
        let (mut s, _) = state();
        let (settled, _) = settle(&mut s, CODE, now);
        deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);
        summon(&mut s, settled);

        // Only what is actually on offer. The menu lists the default open
        // and the two viewers it is *not*, so one of the three named opens
        // is always absent - and an action nobody can reach makes no claim
        // to check.
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

/// An open that fails after the panel has gone is still reported.
///
/// The whole of what makes hiding safe to ship switched on. A document
/// quietly missing page seven is the worst outcome this program can produce,
/// because nothing on screen would ever reveal it.
#[test]
fn an_open_that_fails_after_the_panel_has_gone_is_still_reported() {
    let now = Instant::now();
    let (mut s, _) = state();
    assert!(!s.overlay_up, "the fixture has a panel to report on");

    let r = s.update(
        AppEvent::Open(files::app::event::OpenMsg::Failed {
            path: std::sync::Arc::from(r"R:\jobs\11-D-0704\GA.pdf"),
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

/// And with the panel up it stays a toast, because there is somewhere to
/// read one. A modal on top of a window that is already saying it would be
/// the same news twice, with a button.
#[test]
fn an_open_that_fails_with_the_panel_up_is_a_toast() {
    let now = Instant::now();
    let (mut s, _) = state();
    summon(&mut s, now);

    let r = s.update(
        AppEvent::Open(files::app::event::OpenMsg::Failed {
            path: std::sync::Arc::from(r"R:\jobs\11-D-0704\GA.pdf"),
            detail: "the file no longer exists".into(),
        }),
        now,
    );
    assert!(!r.cmds.iter().any(|c| matches!(c, Cmd::Announce { .. })));
    assert!(s.toast.is_some(), "and nothing was said at all");
}

/// Clicking on something else puts the panel away, which is what a launcher
/// does and is the third of Ueli's three.
#[test]
fn losing_focus_puts_the_panel_away() {
    let now = Instant::now();
    let (mut s, _) = state();
    summon(&mut s, now);

    let r = s.update(AppEvent::WindowFocus(false), now);
    assert!(has(&r, &Cmd::DismissOverlay), "the panel stayed up");
}

/// And it can be turned off, and does nothing at all when the panel was
/// never up in the first place.
#[test]
fn losing_focus_does_nothing_when_it_should_not() {
    let now = Instant::now();

    let mut off = AppState::new(
        Settings {
            hide_on_blur: false,
            ..Settings::default()
        },
        now,
    );
    summon(&mut off, now);
    let r = off.update(AppEvent::WindowFocus(false), now);
    assert!(!has(&r, &Cmd::DismissOverlay), "the setting was ignored");

    // Never summoned: there is no panel to put away, and asking for one to
    // be dismissed would be the state machine telling the hotkey thread
    // about a window neither of them has.
    let (mut down, _) = state();
    let r = down.update(AppEvent::WindowFocus(false), now);
    assert!(!has(&r, &Cmd::DismissOverlay));

    // And gaining focus is nothing either way.
    let (mut up, _) = state();
    summon(&mut up, now);
    let r = up.update(AppEvent::WindowFocus(true), now);
    assert!(!has(&r, &Cmd::DismissOverlay));
}

/// Escape puts the panel away, and can be told not to.
#[test]
fn escape_can_be_told_to_clear_rather_than_close() {
    let now = Instant::now();
    let mut s = AppState::new(
        Settings {
            hide_on_escape: false,
            ..Settings::default()
        },
        now,
    );
    let (settled, _) = settle(&mut s, CODE, now);
    summon(&mut s, settled);

    let r = s.update(press(Key::Esc), settled);
    assert!(!has(&r, &Cmd::DismissOverlay), "Escape closed the panel");
    // Summoning selects the whole code, so the first Escape drops the
    // selection and the second clears the box.
    s.update(press(Key::Esc), settled);
    assert_eq!(s.input.text(), "", "the code was not cleared");
}

/// Losing the overlay having opened nothing would be bad enough; in compact
/// the explanatory message shares the hint row, so staying open is also the
/// only way the user gets to read it.
#[test]
fn enter_with_nothing_to_open_keeps_the_overlay_up() {
    let (mut s, now) = state();
    let (settled, _) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![], settled);
    summon(&mut s, settled);

    let r = s.update(press(Key::Enter), settled);

    assert!(!has(&r, &Cmd::DismissOverlay), "the overlay closed");
    assert!(s.overlay_up);
}

/// An overlay command leaking into the ordinary program would minimise the
/// window of somebody who only pressed Escape to clear the line.
#[test]
fn escape_outside_the_overlay_clears_the_line_and_touches_no_window() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);

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

/// The headline case, and the one a phase-only gate gets wrong.
///
/// The local matcher waits out `SEARCH_DEBOUNCE`, which is shorter than an
/// ordinary pause between syllables - so `inv` reaches `QueryPhase::Local`
/// while somebody is still reading the rest of the code off a drawing. Gating on "a
/// search resolved" would therefore remember every prefix typed on the way to a
/// code, which is the entire thing this feature was asked not to do.
#[test]
fn a_code_abandoned_mid_typing_is_not_remembered_when_the_overlay_closes() {
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

/// And the other half: once the typing has actually stopped, the code is kept.
///
/// Kept by the quiet period itself, with nobody pressing anything and without
/// the server having answered at all - the second of which matters because
/// `run_verify` skips the server entirely on the shipped two-share setup, and
/// the first because the commit used to ride on the verification coming back.
#[test]
fn a_code_that_settled_is_remembered_without_anybody_pressing_anything() {
    let (mut s, now) = state();
    summon(&mut s, now);
    let (settled, r) = settle(&mut s, CODE, now);
    assert!(
        s.verify_due_at().is_none(),
        "the debounce should have fired"
    );

    assert_eq!(s.history.entries(), [CODE]);
    assert_eq!(saved(&r).as_deref(), Some(&[CODE.to_string()][..]));

    // And closing over the top of it is a backstop that finds nothing to do.
    assert_eq!(saves(&s.update(press(Key::Esc), settled)), 0);
}

/// A code that found nothing is exactly the one worth recalling and
/// correcting. It used to be thrown away.
#[test]
fn a_code_that_found_nothing_is_remembered_too() {
    let (mut s, now) = state();
    type_in(&mut s, CODE, now);
    deliver_hits(&mut s, vec![], now);
    assert!(s.hits.is_empty(), "precondition");

    let (settled, r) = quiet(&mut s, now);

    assert_eq!(s.history.entries(), [CODE]);
    assert_eq!(saved(&r).as_deref(), Some(&[CODE.to_string()][..]));

    // Enter on it still opens nothing, and still writes nothing twice.
    let enter = s.update(press(Key::Enter), settled);
    assert!(!enter.cmds.iter().any(|c| matches!(c, Cmd::Open(_))));
    assert_eq!(saves(&enter), 0);
}

#[test]
fn enter_on_a_half_typed_code_with_nothing_to_open_remembers_nothing() {
    let (mut s, now) = state();
    type_in(&mut s, "inv", now);
    deliver_hits(&mut s, vec![], now);

    let r = s.update(press(Key::Enter), now);

    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// Four commit points, one code, one entry and one write. `History::record`
/// refuses an entry already at the head of the list, and `remember` turns that
/// into no command at all, which is what makes the four compose.
#[test]
fn a_settled_code_is_written_once_however_many_triggers_fire() {
    let (mut s, now) = state();
    // 1. the quiet period expiring, which is the ordinary way a code is kept.
    let (settled, quiet) = settle(&mut s, CODE, now);
    deliver_hits(&mut s, vec![hit("drawing.pdf")], settled);

    // 2. the verification answering, skipping the server exactly as it does
    //    with two shares configured. This used to be commit point one.
    let verify = s.update(
        AppEvent::Verify(VerifyMsg {
            epoch: s.query_epoch(),
            query: Query::contains(CODE),
            elapsed: std::time::Duration::ZERO,
            outcome: VerifyOutcome::Skipped(SkipReason::SeveralShares),
        }),
        settled,
    );
    // 3. opening from the overlay, which also dismisses it.
    summon(&mut s, settled);
    let enter = s.update(press(Key::Enter), settled);
    // 4. and the dismiss that went with it.
    let dismissed = s.update(AppEvent::Hotkey(HotkeyMsg::Dismissed), settled);

    let total = saves(&quiet) + saves(&verify) + saves(&enter) + saves(&dismissed);
    assert_eq!(total, 1, "the code was written {total} times");
    assert_eq!(s.history.entries(), [CODE]);
}

/// The bug this whole clock exists for, driven the way a person actually
/// types.
///
/// Every other helper in this repository hands the same `Instant` to every
/// character, which means no deadline can expire in the middle of a word - so
/// the bug was structurally unreachable from the tests while being reachable
/// by anyone who paused to read the next four digits off a drawing. The code
/// is typed here in three bursts with real gaps between them, one of which is
/// longer than the whole quiet period.
///
/// It used to leave `11`, `11-D` and `11-D-0704` behind. `REMEMBER_DEBOUNCE`
/// catches the short gaps and `History::record` collapses whatever survives
/// the long one.
#[test]
fn typing_one_code_with_pauses_leaves_exactly_one_entry() {
    let (mut s, mut now) = state();
    summon(&mut s, now);

    let mut written: Vec<Vec<String>> = Vec::new();
    // Two hundred milliseconds is an ordinary gap between syllables and used
    // to be enough; two seconds is somebody looking back at the drawing, and
    // is longer than the quiet period.
    for (burst, pause) in [
        ("11", std::time::Duration::from_millis(700)),
        ("-D", std::time::Duration::from_millis(2_000)),
        ("-0704", std::time::Duration::from_millis(2_000)),
    ] {
        for c in burst.chars() {
            let r = s.update(key(c), now);
            written.extend(saved(&r));
            now += std::time::Duration::from_millis(120);
        }
        // The frame loop, ticking its way through the pause. A fixed step
        // rather than `next_deadline`, because several of that function's
        // terms are anchored on the last frame drawn and this rig draws none.
        let until = now + pause;
        while now < until {
            now += std::time::Duration::from_millis(50);
            written.extend(saved(&s.update(AppEvent::Tick, now)));
        }
    }

    assert_eq!(
        s.history.entries(),
        [CODE],
        "the prefixes typed on the way to it were kept"
    );

    // Two seconds is long enough that `11-D` really had settled - somebody had
    // stopped typing a valid four-character code for longer than the quiet
    // period, and there is no way to tell that from being finished. So it was
    // written, and then superseded when the typing resumed.
    //
    // That is the right trade and the reason `History::record` collapses a
    // prefix at all: the deadline cannot see the future, so the list has to be
    // able to take the correction. What matters is that no write ever left
    // more than one entry behind, which is what the old behaviour did.
    assert!(
        written.iter().all(|entries| entries.len() == 1),
        "a write left a prefix behind: {written:?}"
    );
    assert_eq!(
        written.last().map(Vec::as_slice),
        Some(&[CODE.to_string()][..]),
        "the last write is the finished code"
    );
}

/// And the codes somebody really did look up are all still there.
#[test]
fn three_codes_looked_up_in_turn_are_three_entries() {
    let (mut s, mut now) = state();
    for code in ["11-D-0704", "P12345-001", "22-A-0001"] {
        let (settled, _) = settle(&mut s, code, now);
        now = settled + std::time::Duration::from_secs(1);
        // Clear the line, the way starting a new search does.
        s.update(ctrl(Key::Char('u')), now);
    }
    assert_eq!(
        s.history.entries(),
        ["22-A-0001", "P12345-001", "11-D-0704"]
    );
}

/// Looking at the list must not rewrite it. The commit inside `request_dismiss`
/// runs before the recall panel is stood down, which is the only reason this
/// can be detected at all.
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

#[test]
fn a_query_below_the_minimum_length_is_never_remembered() {
    let (mut s, now) = state();
    summon(&mut s, now);
    type_in(&mut s, "in", now);
    let later = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, later);

    let r = s.update(press(Key::Esc), later);

    assert_eq!(
        s.phase,
        QueryPhase::TooShort {
            need: files::config::MIN_QUERY_LEN
        }
    );
    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// Typed, but nothing anywhere looked for it, so there is no evidence the code
/// exists and nothing worth recalling.
#[test]
fn a_code_typed_where_no_share_is_configured_is_not_remembered() {
    let (mut s, now) = unconfigured_state();
    summon(&mut s, now);
    type_in(&mut s, CODE, now);
    let later = now + VERIFY_DEBOUNCE;
    s.update(AppEvent::Tick, later);

    let r = s.update(press(Key::Esc), later);

    assert_eq!(s.phase, QueryPhase::NoShares);
    assert!(s.history.is_empty());
    assert_eq!(saves(&r), 0);
}

/// A reduced feature, not a broken one: the shortcut still switches to the
/// compact view where the window cannot be found, and says so once.
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

/// Guards against the key-event kind filter being bypassed: Windows sends a
/// release for every press, and acting on both would toggle twice.
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

/// The settings are a window of their own, so `Ctrl+,` asks for one rather
/// than borrowing the panel's body.
///
/// The shortcuts window used to work this way too, and before that it was a
/// panel that had to close itself before the overlay would - a panel that
/// could not be shut without taking the search box with it would have been a
/// trap. It is gone entirely now; a real window has a title bar, so none of
/// that has to be arranged for the one that is left.
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

    // And Escape still means what it means everywhere else in the overlay,
    // because there is no longer a layer for it to peel first.
    let dismissed = s.update(press(Key::Esc), now);
    assert!(
        has(&dismissed, &Cmd::DismissOverlay),
        "{:?}",
        dismissed.cmds
    );
}
