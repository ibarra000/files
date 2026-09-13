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
//! either present or absent, it is a count - of body changes, of cross-fades,
//! of window resizes - and a count is something a test can hold a number
//! against and a future change can be measured by. `--nocapture` prints them.
//!
//! Nothing here opens a window, touches a GPU or reads a clock. The clock is a
//! variable and the search worker is a dozen lines in the rig.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use files::app::event::{AppEvent, Cmd, HotkeyMsg, SearchMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::config::Settings;
use files::gui::anim::{Content, Phase, Visual};
use files::gui::frame::Frame;
use files::search::matcher::{Hit, SearchOutcome};

/// Sixty frames a second, which is what the compositor gives us.
const FRAME: Duration = Duration::from_nanos(16_666_667);

/// A brisk but ordinary typing speed: about eight characters a second, which
/// is what somebody reading a code off a drawing manages.
const KEYSTROKE_GAP: Duration = Duration::from_millis(120);

/// The code every test here types. Nine characters, the shape the office uses.
const CODE: &str = "11-D-0704";

// --- the rig ---------------------------------------------------------------

/// Everything the frames did, in the order they did it.
#[derive(Default)]
struct Log {
    visuals: Vec<Visual>,
    resizes: Vec<(f32, f32)>,
}

impl Log {
    /// How many times the body changed to something else.
    fn body_changes(&self) -> usize {
        self.visuals
            .windows(2)
            .filter(|w| w[0].content.showing != w[1].content.showing)
            .count()
    }

    /// How many separate cross-fades were started.
    fn cross_fades(&self) -> usize {
        self.visuals
            .windows(2)
            .filter(|w| w[0].content.leaving.is_none() && w[1].content.leaving.is_some())
            .count()
    }

    /// Every body the panel showed, in order, with repeats collapsed.
    fn bodies(&self) -> Vec<Content> {
        let mut out: Vec<Content> = Vec::new();
        for v in &self.visuals {
            if out.last() != Some(&v.content.showing) {
                out.push(v.content.showing);
            }
        }
        out
    }

    /// The tallest and shortest the panel got.
    fn height_span(&self) -> (f32, f32) {
        self.visuals.iter().fold((f32::MAX, 0.0f32), |(lo, hi), v| {
            (lo.min(v.height), hi.max(v.height))
        })
    }

    fn report(&self, what: &str) {
        let (lo, hi) = self.height_span();
        println!(
            "{what}: {} frames, {} resizes, {} body changes, {} cross-fades, \
             height {lo:.0}-{hi:.0}pt, bodies {:?}",
            self.visuals.len(),
            self.resizes.len(),
            self.body_changes(),
            self.cross_fades(),
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
    inflight: Vec<(u64, String)>,
    /// What the index holds.
    corpus: Vec<&'static str>,
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
        rig.log = Log::default();
        rig
    }

    fn feed(&mut self, event: AppEvent) {
        let response = self.state.update(event, self.now);
        for cmd in &response.cmds {
            if let Cmd::Search { query, epoch } = cmd {
                self.inflight.push((*epoch, query.clone()));
            }
        }
    }

    /// Answers whatever the matcher was asked, the way the real one would:
    /// on a later frame than the one that asked.
    fn answer(&mut self, epoch: u64, query: String) {
        let needle = query.to_ascii_lowercase();
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
        let visual = self.frame.advance(&self.state, FRAME.as_secs_f32());
        if let Some(size) = self.frame.resize(&visual, false) {
            self.log.resizes.push((size.x, size.y));
        }
        self.log.visuals.push(visual);
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

    fn last(&self) -> Visual {
        *self.log.visuals.last().expect("no frames were drawn")
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
    assert!(
        !rig.state.hits.is_empty(),
        "the fixture found nothing to work with"
    );

    rig.log = Log::default();
    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.log.report("typing_a_code");

    assert!(
        !rig.log
            .visuals
            .iter()
            .any(|v| v.content.showing == Content::Empty),
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
    rig.log = Log::default();
    rig.type_code("D-0704", KEYSTROKE_GAP);

    assert_eq!(
        rig.log.body_changes(),
        0,
        "the body changed over six keystrokes: {:?}",
        rig.log.bodies()
    );
    assert_eq!(rig.log.cross_fades(), 0, "and cross-faded to get there");
}

/// A cross-fade renders the outgoing body over the first 45% and the incoming
/// one over the last 55%, with no overlap. Run from `Results` back to
/// `Results` that is a list dissolving and returning for no reason, which is
/// what `Cross::retarget` used to do on every keystroke.
#[test]
fn the_body_never_cross_fades_to_itself() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code(CODE, KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));

    for (i, v) in rig.log.visuals.iter().enumerate() {
        if let Some(leaving) = v.content.leaving {
            assert_ne!(
                leaving, v.content.showing,
                "frame {i} is fading {leaving:?} into itself"
            );
        }
    }
}

/// The panel is the window, so a height transition is a `SetWindowPos` and a
/// swapchain reconfigure per frame. That is a fair price for a transition
/// somebody asked for and an unreasonable one for six keystrokes.
#[test]
fn typing_a_code_costs_the_window_system_almost_nothing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(300));
    rig.log = Log::default();

    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(600));
    rig.log.report("window_cost");

    // The result count genuinely narrows as the code gets longer, so some
    // resizing is honest - but it is bounded by how often the count actually
    // changes, not by the frame rate.
    // One per genuine change in the row count, and no more: the panel eases
    // to the new height *inside* a window that was resized once. It was
    // thirty-one - one per frame of every transition - when the window was
    // what moved.
    assert!(
        rig.log.resizes.len() <= 6,
        "six keystrokes cost {} window resizes: {:?}",
        rig.log.resizes.len(),
        rig.log.resizes
    );
}

/// A selection that blinks out and back is the same defect as a list that
/// does, one row tall.
#[test]
fn the_selection_stays_on_screen_while_typing() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.log = Log::default();
    rig.type_code("D-07", KEYSTROKE_GAP);

    assert!(
        rig.log.visuals.iter().all(|v| v.selection_y.is_some()),
        "the selection highlight left the screen while typing"
    );
}

/// The panel is allowed to settle at a new height. It is not allowed to travel
/// somewhere and come back, which is what a transient empty body made it do.
#[test]
fn the_panel_height_never_doubles_back() {
    let mut rig = Rig::summoned(corpus());
    rig.type_code("11-", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(300));
    rig.log = Log::default();

    // Each character can only narrow this result set, so the panel can only
    // ever get shorter. A frame taller than the one before it is the panel
    // going back for something.
    rig.type_code("D-0704", KEYSTROKE_GAP);
    rig.run(Duration::from_millis(400));

    for (i, w) in rig.log.visuals.windows(2).enumerate() {
        assert!(
            w[1].height <= w[0].height + 0.01,
            "frame {i} grew from {:.1}pt to {:.1}pt on a query that can only narrow",
            w[0].height,
            w[1].height
        );
    }
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
    rig.log = Log::default();

    // The same answer, again, as a republished snapshot would deliver it.
    let epoch = rig.state.query_epoch();
    rig.inflight.push((epoch, CODE.to_string()));
    rig.run(Duration::from_millis(400));

    assert_eq!(rig.log.resizes.len(), 0, "it resized the window");
    assert_eq!(rig.log.body_changes(), 0, "it changed the body");
    for v in &rig.log.visuals {
        assert!(
            (v.height - before.height).abs() < 0.01,
            "the panel moved from {:.1}pt to {:.1}pt",
            before.height,
            v.height
        );
        assert_eq!(v.selection_y, before.selection_y, "the selection moved");
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
        rig.last().content.showing,
        Content::Empty,
        "bodies were {:?}",
        rig.log.bodies()
    );
}
