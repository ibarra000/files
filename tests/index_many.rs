//! Many shares at once.
//!
//! The index used to hold exactly two slots - one flat, one tree - while the
//! configuration layer accepted a list. A second flat mapping was refused
//! outright; a second tree mapping parsed, was listed by `--doctor`, and was
//! then silently never walked. These tests pin the shape that replaced it: one
//! slot and one actor per configured mapping, and a cap on how many of them
//! walk at once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use files::app::actors::Actors;
use files::config::Settings;
use files::index::enumerate::DirSource;
use files::index::fake_source::FakeDirSource;
use files::index::store::Activity;
use files::paths::{ConfigSource, Mapping, MappingId, MappingKind, Routes};

const SHARES: u16 = 5;

fn routes(n: u16) -> Routes {
    let mappings = (0..n)
        .map(|i| Mapping {
            id: MappingId(i),
            name: format!("share{i}").into(),
            path: format!("X:\\share{i}").into(),
            kind: MappingKind::Tree,
            enabled: true,
            refresh: Default::default(),
            depth: files::config::DEFAULT_LIVE_DEPTH,
        })
        .collect();
    Routes::new(mappings, ConfigSource::BuiltIn)
}

fn settings(n: u16, cap: usize) -> Settings {
    Settings {
        persist: false,
        live_updates: false,
        max_concurrent_scans: cap,
        ..Settings::with_routes(Arc::new(routes(n)), |s| s)
    }
}

/// Trees big enough, and slow enough, that several walks genuinely overlap.
fn source(n: u16) -> Arc<dyn DirSource> {
    let mut src = FakeDirSource::new();
    for i in 0..n {
        let root = format!("X:\\share{i}");
        let rels: Vec<String> = (0..12)
            .map(|d| format!("dir{d}\\file{d}-{i}.pdf"))
            .collect();
        let refs: Vec<&str> = rels.iter().map(String::as_str).collect();
        src = src.with_tree(&root, &refs);
    }
    src.set_latency(Duration::from_millis(15));
    Arc::new(src)
}

/// What the shares were collectively doing, sampled until they go quiet.
struct Observed {
    peak_walking: usize,
    saw_queued: bool,
    indexed: usize,
}

fn observe(actors: &Actors, budget: Duration) -> Observed {
    let store = &actors.backend.store;
    let mut peak_walking = 0usize;
    let mut saw_queued = false;
    let deadline = Instant::now() + budget;

    loop {
        let mut walking = 0usize;
        let mut indexed = 0usize;
        for slot in store.slots() {
            match slot.status().activity {
                Activity::Walking { .. } => walking += 1,
                Activity::Queued => saw_queued = true,
                _ => {}
            }
            if slot.has_index() {
                indexed += 1;
            }
        }
        peak_walking = peak_walking.max(walking);

        if indexed == store.slots().len() && walking == 0 {
            return Observed {
                peak_walking,
                saw_queued,
                indexed,
            };
        }
        if Instant::now() >= deadline {
            return Observed {
                peak_walking,
                saw_queued,
                indexed,
            };
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The headline: ten mappings used to mean two indexes.
#[test]
fn every_configured_share_is_walked_and_searchable() {
    let (mut actors, _rx) = Actors::start(
        settings(SHARES, 8),
        source(SHARES),
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    let seen = observe(&actors, Duration::from_secs(20));
    assert_eq!(
        seen.indexed, SHARES as usize,
        "every share must end up with an index of its own"
    );
    actors.shutdown();
}

/// And each keeps its own slot, rather than overwriting one shared pair.
#[test]
fn each_share_keeps_its_own_index_and_status() {
    let (mut actors, _rx) = Actors::start(
        settings(SHARES, 8),
        source(SHARES),
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    observe(&actors, Duration::from_secs(20));

    for slot in actors.backend.store.slots() {
        let status = slot.status();
        assert!(
            status.entries > 0,
            "{} has no entries of its own",
            slot.name()
        );
        assert!(
            !status.health.is_unreachable(),
            "{} reported unreachable: {:?}",
            slot.name(),
            status.health
        );
    }
    actors.shutdown();
}

/// The cap holds. One walk already keeps eight directory reads in flight, so
/// without this five shares would open forty at once against one server.
#[test]
fn no_more_shares_walk_at_once_than_the_cap_allows() {
    let cap = 2;
    let (mut actors, _rx) = Actors::start(
        settings(SHARES, cap),
        source(SHARES),
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    let seen = observe(&actors, Duration::from_secs(20));

    assert!(
        seen.peak_walking <= cap,
        "{} shares walked at once against a cap of {cap}",
        seen.peak_walking
    );
    assert!(
        seen.saw_queued,
        "with five shares and a cap of two, some share must have waited - \
         otherwise this test proves nothing about the cap"
    );
    assert_eq!(
        seen.indexed, SHARES as usize,
        "and all of them still finish"
    );
    actors.shutdown();
}

/// A share waiting its turn must not be indistinguishable from an idle one:
/// that is the unexplained wait the activity states exist to prevent.
#[test]
fn a_queued_share_is_reported_rather_than_looking_idle() {
    let (mut actors, _rx) = Actors::start(
        settings(SHARES, 1),
        source(SHARES),
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    let seen = observe(&actors, Duration::from_secs(30));
    assert!(seen.saw_queued, "a queued share reported itself as idle");
    assert!(seen.peak_walking <= 1);
    actors.shutdown();
}

/// Shutting down while shares are still queued must not wait them out.
#[test]
fn shutdown_is_prompt_while_shares_are_queued_for_a_permit() {
    let (mut actors, _rx) = Actors::start(
        settings(SHARES, 1),
        source(SHARES),
        std::sync::Arc::new(|| {}),
    )
    .unwrap();
    // Far enough in that the queue has certainly formed, and nowhere near
    // enough for five serialised walks to have finished.
    std::thread::sleep(Duration::from_millis(60));

    let started = Instant::now();
    actors.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "quitting waited for the queue: {:?}",
        started.elapsed()
    );
}
