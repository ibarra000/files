//! What the panel does while somebody types, frame by frame.
//!
//! The loop nothing else in this repository closes. `tests/shell.rs` checks
//! [`files::gui::anim::Motion`] against targets handed to it by hand, and
//! `tests/state_machine.rs` checks the state those targets are derived from -
//! but the bugs this file exists for live in between, where the two meet:
//! state changes on a keystroke, the view measures the change, the animator is
//! told, and the window system is asked to do something about it.
//!
//! Every assertion here is a *budget*. Jitter is not a behaviour that is
//! either present or absent, it is a count - of body changes, of searches
//! dispatched - and a count is something a test can hold a number against and
//! a future change can be measured by. `--nocapture` prints them.
//!
//! Window resizes used to be the headline count here, because the panel was
//! the window and every row-count change was a `SetWindowPos` and a swapchain
//! reconfigure. The window is a fixed six hundred by four hundred now and
//! resizes exactly never, so that budget is gone along with the thing it was
//! counting - and the body-change and search budgets, which were always the
//! ones about what somebody actually sees, are what is left.
//!
//! Nothing here opens a window, touches a GPU or reads a clock. The clock is a
//! variable and the search worker is a dozen lines in the rig.

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

/// Sixty frames a second, which is what the compositor gives us.
const FRAME: Duration = Duration::from_nanos(16_666_667);

/// A brisk but ordinary typing speed: about eight characters a second, which
/// is what somebody reading a code off a drawing manages.
const KEYSTROKE_GAP: Duration = Duration::from_millis(120);

/// The code every test here types. Nine characters, the shape the office uses.
const CODE: &str = "11-D-0704";

/// Long enough for a typed code to be searched for and answered.
///
/// `SEARCH_DEBOUNCE` plus slack. Typing at `KEYSTROKE_GAP` never reaches it -
/// 120ms between characters against a 180ms pause - which is the whole point:
/// a burst dispatches nothing until it stops.
///
/// The margin is 60ms where it used to be 180ms, which is exactly what the
/// shorter debounce bought and exactly what it costs. The budget below still
/// holds at a hundred and twenty; it would not at two hundred, and a reader
/// who pauses that long between the groups of a code will now spend one
/// extra sweep over an index this process owns.
const SEARCH_SETTLE: Duration = Duration::from_millis(400);

// --- the rig ---------------------------------------------------------------

/// Everything the frames did, in the order they did it.
#[derive(Default)]
struct Log {
    bodies: Vec<Content>,
    /// Which row was selected on each of those frames.
    ///
    /// Read off the state rather than off the frame. It used to come back
    /// with the body, because the animator slid the highlight and therefore
    /// had to be told where it was going; a row paints its own now, so the
    /// only place the answer lives is the state machine.
    selected: Vec<Option<usize>>,
}

impl Log {
    /// How many times the body changed to something else.
    fn body_changes(&self) -> usize {
        self.bodies.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// Every body the panel showed, in order, with repeats collapsed.
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
    /// Dispatched but not yet answered. The matcher is sub-millisecond but it
    /// runs on another thread, so the earliest it can land is the next frame -
    /// which is the whole reason the panel ever sees a list it is about to
    /// replace.
    inflight: Vec<(u64, Query)>,
    /// What the index holds.
    corpus: Vec<&'static str>,
    /// How many searches have actually been dispatched.
    ///
    /// The headline budget. Jitter is a count, and this is the count the
    /// debounce exists to bring down: nine characters used to be nine sweeps
    /// over the index for eight answers nobody read.
    searches: usize,
    log: Log,
}

impl Rig {
    fn new(corpus: Vec<&'static str>) -> Self {
        let now = Instant::now();
        Self {
            state: AppState::new(Settings::default(), now),
            frame: Frame::new(),
            now,
            inflight: Vec::new(),
            corpus,
            searches: 0,
            log: Log::default(),
        }
    }

    /// A panel already up and settled, so a test about typing is not also a
    /// test about the entrance.
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

    /// Forgets everything recorded so far, so a budget measures the burst
    /// rather than the priming that set it up. Clears the search count with
    /// the log, because the two are read together and forgetting one silently
    /// inflates a budget.
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

    /// Answers whatever the matcher was asked, the way the real one would:
    /// on a later frame than the one that asked.
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

    /// One turn of the loop, in the order `Shell::logic` then `Shell::ui` take
    /// it: pending work lands, the clock's deadlines fire, the frame is
    /// composed, and the window system is told if it needs to be.
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

    /// Types `text` at a human cadence, running the frames in between.
    ///
    /// The cadence is the point. Every existing helper in this repository
    /// passes the *same* `Instant` for every character, which means no
    /// debounce can fire in the middle of a word - and a debounce firing in
    /// the middle of a word is precisely the class of bug this file is for.
    fn type_code(&mut self, text: &str, gap: Duration) {
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
        "11-D-0704.pdf",
        "11-D-0704-A.pdf",
        "11-D-0704-B.pdf",
        "11-D-0705.pdf",
        "11-D-0706.pdf",
        "11-D-0801.pdf",
        "11-E-0704.pdf",
        "12-D-0704.pdf",
        "cover-11-D.pdf",
        "index-11.pdf",
    ]
}

// --- the budgets -----------------------------------------------------------

/// The one that mattered. Every character typed used to empty the result list
/// for exactly one frame - the frame between the matcher being asked and
/// answering - and an empty list is not a shorter list, it is a different
/// body three hundred points shorter.
#[test]
fn typing_a_code_never_empties_the_result_list() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    assert!(
        !rig.state.hits.is_empty(),
        "the fixture found nothing to work with"
    );

    rig.forget();
    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.log.report("typing_a_code");

    assert!(
        !rig.log.bodies.contains(&Content::Empty),
        "the panel showed its empty state while a result set was in flight; bodies were {:?}",
        rig.log.bodies()
    );
}

/// The body is allowed to change when it has something else to show. It is not
/// allowed to change on the way to showing the same thing again.
#[test]
fn typing_a_code_does_not_change_the_body_at_all() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();
    rig.type_code("D-0704", KEYSTROKE_GAP);

    assert_eq!(
        rig.log.body_changes(),
        0,
        "the body changed over six keystrokes: {:?}",
        rig.log.bodies()
    );
}

/// Six keystrokes are one sweep over the index.
///
/// This used to count window resizes as well, and that was the headline: the
/// panel was the window, so every row-count change was a `SetWindowPos` and a
/// swapchain reconfigure. It went thirty-one, then six, then one. The window
/// is fixed now and the count is zero by construction, so what is left to
/// budget is the thing that still costs something - a sweep over an index on
/// a file server.
#[test]
fn typing_a_code_costs_the_index_exactly_one_sweep() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();

    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));
    rig.log.report("index_cost");

    // The six characters are typed 120ms apart, inside the debounce, so the
    // matcher is asked once - after the typing stops.
    assert_eq!(rig.searches, 1, "six keystrokes, one sweep over the index");
    assert_eq!(rig.log.body_changes(), 0, "and the body never changed");
}

/// A selection that blinks out and back is the same defect as a list that
/// does, one row tall.
#[test]
fn the_selection_stays_on_screen_while_typing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();
    rig.type_code("D-07", KEYSTROKE_GAP);

    assert!(
        rig.log.selected.iter().all(Option::is_some),
        "the selection left the list while typing: {:?}",
        rig.log.selected
    );
}

/// A result set that arrives saying exactly what the last one said should move
/// nothing at all. This is the guard on a snapshot republishing under a live
/// panel, which re-runs the search with the same epoch.
#[test]
fn an_unchanged_result_set_moves_nothing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code(CODE, KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));

    let before = rig.last();
    let selected = rig.state.selected_row();
    rig.forget();

    // The same answer, again, as a republished snapshot would deliver it.
    let epoch = rig.state.query_epoch();
    rig.inflight.push((epoch, Query::contains(CODE)));
    rig.run(Duration::from_millis(400));

    assert_eq!(rig.log.body_changes(), 0, "it changed the body");
    for body in &rig.log.bodies {
        assert_eq!(*body, before, "the body changed under it");
    }
    for row in &rig.log.selected {
        assert_eq!(*row, selected, "the selection moved");
    }
}

/// A code that really does match nothing still says so - the hold is a delay,
/// not a refusal.
#[test]
fn a_code_that_matches_nothing_still_reaches_the_empty_state() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("99-Z-9999", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));

    assert!(rig.state.hits.is_empty());
    assert_eq!(
        rig.last(),
        Content::Empty,
        "bodies were {:?}",
        rig.log.bodies()
    );
}

/// The budget this whole change exists for: a nine-character code is one
/// sweep over the index, not nine.
#[test]
fn a_burst_of_typing_dispatches_exactly_one_search() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code(CODE, KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.log.report("one_search");

    assert_eq!(
        rig.searches, 1,
        "nine characters cost {} sweeps over the index",
        rig.searches
    );
}

/// And nothing reaches the window system at all until the typing stops.
#[test]
fn a_burst_of_typing_costs_the_window_system_nothing_until_it_stops() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(SEARCH_SETTLE);
    rig.forget();

    // Six characters, 720ms of typing, and deliberately no settle: this is
    // what the panel does *while* somebody is still going.
    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.log.report("during_burst");

    assert_eq!(
        rig.searches, 0,
        "the matcher was asked while the user was still typing"
    );
    assert_eq!(rig.log.body_changes(), 0, "and the body changed under them");
}
