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

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::Sender;

use super::matcher::{self};
use super::verify::{SkipReason, Verifier, VerifyOutcome};
use crate::app::event::{AppEvent, SearchMsg, VerifyMsg};
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use crate::index::snapshot;
use crate::index::snapshot::Snapshot;
use crate::index::store::IndexStore;
use crate::index::tree::TreeIndex;
use crate::paths::TargetList;
use crate::util::cancel::Epoch;
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
    /// The flat index, or an empty listing until one exists.
    ///
    /// Takes no query and cannot fail. It used to do both: a code was routed
    /// to a folder, and the folder was listed over the network on the search
    /// thread. Every share is indexed now, so the listing is already in
    /// memory and there is nothing left to resolve or to wait for.
    pub fn flat(&self) -> Arc<Snapshot> {
        self.store.flat().unwrap_or_else(|| {
            Arc::new(Snapshot::empty(
                &self.settings.custpro_path.to_string_lossy(),
            ))
        })
    }

    /// The walked tree, once it holds anything.
    ///
    /// Separate from [`Self::flat`] because a tree is a different shape, not a
    /// different listing: it spans many directories and carries the folder
    /// names alongside the filenames. Forcing it through `Arc<Snapshot>` would
    /// mean flattening away exactly the structure that makes a folder-name
    /// match possible.
    pub fn tree(&self) -> Option<Arc<TreeIndex>> {
        self.store.tree()
    }

    /// Every share a query is searched against, in configuration order.
    pub fn targets(&self) -> TargetList {
        self.settings.routes.targets()
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

        // No network call and no failure path. Both indexes are already in
        // memory, so a search is a sweep over bytes this process owns - which
        // is why the "no such job folder" error this used to have to report
        // does not exist any more.
        let flat = matcher::search(
            &backend.flat(),
            &request.query,
            backend.settings.matcher,
            &cancel,
        );
        let result = match backend.tree() {
            // One merged, ranked list: which share a file came from is shown
            // on the row, but it must not decide where in the list it sits.
            Some(tree) => flat.and_then(|flat| {
                matcher::search_tree(&tree, &request.query, &cancel)
                    .map(|tree| matcher::merge(flat, tree))
            }),
            None => flat,
        };

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

        // Only a flat mapping is worth verifying against the server, and a
        // tree cannot be: `FindFirstFileExW` matches within one folder, so
        // verifying a tree would mean one round trip per folder the hits came
        // from - turning the one cheap round trip this exists for into
        // hundreds. Freshness for a tree comes from the change watcher and the
        // re-walk floor instead, and saying so is better than implying a check
        // that did not happen.
        let outcome = if backend.settings.custpro_path.as_os_str().is_empty() {
            VerifyOutcome::Skipped(SkipReason::NotApplicable)
        } else {
            let snapshot = backend.store.flat();
            verifier.verify(&request.query, snapshot.as_deref(), &cancel)
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
    use crate::paths::MappingKind;
    use crossbeam_channel::bounded;
    use std::time::Duration;

    fn backend(src: FakeDirSource) -> Arc<Backend> {
        backend_with(Settings::default(), src)
    }

    fn backend_with(settings: Settings, src: FakeDirSource) -> Arc<Backend> {
        Arc::new(Backend {
            settings,
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

    /// Every configured share is searched, whatever was typed.
    ///
    /// This used to be a routing test: a code had to match a pattern before
    /// anything would look for it, and a string matching none was refused
    /// outright with "not a recognised job code" on the status line. That is
    /// the behaviour being removed - an indexed share knows every file it
    /// holds, so an unusual query returns no matches rather than being
    /// declined.
    #[test]
    fn every_configured_share_is_a_target_whatever_the_query() {
        let b = backend(FakeDirSource::new());
        let targets = b.targets();
        assert_eq!(targets.len(), 2, "{targets:?}");
        assert!(targets.iter().any(|t| t.kind == MappingKind::Flat));
        assert!(targets.iter().any(|t| t.kind == MappingKind::Tree));
    }

    #[test]
    fn the_flat_root_is_served_from_the_index_without_touching_the_network() {
        let src = FakeDirSource::new().with_dir(crate::config::CUSTPRO_PATH, &["a.pdf"]);
        let b = backend(src.clone());
        assert!(b.flat().is_empty(), "no index published yet");
        assert!(
            src.calls().is_empty(),
            "the flat root must never be enumerated inline"
        );
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
    fn shutdown_completes_promptly_for_an_idle_worker() {
        let (tx, _rx) = bounded(64);
        let mut w = spawn_search(backend(FakeDirSource::new()), tx).unwrap();
        let started = Instant::now();
        assert!(w.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    /// A search cannot wedge, because it no longer touches the network.
    ///
    /// It used to: an unindexed job folder was listed inline on this thread,
    /// so a hung share hung the search worker and shutdown had to give up on
    /// it. Both shares are indexed now, so a search is a sweep over bytes this
    /// process already owns.
    #[test]
    fn a_search_never_reaches_the_share() {
        let src = FakeDirSource::new().with_dir("R:\\11d", &["a.pdf"]);
        src.set_hang(true);
        let (tx, rx) = bounded(64);
        let mut w = spawn_search(backend(src.clone()), tx).unwrap();
        w.submit(|epoch| SearchRequest {
            query: "11-D-0704".into(),
            epoch,
        });

        let events = drain(&rx, Duration::from_secs(2));
        assert!(
            events.iter().any(|e| matches!(e, AppEvent::Search(_))),
            "a search against a hung share should still answer, got {events:?}"
        );
        assert!(src.calls().is_empty(), "the search reached the network");
        assert!(w.shutdown(Duration::from_millis(500)));
    }
}
