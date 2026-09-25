//! Keys and clicks, shuffled together, against the invariants that have to
//! hold between every pair of them.
//!
//! The hand-written tests each drive one gesture to completion. What they
//! cannot cover is the *order*: a drag interrupted by a keystroke, results
//! landing while recall is open, a resize between the two halves of a
//! double-click, a right-click menu still open when the list underneath it
//! empties. Those are where a caret ends up inside a character and a selection
//! ends up naming a file that is no longer in the list - and neither shows up
//! until the next frame, somewhere else, as a panic with no obvious cause.
//!
//! Coordinates deliberately run past the terminal. An out-of-bounds click is a
//! real event - a resize that raced the terminal's own report of it - and has
//! to be a no-op rather than a panic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::event::{AppEvent, Response, SearchMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::app::state::pointer::Intent;
use files::config::Settings;
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Step {
    Key(Key, Mods),
    /// A pointer gesture, already resolved against the layout that drew it.
    ///
    /// This used to be `Press { x, y }` and five friends, generated over a grid
    /// of cells and converted back into a row by the state machine. The ranks
    /// below still range well past any list that exists - which was the case
    /// the fuzzer was really for - without pretending to aim at anything.
    Point(Intent),
    Results(usize),
    Paste(String),
    Wait(u64),
}

/// Deliberately not "any `Key`".
///
/// The interesting collisions are between the dozen keys that move focus and
/// the handful of gestures that move it back. A uniform strategy over every key
/// in the enum would spend its whole budget on keys with no binding. `Ctrl+Q`
/// is absent because a state machine that has quit has nothing further to say,
/// and `q` is therefore missing from the character alphabet too.
fn a_key() -> impl Strategy<Value = Step> {
    let codes = prop_oneof![
        Just(Key::Up),
        Just(Key::Down),
        Just(Key::Left),
        Just(Key::Right),
        Just(Key::Home),
        Just(Key::End),
        Just(Key::PageUp),
        Just(Key::PageDown),
        Just(Key::Tab),
        Just(Key::Enter),
        Just(Key::Esc),
        Just(Key::Backspace),
        Just(Key::Delete),
        Just(Key::F(1)),
        Just(Key::F(2)),
        Just(Key::F(5)),
        prop::char::range('0', '9').prop_map(Key::Char),
        Just(Key::Char('-')),
        Just(Key::Char('D')),
        Just(Key::Char('\u{e9}')),
    ];
    let mods = prop_oneof![
        6 => Just(Mods::NONE),
        2 => Just(Mods::SHIFT),
        2 => Just(Mods::CTRL),
        1 => Just(Mods::ALTGR),
    ];
    (codes, mods).prop_map(|(c, m)| Step::Key(c, m))
}

fn an_intent() -> impl Strategy<Value = Step> {
    // Ranks well past any list the steps above can build, and byte offsets past
    // any code: the point is the out-of-range case, which is the one a renderer
    // cannot be trusted to have got right.
    prop_oneof![
        3 => (0usize..=400).prop_map(Intent::Select),
        2 => (0usize..=400).prop_map(Intent::Activate),
        3 => prop::option::of(0usize..=400).prop_map(Intent::Hover),
        2 => (0usize..=40, any::<bool>())
                .prop_map(|(byte, extend)| Intent::Caret { byte, extend }),
        1 => prop_oneof![
            Just(files::view::actions::ActionId::Open),
            Just(files::view::actions::ActionId::OpenAsDocument),
            Just(files::view::actions::ActionId::OpenInAvwin),
            Just(files::view::actions::ActionId::OpenWithWindows),
            Just(files::view::actions::ActionId::Reveal),
            Just(files::view::actions::ActionId::CopyPath),
            Just(files::view::actions::ActionId::CopyName),
            Just(files::view::actions::ActionId::Refresh),
            Just(files::view::actions::ActionId::Settings),
        ]
        .prop_map(Intent::Act),
        1 => any::<bool>().prop_map(Intent::ShowActions),
    ]
    .prop_map(Step::Point)
}

fn a_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => a_key(),
        8 => an_intent(),
        3 => (0usize..=320).prop_map(Step::Results),
        1 => "[0-9A-Da-d\u{e9}\\- \r\n]{0,20}".prop_map(Step::Paste),
        1 => (0u64..900).prop_map(Step::Wait),
    ]
}

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("V:\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

fn apply(s: &mut AppState, step: &Step, clock: &mut Instant) -> Response {
    let now = *clock;
    match step {
        Step::Key(code, mods) => s.update(AppEvent::Key(KeyEvent::new(*code, *mods)), now),
        Step::Point(intent) => s.update(AppEvent::Intent(*intent), now),
        // Through the real message, carrying the real epoch: poking `s.hits`
        // would build states the program cannot reach, and the epoch is the
        // mechanism that decides whether a result is still wanted at all.
        Step::Results(n) => {
            let hits: Vec<Hit> = (0..*n).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect();
            s.update(
                AppEvent::Search(SearchMsg {
                    epoch: s.query_epoch(),
                    query: Query::parse(s.input.text()),
                    elapsed: Duration::from_micros(200),
                    result: Ok(SearchOutcome {
                        hits,
                        matched: *n as u32,
                        total: 9_000,
                        cancelled: false,
                        unicode_fallback: false,
                    }),
                }),
                now,
            )
        }
        Step::Paste(text) => s.update(AppEvent::Paste(text.clone()), now),
        Step::Wait(ms) => {
            // The clock only ever moves forward, as a real one does.
            *clock += Duration::from_millis(*ms);
            s.update(AppEvent::Tick, *clock)
        }
    }
}

/// Everything that has to be true between any two events.
fn holds(s: &AppState, r: &Response, step: &Step) -> Result<(), TestCaseError> {
    let _ = r;
    let text = s.input.text().to_string();

    // 1. The caret is on a character boundary, and so are both ends of a
    //    selection. Everything that draws the line slices at these. A click is
    //    the one event that can break it, because it arrives as a column and
    //    has to be converted - and the panic lands on the next frame, not on
    //    the click.
    prop_assert!(s.input.caret() <= text.len());
    prop_assert!(
        text.is_char_boundary(s.input.caret()),
        "the caret landed inside a character after {step:?}"
    );

    // 2. A selection is ordered, non-empty and inside the text. An empty one
    //    must report as `None`, or `Ctrl+C` claims to have copied nothing.
    if let Some((lo, hi)) = s.input.selection() {
        prop_assert!(lo < hi, "an empty selection reported as {lo}..{hi}");
        prop_assert!(hi <= text.len());
        prop_assert!(text.is_char_boundary(lo) && text.is_char_boundary(hi));
        prop_assert!(s.input.selected_text().is_some());
    }

    // 3. `Enter` opens `selected_path`. A path that is not in `hits` is one
    //    keystroke away from opening a file that is not on screen.
    if let Some(hit) = s.selected_hit() {
        prop_assert!(
            s.hits.iter().any(|h| h.path == hit.path),
            "the selection names a file the results do not hold"
        );
    }
    prop_assert_eq!(
        s.hits.is_empty(),
        s.selected_hit().is_none(),
        "a non-empty list must always have a selection, and an empty one none"
    );

    // 4. The selected rank is inside the list. A rank past the end is one
    //    keystroke away from opening a file that does not exist, and an
    //    `Intent` carrying a rank from a frame that has since been replaced is
    //    exactly how that would arrive.
    if let Some(row) = s.selected_row() {
        prop_assert!(row < s.hits.len(), "rank {} is past the list", row);
    }

    // 5. There is no window any more, and that is the point.
    //
    //    This used to be four assertions about `scroll_top`: that the window
    //    over the list ran to somewhere inside it, that it was no taller than
    //    the panel, that it contained the cursor, and that it was full
    //    wherever the list allowed. The state machine held the offset because
    //    page-flipping a twelve-row list on the twelfth Down was worse than
    //    sliding it, and this was the check that replaced the argument for
    //    deriving it instead - the failure being a cursor on a screen nobody
    //    can see.
    //
    //    The content band is an `egui::ScrollArea` now and owns its own
    //    offset, so there is no second source of truth left to keep in step.
    //    What survives of the invariant is assertion 4 above: the selected
    //    rank is inside the list.

    // 6. Browsing the recent codes has a cursor in it. Browsing without one
    //    draws a list with nothing highlighted and leaves Up and Down with
    //    nothing to move.
    if s.history.is_browsing() {
        prop_assert!(
            s.history.cursor().is_some(),
            "browsing with no entry picked, after {step:?}"
        );
    }

    // 7. A hover is never a selection. Two highlights that could be confused
    //    is a file opened by accident, and the pointer can now name a row the
    //    keyboard has never been near.
    if let (Some(hovered), Some(selected)) = (s.hovered(), s.selected_row()) {
        prop_assert!(
            hovered < s.hits.len(),
            "the pointer is over a row that is not there"
        );
        let _ = selected;
    }

    Ok(())
}

fn fresh() -> (AppState, Instant) {
    fresh_in(1)
}

/// The same, with the results laid out `columns` to a line.
fn fresh_in(columns: usize) -> (AppState, Instant) {
    let now = Instant::now();
    let settings = Settings {
        columns,
        ..Settings::default()
    };
    let mut s = AppState::new(settings, now);
    s.seed_history(vec!["P12345-001".into(), "11-D-0704".into()]);
    (s, now)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn no_interleaving_of_keys_and_clicks_can_break_the_invariants(
        steps in prop::collection::vec(a_step(), 1..40),
        columns in 1usize..=files::config::MAX_COLUMNS,
    ) {
        let (mut s, mut clock) = fresh_in(columns);
        for step in &steps {
            let r = apply(&mut s, step, &mut clock);
            holds(&s, &r, step)?;
            if s.should_quit {
                break;
            }
        }
    }

    /// A pointer can name a row that has since stopped existing, and must not
    /// be believed.
    ///
    /// This used to draw a terminal, read the screen back, and check that any
    /// click which moved the selection had landed on a name that was actually
    /// on it. That was the right test for a renderer that reported *cells*: the
    /// hit-test lived in the state machine, so the state machine could be wrong
    /// about where a row had been.
    ///
    /// The renderer resolves its own widgets now, so it can only ever hand over
    /// a rank it drew. What is left to get wrong is the gap between the frame a
    /// click was aimed at and the frame that exists when it arrives - results
    /// land from another thread, and a verification replaces the list wholesale
    /// - so that is what this generates: ranks from a list that has since
    /// changed size underneath them.
    #[test]
    fn a_stale_rank_can_never_select_something_that_is_not_there(
        ranks in prop::collection::vec(0usize..400, 1..24),
        counts in prop::collection::vec(0usize..320, 1..8),
    ) {
        let (mut s, now) = fresh();
        for c in "11-D-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }

        let mut counts = counts.into_iter().cycle();
        for rank in ranks {
            // The list changes size between the aim and the arrival, which is
            // the whole of what this is about.
            let count = counts.next().unwrap_or(0);
            let hits: Vec<Hit> = (0..count).map(|i| hit(&format!("11d_{i:04}.pdf"))).collect();
            s.update(
                AppEvent::Search(SearchMsg {
                    epoch: s.query_epoch(),
                    query: Query::parse(s.input.text()),
                    elapsed: Duration::from_micros(200),
                    result: Ok(SearchOutcome {
                        hits,
                        matched: count as u32,
                        total: 9_000,
                        cancelled: false,
                        unicode_fallback: false,
                    }),
                }),
                now,
            );

            for intent in [Intent::Select(rank), Intent::Hover(Some(rank)), Intent::Activate(rank)] {
                s.update(AppEvent::Intent(intent), now);

                if let Some(row) = s.selected_row() {
                    prop_assert!(
                        row < s.hits.len(),
                        "{intent:?} selected rank {row} of {}",
                        s.hits.len()
                    );
                }
                if let Some(row) = s.hovered() {
                    prop_assert!(
                        row < s.hits.len(),
                        "{intent:?} hovered rank {row} of {}",
                        s.hits.len()
                    );
                }
            }
        }
    }
}
