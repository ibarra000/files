//! Long-lived search and verification workers.
//!
//! The previous implementation called `thread::spawn` once per search, with
//! no bound and no cancellation, and did the "fast path" inline on the UI
//! thread - where it deep-copied the entire listing before matching anything.
//!
//! Here there is exactly one search thread and one verification thread for
//! the life of the process, each fed by a [`LatestSlot`] so a burst of
//! keystrokes collapses to the most recent query. Nothing is spawned per
//! request, so thread growth and network stampedes are prevented structurally
//! rather than by discipline.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::Sender;

use super::matcher::{self, SearchOutcome};
use super::verify::{Verifier, VerifyOutcome};
use crate::app::event::{AppEvent, SearchMsg, VerifyMsg};
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use crate::index::errors::EnumError;
use crate::index::snapshot::Snapshot;
use crate::index::store::IndexStore;
use crate::index::{jobs, snapshot};
use crate::paths::{MappingKind, TargetList};
use crate::util::cancel::{CancelToken, Epoch};
use crate::util::latest_slot::LatestSlot;

/// A request to match `query`.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub epoch: u64,
}

/// Shared context every worker needs.
pub struct Backend {
    pub settings: Settings,
    pub store: Arc<IndexStore>,
    pub source: Arc<dyn DirSource>,
}

impl Backend {
    /// Resolves a query to the listing it should be matched against.
    ///
    /// The flat root is served entirely from the in-memory index; a job
    /// folder is fetched on demand, usually finding the prefetcher has
    /// already warmed it.
    pub fn snapshot_for(
        &self,
        query: &str,
        cancel: &CancelToken,
    ) -> Result<Arc<Snapshot>, EnumError> {
        // One flat index exists so far, so the first target is taken. Merging
        // several is the next stage; the routing table already returns them
        // all.
        let targets = self.targets_for(query);
        let Some(target) = targets.first() else {
            return Ok(Arc::new(Snapshot::empty("")));
        };
        match target.kind {
            MappingKind::Flat => Ok(self
                .store
                .flat()
                .unwrap_or_else(|| Arc::new(Snapshot::empty(&target.dir.to_string_lossy())))),
            MappingKind::JobFolder => jobs::fetch(
                self.source.as_ref(),
                &self.store,
                &target.dir,
                false,
                cancel,
            )
            .map(|(s, _)| s),
        }
    }

    /// Every target a query resolves to, in configuration order.
    pub fn targets_for(&self, query: &str) -> TargetList {
        self.settings.routes.classify(query)
    }

    /// The directory a query resolves to, for prefetching and diagnostics.
    pub fn dir_for(&self, query: &str) -> Option<PathBuf> {
        self.targets_for(query).first().map(|t| t.dir.clone())
    }
}

/// A spawned worker plus the slot used to feed it.
pub struct WorkerHandle<T> {
    pub slot: Arc<LatestSlot<T>>,
    pub epoch: Epoch,
    handle: Option<JoinHandle<()>>,
    name: &'static str,
}

impl<T> WorkerHandle<T> {
    /// Supersedes any pending request and returns the new generation.
    /// The generation is bumped and handed to `build` in one step, so a
    /// caller cannot submit a request stamped with a generation the worker
    /// will immediately discard as stale.
    pub fn submit(&self, build: impl FnOnce(u64) -> T) -> u64 {
        let generation = self.epoch.bump();
        self.slot.put(build(generation));
        generation
    }

    /// Submits a request stamped with a generation the caller already owns.
    ///
    /// The UI's query counter is the authority for what is current, so the
    /// worker adopts it rather than keeping a competing one. That is what
    /// lets a result be matched back to the keystroke that asked for it and
    /// cancelled by the same number.
    pub fn submit_generation(&self, generation: u64, request: T) {
        self.epoch.advance_to(generation);
        self.slot.put(request);
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Signals the worker to stop and waits up to `budget` for it.
    ///
    /// Returns false on timeout. A worker blocked inside an SMB syscall
    /// cannot be woken, so the caller exits rather than waiting for it.
    pub fn shutdown(&mut self, budget: std::time::Duration) -> bool {
        self.slot.close();
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let deadline = Instant::now() + budget;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                // Deliberately leaked: the process is about to exit, and the
                // index is written with temp-then-rename so nothing is left
                // half-updated.
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        handle.join().is_ok()
    }
}

/// Spawns the search worker.
pub fn spawn_search(
    backend: Arc<Backend>,
    tx: Sender<AppEvent>,
) -> std::io::Result<WorkerHandle<SearchRequest>> {
    let slot = Arc::new(LatestSlot::<SearchRequest>::new());
    let epoch = Epoch::new();

    let handle = {
        let slot = Arc::clone(&slot);
        let epoch = epoch.clone();
        std::thread::Builder::new()
            .name("files-search".into())
            .spawn(move || {
                // A panic here must not leave the UI spinning forever.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_search(&backend, &slot, &epoch, &tx);
                }));
                if let Err(payload) = result {
                    let _ = tx.send(AppEvent::ActorDied {
                        actor: "search",
                        detail: crate::util::once::panic_detail(&payload),
                    });
                }
            })?
    };

    Ok(WorkerHandle {
        slot,
        epoch,
        handle: Some(handle),
        name: "search",
    })
}

fn run_search(
    backend: &Backend,
    slot: &LatestSlot<SearchRequest>,
    epoch: &Epoch,
    tx: &Sender<AppEvent>,
) {
    while let Some(request) = slot.take_blocking() {
        let started = Instant::now();
        let cancel = epoch.token(request.epoch);

        // A request already superseded while queued costs nothing.
        if cancel.is_cancelled() {
            continue;
        }

        let snapshot = match backend.snapshot_for(&request.query, &cancel) {
            Ok(s) => s,
            Err(EnumError::Cancelled) => continue,
            Err(err) => {
                // Order matters. The empty result goes first, then the
                // reason: the state machine only adopts a specific
                // explanation once the query has settled into a completed,
                // empty search. Sending them the other way round would let
                // the generic "no matches among 0 files" overwrite
                // "no such job folder".
                let _ = tx.send(AppEvent::Search(SearchMsg {
                    epoch: request.epoch,
                    query: request.query.clone(),
                    elapsed: started.elapsed(),
                    result: Ok(SearchOutcome::default()),
                }));
                let _ = tx.send(AppEvent::Prefetch(crate::app::event::PrefetchMsg::Failed {
                    dir: backend.dir_for(&request.query).unwrap_or_default(),
                    err,
                }));
                continue;
            }
        };

        let result = matcher::search(&snapshot, &request.query, backend.settings.matcher, &cancel);

        let _ = tx.send(AppEvent::Search(SearchMsg {
            epoch: request.epoch,
            query: request.query,
            elapsed: started.elapsed(),
            result,
        }));
    }
}

/// Spawns the verification worker.
pub fn spawn_verify(
    backend: Arc<Backend>,
    verifier: Arc<Verifier>,
    tx: Sender<AppEvent>,
) -> std::io::Result<WorkerHandle<SearchRequest>> {
    let slot = Arc::new(LatestSlot::<SearchRequest>::new());
    let epoch = Epoch::new();

    let handle = {
        let slot = Arc::clone(&slot);
        let epoch = epoch.clone();
        std::thread::Builder::new()
            .name("files-verify".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_verify(&backend, &verifier, &slot, &epoch, &tx);
                }));
                if let Err(payload) = result {
                    let _ = tx.send(AppEvent::ActorDied {
                        actor: "verify",
                        detail: crate::util::once::panic_detail(&payload),
                    });
                }
            })?
    };

    Ok(WorkerHandle {
        slot,
        epoch,
        handle: Some(handle),
        name: "verify",
    })
}

fn run_verify(
    backend: &Backend,
    verifier: &Verifier,
    slot: &LatestSlot<SearchRequest>,
    epoch: &Epoch,
    tx: &Sender<AppEvent>,
) {
    while let Some(request) = slot.take_blocking() {
        let started = Instant::now();
        let cancel = epoch.token(request.epoch);
        if cancel.is_cancelled() {
            continue;
        }

        // Only a flat mapping is worth verifying against the server: a job
        // folder costs one round trip to list in full either way.
        let targets = backend.targets_for(&request.query);
        let Some(target) = targets.first() else {
            continue;
        };
        let outcome = match target.kind {
            MappingKind::Flat => {
                let snapshot = backend.store.flat();
                verifier.verify(&request.query, snapshot.as_deref(), &cancel)
            }
            MappingKind::JobFolder => {
                match jobs::fetch(
                    backend.source.as_ref(),
                    &backend.store,
                    &target.dir,
                    true,
                    &cancel,
                ) {
                    Ok(_) => VerifyOutcome::IndexAuthoritative { stamp: None },
                    Err(err) => VerifyOutcome::Failed(err),
                }
            }
        };

        let _ = tx.send(AppEvent::Verify(VerifyMsg {
            epoch: request.epoch,
            query: request.query,
            elapsed: started.elapsed(),
            outcome,
        }));
    }
}

/// Re-exported so callers do not need the snapshot module directly.
pub use snapshot::Snapshot as WorkerSnapshot;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fake_source::FakeDirSource;
    use crossbeam_channel::bounded;
    use std::time::Duration;

    fn backend(src: FakeDirSource) -> Arc<Backend> {
        Arc::new(Backend {
            settings: Settings::default(),
            store: Arc::new(IndexStore::default()),
            source: Arc::new(src),
        })
    }

    fn drain(rx: &crossbeam_channel::Receiver<AppEvent>, timeout: Duration) -> Vec<AppEvent> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(ev) => out.push(ev),
                Err(_) => {
                    if !out.is_empty() {
                        break;
                    }
                }
            }
        }
        out
    }

    #[test]
    fn resolves_a_job_query_to_its_folder() {
        let b = backend(FakeDirSource::new().with_dir("R:\\11d", &["a.pdf"]));
        assert_eq!(b.dir_for("11-D-0704"), Some(PathBuf::from("R:\\11d")));
    }

    #[test]
    fn resolves_a_custompro_query_to_the_flat_root() {
        let b = backend(FakeDirSource::new());
        assert_eq!(b.dir_for("P12345"), Some(Settings::default().custpro_path));
    }

    #[test]
    fn an_unresolvable_query_has_no_directory() {
        let b = backend(FakeDirSource::new());
        assert_eq!(b.dir_for("!!!"), None);
    }

    #[test]
    fn the_flat_root_is_served_from_the_index_without_touching_the_network() {
        let src = FakeDirSource::new().with_dir(crate::config::CUSTPRO_PATH, &["a.pdf"]);
        let b = backend(src.clone());
        let snapshot = b.snapshot_for("P12345", &CancelToken::never()).unwrap();
        assert!(snapshot.is_empty(), "no index published yet");
        assert!(
            src.calls().is_empty(),
            "the flat root must never be enumerated inline"
        );
    }

    #[test]
    fn a_job_query_fetches_and_then_reuses_the_cache() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["a.pdf", "b.pdf"]);
        let b = backend(src.clone());
        assert_eq!(
            b.snapshot_for("11-D-0704", &CancelToken::never())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            b.snapshot_for("11-D-0704", &CancelToken::never())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(src.list_count("R:\\11d"), 1);
    }

    #[test]
    fn the_search_worker_answers_a_request() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["alpha.pdf", "beta.pdf"]);
        let (tx, rx) = bounded(64);
        let mut w = spawn_search(backend(src), tx).unwrap();

        w.submit(|epoch| SearchRequest {
            query: "11-D-0704".into(),
            epoch,
        });

        let events = drain(&rx, Duration::from_secs(2));
        let found = events.iter().any(|e| matches!(e, AppEvent::Search(_)));
        assert!(found, "expected a search result, got {events:?}");
        w.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_burst_of_requests_collapses_to_the_last_one() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["alpha.pdf"]);
        // Slow enough that the worker is still busy while the burst arrives.
        src.set_latency(Duration::from_millis(120));
        let (tx, rx) = bounded(64);
        let mut w = spawn_search(backend(src), tx).unwrap();

        // Occupy the worker.
        w.submit(|epoch| SearchRequest {
            query: "11-D-0704".into(),
            epoch,
        });
        std::thread::sleep(Duration::from_millis(20));

        // Eight more land while it is blocked; only the last should survive.
        for _ in 0..8 {
            w.submit(|epoch| SearchRequest {
                query: "11-D-0704".into(),
                epoch,
            });
        }

        let events = drain(&rx, Duration::from_secs(5));
        let searches = events
            .iter()
            .filter(|e| matches!(e, AppEvent::Search(_)))
            .count();
        assert!(searches >= 1, "the final request must still be serviced");
        assert!(
            searches <= 3,
            "eight superseded requests should not each run, ran {searches}"
        );
        w.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_failed_job_fetch_reports_the_reason() {
        let src = FakeDirSource::new();
        let (tx, rx) = bounded(64);
        let mut w = spawn_search(backend(src), tx).unwrap();
        w.submit(|epoch| SearchRequest {
            query: "11-D-0704".into(),
            epoch,
        });

        let events = drain(&rx, Duration::from_secs(2));
        assert!(
            events.iter().any(|e| matches!(e, AppEvent::Prefetch(_))),
            "a missing folder should be explained, got {events:?}"
        );

        // The empty result must arrive first. The state machine only adopts a
        // specific explanation once the search has settled, so the reverse
        // order would let "no matches among 0 files" overwrite "no such job
        // folder".
        let search_at = events.iter().position(|e| matches!(e, AppEvent::Search(_)));
        let reason_at = events
            .iter()
            .position(|e| matches!(e, AppEvent::Prefetch(_)));
        assert!(
            search_at < reason_at,
            "the reason must land after the empty result, got {events:?}"
        );
        w.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn shutdown_completes_promptly_for_an_idle_worker() {
        let (tx, _rx) = bounded(64);
        let mut w = spawn_search(backend(FakeDirSource::new()), tx).unwrap();
        let started = Instant::now();
        assert!(w.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    #[test]
    fn shutdown_gives_up_on_a_wedged_worker_rather_than_hanging() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["a.pdf"]);
        src.set_hang(true);
        let (tx, _rx) = bounded(64);
        let mut w = spawn_search(backend(src), tx).unwrap();
        w.submit(|epoch| SearchRequest {
            query: "11-D-0704".into(),
            epoch,
        });
        std::thread::sleep(Duration::from_millis(50));

        let started = Instant::now();
        let clean = w.shutdown(Duration::from_millis(150));
        assert!(!clean, "a blocked syscall cannot be woken");
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "must not wait indefinitely"
        );
    }
}
