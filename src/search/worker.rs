//! Long-lived search, verification and live-search workers.
//!
//! Each worker is one thread for the life of the process, fed by a
//! [`LatestSlot`] so a burst of keystrokes collapses to the most recent query.
//! Nothing is spawned per request, which bounds thread count and network load.

use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use super::matcher::{self, Hit};
use super::verify::{SkipReason, Verifier, VerifyOutcome};
use crate::app::event::{AppEvent, Events, LiveMsg, SearchMsg, VerifyMsg};
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use crate::index::snapshot;
use crate::index::snapshot::Snapshot;
use crate::index::store::{IndexStore, SlotIndex};
use crate::index::tree::TreeIndex;
use crate::paths::{MappingKind, TargetList};
use crate::search::live::{LiveOutcome, LiveShare};
use crate::search::query::Query;
use crate::util::cancel::Epoch;
use crate::util::latest_slot::LatestSlot;

/// A request to match `query`.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: Query,
    pub epoch: u64,
}

/// Shared context every worker needs.
pub struct Backend {
    pub settings: Settings,
    pub store: Arc<IndexStore>,
    pub source: Arc<dyn DirSource>,
    /// Every share searched by asking the file server, in configuration order.
    ///
    /// Built once because each holds a per-share query floor and failure count
    /// that must outlive any single keystroke.
    pub live: Vec<Arc<LiveShare>>,
}

impl Backend {
    /// The listing of the mapping `path` belongs to, when it has one.
    ///
    /// `None` when the file belongs to a tree, which is not a single directory's
    /// listing.
    pub fn snapshot_for(&self, path: &Path) -> Option<Arc<Snapshot>> {
        self.store
            .indexed()
            .find(|s| crate::util::winpath::contains(s.dir(), path))
            .and_then(|s| s.as_flat())
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
    /// The generation is bumped and handed to `build` in one step, so a caller
    /// cannot submit a request with a generation the worker treats as stale.
    pub fn submit(&self, build: impl FnOnce(u64) -> T) -> u64 {
        let generation = self.epoch.bump();
        self.slot.put(build(generation));
        generation
    }

    /// Submits a request stamped with a generation the caller already owns.
    ///
    /// The UI's query counter is authoritative, so a result can be matched to
    /// the keystroke that requested it and cancelled by the same number.
    pub fn submit_generation(&self, generation: u64, request: T) {
        self.epoch.advance_to(generation);
        self.slot.put(request);
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Signals the worker to stop and waits up to `budget` for it.
    ///
    /// Returns false on timeout. A worker blocked inside an SMB syscall cannot
    /// be woken, so the caller exits rather than waiting for it.
    pub fn shutdown(&mut self, budget: std::time::Duration) -> bool {
        self.slot.close();
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let deadline = Instant::now() + budget;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                // Leaked on purpose: the process is about to exit, and the index is
                // written temp-then-rename so nothing is left half-updated.
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
    tx: Events,
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

fn run_search(backend: &Backend, slot: &LatestSlot<SearchRequest>, epoch: &Epoch, tx: &Events) {
    while let Some(request) = slot.take_blocking() {
        let started = Instant::now();
        let cancel = epoch.token(request.epoch);

        // A request already superseded while queued costs nothing.
        if cancel.is_cancelled() {
            continue;
        }

        // No network access: every index is already in memory.
        //
        // The query is judged once, before any index is consulted. `TooShort` and
        // `ContainsNul` are properties of the input, and with nothing indexed yet
        // per-share checks would report "no matches" instead of the real reason.
        let result = match matcher::check_query(&request.query) {
            Err(reject) => Err(reject),
            Ok(()) => {
                let mut parts = Vec::new();
                let mut rejected = None;
                for slot in backend.store.searchable() {
                    // Checked between shares so a superseded query skips the shares
                    // not yet reached.
                    if cancel.is_cancelled() {
                        break;
                    }
                    let Some(index) = slot.index() else { continue };
                    let part = match &*index {
                        SlotIndex::Flat(s) => matcher::search(
                            s,
                            &request.query,
                            backend.settings.matcher,
                            &backend.settings.hidden,
                            &cancel,
                        ),
                        SlotIndex::Tree(t) => matcher::search_tree(
                            t,
                            &request.query,
                            &backend.settings.hidden,
                            &cancel,
                        ),
                    };
                    match part {
                        Ok(part) => parts.push(part),
                        // Unreachable once `check_query` has passed, since both
                        // rejections are properties of the query. Propagated rather
                        // than unwrapped so this stays safe if a third is added.
                        Err(reject) => {
                            rejected = Some(reject);
                            break;
                        }
                    }
                }
                // One merged, ranked list: the source share is shown on the row
                // but does not affect ordering.
                match rejected {
                    Some(reject) => Err(reject),
                    None => Ok(matcher::merge_all(parts)),
                }
            }
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
    tx: Events,
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
    tx: &Events,
) {
    while let Some(request) = slot.take_blocking() {
        let started = Instant::now();
        let cancel = epoch.token(request.epoch);
        if cancel.is_cancelled() {
            continue;
        }

        // Only a flat mapping is verified. `FindFirstFileExW` matches within one
        // folder, so a tree would need a round trip per folder. Trees rely on the
        // change watcher and re-walk floor for freshness.
        let outcome = if verifier.is_empty() {
            VerifyOutcome::Skipped(SkipReason::NotApplicable)
        } else if verifier.len() > 1 {
            VerifyOutcome::Skipped(SkipReason::SeveralShares)
        } else {
            let snapshot = backend
                .store
                .indexed()
                .find(|s| s.kind() == MappingKind::Flat)
                .and_then(|s| s.as_flat());
            verifier.verify(
                &request.query,
                snapshot.as_deref(),
                &backend.settings.hidden,
                &cancel,
            )
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

/// Spawns the live-search worker.
///
/// A separate thread from the verifier because each [`WorkerHandle`]'s
/// `LatestSlot` holds one request: with a flat and a live share configured,
/// verify and live queries fall due together and would cancel each other.
/// Their budgets also differ: verification may take seconds because results
/// are already shown, whereas a live answer is the result.
pub fn spawn_live(
    backend: Arc<Backend>,
    tx: Events,
) -> std::io::Result<WorkerHandle<SearchRequest>> {
    let slot = Arc::new(LatestSlot::<SearchRequest>::new());
    let epoch = Epoch::new();

    let handle = {
        let slot = Arc::clone(&slot);
        let epoch = epoch.clone();
        std::thread::Builder::new()
            .name("files-live".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_live(&backend, &slot, &epoch, &tx);
                }));
                if let Err(payload) = result {
                    let _ = tx.send(AppEvent::ActorDied {
                        actor: "live",
                        detail: crate::util::once::panic_detail(&payload),
                    });
                }
            })?
    };

    Ok(WorkerHandle {
        slot,
        epoch,
        handle: Some(handle),
        name: "live",
    })
}

fn run_live(backend: &Backend, slot: &LatestSlot<SearchRequest>, epoch: &Epoch, tx: &Events) {
    // Loaded before the first keystroke so the local sweep, which runs
    // first, can answer previously searched terms from memory.
    restore(backend);

    while let Some(request) = slot.take_blocking() {
        let cancel = epoch.token(request.epoch);
        if cancel.is_cancelled() {
            continue;
        }
        // Judged once before any share is asked: the rejections are properties of
        // the input.
        if request.query.check().is_err() {
            continue;
        }

        for share in &backend.live {
            if cancel.is_cancelled() {
                break;
            }
            let started = Instant::now();
            let outcome = share.search(
                &request.query,
                &backend.settings.hidden,
                Instant::now(),
                &cancel,
            );
            // Remembered before it is reported, so a repeat search is answered
            // from memory.
            if let LiveOutcome::Answered { hits, .. } = &outcome {
                remember(backend, share, hits);
            }

            // One event per share, since shares answer at different speeds.
            let _ = tx.send(AppEvent::Live(LiveMsg {
                epoch: request.epoch,
                query: request.query.clone(),
                mapping: share.id(),
                elapsed: started.elapsed(),
                outcome: Box::new(outcome),
            }));
        }
    }
}

/// Reads back what earlier sessions remembered for this share.
///
/// Any failure (no cache, changed format, never searched) yields an empty
/// listing, which is the normal starting state for a live share.
fn restore(backend: &Backend) {
    let Some(cache) = backend.settings.cache_dir.as_deref() else {
        return;
    };
    for share in &backend.live {
        let Some(slot) = backend.store.slot(share.id()) else {
            continue;
        };
        let key = crate::index::persist::MappingKey::of(share.root());
        // The volume serial is left unknown: probing it costs a round trip to a
        // share that may be down. The directory check inside `Expect` guards
        // against loading another share's index.
        let expect = crate::index::persist::Expect::new(share.root(), None);
        if let Ok(loaded) = crate::index::persist::load_tree(cache, key, expect)
            && loaded.index.observed()
        {
            slot.publish_observed(Arc::new(loaded.index));
        }
    }
}

/// Folds what a live pass found into that share's index.
///
/// Nothing reads a live share in the background, so search results are its
/// only listing. Keeping them lets a repeated search be answered from memory;
/// without it the per-share floor would skip a repeat query within the
/// second.
///
/// Only this worker publishes to a live mapping's slot, which satisfies the
/// store's one-writer-per-slot rule; live mappings have no index actor.
fn remember(backend: &Backend, share: &LiveShare, hits: &[Hit]) {
    if hits.is_empty() {
        return;
    }
    let Some(slot) = backend.store.slot(share.id()) else {
        return;
    };

    // Grouped by folder, because a directory listing is the unit the index
    // stores.
    let root = share.root();
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for hit in hits {
        let path = std::path::Path::new(hit.path.as_ref());
        let Some(parent) = path.parent() else {
            continue;
        };
        let rel = match parent.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('/', "\\"),
            // A hit from outside the share cannot be placed in its index, so it
            // is dropped.
            Err(_) => continue,
        };
        let name = hit.name.to_string();
        match seen.iter_mut().find(|(d, _)| *d == rel) {
            Some((_, files)) => files.push(name),
            None => seen.push((rel, vec![name])),
        }
    }

    let current = slot
        .as_tree()
        .unwrap_or_else(|| Arc::new(TreeIndex::empty(&root.to_string_lossy())));
    // `None` when every name was already held, the common case for a repeat
    // search; no publication occurs.
    let Some(next) = current.with_observed_files(&seen) else {
        return;
    };
    slot.publish_observed(Arc::new(next.clone()));

    // Saved whenever the index grew. No further pacing is needed: the write
    // is a few kilobytes to local disk after a network round trip.
    if backend.settings.persist
        && let Some(cache) = backend.settings.cache_dir.as_deref()
    {
        let key = crate::index::persist::MappingKey::of(share.root());
        let _ = crate::index::persist::save_tree(cache, key, &next, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fake_source::FakeDirSource;
    use crate::paths::MappingKind;
    use crossbeam_channel::bounded;
    use std::time::Duration;

    fn backend(src: FakeDirSource) -> Arc<Backend> {
        backend_with(Settings::fixture(), src)
    }

    // --- remembering what a search found ------------------------------------

    const LIVE_ROOT: &str = "V:\\archive";

    fn live_backend(src: FakeDirSource) -> Arc<Backend> {
        let source: Arc<dyn DirSource> = Arc::new(src);
        let share = Arc::new(crate::search::live::LiveShare::new(
            crate::paths::MappingId(0),
            std::path::PathBuf::from(LIVE_ROOT),
            1,
            Vec::new(),
            Arc::clone(&source),
        ));
        Arc::new(Backend {
            settings: Settings {
                // Nothing here should touch the disk; the format round trip is
                // covered where the format is.
                persist: false,
                ..Settings::fixture()
            },
            store: Arc::new(IndexStore::single(
                "archive",
                Path::new(LIVE_ROOT),
                MappingKind::Live,
            )),
            source,
            live: vec![share],
        })
    }

    fn ask_and_remember(backend: &Backend, line: &str) {
        let share = &backend.live[0];
        let outcome = share.search(
            &crate::search::query::Query::parse(line),
            &backend.settings.hidden,
            Instant::now(),
            &crate::util::cancel::CancelToken::never(),
        );
        if let LiveOutcome::Answered { hits, .. } = &outcome {
            remember(backend, share, hits);
        }
    }

    /// The per-share floor refuses a second query within the second, so a
    /// repeated search must be answered from the remembered listing.
    #[test]
    fn a_term_searched_once_is_answered_from_memory_afterwards() {
        let backend = live_backend(FakeDirSource::new().with_dir(LIVE_ROOT, &["p12345.pdf"]));
        ask_and_remember(&backend, "p12345");

        let slot = backend
            .store
            .slot(crate::paths::MappingId(0))
            .expect("the slot exists");
        let index = slot.as_tree().expect("the share remembered something");
        assert!(index.observed());
        assert_eq!(index.len(), 1);
        assert_eq!(
            slot.status().origin,
            Some(crate::index::store::Origin::ServerObserved),
            "the variant that had sat unused since it was written"
        );

        // The local sweep now finds it without asking the drive.
        let found = matcher::search_tree(
            &index,
            &crate::search::query::Query::parse("p12345"),
            &backend.settings.hidden,
            &crate::util::cancel::CancelToken::never(),
        )
        .unwrap();
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].path.as_ref(), "V:\\archive\\p12345.pdf");
    }

    /// An observation proves only that those files exist, so it must not move a
    /// clock that only a full pass may set.
    #[test]
    fn remembering_never_claims_the_share_was_read() {
        let backend = live_backend(FakeDirSource::new().with_dir(LIVE_ROOT, &["p12345.pdf"]));
        ask_and_remember(&backend, "p12345");

        let status = backend
            .store
            .slot(crate::paths::MappingId(0))
            .unwrap()
            .status();
        assert!(status.built_at.is_none(), "a search is not a pass");
        assert!(status.confirmed_at.is_none(), "nothing was confirmed");
        assert!(
            !status.health.is_ok(),
            "a share serving only what was searched for has to say so"
        );
    }

    #[test]
    fn a_pass_that_found_nothing_publishes_nothing() {
        let backend = live_backend(FakeDirSource::new().with_dir(LIVE_ROOT, &["other.pdf"]));
        ask_and_remember(&backend, "p12345");
        assert!(
            backend
                .store
                .slot(crate::paths::MappingId(0))
                .unwrap()
                .as_tree()
                .is_none()
        );
    }

    /// A hit from outside the share cannot be placed in its index and is
    /// dropped.
    #[test]
    fn a_hit_from_outside_the_share_is_dropped_rather_than_misfiled() {
        let backend = live_backend(FakeDirSource::new().with_dir(LIVE_ROOT, &["p12345.pdf"]));
        let stray = vec![Hit {
            path: Arc::from("Z:\\elsewhere\\p12345.pdf"),
            name: Arc::from("p12345.pdf"),
            match_pos: 0,
            index: 0,
        }];
        remember(&backend, &backend.live[0], &stray);
        assert!(
            backend
                .store
                .slot(crate::paths::MappingId(0))
                .unwrap()
                .as_tree()
                .is_none()
        );
    }

    fn backend_with(settings: Settings, src: FakeDirSource) -> Arc<Backend> {
        Arc::new(Backend {
            settings,
            store: Arc::new(IndexStore::fixture()),
            source: Arc::new(src),
            live: Vec::new(),
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
        let src = FakeDirSource::new().with_dir(crate::config::FLAT_FIXTURE_PATH, &["a.pdf"]);
        let b = backend(src.clone());
        assert!(
            b.store.indexed().all(|s| !s.has_index()),
            "no index published yet"
        );
        assert!(
            src.calls().is_empty(),
            "the flat root must never be enumerated inline"
        );
    }

    #[test]
    fn the_search_worker_answers_a_request() {
        let src = FakeDirSource::new().with_dir("R:\\abc", &["alpha.pdf", "beta.pdf"]);
        let (tx, rx) = bounded(64);
        let tx = Events::headless(tx);
        let mut w = spawn_search(backend(src), tx).unwrap();

        w.submit(|epoch| SearchRequest {
            query: Query::contains("AB-C-0704"),
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
        let tx = Events::headless(tx);
        let mut w = spawn_search(backend(FakeDirSource::new()), tx).unwrap();
        let started = Instant::now();
        assert!(w.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    /// A search never touches the network, so a hung share cannot block it.
    #[test]
    fn a_search_never_reaches_the_share() {
        let src = FakeDirSource::new().with_dir("R:\\abc", &["a.pdf"]);
        src.set_hang(true);
        let (tx, rx) = bounded(64);
        let tx = Events::headless(tx);
        let mut w = spawn_search(backend(src.clone()), tx).unwrap();
        w.submit(|epoch| SearchRequest {
            query: Query::contains("AB-C-0704"),
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
