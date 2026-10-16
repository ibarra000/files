//! One turn of the application loop: apply events, dispatch commands.
//!
//! The UI toolkit owns the loop and calls [`App::pump`] each frame. Deadlines
//! come from the state, so:
//!
//! * **idle costs almost nothing**: with nothing time-dependent on screen
//!   there is no deadline at all, and a displayed index age wakes only when
//!   its digit changes;
//! * **timers cannot be lost or leaked**: `Tick` is synthesised locally when
//!   a deadline expires, never sent over the channel.
//!
//! A burst of events is drained before drawing, so a twenty-character paste
//! produces one frame.

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
/// A struct of flags rather than an enum so that, across a burst of
/// keystrokes, a second request cannot replace the first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowRequests {
    pub settings: bool,
}

/// The application core, independent of the UI toolkit.
///
/// Redraw and commands accumulate across events, so a burst becomes one frame
/// and one dispatch however the driver feeds it.
pub struct App {
    pub state: AppState,
    pub actors: Actors,
    rx: Receiver<AppEvent>,
    /// What the events applied so far have asked for, not yet dispatched.
    pending: CmdList,
    /// Windows a keystroke asked for that the shell has not acted on yet.
    ///
    /// A latch rather than an entry in `pending`: `logic` feeds and pumps, which
    /// drains `pending`, before `ui` asks.
    requested: WindowRequests,
    /// Whether this process has written the configuration file since the
    /// shell last asked.
    ///
    /// The settings window reads the same file and must be told. Latched for
    /// the same reason as `requested`.
    saved: bool,
    /// Whether the applied events changed anything on screen.
    redraw: Redraw,
}

impl App {
    /// Starts the workers and loads what the first frame needs.
    ///
    /// `wake` is called by workers after posting; see [`event::Events`].
    pub fn start(
        settings: Settings,
        source: Arc<dyn DirSource>,
        wake: event::Wake,
    ) -> io::Result<Self> {
        let viewers_missing = crate::open::probe_viewers(&settings.viewers);
        let (actors, rx) = Actors::start(settings.clone(), source, wake)?;

        // Loaded before the first frame so Up works immediately; read here
        // because the state machine does no I/O.
        let remembered = match (settings.history, settings.history_path.clone()) {
            (true, Some(path)) => crate::history::load(&path),
            _ => Vec::new(),
        };

        let mut state = AppState::new(settings, Instant::now());
        state.seed_history(remembered);
        // Probed here, once, because it searches PATH and the state machine does
        // no I/O. Re-probed when viewers change; see `gui::Shell::adopt_config`.
        state.set_viewers_missing(viewers_missing);

        Ok(Self {
            state,
            actors,
            rx,
            pending: CmdList::new(),
            requested: WindowRequests::default(),
            saved: false,
            redraw: Redraw::No,
        })
    }

    /// Which windows were asked for since this was last called.
    ///
    /// Read-and-clear, so a toggle request is acted on exactly once. Reads a
    /// latch set in [`Self::feed`], because [`Self::pump`] empties `pending`
    /// before the shell asks.
    pub fn take_window_requests(&mut self) -> WindowRequests {
        std::mem::take(&mut self.requested)
    }

    /// Whether this process has written the configuration file since the
    /// last call. See the field.
    pub fn take_saved(&mut self) -> bool {
        std::mem::take(&mut self.saved)
    }

    /// Applies one event, holding on to what it asked for.
    ///
    /// Public so a driver can feed events it received directly, such as
    /// keystrokes, into the same turn as worker events.
    pub fn feed(&mut self, event: AppEvent, now: Instant) {
        // Checked before `update`, which consumes the event. F2 is the only
        // path in this process that writes the configuration file.
        if matches!(&event, AppEvent::Open(event::OpenMsg::ViewerSaved { .. })) {
            self.saved = true;
        }
        let response = self.state.update(event, now);
        self.redraw = self.redraw.or(response.redraw);
        // Latched because `pending` does not survive the `pump` that `logic`
        // performs before `ui` asks.
        for cmd in &response.cmds {
            if cmd == &event::Cmd::ToggleSettings {
                self.requested.settings = true;
            }
        }
        self.pending.extend(response.cmds);
    }

    /// Applies everything queued and dispatches what it all asked for.
    ///
    /// Drains the whole queue so a burst draws once.
    ///
    /// Synthesises [`AppEvent::Tick`] when a deadline has passed. `on_tick`
    /// re-checks every deadline against `now`, so an early tick is harmless.
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

    /// Builds an `App` around a caller-owned channel, so tests can disconnect
    /// it while `Actors` holds its own sender.
    #[cfg(test)]
    fn around(state: AppState, actors: Actors, rx: Receiver<AppEvent>) -> Self {
        Self {
            state,
            actors,
            rx,
            pending: CmdList::new(),
            requested: WindowRequests::default(),
            saved: false,
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

    /// Replays the loop's event sequencing without a window.
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
        AppState::new(Settings::fixture(), Instant::now())
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
        // No key quits; the shutdown and channel-disconnect paths are the only
        // ways in.
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

    /// A channel with no senders left must end the drain, not spin a core.
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

    /// A window request survives the pump between the keystroke and the
    /// shell's query. `gui::Shell::logic` feeds and pumps; `ui` asks later.
    #[test]
    fn a_window_request_survives_the_pump_that_follows_it() {
        let (_tx, rx) = bounded::<AppEvent>(4);
        let mut app = App::around(state(), actors_for_test(), rx);
        let now = Instant::now();

        app.feed(
            AppEvent::Key(KeyEvent::new(Key::Char(','), Mods::CTRL)),
            now,
        );
        // What the shell does between the keystroke and the query.
        let _ = app.pump(now);

        assert!(
            app.take_window_requests().settings,
            "the pump swallowed the request before anybody could read it"
        );
        assert!(
            !app.take_window_requests().settings,
            "read-and-clear, or the window opens again on the next frame"
        );
        app.shutdown();
    }

    /// Workers for tests that need an `App` but do not drive it.
    fn actors_for_test() -> Actors {
        let source: Arc<dyn crate::index::enumerate::DirSource> =
            Arc::new(crate::index::fake_source::FakeDirSource::new().with_dir("V:\\", &["a.pdf"]));
        let settings = Settings {
            persist: false,
            ..Settings::fixture()
        };
        Actors::start(settings, source, Arc::new(|| {}))
            .expect("the workers start")
            .0
    }
}
