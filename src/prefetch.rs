//! Speculative fetching of job folders.
//!
//! `paths::classify` resolves partial input, so the folder a user is heading
//! for is usually known several keystrokes before they finish typing. Warming
//! it in the background turns the first search from a network round trip into
//! a cache hit.
//!
//! The economics are strongly favourable: a wrong guess costs one small
//! directory listing, a right guess removes the entire search latency.
//!
//! # Not stampeding the share
//!
//! Concurrency is bounded **structurally**, by having exactly two worker
//! threads and a one-slot mailbox, rather than by a semaphore - one more
//! thing that could be held across a blocking syscall.
//!
//! De-duplication does most of the work for free. Typing `11-D-07`,
//! `11-D-070`, `11-D-0704` all resolve to `R:\11d`, so three keystrokes
//! become one network operation. In-flight requests are never cancelled:
//! the syscall cannot be interrupted anyway, and the result is still worth
//! caching.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use parking_lot::Mutex;

use crate::app::event::{AppEvent, PrefetchMsg};
use crate::index::jobs;
use crate::search::worker::Backend;
use crate::util::cancel::CancelToken;
use crate::util::latest_slot::LatestSlot;

/// Worker threads. Two rather than one so a slow stale fetch cannot
/// head-of-line block the newest guess; not more, because round trips to the
/// same share serialise server-side regardless.
const WORKERS: usize = 2;

/// Runs speculative listings.
pub struct Prefetcher {
    slot: Arc<LatestSlot<PathBuf>>,
    in_flight: Arc<Mutex<HashSet<PathBuf>>>,
    handles: Vec<JoinHandle<()>>,
}

impl Prefetcher {
    /// Requests `dir`, unless it is already being fetched.
    pub fn request(&self, dir: PathBuf) {
        if self.in_flight.lock().contains(&dir) {
            return;
        }
        self.slot.put(dir);
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.lock().len()
    }

    pub fn shutdown(&mut self, budget: Duration) -> bool {
        self.slot.close();
        let deadline = Instant::now() + budget;
        let mut clean = true;
        for handle in self.handles.drain(..) {
            while !handle.is_finished() {
                if Instant::now() >= deadline {
                    // Leaked deliberately; see the note in the search worker.
                    clean = false;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                clean &= handle.join().is_ok();
            }
        }
        clean
    }
}

/// Starts the prefetch pool.
pub fn spawn(backend: Arc<Backend>, events: Sender<AppEvent>) -> std::io::Result<Prefetcher> {
    let slot = Arc::new(LatestSlot::<PathBuf>::new());
    let in_flight = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = Vec::with_capacity(WORKERS);

    for i in 0..WORKERS {
        let slot = Arc::clone(&slot);
        let in_flight = Arc::clone(&in_flight);
        let backend = Arc::clone(&backend);
        let events = events.clone();
        handles.push(
            std::thread::Builder::new()
                .name(format!("files-prefetch-{i}"))
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        while let Some(dir) = slot.take_blocking() {
                            if !in_flight.lock().insert(dir.clone()) {
                                continue; // the other worker took it
                            }
                            let outcome = jobs::fetch(
                                backend.source.as_ref(),
                                &backend.store,
                                &dir,
                                false,
                                &CancelToken::never(),
                            );
                            in_flight.lock().remove(&dir);

                            // A warmed cache changes nothing on screen, so only
                            // failures are worth reporting.
                            match outcome {
                                Ok(_) => {
                                    let _ = events
                                        .try_send(AppEvent::Prefetch(PrefetchMsg::Ready { dir }));
                                }
                                Err(err) => {
                                    let _ =
                                        events.try_send(AppEvent::Prefetch(PrefetchMsg::Failed {
                                            dir,
                                            err,
                                        }));
                                }
                            }
                        }
                    }));
                    if result.is_err() {
                        let _ = events.send(AppEvent::ActorDied {
                            actor: "prefetch",
                            detail: "panicked".into(),
                        });
                    }
                })?,
        );
    }

    Ok(Prefetcher {
        slot,
        in_flight,
        handles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::index::fake_source::FakeDirSource;
    use crate::index::store::IndexStore;
    use crossbeam_channel::bounded;

    fn backend(src: FakeDirSource) -> Arc<Backend> {
        Arc::new(Backend {
            settings: Settings::default(),
            store: Arc::new(IndexStore::default()),
            source: Arc::new(src),
        })
    }

    fn settle(src: &FakeDirSource, dir: &str, want: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if src.list_count(dir) >= want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_requested_folder_is_fetched_and_cached() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["a.pdf"]);
        let b = backend(src.clone());
        let (tx, _rx) = bounded(64);
        let mut p = spawn(Arc::clone(&b), tx).unwrap();

        p.request(PathBuf::from("R:\\11d"));
        assert!(settle(&src, "R:\\11d", 1, Duration::from_secs(2)));

        // The subsequent real search finds it warm.
        let (_, source) = jobs::fetch(
            b.source.as_ref(),
            &b.store,
            std::path::Path::new("R:\\11d"),
            false,
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(
            source,
            jobs::Source::Cache,
            "the search should hit a warm cache"
        );
        p.shutdown(Duration::from_millis(500));
    }

    /// Typing a longer code resolves to the same folder each time; those
    /// keystrokes must not each become a network round trip.
    #[test]
    fn repeated_requests_for_the_same_folder_collapse() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["a.pdf"]);
        src.set_latency(Duration::from_millis(60));
        let (tx, _rx) = bounded(64);
        let mut p = spawn(backend(src.clone()), tx).unwrap();

        for _ in 0..10 {
            p.request(PathBuf::from("R:\\11d"));
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(400));

        assert!(
            src.list_count("R:\\11d") <= 2,
            "ten keystrokes became {} network calls",
            src.list_count("R:\\11d")
        );
        p.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_failed_prefetch_is_reported() {
        let src = FakeDirSource::new();
        let (tx, rx) = bounded(64);
        let mut p = spawn(backend(src), tx).unwrap();

        p.request(PathBuf::from("R:\\nope"));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw = false;
        while Instant::now() < deadline {
            if let Ok(AppEvent::Prefetch(PrefetchMsg::Failed { .. })) =
                rx.recv_timeout(Duration::from_millis(50))
            {
                saw = true;
                break;
            }
        }
        assert!(
            saw,
            "a missing folder should be explained, not silently ignored"
        );
        p.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_newer_guess_supersedes_a_queued_older_one() {
        let src = FakeDirSource::new()
            .with_dir("R:\\aaa", &["a.pdf"])
            .with_dir("R:\\bbb", &["b.pdf"])
            .with_dir("R:\\ccc", &["c.pdf"]);
        src.set_latency(Duration::from_millis(80));
        let (tx, _rx) = bounded(64);
        let mut p = spawn(backend(src.clone()), tx).unwrap();

        // With two workers, at most two of these can start; the rest are
        // replaced in the slot before anyone picks them up.
        p.request(PathBuf::from("R:\\aaa"));
        p.request(PathBuf::from("R:\\bbb"));
        p.request(PathBuf::from("R:\\ccc"));
        std::thread::sleep(Duration::from_millis(400));

        let total =
            src.list_count("R:\\aaa") + src.list_count("R:\\bbb") + src.list_count("R:\\ccc");
        assert!(
            total <= WORKERS,
            "queued guesses should be dropped, ran {total}"
        );
        p.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn concurrency_is_bounded_by_the_worker_count() {
        let src = FakeDirSource::new()
            .with_dir("R:\\aaa", &["a.pdf"])
            .with_dir("R:\\bbb", &["b.pdf"]);
        src.set_latency(Duration::from_millis(150));
        let (tx, _rx) = bounded(64);
        let mut p = spawn(backend(src), tx).unwrap();

        for name in ["R:\\aaa", "R:\\bbb", "R:\\aaa", "R:\\bbb"] {
            p.request(PathBuf::from(name));
        }
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            p.in_flight_count() <= WORKERS,
            "must never exceed the pool size"
        );
        p.shutdown(Duration::from_millis(600));
    }

    #[test]
    fn shutdown_is_prompt_when_idle() {
        let (tx, _rx) = bounded(64);
        let mut p = spawn(backend(FakeDirSource::new()), tx).unwrap();
        let started = Instant::now();
        assert!(p.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(400));
    }
}
