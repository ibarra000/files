//! The periodic update-check thread.
//!
//! One thread for the life of the process, started only when a source is
//! configured, per the fixed thread population in [`crate::app::actors`].
//!
//! It waits with `recv_timeout` rather than `sleep` so "Check now" and
//! shutdown take effect immediately. The first check is delayed to keep a
//! possible SMB timeout off the startup path.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::Source;

use crossbeam_channel::{Sender, bounded};

use crate::app::event::{AppEvent, Events, UpdateMsg};
use crate::update::{Version, look_at};

/// Delay before the first check after startup.
const FIRST_LOOK: Duration = Duration::from_secs(60);

/// Interval between checks; keeps load on a shared folder negligible across
/// hundreds of installs.
const INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// Requests to the checker thread.
enum Ask {
    /// Check immediately.
    Now,
    Stop,
}

/// The update checker.
pub struct Checker {
    tx: Sender<Ask>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Checker {
    /// Starts the checker; the first check runs after `FIRST_LOOK`.
    ///
    /// `cache_dir` receives a downloaded GitHub installer.
    pub fn start(
        source: Source,
        running: Version,
        cache_dir: Option<PathBuf>,
        events: Events,
    ) -> std::io::Result<Self> {
        // One slot: a second "check now" while one is queued is redundant.
        let (tx, rx) = bounded::<Ask>(1);
        let handle = std::thread::Builder::new()
            .name("files-update".into())
            .spawn(move || {
                let mut due = Instant::now() + FIRST_LOOK;
                loop {
                    let wait = due.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        // A dropped sender also means shutdown.
                        Ok(Ask::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            return;
                        }
                        Ok(Ask::Now) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    }

                    // Every outcome is reported, so the settings window can
                    // show when the last check ran.
                    let found = look_at(&source, running, cache_dir.as_deref());
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

    /// Requests an immediate check; ignored if one is already queued.
    pub fn check_now(&self) {
        let _ = self.tx.try_send(Ask::Now);
    }

    /// Stops the thread, waiting up to `budget`.
    ///
    /// A thread that misses the budget is abandoned; it has no partial state
    /// to leave behind.
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

    #[test]
    fn the_first_look_is_well_clear_of_the_startup_path() {
        assert!(FIRST_LOOK >= Duration::from_secs(30));
        assert!(FIRST_LOOK < INTERVAL);
    }

    #[test]
    fn the_interval_is_hours_rather_than_minutes() {
        assert!(INTERVAL >= Duration::from_secs(60 * 60));
    }

    #[test]
    fn asking_it_to_stop_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = bounded(16);
        let events = Events::headless(tx);
        let mut checker = Checker::start(
            Source::Folder(dir.path().to_path_buf()),
            Version::new(0, 2, 0),
            None,
            events,
        )
        .unwrap();

        assert!(
            checker.shutdown(Duration::from_secs(5)),
            "the checker did not stop when asked"
        );
    }

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
        let mut checker = Checker::start(
            Source::Folder(dir.path().to_path_buf()),
            Version::new(0, 2, 0),
            None,
            events,
        )
        .unwrap();
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

    #[test]
    fn an_unreachable_folder_is_reported_rather_than_swallowed() {
        let (tx, rx) = bounded(16);
        let events = Events::headless(tx);
        let mut checker = Checker::start(
            Source::Folder(PathBuf::from(r"C:\definitely-not-here-5521")),
            Version::new(0, 2, 0),
            None,
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
