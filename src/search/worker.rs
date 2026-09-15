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
    /// Built once, here, rather than per request: each holds the per-share
    /// query floor and the failure count that switches it off, and both have
    /// to outlive any one keystroke to mean anything.
    pub live: Vec<Arc<LiveShare>>,
}

impl Backend {
    /// The listing of the mapping `path` belongs to, when it has one.
    ///
    /// Replaces a `flat()` accessor that returned the *first* flat share's
    /// listing whatever was being opened - so with a second share configured,
    /// a drawing set's sibling pages were looked for in the wrong share, and
    /// with none configured it returned an empty listing built around an empty
    /// path. `None` where the file belongs to a tree, which carries its own
    /// structure and is not a single directory's listing.
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

        // No network call and no failure path. Every index is already in
        // memory, so a search is a sweep over bytes this process owns - which
        // is why the "no such job folder" error this used to have to report
        // does not exist any more.
        //
        // The query is judged once, before any index is consulted. `TooShort`
        // and `ContainsNul` are properties of what was typed, not of a share,
        // so asking each one would give N copies of the same answer - and with
        // nothing indexed yet, no answer at all: a two-character query would
        // come back "no matches" instead of "type at least 3 characters".
        let result = match matcher::check_query(&request.query) {
            Err(reject) => Err(reject),
            Ok(()) => {
                let mut parts = Vec::new();
                let mut rejected = None;
                for slot in backend.store.searchable() {
                    // Checked between shares rather than only within one, so a
                    // superseded keystroke abandons the shares not yet reached
                    // instead of paying for all ten.
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
                        // Unreachable once `check_query` has passed - both
                        // rejections are properties of the query. Propagated
                        // rather than unwrapped so that stays true by
                        // construction if either function grows a third.
                        Err(reject) => {
                            rejected = Some(reject);
                            break;
                        }
                    }
                }
                // One merged, ranked list: which share a file came from is
                // shown on the row, but it must not decide where it sits.
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

        // Only a flat mapping is worth verifying against the server, and a
        // tree cannot be: `FindFirstFileExW` matches within one folder, so
        // verifying a tree would mean one round trip per folder the hits came
        // from - turning the one cheap round trip this exists for into
        // hundreds. Freshness for a tree comes from the change watcher and the
        // re-walk floor instead, and saying so is better than implying a check
        // that did not happen.
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
/// A third thread rather than a share of the verifier's, and the reason is
/// structural rather than tidiness. Each [`WorkerHandle`] is fed by a
/// `LatestSlot` holding **one** request, so with a flat share and a live share
/// configured together the verify and the live query fall due on the same
/// pause and would cancel each other, non-deterministically, depending on
/// which `put` landed second. They also have opposite budgets: a verification
/// may take ten seconds because the results are already on screen, whereas a
/// live answer *is* the results.
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
    // Before the first keystroke rather than on demand, so a code searched
    // yesterday is answered from memory by the *local* sweep today - which
    // runs first and would otherwise see an empty share until the round trip
    // came back.
    restore(backend);

    while let Some(request) = slot.take_blocking() {
        let cancel = epoch.token(request.epoch);
        if cancel.is_cancelled() {
            continue;
        }
        // Judged once, before any share is asked, for the reason the local
        // search judges it once: the rejections are properties of what was
        // typed, so asking each share would put N identical answers on the
        // wire.
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
            // Remembered before it is reported, so the next search for the
            // same code is answered from memory rather than from the wire.
            if let LiveOutcome::Answered { hits, .. } = &outcome {
                remember(backend, share, hits);
            }

            // One event per share rather than one for all of them: they answer
            // at different speeds, and holding the first until the last has
            // arrived would make two shares slower than one for no reason.
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

/// Reads back what earlier sessions of this share remembered.
///
/// Failure is silent and correct: there may be no cache, the format may have
/// moved, or the share may be one nobody has searched yet. Every one of those
/// is "start with nothing", which is what a live share does anyway.
fn restore(backend: &Backend) {
    let Some(cache) = backend.settings.cache_dir.as_deref() else {
        return;
    };
    for share in &backend.live {
        let Some(slot) = backend.store.slot(share.id()) else {
            continue;
        };
        let key = crate::index::persist::MappingKey::of(share.root());
        // The volume serial is left unknown rather than probed for. Resolving
        // it costs a round trip to a share that may be down, on the path that
        // exists so nothing touches it until somebody searches - and the
        // directory check inside `Expect` is what actually guards against
        // loading another share's index.
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
/// The whole point of a live share is that nothing reads it in the background,
/// so the only listing it will ever have is the one searches build. Keeping
/// what came back means a repeated code is answered from memory in about a
/// millisecond instead of a round trip - and, because the per-share floor
/// refuses a second query within the second, it is the difference between a
/// repeated search being instant and it being *skipped*.
///
/// Writes from this thread and no other. The store's single-writer rule is
/// about one writer per slot rather than about which thread it is, and a live
/// mapping gets no index actor: this worker is the only thing that will ever
/// publish here.
fn remember(backend: &Backend, share: &LiveShare, hits: &[Hit]) {
    if hits.is_empty() {
        return;
    }
    let Some(slot) = backend.store.slot(share.id()) else {
        return;
    };

    // Grouped by the folder each file sits in, because a directory listing is
    // the unit the index stores.
    let root = share.root();
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for hit in hits {
        let path = std::path::Path::new(hit.path.as_ref());
        let Some(parent) = path.parent() else {
            continue;
        };
        let rel = match parent.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('/', "\\"),
            // A hit from outside the share cannot be placed in its index, and
            // guessing where it belongs would be worse than dropping it.
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
    // `None` when every name was already held, which is the common case for a
    // code somebody searches twice - and costs no publication and no reader a
    // re-read.
    let Some(next) = current.with_observed_files(&seen) else {
        return;
    };
    slot.publish_observed(Arc::new(next.clone()));

    // Written every time the index actually grew, which the `None` above makes
    // rare: a code searched twice adds nothing the second time. There is no
    // spacing guard beyond that because there is nothing to pace - this is a
    // few kilobytes to a local disk, on a thread that has just spent a round
    // trip on the network.
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
        backend_with(Settings::default(), src)
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
                ..Settings::default()
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

    /// The payoff, and the reason writeback exists at all: the per-share floor
    /// refuses a second query within the second, so without a remembered
    /// listing a repeated code would not merely be slow, it would be *skipped*.
    #[test]
    fn a_code_searched_once_is_answered_from_memory_afterwards() {
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

        // The local sweep now finds it with the drive not asked at all.
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

    /// An observation proves those files exist and proves nothing whatever
    /// about the rest, so it must not move a clock only a pass could set.
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

    /// A hit from outside the share cannot be placed in its index, and
    /// guessing where it belongs would be worse than dropping it.
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
            store: Arc::new(IndexStore::default()),
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
        let src = FakeDirSource::new().with_dir("R:\\11d", &["alpha.pdf", "beta.pdf"]);
        let (tx, rx) = bounded(64);
        let tx = Events::headless(tx);
        let mut w = spawn_search(backend(src), tx).unwrap();

        w.submit(|epoch| SearchRequest {
            query: Query::contains("11-D-0704"),
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
        let tx = Events::headless(tx);
        let mut w = spawn_search(backend(src.clone()), tx).unwrap();
        w.submit(|epoch| SearchRequest {
            query: Query::contains("11-D-0704"),
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
