//! The main loop.
//!
//! One receiver, one state machine, one place that draws. The loop blocks on
//! the event channel with a deadline computed from the state, so:
//!
//! * **idle costs almost nothing** - a blocking receive with no deadline at
//!   all when nothing on screen goes stale on its own, rather than the
//!   previous fifty-frames-a-second poll. A displayed index age costs exactly
//!   one wakeup per change of the digit: once a second while it reads in
//!   seconds, once a minute while it reads in minutes, once a day while it
//!   reads in days. It used to cost none, and the age simply froze until a
//!   keystroke arrived and then jumped;
//! * **latency is zero** - a keystroke wakes the thread immediately;
//! * **timers cannot be lost or leaked** - `Tick` is synthesised locally when
//!   a deadline expires, never sent by anyone.
//!
//! A burst of events is drained before drawing, so pasting twenty characters
//! produces one frame rather than twenty.

pub mod actors;
pub mod event;
pub mod input;
pub mod key;
pub mod state;

use std::io;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Receiver;

use self::actors::Actors;
use self::event::{AppEvent, CmdList, Redraw};
use self::state::AppState;
use crate::config::Settings;
use crate::index::enumerate::DirSource;

/// Windows a keystroke asked the shell to show or hide.
///
/// A struct of bools rather than one enum, because two can legitimately be
/// asked for in the same turn - a burst of keystrokes is fed before anything
/// reads this - and an enum would make the second silently replace the first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowRequests {
    pub help: bool,
    pub settings: bool,
}

/// Everything a driver needs, with no opinion about who is drawing.
///
/// `run` used to own the loop. A window toolkit owns its own, so what is left
/// here is the body of one turn - and both drivers take it: the terminal once
/// per blocking receive, a window once per frame.
///
/// The redraw and the commands are accumulated rather than returned one event
/// at a time, which is what lets a burst of twenty keystrokes become one frame
/// and one dispatch however the driver chose to feed them in.
pub struct App {
    pub state: AppState,
    pub actors: Actors,
    rx: Receiver<AppEvent>,
    /// What the events applied so far have asked for, not yet dispatched.
    pending: CmdList,
    /// Windows a keystroke asked for that the shell has not acted on yet.
    ///
    /// A latch rather than an entry in `pending`, and that is the whole of the
    /// fix for a keystroke that did nothing for as long as it existed. `pump`
    /// drains `pending` into `Actors::dispatch`, which names `Cmd::ToggleHelp`
    /// only to ignore it - so by the time the shell asked, the request had
    /// already been thrown away. `logic` feeds and pumps; `ui` asks. Nothing
    /// that survives only between those two can be read from there.
    requested: WindowRequests,
    /// ...and whether they changed anything on screen.
    redraw: Redraw,
}

impl App {
    /// Starts the workers and loads what the first frame needs.
    ///
    /// `wake` is what the workers call after posting; see [`event::Events`].
    /// A driver that blocks on the receiver passes a no-op, because a send
    /// wakes it by construction.
    pub fn start(
        settings: Settings,
        source: Arc<dyn DirSource>,
        wake: event::Wake,
    ) -> io::Result<Self> {
        let avwin_missing = !crate::open::avwin_available();
        let (actors, rx) = Actors::start(settings.clone(), source, wake)?;

        // Loaded before the first frame so the Up arrow works immediately, and
        // read here rather than inside the state machine, which does no I/O.
        let remembered = match (settings.history, settings.history_path.clone()) {
            (true, Some(path)) => crate::history::load(&path),
            _ => Vec::new(),
        };

        let mut state = AppState::new(settings, Instant::now());
        state.seed_history(remembered);
        // Probed here rather than in the state machine, which does no I/O, and
        // once rather than per frame: it is a search of PATH, and the answer
        // cannot change without the program being restarted.
        state.set_avwin_missing(avwin_missing);

        Ok(Self {
            state,
            actors,
            rx,
            pending: CmdList::new(),
            requested: WindowRequests::default(),
            redraw: Redraw::No,
        })
    }

    /// Which windows were asked for since this was last called.
    ///
    /// These commands are addressed to whoever is drawing rather than to a
    /// worker. Read-and-clear, so two readers of one "somebody asked for help"
    /// cannot toggle the window twice or not at all - which with a toggle is
    /// the difference between opening it and leaving it exactly as it was.
    ///
    /// Reads a latch set in [`Self::feed`] rather than scanning `pending`. It
    /// used to scan, and could therefore never see a keystroke: `logic` feeds
    /// the key and then calls [`Self::pump`], which empties `pending` into
    /// `Actors::dispatch`; `ui` asks afterwards, by which time there is
    /// nothing left to find. Clicking the F1 chip worked and pressing F1 did
    /// not, for the sole reason that the click is fed two lines above the ask.
    pub fn take_window_requests(&mut self) -> WindowRequests {
        std::mem::take(&mut self.requested)
    }

    /// Applies one event, holding on to what it asked for.
    ///
    /// Public so a driver can feed events of its own - a keystroke a window
    /// received directly, rather than one a worker posted - and have them
    /// coalesce into the same turn as everything else.
    pub fn feed(&mut self, event: AppEvent, now: Instant) {
        let response = self.state.update(event, now);
        self.redraw = self.redraw.or(response.redraw);
        // Latched on the way past, because `pending` does not survive the
        // `pump` that `logic` performs before `ui` ever asks.
        for cmd in &response.cmds {
            match cmd {
                event::Cmd::ToggleHelp => self.requested.help = true,
                event::Cmd::ToggleSettings => self.requested.settings = true,
                _ => {}
            }
        }
        self.pending.extend(response.cmds);
    }

    /// Applies everything queued and dispatches what it all asked for.
    ///
    /// Drains rather than taking one, so a burst - twenty characters of a
    /// paste, or five index reports landing together - draws once.
    ///
    /// Synthesises [`AppEvent::Tick`] when a deadline has passed, because
    /// nobody sends it: it exists precisely so that a timer cannot be lost or
    /// leaked in transit. `on_tick` re-checks every deadline against `now`, so
    /// feeding it on a frame some keystroke caused is correct and slightly
    /// more prompt than waiting for the deadline to arrive on its own.
    pub fn pump(&mut self, now: Instant) -> Redraw {
        if self.state.next_deadline().is_some_and(|due| due <= now) {
            self.feed(AppEvent::Tick, now);
        }

        while !self.state.should_quit {
            match self.rx.try_recv() {
                Ok(event) => self.feed(event, now),
                Err(_) => break,
            }
        }

        let cmds = std::mem::take(&mut self.pending);
        self.actors.dispatch(cmds);
        std::mem::replace(&mut self.redraw, Redraw::No)
    }

    /// When to wake next, or `None` to sleep until something happens.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.state.next_deadline()
    }

    pub fn should_quit(&self) -> bool {
        self.state.should_quit
    }

    pub fn shutdown(&mut self) -> bool {
        self.actors.shutdown()
    }

    /// Builds one around a channel the caller owns.
    ///
    /// Only for the two tests below, which are about what the loop does when
    /// the channel dies - a thing that cannot be arranged from outside while
    /// `Actors` is holding the only sender.
    #[cfg(test)]
    fn around(state: AppState, actors: Actors, rx: Receiver<AppEvent>) -> Self {
        Self {
            state,
            actors,
            rx,
            pending: CmdList::new(),
            requested: WindowRequests::default(),
            redraw: Redraw::No,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::{Cmd, Events, Redraw, Response};
    use crate::app::key::{Key, KeyEvent, Mods};
    use crossbeam_channel::bounded;
    use std::time::Duration;

    /// Drives the loop's decision logic without the terminal, by replaying the
    /// same sequencing the loop performs.
    fn step(state: &mut AppState, events: Vec<AppEvent>, now: Instant) -> Response {
        let mut it = events.into_iter();
        let first = it.next().expect("at least one event");
        let mut response = state.update(first, now);
        for event in it {
            response.merge(state.update(event, now));
        }
        response
    }

    fn state() -> AppState {
        AppState::new(Settings::default(), Instant::now())
    }

    #[test]
    fn a_burst_of_keystrokes_produces_one_redraw_request() {
        let mut s = state();
        let now = Instant::now();
        let events: Vec<AppEvent> = "11-D-0704"
            .chars()
            .map(|c| AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)))
            .collect();

        let response = step(&mut s, events, now);
        assert_eq!(response.redraw, Redraw::Yes, "one merged redraw, not nine");
        assert_eq!(s.input, "11-D-0704");
    }

    #[test]
    fn an_idle_state_asks_the_loop_to_block_indefinitely() {
        let s = state();
        assert!(
            s.next_deadline().is_none(),
            "idle must not schedule a wakeup, or the CPU never sleeps"
        );
    }

    #[test]
    fn a_pending_debounce_gives_the_loop_a_deadline() {
        let mut s = state();
        let now = Instant::now();
        for c in "11-D-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
        let deadline = s.next_deadline().expect("verification is pending");
        assert!(deadline > now);
        assert!(deadline <= now + crate::config::VERIFY_DEBOUNCE);
    }

    #[test]
    fn quitting_is_requested_exactly_once() {
        let mut s = state();
        let now = Instant::now();
        // Driven by `Shutdown`, because no key quits any more: Ctrl+C copies
        // and Esc clears. The window's close button ends the program, so the
        // shutdown and channel-disconnect paths are the only ways in.
        let response = s.update(AppEvent::Shutdown, now);
        assert!(s.should_quit);
        assert_eq!(
            response
                .cmds
                .iter()
                .filter(|c| matches!(c, Cmd::Quit))
                .count(),
            1
        );
    }

    /// A channel with no senders left must not be something `pump` sits in.
    ///
    /// The terminal loop blocked on this channel, so a disconnect was the
    /// thing that ended it. Nothing blocks on it now - the toolkit owns the
    /// waiting - but a `try_recv` loop that treats "disconnected" as "nothing
    /// this time" is a frame that never finishes, which is worse than the
    /// hang it replaced because it burns a core while doing it.
    #[test]
    fn a_disconnected_channel_does_not_wedge_a_frame() {
        let (tx, rx) = bounded::<AppEvent>(4);
        drop(tx);

        let mut app = App::around(state(), actors_for_test(), rx);

        let started = Instant::now();
        let _ = app.pump(Instant::now());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a channel with no senders left should end the drain, not spin on it"
        );
        app.shutdown();
    }

    /// One turn applies everything waiting, in order, and stops at the quit.
    #[test]
    fn one_turn_applies_the_whole_queue_and_notices_the_quit() {
        let (tx, rx) = bounded::<AppEvent>(8);
        let tx = Events::headless(tx);

        tx.send(AppEvent::Key(KeyEvent::new(Key::Char('a'), Mods::NONE)))
            .unwrap();
        tx.send(AppEvent::Shutdown).unwrap();

        let mut app = App::around(state(), actors_for_test(), rx);
        let redraw = app.pump(Instant::now());

        assert!(app.should_quit());
        assert!(redraw.is_yes(), "a keystroke and a quit changed nothing?");
        assert_eq!(
            app.state.input.text(),
            "a",
            "the keystroke ahead of the quit was dropped"
        );
        app.shutdown();
    }

    /// F1 survives the pump that happens between the key and the question.
    ///
    /// The seam nothing covered, and the reason F1 did nothing for as long as
    /// it existed. `tests/overlay.rs` asserts the state machine *emits*
    /// `Cmd::ToggleHelp`, which it always did; `gui::Shell::logic` then feeds and
    /// pumps, and `ui` asks afterwards. Anything that lived only in `pending`
    /// was gone by then.
    #[test]
    fn a_help_request_survives_the_pump_that_follows_it() {
        let (_tx, rx) = bounded::<AppEvent>(4);
        let mut app = App::around(state(), actors_for_test(), rx);
        let now = Instant::now();

        app.feed(AppEvent::Key(KeyEvent::new(Key::F(1), Mods::NONE)), now);
        // Exactly what the shell does between the keystroke and the question.
        let _ = app.pump(now);

        assert!(
            app.take_window_requests().help,
            "the pump swallowed the request before anybody could read it"
        );
        assert!(
            !app.take_window_requests().help,
            "read-and-clear, or the window opens again on the next frame"
        );
        app.shutdown();
    }

    /// Workers nothing here drives, for the two tests that need an `App` and
    /// do not care what is behind it.
    fn actors_for_test() -> Actors {
        let source: Arc<dyn crate::index::enumerate::DirSource> =
            Arc::new(crate::index::fake_source::FakeDirSource::new().with_dir("V:\\", &["a.pdf"]));
        let settings = Settings {
            persist: false,
            ..Default::default()
        };
        Actors::start(settings, source, Arc::new(|| {}))
            .expect("the workers start")
            .0
    }
}
