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
    /// Waiting for a walk permit, because other shares are already walking.
    ///
    /// Its own state rather than `Idle`: a share sitting behind two walks for
    /// three minutes while reporting nothing is exactly the unexplained wait
    /// the other variants here exist to replace.
    Queued,
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

/// Why a share's index may no longer describe what is on the drive.
///
/// Distinct from [`Health`], which is about whether the share can be *reached*.
/// A stale index is one that was read successfully and has since been
/// overtaken: everything in it may still be right, and nothing in it can be
/// trusted to be complete.
///
/// Named per share rather than reported as a single flag, because the answer
/// is always "refresh that one" and a user asked to refresh everything will
/// either refresh everything - which is the load this exists to avoid - or
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// The watcher lost events.
    ///
    /// A bulk copy overruns the remote 64 KB buffer, and the API reports that
    /// by completing with zero bytes: nothing about what was lost can be
    /// recovered. Only a full pass can say what the share now holds.
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
    /// Why this share's index may be behind the drive, if it is.
    ///
    /// Cleared by a completed full pass, which is the only thing that can
    /// answer the question.
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

/// One status line's worth of truth about every configured index.
///
/// Totals, because somebody searching four shares is searching one corpus and
/// wants one number. Names, because a share that is down is one they have to
/// go and fix, and "something is unreachable" sends nobody anywhere.
///
/// This exists because there used to be a single `IndexStatus` on the app
/// state and one actor per index writing into it. With two actors the line
/// already described whichever share published last; with ten it would be a
/// flicker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexOverview {
    /// Summed over every mapping holding data. `u64` because a share's own
    /// count is a `u32` and ten of them is not obviously one.
    pub entries: u64,
    /// The mapping whose listing is *oldest*, and that mapping's timestamps.
    ///
    /// Oldest rather than newest: the newest lets one freshly rebuilt share
    /// vouch for nine stale ones. All three fields come from that same
    /// mapping, because the status line compares `built_at` against
    /// `confirmed_at`, and two minima drawn from different shares would make
    /// that comparison meaningless.
    pub oldest: Option<MappingId>,
    pub built_at: Option<SystemTime>,
    pub confirmed_at: Option<SystemTime>,
    pub origin: Option<Origin>,
    /// Any mapping truncated. A truncated share means a file that exists is
    /// not findable, which is the one thing this program refuses to hide, so
    /// one of them outweighs nine clean ones.
    pub truncated: bool,
    /// The worst health, and whose. `Unreachable` beats `Degraded` beats
    /// `Ok`; ties go to the lowest id, which is configuration order - the
    /// order the user thinks in.
    pub worst: Option<(MappingId, Health)>,
    /// The most significant thing any share is doing, and how many are doing
    /// something. Averaging three shares' folder counts would be fiction, and
    /// three sets of counters do not fit on one line.
    pub activity: Activity,
    pub busy: usize,
    /// Set when exactly one share is busy, so the common case can read
    /// "walking jobs..." rather than "walking 1 share...".
    pub busy_only: Option<MappingId>,
    /// Shares holding a listing, out of those configured to be searched - so
    /// "3 of 10 shares indexed" can be said rather than implied by a total
    /// that is quietly short.
    pub ready: usize,
    pub configured: usize,
    /// The share with the strongest reason to be refreshed, and why.
    ///
    /// Named, because the answer is always "refresh that one" - and a user
    /// told only that something is stale will refresh everything, which is the
    /// load this whole arrangement exists to avoid.
    pub stalest: Option<(MappingId, StaleReason)>,
    /// How many shares are reporting themselves stale.
    pub stale_count: usize,
}

impl IndexOverview {
    /// Folds every mapping's status into one.
    ///
    /// `statuses` is indexed by [`MappingId`], the same way the store's slots
    /// are, so a disabled mapping occupies a position and contributes nothing.
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

    /// Wall-clock age of the oldest listing, clamped at zero.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let built = self.built_at?;
        Some(now.duration_since(built).unwrap_or(Duration::ZERO))
    }

    /// How long ago that same listing was last proven current.
    pub fn confirmed_age(&self, now: SystemTime) -> Option<Duration> {
        let at = self.confirmed_at?;
        Some(now.duration_since(at).unwrap_or(Duration::ZERO))
    }

    pub fn is_busy(&self) -> bool {
        self.busy > 0
    }

    /// The share most in need of a refresh, and why.
    ///
    /// Age is judged here rather than stored, because it is the one reason
    /// that becomes true while nothing happens - an actor for an on-demand
    /// share may not wake for hours, and a flag it never got round to setting
    /// would be a promise the status line could not keep.
    ///
    /// A reported reason always wins over age: "changes were missed" says the
    /// index is *incomplete*, which is worth acting on however recently it was
    /// built.
    pub fn stale_at(&self, now: SystemTime) -> Option<(MappingId, StaleReason)> {
        if let Some(found) = self.stalest {
            return Some(found);
        }
        let old = self.age(now)? > crate::config::MAX_INDEX_AGE;
        old.then(|| self.oldest.map(|id| (id, StaleReason::Age)))?
    }

    /// The unreachable share, when the worst thing happening is one.
    pub fn unreachable(&self) -> Option<(MappingId, &Health)> {
        match &self.worst {
            Some((id, h @ Health::Unreachable { .. })) => Some((*id, h)),
            _ => None,
        }
    }

    /// The degraded share and its reason, when nothing worse is happening.
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

/// Which verb wins when several shares are busy at once.
///
/// The slowest work wins, because it is the one the user is waiting on.
/// `Persisting` loses to everything: it is seconds where a walk is minutes.
/// `Queued` loses to real work, so "two walking, three queued" reads as
/// walking - which is what is actually happening to the machine.
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

/// What a slot holds.
///
/// One field rather than a `Snapshot` slot beside a `TreeIndex` slot: a
/// mapping is one shape or the other, and two `Option`s where exactly one is
/// ever populated makes "both at once" representable and then relies on every
/// call site to remember that it cannot happen. The enum *is* the dispatch a
/// search needs, so the check and the branch are one line.
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
/// Exactly one writer thread - the actor that owns this mapping - which is
/// what the single-writer rule above actually requires. The previous shape
/// was one flat slot beside one tree slot, which satisfied that rule only
/// because there happened to be at most one actor of each kind.
pub struct MappingSlot {
    id: MappingId,
    kind: MappingKind,
    /// Copied from the routing table rather than looked up through it, so a
    /// reader never needs `Routes` in hand and an actor cannot write one
    /// mapping's state while reading another's.
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

    /// Whether a background actor should read this mapping and hold it.
    ///
    /// The empty path is checked rather than assumed away. An empty `PathBuf`
    /// standing in for "absent" is exactly what pinned a permanent
    /// `os error 3` on an actor spawned for a mapping nobody configured.
    pub fn is_indexed(&self) -> bool {
        self.enabled && self.kind.is_indexed() && !self.dir.as_os_str().is_empty()
    }

    /// Whether a search visits this mapping at all, by either route.
    ///
    /// Split from [`Self::is_indexed`] because a live share is searched and
    /// never read: one predicate answering both questions meant every caller
    /// got whichever answer it was named after.
    pub fn is_searchable(&self) -> bool {
        self.enabled && self.kind.is_searched() && !self.dir.as_os_str().is_empty()
    }

    /// Whether a search asks the file server about this mapping directly.
    pub fn is_live(&self) -> bool {
        self.enabled && self.kind.is_live() && !self.dir.as_os_str().is_empty()
    }

    // --- reads, lock-free --------------------------------------------------

    pub fn index(&self) -> Option<Arc<SlotIndex>> {
        self.index.load_full()
    }

    /// This slot's listing, when it holds a flat one.
    pub fn as_flat(&self) -> Option<Arc<Snapshot>> {
        match &*self.index.load_full()? {
            SlotIndex::Flat(s) => Some(Arc::clone(s)),
            SlotIndex::Tree(_) => None,
        }
    }

    /// This slot's walked tree, when it holds one.
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
            // The listing *is* the confirmation now.
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.truncated = truncated;
            s.stamp = stamp;
            s.last_error = None;
            // A completed pass is the only thing that can answer "is this
            // still what the share holds", so it is the only thing that
            // clears the question.
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
        self.index.store(Some(Arc::new(SlotIndex::Tree(index))));
        self.update_status(|s| {
            s.origin = Some(origin);
            s.built_at = Some(built_at);
            s.confirmed_at = Some(built_at);
            s.entries = entries;
            s.last_error = None;
            // Only once the walk has finished: a mid-walk publish has not yet
            // established anything about the share as a whole.
            if coverage.is_some() {
                s.stale = None;
            }
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

    /// Records a failed refresh.
    ///
    /// Deliberately does not touch the index: stale data plus an honest label
    /// beats an empty list.
    ///
    /// `since` survives a retry, so "unreachable for six minutes" stays true
    /// instead of resetting on every attempt. `activity` is cleared, because a
    /// walk that failed is not still walking - the tree half of this used to
    /// do that and the flat half did not, and the tree half was right.
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

    /// Records that this share's index may be behind the drive.
    ///
    /// Keeps the strongest reason: "changes were missed" is a statement about
    /// completeness, while "not refreshed recently" is only about age, and
    /// letting the second overwrite the first would soften the message that
    /// actually needs acting on.
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

    /// Marks the index degraded, unless something worse is already reported.
    ///
    /// The guard is the point: an unreachable share is a stronger statement
    /// than a degraded one, and letting a refused probe overwrite it would
    /// replace "this drive is gone" with "change detection unavailable".
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
    /// Only when nothing worse is already being reported. A subtree the walk
    /// could not read is a hole in what can be found at all, while a dead
    /// watch only means new files take until the floor to appear - and
    /// replacing the first message with the second would hide the one somebody
    /// has to act on.
    ///
    /// Set from the actor on every wake rather than once, because a
    /// *successful walk* publishes a fresh verdict on the tree's health, and a
    /// walk succeeding is not evidence that the watch came back.
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
    /// `MappingId` *is* a position in the configured list - which
    /// [`Routes::get`] already relies on. A map keyed by id would cost a hash
    /// on every slot lookup on the search path and, worse, would make the
    /// container itself mutable, which quietly dissolves the single-writer
    /// rule: a fixed slice means every `&MappingSlot` is stable for the life
    /// of the process and each actor owns exactly one by construction.
    slots: Box<[MappingSlot]>,
    jobs: Mutex<LruCache<PathBuf, JobSlot>>,
}

impl Default for IndexStore {
    /// A store over the shipped routing table, which is one flat mapping and
    /// one tree. Used by tests; the running program builds from the
    /// configuration that was actually loaded.
    fn default() -> Self {
        Self::for_routes(&crate::config::default_routes(), JOB_CACHE_CAPACITY)
    }
}

impl IndexStore {
    /// Builds a slot for every configured mapping, enabled or not.
    ///
    /// Disabled mappings get a slot and no actor. Keeping them means
    /// `MappingId::index()` stays a direct index, so an out-of-range id is
    /// unreachable except through a construction bug.
    pub fn for_routes(routes: &Routes, job_capacity: usize) -> Self {
        let slots = routes
            .all()
            .iter()
            .map(|m| MappingSlot::new(m.id, m.kind, &m.name, &m.path, m.enabled))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self::from_slots(slots, job_capacity)
    }

    /// A store with no mappings and a bounded job cache, for the job-cache
    /// tests, which are about the LRU rather than about any share.
    pub fn with_job_capacity(job_capacity: usize) -> Self {
        Self::from_slots(Box::new([]), job_capacity)
    }

    /// A store over a single mapping. For tests and for `--doctor`.
    pub fn single(name: &str, dir: &Path, kind: MappingKind) -> Self {
        let slots = vec![MappingSlot::new(MappingId(0), kind, name, dir, true)].into_boxed_slice();
        Self::from_slots(slots, JOB_CACHE_CAPACITY)
    }

    fn from_slots(slots: Box<[MappingSlot]>, job_capacity: usize) -> Self {
        let cap = NonZeroUsize::new(job_capacity.max(1)).expect("capacity is at least one");
        Self {
            slots,
            jobs: Mutex::new(LruCache::new(cap)),
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
    /// What gets an actor, a persisted index and a refresh schedule. **Not**
    /// what a search visits - see [`Self::searchable`]. The distinction is
    /// load-bearing: a live share reaching this iterator would be handed an
    /// index actor, whose first wake is a full walk of the share somebody
    /// configured precisely so it would never be walked.
    pub fn indexed(&self) -> impl Iterator<Item = &MappingSlot> {
        self.slots.iter().filter(|s| s.is_indexed())
    }

    /// Every mapping a search visits, in configuration order.
    pub fn searchable(&self) -> impl Iterator<Item = &MappingSlot> {
        self.slots.iter().filter(|s| s.is_searchable())
    }

    /// Every mapping a search asks the file server about, in configuration
    /// order.
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
    // Named `first_*` deliberately. An implicit first-match hiding behind an
    // innocent name is the exact mechanism that produced the two derived
    // `Settings` paths and the bug this module was re-keyed to remove, so
    // these say what they do and nothing in the running program calls them.

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
        assert!(s.first_flat_slot().as_flat().is_none());
        let st = s.first_flat_slot().status();
        assert!(st.health.is_ok());
        assert_eq!(st.entries, 0);
        assert!(st.origin.is_none());
    }

    #[test]
    fn publishing_makes_the_snapshot_and_its_metadata_visible() {
        let s = IndexStore::default();
        s.first_flat_slot()
            .publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(s.first_flat_slot().as_flat().unwrap().len(), 2);
        let st = s.first_flat_slot().status();
        assert_eq!(st.entries, 2);
        assert_eq!(st.origin, Some(Origin::Network));
        assert!(st.health.is_ok());
    }

    /// The modelling fix: an error must never cost us the data we already had.
    #[test]
    fn a_failed_refresh_keeps_the_last_known_good_snapshot() {
        let s = IndexStore::default();
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
        let s = IndexStore::default();
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
        let s = IndexStore::default();
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
        let s = IndexStore::default();
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
        let s = IndexStore::default();
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
        let s = IndexStore::with_job_capacity(4);
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
        let s = IndexStore::default();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let st = s.first_flat_slot().status();
        assert_eq!(st.built_at, st.confirmed_at);
    }

    #[test]
    fn the_scan_reason_is_visible_to_the_ui() {
        let s = IndexStore::default();
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
        let s = IndexStore::default();
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

    /// A rejected cache used to be swallowed, so "the drive letter now points
    /// somewhere else" looked identical to "first run".
    #[test]
    fn a_rejected_cache_is_recorded_rather_than_discarded() {
        let s = IndexStore::default();
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
        // Holding a snapshot must not prevent a publish - that is the whole
        // reason for the arc-swap.
        let s = IndexStore::default();
        s.first_flat_slot()
            .publish_flat(snap(&["a"], SystemTime::UNIX_EPOCH), Origin::Network);
        let held = s.first_flat_slot().as_flat().unwrap();
        s.first_flat_slot()
            .publish_flat(snap(&["a", "b"], SystemTime::UNIX_EPOCH), Origin::Network);
        assert_eq!(held.len(), 1, "the old view stays consistent");
        assert_eq!(s.first_flat_slot().as_flat().unwrap().len(), 2);
    }
}
