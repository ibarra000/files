//! The thread that looks at the update folder now and then.
//!
//! One thread for the life of the process, started only when a folder is
//! configured, and it spends all but a few milliseconds of its life asleep.
//! That is the shape every other worker here has, and for the same reason:
//! [`crate::app::actors`] guarantees a fixed thread population, so nothing is
//! spawned per keystroke and thread growth is impossible by construction
//! rather than by discipline.
//!
//! # Why it sleeps on a channel rather than on the clock
//!
//! `recv_timeout` rather than `sleep`, so the thread is reachable while it
//! waits. It has to be for two reasons: somebody pressing "Check now" should
//! not wait out the rest of a four-hour nap, and quitting must not wait out
//! any of it. A thread parked in `sleep` can do neither.
//!
//! # Why the first look is late
//!
//! Reading the folder is an SMB round trip, and an SMB round trip to a share
//! that is not there is a timeout. Neither belongs in front of a window
//! somebody is waiting for, so the first look happens a minute in, by which
//! time the panel has long since been summoned, used and dismissed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded};

use crate::app::event::{AppEvent, Events, UpdateMsg};
use crate::update::{Version, look};

/// How long after startup the folder is first read.
///
/// Long enough to be well clear of the startup path, short enough that
/// somebody who leaves the program running all day is told the same morning.
const FIRST_LOOK: Duration = Duration::from_secs(60);

/// How often after that.
///
/// Four hours. A release happens a few times a year, so this is already far
/// more often than it needs to be; what sets it is the other end - a few
/// hundred copies reading one small file on a timer, where four hours is two
/// reads a working day each and invisible against anything else on the share.
const INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// What the thread can be asked to do.
enum Ask {
    /// Look now, because somebody asked.
    Now,
    Stop,
}

/// The update checker.
pub struct Checker {
    tx: Sender<Ask>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Checker {
    /// Starts looking, from a minute after now.
    ///
    /// `running` is passed in rather than read, so a test can be a version
    /// other than the one it happens to be compiled as.
    pub fn start(folder: PathBuf, running: Version, events: Events) -> std::io::Result<Self> {
        // One slot. Two "check now" presses a second apart are one question,
        // and the second may be dropped without anybody being misled - the
        // answer to the first is about to arrive.
        let (tx, rx) = bounded::<Ask>(1);
        let handle = std::thread::Builder::new()
            .name("files-update".into())
            .spawn(move || {
                let mut due = Instant::now() + FIRST_LOOK;
                loop {
                    let wait = due.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        // The sender is gone, which is shutdown by another
                        // name and must be treated as one: a thread that kept
                        // looking would hold the process open.
                        Ok(Ask::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            return;
                        }
                        Ok(Ask::Now) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    }

                    // Every outcome is reported, including the dull one. The
                    // settings window says when it last looked, and a check
                    // that answered nothing at all would leave that reading as
                    // though the thread had died.
                    let found = look(&folder, running);
                    if events
                        .send(AppEvent::Update(UpdateMsg::Looked(Box::new(found))))
                        .is_err()
                    {
                        return;
                    }
                    due = Instant::now() + INTERVAL;
                }
            })?;

        Ok(Self {
            tx,
            handle: Some(handle),
        })
    }

    /// Asks for a look now. Dropped if one is already queued.
    pub fn check_now(&self) {
        let _ = self.tx.try_send(Ask::Now);
    }

    /// Stops the thread and waits for it, briefly.
    ///
    /// Budgeted like every other join here. The worst this can be waiting on
    /// is one read of a small file, and unlike an abandoned index write there
    /// is nothing half-finished for it to leave behind - so a thread that
    /// misses the budget is abandoned rather than waited for.
    pub fn shutdown(&mut self, budget: Duration) -> bool {
        let _ = self.tx.try_send(Ask::Stop);
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if handle.is_finished() {
                return handle.join().is_ok();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Found;

    /// The cadence has to be sane in both directions: not so eager that a few
    /// hundred copies are a load on the share, not so lazy that somebody who
    /// never restarts is never told.
    #[test]
    fn the_first_look_is_well_clear_of_the_startup_path() {
        assert!(FIRST_LOOK >= Duration::from_secs(30));
        assert!(FIRST_LOOK < INTERVAL);
    }

    #[test]
    fn the_interval_is_hours_rather_than_minutes() {
        assert!(INTERVAL >= Duration::from_secs(60 * 60));
    }

    /// A thread that ignored a stop would hold the process open.
    #[test]
    fn asking_it_to_stop_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = bounded(16);
        let events = Events::headless(tx);
        let mut checker =
            Checker::start(dir.path().to_path_buf(), Version::new(0, 2, 0), events).unwrap();

        assert!(
            checker.shutdown(Duration::from_secs(5)),
            "the checker did not stop when asked"
        );
    }

    /// Nobody should wait out the first minute to find out where they stand.
    #[test]
    fn asking_for_a_look_now_does_not_wait_for_the_timer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::update::MANIFEST_NAME),
            "version = \"9.9.9\"\nmsi = \"files.msi\"\n",
        )
        .unwrap();

        let (tx, rx) = bounded(16);
        let events = Events::headless(tx);
        let mut checker =
            Checker::start(dir.path().to_path_buf(), Version::new(0, 2, 0), events).unwrap();
        checker.check_now();

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a look asked for now must answer now");
        match event {
            AppEvent::Update(UpdateMsg::Looked(found)) => match *found {
                Found::Available { manifest, .. } => {
                    assert_eq!(manifest.version, Version::new(9, 9, 9));
                }
                other => panic!("expected an available update, got {other:?}"),
            },
            other => panic!("expected a look, got {other:?}"),
        }
        checker.shutdown(Duration::from_secs(5));
    }

    /// Including the dull answer, or the window could not say when it last
    /// looked without that reading as though the thread had died.
    #[test]
    fn an_unreachable_folder_is_reported_rather_than_swallowed() {
        let (tx, rx) = bounded(16);
        let events = Events::headless(tx);
        let mut checker = Checker::start(
            PathBuf::from(r"C:\definitely-not-here-5521"),
            Version::new(0, 2, 0),
            events,
        )
        .unwrap();
        checker.check_now();

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("it must answer");
        let AppEvent::Update(UpdateMsg::Looked(found)) = event else {
            panic!("expected a look, got {event:?}");
        };
        assert!(matches!(*found, Found::Unavailable { .. }));
        checker.shutdown(Duration::from_secs(5));
    }
}
