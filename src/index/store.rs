//! Shared, lock-free access to the current index.
//!
//! Readers only ever clone an `Arc`. The previous implementation deep-copied
//! the entire listing out of a mutex on every search - two million string
//! allocations and about 144 MB of memcpy, on the UI thread - which was
//! plausibly the single largest contributor to the perceived slowness.
//!
//! # Why the snapshot and the health are separate fields
//!
//! Modelling "healthy" and "has data" as one value is what made an
//! unreachable drive render as `0 files`: an error replaced the listing.
//! Here [`IndexStatus::health`] and the snapshot are orthogonal, so
//! **losing the last-known-good listing by entering an error state is not
//! representable**. A failed refresh changes the health and leaves the data
//! alone.
//!
//! # Single writer
//!
//! All mutation happens on the index actor thread. `publish_*` is the only
//! cross-thread write, which removes writer-writer races by construction.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use arc_swap::{ArcSwap, ArcSwapOption};
use lru::LruCache;
use parking_lot::Mutex;

use super::DirStamp;
use super::errors::EnumError;
use super::schedule::{Counters, ScanReason, StampHealth};
use super::snapshot::Snapshot;
use super::tree::TreeIndex;
use crate::config::JOB_CACHE_CAPACITY;

/// Where the current listing came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Restored from the persisted index at startup.
    DiskCache,
    /// A full enumeration of the share.
    Network,
    /// Observed via a server-side wildcard query.
    ServerObserved,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::DiskCache => "disk",
            Self::Network => "live",
            Self::ServerObserved => "server",
        }
    }
}

/// What the index is doing right now. Orthogonal to health and to whether
/// data is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    LoadingDisk,
    Scanning {
        seen: usize,
    },
    /// A recursive walk.
    ///
    /// Reports folders rather than only files, because "8,402 folders, 1,204
    /// queued" says both that it is moving and roughly how much is left,
    /// which a climbing file count does not. On a three-minute walk that is
    /// the difference between progress and an unexplained wait.
    Walking {
        dirs: usize,
        queued: usize,
        files: usize,
    },
    Persisting,
}

impl Activity {
    pub fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Why the index is not fully trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    /// The server does not appear to update the directory timestamp, so
    /// freshness relies on the periodic full rescan.
    StampUnreliable,
    /// Server-side filtering returned fewer results than a local match, so it
    /// has been disabled for this process.
    ServerFilterDisabled,
    /// The listing hit the arena ceiling.
    Truncated,
    /// Part of the tree could not be read, so a file that exists may not be
    /// findable.
    ///
    /// This is the same failure the routing rules produced - a silently
    /// absent file - and it is surfaced for the same reason. Someone told
    /// "3 folders unreadable" goes and looks at them; someone told nothing
    /// concludes the file is not there, and is wrong, and has no way to find
    /// that out.
    PartiallyUnreadable,
    /// The walk stopped before the end of the tree: a limit, a cancellation,
    /// or the share going away part way through.
    IncompleteWalk,
    /// The share will not report changes, so freshness rests entirely on the
    /// periodic re-walk.
    ///
    /// Distinct from [`Self::StampUnreliable`], which says the cheap *probe*
    /// is unavailable. A tree never had one, so reporting a healthy tree as
    /// "change detection unavailable" would be true in a way that sends
    /// someone to check the wrong thing.
    LiveUpdatesUnavailable,
}

impl DegradeReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::StampUnreliable => "change detection unavailable",
            Self::ServerFilterDisabled => "server filter disabled",
            Self::Truncated => "index truncated",
            Self::PartiallyUnreadable => "part of the tree was unreadable",
            Self::IncompleteWalk => "tree only partly walked",
            Self::LiveUpdatesUnavailable => "no live updates",
        }
    }
}

/// What the last walk reached, and what it could not.
///
/// Separate from [`Health`] because that is a single label and this is a list
/// someone is expected to act on. Behind an `Arc` because [`IndexStatus`] is
/// cloned on every status update, and a progress tick every hundred
/// milliseconds must not deep-copy sixty-four paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCoverage {
    pub dirs: usize,
    pub files: usize,
    /// Folders that could not be read, so their files are absent from the
    /// index without being absent from the share. This is the number that
    /// matters.
    pub holes: u32,
    /// Folders that were gone by the time the walk reached them.
    ///
    /// Counted apart from `holes` and deliberately not a reason to degrade:
    /// on a share people are working on, folders are deleted between being
    /// queued and being read all day, and calling that a fault would train
    /// the reader to ignore the warnings that matter.
    pub vanished: u32,
    /// A few unreadable folders verbatim, so they can be gone and looked at.
    pub examples: Vec<String>,
    pub skipped_junctions: u32,
    pub elapsed: Duration,
}

/// Index health, independent of whether a snapshot is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok,
    Degraded {
        reason: DegradeReason,
        since: Instant,
    },
    Unreachable {
        err: EnumError,
        since: Instant,
        attempt: u32,
        next_retry_at: Instant,
    },
}

impl Health {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }
}

/// Everything the UI needs to describe the flat index honestly.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    pub origin: Option<Origin>,
    /// When the listing being served was actually captured.
    pub built_at: Option<SystemTime>,
    /// When the listing was last *proven* still current by a stamp probe.
    ///
    /// Separate from `built_at` on purpose. The previous implementation moved
    /// `built_at` forward on every successful probe, so a listing an hour old
    /// rendered as "0s" - the one number a user checks before trusting a
    /// result was the only one guaranteed to be wrong.
    pub confirmed_at: Option<SystemTime>,
    pub entries: u32,
    pub truncated: bool,
    pub stamp: Option<DirStamp>,
    pub activity: Activity,
    pub health: Health,
    /// Why the enumeration currently running, or most recently run, happened.
    pub last_scan_reason: Option<ScanReason>,
    /// Whether change detection works against this share at all.
    pub stamp_health: StampHealth,
    /// Session totals, so "it keeps rebuilding" has a number.
    pub counters: Counters,
    /// Why the persisted index was not usable at startup, if it was not.
    /// What the last walk reached. `None` for a flat index, which has no
    /// notion of partial coverage: a single listing either worked or did not.
    pub coverage: Option<Arc<TreeCoverage>>,
    pub cache_rejected: Option<String>,
    /// Set when the last refresh failed, even though the previous snapshot is
    /// still being served.
    pub last_error: Option<EnumError>,
}

impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            origin: None,
            built_at: None,
            confirmed_at: None,
            entries: 0,
            truncated: false,
            stamp: None,
            activity: Activity::Idle,
            health: Health::Ok,
            last_scan_reason: None,
            stamp_health: StampHealth::Unknown,
            counters: Counters::default(),
            coverage: None,
            cache_rejected: None,
            last_error: None,
        }
    }
}

impl IndexStatus {
    /// Wall-clock age of the current listing, clamped at zero.
    ///
    /// Clamped because `built_at` is a `SystemTime`: an NTP correction or a
    /// DST shift must not render as a negative age.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let built = self.built_at?;
        Some(now.duration_since(built).unwrap_or(Duration::ZERO))
    }

    /// How long ago the listing was last proven current, clamped as above.
    pub fn confirmed_age(&self, now: SystemTime) -> Option<Duration> {
        let at = self.confirmed_at?;
        Some(now.duration_since(at).unwrap_or(Duration::ZERO))
    }
}

/// A cached job-folder listing, including the negative case.
#[derive(Debug, Clone)]
enum JobSlot {
    Present {
        snapshot: Arc<Snapshot>,
        fetched_at: Instant,
    },
    /// Remembering a miss stops a partially-typed code from re-probing a
    /// nonexistent folder on every keystroke.
    Missing { err: EnumError, fetched_at: Instant },
}

impl JobSlot {
    fn fetched_at(&self) -> Instant {
        match self {
            Self::Present { fetched_at, .. } | Self::Missing { fetched_at, .. } => *fetched_at,
        }
    }
}

/// What a cache lookup found.
#[derive(Debug, Clone)]
pub enum Cached {
    Hit(Arc<Snapshot>),
    /// A remembered failure, still within its TTL.
    Miss(EnumError),
    /// Nothing usable; the caller must go to the network.
    Absent,
}

/// The index, shared by every thread.
pub struct IndexStore {
    flat: ArcSwapOption<Snapshot>,
    status: ArcSwap<IndexStatus>,
    /// The walked tree, when a tree mapping is configured.
    ///
    /// A second slot rather than a map keyed by mapping: the shipped
    /// configuration is exactly one flat share and one tree, and the two are
    /// genuinely different things - one is a single directory with a cheap
    /// freshness probe, the other is three hundred thousand directories with
    /// none. A keyed map would hide that difference behind a uniformity the
    /// rest of the program does not have.
    ///
    /// Each slot still has exactly one writer thread, which is what the
    /// single-writer rule above actually requires.
    tree: ArcSwapOption<TreeIndex>,
    tree_status: ArcSwap<IndexStatus>,
    jobs: Mutex<LruCache<PathBuf, JobSlot>>,
}

impl Default for IndexStore {
    fn default() -> Self {
        Self::new(JOB_CACHE_CAPACITY)
    }
}

impl IndexStore {
    pub fn new(job_capacity: usize) -> Self {
        let cap = NonZeroUsize::new(job_capacity.max(1)).expect("capacity is at least one");
        Self {
            flat: ArcSwapOption::empty(),
            status: ArcSwap::from_pointee(IndexStatus::default()),
            tree: ArcSwapOption::empty(),
            tree_status: ArcSwap::from_pointee(IndexStatus::default()),
            jobs: Mutex::new(LruCache::new(cap)),
        }
    }

    // --- the walked tree ---------------------------------------------------

    /// The current tree index, if a tree mapping is configured and has been
    /// walked at all. Lock-free, like [`Self::flat`].
    pub fn tree(&self) -> Option<Arc<TreeIndex>> {
        self.tree.load_full()
    }

    pub fn tree_status(&self) -> Arc<IndexStatus> {
        self.tree_status.load_full()
    }

    /// Installs a tree index.
    ///
    /// Called repeatedly during a walk, each time with one more segment, so
    /// the share becomes searchable about a second in rather than after
    /// minutes. `coverage` says what the walk could not reach; `None` while
    /// one is still running, since a partial view is not yet a verdict.
    pub fn publish_tree(
        &self,
        index: Arc<TreeIndex>,
        origin: Origin,
        coverage: Option<Arc<TreeCoverage>>,
    ) {
        let entries = index.len() as u32;
        let built_at = index.captured_at();
        let complete = index.complete();
        self.tree.store(Some(index));
        self.update_tree_status(|s| {
            s.origin = Some(origin);
            s.built_at = Some(built_at);
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.last_error = None;
            if let Some(coverage) = &coverage {
                s.coverage = Some(Arc::clone(coverage));
            }
            // Health is only settled once the walk has finished. Judging a
            // half-finished walk would flag every one of them as incomplete
            // for the minutes it is running.
            s.health = match coverage.as_deref() {
                None => s.health.clone(),
                Some(c) if c.holes > 0 => Health::Degraded {
                    reason: DegradeReason::PartiallyUnreadable,
                    since: Instant::now(),
                },
                Some(_) if !complete => Health::Degraded {
                    reason: DegradeReason::IncompleteWalk,
                    since: Instant::now(),
                },
                Some(_) => Health::Ok,
            };
        });
    }

    /// Records a failed walk, leaving whatever index is already published in
    /// place - a stale tree beats no tree.
    pub fn record_tree_failure(&self, err: EnumError, attempt: u32, next_retry_at: Instant) {
        self.update_tree_status(|s| {
            s.last_error = Some(err);
            s.activity = Activity::Idle;
            s.health = Health::Unreachable {
                err,
                since: Instant::now(),
                attempt,
                next_retry_at,
            };
        });
    }

    pub fn note_tree_cache_rejected(&self, why: String) {
        self.update_tree_status(|s| s.cache_rejected = Some(why));
    }

    pub fn set_tree_activity(&self, activity: Activity) {
        self.update_tree_status(|s| s.activity = activity);
    }

    pub fn set_tree_degraded(&self, reason: DegradeReason) {
        self.update_tree_status(|s| {
            s.health = Health::Degraded {
                reason,
                since: Instant::now(),
            };
        });
    }

    pub fn note_tree_scan_started(&self, reason: ScanReason) {
        self.update_tree_status(|s| s.last_scan_reason = Some(reason));
    }

    pub fn note_tree_schedule(&self, counters: Counters, stamp_health: StampHealth) {
        self.update_tree_status(|s| {
            s.counters = counters;
            s.stamp_health = stamp_health;
        });
    }

    fn update_tree_status(&self, f: impl FnOnce(&mut IndexStatus)) {
        let mut next = (**self.tree_status.load()).clone();
        f(&mut next);
        self.tree_status.store(Arc::new(next));
    }

    // --- flat root ---------------------------------------------------------

    /// The current flat-root snapshot. Lock-free, never blocks, never does
    /// I/O.
    pub fn flat(&self) -> Option<Arc<Snapshot>> {
        self.flat.load_full()
    }

    pub fn status(&self) -> Arc<IndexStatus> {
        self.status.load_full()
    }

    /// Installs a new flat snapshot and clears any error state.
    pub fn publish_flat(&self, snapshot: Arc<Snapshot>, origin: Origin) {
        let entries = snapshot.len() as u32;
        let truncated = snapshot.truncated();
        let built_at = snapshot.captured_at();
        let stamp = snapshot.stamp();
        self.flat.store(Some(snapshot));
        self.update_status(|s| {
            s.origin = Some(origin);
            s.built_at = Some(built_at);
            // The listing *is* the confirmation now.
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.truncated = truncated;
            s.stamp = stamp;
            s.last_error = None;
            s.health = if truncated {
                Health::Degraded {
                    reason: DegradeReason::Truncated,
                    since: Instant::now(),
                }
            } else {
                Health::Ok
            };
        });
    }

    /// Records a failed refresh.
    ///
    /// Deliberately does not touch the snapshot: stale data plus an honest
    /// label beats an empty list.
    pub fn record_failure(&self, err: EnumError, attempt: u32, next_retry_at: Instant) {
        self.update_status(|s| {
            let since = match s.health {
                Health::Unreachable { since, .. } => since,
                _ => Instant::now(),
            };
            s.last_error = Some(err);
            s.health = Health::Unreachable {
                err,
                since,
                attempt,
                next_retry_at,
            };
        });
    }

    pub fn set_activity(&self, activity: Activity) {
        self.update_status(|s| s.activity = activity);
    }

    pub fn set_degraded(&self, reason: DegradeReason) {
        self.update_status(|s| {
            if !matches!(s.health, Health::Unreachable { .. }) {
                s.health = Health::Degraded {
                    reason,
                    since: Instant::now(),
                };
            }
        });
    }

    /// Notes why the enumeration about to start is happening, so the status
    /// line can say so rather than leaving the user to guess.
    pub fn note_scan_started(&self, reason: ScanReason) {
        self.update_status(|s| s.last_scan_reason = Some(reason));
    }

    /// Publishes the scheduler's running totals and stamp verdict.
    pub fn note_schedule(&self, counters: Counters, stamp_health: StampHealth) {
        self.update_status(|s| {
            s.counters = counters;
            s.stamp_health = stamp_health;
        });
    }

    /// Records why the persisted index could not be used.
    ///
    /// The previous implementation discarded this entirely, which made a
    /// rejected cache indistinguishable from a missing one - so a drive letter
    /// remapped to a different volume looked exactly like a first run, every
    /// single launch.
    pub fn note_cache_rejected(&self, why: String) {
        self.update_status(|s| s.cache_rejected = Some(why));
    }

    /// Records that the listing is still current as of `at`, without having
    /// rebuilt it. Used when the directory stamp proves nothing changed.
    ///
    /// Deliberately does not touch `built_at`: the data is exactly as old as
    /// it was a moment ago, and claiming otherwise is how a stale index came
    /// to render as a fresh one.
    pub fn confirm_fresh(&self, stamp: Option<DirStamp>, at: SystemTime) {
        self.update_status(|s| {
            s.confirmed_at = Some(at);
            if stamp.is_some() {
                s.stamp = stamp;
            }
            s.last_error = None;
            if s.health.is_unreachable() {
                s.health = Health::Ok;
            }
        });
    }

    fn update_status(&self, f: impl FnOnce(&mut IndexStatus)) {
        let mut next = (*self.status.load_full()).clone();
        f(&mut next);
        self.status.store(Arc::new(next));
    }

    // --- job folders -------------------------------------------------------

    /// Looks up a job folder, honouring the TTL. Never does I/O; the mutex is
    /// held only long enough to bump a refcount.
    pub fn job(&self, dir: &Path, ttl: Duration) -> Cached {
        let mut guard = self.jobs.lock();
        let Some(slot) = guard.get(dir) else {
            return Cached::Absent;
        };
        if slot.fetched_at().elapsed() >= ttl {
            return Cached::Absent;
        }
        match slot {
            JobSlot::Present { snapshot, .. } => Cached::Hit(Arc::clone(snapshot)),
            JobSlot::Missing { err, .. } => Cached::Miss(*err),
        }
    }

    pub fn publish_job(&self, dir: PathBuf, snapshot: Arc<Snapshot>) {
        self.jobs.lock().put(
            dir,
            JobSlot::Present {
                snapshot,
                fetched_at: Instant::now(),
            },
        );
    }

    /// Remembers that a folder could not be listed.
    ///
    /// Only *definite* answers about the folder itself are cached, and the
    /// set is a whitelist rather than a blacklist. Anything else - a network
    /// blip, a cancelled search, a malformed buffer - says nothing about
    /// whether the folder exists, and remembering it would pin a wrong answer
    /// for the whole TTL. That is the trap the previous implementation fell
    /// into by storing an empty listing after a failed read.
    pub fn publish_job_miss(&self, dir: PathBuf, err: EnumError) {
        let definite = matches!(
            err,
            EnumError::PathNotFound(_) | EnumError::NotADirectory(_) | EnumError::AccessDenied(_)
        );
        if !definite {
            return;
        }
        self.jobs.lock().put(
            dir,
            JobSlot::Missing {
                err,
                fetched_at: Instant::now(),
            },
        );
    }

    pub fn invalidate_job(&self, dir: &Path) {
        self.jobs.lock().pop(dir);
    }

    pub fn clear_jobs(&self) {
        self.jobs.lock().clear();
    }

    pub fn job_cache_len(&self) -> usize {
        self.jobs.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;

    fn snap(names: &[&str], at: SystemTime) -> Arc<Snapshot> {
        let mut b = SnapshotBuilder::new("V:\\");
        for n in names {
            b.push_str(n);
        }
        Arc::new(b.finish(at, 0, None))
    }

    #[test]
    fn starts_with_no_snapshot_and_a_clean_status() {
        let s = IndexStore::default();
        assert!(s.flat().is_none());
        let st = s.status();
        assert!(st.health.is_ok());
        assert_eq!(st.entries, 0);
        assert!(st.origin.is_none());
    }

    #[test]
    fn publishing_makes_the_snapshot_and_its_metadata_visible() {
        let s = IndexStore::default();
        s.publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(s.flat().unwrap().len(), 2);
        let st = s.status();
        assert_eq!(st.entries, 2);
        assert_eq!(st.origin, Some(Origin::Network));
        assert!(st.health.is_ok());
    }

    /// The modelling fix: an error must never cost us the data we already had.
    #[test]
    fn a_failed_refresh_keeps_the_last_known_good_snapshot() {
        let s = IndexStore::default();
        s.publish_flat(
            snap(&["a", "b", "c"], SystemTime::UNIX_EPOCH),
            Origin::Network,
        );

        s.record_failure(EnumError::Transient(53), 1, Instant::now());

        assert_eq!(s.flat().unwrap().len(), 3, "data must survive the failure");
        let st = s.status();
        assert!(st.health.is_unreachable());
        assert_eq!(st.last_error, Some(EnumError::Transient(53)));
        assert_eq!(st.entries, 3, "the count still describes the served data");
    }

    #[test]
    fn repeated_failures_keep_the_original_onset_time() {
        let s = IndexStore::default();
        s.record_failure(EnumError::Transient(53), 1, Instant::now());
        let status = s.status();
        let first = match &status.health {
            Health::Unreachable { since, .. } => *since,
            other => panic!("expected unreachable, got {other:?}"),
        };
        std::thread::sleep(Duration::from_millis(5));
        s.record_failure(EnumError::Transient(53), 2, Instant::now());
        let status = s.status();
        match &status.health {
            Health::Unreachable { since, attempt, .. } => {
                assert_eq!(*since, first, "onset should not reset on every retry");
                assert_eq!(*attempt, 2);
            }
            other => panic!("expected unreachable, got {other:?}"),
        }
    }

    #[test]
    fn a_successful_publish_clears_the_error() {
        let s = IndexStore::default();
        s.record_failure(EnumError::Transient(53), 3, Instant::now());
        s.publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let st = s.status();
        assert!(st.health.is_ok());
        assert!(st.last_error.is_none());
    }

    #[test]
    fn a_truncated_snapshot_is_reported_as_degraded() {
        let s = IndexStore::default();
        let mut b = SnapshotBuilder::new("V:\\");
        b.push_str("a");
        let mut snapshot = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        // Force the flag the arena ceiling would set.
        snapshot = crate::index::snapshot::Snapshot::from_parts(
            snapshot.prefix().into(),
            snapshot.offsets().to_vec().into_boxed_slice(),
            crate::index::snapshot::Arenas::Owned {
                lower: snapshot.lower().to_vec().into_boxed_slice(),
                orig: snapshot.orig().to_vec().into_boxed_slice(),
            },
            snapshot.max_name_len(),
            SystemTime::UNIX_EPOCH,
            0,
            None,
            true,
        );
        s.publish_flat(Arc::new(snapshot), Origin::Network);
        assert!(matches!(
            s.status().health,
            Health::Degraded {
                reason: DegradeReason::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn confirming_freshness_recovers_from_unreachable() {
        let s = IndexStore::default();
        s.publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        s.record_failure(EnumError::Transient(53), 1, Instant::now());
        s.confirm_fresh(Some(DirStamp::new(9, 9)), SystemTime::UNIX_EPOCH);
        let st = s.status();
        assert!(st.health.is_ok());
        assert_eq!(st.stamp, Some(DirStamp::new(9, 9)));
    }

    #[test]
    fn age_is_clamped_rather_than_going_negative() {
        // A clock correction can put built_at in the future.
        let future = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let s = IndexStore::default();
        s.publish_flat(snap(&["a"], future), Origin::Network);
        let age = s.status().age(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(age, Duration::ZERO);
    }

    #[test]
    fn job_lookups_honour_the_ttl() {
        let s = IndexStore::default();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_job(dir.clone(), snap(&["f.txt"], SystemTime::UNIX_EPOCH));

        assert!(matches!(
            s.job(&dir, Duration::from_secs(60)),
            Cached::Hit(_)
        ));
        assert!(matches!(s.job(&dir, Duration::ZERO), Cached::Absent));
    }

    #[test]
    fn an_unknown_job_folder_is_absent() {
        let s = IndexStore::default();
        assert!(matches!(
            s.job(Path::new("R:\\nope"), Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    #[test]
    fn definite_misses_are_cached_so_typing_does_not_reprobe() {
        let s = IndexStore::default();
        let dir = PathBuf::from("R:\\nope");
        s.publish_job_miss(dir.clone(), EnumError::PathNotFound(3));
        assert!(matches!(
            s.job(&dir, Duration::from_secs(60)),
            Cached::Miss(_)
        ));
    }

    /// The bug the previous implementation had: a transient failure must not
    /// be remembered, or one network blip pins an empty answer for the TTL.
    #[test]
    fn transient_failures_are_never_cached() {
        let s = IndexStore::default();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_job_miss(dir.clone(), EnumError::Transient(53));
        assert!(
            matches!(s.job(&dir, Duration::from_secs(60)), Cached::Absent),
            "a blip must not poison the cache"
        );
    }

    /// Only answers about the folder itself may be remembered. Caching a
    /// cancellation would make a superseded keystroke suppress the results of
    /// the keystroke that replaced it.
    #[test]
    fn only_definite_answers_about_the_folder_are_cached() {
        let s = IndexStore::default();

        for definite in [
            EnumError::PathNotFound(3),
            EnumError::NotADirectory(267),
            EnumError::AccessDenied(5),
        ] {
            let dir = PathBuf::from(format!("R:\\d{definite:?}"));
            s.publish_job_miss(dir.clone(), definite);
            assert!(
                matches!(s.job(&dir, Duration::from_secs(60)), Cached::Miss(_)),
                "{definite:?} is a definite answer and should be cached"
            );
        }

        for indefinite in [
            EnumError::Cancelled,
            EnumError::TimedOut,
            EnumError::Transient(53),
            EnumError::Corrupt(13),
            EnumError::Unsupported(87),
            EnumError::Other(999),
            EnumError::Empty,
        ] {
            let dir = PathBuf::from(format!("R:\\i{indefinite:?}"));
            s.publish_job_miss(dir.clone(), indefinite);
            assert!(
                matches!(s.job(&dir, Duration::from_secs(60)), Cached::Absent),
                "{indefinite:?} says nothing about the folder and must not be cached"
            );
        }
    }

    #[test]
    fn the_job_cache_is_bounded() {
        let s = IndexStore::new(4);
        for i in 0..10 {
            s.publish_job(
                PathBuf::from(format!("R:\\job{i}")),
                snap(&["f"], SystemTime::UNIX_EPOCH),
            );
        }
        assert_eq!(s.job_cache_len(), 4, "must not grow without bound");
        assert!(matches!(
            s.job(Path::new("R:\\job9"), Duration::from_secs(60)),
            Cached::Hit(_)
        ));
        assert!(matches!(
            s.job(Path::new("R:\\job0"), Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    #[test]
    fn invalidating_a_job_forces_a_refetch() {
        let s = IndexStore::default();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_job(dir.clone(), snap(&["f"], SystemTime::UNIX_EPOCH));
        s.invalidate_job(&dir);
        assert!(matches!(
            s.job(&dir, Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    /// The number a user checks before trusting a result is the index age.
    /// Moving it forward on every probe made a listing an hour old render as
    /// "0s" - honest about the check, dishonest about the data.
    #[test]
    fn confirming_freshness_does_not_pretend_the_data_is_new() {
        let s = IndexStore::default();
        let built = SystemTime::UNIX_EPOCH;
        s.publish_flat(snap(&["a"], built), Origin::Network);

        let later = built + Duration::from_secs(3600);
        s.confirm_fresh(Some(DirStamp::new(9, 9)), later);

        let st = s.status();
        assert_eq!(st.built_at, Some(built), "the data did not get any newer");
        assert_eq!(st.confirmed_at, Some(later));
        assert_eq!(st.age(later), Some(Duration::from_secs(3600)));
        assert_eq!(st.confirmed_age(later), Some(Duration::ZERO));
    }

    #[test]
    fn a_fresh_listing_is_its_own_confirmation() {
        let s = IndexStore::default();
        s.publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let st = s.status();
        assert_eq!(st.built_at, st.confirmed_at);
    }

    #[test]
    fn the_scan_reason_is_visible_to_the_ui() {
        let s = IndexStore::default();
        assert!(s.status().last_scan_reason.is_none());
        s.note_scan_started(ScanReason::StampMoved);
        assert_eq!(s.status().last_scan_reason, Some(ScanReason::StampMoved));

        // It survives the publish, so the user can still see why afterwards.
        s.publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(s.status().last_scan_reason, Some(ScanReason::StampMoved));
    }

    #[test]
    fn the_session_totals_reach_the_ui() {
        let s = IndexStore::default();
        s.note_schedule(
            Counters {
                full_scans: 3,
                probes: 40,
                ..Counters::default()
            },
            StampHealth::Blind { failures: 2 },
        );
        let st = s.status();
        assert_eq!(st.counters.full_scans, 3);
        assert!(st.stamp_health.is_blind());
    }

    /// A rejected cache used to be swallowed, so "the drive letter now points
    /// somewhere else" looked identical to "first run".
    #[test]
    fn a_rejected_cache_is_recorded_rather_than_discarded() {
        let s = IndexStore::default();
        s.note_cache_rejected("cached index belongs to volume 00000000".into());
        assert!(
            s.status()
                .cache_rejected
                .as_deref()
                .is_some_and(|r| r.contains("volume")),
            "the reason must survive"
        );
    }

    #[test]
    fn readers_never_block_the_writer() {
        // Holding a snapshot must not prevent a publish - that is the whole
        // reason for the arc-swap.
        let s = IndexStore::default();
        s.publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let held = s.flat().unwrap();
        s.publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(held.len(), 1, "the old view stays consistent");
        assert_eq!(s.flat().unwrap().len(), 2);
    }
}
