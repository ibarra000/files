//! Shared, lock-free access to the current index.
//!
//! Readers only clone an `Arc`; nothing is copied out of a lock on the search
//! path.
//!
//! [`IndexStatus::health`] and the snapshot are separate, so entering an error
//! state cannot discard the last-known-good listing. A failed refresh changes
//! the health and leaves the data alone.
//!
//! # Single writer
//!
//! Each mapping's slot is written only by that mapping's index actor, so there
//! are no writer-writer races.

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
use crate::config::FOLDER_CACHE_CAPACITY;
use crate::paths::{MappingId, MappingKind, Routes};

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    Idle,
    /// Waiting for a walk permit because other shares are already walking.
    /// Separate from `Idle` so the wait is explained in the status line.
    Queued,
    LoadingDisk,
    Scanning {
        seen: usize,
    },
    /// A recursive walk. Reports folders done and queued as well as files, so
    /// the status line shows both progress and remaining work.
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
    /// findable. Surfaced so the user knows to check those folders.
    PartiallyUnreadable,
    /// The walk stopped before the end of the tree: a limit, a cancellation,
    /// or the share going away part way through.
    IncompleteWalk,
    /// The share does not report changes, so freshness rests on the periodic
    /// re-walk.
    ///
    /// Distinct from [`Self::StampUnreliable`], which is about the stamp
    /// probe; a tree never has one.
    LiveUpdatesUnavailable,
    /// The index holds only what searches have found; no pass read these
    /// folders.
    ///
    /// Distinct from [`Self::IncompleteWalk`]: a file nobody has searched for
    /// is absent, not merely stale.
    PartiallyObserved,
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
            Self::PartiallyObserved => "only what has been searched for",
        }
    }
}

/// What the last walk reached, and what it could not.
///
/// Separate from [`Health`] because it is an actionable list. Behind an `Arc`
/// because [`IndexStatus`] is cloned on every status update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCoverage {
    pub dirs: usize,
    pub files: usize,
    /// Folders that could not be read, so their files are missing from the
    /// index but not from the share.
    pub holes: u32,
    /// Folders deleted between being queued and being read.
    ///
    /// Not a reason to degrade: this is normal churn on a busy share.
    pub vanished: u32,
    /// A few unreadable folders verbatim, so they can be inspected.
    pub examples: Vec<String>,
    pub skipped_junctions: u32,
    pub elapsed: Duration,
}

/// Why a share's index may no longer match the drive.
///
/// Distinct from [`Health`], which is about reachability: a stale index was
/// read successfully but may since have been overtaken.
///
/// Reported per share, so the user refreshes that share rather than all of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// The watcher lost events.
    ///
    /// A bulk copy overruns the remote 64 KB buffer and the lost events cannot
    /// be recovered. Only a full pass can say what the share now holds.
    EventsLost,
    /// Live updates are not running here, so nothing is arriving at all.
    NoLiveUpdates,
    /// Not re-read for longer than [`crate::config::MAX_INDEX_AGE`].
    Age,
}

impl StaleReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::EventsLost => "changes were missed",
            Self::NoLiveUpdates => "not receiving updates",
            Self::Age => "not refreshed recently",
        }
    }

    /// Higher wins when a share has more than one reason.
    fn rank(self) -> u8 {
        match self {
            Self::Age => 0,
            Self::NoLiveUpdates => 1,
            Self::EventsLost => 2,
        }
    }
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

/// Everything the UI needs to describe the flat index accurately.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    pub origin: Option<Origin>,
    /// When the listing being served was actually captured.
    pub built_at: Option<SystemTime>,
    /// When the listing was last proven current by a stamp probe.
    ///
    /// Kept separate from `built_at` so a probe never makes old data look new.
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
    /// Session totals for scans, probes and failures.
    pub counters: Counters,
    /// What the last walk reached. `None` for a flat index, which has no
    /// notion of partial coverage.
    pub coverage: Option<Arc<TreeCoverage>>,
    /// Why the persisted index was not usable at startup, if it was not.
    pub cache_rejected: Option<String>,
    /// Set when the last refresh failed, even though the previous snapshot is
    /// still being served.
    pub last_error: Option<EnumError>,
    /// Why this share's index may be behind the drive, if it is.
    ///
    /// Cleared only by a completed full pass.
    pub stale: Option<StaleReason>,
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
            stale: None,
        }
    }
}

impl IndexStatus {
    /// Returns the wall-clock age of the current listing, clamped at zero.
    ///
    /// Clamped because `built_at` is a `SystemTime`: an NTP correction or a
    /// DST shift must not render as a negative age.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let built = self.built_at?;
        Some(now.duration_since(built).unwrap_or(Duration::ZERO))
    }

    /// Returns how long ago the listing was last proven current, clamped as
    /// above.
    pub fn confirmed_age(&self, now: SystemTime) -> Option<Duration> {
        let at = self.confirmed_at?;
        Some(now.duration_since(at).unwrap_or(Duration::ZERO))
    }
}

/// A summary of every configured index for the status line.
///
/// Totals, since several shares are searched as one corpus, and names, so a
/// problem share can be identified.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexOverview {
    /// Summed over every mapping holding data. `u64` because the per-share
    /// counts are `u32`.
    pub entries: u64,
    /// The mapping whose listing is oldest, and that mapping's timestamps.
    ///
    /// Oldest, so one fresh share cannot vouch for stale ones. All three
    /// fields come from the same mapping because the status line compares
    /// `built_at` against `confirmed_at`.
    pub oldest: Option<MappingId>,
    pub built_at: Option<SystemTime>,
    pub confirmed_at: Option<SystemTime>,
    pub origin: Option<Origin>,
    /// Any mapping truncated. A truncated share hides files that exist, so it
    /// always takes precedence.
    pub truncated: bool,
    /// The worst health, and whose. `Unreachable` beats `Degraded` beats
    /// `Ok`; ties go to the lowest id (configuration order).
    pub worst: Option<(MappingId, Health)>,
    /// The most significant activity across shares, and how many are busy.
    pub activity: Activity,
    pub busy: usize,
    /// Set when exactly one share is busy, so the status line can name it.
    pub busy_only: Option<MappingId>,
    /// Shares holding a listing, out of those configured to be indexed.
    pub ready: usize,
    pub configured: usize,
    /// The share with the strongest reason to be refreshed, and why.
    pub stalest: Option<(MappingId, StaleReason)>,
    /// How many shares are reporting themselves stale.
    pub stale_count: usize,
}

impl IndexOverview {
    /// Folds every mapping's status into one.
    ///
    /// `statuses` is indexed by [`MappingId`], like the store's slots, so a
    /// disabled mapping occupies a position and contributes nothing.
    pub fn of(routes: &Routes, statuses: &[Arc<IndexStatus>]) -> Self {
        let mut out = Self::default();
        for m in routes.all() {
            if !m.enabled || !m.kind.is_indexed() || m.path.as_os_str().is_empty() {
                continue;
            }
            let Some(status) = statuses.get(m.id.index()) else {
                continue;
            };
            out.configured += 1;
            out.entries = out.entries.saturating_add(u64::from(status.entries));
            out.truncated |= status.truncated;
            if status.built_at.is_some() {
                out.ready += 1;
            }

            // Oldest wins, and an absent timestamp is not "new".
            if let Some(built) = status.built_at
                && out.built_at.is_none_or(|worst| built < worst)
            {
                out.oldest = Some(m.id);
                out.built_at = Some(built);
                out.confirmed_at = status.confirmed_at;
                out.origin = status.origin;
            }

            if let Some(reason) = status.stale {
                out.stale_count += 1;
                if out
                    .stalest
                    .is_none_or(|(_, held)| reason.rank() > held.rank())
                {
                    out.stalest = Some((m.id, reason));
                }
            }

            if health_rank(&status.health) > out.worst.as_ref().map_or(0, |(_, h)| health_rank(h)) {
                out.worst = Some((m.id, status.health.clone()));
            }

            if status.activity.is_busy() {
                out.busy += 1;
                out.busy_only = if out.busy == 1 { Some(m.id) } else { None };
                if activity_rank(&status.activity) > activity_rank(&out.activity) {
                    out.activity = status.activity.clone();
                }
            }
        }
        out
    }

    /// Returns the wall-clock age of the oldest listing, clamped at zero.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let built = self.built_at?;
        Some(now.duration_since(built).unwrap_or(Duration::ZERO))
    }

    /// Returns how long ago that same listing was last proven current.
    pub fn confirmed_age(&self, now: SystemTime) -> Option<Duration> {
        let at = self.confirmed_at?;
        Some(now.duration_since(at).unwrap_or(Duration::ZERO))
    }

    pub fn is_busy(&self) -> bool {
        self.busy > 0
    }

    /// Returns the share most in need of a refresh, and why.
    ///
    /// Age is evaluated here rather than stored, because it becomes true
    /// without any event; an on-demand share's actor may not wake for hours.
    /// A reported reason always takes precedence over age.
    pub fn stale_at(&self, now: SystemTime) -> Option<(MappingId, StaleReason)> {
        if let Some(found) = self.stalest {
            return Some(found);
        }
        let old = self.age(now)? > crate::config::MAX_INDEX_AGE;
        old.then(|| self.oldest.map(|id| (id, StaleReason::Age)))?
    }

    /// Returns the unreachable share, when that is the worst status.
    pub fn unreachable(&self) -> Option<(MappingId, &Health)> {
        match &self.worst {
            Some((id, h @ Health::Unreachable { .. })) => Some((*id, h)),
            _ => None,
        }
    }

    /// Returns the degraded share and its reason, when nothing worse is
    /// happening.
    pub fn degraded(&self) -> Option<(MappingId, DegradeReason)> {
        match &self.worst {
            Some((id, Health::Degraded { reason, .. })) => Some((*id, *reason)),
            _ => None,
        }
    }
}

/// `Unreachable` outranks `Degraded` outranks `Ok`.
fn health_rank(h: &Health) -> u8 {
    match h {
        Health::Ok => 0,
        Health::Degraded { .. } => 1,
        Health::Unreachable { .. } => 2,
    }
}

/// Ranks activities for the summary when several shares are busy.
///
/// Slower work ranks higher. `Persisting` ranks lowest; `Queued` ranks below
/// real work.
fn activity_rank(a: &Activity) -> u8 {
    match a {
        Activity::Idle => 0,
        Activity::Persisting => 1,
        Activity::Queued => 2,
        Activity::LoadingDisk => 3,
        Activity::Scanning { .. } => 4,
        Activity::Walking { .. } => 5,
    }
}

/// A cached folder listing, including the negative case.
#[derive(Debug, Clone)]
enum FolderSlot {
    Present {
        snapshot: Arc<Snapshot>,
        fetched_at: Instant,
    },
    /// A remembered miss, so a partially typed search does not re-probe a
    /// nonexistent folder on every keystroke.
    Missing { err: EnumError, fetched_at: Instant },
}

impl FolderSlot {
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

/// What a slot holds: a flat listing or a walked tree, never both.
#[derive(Debug)]
pub enum SlotIndex {
    Flat(Arc<Snapshot>),
    Tree(Arc<TreeIndex>),
}

impl SlotIndex {
    pub fn len(&self) -> usize {
        match self {
            Self::Flat(s) => s.len(),
            Self::Tree(t) => t.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One mapping's index and its health.
///
/// Written only by the actor that owns this mapping.
pub struct MappingSlot {
    id: MappingId,
    kind: MappingKind,
    /// Copied from the routing table so readers do not need `Routes`.
    name: Box<str>,
    dir: PathBuf,
    enabled: bool,
    index: ArcSwapOption<SlotIndex>,
    status: ArcSwap<IndexStatus>,
}

impl MappingSlot {
    fn new(id: MappingId, kind: MappingKind, name: &str, dir: &Path, enabled: bool) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            dir: dir.to_path_buf(),
            enabled,
            index: ArcSwapOption::empty(),
            status: ArcSwap::from_pointee(IndexStatus::default()),
        }
    }

    // --- identity ----------------------------------------------------------

    pub fn id(&self) -> MappingId {
        self.id
    }

    pub fn kind(&self) -> MappingKind {
        self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn is_tree(&self) -> bool {
        self.kind == MappingKind::Tree
    }

    /// Returns whether a background actor should read and hold this mapping.
    ///
    /// The empty path is checked explicitly so an unconfigured mapping never
    /// gets an actor.
    pub fn is_indexed(&self) -> bool {
        self.enabled && self.kind.is_indexed() && !self.dir.as_os_str().is_empty()
    }

    /// Returns whether a search visits this mapping by either route.
    ///
    /// Differs from [`Self::is_indexed`] because a live share is searched but
    /// never read.
    pub fn is_searchable(&self) -> bool {
        self.enabled && self.kind.is_searched() && !self.dir.as_os_str().is_empty()
    }

    /// Returns whether a search queries the file server for this mapping
    /// directly.
    pub fn is_live(&self) -> bool {
        self.enabled && self.kind.is_live() && !self.dir.as_os_str().is_empty()
    }

    // --- reads, lock-free --------------------------------------------------

    pub fn index(&self) -> Option<Arc<SlotIndex>> {
        self.index.load_full()
    }

    /// Returns this slot's listing, when it holds a flat one.
    pub fn as_flat(&self) -> Option<Arc<Snapshot>> {
        match &*self.index.load_full()? {
            SlotIndex::Flat(s) => Some(Arc::clone(s)),
            SlotIndex::Tree(_) => None,
        }
    }

    /// Returns this slot's walked tree, when it holds one.
    pub fn as_tree(&self) -> Option<Arc<TreeIndex>> {
        match &*self.index.load_full()? {
            SlotIndex::Tree(t) => Some(Arc::clone(t)),
            SlotIndex::Flat(_) => None,
        }
    }

    pub fn has_index(&self) -> bool {
        self.index.load().is_some()
    }

    pub fn status(&self) -> Arc<IndexStatus> {
        self.status.load_full()
    }

    // --- writes, from this mapping's actor only ----------------------------

    /// Installs a new flat snapshot and clears any error state.
    pub fn publish_flat(&self, snapshot: Arc<Snapshot>, origin: Origin) {
        let entries = snapshot.len() as u32;
        let truncated = snapshot.truncated();
        let built_at = snapshot.captured_at();
        let stamp = snapshot.stamp();
        self.index.store(Some(Arc::new(SlotIndex::Flat(snapshot))));
        self.update_status(|s| {
            s.origin = Some(origin);
            s.built_at = Some(built_at);
            // The new listing is its own confirmation.
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.truncated = truncated;
            s.stamp = stamp;
            s.last_error = None;
            // Only a completed pass clears staleness.
            s.stale = None;
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

    /// Installs a tree index.
    ///
    /// Called repeatedly during a walk with one more segment each time, so the
    /// share is searchable early. `coverage` reports what the walk could not
    /// reach and is `None` until the walk finishes.
    pub fn publish_tree(
        &self,
        index: Arc<TreeIndex>,
        origin: Origin,
        coverage: Option<Arc<TreeCoverage>>,
    ) {
        let entries = index.len() as u32;
        let built_at = index.captured_at();
        let complete = index.complete();
        self.index.store(Some(Arc::new(SlotIndex::Tree(index))));
        self.update_status(|s| {
            s.origin = Some(origin);
            s.built_at = Some(built_at);
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.last_error = None;
            // Only a finished walk clears staleness.
            if coverage.is_some() {
                s.stale = None;
            }
            if let Some(coverage) = &coverage {
                s.coverage = Some(Arc::clone(coverage));
            }
            // Health is settled only once the walk has finished, so a running
            // walk is not flagged as incomplete.
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

    /// Installs an index assembled from what searches have found.
    ///
    /// Unlike [`Self::publish_tree`], nothing read the share, so `built_at`
    /// and `confirmed_at` are unchanged and health is degraded while this is
    /// the only index. Uses [`Origin::ServerObserved`].
    pub fn publish_observed(&self, index: Arc<TreeIndex>) {
        let entries = index.len() as u32;
        self.index.store(Some(Arc::new(SlotIndex::Tree(index))));
        self.update_status(|s| {
            s.origin = Some(Origin::ServerObserved);
            s.entries = entries;
            s.last_error = None;
            // Only if nothing worse is reported: unreachability is more
            // urgent than partial coverage, and this runs on every keystroke.
            if s.health.is_ok() {
                s.health = Health::Degraded {
                    reason: DegradeReason::PartiallyObserved,
                    since: Instant::now(),
                };
            }
        });
    }

    /// Records a failed refresh.
    ///
    /// Leaves the index alone, so stale data stays searchable with an accurate
    /// label. `since` survives retries so the onset time is preserved, and
    /// `activity` is cleared because a failed walk is no longer walking.
    pub fn record_failure(&self, err: EnumError, attempt: u32, next_retry_at: Instant) {
        self.update_status(|s| {
            let since = match s.health {
                Health::Unreachable { since, .. } => since,
                _ => Instant::now(),
            };
            s.last_error = Some(err);
            s.activity = Activity::Idle;
            s.health = Health::Unreachable {
                err,
                since,
                attempt,
                next_retry_at,
            };
        });
    }

    /// Records that the listing is still current as of `at`, without having
    /// rebuilt it. Used when the directory stamp proves nothing changed.
    ///
    /// Leaves `built_at` alone: the data is as old as it was.
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

    /// Records that this share's index may be behind the drive.
    ///
    /// Keeps the strongest reason, so "changes were missed" (completeness) is
    /// not overwritten by "not refreshed recently" (age).
    pub fn note_stale(&self, reason: StaleReason) {
        self.update_status(|s| {
            if s.stale.is_none_or(|held| reason.rank() >= held.rank()) {
                s.stale = Some(reason);
            }
        });
    }

    pub fn set_activity(&self, activity: Activity) {
        self.update_status(|s| s.activity = activity);
    }

    /// Marks the index degraded, unless it is already unreachable.
    ///
    /// Unreachability is the stronger statement and must not be overwritten.
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

    /// Records that live change notification is not running here.
    ///
    /// Only when health is otherwise `Ok`: an unreadable subtree is more
    /// important to report than a dead watch.
    ///
    /// Called on every actor wake, because a successful walk publishes a fresh
    /// health verdict that would otherwise clear this.
    pub fn note_live_updates_unavailable(&self) {
        self.update_status(|s| {
            if s.health == Health::Ok {
                s.health = Health::Degraded {
                    reason: DegradeReason::LiveUpdatesUnavailable,
                    since: Instant::now(),
                };
            }
        });
    }

    /// Records why the enumeration about to start is happening, for the
    /// status line.
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

    /// Records why the persisted index could not be used, so a rejected cache
    /// is distinguishable from a missing one.
    pub fn note_cache_rejected(&self, why: String) {
        self.update_status(|s| s.cache_rejected = Some(why));
    }

    fn update_status(&self, f: impl FnOnce(&mut IndexStatus)) {
        let mut next = (**self.status.load()).clone();
        f(&mut next);
        self.status.store(Arc::new(next));
    }
}

/// The index, shared by every thread.
pub struct IndexStore {
    /// One slot per configured mapping, indexed by [`MappingId`].
    ///
    /// Dense and allocated once, including disabled mappings, because
    /// `MappingId` is a position in the configured list (as [`Routes::get`]
    /// assumes). A fixed slice keeps every `&MappingSlot` stable for the life
    /// of the process, so each actor owns exactly one slot.
    slots: Box<[MappingSlot]>,
    folders: Mutex<LruCache<PathBuf, FolderSlot>>,
}

impl Default for IndexStore {
    /// A store over the shipped routing table (one flat mapping and one
    /// tree). Used by tests.
    fn default() -> Self {
        Self::for_routes(&crate::config::default_routes(), FOLDER_CACHE_CAPACITY)
    }
}

impl IndexStore {
    /// A store over [`crate::config::fixture_routes`]. Test fixture only.
    #[doc(hidden)]
    pub fn fixture() -> Self {
        Self::for_routes(&crate::config::fixture_routes(), FOLDER_CACHE_CAPACITY)
    }

    /// Builds a slot for every configured mapping, enabled or not.
    ///
    /// Disabled mappings get a slot but no actor, so `MappingId::index()`
    /// stays a direct index.
    pub fn for_routes(routes: &Routes, folder_capacity: usize) -> Self {
        let slots = routes
            .all()
            .iter()
            .map(|m| MappingSlot::new(m.id, m.kind, &m.name, &m.path, m.enabled))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self::from_slots(slots, folder_capacity)
    }

    /// A store with no mappings and a bounded folder cache, for the folder-cache
    /// tests.
    pub fn with_folder_capacity(folder_capacity: usize) -> Self {
        Self::from_slots(Box::new([]), folder_capacity)
    }

    /// A store over a single mapping. For tests and for `--doctor`.
    pub fn single(name: &str, dir: &Path, kind: MappingKind) -> Self {
        let slots = vec![MappingSlot::new(MappingId(0), kind, name, dir, true)].into_boxed_slice();
        Self::from_slots(slots, FOLDER_CACHE_CAPACITY)
    }

    fn from_slots(slots: Box<[MappingSlot]>, folder_capacity: usize) -> Self {
        let cap = NonZeroUsize::new(folder_capacity.max(1)).expect("capacity is at least one");
        Self {
            slots,
            folders: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn slot(&self, id: MappingId) -> Option<&MappingSlot> {
        self.slots.get(id.index())
    }

    pub fn slots(&self) -> &[MappingSlot] {
        &self.slots
    }

    /// Every mapping a background actor reads, in configuration order.
    ///
    /// Not the same as [`Self::searchable`]: a live share must never get an
    /// index actor, whose first wake would walk the whole share.
    pub fn indexed(&self) -> impl Iterator<Item = &MappingSlot> {
        self.slots.iter().filter(|s| s.is_indexed())
    }

    /// Every mapping a search visits, in configuration order.
    pub fn searchable(&self) -> impl Iterator<Item = &MappingSlot> {
        self.slots.iter().filter(|s| s.is_searchable())
    }

    /// Every mapping a search queries the file server for directly, in
    /// configuration order.
    pub fn live(&self) -> impl Iterator<Item = &MappingSlot> {
        self.slots.iter().filter(|s| s.is_live())
    }

    /// Total entries across every mapping holding data.
    pub fn entries(&self) -> u64 {
        self.slots
            .iter()
            .map(|s| u64::from(s.status().entries))
            .sum()
    }

    // --- first-match convenience -------------------------------------------
    //
    // Named `first_*` so the implicit first-match is explicit. Only tests and
    // diagnostics call these.

    /// The first enabled flat mapping's slot, for tests and diagnostics.
    pub fn first_flat_slot(&self) -> &MappingSlot {
        self.slots
            .iter()
            .find(|s| s.enabled && s.kind == MappingKind::Flat)
            .expect("the store has a flat mapping")
    }

    /// The first enabled tree mapping's slot, for tests and diagnostics.
    pub fn first_tree_slot(&self) -> &MappingSlot {
        self.slots
            .iter()
            .find(|s| s.enabled && s.kind == MappingKind::Tree)
            .expect("the store has a tree mapping")
    }

    // --- folder cache ------------------------------------------------------

    /// Looks up a folder listing, honouring the TTL. Never does I/O; the mutex is
    /// held only long enough to bump a refcount.
    pub fn folder(&self, dir: &Path, ttl: Duration) -> Cached {
        let mut guard = self.folders.lock();
        let Some(slot) = guard.get(dir) else {
            return Cached::Absent;
        };
        if slot.fetched_at().elapsed() >= ttl {
            return Cached::Absent;
        }
        match slot {
            FolderSlot::Present { snapshot, .. } => Cached::Hit(Arc::clone(snapshot)),
            FolderSlot::Missing { err, .. } => Cached::Miss(*err),
        }
    }

    pub fn publish_folder(&self, dir: PathBuf, snapshot: Arc<Snapshot>) {
        self.folders.lock().put(
            dir,
            FolderSlot::Present {
                snapshot,
                fetched_at: Instant::now(),
            },
        );
    }

    /// Remembers that a folder could not be listed.
    ///
    /// Only definite answers about the folder itself are cached (a
    /// whitelist). A network blip, cancellation or malformed buffer says
    /// nothing about whether the folder exists, and caching it would pin a
    /// wrong answer for the whole TTL.
    pub fn publish_folder_miss(&self, dir: PathBuf, err: EnumError) {
        let definite = matches!(
            err,
            EnumError::PathNotFound(_) | EnumError::NotADirectory(_) | EnumError::AccessDenied(_)
        );
        if !definite {
            return;
        }
        self.folders.lock().put(
            dir,
            FolderSlot::Missing {
                err,
                fetched_at: Instant::now(),
            },
        );
    }

    pub fn invalidate_folder(&self, dir: &Path) {
        self.folders.lock().pop(dir);
    }

    pub fn clear_folders(&self) {
        self.folders.lock().clear();
    }

    pub fn folder_cache_len(&self) -> usize {
        self.folders.lock().len()
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
        let s = IndexStore::fixture();
        assert!(s.first_flat_slot().as_flat().is_none());
        let st = s.first_flat_slot().status();
        assert!(st.health.is_ok());
        assert_eq!(st.entries, 0);
        assert!(st.origin.is_none());
    }

    #[test]
    fn publishing_makes_the_snapshot_and_its_metadata_visible() {
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(s.first_flat_slot().as_flat().unwrap().len(), 2);
        let st = s.first_flat_slot().status();
        assert_eq!(st.entries, 2);
        assert_eq!(st.origin, Some(Origin::Network));
        assert!(st.health.is_ok());
    }

    /// An error must never discard the data already held.
    #[test]
    fn a_failed_refresh_keeps_the_last_known_good_snapshot() {
        let s = IndexStore::fixture();
        s.first_flat_slot().publish_flat(
            snap(&["a", "b", "c"], SystemTime::UNIX_EPOCH),
            Origin::Network,
        );

        s.first_flat_slot()
            .record_failure(EnumError::Transient(53), 1, Instant::now());

        assert_eq!(
            s.first_flat_slot().as_flat().unwrap().len(),
            3,
            "data must survive the failure"
        );
        let st = s.first_flat_slot().status();
        assert!(st.health.is_unreachable());
        assert_eq!(st.last_error, Some(EnumError::Transient(53)));
        assert_eq!(st.entries, 3, "the count still describes the served data");
    }

    #[test]
    fn repeated_failures_keep_the_original_onset_time() {
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .record_failure(EnumError::Transient(53), 1, Instant::now());
        let status = s.first_flat_slot().status();
        let first = match &status.health {
            Health::Unreachable { since, .. } => *since,
            other => panic!("expected unreachable, got {other:?}"),
        };
        std::thread::sleep(Duration::from_millis(5));
        s.first_flat_slot()
            .record_failure(EnumError::Transient(53), 2, Instant::now());
        let status = s.first_flat_slot().status();
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
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .record_failure(EnumError::Transient(53), 3, Instant::now());
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let st = s.first_flat_slot().status();
        assert!(st.health.is_ok());
        assert!(st.last_error.is_none());
    }

    #[test]
    fn a_truncated_snapshot_is_reported_as_degraded() {
        let s = IndexStore::fixture();
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
        s.first_flat_slot()
            .publish_flat(Arc::new(snapshot), Origin::Network);
        assert!(matches!(
            s.first_flat_slot().status().health,
            Health::Degraded {
                reason: DegradeReason::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn confirming_freshness_recovers_from_unreachable() {
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        s.first_flat_slot()
            .record_failure(EnumError::Transient(53), 1, Instant::now());
        s.first_flat_slot()
            .confirm_fresh(Some(DirStamp::new(9, 9)), SystemTime::UNIX_EPOCH);
        let st = s.first_flat_slot().status();
        assert!(st.health.is_ok());
        assert_eq!(st.stamp, Some(DirStamp::new(9, 9)));
    }

    #[test]
    fn age_is_clamped_rather_than_going_negative() {
        // A clock correction can put built_at in the future.
        let future = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], future), Origin::Network);
        let age = s
            .first_flat_slot()
            .status()
            .age(SystemTime::UNIX_EPOCH)
            .unwrap();
        assert_eq!(age, Duration::ZERO);
    }

    #[test]
    fn folder_lookups_honour_the_ttl() {
        let s = IndexStore::fixture();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_folder(dir.clone(), snap(&["f.txt"], SystemTime::UNIX_EPOCH));

        assert!(matches!(
            s.folder(&dir, Duration::from_secs(60)),
            Cached::Hit(_)
        ));
        assert!(matches!(s.folder(&dir, Duration::ZERO), Cached::Absent));
    }

    #[test]
    fn an_unknown_folder_is_absent() {
        let s = IndexStore::fixture();
        assert!(matches!(
            s.folder(Path::new("R:\\nope"), Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    #[test]
    fn definite_misses_are_cached_so_typing_does_not_reprobe() {
        let s = IndexStore::fixture();
        let dir = PathBuf::from("R:\\nope");
        s.publish_folder_miss(dir.clone(), EnumError::PathNotFound(3));
        assert!(matches!(
            s.folder(&dir, Duration::from_secs(60)),
            Cached::Miss(_)
        ));
    }

    /// A transient failure must not be cached, or one network blip pins an
    /// empty answer for the TTL.
    #[test]
    fn transient_failures_are_never_cached() {
        let s = IndexStore::fixture();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_folder_miss(dir.clone(), EnumError::Transient(53));
        assert!(
            matches!(s.folder(&dir, Duration::from_secs(60)), Cached::Absent),
            "a blip must not poison the cache"
        );
    }

    /// Caching a cancellation would let a superseded keystroke suppress the
    /// results of the one that replaced it.
    #[test]
    fn only_definite_answers_about_the_folder_are_cached() {
        let s = IndexStore::fixture();

        for definite in [
            EnumError::PathNotFound(3),
            EnumError::NotADirectory(267),
            EnumError::AccessDenied(5),
        ] {
            let dir = PathBuf::from(format!("R:\\d{definite:?}"));
            s.publish_folder_miss(dir.clone(), definite);
            assert!(
                matches!(s.folder(&dir, Duration::from_secs(60)), Cached::Miss(_)),
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
            s.publish_folder_miss(dir.clone(), indefinite);
            assert!(
                matches!(s.folder(&dir, Duration::from_secs(60)), Cached::Absent),
                "{indefinite:?} says nothing about the folder and must not be cached"
            );
        }
    }

    #[test]
    fn the_folder_cache_is_bounded() {
        let s = IndexStore::with_folder_capacity(4);
        for i in 0..10 {
            s.publish_folder(
                PathBuf::from(format!("R:\\folder{i}")),
                snap(&["f"], SystemTime::UNIX_EPOCH),
            );
        }
        assert_eq!(s.folder_cache_len(), 4, "must not grow without bound");
        assert!(matches!(
            s.folder(Path::new("R:\\folder9"), Duration::from_secs(60)),
            Cached::Hit(_)
        ));
        assert!(matches!(
            s.folder(Path::new("R:\\folder0"), Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    #[test]
    fn invalidating_a_folder_forces_a_refetch() {
        let s = IndexStore::fixture();
        let dir = PathBuf::from("R:\\ab1234");
        s.publish_folder(dir.clone(), snap(&["f"], SystemTime::UNIX_EPOCH));
        s.invalidate_folder(&dir);
        assert!(matches!(
            s.folder(&dir, Duration::from_secs(60)),
            Cached::Absent
        ));
    }

    /// A freshness confirmation must not advance the data's age.
    #[test]
    fn confirming_freshness_does_not_pretend_the_data_is_new() {
        let s = IndexStore::fixture();
        let built = SystemTime::UNIX_EPOCH;
        s.first_flat_slot()
            .publish_flat(snap(&["a"], built), Origin::Network);

        let later = built + Duration::from_secs(3600);
        s.first_flat_slot()
            .confirm_fresh(Some(DirStamp::new(9, 9)), later);

        let st = s.first_flat_slot().status();
        assert_eq!(st.built_at, Some(built), "the data did not get any newer");
        assert_eq!(st.confirmed_at, Some(later));
        assert_eq!(st.age(later), Some(Duration::from_secs(3600)));
        assert_eq!(st.confirmed_age(later), Some(Duration::ZERO));
    }

    #[test]
    fn a_fresh_listing_is_its_own_confirmation() {
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let st = s.first_flat_slot().status();
        assert_eq!(st.built_at, st.confirmed_at);
    }

    #[test]
    fn the_scan_reason_is_visible_to_the_ui() {
        let s = IndexStore::fixture();
        assert!(s.first_flat_slot().status().last_scan_reason.is_none());
        s.first_flat_slot()
            .note_scan_started(ScanReason::StampMoved);
        assert_eq!(
            s.first_flat_slot().status().last_scan_reason,
            Some(ScanReason::StampMoved)
        );

        // It survives the publish, so the user can still see why afterwards.
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(
            s.first_flat_slot().status().last_scan_reason,
            Some(ScanReason::StampMoved)
        );
    }

    #[test]
    fn the_session_totals_reach_the_ui() {
        let s = IndexStore::fixture();
        s.first_flat_slot().note_schedule(
            Counters {
                full_scans: 3,
                probes: 40,
                ..Counters::default()
            },
            StampHealth::Blind { failures: 2 },
        );
        let st = s.first_flat_slot().status();
        assert_eq!(st.counters.full_scans, 3);
        assert!(st.stamp_health.is_blind());
    }

    /// A rejected cache is recorded so it is distinguishable from a first run.
    #[test]
    fn a_rejected_cache_is_recorded_rather_than_discarded() {
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .note_cache_rejected("cached index belongs to volume 00000000".into());
        assert!(
            s.first_flat_slot()
                .status()
                .cache_rejected
                .as_deref()
                .is_some_and(|r| r.contains("volume")),
            "the reason must survive"
        );
    }

    #[test]
    fn readers_never_block_the_writer() {
        // Holding a snapshot must not prevent a publish.
        let s = IndexStore::fixture();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let held = s.first_flat_slot().as_flat().unwrap();
        s.first_flat_slot()
            .publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(held.len(), 1, "the old view stays consistent");
        assert_eq!(s.first_flat_slot().as_flat().unwrap().len(), 2);
    }
}
