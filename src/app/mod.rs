//! The main loop.
//!
//! One receiver, one state machine, one place that draws. The loop blocks on
//! the event channel with a deadline computed from the state, so:
//!
//! * **idle costs nothing** - a blocking receive with no deadline, zero
//!   wakeups, rather than the previous fifty-frames-a-second poll;
//! * **latency is zero** - a keystroke wakes the thread immediately;
//! * **timers cannot be lost or leaked** - `Tick` is synthesised locally when
//!   a deadline expires, never sent by anyone.
//!
//! A burst of events is drained before drawing, so pasting twenty characters
//! produces one frame rather than twenty.

pub mod actors;
pub mod event;
pub mod input;
pub mod state;

use std::io;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use ratatui::Terminal;
use ratatui::backend::Backend as RatatuiBackend;

use self::actors::Actors;
use self::event::AppEvent;
use self::state::AppState;
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use crate::ui;

/// Runs the application until the user quits.
pub fn run<B: RatatuiBackend>(
    terminal: &mut Terminal<B>,
    settings: Settings,
    source: Arc<dyn DirSource>,
    volume_serial: Option<u32>,
) -> io::Result<()> {
    let avwin_missing = !crate::open::avwin_available();
    let (mut actors, rx) = Actors::start(settings.clone(), source, volume_serial)?;

    // Loaded before the first frame so the Up arrow works immediately, and
    // read here rather than inside the state machine, which does no I/O.
    let remembered = match (settings.history, settings.history_path.clone()) {
        (true, Some(path)) => crate::history::load(&path),
        _ => Vec::new(),
    };

    let mut state = AppState::new(settings, Instant::now());
    state.seed_history(remembered);
    // Mouse events arrive in screen coordinates, so the state needs the size
    // before the first click, not merely after the first resize.
    state.set_area(terminal.size()?);

    let result = event_loop(terminal, &mut state, &actors, &rx, avwin_missing);

    // Workers are stopped after the caller has already restored the terminal,
    // so a wedged network thread can never keep the user out of their shell.
    actors.shutdown();
    result
}

fn event_loop<B: RatatuiBackend>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    actors: &Actors,
    rx: &Receiver<AppEvent>,
    avwin_missing: bool,
) -> io::Result<()> {
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| ui::draw(frame, state, avwin_missing))?;
            state.note_frame(Instant::now());
            dirty = false;
        }

        let event = match state.next_deadline() {
            Some(deadline) => match rx.recv_deadline(deadline) {
                Ok(event) => event,
                // The deadline passed: synthesise the tick locally rather than
                // relying on anyone to have sent one.
                Err(RecvTimeoutError::Timeout) => AppEvent::Tick,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
            // Nothing pending, so block indefinitely at zero cost.
            None => match rx.recv() {
                Ok(event) => event,
                Err(_) => return Ok(()),
            },
        };

        let mut response = state.update(event, Instant::now());

        // Drain whatever else is already queued, so a burst draws once.
        while !state.should_quit {
            match rx.try_recv() {
                Ok(event) => response.merge(state.update(event, Instant::now())),
                Err(_) => break,
            }
        }

        dirty |= response.redraw.is_yes();
        actors.dispatch(response.cmds);

        if state.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::{Cmd, Redraw, Response};
    use crossbeam_channel::bounded;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
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
            .map(|c| AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
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
            s.update(
                AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                now,
            );
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

    #[test]
    fn a_disconnected_channel_ends_the_loop_rather_than_spinning() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut s = state();
        let (tx, rx) = bounded::<AppEvent>(4);
        drop(tx);

        let source: Arc<dyn crate::index::enumerate::DirSource> =
            Arc::new(crate::index::fake_source::FakeDirSource::new().with_dir("V:\\", &[]));
        let settings = Settings {
            persist: false,
            ..Default::default()
        };
        let (actors, _rx2) = Actors::start(settings, source, None).unwrap();

        let started = Instant::now();
        event_loop(&mut terminal, &mut s, &actors, &rx, false).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut actors = actors;
        actors.shutdown();
    }

    #[test]
    fn the_loop_renders_and_exits_on_a_quit_event() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut s = state();
        let (tx, rx) = bounded::<AppEvent>(8);

        let source: Arc<dyn crate::index::enumerate::DirSource> =
            Arc::new(crate::index::fake_source::FakeDirSource::new().with_dir("V:\\", &["a.pdf"]));
        let settings = Settings {
            persist: false,
            ..Default::default()
        };
        let (actors, _rx2) = Actors::start(settings, source, None).unwrap();

        tx.send(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )))
        .unwrap();
        tx.send(AppEvent::Shutdown).unwrap();

        event_loop(&mut terminal, &mut s, &actors, &rx, false).unwrap();
        assert!(s.should_quit);

        // Something was actually drawn.
        let buffer = terminal.backend().buffer().clone();
        let text: String = (0..buffer.area.width)
            .map(|x| buffer.get(x, 1).symbol())
            .collect();
        assert!(text.contains("File Code Search"), "{text}");

        let mut actors = actors;
        actors.shutdown();
    }
}
