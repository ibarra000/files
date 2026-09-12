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

use crossbeam_channel::{Sender, bounded};
use files::app::event::AppEvent;
use files::config::Settings;
use files::index::actor::{self, IndexContext};
use files::index::errors::EnumError;
use files::index::fake_source::FakeDirSource;
use files::index::schedule::Cadence;
use files::index::store::{Activity, DegradeReason, Health, IndexStore, Origin};
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
        tree_path: TREE_ROOT.into(),
        ..Default::default()
    }
}

/// Drains the event channel, so a `try_send` from the actor never blocks and
/// the test is not timing-sensitive to how fast it reads.
fn draining_events() -> Sender<AppEvent> {
    let (tx, rx) = bounded::<AppEvent>(256);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    tx
}

fn tree_actor(src: FakeDirSource, store: Arc<IndexStore>) -> actor::IndexActor {
    let ctx = IndexContext::new(settings(), store, Arc::new(src), None)
        .for_tree()
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
    let Some(tree) = store.tree() else {
        return Vec::new();
    };
    matcher::search_tree(&tree, query, &CancelToken::never())
        .map(|o| o.hits.iter().map(|h| h.path.to_string()).collect())
        .unwrap_or_default()
}

/// The headline. A file whose folder no rule would have guessed is walked by
/// the actor, published to the store, and found by its code - the whole
/// project in one assertion.
#[test]
fn the_actor_walks_the_tree_and_the_file_becomes_findable() {
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(share(), Arc::clone(&store));

    assert!(
        wait_for(&store, |s| s.tree().is_some(), Duration::from_secs(5)),
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
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(share(), Arc::clone(&store));
    assert!(wait_for(
        &store,
        |s| s.tree().is_some_and(|t| t.len() == 4),
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
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(share(), Arc::clone(&store));
    assert!(wait_for(
        &store,
        |s| s.tree().is_some(),
        Duration::from_secs(5)
    ));

    assert!(store.flat().is_none(), "the flat index was never built");
    assert_eq!(store.status().entries, 0, "nor its status touched");
    assert!(store.tree_status().entries > 0);
    actor.shutdown(Duration::from_millis(500));
}

/// An unreadable folder is reported rather than silently dropped - the same
/// failure the routing rules produced, and the reason this index exists.
#[test]
fn an_unreadable_folder_degrades_the_tree_and_names_itself() {
    let src = share();
    src.fail_dir("R:\\11d\\0704", EnumError::AccessDenied(5));
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(src, Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| matches!(
            s.tree_status().health,
            Health::Degraded {
                reason: DegradeReason::PartiallyUnreadable,
                ..
            }
        ),
        Duration::from_secs(5)
    ));

    let status = store.tree_status();
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
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(src, Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.tree_status().coverage.is_some(),
        Duration::from_secs(5)
    ));
    let status = store.tree_status();
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
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(src.clone(), Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.tree().is_some(),
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
    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(share(), Arc::clone(&store));

    assert!(wait_for(
        &store,
        |s| s.tree_status().coverage.is_some(),
        Duration::from_secs(5)
    ));
    let status = store.tree_status();
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

    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(src, Arc::clone(&store));
    assert!(
        wait_for(
            &store,
            |s| matches!(s.tree_status().activity, Activity::Walking { .. }),
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

    let store = Arc::new(IndexStore::default());
    let mut actor = tree_actor(src, Arc::clone(&store));

    let searchable_mid_walk = wait_for(
        &store,
        |s| {
            matches!(s.tree_status().activity, Activity::Walking { .. })
                && s.tree().is_some_and(|t| !t.is_empty())
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
        tree_path: TREE_ROOT.into(),
        ..Default::default()
    }
}

fn persisting_tree_actor(
    src: FakeDirSource,
    store: Arc<IndexStore>,
    cache: &std::path::Path,
) -> actor::IndexActor {
    let ctx = IndexContext::new(persisting_settings(cache), store, Arc::new(src), None)
        .for_tree()
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

/// The point of persisting at all: 300,000 directories is one to three minutes
/// of round trips, and without a cache that is the cost of every launch.
///
/// The second actor is given a share that is *not there*, so its own walk
/// fails immediately and cannot mask the question being asked - whatever is in
/// the store afterwards came off the disk.
#[test]
fn a_walked_tree_is_cached_and_comes_back_on_the_next_launch() {
    let cache = tempfile::tempdir().unwrap();

    let first = Arc::new(IndexStore::default());
    let mut actor = persisting_tree_actor(share(), Arc::clone(&first), cache.path());
    assert!(
        wait_for(
            &first,
            |s| s.tree().is_some_and(|t| t.len() == 4),
            Duration::from_secs(5)
        ),
        "the first launch should have walked the share"
    );
    assert!(
        wait_for(&first, |_| cached(cache.path()), Duration::from_secs(5)),
        "the walk should have written a cache file"
    );
    actor.shutdown(Duration::from_millis(500));

    let second = Arc::new(IndexStore::default());
    let mut actor = persisting_tree_actor(FakeDirSource::new(), Arc::clone(&second), cache.path());
    assert!(
        wait_for(
            &second,
            |s| s.tree().is_some_and(|t| t.len() == 4),
            Duration::from_secs(5)
        ),
        "the second launch should have restored the index from its cache"
    );
    assert_eq!(second.tree_status().origin, Some(Origin::DiskCache));
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
    let flat_store = Arc::new(IndexStore::default());
    let flat_settings = Settings {
        persist: true,
        cache_dir: Some(cache.path().to_path_buf()),
        custpro_path: TREE_ROOT.into(),
        ..Default::default()
    };
    let ctx = IndexContext::new(
        flat_settings,
        Arc::clone(&flat_store),
        Arc::new(share()),
        None,
    )
    .with_cadence(Cadence::fast())
    .with_seed(7);
    let mut flat = actor::spawn(ctx, draining_events()).expect("the actor starts");
    assert!(wait_for(
        &flat_store,
        |s| s.flat().is_some() && cached(cache.path()),
        Duration::from_secs(5)
    ));
    flat.shutdown(Duration::from_millis(500));

    // Now a tree actor on the same root, with the share gone so nothing but
    // the cache can populate it.
    let store = Arc::new(IndexStore::default());
    let mut tree = persisting_tree_actor(FakeDirSource::new(), Arc::clone(&store), cache.path());
    assert!(
        wait_for(
            &store,
            |s| s.tree_status().cache_rejected.is_some(),
            Duration::from_secs(5)
        ),
        "the flat cache should have been refused, and visibly so"
    );
    assert!(store.tree().is_none(), "nothing should have been published");
    tree.shutdown(Duration::from_millis(500));
}
