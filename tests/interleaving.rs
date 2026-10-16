//! Property tests that interleave keys, pointer intents, results and pastes
//! and check the state invariants after every event.
//!
//! Ranks and byte offsets deliberately exceed any real list or text: an
//! out-of-range intent must be a no-op, not a panic.

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
    Point(Intent),
    Results(usize),
    Paste(String),
    Wait(u64),
}

/// Generates bound keys only, so the case budget is not spent on keys that do
/// nothing. `q` is excluded so `Ctrl+Q` cannot end the run early.
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
    // Ranks and byte offsets range past any list or text to exercise the
    // out-of-range case.
    prop_oneof![
        3 => (0usize..=400).prop_map(Intent::Select),
        2 => (0usize..=400).prop_map(Intent::Activate),
        3 => prop::option::of(0usize..=400).prop_map(Intent::Hover),
        2 => (0usize..=40, any::<bool>())
                .prop_map(|(byte, extend)| Intent::Caret { byte, extend }),
        1 => prop_oneof![
            Just(files::view::actions::ActionId::Open),
            Just(files::view::actions::ActionId::OpenAsDocument),
            Just(files::view::actions::ActionId::OpenWith(0)),
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
        // Sent as a real message with the current epoch; setting `s.hits`
        // directly could build unreachable states.
        Step::Results(n) => {
            let hits: Vec<Hit> = (0..*n).map(|i| hit(&format!("abc_{i:04}.pdf"))).collect();
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
            // The clock is monotonic.
            *clock += Duration::from_millis(*ms);
            s.update(AppEvent::Tick, *clock)
        }
    }
}

/// Everything that has to be true between any two events.
fn holds(s: &AppState, r: &Response, step: &Step) -> Result<(), TestCaseError> {
    let _ = r;
    let text = s.input.text().to_string();

    // 1. The caret is on a character boundary; rendering slices the text there.
    prop_assert!(s.input.caret() <= text.len());
    prop_assert!(
        text.is_char_boundary(s.input.caret()),
        "the caret landed inside a character after {step:?}"
    );

    // 2. A selection is ordered, non-empty, on character boundaries and inside
    //    the text. An empty selection reports as `None`.
    if let Some((lo, hi)) = s.input.selection() {
        prop_assert!(lo < hi, "an empty selection reported as {lo}..{hi}");
        prop_assert!(hi <= text.len());
        prop_assert!(text.is_char_boundary(lo) && text.is_char_boundary(hi));
        prop_assert!(s.input.selected_text().is_some());
    }

    // 3. The selected hit is in `hits`, and a non-empty list always has one.
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

    // 4. The selected rank is inside the list, even after an `Intent` carrying
    //    a rank from a replaced frame.
    if let Some(row) = s.selected_row() {
        prop_assert!(row < s.hits.len(), "rank {} is past the list", row);
    }

    // 5. Browsing recent searches always has a cursor.
    if s.history.is_browsing() {
        prop_assert!(
            s.history.cursor().is_some(),
            "browsing with no entry picked, after {step:?}"
        );
    }

    // 6. A hovered row is inside the list.
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

/// Like `fresh`, with results laid out `columns` per line.
fn fresh_in(columns: usize) -> (AppState, Instant) {
    let now = Instant::now();
    let settings = Settings {
        columns,
        viewers: std::sync::Arc::new(files::config::Viewers::new(vec![
            files::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
        ])),
        ..Settings::fixture()
    };
    let mut s = AppState::new(settings, now);
    s.seed_history(vec!["P12345-001".into(), "AB-C-0704".into()]);
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

    /// Pointer intents carrying ranks from a list that has since changed size
    /// never select or hover a row past the end.
    #[test]
    fn a_stale_rank_can_never_select_something_that_is_not_there(
        ranks in prop::collection::vec(0usize..400, 1..24),
        counts in prop::collection::vec(0usize..320, 1..8),
    ) {
        let (mut s, now) = fresh();
        for c in "AB-C-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }

        let mut counts = counts.into_iter().cycle();
        for rank in ranks {
            // Resize the list between the frame a click aimed at and its arrival.
            let count = counts.next().unwrap_or(0);
            let hits: Vec<Hit> = (0..count).map(|i| hit(&format!("abc_{i:04}.pdf"))).collect();
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
