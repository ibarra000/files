//! Runs the `--doctor` diagnostics report off the frame thread for the
//! Diagnostics page.
//!
//! `doctor` can take several seconds (it times uncached round trips to every
//! share and decodes the whole index), so it runs on a named thread that
//! wakes the frame loop when done. A refresh keeps the previous report on
//! screen ([`State::Refreshing`]), and a result is reused for [`TTL`].
//!
//! Panics in `doctor` are caught and shown as text in the report box rather
//! than taking down the process.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::config::Settings;

/// How long a report is reused before a page visit triggers a new run.
pub const TTL: Duration = Duration::from_secs(60);

enum State {
    /// No report and none requested.
    Idle,
    /// First run in progress.
    Running,
    /// A report and when it was taken.
    Ready { text: Arc<str>, at: Instant },
    /// A previous report, with a new run in progress.
    Refreshing { text: Arc<str> },
}

/// What the Diagnostics page should draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View<'a> {
    /// A spinner with no report.
    Waiting,
    Ready(&'a str),
    /// The previous report, with a newer one pending.
    Stale(&'a str),
}

/// The cached report and any run in progress.
pub struct Reporter {
    state: State,
    /// Receiver for the run in progress, if any.
    running: Option<Receiver<String>>,
}

impl Default for Reporter {
    fn default() -> Self {
        Self {
            state: State::Idle,
            running: None,
        }
    }
}

impl Reporter {
    /// Collects a finished run, if any. Call once per frame.
    pub fn poll(&mut self, now: Instant) {
        let Some(rx) = &self.running else {
            return;
        };
        match rx.try_recv() {
            Ok(text) => {
                self.state = State::Ready {
                    text: text.into(),
                    at: now,
                };
                self.running = None;
            }
            Err(TryRecvError::Empty) => {}
            // The thread exited without sending (`spawn` prevents this).
            // Fall back to Idle rather than spinning indefinitely.
            Err(TryRecvError::Disconnected) => {
                self.state = State::Idle;
                self.running = None;
            }
        }
    }

    /// Starts a run if `wanted` and no report younger than [`TTL`] exists.
    /// Call every frame.
    pub fn wanted(
        &mut self,
        wanted: bool,
        settings: &Settings,
        hotkey: Option<Result<(), String>>,
        now: Instant,
        wake: impl FnOnce() + Send + 'static,
    ) {
        if !wanted || self.running.is_some() {
            return;
        }
        let fresh = match &self.state {
            State::Ready { at, .. } => now.duration_since(*at) < TTL,
            State::Idle => false,
            // A run is already in progress.
            State::Running | State::Refreshing { .. } => true,
        };
        if fresh {
            return;
        }
        self.state = State::Running;
        self.running = Some(spawn(settings.clone(), hotkey, wake));
    }

    /// Starts a run regardless of the cache, keeping the old report visible.
    pub fn refresh(
        &mut self,
        settings: &Settings,
        hotkey: Option<Result<(), String>>,
        wake: impl FnOnce() + Send + 'static,
    ) {
        if self.running.is_some() {
            return;
        }
        self.state = match std::mem::replace(&mut self.state, State::Running) {
            State::Ready { text, .. } | State::Refreshing { text } => State::Refreshing { text },
            _ => State::Running,
        };
        self.running = Some(spawn(settings.clone(), hotkey, wake));
    }

    pub fn view(&self) -> View<'_> {
        match &self.state {
            State::Idle | State::Running => View::Waiting,
            State::Ready { text, .. } => View::Ready(text),
            State::Refreshing { text } => View::Stale(text),
        }
    }

    /// Returns the report text for copying; empty while waiting.
    pub fn text(&self) -> &str {
        match &self.state {
            State::Idle | State::Running => "",
            State::Ready { text, .. } | State::Refreshing { text } => text,
        }
    }
}

/// Runs `doctor` on a named thread and sends the text back. The name makes
/// the thread identifiable in a debugger when blocked on SMB.
fn spawn(
    settings: Settings,
    hotkey: Option<Result<(), String>>,
    wake: impl FnOnce() + Send + 'static,
) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("files-doctor".to_owned())
        .spawn(move || {
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let source = crate::app::actors::default_source(&settings);
                let mut out = Vec::new();
                crate::doctor::doctor(&settings, source, hotkey, &mut out);
                String::from_utf8_lossy(&out).into_owned()
            }))
            .unwrap_or_else(|_| {
                "The diagnostics run failed part way through.\n\n\
                 This is a fault in files rather than in the machine it is \
                 looking at. Copying this box into an email is the useful \
                 thing to do with it."
                    .to_owned()
            });
            // Send before waking so the woken frame finds the result.
            let _ = tx.send(text);
            wake();
        });

    // Report a failed spawn in the box instead of spinning indefinitely.
    if spawned.is_err() {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send("The diagnostics report could not be started.".to_owned());
        return rx;
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a reporter holding `text`, without running `doctor`.
    fn ready(text: &str, at: Instant) -> Reporter {
        Reporter {
            state: State::Ready {
                text: text.into(),
                at,
            },
            running: None,
        }
    }

    #[test]
    fn a_page_with_no_report_yet_shows_that_it_is_coming() {
        let reporter = Reporter::default();
        assert_eq!(reporter.view(), View::Waiting);
        assert_eq!(reporter.text(), "");
    }

    #[test]
    fn coming_back_to_the_page_within_the_minute_reuses_the_answer() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        reporter.wanted(true, &Settings::fixture(), None, now + TTL / 2, move || {
            flag.store(true, std::sync::atomic::Ordering::Relaxed)
        });
        assert!(reporter.running.is_none(), "a second run was started");
        assert!(!woken.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(reporter.view(), View::Ready("all fine"));
    }

    #[test]
    fn an_answer_older_than_the_minute_is_taken_again() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        reporter.wanted(true, &Settings::fixture(), None, now + TTL * 2, || {});
        assert!(reporter.running.is_some(), "no second run was started");
        assert_eq!(reporter.view(), View::Waiting);
    }

    #[test]
    fn a_page_that_wants_nothing_starts_nothing() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        reporter.wanted(false, &Settings::fixture(), None, now + TTL * 2, || {});
        assert!(reporter.running.is_none());
        assert_eq!(reporter.view(), View::Ready("all fine"));
    }

    #[test]
    fn refreshing_keeps_the_old_report_on_screen() {
        let mut reporter = ready("all fine", Instant::now());
        reporter.refresh(&Settings::fixture(), None, || {});
        assert_eq!(reporter.view(), View::Stale("all fine"));
        assert_eq!(reporter.text(), "all fine");
    }

    #[test]
    fn refreshing_from_nothing_is_a_plain_wait() {
        let mut reporter = Reporter::default();
        reporter.refresh(&Settings::fixture(), None, || {});
        assert_eq!(reporter.view(), View::Waiting);
    }

    #[test]
    fn an_answer_lands_and_the_spinner_stops() {
        let now = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut reporter = Reporter {
            state: State::Running,
            running: Some(rx),
        };
        reporter.poll(now);
        assert_eq!(reporter.view(), View::Waiting, "polled an empty channel");

        tx.send("a fresh reading".to_owned()).unwrap();
        reporter.poll(now);
        assert_eq!(reporter.view(), View::Ready("a fresh reading"));
        assert!(reporter.running.is_none(), "the finished run was kept");
    }

    /// A thread that exits without sending returns the reporter to Idle.
    #[test]
    fn a_thread_that_vanishes_does_not_spin_for_ever() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let mut reporter = Reporter {
            state: State::Running,
            running: Some(rx),
        };
        drop(tx);
        reporter.poll(Instant::now());
        assert!(reporter.running.is_none());
        // A new request starts a new run.
        reporter.wanted(true, &Settings::fixture(), None, Instant::now(), || {});
        assert!(reporter.running.is_some());
    }
}
