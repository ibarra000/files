//! When to probe, when to enumerate, and when to do nothing.
//!
//! This module is the answer to a field report of "random indexing reloads":
//! the share was being re-enumerated every ~60 seconds instead of once an
//! hour, and the +/-10% jitter on the probe cadence is what made the timing
//! look arbitrary.
//!
//! # Why it is a separate, pure module
//!
//! The decision used to live inline in [`super::actor`]'s loop, interleaved
//! with `Instant::now()`, `SystemTime::now()`, `Rng::from_entropy()` and the
//! I/O itself. That made it untestable in the most literal sense: the probe
//! interval is 60 seconds and the rescan floor is an hour, so **no
//! timer-driven refresh had ever fired in a test**. The only way a test could
//! make the actor act was to send it an explicit refresh.
//!
//! Here the caller supplies `now` and performs the work; this module only
//! decides. Nothing in this file reads a clock, touches the filesystem, or
//! seeds itself from entropy. A day of operation can therefore be simulated
//! exhaustively in microseconds, which is what `tests/index_schedule.rs` does.
//!
//! # The two invariants that stop a reload storm
//!
//! Both are enforced here, once, rather than being re-derived at each of the
//! places that used to decide independently:
//!
//! 1. **A failed probe never causes an enumeration.** Not knowing whether the
//!    directory changed is not evidence that it did. The previous code
//!    returned "changed" whenever it had nothing to compare against, which
//!    turned one failed probe into a permanent per-minute full scan.
//! 2. **No two enumerations start closer together than
//!    [`Cadence::min_scan_spacing`]**, except an explicit F5 - which the user
//!    asked for and is entitled to.
//!
//! When change detection is genuinely unavailable, the schedule falls back to
//! [`Cadence::rescan_floor`], which is the behaviour the actor's own
//! documentation and `--doctor` have always claimed.

use std::time::{Duration, Instant};

use super::DirStamp;
use super::errors::EnumError;
use crate::config::{
    BACKOFF_BASE, BACKOFF_CAP, FULL_RESCAN_FLOOR, MIN_FULL_SCAN_SPACING, PATCH_SPACING,
    PROBE_JITTER_PERCENT, SCAN_BACKOFF_BASE, SCAN_BACKOFF_CAP, STAMP_FAILURES_BEFORE_BLIND,
    STAMP_PROBE_INTERVAL, TREE_MIN_SCAN_SPACING, TREE_RESCAN_FLOOR, env_secs,
};
use crate::util::backoff::{Backoff, jitter};
use crate::util::rng::Rng;

/// Smallest gap the scheduler will ever ask the actor to wait.
///
/// Every terminal decision lands at least this far in the future, which is
/// what makes "the index thread cannot busy-spin" a property rather than a
/// hope. It matters because the backoff is *full* jitter and can legitimately
/// return a delay of zero.
const TICK: Duration = Duration::from_millis(1);

/// How often the actor does each kind of work.
///
/// Injected rather than read from [`crate::config`] constants directly, so a
/// test can run a simulated day in milliseconds and so a misbehaving share can
/// be paced differently in the field without a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// Gap between cheap directory-stamp probes when everything is healthy.
    pub probe_interval: Duration,
    /// Longest the index may go without a full enumeration, for a server that
    /// does not move the directory stamp when entries are added or removed.
    pub rescan_floor: Duration,
    /// Hard lower bound on the gap between two enumerations.
    pub min_scan_spacing: Duration,
    /// Hard lower bound on the gap between two incremental updates.
    ///
    /// Its own floor, and far shorter than [`Self::min_scan_spacing`], because
    /// a patch reads the folders that changed rather than the share: seconds
    /// of round trips against minutes. Pacing both by one number would mean
    /// either re-walking every five minutes or learning about a new file half
    /// an hour late, and neither is the behaviour anybody asked for.
    pub patch_spacing: Duration,
    /// Jitter applied to `probe_interval`, in percent.
    pub jitter_percent: u32,
    /// Retry pacing for a failed probe.
    pub probe_backoff: Backoff,
    /// Retry pacing for a failed enumeration.
    pub scan_backoff: Backoff,
    /// Consecutive unanswerable probes before change detection is declared
    /// unavailable.
    pub stamp_failures_before_blind: u32,
}

impl Default for Cadence {
    fn default() -> Self {
        Self::shipped()
    }
}

impl Cadence {
    /// The shipped schedule.
    pub const fn shipped() -> Self {
        Self {
            probe_interval: STAMP_PROBE_INTERVAL,
            rescan_floor: FULL_RESCAN_FLOOR,
            min_scan_spacing: MIN_FULL_SCAN_SPACING,
            patch_spacing: PATCH_SPACING,
            jitter_percent: PROBE_JITTER_PERCENT,
            probe_backoff: Backoff::new(BACKOFF_BASE, BACKOFF_CAP),
            scan_backoff: Backoff::new(SCAN_BACKOFF_BASE, SCAN_BACKOFF_CAP),
            stamp_failures_before_blind: STAMP_FAILURES_BEFORE_BLIND,
        }
    }

    /// The schedule for a walked tree.
    ///
    /// A tree never probes - no directory timestamp can stand for three
    /// hundred thousand directories - so it lives permanently on the floor,
    /// and the floor is correspondingly shorter and the spacing between passes
    /// correspondingly longer, because a pass costs minutes rather than
    /// seconds.
    ///
    /// `probe_interval` carries the spacing rather than a probe interval,
    /// which is not a trick: [`Self::normalised`] clamps `min_scan_spacing`
    /// to at most `probe_interval`, so leaving the probe interval at a minute
    /// would quietly clamp a five-minute spacing back down to one. The field
    /// is unused by a tree, so it is free to hold the value that keeps the
    /// clamp honest.
    pub const fn tree() -> Self {
        Self {
            probe_interval: TREE_MIN_SCAN_SPACING,
            rescan_floor: TREE_RESCAN_FLOOR,
            min_scan_spacing: TREE_MIN_SCAN_SPACING,
            patch_spacing: PATCH_SPACING,
            jitter_percent: PROBE_JITTER_PERCENT,
            probe_backoff: Backoff::new(BACKOFF_BASE, BACKOFF_CAP),
            scan_backoff: Backoff::new(SCAN_BACKOFF_BASE, SCAN_BACKOFF_CAP),
            stamp_failures_before_blind: STAMP_FAILURES_BEFORE_BLIND,
        }
    }

    /// Milliseconds instead of minutes, so an integration test can watch an
    /// "hour" go by. Scaled as a whole, so the ratios that actually matter -
    /// probe to floor, backoff to spacing - are the shipped ones.
    pub const fn fast() -> Self {
        Self {
            probe_interval: Duration::from_millis(20),
            rescan_floor: Duration::from_millis(400),
            min_scan_spacing: Duration::from_millis(10),
            patch_spacing: Duration::from_millis(2),
            jitter_percent: PROBE_JITTER_PERCENT,
            probe_backoff: Backoff::new(Duration::from_millis(2), Duration::from_millis(100)),
            scan_backoff: Backoff::new(Duration::from_millis(10), Duration::from_millis(200)),
            stamp_failures_before_blind: STAMP_FAILURES_BEFORE_BLIND,
        }
    }

    /// Layers `FILES_PROBE_INTERVAL` / `FILES_RESCAN_FLOOR` (whole seconds)
    /// over `self`.
    ///
    /// Readable from the environment for the same reason every other strategy
    /// switch in this crate is: none of this can be exercised against the real
    /// network shares on the machine it is written on, so the cadence has to
    /// be adjustable in the field - and compressible during a diagnosis.
    pub fn from_env(mut self) -> Self {
        if let Some(v) = env_secs("FILES_PROBE_INTERVAL") {
            self.probe_interval = v;
        }
        if let Some(v) = env_secs("FILES_RESCAN_FLOOR") {
            self.rescan_floor = v;
        }
        self.normalised()
    }

    /// Reconciles the four intervals with each other.
    ///
    /// Separate from [`Cadence::from_env`] so it can be tested without the
    /// process environment, which `cargo test` shares across threads.
    pub fn normalised(mut self) -> Self {
        // Probing less often than rescanning would make the probe pointless.
        self.probe_interval = self.probe_interval.min(self.rescan_floor);
        // The spacing backstop has to scale with the rest, or compressing the
        // cadence to reproduce a problem in minutes silently suppresses the
        // very rescan being reproduced. At the shipped values this changes
        // nothing: thirty seconds is already below both half an hour and a
        // minute.
        self.min_scan_spacing = self
            .min_scan_spacing
            .min(self.rescan_floor / 2)
            .min(self.probe_interval);
        self
    }
}

/// Whether the source can tell us the directory changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampHealth {
    /// Nothing has been probed yet.
    #[default]
    Unknown,
    /// A stamp has been read, so change detection works here.
    Working,
    /// This share will not answer the probe. The rescan floor is now the only
    /// freshness guarantee, which is exactly what it is for.
    Blind { failures: u32 },
}

impl StampHealth {
    pub fn is_blind(self) -> bool {
        matches!(self, Self::Blind { .. })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "not yet probed",
            Self::Working => "working",
            Self::Blind { .. } => "unavailable",
        }
    }
}

/// Why a full enumeration happened.
///
/// Carried into the status line and the decision log, because "the index
/// rebuilt and I do not know why" is the actual bug report this module came
/// from. A rebuild the user can attribute is a rebuild they can live with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanReason {
    /// No listing at all yet.
    FirstRun,
    /// The user pressed F5.
    Forced,
    /// The directory stamp moved, so something was added or removed.
    StampMoved,
    /// The rescan floor elapsed. Expected, once per floor.
    Floor,
    /// Change detection is unavailable here, so the floor is all there is.
    Blind,
    /// The watcher reported changes it could not describe, so nothing short of
    /// a full pass can say what the share now holds.
    Changed,
}

impl ScanReason {
    /// Short phrase for the status line, e.g. `building index (F5)...`.
    pub fn label(self) -> &'static str {
        match self {
            Self::FirstRun => "first run",
            Self::Forced => "F5",
            Self::StampMoved => "directory changed",
            Self::Floor => "periodic refresh",
            Self::Blind => "no change detection",
            Self::Changed => "changes detected",
        }
    }
}

/// What the actor feeds back after doing what it was told.
#[derive(Debug, Clone, Copy)]
pub enum Input {
    /// The persisted index was restored. `captured_age` is how old that
    /// listing's data is by the wall clock.
    DiskLoaded {
        stamp: Option<DirStamp>,
        captured_age: Option<Duration>,
    },
    /// A deadline expired, or a refresh command arrived.
    Woke { forced: bool },
    /// The result of a [`Step::Probe`].
    Probed(Result<DirStamp, EnumError>),
    /// The result of a [`Step::FullScan`]. `Ok(stamp)` carries the stamp
    /// captured alongside the listing, which is `None` when even that probe
    /// failed.
    Scanned(Result<Option<DirStamp>, EnumError>),
    /// The watcher has changes pending. `full` means it could not say what
    /// they were - an overflowed buffer, or more dirty folders than re-reading
    /// one at a time could pay for.
    ///
    /// This is what a directory stamp supplies for a flat listing and cannot
    /// supply for a tree: no single timestamp speaks for three hundred
    /// thousand directories, so the notification has to.
    Changed { full: bool },
    /// The result of a [`Step::ApplyChanges`].
    Applied(Result<(), EnumError>),
}

/// What the actor should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Read the directory stamp and report back with [`Input::Probed`].
    Probe,
    /// Enumerate the whole directory and report back with [`Input::Scanned`].
    FullScan(ScanReason),
    /// Re-read the dirty subtrees and patch them into the index, reporting
    /// back with [`Input::Applied`].
    ///
    /// A separate step from `FullScan` because the two differ by three orders
    /// of magnitude in cost - a handful of round trips against nine hundred
    /// thousand - and pacing them by one rule would either re-walk the share
    /// every few minutes or make a live update as rare as the floor.
    ApplyChanges,
    /// Nothing moved: mark the listing confirmed-current, then wait.
    ConfirmFresh,
    /// Wait until `next_action`.
    Wait,
}

impl Step {
    /// True when the actor should wait rather than immediately feed a result
    /// back in. Only terminal steps carry a meaningful `next_action`.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::ConfirmFresh | Self::Wait)
    }

    pub fn scan_reason(self) -> Option<ScanReason> {
        match self {
            Self::FullScan(r) => Some(r),
            _ => None,
        }
    }
}

/// A step plus when to wake up afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub step: Step,
    /// For a terminal step, strictly in the future. For `Probe` and
    /// `FullScan`, `now` - the actor acts at once and then decides again.
    pub next_action: Instant,
}

/// Running totals for one session, so "how often does this actually rebuild?"
/// has a number rather than an impression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub full_scans: u32,
    pub probes: u32,
    pub probe_failures: u32,
    pub scan_failures: u32,
    /// Times the stamp moved, i.e. the directory genuinely changed.
    pub stamp_moves: u32,
    /// Times an enumeration was suppressed by [`Cadence::min_scan_spacing`].
    /// Non-zero here means something upstream is asking too often.
    pub deferred_scans: u32,
    /// Incremental updates applied from change notifications.
    pub patches: u32,
    /// Times one was suppressed by [`Cadence::patch_spacing`].
    pub deferred_patches: u32,
}

/// Decides what the index actor does, and when.
///
/// Deterministic: given the same cadence, seed, and sequence of [`Input`]s at
/// the same `now` values, it produces the same decisions.
#[derive(Debug)]
pub struct Scheduler {
    cadence: Cadence,
    rng: Rng,
    /// Stamp of the listing currently being served, mirroring the store.
    recorded_stamp: Option<DirStamp>,
    stamp_health: StampHealth,
    last_full_scan: Option<Instant>,
    /// Attempts, not successes. This is what paces retries after a *failed*
    /// scan; keying off successes is what let the old code retry a
    /// million-entry enumeration under a second after it failed.
    last_scan_attempt: Option<Instant>,
    last_patch_attempt: Option<Instant>,
    probe_attempt: u32,
    scan_attempt: u32,
    unanswerable_streak: u32,
    counters: Counters,
    last_scan_reason: Option<ScanReason>,
}

impl Scheduler {
    /// `seed` of `None` seeds the jitter from entropy, which is what the
    /// running application wants; a test passes `Some` and gets a fixed
    /// schedule.
    pub fn new(cadence: Cadence, seed: Option<u64>) -> Self {
        Self {
            cadence,
            rng: seed.map_or_else(Rng::from_entropy, Rng::from_seed),
            recorded_stamp: None,
            stamp_health: StampHealth::Unknown,
            last_full_scan: None,
            last_scan_attempt: None,
            last_patch_attempt: None,
            probe_attempt: 0,
            scan_attempt: 0,
            unanswerable_streak: 0,
            counters: Counters::default(),
            last_scan_reason: None,
        }
    }

    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn stamp_health(&self) -> StampHealth {
        self.stamp_health
    }

    pub fn last_scan_reason(&self) -> Option<ScanReason> {
        self.last_scan_reason
    }

    pub fn recorded_stamp(&self) -> Option<DirStamp> {
        self.recorded_stamp
    }

    /// Folds one input in and returns the next step.
    pub fn on(&mut self, now: Instant, input: Input, have_snapshot: bool) -> Decision {
        match input {
            Input::DiskLoaded {
                stamp,
                captured_age,
            } => self.on_disk_loaded(now, stamp, captured_age),
            Input::Woke { forced } => self.on_woke(now, forced, have_snapshot),
            Input::Probed(result) => self.on_probed(now, result),
            Input::Scanned(result) => self.on_scanned(now, result),
            Input::Changed { full } => self.on_changed(now, full, have_snapshot),
            Input::Applied(result) => self.on_applied(now, result),
        }
    }

    // --- inputs ------------------------------------------------------------

    /// A restored listing counts against the rescan floor from when its data
    /// was captured, not from now.
    ///
    /// That is what lets a warm start probe the stamp and skip the
    /// enumeration. Previously `last_full_scan` began as `None`, the floor was
    /// therefore immediately due, and every single launch re-enumerated the
    /// whole share no matter how good the cache was - which made the disk
    /// cache worth only the few seconds before the scan superseded it.
    ///
    /// A cache with no stamp cannot be proven current, so it deliberately does
    /// *not* satisfy the floor: one scan brings it up to date and establishes
    /// a stamp for next time.
    fn on_disk_loaded(
        &mut self,
        now: Instant,
        stamp: Option<DirStamp>,
        captured_age: Option<Duration>,
    ) -> Decision {
        self.recorded_stamp = stamp;
        if stamp.is_some() {
            self.stamp_health = StampHealth::Working;
            // `checked_sub` fails when the data predates the monotonic clock's
            // origin, which leaves the floor due - the safe direction.
            self.last_full_scan = captured_age.and_then(|age| now.checked_sub(age));
        }
        self.wait(now, now)
    }

    fn on_woke(&mut self, now: Instant, forced: bool, have_snapshot: bool) -> Decision {
        if forced {
            return self.scan(now, ScanReason::Forced);
        }
        if !have_snapshot {
            return self.scan(now, ScanReason::FirstRun);
        }
        if self.stamp_health.is_blind() || self.recorded_stamp.is_none() {
            // Change detection is unavailable. The floor is the whole
            // guarantee, and probing again would only burn round trips.
            return if self.floor_elapsed(now) {
                self.scan(now, ScanReason::Blind)
            } else {
                let at = self.floor_due_at(now);
                self.wait(now, at)
            };
        }
        if self.floor_elapsed(now) {
            return self.scan(now, ScanReason::Floor);
        }
        Decision {
            step: Step::Probe,
            next_action: now,
        }
    }

    /// The watcher has something pending.
    ///
    /// A batch it could describe is patched in; one it could not is a full
    /// pass, paced by `min_scan_spacing` exactly as any other scan - which is
    /// what stops an overflowing share from re-walking itself continuously.
    fn on_changed(&mut self, now: Instant, full: bool, have_snapshot: bool) -> Decision {
        // Nothing to patch into. The first pass has to happen anyway, and
        // running it as a scan is what records the floor the rest depends on.
        if !have_snapshot {
            return self.scan(now, ScanReason::FirstRun);
        }
        if full {
            return self.scan(now, ScanReason::Changed);
        }
        if let Some(last) = self.last_patch_attempt {
            let earliest = last + self.cadence.patch_spacing;
            if now < earliest {
                self.counters.deferred_patches = self.counters.deferred_patches.saturating_add(1);
                return self.wait(now, earliest);
            }
        }
        self.last_patch_attempt = Some(now);
        self.counters.patches = self.counters.patches.saturating_add(1);
        Decision {
            step: Step::ApplyChanges,
            next_action: now,
        }
    }

    /// A patch finished, well or badly.
    ///
    /// Either way the schedule is unchanged: a patch is not a full pass, so it
    /// neither satisfies the floor nor - when it fails - justifies backing the
    /// floor off. The floor is the guarantee that a missed notification costs
    /// at most half an hour, and a failing watcher is precisely when that
    /// guarantee is load-bearing.
    fn on_applied(&mut self, now: Instant, _result: Result<(), EnumError>) -> Decision {
        let at = self.floor_due_at(now);
        self.wait(now, at)
    }

    fn on_probed(&mut self, now: Instant, result: Result<DirStamp, EnumError>) -> Decision {
        self.counters.probes = self.counters.probes.saturating_add(1);
        match result {
            Ok(current) => {
                self.probe_attempt = 0;
                self.unanswerable_streak = 0;
                self.stamp_health = StampHealth::Working;
                match self.recorded_stamp {
                    Some(recorded) if recorded.matches(current) => {
                        let at = self.next_probe(now);
                        self.wait_confirmed(now, at)
                    }
                    _ => {
                        self.counters.stamp_moves = self.counters.stamp_moves.saturating_add(1);
                        self.scan(now, ScanReason::StampMoved)
                    }
                }
            }
            Err(err) => {
                self.counters.probe_failures = self.counters.probe_failures.saturating_add(1);

                if is_probe_refusal(err) {
                    self.unanswerable_streak = self.unanswerable_streak.saturating_add(1);
                    if self.unanswerable_streak >= self.cadence.stamp_failures_before_blind {
                        self.stamp_health = StampHealth::Blind {
                            failures: self.unanswerable_streak,
                        };
                        let at = self.floor_due_at(now);
                        return self.wait(now, at);
                    }
                } else {
                    // A network blip says nothing about whether this share can
                    // do change detection, so it must not disable it.
                    self.unanswerable_streak = 0;
                }

                // Crucially NOT a scan. Backing off keeps the cheap probe
                // cheap, and the floor still bounds how stale the index can
                // get.
                self.probe_attempt = self.probe_attempt.saturating_add(1);
                let delay = self
                    .cadence
                    .probe_backoff
                    .delay(self.probe_attempt, &mut self.rng);
                self.wait(now, now + delay)
            }
        }
    }

    fn on_scanned(
        &mut self,
        now: Instant,
        result: Result<Option<DirStamp>, EnumError>,
    ) -> Decision {
        match result {
            Ok(stamp) => {
                self.counters.full_scans = self.counters.full_scans.saturating_add(1);
                self.probe_attempt = 0;
                self.scan_attempt = 0;
                self.last_full_scan = Some(now);
                self.recorded_stamp = stamp;
                match stamp {
                    Some(_) => {
                        self.unanswerable_streak = 0;
                        self.stamp_health = StampHealth::Working;
                        let at = self.next_probe(now);
                        self.wait(now, at)
                    }
                    None => {
                        // The scan worked but the stamp did not. Without a
                        // stamp there is nothing to compare next time, so
                        // probing is pointless and the floor takes over. The
                        // old code instead re-enumerated on every probe
                        // interval, for the rest of the session - the bug this
                        // module exists to make unrepresentable.
                        self.unanswerable_streak = self.unanswerable_streak.max(1);
                        self.stamp_health = StampHealth::Blind {
                            failures: self.unanswerable_streak,
                        };
                        let at = self.floor_due_at(now);
                        self.wait(now, at)
                    }
                }
            }
            Err(_) => {
                self.counters.scan_failures = self.counters.scan_failures.saturating_add(1);
                self.scan_attempt = self.scan_attempt.saturating_add(1);
                // `delay` is full jitter, so it can return almost zero. The
                // spacing floor is what stops that becoming a tight loop of
                // million-entry enumerations.
                let delay = self
                    .cadence
                    .scan_backoff
                    .delay(self.scan_attempt, &mut self.rng)
                    .max(self.cadence.min_scan_spacing);
                self.wait(now, now + delay)
            }
        }
    }

    // --- helpers -----------------------------------------------------------

    /// Emits a full scan, unless one was started too recently.
    fn scan(&mut self, now: Instant, reason: ScanReason) -> Decision {
        if reason != ScanReason::Forced
            && let Some(earliest) = self.earliest_scan(now)
        {
            self.counters.deferred_scans = self.counters.deferred_scans.saturating_add(1);
            return self.wait(now, earliest);
        }
        self.last_scan_attempt = Some(now);
        self.last_scan_reason = Some(reason);
        Decision {
            step: Step::FullScan(reason),
            next_action: now,
        }
    }

    /// `Some(t)` when a scan must wait until `t` to honour the spacing floor.
    fn earliest_scan(&self, now: Instant) -> Option<Instant> {
        let last = self.last_scan_attempt?;
        let earliest = last + self.cadence.min_scan_spacing;
        (now < earliest).then_some(earliest)
    }

    fn floor_elapsed(&self, now: Instant) -> bool {
        match self.last_full_scan {
            Some(t) => now.saturating_duration_since(t) >= self.cadence.rescan_floor,
            None => true,
        }
    }

    fn floor_due_at(&self, now: Instant) -> Instant {
        match self.last_full_scan {
            Some(t) => t + self.cadence.rescan_floor,
            None => now,
        }
    }

    fn next_probe(&mut self, now: Instant) -> Instant {
        now + jitter(
            self.cadence.probe_interval,
            self.cadence.jitter_percent,
            &mut self.rng,
        )
    }

    fn wait(&self, now: Instant, at: Instant) -> Decision {
        Decision {
            step: Step::Wait,
            next_action: not_before(now, at),
        }
    }

    fn wait_confirmed(&self, now: Instant, at: Instant) -> Decision {
        Decision {
            step: Step::ConfirmFresh,
            next_action: not_before(now, at),
        }
    }
}

/// Clamps a deadline strictly into the future, so the actor always sleeps.
fn not_before(now: Instant, at: Instant) -> Instant {
    at.max(now + TICK)
}

/// True when the error says *this share cannot answer the probe*, as opposed
/// to *the share is unreachable right now*.
///
/// The distinction decides two things: whether change detection gets disabled
/// or merely retried, and whether the user is told their drive is unreachable
/// or that freshness checking is unavailable. Only one of those is true at a
/// time, and telling them the wrong one sends them to check a network that is
/// working.
///
/// Deliberately a whitelist of definite answers about the capability - the
/// same shape as the job-cache miss whitelist in
/// [`crate::index::store::IndexStore::publish_job_miss`], and for the same
/// reason. A network blip must never disable the mechanism that exists to
/// avoid a million-entry enumeration.
pub fn is_probe_refusal(err: EnumError) -> bool {
    matches!(
        err,
        EnumError::Unsupported(_)
            | EnumError::AccessDenied(_)
            | EnumError::NotADirectory(_)
            | EnumError::Corrupt(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: u64 = 0xC0FF_EE00_1234_5678;

    /// Well clear of the monotonic clock's origin, so the tests that need to
    /// talk about the past can subtract without risking an underflow.
    fn base() -> Instant {
        Instant::now() + Duration::from_secs(100_000)
    }

    fn stamp(n: i64) -> DirStamp {
        DirStamp::new(n, n)
    }

    /// Brings a scheduler to the steady state the application spends almost
    /// all of its time in: one snapshot, a working stamp, floor not due.
    fn settled(now: Instant) -> Scheduler {
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        let d = s.on(now, Input::Woke { forced: false }, false);
        assert_eq!(d.step, Step::FullScan(ScanReason::FirstRun));
        s.on(now, Input::Scanned(Ok(Some(stamp(1)))), true);
        s
    }

    // --- the rule table ----------------------------------------------------

    #[test]
    fn a_forced_refresh_always_enumerates() {
        let now = base();
        let mut s = settled(now);
        let d = s.on(now, Input::Woke { forced: true }, true);
        assert_eq!(
            d.step,
            Step::FullScan(ScanReason::Forced),
            "F5 is the user asking, and must never be deferred"
        );
    }

    #[test]
    fn no_snapshot_at_all_enumerates() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        let d = s.on(now, Input::Woke { forced: false }, false);
        assert_eq!(d.step, Step::FullScan(ScanReason::FirstRun));
    }

    #[test]
    fn a_healthy_wake_probes_rather_than_enumerating() {
        let now = base();
        let mut s = settled(now);
        let d = s.on(
            now + Duration::from_secs(60),
            Input::Woke { forced: false },
            true,
        );
        assert_eq!(
            d.step,
            Step::Probe,
            "the whole point is three round trips instead of a million"
        );
    }

    #[test]
    fn an_unchanged_stamp_confirms_freshness_and_reschedules_a_probe() {
        let now = base();
        let mut s = settled(now);
        let at = now + Duration::from_secs(60);
        s.on(at, Input::Woke { forced: false }, true);
        let d = s.on(at, Input::Probed(Ok(stamp(1))), true);
        assert_eq!(d.step, Step::ConfirmFresh);
        assert!(d.next_action > at + Duration::from_secs(53), "{d:?}");
        assert!(d.next_action < at + Duration::from_secs(67), "{d:?}");
    }

    #[test]
    fn a_moved_stamp_enumerates() {
        let now = base();
        let mut s = settled(now);
        let at = now + Duration::from_secs(60);
        s.on(at, Input::Woke { forced: false }, true);
        let d = s.on(at, Input::Probed(Ok(stamp(2))), true);
        assert_eq!(d.step, Step::FullScan(ScanReason::StampMoved));
        assert_eq!(s.counters().stamp_moves, 1);
    }

    #[test]
    fn the_floor_enumerates_without_probing() {
        let now = base();
        let mut s = settled(now);
        let d = s.on(now + FULL_RESCAN_FLOOR, Input::Woke { forced: false }, true);
        assert_eq!(d.step, Step::FullScan(ScanReason::Floor));
    }

    // --- the reload storm --------------------------------------------------

    /// RC1. A share that will not answer the probe used to be re-enumerated on
    /// every probe interval, forever, because "nothing to compare against" was
    /// read as "it changed".
    #[test]
    fn a_share_that_cannot_answer_the_probe_falls_back_to_the_floor() {
        let now = base();
        let mut s = settled(now);
        let mut at = now + Duration::from_secs(60);

        // Two unanswerable probes are enough to declare it blind.
        for _ in 0..2 {
            assert_eq!(
                s.on(at, Input::Woke { forced: false }, true).step,
                Step::Probe
            );
            let d = s.on(at, Input::Probed(Err(EnumError::AccessDenied(5))), true);
            assert_eq!(
                d.step,
                Step::Wait,
                "a failed probe is not evidence the directory changed"
            );
            at = d.next_action;
        }
        assert!(s.stamp_health().is_blind());

        // From here every wake until the floor must do nothing at all.
        let mut scans = 0;
        let mut wakes = 0;
        while at < now + FULL_RESCAN_FLOOR {
            let d = s.on(at, Input::Woke { forced: false }, true);
            if matches!(d.step, Step::FullScan(_)) {
                scans += 1;
            }
            at = d.next_action;
            wakes += 1;
            assert!(wakes < 1000, "the schedule should converge on the floor");
        }
        assert_eq!(
            scans, 0,
            "no enumeration may happen before the floor, however often we wake"
        );

        let d = s.on(now + FULL_RESCAN_FLOOR, Input::Woke { forced: false }, true);
        assert_eq!(d.step, Step::FullScan(ScanReason::Blind));
    }

    /// The same trap reached from the other direction: a scan whose own stamp
    /// capture failed leaves nothing to compare against.
    #[test]
    fn a_scan_that_captured_no_stamp_waits_for_the_floor() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        s.on(now, Input::Woke { forced: false }, false);
        let d = s.on(now, Input::Scanned(Ok(None)), true);
        assert!(s.stamp_health().is_blind());
        assert_eq!(
            d.next_action,
            now + FULL_RESCAN_FLOOR,
            "a stampless listing must wait out the floor, not the probe interval"
        );
    }

    #[test]
    fn a_transient_probe_failure_never_enumerates_and_never_goes_blind() {
        let now = base();
        let mut s = settled(now);
        let mut at = now + Duration::from_secs(60);
        for _ in 0..20 {
            assert_eq!(
                s.on(at, Input::Woke { forced: false }, true).step,
                Step::Probe
            );
            let d = s.on(at, Input::Probed(Err(EnumError::Transient(53))), true);
            assert_eq!(d.step, Step::Wait, "a blip must not cost an enumeration");
            at = d.next_action;
            assert!(at < now + FULL_RESCAN_FLOOR, "stayed inside the floor");
        }
        assert_eq!(
            s.stamp_health(),
            StampHealth::Working,
            "an unreachable share has not lost the ability to answer a probe"
        );
        assert_eq!(s.counters().full_scans, 1, "only the initial scan");
    }

    /// RC2. `last_full_scan` was only set on success, so a failing scan left
    /// the floor permanently "due" and every full-jitter retry - uniform in
    /// `0..=ceiling` - attempted another million-entry enumeration.
    #[test]
    fn a_failing_scan_is_never_retried_faster_than_the_spacing_floor() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        let mut at = now;
        let mut starts = Vec::new();

        for _ in 0..40 {
            let d = s.on(at, Input::Woke { forced: false }, false);
            match d.step {
                Step::FullScan(_) => {
                    starts.push(at);
                    at = s
                        .on(at, Input::Scanned(Err(EnumError::Transient(53))), false)
                        .next_action;
                }
                Step::Wait => at = d.next_action,
                other => panic!("unexpected {other:?}"),
            }
        }

        assert!(starts.len() > 2, "the retries should keep happening");
        for pair in starts.windows(2) {
            assert!(
                pair[1].saturating_duration_since(pair[0]) >= MIN_FULL_SCAN_SPACING,
                "two enumerations started {:?} apart",
                pair[1].saturating_duration_since(pair[0])
            );
        }
    }

    #[test]
    fn the_spacing_floor_defers_a_second_enumeration_but_not_an_f5() {
        let now = base();
        let mut s = settled(now);

        // Force one, then immediately ask again through a non-forced path.
        s.on(now, Input::Woke { forced: true }, true);
        s.on(now, Input::Scanned(Ok(Some(stamp(2)))), true);

        let d = s.on(now, Input::Woke { forced: false }, false);
        assert_eq!(d.step, Step::Wait, "back-to-back enumerations are the bug");
        assert_eq!(s.counters().deferred_scans, 1);

        let d = s.on(now, Input::Woke { forced: true }, true);
        assert_eq!(
            d.step,
            Step::FullScan(ScanReason::Forced),
            "the user pressing F5 outranks the backstop"
        );
    }

    // --- warm start --------------------------------------------------------

    #[test]
    fn a_fresh_cached_listing_is_probed_rather_than_re_enumerated() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        s.on(
            now,
            Input::DiskLoaded {
                stamp: Some(stamp(1)),
                captured_age: Some(Duration::from_secs(120)),
            },
            true,
        );
        let d = s.on(now, Input::Woke { forced: false }, true);
        assert_eq!(
            d.step,
            Step::Probe,
            "a warm start should cost three round trips, not a million"
        );
    }

    #[test]
    fn a_cached_listing_older_than_the_floor_is_re_enumerated() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        s.on(
            now,
            Input::DiskLoaded {
                stamp: Some(stamp(1)),
                captured_age: Some(FULL_RESCAN_FLOOR + Duration::from_secs(60)),
            },
            true,
        );
        let d = s.on(now, Input::Woke { forced: false }, true);
        assert_eq!(d.step, Step::FullScan(ScanReason::Floor));
    }

    #[test]
    fn a_cached_listing_with_no_stamp_is_brought_up_to_date_once() {
        let now = base();
        let mut s = Scheduler::new(Cadence::shipped(), Some(SEED));
        s.on(
            now,
            Input::DiskLoaded {
                stamp: None,
                captured_age: Some(Duration::from_secs(1)),
            },
            true,
        );
        let d = s.on(now, Input::Woke { forced: false }, true);
        assert!(
            matches!(d.step, Step::FullScan(_)),
            "it cannot be proven current, so establish a stamp: {d:?}"
        );
    }

    // --- general properties ------------------------------------------------

    #[test]
    fn every_terminal_decision_is_strictly_in_the_future() {
        let now = base();
        let mut s = settled(now);
        let inputs = [
            Input::Woke { forced: false },
            Input::Probed(Ok(stamp(1))),
            Input::Probed(Err(EnumError::Transient(53))),
            Input::Probed(Err(EnumError::AccessDenied(5))),
            Input::Scanned(Ok(Some(stamp(9)))),
            Input::Scanned(Ok(None)),
            Input::Scanned(Err(EnumError::TimedOut)),
        ];
        for input in inputs {
            let d = s.on(now, input, true);
            if d.step.is_terminal() {
                assert!(
                    d.next_action > now,
                    "{:?} scheduled a wake in the past, which would busy-spin",
                    d.step
                );
            }
        }
    }

    #[test]
    fn recovering_from_blind_restores_the_probe_cadence() {
        let now = base();
        let mut s = settled(now);
        let mut at = now + Duration::from_secs(60);
        for _ in 0..2 {
            s.on(at, Input::Woke { forced: false }, true);
            at = s
                .on(at, Input::Probed(Err(EnumError::Unsupported(50))), true)
                .next_action;
        }
        assert!(s.stamp_health().is_blind());

        // The floor scan manages to capture a stamp this time.
        let at = now + FULL_RESCAN_FLOOR;
        s.on(at, Input::Woke { forced: false }, true);
        let d = s.on(at, Input::Scanned(Ok(Some(stamp(5)))), true);
        assert_eq!(s.stamp_health(), StampHealth::Working);
        assert!(
            d.next_action < at + Duration::from_secs(67),
            "back to the cheap cadence: {d:?}"
        );
    }

    #[test]
    fn a_stamp_of_a_different_kind_counts_as_changed_exactly_once() {
        let now = base();
        let mut s = settled(now);
        let at = now + Duration::from_secs(60);
        s.on(at, Input::Woke { forced: false }, true);
        let d = s.on(at, Input::Probed(Ok(DirStamp::write_only(1))), true);
        assert_eq!(
            d.step,
            Step::FullScan(ScanReason::StampMoved),
            "the second field means something else now, so it cannot be compared"
        );

        // The scan records the new kind, and the comparison settles.
        s.on(at, Input::Scanned(Ok(Some(DirStamp::write_only(1)))), true);
        let at = at + Duration::from_secs(60);
        s.on(at, Input::Woke { forced: false }, true);
        let d = s.on(at, Input::Probed(Ok(DirStamp::write_only(1))), true);
        assert_eq!(d.step, Step::ConfirmFresh);
    }

    #[test]
    fn counters_account_for_every_decision() {
        let now = base();
        let mut s = settled(now);
        let at = now + Duration::from_secs(60);
        s.on(at, Input::Woke { forced: false }, true);
        s.on(at, Input::Probed(Ok(stamp(1))), true);
        let at = at + Duration::from_secs(120);
        s.on(at, Input::Woke { forced: false }, true);
        s.on(at, Input::Probed(Err(EnumError::Transient(53))), true);

        let c = s.counters();
        assert_eq!(c.full_scans, 1);
        assert_eq!(c.probes, 2);
        assert_eq!(c.probe_failures, 1);
        assert_eq!(c.stamp_moves, 0);
    }

    #[test]
    fn the_same_seed_produces_the_same_schedule() {
        let now = base();
        let run = || {
            let mut s = Scheduler::new(Cadence::shipped(), Some(7));
            s.on(now, Input::Woke { forced: false }, false);
            s.on(now, Input::Scanned(Ok(Some(stamp(1)))), true)
                .next_action
        };
        assert_eq!(run(), run());
    }

    // --- cadence -----------------------------------------------------------

    #[test]
    fn the_fast_cadence_keeps_the_shipped_ratios() {
        let f = Cadence::fast();
        assert!(f.probe_interval < f.rescan_floor);
        assert!(f.min_scan_spacing < f.probe_interval);
        assert!(
            f.rescan_floor < Duration::from_secs(1),
            "an 'hour' must pass inside a test"
        );
    }

    /// Compressing the cadence is how a freshness problem gets reproduced in
    /// minutes instead of hours. A fixed thirty-second spacing backstop would
    /// suppress exactly the rescan being reproduced.
    #[test]
    fn compressing_the_cadence_scales_the_spacing_backstop_with_it() {
        let c = Cadence {
            probe_interval: Duration::from_secs(1),
            rescan_floor: Duration::from_secs(6),
            ..Cadence::shipped()
        }
        .normalised();
        assert!(
            c.min_scan_spacing <= c.probe_interval,
            "the backstop must not outrank the cadence it is protecting: {:?}",
            c.min_scan_spacing
        );
    }

    #[test]
    fn the_shipped_spacing_backstop_is_unaffected_by_the_clamp() {
        let c = Cadence::shipped().normalised();
        assert_eq!(
            c.min_scan_spacing, MIN_FULL_SCAN_SPACING,
            "the clamp is for compressed cadences, not for production"
        );
    }

    #[test]
    fn a_probe_interval_longer_than_the_floor_is_clamped() {
        let c = Cadence {
            probe_interval: Duration::from_secs(9_999),
            rescan_floor: Duration::from_secs(60),
            ..Cadence::shipped()
        }
        .normalised();
        assert_eq!(
            c.probe_interval,
            Duration::from_secs(60),
            "probing less often than rescanning would make the probe pointless"
        );
    }

    #[test]
    fn a_probe_refusal_is_a_whitelist_of_definite_answers() {
        for definite in [
            EnumError::Unsupported(50),
            EnumError::AccessDenied(5),
            EnumError::NotADirectory(267),
            EnumError::Corrupt(13),
        ] {
            assert!(
                is_probe_refusal(definite),
                "{definite:?} is a definite answer"
            );
        }
        for indefinite in [
            EnumError::Transient(53),
            EnumError::TimedOut,
            EnumError::Cancelled,
            EnumError::PathNotFound(3),
            EnumError::Other(999),
            EnumError::Empty,
        ] {
            assert!(
                !is_probe_refusal(indefinite),
                "{indefinite:?} says nothing about the share's capabilities"
            );
        }
    }

    #[test]
    fn scan_reasons_all_have_a_label() {
        for r in [
            ScanReason::FirstRun,
            ScanReason::Forced,
            ScanReason::StampMoved,
            ScanReason::Floor,
            ScanReason::Blind,
        ] {
            assert!(!r.label().is_empty());
        }
    }
}
