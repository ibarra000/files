//! Fetching a single job folder.
//!
//! Shared by the search worker (which needs the listing now) and the
//! prefetcher (which is guessing the user will need it shortly), so both go
//! through the same cache, the same TTL, and the same failure handling.
//!
//! # Never cache a failed read
//!
//! The previous implementation funnelled read errors through
//! `unwrap_or_default()` and then stored the resulting *empty* listing with a
//! fresh timestamp. One dropped packet therefore pinned "0 files" for the
//! whole TTL - which reads to the user as the tool being broken, or slow, or
//! both. Here a transient failure is never remembered; only a definite answer
//! such as "this folder does not exist" is.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use super::builder::SnapshotBuilder;
use super::enumerate::{DirSource, ListOpts};
use super::errors::EnumError;
use super::snapshot::Snapshot;
use super::store::{Cached, IndexStore};
use crate::config::JOB_CACHE_TTL;
use crate::util::cancel::CancelToken;

/// Where a listing came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cache,
    Network,
}

/// Returns a job folder's listing, from cache when it is fresh enough.
pub fn fetch(
    source: &dyn DirSource,
    store: &IndexStore,
    dir: &Path,
    force: bool,
    cancel: &CancelToken,
) -> Result<(Arc<Snapshot>, Source), EnumError> {
    if !force {
        match store.job(dir, JOB_CACHE_TTL) {
            Cached::Hit(snapshot) => return Ok((snapshot, Source::Cache)),
            Cached::Miss(err) => return Err(err),
            Cached::Absent => {}
        }
    }

    let prefix = dir.to_string_lossy().into_owned();
    let mut builder = SnapshotBuilder::new(&prefix);

    // Note the absence of any `exists()` probe beforehand. It cost an extra
    // round trip and, because `Path::exists` reports false on
    // ERROR_ACCESS_DENIED, it turned a permissions problem into "not found".
    // The enumeration itself gives a better answer for free.
    match source.list(dir, &mut builder, &ListOpts::default(), cancel) {
        Ok(_) => {}
        Err(EnumError::Empty) => {}
        Err(err) => {
            store.publish_job_miss(PathBuf::from(dir), err);
            return Err(err);
        }
    }

    let stamp = source.probe_stamp(dir).ok();
    let snapshot = Arc::new(builder.finish(SystemTime::now(), 0, stamp));
    store.publish_job(PathBuf::from(dir), Arc::clone(&snapshot));
    Ok((snapshot, Source::Network))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fake_source::{Call, FakeDirSource};

    fn setup() -> (FakeDirSource, IndexStore) {
        let src = FakeDirSource::new().with_dir("R:\\ab1234", &["a.pdf", "b.pdf"]);
        (src, IndexStore::default())
    }

    #[test]
    fn fetches_and_caches_a_job_folder() {
        let (src, store) = setup();
        let dir = Path::new("R:\\ab1234");

        let (snap, from) = fetch(&src, &store, dir, false, &CancelToken::never()).unwrap();
        assert_eq!(snap.len(), 2);
        assert_eq!(from, Source::Network);

        let (snap2, from2) = fetch(&src, &store, dir, false, &CancelToken::never()).unwrap();
        assert_eq!(snap2.len(), 2);
        assert_eq!(from2, Source::Cache);
        assert_eq!(
            src.list_count(dir),
            1,
            "the second call must not hit the network"
        );
    }

    #[test]
    fn forcing_bypasses_the_cache() {
        let (src, store) = setup();
        let dir = Path::new("R:\\ab1234");
        fetch(&src, &store, dir, false, &CancelToken::never()).unwrap();
        fetch(&src, &store, dir, true, &CancelToken::never()).unwrap();
        assert_eq!(src.list_count(dir), 2);
    }

    #[test]
    fn the_listing_carries_the_folder_as_its_prefix() {
        let (src, store) = setup();
        let (snap, _) = fetch(
            &src,
            &store,
            Path::new("R:\\ab1234"),
            false,
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(snap.full_path(0), "R:\\ab1234\\a.pdf");
    }

    #[test]
    fn a_missing_folder_is_remembered_so_typing_does_not_reprobe() {
        let (src, store) = setup();
        let dir = Path::new("R:\\nope");

        assert!(fetch(&src, &store, dir, false, &CancelToken::never()).is_err());
        assert!(fetch(&src, &store, dir, false, &CancelToken::never()).is_err());
        assert_eq!(src.list_count(dir), 1, "a definite miss should be cached");
    }

    /// The trap the previous implementation fell into.
    #[test]
    fn a_transient_failure_is_never_cached() {
        let (src, store) = setup();
        let dir = Path::new("R:\\ab1234");
        src.set_error(Some(EnumError::Transient(53)));

        assert_eq!(
            fetch(&src, &store, dir, false, &CancelToken::never()).unwrap_err(),
            EnumError::Transient(53)
        );

        // The drive comes back.
        src.set_error(None);
        let (snap, _) = fetch(&src, &store, dir, false, &CancelToken::never()).unwrap();
        assert_eq!(
            snap.len(),
            2,
            "a blip must not pin an empty answer for the TTL"
        );
    }

    #[test]
    fn a_failed_read_never_replaces_a_good_listing_with_an_empty_one() {
        let (src, store) = setup();
        let dir = Path::new("R:\\ab1234");
        fetch(&src, &store, dir, false, &CancelToken::never()).unwrap();

        src.set_error(Some(EnumError::Transient(53)));
        let _ = fetch(&src, &store, dir, true, &CancelToken::never());

        match store.job(dir, JOB_CACHE_TTL) {
            Cached::Hit(s) => assert_eq!(s.len(), 2, "the good listing must survive"),
            other => panic!("expected the cached listing to remain, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_folder_is_a_valid_empty_listing() {
        let src = FakeDirSource::new().with_dir("R:\\empty", &[]);
        let store = IndexStore::default();
        let (snap, _) = fetch(
            &src,
            &store,
            Path::new("R:\\empty"),
            false,
            &CancelToken::never(),
        )
        .unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn cancellation_propagates() {
        let (src, store) = setup();
        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.current());
        epoch.bump();
        assert_eq!(
            fetch(&src, &store, Path::new("R:\\ab1234"), false, &token).unwrap_err(),
            EnumError::Cancelled
        );
    }

    #[test]
    fn a_stamp_is_recorded_when_the_source_can_supply_one() {
        let (src, store) = setup();
        src.set_stamp(Some(crate::index::DirStamp::new(7, 8)));
        let (snap, _) = fetch(
            &src,
            &store,
            Path::new("R:\\ab1234"),
            false,
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(snap.stamp(), Some(crate::index::DirStamp::new(7, 8)));
        assert!(src.calls().iter().any(|c| matches!(c, Call::Stamp(_))));
    }
}
