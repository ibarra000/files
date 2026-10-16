//! Frame-by-frame panel behaviour while typing: state, animator and a stub
//! search worker driven together on a simulated clock.
//!
//! Each assertion is a budget on a count (body changes, searches dispatched);
//! `--nocapture` prints the counts. No window, GPU or real clock is used.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use files::app::event::{AppEvent, Cmd, HotkeyMsg, SearchMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::config::Settings;
use files::gui::anim::{Content, Phase};
use files::gui::frame::Frame;
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;

/// One frame at 60 Hz.
const FRAME: Duration = Duration::from_nanos(16_666_667);

/// Typing speed of about eight characters a second.
///
/// Must stay below `SEARCH_DEBOUNCE` (180ms) so a burst dispatches nothing
/// until it stops.
const KEYSTROKE_GAP: Duration = Duration::from_millis(120);

/// The search term every test types.
const TERM: &str = "AB-C-0704";

/// Long enough for a typed term to be searched for and answered:
/// `SEARCH_DEBOUNCE` plus slack.
const SEARCH_SETTLE: Duration = Duration::from_millis(400);

// --- the rig ---------------------------------------------------------------

/// Everything the frames did, in the order they did it.
#[derive(Default)]
struct Log {
    bodies: Vec<Content>,
    /// The selected row on each frame, read from the state.
    selected: Vec<Option<usize>>,
}

impl Log {
    /// Counts frames whose body differs from the previous one.
    fn body_changes(&self) -> usize {
        self.bodies.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// Returns every body shown, in order, with repeats collapsed.
    fn bodies(&self) -> Vec<Content> {
        let mut out: Vec<Content> = Vec::new();
        for body in &self.bodies {
            if out.last() != Some(body) {
                out.push(*body);
            }
        }
        out
    }

    fn report(&self, what: &str) {
        println!(
            "{what}: {} frames, {} body changes, bodies {:?}",
            self.bodies.len(),
            self.body_changes(),
            self.bodies(),
        );
    }
}

/// A panel, its animator, a clock, and a stand-in for the search worker.
struct Rig {
    state: AppState,
    frame: Frame,
    now: Instant,
    /// Searches dispatched but not yet answered. The real matcher runs on
    /// another thread, so answers land on the next frame at the earliest.
    inflight: Vec<(u64, Query)>,
    /// The file names the index holds.
    corpus: Vec<&'static str>,
    /// Number of searches dispatched; the debounce keeps this low.
    searches: usize,
    log: Log,
}

impl Rig {
    fn new(corpus: Vec<&'static str>) -> Self {
        let now = Instant::now();
        Self {
            state: AppState::new(Settings::fixture(), now),
            frame: Frame::new(),
            now,
            inflight: Vec::new(),
            corpus,
            searches: 0,
            log: Log::default(),
        }
    }

    /// Returns a rig whose panel is already shown and settled.
    fn summoned(corpus: Vec<&'static str>) -> Self {
        let mut rig = Self::new(corpus);
        rig.feed(AppEvent::Hotkey(HotkeyMsg::Summoned));
        rig.frame.motion.summon();
        rig.run(Duration::from_millis(400));
        assert_eq!(
            rig.frame.motion.phase(),
            Phase::Shown,
            "the entrance never finished"
        );
        rig.forget();
        rig.searches = 0;
        rig
    }

    /// Clears the log and the search count so a budget measures only what
    /// follows. Both are cleared together because they are read together.
    fn forget(&mut self) {
        self.log = Log::default();
        self.searches = 0;
    }

    fn feed(&mut self, event: AppEvent) {
        let response = self.state.update(event, self.now);
        for cmd in &response.cmds {
            if let Cmd::Search { query, epoch } = cmd {
                self.searches += 1;
                self.inflight.push((*epoch, query.clone()));
            }
        }
    }

    /// Answers a dispatched search with a substring match over the corpus.
    fn answer(&mut self, epoch: u64, query: Query) {
        let needle = query.term().to_ascii_lowercase();
        let hits: Vec<Hit> = self
            .corpus
            .iter()
            .filter(|name| name.to_ascii_lowercase().contains(&needle))
            .map(|name| Hit {
                path: Arc::from(format!("V:\\{name}").as_str()),
                name: Arc::from(*name),
                match_pos: 0,
                index: 0,
            })
            .collect();
        let matched = hits.len() as u32;
        let total = self.corpus.len() as u32;
        self.feed(AppEvent::Search(SearchMsg {
            epoch,
            query,
            elapsed: Duration::ZERO,
            result: Ok(SearchOutcome {
                hits,
                matched,
                total,
                cancelled: false,
                unicode_fallback: false,
            }),
        }));
    }

    /// Runs one frame in the order `Shell::logic` then `Shell::ui` use:
    /// pending results land, due deadlines fire, then the frame is composed.
    fn tick(&mut self) {
        self.now += FRAME;

        for (epoch, query) in std::mem::take(&mut self.inflight) {
            self.answer(epoch, query);
        }

        if self
            .state
            .next_deadline()
            .is_some_and(|due| self.now >= due)
        {
            self.feed(AppEvent::Tick);
        }

        self.state.note_frame(self.now, SystemTime::now());
        self.log
            .bodies
            .push(self.frame.advance(&self.state, FRAME.as_secs_f32()));
        self.log.selected.push(self.state.selected_row());
    }

    fn run(&mut self, span: Duration) {
        let until = self.now + span;
        while self.now < until {
            self.tick();
        }
    }

    /// Types `text` with `gap` between characters, running the frames in
    /// between so a debounce can fire mid-word.
    fn type_search(&mut self, text: &str, gap: Duration) {
        for c in text.chars() {
            self.feed(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)));
            self.run(gap);
        }
    }

    fn last(&self) -> Content {
        *self.log.bodies.last().expect("no frames were drawn")
    }
}

fn corpus() -> Vec<&'static str> {
    vec![
        "AB-C-0704.pdf",
        "AB-C-0704-A.pdf",
        "AB-C-0704-B.pdf",
        "AB-C-0705.pdf",
        "AB-C-0706.pdf",
        "AB-C-0801.pdf",
        "AB-D-0704.pdf",
        "AD-C-0704.pdf",
        "cover-AB-C.pdf",
        "index-AB.pdf",
    ]
}

// --- the budgets -----------------------------------------------------------

/// The list does not show its empty state while a refined search is in flight.
#[test]
fn typing_a_search_never_empties_the_result_list() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("AB-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    assert!(
        !rig.state.hits.is_empty(),
        "the fixture found nothing to work with"
    );

    rig.forget();
    rig.type_search("C-0704", KEYSTROKE_GAP);
    rig.log.report("typing_a_search");

    assert!(
        !rig.log.bodies.contains(&Content::Empty),
        "the panel showed its empty state while a result set was in flight; bodies were {:?}",
        rig.log.bodies()
    );
}

/// The body does not change on the way to showing the same content again.
#[test]
fn typing_a_search_does_not_change_the_body_at_all() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("AB-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();
    rig.type_search("C-0704", KEYSTROKE_GAP);

    assert_eq!(
        rig.log.body_changes(),
        0,
        "the body changed over six keystrokes: {:?}",
        rig.log.bodies()
    );
}

/// Six keystrokes cost one sweep over the index.
#[test]
fn typing_a_search_costs_the_index_exactly_one_sweep() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("AB-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();

    rig.type_search("C-0704", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));
    rig.log.report("index_cost");

    // Keystrokes 120ms apart fall inside the debounce, so the matcher is asked
    // once, after typing stops.
    assert_eq!(rig.searches, 1, "six keystrokes, one sweep over the index");
    assert_eq!(rig.log.body_changes(), 0, "and the body never changed");
}

/// The selection never drops to `None` while typing.
#[test]
fn the_selection_stays_on_screen_while_typing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("AB-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();
    rig.type_search("C-07", KEYSTROKE_GAP);

    assert!(
        rig.log.selected.iter().all(Option::is_some),
        "the selection left the list while typing: {:?}",
        rig.log.selected
    );
}

/// A result set identical to the last one changes neither body nor selection,
/// as when a republished snapshot re-runs the search with the same epoch.
#[test]
fn an_unchanged_result_set_moves_nothing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search(TERM, KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));

    let before = rig.last();
    let selected = rig.state.selected_row();
    rig.forget();

    // Re-deliver the same answer.
    let epoch = rig.state.query_epoch();
    rig.inflight.push((epoch, Query::contains(TERM)));
    rig.run(Duration::from_millis(400));

    assert_eq!(rig.log.body_changes(), 0, "it changed the body");
    for body in &rig.log.bodies {
        assert_eq!(*body, before, "the body changed under it");
    }
    for row in &rig.log.selected {
        assert_eq!(*row, selected, "the selection moved");
    }
}

/// A search with no matches still reaches the empty state; the hold only delays it.
#[test]
fn a_search_that_matches_nothing_still_reaches_the_empty_state() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("ZZ-Z-9999", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));

    assert!(rig.state.hits.is_empty());
    assert_eq!(
        rig.last(),
        Content::Empty,
        "bodies were {:?}",
        rig.log.bodies()
    );
}

/// A nine-character search costs one sweep over the index.
#[test]
fn a_burst_of_typing_dispatches_exactly_one_search() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search(TERM, KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.log.report("one_search");

    assert_eq!(
        rig.searches, 1,
        "nine characters cost {} sweeps over the index",
        rig.searches
    );
}

/// No search is dispatched and the body does not change until typing stops.
#[test]
fn a_burst_of_typing_costs_the_window_system_nothing_until_it_stops() {
    let mut rig = Rig::summoned(corpus());
    rig.type_search("AB-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();

    // No settle afterwards: this measures the panel during the burst.
    rig.type_search("C-0704", KEYSTROKE_GAP);
    rig.log.report("during_burst");

    assert_eq!(
        rig.searches, 0,
        "the matcher was asked while the user was still typing"
    );
    assert_eq!(rig.log.body_changes(), 0, "and the body changed under them");
}
