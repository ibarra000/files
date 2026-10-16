//! The thread that owns one mapping's index.
//!
//! Single writer: every mutation of this mapping's shared index happens here.
//!
//! # Freshness without rescanning
//!
//! A flat share's directory timestamps are probed about once a minute (about
//! three round trips), and a full enumeration happens only when they move.
//! The probe also proves to [`crate::search::verify`] that the local index is
//! still authoritative.
//!
//! Timing decisions live in [`super::schedule`], which reads no clock and does
//! no I/O. This module is the driver: it waits, performs the [`Step`] it is
//! given, and reports the outcome back.
//!
//! # Waiting
//!
//! The loop blocks on the command channel with a deadline and never sleeps,
//! so F5 interrupts a long backoff immediately.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use super::builder::SnapshotBuilder;
use super::enumerate::{DirSource, ListOpts};
use super::errors::EnumError;
use super::log::{IndexLog, Record};
use super::permit::{Acquired, WalkPermits};
use super::persist;
use super::schedule::{Cadence, Decision, Input, Scheduler, Step, is_probe_refusal};
use super::store::{Activity, DegradeReason, IndexStore, MappingSlot, Origin, TreeCoverage};
use super::watch::{self, ChangeWatcher, WatchQueue};
use super::{DirStamp, Snapshot};
use super::{tree, walk};
use crate::app::event::{AppEvent, Events, IndexMsg};
use crate::config::Settings;
use crate::paths::{MappingId, MappingKind};
use crate::util::cancel::CancelToken;

/// Minimum interval between progress reports. Rate limiting at the source
/// keeps the UI channel far from full.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const PROGRESS_ENTRIES: usize = 50_000;

/// Consecutive empty enumerations required before a populated index is
/// replaced with an empty one.
///
/// An empty result is legitimate, but a half-open SMB session can also
/// produce one, and discarding the whole index on one such answer is not
/// recoverable. The second opinion costs one scan-backoff interval.
const EMPTY_CONFIRMATIONS: u32 = 2;

/// Upper bound on steps taken for one wake-up.
///
/// The longest legitimate chain is `Woke -> Probe -> Probed -> FullScan ->
/// Scanned -> Wait` (five). The bound turns a non-terminating rule into a
/// caught assertion rather than a thread spinning against a file server.
const MAX_STEPS_PER_WAKE: usize = 6;

#[derive(Debug, Clone)]
pub enum IndexCmd {
    /// Re-examine the share. `force` skips the stamp check.
    Refresh {
        force: bool,
    },
    /// The watch queue has something in it. Carries no payload because the
    /// dirty set lives in the queue, so a dropped message costs only a
    /// wake-up.
    Changed,
    Shutdown,
}

/// Handle to the running actor.
pub struct IndexActor {
    /// Which share this actor owns, so a refresh can name one.
    mapping: MappingId,
    tx: Sender<IndexCmd>,
    /// Set from the caller's thread and checked from inside a running scan.
    ///
    /// The command channel cannot deliver a shutdown during a scan, because
    /// the actor is both its only reader and the thing scanning. A tree walk
    /// can take minutes, so this flag is how shutdown reaches it.
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Kept so shutdown can unblock it: a thread blocked in
    /// `ReadDirectoryChangesW` cannot poll a cancellation flag.
    watcher: Option<Arc<dyn ChangeWatcher>>,
    pump: Option<JoinHandle<()>>,
}

impl IndexActor {
    pub fn mapping(&self) -> MappingId {
        self.mapping
    }

    /// Asks for a refresh.
    ///
    /// A full channel drops the request; the actor coalesces queued requests
    /// in `next_wake`, so one pending refresh is enough.
    pub fn refresh(&self, force: bool) {
        match self.tx.try_send(IndexCmd::Refresh { force }) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Tells the actor to stop, without waiting for it.
    ///
    /// Separate from [`Self::join`] so a process with many actors can signal
    /// all of them and then wait once, keeping the shutdown budget global
    /// rather than per thread.
    pub fn begin_shutdown(&mut self) {
        // Set the flag before sending, so a scan between directories when the
        // message lands has already been told to stop.
        self.stop.store(true, Ordering::Relaxed);
        // Stop the watcher before the actor: its thread is blocked in a call
        // the flag cannot reach.
        if let Some(watcher) = &self.watcher {
            watcher.stop();
        }
        let _ = self.tx.send(IndexCmd::Shutdown);
    }

    /// Waits for the actor to finish, up to `deadline`.
    pub fn join(&mut self, deadline: Instant) -> bool {
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
        let Some(handle) = self.handle.take() else {
            return true;
        };
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                // A thread blocked in an SMB call cannot be woken. Leaking it
                // is safe: index writes are temp-then-rename, so nothing is
                // left half-written.
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.join().is_ok()
    }

    pub fn shutdown(&mut self, budget: Duration) -> bool {
        self.begin_shutdown();
        self.join(Instant::now() + budget)
    }
}

/// Everything the actor needs.
pub struct IndexContext {
    pub settings: Settings,
    /// Which mapping this actor owns.
    ///
    /// One actor per mapping, so a long walk or a backoff on one share never
    /// delays another, and each slot still has exactly one writer. Only the
    /// id is held; path, kind and name are read from the slot, so the actor
    /// cannot mix one mapping's settings with another's state.
    pub mapping: MappingId,
    pub store: Arc<IndexStore>,
    pub source: Arc<dyn DirSource>,
    /// Volume serial resolved at startup, if it was available then.
    pub volume_serial: Option<u32>,
    pub cadence: Cadence,
    /// Fixed jitter seed. `None` seeds from entropy; tests pass `Some` for a
    /// fixed schedule.
    pub rng_seed: Option<u64>,
    pub log: Arc<IndexLog>,
    /// Live change notifications, when available. The actor reads only the
    /// queue; the watcher thread fills it.
    pub watch: Option<Arc<WatchQueue>>,
    /// The watcher feeding [`Self::watch`], kept so shutdown can unblock it.
    pub watcher: Option<Arc<dyn ChangeWatcher>>,
    /// Shared cap on how many shares walk at once. See [`super::permit`].
    ///
    /// Defaults to a private set so single-actor tests need no setup; the
    /// running program gives every actor the same set.
    pub permits: Arc<WalkPermits>,
}

impl IndexContext {
    pub fn new(
        settings: Settings,
        mapping: MappingId,
        store: Arc<IndexStore>,
        source: Arc<dyn DirSource>,
    ) -> Self {
        // The cadence follows the mapping kind, so a tree can never get the
        // flat schedule by mistake.
        let kind = store
            .slot(mapping)
            .map(|s| s.kind())
            .unwrap_or(MappingKind::Flat);
        let cadence = match kind {
            MappingKind::Tree => Cadence::tree(),
            MappingKind::Flat => Cadence::shipped(),
            // Unreachable while `Actors::start` spawns actors only for indexed
            // mappings. Spelled out because the flat cadence would walk a
            // live share on its first wake.
            MappingKind::Live => Cadence::tree(),
        };
        let settings_max_scans = settings.max_concurrent_scans;
        Self {
            settings,
            mapping,
            store,
            source,
            // Resolved on the actor thread before the cache is read, so
            // startup never blocks on possibly-dead shares.
            volume_serial: None,
            cadence: cadence.from_env(),
            rng_seed: None,
            log: Arc::new(IndexLog::disabled()),
            watch: None,
            watcher: None,
            permits: Arc::new(WalkPermits::new(settings_max_scans)),
        }
    }

    /// Pins the volume serial instead of resolving it from the share.
    ///
    /// Used by tests to simulate a drive letter remapped to another volume.
    pub fn with_volume_serial(mut self, serial: u32) -> Self {
        self.volume_serial = Some(serial);
        self
    }

    /// Shares one cap across every actor, rather than one cap each.
    pub fn with_permits(mut self, permits: Arc<WalkPermits>) -> Self {
        self.permits = permits;
        self
    }

    /// Feeds this actor live change notifications.
    pub fn with_watch(mut self, watcher: Arc<dyn ChangeWatcher>, queue: Arc<WatchQueue>) -> Self {
        self.watcher = Some(watcher);
        self.watch = Some(queue);
        self
    }

    /// Attaches OS change notifications, if enabled and the share allows it.
    ///
    /// A refusal is recorded in the queue and reported in the status line on
    /// the next wake; the rescan floor still applies.
    #[cfg(windows)]
    pub fn with_live_updates(self, settings: &Settings) -> Self {
        use super::win_watch::DirectoryWatcher;

        if !settings.live_updates || !self.is_tree() {
            return self;
        }
        let queue = Arc::new(WatchQueue::default());
        match DirectoryWatcher::open(self.slot().dir()) {
            Ok(watcher) => self.with_watch(Arc::new(watcher), queue),
            Err(why) => {
                queue.record(super::watch::WatchEvent::Unavailable(why), Instant::now());
                // Attach the queue anyway: it carries the reason, so the status
                // line reports that live updates are off.
                Self {
                    watch: Some(queue),
                    ..self
                }
            }
        }
    }

    #[cfg(not(windows))]
    pub fn with_live_updates(self, _settings: &Settings) -> Self {
        self
    }

    // --- which index this actor is talking about ---------------------------
    //
    // Every accessor below routes through `slot()`, so an actor reads and
    // writes only its own mapping's state.

    /// This actor's slot.
    pub fn slot(&self) -> &super::store::MappingSlot {
        self.store
            .slot(self.mapping)
            .expect("an actor owns a configured slot")
    }

    fn is_tree(&self) -> bool {
        self.slot().is_tree()
    }

    /// Returns whether a timer may re-read this share, or only the user and
    /// the watcher may.
    fn refresh(&self) -> crate::paths::RefreshPolicy {
        self.settings
            .routes
            .get(self.mapping)
            .map(|m| m.refresh)
            .unwrap_or_default()
    }

    /// Returns whether this actor's index holds anything yet.
    fn have_index(&self) -> bool {
        self.slot().has_index()
    }

    fn status(&self) -> Arc<super::store::IndexStatus> {
        self.slot().status()
    }

    fn set_activity(&self, activity: Activity) {
        self.slot().set_activity(activity);
    }

    fn note_scan_started(&self, reason: super::schedule::ScanReason) {
        self.slot().note_scan_started(reason);
    }

    fn note_schedule(
        &self,
        counters: super::schedule::Counters,
        health: super::schedule::StampHealth,
    ) {
        self.slot().note_schedule(counters, health);
    }

    fn note_cache_rejected(&self, why: String) {
        self.slot().note_cache_rejected(why);
    }

    fn record_failure(&self, err: EnumError, attempt: u32, next_retry_at: Instant) {
        self.slot().record_failure(err, attempt, next_retry_at);
    }

    fn set_degraded(&self, reason: DegradeReason) {
        self.slot().set_degraded(reason);
    }

    fn confirm_fresh(&self, stamp: Option<DirStamp>, at: SystemTime) {
        self.slot().confirm_fresh(stamp, at);
    }

    fn note_live_updates_unavailable(&self) {
        self.slot().note_live_updates_unavailable();
        // On a manually refreshed share a dead watch means nothing arrives at
        // all, so the index is stale, not merely degraded. An auto share
        // still has the floor.
        if self.refresh().is_manual() {
            self.slot()
                .note_stale(super::store::StaleReason::NoLiveUpdates);
        }
    }

    fn note_stale(&self, reason: super::store::StaleReason) {
        self.slot().note_stale(reason);
    }

    pub fn with_cadence(mut self, cadence: Cadence) -> Self {
        self.cadence = cadence;
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng_seed = Some(seed);
        self
    }

    pub fn with_log(mut self, log: Arc<IndexLog>) -> Self {
        self.log = log;
        self
    }
}

/// Starts the actor.
pub fn spawn(ctx: IndexContext, events: Events) -> std::io::Result<IndexActor> {
    let (tx, rx) = crossbeam_channel::bounded(16);
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = CancelToken::from_flag(Arc::clone(&stop));
    let watcher = ctx.watcher.clone();
    // Read before `ctx` moves into the thread.
    let mapping = ctx.mapping;
    let pump = spawn_pump(&ctx, tx.clone(), &cancel, &events)?;
    let handle = std::thread::Builder::new()
        .name("files-index".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(ctx, rx, &events, &cancel);
            }));
            if let Err(_payload) = result {
                let _ = events.send(AppEvent::ActorDied {
                    actor: "index",
                    detail: "panicked".into(),
                });
            }
        })?;
    Ok(IndexActor {
        mapping,
        tx,
        stop,
        handle: Some(handle),
        watcher,
        pump,
    })
}

/// Starts the thread that drains the watcher into the queue.
///
/// A separate thread because `next_event` blocks, possibly for hours on an
/// outstanding SMB `CHANGE_NOTIFY`. It only folds events into the queue and
/// nudges the actor, so a lost nudge costs a wake-up, not a change.
fn spawn_pump(
    ctx: &IndexContext,
    tx: Sender<IndexCmd>,
    cancel: &CancelToken,
    events: &Events,
) -> std::io::Result<Option<JoinHandle<()>>> {
    let (Some(watcher), Some(queue)) = (ctx.watcher.clone(), ctx.watch.clone()) else {
        return Ok(None);
    };
    let cancel = cancel.clone();
    let events = events.clone();
    let handle = std::thread::Builder::new()
        .name("files-watch".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                watch::pump(watcher.as_ref(), &queue, &cancel, || {
                    // Dropping on a full channel is fine: the actor reads the
                    // queue, not the message.
                    let _ = tx.try_send(IndexCmd::Changed);
                });
            }));
            if result.is_err() {
                let _ = events.send(AppEvent::ActorDied {
                    actor: "watch",
                    detail: "panicked".into(),
                });
            }
        })?;
    Ok(Some(handle))
}

/// What ended the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wake {
    Timer,
    Refresh { force: bool },
    Changed,
    Shutdown,
}

/// A failure not yet reported to the store, because the retry time is known
/// only after the scheduler decides.
#[derive(Debug, Clone, Copy)]
struct PendingFailure {
    err: EnumError,
    /// True when the cheap probe failed rather than the scan.
    from_probe: bool,
}

fn run(mut ctx: IndexContext, rx: Receiver<IndexCmd>, events: &Events, cancel: &CancelToken) {
    // A tree cannot answer a stamp probe and never probes, so the scheduler
    // is told up front to measure the floor from the cache's capture time.
    let mut sched = Scheduler::new(ctx.cadence, ctx.rng_seed);
    if ctx.is_tree() {
        sched = sched.without_stamps();
    }
    if ctx.refresh().is_manual() {
        sched = sched.on_demand_only();
    }
    // This mapping's directory; everything below probes and reads this path.
    let dir = indexed_dir(&ctx).to_path_buf();
    let mut serial = ctx.volume_serial;
    let mut failures: u32 = 0;
    let mut empty_streak: u32 = 0;
    // When unsaved patches began. A manually refreshed share is kept current
    // only by the watcher, so unsaved patches would be lost on restart with
    // nothing to re-walk them.
    let mut unsaved_since: Option<Instant> = None;

    // Warm the SMB session before anything needs it. First contact with a
    // mapped drive can cost seconds of session setup and DFS resolution.
    ctx.source.prewarm(&dir);

    // Resolved here, before the cache is read: `load_from_disk` uses it to
    // check the drive letter still points at the cache's volume, and doing it
    // on this thread keeps volume queries off the UI thread.
    ctx.volume_serial = volume_serial_for(&ctx, &mut serial, &dir);

    // Serve the previous run's index immediately, then refresh.
    let loaded = load_from_disk(&ctx, events);
    let mut decision = sched.on(
        Instant::now(),
        Input::DiskLoaded {
            stamp: loaded.and_then(|l| l.stamp),
            captured_age: loaded.and_then(|l| l.age),
        },
        ctx.have_index(),
    );
    log_decision(&ctx, &sched, "start", &describe_load(loaded), &decision);

    loop {
        // Wake at whichever comes first: the scheduler's deadline or the
        // watch queue's debounce. The debounce belongs to the queue, so it is
        // folded in here rather than into `schedule.rs`.
        let deadline = match ctx.watch.as_ref().and_then(|q| q.due_at()) {
            Some(due) => decision.next_action.min(due),
            None => decision.next_action,
        };
        let mut input = match next_wake(&rx, deadline) {
            Wake::Shutdown => {
                // Save pending patches before exiting; with no timed re-walk
                // those folders would otherwise stay stale.
                if unsaved_since.is_some() {
                    persist_current(&ctx, events);
                }
                return;
            }
            Wake::Refresh { force } => Input::Woke { forced: force },
            Wake::Timer | Wake::Changed => match pending_changes(&ctx, Instant::now()) {
                Some(full) => {
                    // `full` means the watcher lost events, so only a full
                    // pass can say what the share holds. On a manual share,
                    // mark it stale rather than start that pass.
                    if full && ctx.refresh().is_manual() {
                        ctx.note_stale(super::store::StaleReason::EventsLost);
                    }
                    Input::Changed { full }
                }
                None => Input::Woke { forced: false },
            },
        };
        // Mutable because a forced refresh can arrive while queued for a walk
        // permit; it applies to the pass about to start.
        let mut forced = matches!(input, Input::Woke { forced: true });
        let mut event = match input {
            Input::Woke { forced: true } => "f5",
            Input::Changed { .. } => "watch",
            _ => "timer",
        };
        let mut detail = String::new();
        let mut pending: Option<PendingFailure> = None;

        for step in 0..MAX_STEPS_PER_WAKE {
            let now = Instant::now();
            decision = sched.on(now, input, ctx.have_index());
            ctx.note_schedule(sched.counters(), sched.stamp_health());

            // Reported only now because the retry time, shown as a countdown
            // in the status line, is part of the decision.
            if let Some(f) = pending.take() {
                apply_failure(&ctx, f, &mut failures, decision.next_action);
            }
            log_decision(&ctx, &sched, event, &detail, &decision);

            match decision.step {
                Step::Probe => {
                    let result = ctx.source.probe_stamp(&dir);
                    detail = describe_probe(sched.recorded_stamp(), &result);
                    if let Err(err) = result {
                        pending = Some(PendingFailure {
                            err,
                            from_probe: true,
                        });
                    }
                    input = Input::Probed(result);
                    event = "probe";
                }
                Step::ApplyChanges => {
                    let outcome = apply_changes(&ctx, events, &mut detail, cancel);
                    if let Err(err) = outcome {
                        pending = Some(PendingFailure {
                            err,
                            from_probe: false,
                        });
                    } else {
                        // Saves are paced: a patch is a few round trips, but
                        // the cache file is rewritten whole.
                        let since = *unsaved_since.get_or_insert(now);
                        if now.saturating_duration_since(since) >= ctx.cadence.persist_spacing {
                            persist_current(&ctx, events);
                            unsaved_since = None;
                        }
                    }
                    input = Input::Applied(outcome);
                    event = "patch";
                }
                Step::FullScan(reason) => {
                    // A tree pass is the most expensive operation, so it
                    // queues behind the shared cap, showing `Queued` meanwhile.
                    // An incremental patch takes no permit; it is seconds of
                    // work and must not wait behind minutes of walks.
                    let permit = if ctx.is_tree() {
                        ctx.set_activity(Activity::Queued);
                        publish_status(&ctx, events);
                        match ctx.permits.acquire(&rx, &mut forced) {
                            // Told to stop while queued. No permit was taken.
                            Acquired::Shutdown => return,
                            Acquired::Held(p) => Some(p),
                        }
                    } else {
                        None
                    };
                    // The full pass covers anything the watcher had pending,
                    // so drop it rather than trigger a second pass afterwards.
                    if let Some(queue) = &ctx.watch {
                        let _ = queue.take();
                    }
                    ctx.note_scan_started(reason);
                    let outcome = scan_and_publish(
                        &ctx,
                        events,
                        &mut serial,
                        &mut empty_streak,
                        forced,
                        &mut detail,
                        cancel,
                    );
                    // Release the permit right after the pass; it must never
                    // be held when the loop reaches `Step::Wait`.
                    drop(permit);
                    if let Err(err) = outcome {
                        pending = Some(PendingFailure {
                            err,
                            from_probe: false,
                        });
                    } else {
                        failures = 0;
                        // `scan_and_publish` persists on the way out, covering
                        // every earlier patch.
                        unsaved_since = None;
                    }
                    input = Input::Scanned(outcome);
                    event = "scan";
                }
                Step::ConfirmFresh => {
                    // The common case: nothing moved, so the listing is still
                    // correct and no enumeration is needed.
                    ctx.confirm_fresh(sched.recorded_stamp(), SystemTime::now());
                    failures = 0;
                    break;
                }
                Step::Wait => break,
            }

            debug_assert!(
                step + 1 < MAX_STEPS_PER_WAKE,
                "the scheduler did not terminate within {MAX_STEPS_PER_WAKE} steps"
            );
        }

        // Re-asserted every wake: a successful walk publishes a fresh health
        // verdict, which does not mean a dead watch came back.
        if ctx
            .watch
            .as_ref()
            .is_some_and(|q| q.unavailable().is_some())
        {
            ctx.note_live_updates_unavailable();
        }

        // Safety net: a non-terminal decision has `next_action == now`, and
        // leaving it would spin this thread against the file server.
        if !decision.step.is_terminal() {
            debug_assert!(false, "unterminated decision: {:?}", decision.step);
            decision.next_action = Instant::now() + ctx.cadence.probe_interval;
        }

        publish_status(&ctx, events);
    }
}

/// Returns whether the watch queue has an actionable batch, and whether it
/// needs a full re-walk.
///
/// Does not consume the set: the scheduler may defer the update, and a
/// consumed set would then be lost.
fn pending_changes(ctx: &IndexContext, now: Instant) -> Option<bool> {
    ctx.watch.as_ref()?.due(now)
}

/// Blocks until the deadline or a command, coalescing queued refreshes into
/// one wake.
fn next_wake(rx: &Receiver<IndexCmd>, deadline: Instant) -> Wake {
    let mut refresh = false;
    let mut force = false;
    match rx.recv_deadline(deadline) {
        Err(RecvTimeoutError::Timeout) => return Wake::Timer,
        Err(RecvTimeoutError::Disconnected) | Ok(IndexCmd::Shutdown) => return Wake::Shutdown,
        Ok(IndexCmd::Refresh { force: f }) => {
            refresh = true;
            force = f;
        }
        Ok(IndexCmd::Changed) => {}
    }
    loop {
        match rx.try_recv() {
            Ok(IndexCmd::Refresh { force: f }) => {
                refresh = true;
                force |= f;
            }
            // A change notification never outranks a refresh: an explicit F5
            // re-reads the share, which covers whatever the watcher saw.
            Ok(IndexCmd::Changed) => {}
            Ok(IndexCmd::Shutdown) => return Wake::Shutdown,
            Err(_) => {
                return if refresh {
                    Wake::Refresh { force }
                } else {
                    Wake::Changed
                };
            }
        }
    }
}

/// Reflects a failure in the shared status.
///
/// A probe the share refused is reported as degraded, not unreachable: the
/// drive works but will not answer the freshness check.
fn apply_failure(
    ctx: &IndexContext,
    failure: PendingFailure,
    failures: &mut u32,
    retry_at: Instant,
) {
    if failure.from_probe && is_probe_refusal(failure.err) {
        ctx.set_degraded(DegradeReason::StampUnreliable);
        return;
    }
    *failures = failures.saturating_add(1);
    ctx.record_failure(failure.err, *failures, retry_at);
}

/// What [`load_from_disk`] recovered.
#[derive(Debug, Clone, Copy)]
struct DiskLoad {
    stamp: Option<DirStamp>,
    /// Wall-clock age of the restored listing's data.
    age: Option<Duration>,
}

fn load_from_disk(ctx: &IndexContext, events: &Events) -> Option<DiskLoad> {
    if !ctx.settings.persist {
        return None;
    }
    let cache_dir = ctx.settings.cache_dir.clone()?;

    ctx.set_activity(Activity::LoadingDisk);
    publish_status(ctx, events);

    let dir = indexed_dir(ctx);
    let key = persist::MappingKey::of(dir);
    let expect = persist::Expect::new(dir, ctx.volume_serial);

    // The loader refuses to read a tree file as flat or vice versa. The cache
    // key already differs per directory; this is a backstop.
    let result = if ctx.is_tree() {
        load_tree_from_disk(ctx, events, &cache_dir, key, expect)
    } else {
        load_flat_from_disk(ctx, events, &cache_dir, key, expect)
    };

    let loaded = match result {
        Ok(loaded) => Some(loaded),
        Err(err) => {
            // A missing cache is normal on first run. A rejected one is
            // recorded so it is distinguishable from a cold start.
            if !matches!(err, persist::LoadError::Missing) {
                ctx.note_cache_rejected(err.to_string());
            }
            None
        }
    };

    ctx.set_activity(Activity::Idle);
    publish_status(ctx, events);
    loaded
}

fn load_flat_from_disk(
    ctx: &IndexContext,
    events: &Events,
    cache_dir: &Path,
    key: persist::MappingKey,
    expect: persist::Expect<'_>,
) -> Result<DiskLoad, persist::LoadError> {
    let snapshot = persist::load(cache_dir, key, expect)?;
    let stamp = snapshot.stamp();
    let age = SystemTime::now()
        .duration_since(snapshot.captured_at())
        .ok();
    ctx.slot()
        .publish_flat(Arc::new(snapshot), Origin::DiskCache);
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    Ok(DiskLoad { stamp, age })
}

fn load_tree_from_disk(
    ctx: &IndexContext,
    events: &Events,
    cache_dir: &Path,
    key: persist::MappingKey,
    expect: persist::Expect<'_>,
) -> Result<DiskLoad, persist::LoadError> {
    let loaded = persist::load_tree(cache_dir, key, expect)?;
    let age = SystemTime::now()
        .duration_since(loaded.index.captured_at())
        .ok();
    // Publish the cached coverage with the index, so a cache from a partial
    // walk still reports its gaps after a restart.
    ctx.slot().publish_tree(
        Arc::new(loaded.index),
        Origin::DiskCache,
        Some(Arc::new(loaded.coverage)),
    );
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    // A tree records no directory stamp, so the scheduler settles into
    // `Blind` and the re-walk floor is the freshness guarantee.
    Ok(DiskLoad { stamp: None, age })
}

fn persist_current(ctx: &IndexContext, events: &Events) {
    if !ctx.settings.persist {
        return;
    }
    let Some(cache_dir) = ctx.settings.cache_dir.clone() else {
        return;
    };
    let key = persist::MappingKey::of(indexed_dir(ctx));

    ctx.set_activity(Activity::Persisting);
    publish_status(ctx, events);
    // Best effort: a failed write costs only a slower cold start next time.
    if ctx.is_tree() {
        if let Some(index) = ctx.slot().as_tree() {
            let coverage = ctx.status().coverage.clone();
            let _ = persist::save_tree(&cache_dir, key, &index, coverage.as_deref());
        }
    } else if let Some(snapshot) = ctx.slot().as_flat() {
        let _ = persist::save(&cache_dir, key, &snapshot);
    }
    ctx.set_activity(Activity::Idle);
    publish_status(ctx, events);
}

/// Enumerates, decides whether to trust the result, and publishes it.
///
/// Returns the stamp recorded with the listing, which tells the scheduler
/// whether change detection is available.
fn scan_and_publish(
    ctx: &IndexContext,
    events: &Events,
    serial: &mut Option<u32>,
    empty_streak: &mut u32,
    forced: bool,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<Option<DirStamp>, EnumError> {
    if ctx.is_tree() {
        return walk_and_publish(ctx, events, forced, detail, cancel);
    }
    let previous_entries = ctx.status().entries;
    let (snapshot, elapsed) = match full_scan(ctx, events, serial, cancel) {
        Ok(v) => v,
        Err(err) => {
            *detail = format!("scan failed: {err}");
            if forced {
                let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
                    id: ctx.mapping,
                    entries: 0,
                    elapsed: Duration::ZERO,
                    error: Some(err),
                }));
            }
            // Keep the previous snapshot: stale results with an accurate label
            // beat an empty screen.
            return Err(err);
        }
    };

    let entries = snapshot.len();
    if entries == 0 && previous_entries > 0 {
        *empty_streak = empty_streak.saturating_add(1);
        if *empty_streak < EMPTY_CONFIRMATIONS {
            *detail = format!(
                "refused an empty listing over {previous_entries} entries (opinion {} of {EMPTY_CONFIRMATIONS})",
                *empty_streak
            );
            return Err(EnumError::Empty);
        }
        *detail = format!("accepted an empty listing after {empty_streak} consistent answers");
    } else {
        *empty_streak = 0;
        *detail = format!(
            "{entries} entries in {}",
            crate::util::humanize::elapsed(elapsed)
        );
    }

    let stamp = snapshot.stamp();
    ctx.slot().publish_flat(Arc::new(snapshot), Origin::Network);
    if stamp.is_none() {
        // Set after the publish, which resets health. Without a stamp the
        // scheduler falls back to the rescan floor, and the user is told why.
        ctx.set_degraded(DegradeReason::StampUnreliable);
    }
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));

    if forced {
        let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
            id: ctx.mapping,
            entries,
            elapsed,
            error: None,
        }));
    }

    persist_current(ctx, events);
    Ok(stamp)
}

/// Returns the directory this actor indexes.
fn indexed_dir(ctx: &IndexContext) -> &Path {
    ctx.slot().dir()
}

/// Returns the volume serial for the persisted index's identity check.
///
/// Retried on the actor thread when not yet known, since an earlier attempt
/// may have raced SMB session warm-up.
fn volume_serial_for(ctx: &IndexContext, cached: &mut Option<u32>, dir: &Path) -> Option<u32> {
    if cached.is_none() && ctx.settings.persist {
        *cached = resolve_volume_serial(dir);
    }
    *cached
}

#[cfg(windows)]
fn resolve_volume_serial(dir: &Path) -> Option<u32> {
    super::volume::volume_serial(dir)
}

#[cfg(not(windows))]
fn resolve_volume_serial(_dir: &Path) -> Option<u32> {
    None
}

fn full_scan(
    ctx: &IndexContext,
    events: &Events,
    serial: &mut Option<u32>,
    cancel: &CancelToken,
) -> Result<(Snapshot, Duration), EnumError> {
    let started = Instant::now();
    let dir = indexed_dir(ctx).to_path_buf();
    let prefix = dir.to_string_lossy().into_owned();

    // Probe the stamp before listing and record that value. A stamp read
    // afterwards could hide a change that landed mid-scan; reading it first
    // errs toward one extra rescan instead of a silently incomplete index.
    let stamp = ctx.source.probe_stamp(&dir).ok();

    // Size the arenas from the previous listing, so later runs do not grow
    // the buffers.
    let (hint_entries, hint_avg) = ctx
        .slot()
        .as_flat()
        .map(|s| {
            let n = s.len().max(1);
            (s.len(), (s.lower().len() / n).saturating_sub(1))
        })
        .unwrap_or((0, 0));

    let mut builder = SnapshotBuilder::with_capacity(&prefix, hint_entries, hint_avg);
    let mut progress = ProgressSink {
        inner: &mut builder,
        events,
        slot: ctx.slot(),
        last_report: Instant::now(),
        last_count: 0,
    };

    ctx.set_activity(Activity::Scanning { seen: 0 });
    publish_status(ctx, events);

    let result = ctx.source.list(
        &dir,
        &mut progress,
        &ListOpts::default().hiding_system(ctx.settings.hidden.hides_system()),
        cancel,
    );
    match result {
        Ok(_) | Err(EnumError::Empty) => {}
        Err(err) => {
            ctx.set_activity(Activity::Idle);
            return Err(err);
        }
    }

    let serial = volume_serial_for(ctx, serial, &dir).unwrap_or(0);
    let snapshot = builder.finish(SystemTime::now(), serial, stamp);
    ctx.set_activity(Activity::Idle);
    Ok((snapshot, started.elapsed()))
}

fn publish_status(ctx: &IndexContext, events: &Events) {
    // Droppable: the next status supersedes this one, so a full channel is
    // not worth blocking the scan for.
    let _ = events.try_send(AppEvent::Index(IndexMsg::Status {
        id: ctx.mapping,
        status: ctx.status(),
    }));
}

fn log_decision(
    ctx: &IndexContext,
    sched: &Scheduler,
    event: &str,
    detail: &str,
    decision: &Decision,
) {
    if !ctx.log.is_enabled() {
        return;
    }
    let wait = decision.step.is_terminal().then(|| {
        decision
            .next_action
            .saturating_duration_since(Instant::now())
    });
    ctx.log.record(&Record {
        event,
        detail,
        step: decision.step,
        wait,
        counters: sched.counters(),
        stamp_health: sched.stamp_health(),
    });
}

fn describe_load(loaded: Option<DiskLoad>) -> String {
    match loaded {
        None => "no cached index".into(),
        Some(l) => format!(
            "restored cache, stamp {}, age {}",
            match l.stamp {
                Some(s) => format!("{},{} ({})", s.last_write, s.change_time, s.kind.label()),
                None => "none".into(),
            },
            l.age
                .map(crate::util::humanize::elapsed)
                .unwrap_or_else(|| "unknown".into())
        ),
    }
}

fn describe_probe(recorded: Option<DirStamp>, result: &Result<DirStamp, EnumError>) -> String {
    let show = |s: Option<DirStamp>| match s {
        Some(s) => format!("{},{}", s.last_write, s.change_time),
        None => "none".into(),
    };
    match result {
        Ok(current) => format!("stamp {} -> {}", show(recorded), show(Some(*current))),
        Err(err) => format!("probe failed: {err}"),
    }
}

/// Wraps the builder to emit rate-limited progress while a long scan runs.
struct ProgressSink<'a> {
    inner: &'a mut SnapshotBuilder,
    events: &'a Events,
    slot: &'a MappingSlot,
    last_report: Instant,
    last_count: usize,
}

impl ProgressSink<'_> {
    fn maybe_report(&mut self) {
        let seen = self.inner.len();
        if seen - self.last_count < PROGRESS_ENTRIES
            && self.last_report.elapsed() < PROGRESS_INTERVAL
        {
            return;
        }
        self.last_report = Instant::now();
        self.last_count = seen;
        self.slot.set_activity(Activity::Scanning { seen });
        let _ = self.events.try_send(AppEvent::Index(IndexMsg::Status {
            id: self.slot.id(),
            status: self.slot.status(),
        }));
    }
}

impl super::enumerate::EntrySink for ProgressSink<'_> {
    fn push_wide(&mut self, name: &[u16], meta: super::enumerate::EntryMeta) -> bool {
        let ok = self.inner.push_wide(name);
        let _ = meta;
        self.maybe_report();
        ok
    }

    fn push_str(&mut self, name: &str, meta: super::enumerate::EntryMeta) -> bool {
        let ok = self.inner.push_str(name);
        let _ = meta;
        self.maybe_report();
        ok
    }

    fn accepted(&self) -> usize {
        self.inner.len()
    }
}

/// Runs one full scan for `--doctor`, without an actor or publishing.
pub fn scan_once(
    source: &dyn DirSource,
    dir: &Path,
    opts: &ListOpts,
) -> Result<(Snapshot, super::enumerate::ListStats), EnumError> {
    let prefix = dir.to_string_lossy().into_owned();
    let mut builder = SnapshotBuilder::new(&prefix);
    let stats = source.list(dir, &mut builder, opts, &CancelToken::never())?;
    let stamp = source.probe_stamp(dir).ok();
    Ok((builder.finish(SystemTime::now(), 0, stamp), stats))
}

/// Walks the tree and publishes it, segment by segment as it goes.
///
/// Always returns `Ok(None)`. A directory's timestamp moves only when its own
/// children change, so a root stamp cannot detect a file added deeper down;
/// recording one would let a probe falsely confirm the index as current. No
/// stamp settles the scheduler into `Blind`, where the rescan floor is the
/// guarantee.
fn walk_and_publish(
    ctx: &IndexContext,
    events: &Events,
    forced: bool,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<Option<DirStamp>, EnumError> {
    let root = indexed_dir(ctx).to_path_buf();
    let sink = WalkSink::new(&root.to_string_lossy(), ctx.slot(), events.clone());

    ctx.slot().set_activity(Activity::Walking {
        dirs: 0,
        queued: 0,
        files: 0,
    });
    publish_status(ctx, events);

    let opts = walk::WalkOpts {
        // Read at scan time because the attribute is not stored, so a change
        // takes effect on the next re-read (see `config::hidden`).
        hide_system: ctx.settings.hidden.hides_system(),
        ..walk::WalkOpts::default()
    };
    let report = walk::walk_tree(ctx.source.as_ref(), &root, &opts, &sink, cancel);
    ctx.slot().set_activity(Activity::Idle);

    if report.cancelled {
        *detail = "walk cancelled".into();
        return Err(EnumError::Cancelled);
    }
    if let Some(err) = report.aborted {
        // The share stopped answering. Mid-walk publishes stay: a partial tree
        // with an accurate label beats an empty screen.
        *detail = format!("walk aborted: {err}");
        if forced {
            let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
                id: ctx.mapping,
                entries: 0,
                elapsed: report.elapsed,
                error: Some(err),
            }));
        }
        return Err(err);
    }

    let coverage = Arc::new(TreeCoverage {
        dirs: report.dirs_visited,
        files: report.files,
        holes: report.errors.holes(),
        vanished: report.errors.vanished,
        examples: report
            .errors
            .recorded
            .iter()
            .map(|(rel, _)| {
                if rel.is_empty() {
                    "<root>".into()
                } else {
                    rel.clone()
                }
            })
            .collect(),
        skipped_junctions: report.skipped_reparse,
        elapsed: report.elapsed,
    });

    let index = Arc::new(sink.finish(report.complete()));
    let entries = index.len();
    *detail = format!(
        "{} files in {} folders, {}",
        entries,
        report.dirs_visited,
        crate::util::humanize::elapsed(report.elapsed)
    );
    ctx.slot()
        .publish_tree(index, Origin::Network, Some(coverage));
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));

    if forced {
        let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
            id: ctx.mapping,
            entries,
            elapsed: report.elapsed,
            error: None,
        }));
    }
    // Persisted only at the end, never from mid-walk publishes: each save
    // rewrites the whole (possibly hundreds of MB) file.
    persist_current(ctx, events);

    // No stamp; see this function's doc comment.
    Ok(None)
}

/// Publishes a tree index as the walk seals each segment.
///
/// Wraps [`tree::SegmentSink`] with publication into the store and
/// rate-limited progress for the UI.
struct WalkSink<'a> {
    inner: tree::SegmentSink,
    /// The slot itself rather than a store and id, so progress cannot be
    /// published into a different mapping.
    slot: &'a MappingSlot,
    events: Events,
    gate: ProgressGate,
}

impl<'a> WalkSink<'a> {
    fn new(root: &str, slot: &'a MappingSlot, events: Events) -> Self {
        Self {
            inner: tree::SegmentSink::new(root),
            slot,
            events,
            gate: ProgressGate::new(),
        }
    }

    fn finish(&self, complete: bool) -> tree::TreeIndex {
        self.inner
            .index()
            .with_metadata(SystemTime::now(), 0)
            .with_complete(complete)
    }
}

impl walk::TreeSink for WalkSink<'_> {
    fn push_dir(&self, rel: &str, files: &[String]) -> bool {
        self.inner.push_dir(rel, files)
    }

    fn progress(&self, dirs_done: usize, queued: usize, files: usize) {
        if !self.gate.claim(Instant::now(), files) {
            return;
        }
        // Publish as the walk goes, so the share is searchable early.
        // Rebuilding the index from sealed segments copies only `Arc`s.
        self.slot.publish_tree(
            Arc::new(self.inner.index()),
            Origin::Network,
            // No coverage verdict until the walk finishes.
            None,
        );
        self.slot.set_activity(Activity::Walking {
            dirs: dirs_done,
            queued,
            files,
        });
        let _ = self.events.try_send(AppEvent::Index(IndexMsg::Status {
            id: self.slot.id(),
            status: self.slot.status(),
        }));
    }
}

/// A lock-free rate limit for progress from many walk workers at once.
///
/// `ProgressSink` uses plain fields because one thread calls it. A walk calls
/// `progress` once per directory from every worker, so the limit must hold
/// without a lock.
struct ProgressGate {
    started: Instant,
    last_report_nanos: AtomicU64,
    last_count: AtomicU64,
}

impl ProgressGate {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_report_nanos: AtomicU64::new(0),
            last_count: AtomicU64::new(0),
        }
    }

    /// Returns true for exactly one caller per interval.
    fn claim(&self, now: Instant, count: usize) -> bool {
        let t = now.saturating_duration_since(self.started).as_nanos() as u64;
        let previous = self.last_report_nanos.load(Ordering::Relaxed);
        let by_time = t.saturating_sub(previous) >= PROGRESS_INTERVAL.as_nanos() as u64;
        let by_count = (count as u64).saturating_sub(self.last_count.load(Ordering::Relaxed))
            >= PROGRESS_ENTRIES as u64;
        if !by_time && !by_count {
            return false;
        }
        // Compare-exchange rather than `fetch_max`: with `fetch_max`, two
        // workers arriving together would both believe they won.
        if self
            .last_report_nanos
            .compare_exchange(previous, t, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        self.last_count.store(count as u64, Ordering::Relaxed);
        true
    }
}
/// Re-reads the dirty subtrees and patches them into the published index.
///
/// A handful of round trips for the changed folders, instead of a full walk.
fn apply_changes(
    ctx: &IndexContext,
    events: &Events,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<(), EnumError> {
    let (Some(queue), Some(index)) = (ctx.watch.as_ref(), ctx.slot().as_tree()) else {
        return Ok(());
    };
    // Taken, not peeked: the batch is consumed even if it cannot be applied,
    // so an unresponsive share is not retried every few seconds. The floor
    // bounds the cost of a lost notification.
    let Some(batch) = queue.take() else {
        return Ok(());
    };
    if batch.dirs.is_empty() {
        return Ok(());
    }

    let root = indexed_dir(ctx).to_path_buf();
    ctx.slot().set_activity(Activity::Walking {
        dirs: 0,
        queued: batch.dirs.len(),
        files: 0,
    });
    publish_status(ctx, events);

    let sink = tree::SegmentSink::new(&root.to_string_lossy());
    let opts = walk::WalkOpts {
        // Read at scan time because the attribute is not stored, so a change
        // takes effect on the next re-read (see `config::hidden`).
        hide_system: ctx.settings.hidden.hides_system(),
        ..walk::WalkOpts::default()
    };
    let report = walk::walk_subtrees(
        ctx.source.as_ref(),
        &root,
        &batch.dirs,
        &opts,
        &sink,
        cancel,
    );
    ctx.slot().set_activity(Activity::Idle);

    if report.cancelled {
        return Err(EnumError::Cancelled);
    }
    if let Some(err) = report.aborted {
        *detail = format!("update aborted: {err}");
        return Err(err);
    }

    let replaced = covered(&batch.dirs, &report);
    if replaced.is_empty() {
        // Every dirty folder failed to read. Replacing them would delete
        // folders that still exist and turn a transient error into missing
        // files.
        *detail = format!("{} folders unreadable, left alone", batch.dirs.len());
        return Ok(());
    }

    let next = index.with_subtrees_replaced(&replaced, &sink.index());
    *detail = format!(
        "{} folders refreshed, {} files",
        report.dirs_visited,
        next.len()
    );
    // `coverage` stays `None`: a patch says nothing about whole-share
    // coverage and must not clear the walk's verdict.
    ctx.slot()
        .publish_tree(Arc::new(next), Origin::Network, None);
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    Ok(())
}

/// Returns the seeds the walk covered, which may therefore be replaced.
///
/// An unreadable folder is left as it was: a stale entry is better than a
/// dropped subtree. A folder that has gone is replaced by nothing, since
/// that is a deletion; `WalkErrors` separates the two.
fn covered(seeds: &[String], report: &walk::WalkReport) -> Vec<String> {
    let holes = report.errors.holes() as usize;
    if holes == 0 {
        return seeds.to_vec();
    }
    let recorded: Vec<&str> = report
        .errors
        .recorded
        .iter()
        .filter(|(_, err)| walk::is_hole(*err))
        .map(|(rel, _)| rel.as_str())
        .collect();
    // More holes than recorded examples, so the affected seeds cannot be
    // identified. Replace none of them.
    if recorded.len() < holes {
        return Vec::new();
    }
    seeds
        .iter()
        .filter(|seed| !recorded.iter().any(|rel| walk::is_within(rel, seed)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FLAT_FIXTURE_PATH;
    use crate::index::DirStamp;
    use crate::index::fake_source::{Call, FakeDirSource};
    use crate::index::schedule::{ScanReason, StampHealth};
    use crossbeam_channel::bounded;

    /// The fast cadence puts the floor at 400 ms, so timer-driven paths can be
    /// observed in tests.
    const FLOOR: Duration = Duration::from_millis(400);
    const SEED: u64 = 0x5EED_5EED_5EED_5EED;

    fn ctx(src: FakeDirSource, store: Arc<IndexStore>) -> IndexContext {
        let settings = Settings {
            persist: false,
            ..Settings::fixture()
        };
        IndexContext::new(settings, MappingId(0), store, Arc::new(src))
    }

    /// As `ctx`, but with the compressed schedule and a fixed jitter seed.
    fn fast_ctx(src: FakeDirSource, store: Arc<IndexStore>) -> IndexContext {
        ctx(src, store)
            .with_cadence(Cadence::fast())
            .with_seed(SEED)
    }

    /// Drains the event channel in the background.
    ///
    /// `SnapshotChanged` is sent blocking, so without a reader the actor would
    /// stall once 256 events accumulated.
    fn draining_events() -> Events {
        let (tx, rx) = bounded(256);
        let tx = Events::headless(tx);
        std::thread::spawn(move || while rx.recv().is_ok() {});
        tx
    }

    fn wait_for(store: &IndexStore, pred: impl Fn(&IndexStore) -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred(store) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Waits for a line to reach the decision log, which the actor thread
    /// appends asynchronously.
    fn wait_for_log(path: &std::path::Path, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if std::fs::read_to_string(path).is_ok_and(|t| t.contains(needle)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn the_actor_publishes_an_initial_snapshot() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf", "b.pdf"]);
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        assert_eq!(store.first_flat_slot().as_flat().unwrap().len(), 2);
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn the_session_is_warmed_before_anything_needs_it() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        assert!(
            src.calls().iter().any(|c| matches!(c, Call::Prewarm(_))),
            "first contact with a mapped drive is slow; warm it early"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// An unchanged stamp avoids a full rescan.
    #[test]
    fn an_unchanged_stamp_avoids_re_enumeration() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        let scans_after_first = src.list_count(FLAT_FIXTURE_PATH);

        // Several explicit non-forced refreshes with an unchanged stamp.
        for _ in 0..3 {
            actor.refresh(false);
            std::thread::sleep(Duration::from_millis(40));
        }

        assert_eq!(
            src.list_count(FLAT_FIXTURE_PATH),
            scans_after_first,
            "an unchanged directory must not be re-enumerated"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_changed_stamp_triggers_a_rescan() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(fast_ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some_and(|f| f.len() == 1),
            Duration::from_secs(3)
        ));

        src.add_file(FLAT_FIXTURE_PATH, "b.pdf");
        src.set_stamp(Some(DirStamp::new(2, 2)));
        actor.refresh(false);

        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().as_flat().is_some_and(|f| f.len() == 2),
                Duration::from_secs(3)
            ),
            "a moved timestamp should produce a fresh listing"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_forced_refresh_bypasses_the_stamp_check() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        let before = src.list_count(FLAT_FIXTURE_PATH);

        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |_| src.list_count(FLAT_FIXTURE_PATH) > before,
                Duration::from_secs(3)
            ),
            "F5 must re-enumerate regardless of the stamp"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// An error must never discard the data already held.
    #[test]
    fn a_failing_refresh_keeps_serving_the_previous_snapshot() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf", "b.pdf"]);
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));

        src.set_error(Some(EnumError::Transient(53)));
        actor.refresh(true);
        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().status().health.is_unreachable(),
            Duration::from_secs(3)
        ));

        assert_eq!(
            store.first_flat_slot().as_flat().unwrap().len(),
            2,
            "the listing must survive the failure"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_refresh_report_reaches_the_ui() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::fixture());
        let (tx, rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();
        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));

        actor.refresh(true);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_report = false;
        while Instant::now() < deadline {
            if let Ok(AppEvent::Index(IndexMsg::RefreshReport { .. })) =
                rx.recv_timeout(Duration::from_millis(50))
            {
                saw_report = true;
                break;
            }
        }
        assert!(saw_report, "an explicit refresh should say what happened");
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn shutdown_is_prompt() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();
        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));

        let started = Instant::now();
        assert!(actor.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// Shutdown reaches a scan that is already running, via the stop flag.
    #[test]
    fn shutdown_reaches_a_scan_that_is_already_running() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        // Make the listing block, not the probe: `probe_stamp` takes no
        // cancellation token, so hanging it would test nothing here.
        src.set_stamp_error(Some(EnumError::Unsupported(50)));
        src.set_hang(true);
        let store = Arc::new(IndexStore::fixture());
        let (tx, _rx) = bounded(256);
        let tx = Events::headless(tx);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();

        // Wait until the scan is under way, or this proves only that an idle
        // actor exits.
        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().status().activity.is_busy(),
                Duration::from_secs(2)
            ),
            "the scan should be in flight"
        );

        let started = Instant::now();
        assert!(
            actor.shutdown(Duration::from_millis(500)),
            "shutdown timed out against a running scan"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    // --- reload storm, end to end -------------------------------------------

    /// A share that enumerates but will not answer the freshness probe is
    /// rescanned on the floor, not on every probe interval.
    #[test]
    fn a_share_that_will_not_report_a_stamp_is_rescanned_on_the_floor_not_the_probe_interval() {
        let src = FakeDirSource::new().with_synthetic(FLAT_FIXTURE_PATH, 50);
        src.set_stamp_supported(false);
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));

        // Three floors of wall clock; on the probe cadence that would be
        // around sixty enumerations.
        std::thread::sleep(FLOOR * 3);
        let scans = src.list_count(FLAT_FIXTURE_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert!(
            scans <= 5,
            "expected about one enumeration per floor, got {scans}"
        );
        assert!(
            store.first_flat_slot().status().stamp_health.is_blind(),
            "and the UI must be able to say why"
        );
    }

    #[test]
    fn a_transient_stamp_failure_never_triggers_an_enumeration() {
        let src = FakeDirSource::new().with_synthetic(FLAT_FIXTURE_PATH, 50);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        let after_first = src.list_count(FLAT_FIXTURE_PATH);

        src.set_stamp_error(Some(EnumError::Transient(53)));
        // Well inside one floor, so nothing else may cause a scan.
        std::thread::sleep(FLOOR / 2);
        let scans = src.list_count(FLAT_FIXTURE_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert_eq!(
            scans, after_first,
            "a blip in the cheap probe must not cost a full enumeration"
        );
    }

    /// A failing first scan is retried no faster than the minimum spacing.
    #[test]
    fn a_failed_first_scan_is_retried_no_faster_than_the_minimum_spacing() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_error(Some(EnumError::Transient(53)));
        let store = Arc::new(IndexStore::fixture());
        let cadence = Cadence::fast();
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        let window = Duration::from_millis(300);
        std::thread::sleep(window);
        let attempts = src.list_count(FLAT_FIXTURE_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert!(attempts >= 2, "it must keep trying, got {attempts}");
        let ceiling = (window.as_micros() / cadence.min_scan_spacing.as_micros()) as usize + 2;
        assert!(
            attempts <= ceiling,
            "{attempts} enumerations in {window:?} exceeds the {:?} spacing floor",
            cadence.min_scan_spacing
        );
    }

    #[test]
    fn an_unchanged_stamp_is_never_re_enumerated_however_long_the_app_runs() {
        let src = FakeDirSource::new().with_synthetic(FLAT_FIXTURE_PATH, 50);
        src.set_stamp(Some(DirStamp::new(7, 7)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        let after_first = src.list_count(FLAT_FIXTURE_PATH);

        // Most of a floor: many probe intervals, no enumeration.
        std::thread::sleep(Duration::from_millis(300));
        let scans = src.list_count(FLAT_FIXTURE_PATH);
        let probes = src.stamp_count(FLAT_FIXTURE_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert_eq!(scans, after_first, "nothing changed, so nothing to rebuild");
        assert!(
            probes > scans * 2,
            "the cheap probe should be doing the work: {probes} probes, {scans} scans"
        );
    }

    #[test]
    fn sixteen_f5_presses_produce_one_enumeration() {
        let src = FakeDirSource::new().with_synthetic(FLAT_FIXTURE_PATH, 50);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        // Settle, so the initial scan is not still in flight.
        std::thread::sleep(Duration::from_millis(50));
        let before = src.list_count(FLAT_FIXTURE_PATH);

        for _ in 0..16 {
            actor.refresh(true);
        }
        std::thread::sleep(Duration::from_millis(100));
        let after = src.list_count(FLAT_FIXTURE_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert!(after > before, "F5 must do something");
        assert!(
            after - before <= 2,
            "a held-down F5 queued {} enumerations",
            after - before
        );
    }

    /// A half-open session can report a directory as empty, so one empty
    /// answer must not replace a populated index.
    #[test]
    fn an_empty_listing_does_not_immediately_replace_a_populated_index() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some_and(|f| f.len() == 2),
            Duration::from_secs(3)
        ));

        src.set_dir(FLAT_FIXTURE_PATH, &[]);
        actor.refresh(true);
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(
            store.first_flat_slot().as_flat().unwrap().len(),
            2,
            "the first empty answer must be treated as suspect"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_genuinely_emptied_directory_is_accepted_on_the_second_answer() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some_and(|f| f.len() == 2),
            Duration::from_secs(3)
        ));

        src.set_dir(FLAT_FIXTURE_PATH, &[]);
        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().as_flat().is_some_and(|f| f.is_empty()),
                Duration::from_secs(3)
            ),
            "a directory that is consistently empty really is empty"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn the_scan_reason_reaches_the_ui() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().status().last_scan_reason == Some(ScanReason::FirstRun),
            Duration::from_secs(3)
        ));

        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().status().last_scan_reason == Some(ScanReason::Forced),
                Duration::from_secs(3)
            ),
            "the user pressing F5 must be attributable afterwards"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn the_decision_log_records_why_each_scan_happened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.log");
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::fixture());
        let context =
            fast_ctx(src.clone(), Arc::clone(&store)).with_log(Arc::new(IndexLog::to_path(&path)));
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        // Polled rather than slept on, so the test is not timing-sensitive
        // under load.
        assert!(wait_for_log(
            &path,
            "reason=first-run",
            Duration::from_secs(3)
        ));
        actor.refresh(true);
        assert!(wait_for_log(&path, "reason=f5", Duration::from_secs(3)));
        // The confirm-fresh line comes from the probe after the forced scan,
        // so wait for it.
        assert!(wait_for_log(
            &path,
            "step=confirm-fresh",
            Duration::from_secs(3)
        ));
        actor.shutdown(Duration::from_millis(500));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("reason=first-run"), "{text}");
        assert!(text.contains("reason=f5"), "{text}");
        assert!(
            text.contains("step=confirm-fresh"),
            "the quiet path must be visible too: {text}"
        );
    }

    // --- warm start --------------------------------------------------------

    /// The rescan floor is measured from the cache's capture time, so a warm
    /// start with a matching stamp does not enumerate.
    #[test]
    fn a_warm_start_with_a_matching_stamp_serves_the_cache_without_enumerating() {
        let cache = tempfile::tempdir().unwrap();
        let stamp = DirStamp::new(11, 11);
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(stamp));

        // First run: populate the cache.
        {
            let store = Arc::new(IndexStore::fixture());
            let settings = Settings {
                persist: true,
                cache_dir: Some(cache.path().to_path_buf()),
                ..Settings::fixture()
            };
            let context = IndexContext::new(
                settings,
                MappingId(0),
                Arc::clone(&store),
                Arc::new(src.clone()),
            )
            .with_cadence(Cadence::fast())
            .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.first_flat_slot().status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // Second run: the stamp still matches, so no enumeration is needed.
        src.clear_calls();
        let store = Arc::new(IndexStore::fixture());
        let settings = Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Settings::fixture()
        };
        let context = IndexContext::new(
            settings,
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_cadence(Cadence::fast())
        .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.first_flat_slot().as_flat().is_some(),
            Duration::from_secs(3)
        ));
        std::thread::sleep(Duration::from_millis(100));
        let scans = src.list_count(FLAT_FIXTURE_PATH);
        let origin = store.first_flat_slot().status().origin;
        actor.shutdown(Duration::from_millis(500));

        assert_eq!(
            scans, 0,
            "a provably-current cache must not be re-enumerated"
        );
        assert_eq!(origin, Some(Origin::DiskCache));
    }

    #[test]
    fn a_warm_start_whose_directory_moved_re_enumerates() {
        let cache = tempfile::tempdir().unwrap();
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(11, 11)));

        let settings = || Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Settings::fixture()
        };

        {
            let store = Arc::new(IndexStore::fixture());
            let context = IndexContext::new(
                settings(),
                MappingId(0),
                Arc::clone(&store),
                Arc::new(src.clone()),
            )
            .with_cadence(Cadence::fast())
            .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.first_flat_slot().status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // Something was added while the app was closed.
        src.add_file(FLAT_FIXTURE_PATH, "b.pdf");
        src.bump_stamp();
        src.clear_calls();

        let store = Arc::new(IndexStore::fixture());
        let context = IndexContext::new(
            settings(),
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_volume_serial(1)
        .with_cadence(Cadence::fast())
        .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().as_flat().is_some_and(|f| f.len() == 2),
                Duration::from_secs(3)
            ),
            "a change made while the app was closed must still be picked up"
        );
        assert_eq!(
            store.first_flat_slot().status().last_scan_reason,
            Some(ScanReason::StampMoved)
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// A source without stamp support is reported as degraded.
    #[test]
    fn a_source_with_no_stamp_support_is_marked_degraded() {
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp_supported(false);
        let store = Arc::new(IndexStore::fixture());
        let mut actor = spawn(fast_ctx(src, Arc::clone(&store)), draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| matches!(
                    s.first_flat_slot().status().health,
                    crate::index::Health::Degraded {
                        reason: DegradeReason::StampUnreliable,
                        ..
                    }
                ),
                Duration::from_secs(3)
            ),
            "the user must be told change detection is unavailable"
        );
        assert_eq!(
            store.first_flat_slot().status().stamp_health,
            StampHealth::Blind { failures: 1 }
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_rejected_cache_is_reported_rather_than_silently_ignored() {
        let cache = tempfile::tempdir().unwrap();
        let src = FakeDirSource::new().with_dir(FLAT_FIXTURE_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let settings = Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Settings::fixture()
        };

        // Write a cache under one volume identity...
        {
            let store = Arc::new(IndexStore::fixture());
            let context = IndexContext::new(
                settings.clone(),
                MappingId(0),
                Arc::clone(&store),
                Arc::new(src.clone()),
            )
            .with_volume_serial(0xAAAA)
            .with_cadence(Cadence::fast())
            .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.first_flat_slot().status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // ...then read it back as a different one.
        let store = Arc::new(IndexStore::fixture());
        let context = IndexContext::new(
            settings,
            MappingId(0),
            Arc::clone(&store),
            Arc::new(src.clone()),
        )
        .with_volume_serial(0xBBBB)
        .with_cadence(Cadence::fast())
        .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| s.first_flat_slot().status().cache_rejected.is_some(),
                Duration::from_secs(3)
            ),
            "a rejected cache must not look like a first run"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn scan_once_returns_a_snapshot_and_its_stats() {
        let src = FakeDirSource::new().with_synthetic(FLAT_FIXTURE_PATH, 500);
        let (snapshot, stats) =
            scan_once(&src, Path::new(FLAT_FIXTURE_PATH), &ListOpts::default()).unwrap();
        assert_eq!(snapshot.len(), 500);
        assert_eq!(stats.entries, 500);
    }

    #[test]
    fn queued_refreshes_are_coalesced_into_one_wake() {
        let (tx, rx) = bounded(16);
        tx.send(IndexCmd::Refresh { force: false }).unwrap();
        tx.send(IndexCmd::Refresh { force: true }).unwrap();
        tx.send(IndexCmd::Refresh { force: false }).unwrap();

        assert_eq!(
            next_wake(&rx, Instant::now() + Duration::from_millis(50)),
            Wake::Refresh { force: true },
            "one forced request in the queue must force the batch"
        );
        assert!(rx.is_empty(), "the whole queue should have been drained");
    }

    #[test]
    fn a_shutdown_anywhere_in_the_queue_wins() {
        let (tx, rx) = bounded(16);
        tx.send(IndexCmd::Refresh { force: true }).unwrap();
        tx.send(IndexCmd::Shutdown).unwrap();
        assert_eq!(
            next_wake(&rx, Instant::now() + Duration::from_millis(50)),
            Wake::Shutdown
        );
    }

    #[test]
    fn an_expired_deadline_reports_a_timer_wake() {
        let (_tx, rx) = bounded::<IndexCmd>(16);
        assert_eq!(next_wake(&rx, Instant::now()), Wake::Timer);
    }
}
