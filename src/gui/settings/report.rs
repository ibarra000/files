//! Whether the diagnostics report is there yet.
//!
//! The Diagnostics page shows the output of `--doctor`, which is the same
//! function the console build runs. That function is not fast and it was
//! never meant to be: it reads a volume's flags, times thirty-two
//! deliberately-uncached round trips to every enabled share, memory-maps and
//! decodes the whole index, and walks a cache directory to add its files up.
//! On a laptop over a VPN that is several seconds.
//!
//! It was being run **inline, on the frame thread, before the viewport was
//! created**. So opening the settings window on the Diagnostics page did not
//! hitch the window - it delayed the window, entirely, until the report came
//! back. Nothing appeared, including the title bar, and the program looked
//! hung because for those seconds it was.
//!
//! # What this is
//!
//! A named thread, a channel, and four states. The window opens immediately
//! with a spinner where the report will be, and the thread wakes the frame
//! loop once when it has an answer.
//!
//! Two properties are worth naming because they took the shape they did on
//! purpose:
//!
//! * **A refresh keeps the old report on screen.** [`State::Refreshing`]
//!   holds the previous text, so pressing the refresh button replaces a page
//!   of diagnostics with a page of diagnostics and a spinner, rather than
//!   with nothing. Blanking a page somebody is reading in order to tell them
//!   it is being re-read is the worst of both.
//! * **The answer is kept for a minute.** It used to be discarded whenever
//!   the page was left, so clicking Diagnostics, going to About to check a
//!   version, and coming back re-ran the whole thing. A minute is short
//!   enough that a report is about the machine as it is now and long enough
//!   to cover somebody moving around the window. The button is there for
//!   when it is not.
//!
//! # Panics are caught
//!
//! `doctor` touches network shares, memory-mapped files and the registry,
//! and a panic on this thread would otherwise take the process down with the
//! user's settings window open. It is caught and reported as text, in the
//! box, where somebody can copy it into an email - which is what that box is
//! for.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::config::Settings;

/// How long a report stays current.
///
/// See the module note. Short enough that it describes the machine as it is,
/// long enough to survive somebody looking at another page and coming back.
pub const TTL: Duration = Duration::from_secs(60);

/// What the page has, and what is on its way.
enum State {
    /// Nothing, and nothing asked for.
    Idle,
    /// Nothing yet.
    Running,
    /// This, taken then.
    Ready { text: Arc<str>, at: Instant },
    /// This, and a fresher one on its way.
    Refreshing { text: Arc<str> },
}

/// What the page should draw.
///
/// Three cases and not two: "nothing yet" and "this, but it is being
/// replaced" want different things on screen, and collapsing them would mean
/// a refresh blanks the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View<'a> {
    /// A spinner, and no box.
    Waiting,
    /// The report.
    Ready(&'a str),
    /// The report, and a note that a fresher one is coming.
    Stale(&'a str),
}

/// The report, and whatever is being done about it.
pub struct Reporter {
    state: State,
    /// The thread's end of the channel, while one is running.
    job: Option<Receiver<String>>,
}

impl Default for Reporter {
    fn default() -> Self {
        Self {
            state: State::Idle,
            job: None,
        }
    }
}

impl Reporter {
    /// Collects a finished run, if there is one. Call once a frame.
    pub fn poll(&mut self, now: Instant) {
        let Some(rx) = &self.job else {
            return;
        };
        match rx.try_recv() {
            Ok(text) => {
                self.state = State::Ready {
                    text: text.into(),
                    at: now,
                };
                self.job = None;
            }
            Err(TryRecvError::Empty) => {}
            // The thread went away without sending, which `spawn` below
            // arranges not to happen. Falling back to Idle rather than
            // spinning for ever is the only safe answer.
            Err(TryRecvError::Disconnected) => {
                self.state = State::Idle;
                self.job = None;
            }
        }
    }

    /// Asks for a report if the page showing wants one and there is no
    /// current answer.
    ///
    /// Called every frame with whether the page wants it, so leaving the page
    /// does not throw the answer away and arriving at it within [`TTL`] does
    /// not start a second run.
    pub fn wanted(
        &mut self,
        wanted: bool,
        settings: &Settings,
        now: Instant,
        wake: impl FnOnce() + Send + 'static,
    ) {
        if !wanted || self.job.is_some() {
            return;
        }
        let fresh = match &self.state {
            State::Ready { at, .. } => now.duration_since(*at) < TTL,
            State::Idle => false,
            // Something is already running, or the poll above will pick it
            // up on the next frame.
            State::Running | State::Refreshing { .. } => true,
        };
        if fresh {
            return;
        }
        self.state = State::Running;
        self.job = Some(spawn(settings.clone(), wake));
    }

    /// Takes a fresh reading whatever the cache says, keeping the old one on
    /// screen while it runs.
    pub fn refresh(&mut self, settings: &Settings, wake: impl FnOnce() + Send + 'static) {
        if self.job.is_some() {
            return;
        }
        self.state = match std::mem::replace(&mut self.state, State::Running) {
            State::Ready { text, .. } | State::Refreshing { text } => State::Refreshing { text },
            _ => State::Running,
        };
        self.job = Some(spawn(settings.clone(), wake));
    }

    /// What to draw.
    pub fn view(&self) -> View<'_> {
        match &self.state {
            State::Idle | State::Running => View::Waiting,
            State::Ready { text, .. } => View::Ready(text),
            State::Refreshing { text } => View::Stale(text),
        }
    }

    /// The text there is, for the copy button. Empty while waiting.
    pub fn text(&self) -> &str {
        match &self.state {
            State::Idle | State::Running => "",
            State::Ready { text, .. } | State::Refreshing { text } => text,
        }
    }
}

/// Runs `doctor` on a thread of its own and sends the text back.
///
/// Named, because a thread with no name is a thread nobody can find in a
/// debugger, and this is the one most likely to be sitting in a blocked SMB
/// call when somebody looks.
fn spawn(settings: Settings, wake: impl FnOnce() + Send + 'static) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("files-doctor".to_owned())
        .spawn(move || {
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let source = crate::app::actors::default_source(&settings);
                let mut out = Vec::new();
                crate::doctor::doctor(&settings, source, &mut out);
                String::from_utf8_lossy(&out).into_owned()
            }))
            .unwrap_or_else(|_| {
                "The diagnostics run failed part way through.\n\n\
                 This is a fault in files rather than in the machine it is \
                 looking at. Copying this box into an email is the useful \
                 thing to do with it."
                    .to_owned()
            });
            // Before the wake, so the frame that is woken finds it.
            let _ = tx.send(text);
            wake();
        });

    // A machine that cannot start a thread is a machine in real trouble, and
    // the honest thing is to say so in the box rather than to leave a
    // spinner turning for ever.
    if spawned.is_err() {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send("A thread could not be started to take the reading.".to_owned());
        return rx;
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reporter that has been given an answer, without running one.
    fn ready(text: &str, at: Instant) -> Reporter {
        Reporter {
            state: State::Ready {
                text: text.into(),
                at,
            },
            job: None,
        }
    }

    /// Nothing asked for is a spinner, not an empty box.
    #[test]
    fn a_page_with_no_report_yet_shows_that_it_is_coming() {
        let reporter = Reporter::default();
        assert_eq!(reporter.view(), View::Waiting);
        assert_eq!(reporter.text(), "");
    }

    /// The whole point of the TTL: coming back inside it does not re-run.
    #[test]
    fn coming_back_to_the_page_within_the_minute_reuses_the_answer() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        reporter.wanted(true, &Settings::default(), now + TTL / 2, move || {
            flag.store(true, std::sync::atomic::Ordering::Relaxed)
        });
        assert!(reporter.job.is_none(), "a second run was started");
        assert!(!woken.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(reporter.view(), View::Ready("all fine"));
    }

    /// And after it, it does.
    #[test]
    fn an_answer_older_than_the_minute_is_taken_again() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        reporter.wanted(true, &Settings::default(), now + TTL * 2, || {});
        assert!(reporter.job.is_some(), "no second run was started");
        assert_eq!(reporter.view(), View::Waiting);
    }

    /// A page that does not want a report does not get one, however stale.
    #[test]
    fn a_page_that_wants_nothing_starts_nothing() {
        let now = Instant::now();
        let mut reporter = ready("all fine", now);
        reporter.wanted(false, &Settings::default(), now + TTL * 2, || {});
        assert!(reporter.job.is_none());
        assert_eq!(reporter.view(), View::Ready("all fine"));
    }

    /// The property the `Refreshing` state exists for: a refresh does not
    /// blank the page somebody is reading.
    #[test]
    fn refreshing_keeps_the_old_report_on_screen() {
        let mut reporter = ready("all fine", Instant::now());
        reporter.refresh(&Settings::default(), || {});
        assert_eq!(reporter.view(), View::Stale("all fine"));
        assert_eq!(reporter.text(), "all fine");
    }

    /// A refresh with nothing to keep is just a run.
    #[test]
    fn refreshing_from_nothing_is_a_plain_wait() {
        let mut reporter = Reporter::default();
        reporter.refresh(&Settings::default(), || {});
        assert_eq!(reporter.view(), View::Waiting);
    }

    /// An answer arriving replaces whatever was showing and stops the wait.
    #[test]
    fn an_answer_lands_and_the_spinner_stops() {
        let now = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut reporter = Reporter {
            state: State::Running,
            job: Some(rx),
        };
        reporter.poll(now);
        assert_eq!(reporter.view(), View::Waiting, "polled an empty channel");

        tx.send("a fresh reading".to_owned()).unwrap();
        reporter.poll(now);
        assert_eq!(reporter.view(), View::Ready("a fresh reading"));
        assert!(reporter.job.is_none(), "the finished job was kept");
    }

    /// A thread that dies without answering leaves the page able to ask
    /// again, rather than spinning for ever.
    #[test]
    fn a_thread_that_vanishes_does_not_spin_for_ever() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let mut reporter = Reporter {
            state: State::Running,
            job: Some(rx),
        };
        drop(tx);
        reporter.poll(Instant::now());
        assert!(reporter.job.is_none());
        // And asking again starts a new one.
        reporter.wanted(true, &Settings::default(), Instant::now(), || {});
        assert!(reporter.job.is_some());
    }
}
