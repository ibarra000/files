//! Tree walks driven through the index actor on real threads.
//!
//! Covers the wiring between walk, schedule, store, cache and watcher, so it
//! uses real threads and a compressed cadence rather than a simulated clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use files::app::event::AppEvent;
use files::config::Settings;
use files::config::hidden::Hidden;
use files::index::actor::{self, IndexContext};
use files::index::errors::EnumError;
use files::index::fake_source::FakeDirSource;
use files::index::schedule::Cadence;
use files::index::store::{Activity, DegradeReason, Health, IndexStore, Origin};
use files::paths::{MappingId, MappingKind};
use files::search::matcher;
use files::util::cancel::CancelToken;

const TREE_ROOT: &str = "R:\\";

/// A fake share with a flat directory and a tree, including a file in an
/// unconventionally named folder.
fn share() -> FakeDirSource {
    FakeDirSource::new()
        .with_dir(files::config::CUSTPRO_PATH, &["p12345 cover.pdf"])
        .with_tree(
            TREE_ROOT,
            &[
                "11d\\0704\\quote.pdf",
                "11d\\0704\\drawing.pdf",
                "ab12\\spec.pdf",
                "archive\\2019\\odd name\\11-3-0704 survey.pdf",
            ],
        )
}

/// Settings with a single tree mapping at `TREE_ROOT` and persistence off.
fn settings() -> Settings {
    Settings {
        persist: false,
        ..Settings::for_mapping("jobs", TREE_ROOT, MappingKind::Tree)
    }
}

/// A flat mapping and a tree mapping in one store, in that order, so an
/// actor owning `MappingId(1)` can be checked against `MappingId(0)`.
fn mixed_store() -> Arc<IndexStore> {
    Arc::new(IndexStore::for_routes(
        &files::paths::Routes::new(
            vec![
                files::paths::Mapping {
                    id: MappingId(0),
                    name: "custompro".into(),
                    path: "V:\\custpro".into(),
                    kind: MappingKind::Flat,
                    enabled: true,
                    refresh: Default::default(),
                    depth: files::config::DEFAULT_LIVE_DEPTH,
                },
                files::paths::Mapping {
                    id: MappingId(1),
                    name: "jobs".into(),
                    path: TREE_ROOT.into(),
                    kind: MappingKind::Tree,
                    enabled: true,
                    refresh: Default::default(),
                    depth: files::config::DEFAULT_LIVE_DEPTH,
                },
            ],
            files::paths::ConfigSource::BuiltIn,
        ),
        16,
    ))
}

fn tree_store() -> Arc<IndexStore> {
    Arc::new(IndexStore::single(
        "jobs",
        std::path::Path::new(TREE_ROOT),
        MappingKind::Tree,
    ))
}

/// Returns an event sink drained on its own thread, so the actor's `try_send`
/// never blocks.
fn draining_events() -> files::app::event::Events {
    let (tx, rx) = bounded::<AppEvent>(256);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    // No UI to wake.
    files::app::event::Events::headless(tx)
}

fn tree_actor(src: FakeDirSource, store: Arc<IndexStore>) -> actor::IndexActor {
    let ctx = IndexContext::new(settings(), MappingId(0), store, Arc::new(src))
        .with_cadence(Cadence::fast())
        .with_seed(7);
    actor::spawn(ctx, draining_events()).expect("the actor starts")
}

fn wait_for(
    store: &IndexStore,
    mut done: impl FnMut(&IndexStore) -> bool,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if done(store) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done(store)
}

fn found(store: &IndexStore, query: &str) -> Vec<String> {
    let Some(tree) = store.first_tree_slot().as_tree() else {
        return Vec::new();
    };
    matcher::search_tree(
        &tree,
        &files::search::query::Query::contains(query),
        &Hidden::none(),
        &CancelToken::never(),
    )
    .map(|o| o.hits.iter().map(|h| h.path.to_string()).collect())
    .unwrap_or_default()
}

/// The actor walks the tree, publishes it, and a nested file is found by code.
#[test]
fn the_actor_walks_the_tree_and_the_file_becomes_findable() {
    let store = tree_store();
    let mut actor = tree_actor(share(), Arc::clone(&store));

    assert!(
        wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some(),
            Duration::from_secs(5)
        ),
        "the tree should have been walked and published"
    );
    assert_eq!(
        found(&store, "11-3-0704"),
        vec!["R:\\archive\\2019\\odd name\\11-3-0704 survey.pdf"]
    );
    actor.shutdown(Duration::from_millis(500));
}

/// A code matching a folder name finds the files inside it.
#[test]
fn a_code_naming_a_folder_finds_its_contents_through_the_actor() {
    let store = tree_store();
    let mut actor = tree_actor(share(), Arc::clone(&store));
    assert!(wait_for(
        &store,
        |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
        Duration::from_secs(5)
    ));

    let mut got = found(&store, "0704");
    got.sort();
    assert_eq!(
        got,
        vec![
            "R:\\11d\\0704\\drawing.pdf",
            "R:\\11d\\0704\\quote.pdf",
            // Matched on its own name, which contains `0704`.
            "R:\\archive\\2019\\odd name\\11-3-0704 survey.pdf",
        ]
    );
    // Folder matches rank behind the name match.
    assert!(
        found(&store, "0704")[0].ends_with("11-3-0704 survey.pdf"),
        "a name match should outrank a folder match"
    );
    actor.shutdown(Duration::from_millis(500));
}

/// A tree actor writes only its own slot; the store requires one writer per
/// slot.
#[test]
fn the_tree_actor_leaves_the_flat_index_alone() {
    let store = mixed_store();
    // Owns the tree, which is mapping 1.
    let ctx = IndexContext::new(
        settings(),
        MappingId(1),
        Arc::clone(&store),
        Arc::new(share()),
    )
    .with_cadence(Cadence::fast())
    .with_seed(7);
    let mut actor = actor::spawn(ctx, draining_events()).expect("the actor starts");
    assert!(wait_for(
        &store,
        |s| s.first_tree_slot().as_tree().is_some(),
        Duration::from_secs(5)
    ));

    let flat = store.slot(MappingId(0)).unwrap();
    assert!(flat.as_flat().is_none(), "the flat index was never built");
    assert_eq!(flat.status().entries, 0, "nor its status touched");
    assert!(
        flat.status().health.is_ok(),
        "a tree actor must not degrade another mapping: {:?}",
        flat.status().health
    );
    assert!(store.slot(MappingId(1)).unwrap().status().entries > 0);
    actor.shutdown(Duration::from_millis(500));
}

/// An unreadable folder degrades the tree and is named in its coverage report.
#[test]
fn an_unreadable_folder_degrades_the_tree_and_names_itself() {
    let src = share();
    src.fail_dir("R:\\11d\\0704", EnumError::AccessDenied(5));
    let store = tree_store();
    let mut actor = tree_actor(src, Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| matches!(
            s.first_tree_slot().status().health,
            Health::Degraded {
                reason: DegradeReason::PartiallyUnreadable,
                ..
            }
        ),
        Duration::from_secs(5)
    ));

    let status = store.first_tree_slot().status();
    let coverage = status.coverage.as_ref().expect("coverage is reported");
    assert_eq!(coverage.holes, 1);
    assert!(coverage.examples.iter().any(|e| e == "11d\\0704"));
    // The rest of the share is still searchable.
    assert_eq!(found(&store, "spec"), vec!["R:\\ab12\\spec.pdf"]);
    actor.shutdown(Duration::from_millis(500));
}

/// A folder deleted during the walk counts as vanished, not as a fault.
#[test]
fn a_folder_that_vanished_mid_walk_does_not_degrade_the_tree() {
    let src = share();
    src.fail_dir("R:\\11d\\0704", EnumError::PathNotFound(3));
    let store = tree_store();
    let mut actor = tree_actor(src, Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.first_tree_slot().status().coverage.is_some(),
        Duration::from_secs(5)
    ));
    let status = store.first_tree_slot().status();
    assert!(status.health.is_ok(), "{:?}", status.health);
    assert_eq!(status.coverage.as_ref().unwrap().vanished, 1);
    assert_eq!(status.coverage.as_ref().unwrap().holes, 0);
    actor.shutdown(Duration::from_millis(500));
}

/// The actor settles rather than re-walking on every wake.
///
/// Guards against the actor checking the wrong slot for an existing snapshot,
/// which would make every wake a first run.
#[test]
fn the_tree_settles_instead_of_walking_continuously() {
    let src = share();
    let store = tree_store();
    let mut actor = tree_actor(src.clone(), Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.first_tree_slot().as_tree().is_some(),
        Duration::from_secs(5)
    ));
    // `Cadence::fast` scales an hour to 400ms, so a runaway would show here.
    let after_first = src.list_count(TREE_ROOT);
    std::thread::sleep(Duration::from_millis(200));
    let after_settling = src.list_count(TREE_ROOT);

    assert!(
        after_settling - after_first <= 2,
        "the root was re-listed {} times while idle",
        after_settling - after_first
    );
    actor.shutdown(Duration::from_millis(500));
}

/// A healthy tree is not reported as degraded.
///
/// A tree captures no directory stamp by design, unlike a flat scan, where a
/// missing stamp means degraded change detection.
#[test]
fn a_healthy_tree_is_not_reported_as_degraded() {
    let store = tree_store();
    let mut actor = tree_actor(share(), Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.first_tree_slot().status().coverage.is_some(),
        Duration::from_secs(5)
    ));
    let status = store.first_tree_slot().status();
    assert!(
        status.health.is_ok(),
        "a clean walk left the tree {:?}",
        status.health
    );
    actor.shutdown(Duration::from_millis(500));
}

/// Shutdown interrupts a running walk instead of leaking its thread.
#[test]
fn shutdown_interrupts_a_running_walk() {
    let paths: Vec<String> = (0..400).map(|i| format!("d{i:03}\\f.pdf")).collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree(TREE_ROOT, &refs);
    // Per-directory latency keeps the walk running during shutdown.
    src.set_latency(Duration::from_millis(5));

    let store = tree_store();
    let mut actor = tree_actor(src, Arc::clone(&store));
    assert!(
        wait_for(
            &store,
            |s| matches!(
                s.first_tree_slot().status().activity,
                Activity::Walking { .. }
            ),
            Duration::from_secs(5)
        ),
        "the walk should be in flight"
    );

    let started = Instant::now();
    assert!(
        actor.shutdown(Duration::from_millis(500)),
        "shutdown timed out against a running walk"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
}

/// Partial results are searchable while the walk is still running.
#[test]
fn the_tree_is_searchable_before_the_walk_finishes() {
    let paths: Vec<String> = (0..400)
        .map(|i| format!("d{i:03}\\0704 file.pdf"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree(TREE_ROOT, &refs);
    src.set_latency(Duration::from_millis(2));

    let store = tree_store();
    let mut actor = tree_actor(src, Arc::clone(&store));

    let searchable_mid_walk = wait_for(
        &store,
        |s| {
            matches!(
                s.first_tree_slot().status().activity,
                Activity::Walking { .. }
            ) && s.first_tree_slot().as_tree().is_some_and(|t| !t.is_empty())
        },
        Duration::from_secs(5),
    );
    assert!(
        searchable_mid_walk,
        "nothing was searchable until the walk had finished"
    );
    actor.shutdown(Duration::from_millis(500));
}

// --- persistence -----------------------------------------------------------

/// As [`settings`], but writing and reading a cache under `cache`.
fn persisting_settings(cache: &std::path::Path) -> Settings {
    Settings {
        persist: true,
        cache_dir: Some(cache.to_path_buf()),
        ..Settings::for_mapping("jobs", TREE_ROOT, MappingKind::Tree)
    }
}

fn persisting_tree_actor(
    src: FakeDirSource,
    store: Arc<IndexStore>,
    cache: &std::path::Path,
) -> actor::IndexActor {
    let ctx = IndexContext::new(
        persisting_settings(cache),
        MappingId(0),
        store,
        Arc::new(src),
    )
    .with_cadence(Cadence::fast())
    .with_seed(7);
    actor::spawn(ctx, draining_events()).expect("the actor starts")
}

/// Returns true once a `.idx` cache file exists under `cache`.
fn cached(cache: &std::path::Path) -> bool {
    std::fs::read_dir(cache).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".idx"))
    })
}

/// A fresh cache is trusted on launch: the share is not enumerated at all.
///
/// A tree has no directory stamp, so the cache's age must count against the
/// re-walk floor without one; otherwise every launch on every client re-walks
/// the whole share.
#[test]
fn a_warm_tree_does_not_re_walk_the_share() {
    let cache = tempfile::tempdir().unwrap();

    // First launch builds the cache.
    let first = tree_store();
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(
        wait_for(&first, |_| cached(cache.path()), Duration::from_secs(5)),
        "the first launch must write a cache to read back"
    );
    actor.shutdown(Duration::from_millis(500));

    // Second launch with the same cache and a source that records calls.
    let src = share();
    let second = tree_store();
    let mut actor = persisting_tree_actor(src.clone(), Arc::clone(&second), cache.path());
    assert!(
        wait_for(
            &second,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Duration::from_secs(5)
        ),
        "the cache should have been restored"
    );

    // Long enough that a walk would have started and been recorded.
    std::thread::sleep(Duration::from_millis(300));
    let listed: Vec<_> = src
        .calls()
        .into_iter()
        .filter(|c| matches!(c, files::index::fake_source::Call::List(_)))
        .collect();
    actor.shutdown(Duration::from_millis(500));

    assert!(
        listed.is_empty(),
        "a fresh cache must be believed; the share was read {} times: {listed:?}",
        listed.len()
    );
    assert_eq!(
        second.first_tree_slot().status().origin,
        Some(Origin::DiskCache),
        "and the results must still be the ones off the disk"
    );
}

/// On an `Auto` tree, a cache older than the floor still triggers a walk.
///
/// Trees default to `Manual`, so this opts in to `Auto`.
#[test]
fn an_auto_tree_whose_cache_is_past_the_floor_still_walks() {
    let cache = tempfile::tempdir().unwrap();

    let first = tree_store();
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(wait_for(
        &first,
        |_| cached(cache.path()),
        Duration::from_secs(5)
    ));
    actor.shutdown(Duration::from_millis(500));

    // `Cadence::fast()` compresses the floor to milliseconds, so the restored
    // cache is already past it when the second actor starts.
    std::thread::sleep(Duration::from_millis(50));

    let src = share();
    let second = tree_store();
    let ctx = IndexContext::new(
        auto_refresh(persisting_settings(cache.path())),
        MappingId(0),
        Arc::clone(&second),
        Arc::new(src.clone()),
    )
    .with_cadence(Cadence::fast())
    .with_seed(7);
    let mut actor = actor::spawn(ctx, draining_events()).expect("the actor starts");
    let walked = wait_for(
        &second,
        |_| {
            src.calls()
                .iter()
                .any(|c| matches!(c, files::index::fake_source::Call::List(_)))
        },
        Duration::from_secs(5),
    );
    actor.shutdown(Duration::from_millis(500));
    assert!(
        walked,
        "a stale cache on an auto share must still be refreshed"
    );
}

/// On a `Manual` tree, a stale cache is not re-walked on a timer.
#[test]
fn a_manual_tree_leaves_a_stale_cache_alone() {
    let cache = tempfile::tempdir().unwrap();

    let first = tree_store();
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(wait_for(
        &first,
        |_| cached(cache.path()),
        Duration::from_secs(5)
    ));
    actor.shutdown(Duration::from_millis(500));
    std::thread::sleep(Duration::from_millis(50));

    let src = share();
    let second = tree_store();
    // `persisting_settings` builds a tree mapping, which defaults to manual.
    let mut actor = persisting_tree_actor(src.clone(), Arc::clone(&second), cache.path());
    assert!(wait_for(
        &second,
        |s| s.first_tree_slot().as_tree().is_some(),
        Duration::from_secs(5)
    ));
    std::thread::sleep(Duration::from_millis(300));
    let listed = src
        .calls()
        .iter()
        .filter(|c| matches!(c, files::index::fake_source::Call::List(_)))
        .count();
    actor.shutdown(Duration::from_millis(500));
    assert_eq!(listed, 0, "a manual share must not walk on a timer");
}

/// A `Manual` tree still walks on a forced refresh.
#[test]
fn a_manual_tree_still_answers_a_forced_refresh() {
    let cache = tempfile::tempdir().unwrap();

    let first = tree_store();
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(wait_for(
        &first,
        |_| cached(cache.path()),
        Duration::from_secs(5)
    ));
    actor.shutdown(Duration::from_millis(500));

    let src = share();
    let second = tree_store();
    let mut actor = persisting_tree_actor(src.clone(), Arc::clone(&second), cache.path());
    assert!(wait_for(
        &second,
        |s| s.first_tree_slot().as_tree().is_some(),
        Duration::from_secs(5)
    ));

    actor.refresh(true);
    let walked = wait_for(
        &second,
        |_| {
            src.calls()
                .iter()
                .any(|c| matches!(c, files::index::fake_source::Call::List(_)))
        },
        Duration::from_secs(5),
    );
    actor.shutdown(Duration::from_millis(500));
    assert!(walked, "F5 must reach a manual share");
}

/// Returns `s` with every mapping's refresh policy set to `Auto`.
fn auto_refresh(s: Settings) -> Settings {
    let mut mappings: Vec<_> = s.routes.all().to_vec();
    for m in &mut mappings {
        m.refresh = files::paths::RefreshPolicy::Auto;
    }
    Settings {
        routes: Arc::new(files::paths::Routes::new(
            mappings,
            files::paths::ConfigSource::BuiltIn,
        )),
        ..s
    }
}

/// A walked tree is cached and restored on the next launch.
///
/// The second actor gets an empty share, so anything in the store afterwards
/// came from the cache.
#[test]
fn a_walked_tree_is_cached_and_comes_back_on_the_next_launch() {
    let cache = tempfile::tempdir().unwrap();

    let first = tree_store();
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(
        wait_for(
            &first,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Duration::from_secs(5)
        ),
        "the first launch should have walked the share"
    );
    assert!(
        wait_for(&first, |_| cached(cache.path()), Duration::from_secs(5)),
        "the walk should have written a cache file"
    );
    actor.shutdown(Duration::from_millis(500));

    let second = tree_store();
    let mut actor = persisting_tree_actor(FakeDirSource::new(), Arc::clone(&second), cache.path());
    assert!(
        wait_for(
            &second,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Duration::from_secs(5)
        ),
        "the second launch should have restored the index from its cache"
    );
    assert_eq!(
        second.first_tree_slot().status().origin,
        Some(Origin::DiskCache)
    );
    assert_eq!(
        found(&second, "11-3-0704"),
        vec!["R:\\archive\\2019\\odd name\\11-3-0704 survey.pdf"],
        "a restored tree must answer exactly as the walked one did"
    );
    actor.shutdown(Duration::from_millis(500));
}

/// A tree actor rejects a flat cache written for the same root.
///
/// Flat and tree caches share a magic number and version, so the kind must be
/// checked explicitly.
#[test]
fn a_tree_actor_will_not_read_a_flat_cache_written_for_the_same_root() {
    let cache = tempfile::tempdir().unwrap();

    // A flat actor indexing the tree root, so both use the same cache key.
    let flat_store = Arc::new(IndexStore::single(
        "custompro",
        std::path::Path::new(TREE_ROOT),
        MappingKind::Flat,
    ));
    let flat_settings = Settings {
        persist: true,
        cache_dir: Some(cache.path().to_path_buf()),
        ..Settings::for_mapping("custompro", TREE_ROOT, MappingKind::Flat)
    };
    let ctx = IndexContext::new(
        flat_settings,
        MappingId(0),
        Arc::clone(&flat_store),
        Arc::new(share()),
    )
    .with_cadence(Cadence::fast())
    .with_seed(7);
    let mut flat = actor::spawn(ctx, draining_events()).expect("the actor starts");
    assert!(wait_for(
        &flat_store,
        |s| s.first_flat_slot().as_flat().is_some() && cached(cache.path()),
        Duration::from_secs(5)
    ));
    flat.shutdown(Duration::from_millis(500));

    // A tree actor on the same root with an empty share, so only the cache
    // could populate it.
    let store = tree_store();
    let mut tree = persisting_tree_actor(FakeDirSource::new(), Arc::clone(&store), cache.path());
    assert!(
        wait_for(
            &store,
            |s| s.first_tree_slot().status().cache_rejected.is_some(),
            Duration::from_secs(5)
        ),
        "the flat cache should have been refused, and visibly so"
    );
    assert!(
        store.first_tree_slot().as_tree().is_none(),
        "nothing should have been published"
    );
    tree.shutdown(Duration::from_millis(500));
}

// --- live updates ----------------------------------------------------------

/// Change notifications end to end: the fake watcher fires, the queue
/// debounces, the scheduler requests a patch, and the actor re-reads the dirty
/// folder and republishes.
mod live {
    use super::*;
    use files::index::watch::fake::ScriptedWatcher;
    use files::index::watch::{WatchEvent, WatchQueue};
    use std::time::Duration as Dur;

    /// A watch queue with a 10ms debounce.
    fn queue() -> Arc<WatchQueue> {
        Arc::new(WatchQueue::new(Dur::from_millis(10), 1_000))
    }

    fn watching_actor(
        src: FakeDirSource,
        store: Arc<IndexStore>,
        watcher: ScriptedWatcher,
        queue: Arc<WatchQueue>,
    ) -> actor::IndexActor {
        let ctx = IndexContext::new(settings(), MappingId(0), store, Arc::new(src))
            .with_cadence(Cadence::fast())
            .with_seed(7)
            .with_watch(Arc::new(watcher), queue);
        actor::spawn(ctx, draining_events()).expect("the actor starts")
    }

    /// A patched folder is persisted and survives a restart.
    ///
    /// A manual share is never re-walked on a timer, so an unsaved patch would
    /// stay stale until a forced refresh.
    #[test]
    fn a_patched_folder_survives_a_restart() {
        let cache = tempfile::tempdir().unwrap();
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let q = queue();
        let ctx = IndexContext::new(
            persisting_settings(cache.path()),
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_cadence(Cadence::fast())
        .with_seed(7)
        .with_watch(Arc::new(watcher.clone()), q);
        let mut actor = actor::spawn(ctx, draining_events()).expect("the actor starts");

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));

        src.add_file("R:\\ab12", "urgent quote.pdf");
        watcher.changed(&["ab12"]);
        assert!(
            wait_for(
                &store,
                |s| !found(s, "urgent").is_empty(),
                Dur::from_secs(5)
            ),
            "the change never reached the index"
        );

        // `Cadence::fast()` sets `persist_spacing` to milliseconds, so the
        // save has happened by now; the shutdown path would catch it anyway.
        actor.shutdown(Dur::from_millis(500));

        // Restart against a share without the new file, so a hit can only come
        // from the cache.
        let cold = share();
        let second = tree_store();
        let mut actor = persisting_tree_actor(cold, Arc::clone(&second), cache.path());
        assert!(
            wait_for(
                &second,
                |s| !found(s, "urgent").is_empty(),
                Dur::from_secs(5)
            ),
            "the patch was lost: the restored index is the original walk"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// Repeated saves use distinct filenames.
    ///
    /// A patched index keeps its walk's capture time, so a filename derived
    /// from that alone would rename over the currently mapped file, which
    /// fails on Windows with `ERROR_SHARING_VIOLATION`.
    #[test]
    fn repeated_saves_do_not_collide_on_a_filename() {
        let cache = tempfile::tempdir().unwrap();
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let ctx = IndexContext::new(
            persisting_settings(cache.path()),
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_cadence(Cadence::fast())
        .with_seed(7)
        .with_watch(Arc::new(watcher.clone()), queue());
        let mut actor = actor::spawn(ctx, draining_events()).expect("the actor starts");

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some(),
            Dur::from_secs(5)
        ));

        for (i, name) in ["one.pdf", "two.pdf", "three.pdf"].iter().enumerate() {
            src.add_file("R:\\ab12", name);
            watcher.changed(&["ab12"]);
            let want = i + 5;
            assert!(
                wait_for(
                    &store,
                    |s| s
                        .first_tree_slot()
                        .as_tree()
                        .is_some_and(|t| t.len() == want),
                    Dur::from_secs(5)
                ),
                "patch {i} never landed"
            );
        }
        actor.shutdown(Dur::from_millis(500));

        // The cache must still be readable after several saves.
        let second = tree_store();
        let mut actor = persisting_tree_actor(share(), Arc::clone(&second), cache.path());
        assert!(
            wait_for(
                &second,
                |s| s.first_tree_slot().as_tree().is_some(),
                Dur::from_secs(5)
            ),
            "the cache was not readable after repeated saves"
        );
        assert!(
            second.first_tree_slot().status().cache_rejected.is_none(),
            "cache rejected: {:?}",
            second.first_tree_slot().status().cache_rejected
        );
        actor.shutdown(Dur::from_millis(500));
    }

    #[test]
    fn a_notified_change_makes_a_new_file_findable() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let q = queue();
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), q);

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));
        assert!(found(&store, "urgent").is_empty());

        src.add_file("R:\\ab12", "urgent quote.pdf");
        watcher.changed(&["ab12"]);

        assert!(
            wait_for(
                &store,
                |s| !found(s, "urgent").is_empty(),
                Dur::from_secs(5)
            ),
            "the change never reached the index"
        );
        assert_eq!(
            found(&store, "urgent"),
            vec!["R:\\ab12\\urgent quote.pdf".to_string()]
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// A notified deletion removes the file from the index.
    #[test]
    fn a_notified_deletion_removes_the_file() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| !found(s, "spec").is_empty(),
            Dur::from_secs(5)
        ));

        src.remove_file("R:\\ab12", "spec.pdf");
        watcher.changed(&["ab12"]);

        assert!(
            wait_for(&store, |s| found(s, "spec").is_empty(), Dur::from_secs(5)),
            "the deleted file is still in the index"
        );
        // The rest of the share is untouched.
        assert!(!found(&store, "11-3-0704").is_empty());
        actor.shutdown(Dur::from_millis(500));
    }

    /// A live update reads only the changed folder, not the whole share.
    #[test]
    fn a_live_update_reads_the_changed_folder_and_not_the_share() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));
        src.clear_calls();

        src.add_file("R:\\ab12", "urgent quote.pdf");
        watcher.changed(&["ab12"]);
        assert!(wait_for(
            &store,
            |s| !found(s, "urgent").is_empty(),
            Dur::from_secs(5)
        ));

        assert_eq!(
            src.list_count("R:\\archive\\2019\\odd name"),
            0,
            "a folder nobody touched was read anyway, so this is a re-walk"
        );
        assert!(src.list_count("R:\\ab12") >= 1);
        actor.shutdown(Dur::from_millis(500));
    }

    /// On an `Auto` share, an overflow triggers a full walk that recovers all
    /// missed changes.
    #[test]
    fn an_overflow_falls_back_to_a_full_walk_on_an_auto_share() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let ctx = IndexContext::new(
            auto_refresh(settings()),
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_cadence(Cadence::fast())
        .with_seed(7)
        .with_watch(Arc::new(watcher.clone()), queue());
        let mut actor = actor::spawn(ctx, draining_events()).expect("the actor starts");

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));

        // Two folders change, but the watcher reports only an overflow.
        src.add_file("R:\\ab12", "one.pdf");
        src.add_file("R:\\11d\\0704", "two.pdf");
        watcher.push(WatchEvent::Overflow);

        assert!(
            wait_for(
                &store,
                |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 6),
                Dur::from_secs(5)
            ),
            "an overflow should have recovered both files"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// On a `Manual` share, an overflow marks the share stale instead of
    /// walking.
    ///
    /// A large copy overflows every client's watch buffer at once; walking on
    /// overflow would put simultaneous re-walks from all clients on the server.
    #[test]
    fn an_overflow_on_a_manual_share_is_reported_rather_than_walked() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        // `settings()` builds a tree mapping, which defaults to on-demand.
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));
        let before = src.list_count("R:\\");

        src.add_file("R:\\ab12", "one.pdf");
        watcher.push(WatchEvent::Overflow);

        assert!(
            wait_for(
                &store,
                |s| s.first_tree_slot().status().stale
                    == Some(files::index::store::StaleReason::EventsLost),
                Dur::from_secs(5)
            ),
            "the share must say it has missed changes"
        );
        std::thread::sleep(Dur::from_millis(200));
        assert_eq!(
            src.list_count("R:\\"),
            before,
            "an on-demand share must not re-walk itself on an overflow"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// A forced refresh clears the missed-changes mark.
    #[test]
    fn refreshing_clears_the_missed_changes_mark() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some(),
            Dur::from_secs(5)
        ));
        src.add_file("R:\\ab12", "one.pdf");
        watcher.push(WatchEvent::Overflow);
        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().status().stale.is_some(),
            Dur::from_secs(5)
        ));

        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |s| s.first_tree_slot().status().stale.is_none()
                    && s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 5),
                Dur::from_secs(5)
            ),
            "a completed pass must answer the question and clear the mark"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// A folder that fails to re-read keeps its previous contents rather than
    /// being emptied.
    #[test]
    fn an_unreadable_folder_is_left_alone_rather_than_emptied() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(src.clone(), Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| !found(s, "spec").is_empty(),
            Dur::from_secs(5)
        ));

        src.fail_dir("R:\\ab12", EnumError::AccessDenied(5));
        watcher.changed(&["ab12"]);
        // Give the patch time to run and fail.
        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().status().activity == Activity::Idle
                && src.list_count("R:\\ab12") >= 2,
            Dur::from_secs(5)
        ));

        assert!(
            !found(&store, "spec").is_empty(),
            "an unreadable folder was emptied instead of being left alone"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// An unavailable watch is recorded and degrades health, without
    /// triggering a re-read.
    #[test]
    fn an_unavailable_watch_is_recorded_and_costs_no_extra_walk() {
        let src = share();
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let q = queue();
        let mut actor = watching_actor(
            src.clone(),
            Arc::clone(&store),
            watcher.clone(),
            Arc::clone(&q),
        );

        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some_and(|t| t.len() == 4),
            Dur::from_secs(5)
        ));
        src.clear_calls();

        watcher.push(WatchEvent::Unavailable("no CHANGE_NOTIFY".into()));
        assert!(wait_for(
            &store,
            |_| q.unavailable().is_some(),
            Dur::from_secs(5)
        ));
        assert!(
            q.is_empty(),
            "a dead watch is not a reason to re-read anything"
        );
        assert!(
            wait_for(
                &store,
                |s| matches!(
                    s.first_tree_slot().status().health,
                    Health::Degraded {
                        reason: DegradeReason::LiveUpdatesUnavailable,
                        ..
                    }
                ),
                Dur::from_secs(5)
            ),
            "the status line never said live updates had stopped"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// An unreadable subtree outranks a dead watch on the status line, since
    /// it means files cannot be found at all.
    #[test]
    fn an_unreadable_subtree_outranks_a_dead_watch_on_the_status_line() {
        let src = share();
        src.fail_dir("R:\\ab12", EnumError::AccessDenied(5));
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(src, Arc::clone(&store), watcher.clone(), queue());

        assert!(wait_for(
            &store,
            |s| matches!(
                s.first_tree_slot().status().health,
                Health::Degraded {
                    reason: DegradeReason::PartiallyUnreadable,
                    ..
                }
            ),
            Dur::from_secs(5)
        ));

        watcher.push(WatchEvent::Unavailable("no CHANGE_NOTIFY".into()));
        std::thread::sleep(Dur::from_millis(100));

        assert!(
            matches!(
                store.first_tree_slot().status().health,
                Health::Degraded {
                    reason: DegradeReason::PartiallyUnreadable,
                    ..
                }
            ),
            "the unreadable subtree was masked by the dead watch"
        );
        actor.shutdown(Dur::from_millis(500));
    }

    /// Shutdown stops the watcher thread, which blocks in a call a flag cannot
    /// interrupt.
    #[test]
    fn shutdown_stops_the_watcher_thread() {
        let store = tree_store();
        let watcher = ScriptedWatcher::new();
        let mut actor = watching_actor(share(), Arc::clone(&store), watcher, queue());
        assert!(wait_for(
            &store,
            |s| s.first_tree_slot().as_tree().is_some(),
            Dur::from_secs(5)
        ));
        assert!(actor.shutdown(Dur::from_secs(2)), "shutdown timed out");
    }
}
