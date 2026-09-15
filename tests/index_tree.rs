//! A walk driven through the index actor, on real threads.
//!
//! `tests/tree_walk.rs` proves the walk and `tests/index_schedule.rs` proves
//! the schedule; until this file existed nothing proved they compose. That gap
//! is where the interesting failures live: an actor that reads the wrong
//! index's state, a walk whose results never reach the store, a shutdown that
//! cannot interrupt a pass running for minutes.
//!
//! Real threads and a compressed cadence rather than a simulated clock,
//! because what is under test here *is* the wiring.

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

/// A share shaped like the real one, including the folder no routing rule
/// would have guessed.
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

/// The shipped configuration has no tree mapping yet - that arrives with the
/// rewritten `default_config.toml` - so the root is set directly here.
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
                },
                files::paths::Mapping {
                    id: MappingId(1),
                    name: "jobs".into(),
                    path: TREE_ROOT.into(),
                    kind: MappingKind::Tree,
                    enabled: true,
                    refresh: Default::default(),
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

/// Drains the event channel, so a `try_send` from the actor never blocks and
/// the test is not timing-sensitive to how fast it reads.
fn draining_events() -> files::app::event::Events {
    let (tx, rx) = bounded::<AppEvent>(256);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    // Nothing is drawing, so nothing needs waking.
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
    matcher::search_tree(&tree, query, &Hidden::none(), &CancelToken::never())
        .map(|o| o.hits.iter().map(|h| h.path.to_string()).collect())
        .unwrap_or_default()
}

/// The headline. A file whose folder no rule would have guessed is walked by
/// the actor, published to the store, and found by its code - the whole
/// project in one assertion.
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

/// And the folder case, which is how a job code usually appears.
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
            // Matched on its own name rather than its folder: `0704` really is
            // a substring of it, and a search is not a router - it reports
            // what matched rather than deciding what was meant.
            "R:\\archive\\2019\\odd name\\11-3-0704 survey.pdf",
        ]
    );
    // The two whose *folder* matched rank behind the one whose name did.
    assert!(
        found(&store, "0704")[0].ends_with("11-3-0704 survey.pdf"),
        "a name match should outrank a folder match"
    );
    actor.shutdown(Duration::from_millis(500));
}

/// The walk must not touch the flat index, and the flat actor must not touch
/// the tree. Each slot has exactly one writer, which is what the store's
/// single-writer rule actually requires.
#[test]
fn the_tree_actor_leaves_the_flat_index_alone() {
    let store = mixed_store();
    // Owns the tree, which is mapping 1. Two of the writes in `run` used to
    // ignore the actor's kind entirely and land on the flat mapping's status,
    // so this asserts more than it used to.
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

/// An unreadable folder is reported rather than silently dropped - the same
/// failure the routing rules produced, and the reason this index exists.
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

/// A folder deleted while the walk is running is ordinary churn on a share
/// people are working on, and must not be reported as a fault.
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

/// The actor must settle rather than re-walking on every wake.
///
/// This is the failure the review caught before it shipped: an actor that
/// consulted the *flat* index for "do I have a snapshot" would be told no on
/// every wake, make every scan a first run, and leave the minimum spacing as
/// the only thing between the share and a continuous re-walk.
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
    // `Cadence::fast` scales an hour to 400ms, so this is several floors'
    // worth of wall clock - long enough that a runaway would be obvious.
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

/// A healthy tree must not be reported as degraded.
///
/// The other defect the review caught: a tree captures no directory stamp by
/// design, and the flat path degrades whenever a scan captures none - so a
/// perfectly healthy tree would have read "change detection unavailable"
/// forever, on the happy path.
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

/// Shutdown has to reach a walk that is already running, or every exit during
/// one leaks the thread and its outstanding queries.
#[test]
fn shutdown_interrupts_a_running_walk() {
    let paths: Vec<String> = (0..400).map(|i| format!("d{i:03}\\f.pdf")).collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let src = FakeDirSource::new().with_tree(TREE_ROOT, &refs);
    // Enough latency per directory that the walk is certainly still running.
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

/// Results appear while the walk is still running, which is what makes a
/// three-minute pass usable rather than a blank screen.
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

/// True once the walk has written its cache file.
fn cached(cache: &std::path::Path) -> bool {
    std::fs::read_dir(cache).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".idx"))
    })
}

/// The regression test for the morning stampede.
///
/// A tree records no directory stamp - no single timestamp can stand for three
/// hundred thousand directories - and the scheduler used to credit a cache's
/// age against the re-walk floor *only* when a stamp came with it. Its stated
/// remedy was "one scan brings it up to date and establishes a stamp for next
/// time", which for a tree never terminates: the walk returns no stamp either,
/// so the next launch is in exactly the same position and reads the whole
/// share again. Every launch, on every machine.
///
/// Three hundred people logging on at nine o'clock made that 2.7 x 10^8 SMB
/// round trips in a fifteen-minute window, so this asserts the strongest thing
/// available: after a warm start the source is not enumerated *at all*.
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

    // Second launch, same share, same cache, and a source that counts.
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

/// The other half of the same rule, for a share that *is* on a timer: a cache
/// old enough to be past the floor still triggers a pass. Without this, the
/// fix above would be "never re-read anything", which is a different bug.
///
/// A tree defaults to `Manual` now, so this has to opt back in - which is the
/// contract worth pinning, since `Auto` is what a small share will use.
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

    // `Cadence::fast()` compresses the floor to milliseconds, so by the time
    // the second actor starts the restored cache is already past it.
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

/// And the point of `manual`: the same stale cache, left alone.
///
/// This is what three hundred clients do instead of re-walking nine hundred
/// thousand directories apiece every half hour.
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
    // `persisting_settings` builds a tree mapping, which now defaults to
    // manual - so this is the shipped behaviour, not an opt-in.
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

/// Manual means "not on a timer", not "never". The user asking still works.
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

/// Turns a settings fixture's mapping back to `auto`.
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

/// The point of persisting at all: 300,000 directories is one to three minutes
/// of round trips, and without a cache that is the cost of every launch.
///
/// The second actor is given a share that is *not there*, so its own walk
/// fails immediately and cannot mask the question being asked - whatever is in
/// the store afterwards came off the disk.
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

/// A tree actor pointed at a flat mapping's cache, or the reverse, must not
/// decode it. The cache key is per-directory so this should never arise - but
/// the two formats share a magic number and a version, and "should never
/// arise" is how one directory comes to be served as a whole share.
#[test]
fn a_tree_actor_will_not_read_a_flat_cache_written_for_the_same_root() {
    let cache = tempfile::tempdir().unwrap();

    // A *flat* actor indexing the tree root, so both write under the same key.
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

    // Now a tree actor on the same root, with the share gone so nothing but
    // the cache can populate it.
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

/// A change notification arriving at a running actor, end to end: the fake
/// watcher fires, the queue debounces, the scheduler asks for a patch, the
/// actor re-reads the dirty folder and republishes.
///
/// The property that decides whether any of this was worth building is the
/// last one here - that a live update reads the folders that changed and not
/// the share.
mod live {
    use super::*;
    use files::index::watch::fake::ScriptedWatcher;
    use files::index::watch::{WatchEvent, WatchQueue};
    use std::time::Duration as Dur;

    /// Milliseconds, so a test can watch a debounce go by.
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

    /// A patched folder has to reach the disk, or it is lost on every restart.
    ///
    /// This is what makes a manually-refreshed share workable at all. The
    /// watcher keeps it current while the program runs, but nothing re-walks
    /// on a timer any more - so a patch that never got written is a folder
    /// that stays stale until somebody asks for a full pass.
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

        // Restart against the same cache and a share that no longer has the
        // file, so anything found afterwards can only have come off the disk.
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

    /// Two saves in a row must not land on the same filename.
    ///
    /// A patched index keeps the capture time of the walk it grew from, and
    /// the cache filename used to be built from that alone - so the second
    /// save of a session renamed over the file the process already had mapped,
    /// which on Windows is `ERROR_SHARING_VIOLATION`.
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

        // The point: the index is still readable after several saves. A
        // rename over a mapped file would have failed the save, leaving the
        // pointer naming a file that is not there.
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

    /// A file deleted on the share has to leave the index, or a search offers
    /// a path that opens nothing.
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
        // And the rest of the share is untouched.
        assert!(!found(&store, "11-3-0704").is_empty());
        actor.shutdown(Dur::from_millis(500));
    }

    /// The whole cost argument. If a notification re-walked the share there
    /// would be no reason to have a watcher at all: the floor already does
    /// that, once every half hour.
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

    /// An overflow cannot say what changed, so on a share that refreshes on a
    /// timer it is the one case that re-walks - and everything it lost comes
    /// back.
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

        // Two folders changed; the watcher only knows that it lost track.
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

    /// On a share that refreshes on demand, the same overflow says so and
    /// waits.
    ///
    /// One person copying a large folder in overruns every client's watch
    /// buffer at once. If that started a full pass on each of them, a single
    /// ordinary action would put three hundred simultaneous re-walks on the
    /// server - which is the burst the on-demand policy exists to prevent. So
    /// the share is marked, named, and left for somebody to ask about.
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

    /// And asking for the pass clears it.
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

    /// A folder that cannot be read is left exactly as it was. Replacing it
    /// with nothing would turn a transient network error into files that
    /// cannot be found at all - the failure this whole index exists to remove.
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

    /// Believing you are live when you are not is worse than knowing you are
    /// not, so the failure is recorded rather than swallowed.
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

    /// A real coverage hole outranks a dead watch. One means files that cannot
    /// be found at all; the other means new ones take until the floor to
    /// appear, and replacing the first message with the second would hide the
    /// one somebody has to act on.
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

    /// The watcher's thread parks in a call no flag can reach, so shutdown has
    /// to tell it out of band or it waits out the whole budget.
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
