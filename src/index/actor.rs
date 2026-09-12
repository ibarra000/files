//! The thread that owns the flat-root index.
//!
//! Single writer by construction: every mutation of the shared index happens
//! here, so there are no writer-writer races to reason about.
//!
//! # Freshness without rescanning
//!
//! The original implementation re-enumerated the whole share every five
//! minutes, unconditionally. On a million-entry drive that is seconds of
//! network traffic per cycle, whether or not anything changed, and it competes
//! with whatever the user is doing at the time.
//!
//! Instead the directory's timestamps are probed once a minute - about three
//! round trips, a couple of milliseconds - and a full enumeration only happens
//! when they have actually moved. That is roughly a 700x reduction in
//! background cost, and the probe doubles as proof to
//! [`crate::search::verify`] that the local index is still authoritative.
//!
//! # Why the decision lives elsewhere
//!
//! The *timing* used to be inline here, tangled up with the I/O, and it had
//! three separate ways to collapse the hourly floor into a per-minute full
//! enumeration - which is exactly what was reported from the field as "random
//! indexing reloads". It now lives in [`super::schedule`], which reads no
//! clock and does no I/O and can therefore be tested against a simulated day.
//!
//! This module is the driver: it waits, performs what it is told, and reports
//! the outcome back. Every branch below corresponds to one [`Step`].
//!
//! # Waiting
//!
//! The loop blocks on the command channel with a deadline; it never sleeps.
//! That is what lets `F5` interrupt a five-minute backoff instantly instead of
//! the user waiting it out.

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
use super::persist;
use super::schedule::{Cadence, Decision, Input, Scheduler, Step, is_probe_refusal};
use super::store::{Activity, DegradeReason, IndexStore, Origin, TreeCoverage};
use super::watch::{self, ChangeWatcher, WatchQueue};
use super::{DirStamp, Snapshot};
use super::{tree, walk};
use crate::app::event::{AppEvent, IndexMsg};
use crate::config::Settings;
use crate::paths::MappingKind;
use crate::util::cancel::CancelToken;

/// Progress is rate limited at the source, so the UI channel is never close
/// to full and the bounded-versus-unbounded question stays academic.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const PROGRESS_ENTRIES: usize = 50_000;

/// Consecutive empty enumerations required before a populated index is
/// replaced with an empty one.
///
/// `ERROR_FILE_NOT_FOUND` from a directory enumeration legitimately means
/// "empty", so an empty listing is not an error - but it is also what a
/// half-open SMB session can produce, and discarding a million entries on the
/// strength of one such answer is not recoverable by anything the user can do.
/// The second opinion costs one scan-backoff interval.
const EMPTY_CONFIRMATIONS: u32 = 2;

/// Upper bound on steps taken for one wake-up.
///
/// The sequence is `Woke -> [Probe -> Probed ->] FullScan -> Scanned -> Wait`,
/// so five is the longest legitimate chain. The bound exists so that a future
/// rule which forgot to terminate becomes a caught assertion rather than a
/// thread spinning against a file server.
const MAX_STEPS_PER_WAKE: usize = 6;

#[derive(Debug, Clone)]
pub enum IndexCmd {
    /// Re-examine the share. `force` skips the stamp check.
    Refresh {
        force: bool,
    },
    /// The watch queue has something in it. Carries no payload: the dirty set
    /// lives in the queue, so a message dropped on a full channel costs a
    /// wake-up rather than a file nobody can find again.
    Changed,
    Shutdown,
}

/// Handle to the running actor.
pub struct IndexActor {
    tx: Sender<IndexCmd>,
    /// Set from the caller's thread, checked from inside a running scan.
    ///
    /// The command channel cannot deliver a shutdown while a scan is in
    /// progress, because the actor is the channel's only reader and the actor
    /// is the thing scanning. That has never mattered: a flat enumeration is
    /// milliseconds. A recursive walk of a large share is minutes, and for all
    /// of them `shutdown` would time out and leak the thread. A flag is the
    /// only thing that crosses that gap.
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Kept only so shutdown can unblock it: a thread parked in
    /// `ReadDirectoryChangesW` never gets to poll a cancellation flag.
    watcher: Option<Arc<dyn ChangeWatcher>>,
    pump: Option<JoinHandle<()>>,
}

impl IndexActor {
    /// Asks for a refresh.
    ///
    /// Requests are coalesced by the actor rather than here - see
    /// [`next_wake`] - because dropping them on a full channel, which is all
    /// this could do, left sixteen queued refreshes to be executed one after
    /// another as sixteen separate full enumerations.
    pub fn refresh(&self, force: bool) {
        match self.tx.try_send(IndexCmd::Refresh { force }) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn shutdown(&mut self, budget: Duration) -> bool {
        // The flag first, so a scan that is between directories when the
        // message lands has already been told to stop. Sending first and
        // setting second would leave exactly the window this exists to close.
        self.stop.store(true, Ordering::Relaxed);
        // The watcher before the actor. Its thread is parked in a call the
        // flag cannot reach, so it has to be told out of band or shutdown
        // waits out the whole budget for a thread that was never going to
        // notice.
        if let Some(watcher) = &self.watcher {
            watcher.stop();
        }
        let _ = self.tx.send(IndexCmd::Shutdown);
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let deadline = Instant::now() + budget;
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
}

/// Everything the actor needs.
pub struct IndexContext {
    pub settings: Settings,
    /// Which index this actor owns.
    ///
    /// One actor per index, not one actor for both: a tree walk runs for
    /// minutes, and sharing a thread would mean the flat share's freshness
    /// waited behind it - or worse, that a five-minute backoff on an
    /// unreachable tree delayed the healthy one. Each still has exactly one
    /// writer, which is what the store's single-writer rule requires.
    pub kind: MappingKind,
    pub store: Arc<IndexStore>,
    pub source: Arc<dyn DirSource>,
    /// Volume serial resolved at startup, if it was available then.
    pub volume_serial: Option<u32>,
    pub cadence: Cadence,
    /// Fixed jitter seed. `None` seeds from entropy, which is what the
    /// application wants; a test passes `Some` and gets a fixed schedule.
    pub rng_seed: Option<u64>,
    pub log: Arc<IndexLog>,
    /// Live change notifications, when they are available here.
    ///
    /// The queue rather than the watcher: the actor never talks to the
    /// operating system about this, it reads a set somebody else fills in.
    pub watch: Option<Arc<WatchQueue>>,
    /// The watcher feeding [`Self::watch`], kept so shutdown can unblock it.
    pub watcher: Option<Arc<dyn ChangeWatcher>>,
}

impl IndexContext {
    pub fn new(
        settings: Settings,
        store: Arc<IndexStore>,
        source: Arc<dyn DirSource>,
        volume_serial: Option<u32>,
    ) -> Self {
        Self {
            settings,
            kind: MappingKind::Flat,
            store,
            source,
            volume_serial,
            cadence: Cadence::shipped().from_env(),
            rng_seed: None,
            log: Arc::new(IndexLog::disabled()),
            watch: None,
            watcher: None,
        }
    }

    /// Feeds this actor live change notifications.
    pub fn with_watch(mut self, watcher: Arc<dyn ChangeWatcher>, queue: Arc<WatchQueue>) -> Self {
        self.watcher = Some(watcher);
        self.watch = Some(queue);
        self
    }

    /// Makes this actor own the walked tree instead of the flat index.
    pub fn for_tree(mut self) -> Self {
        self.kind = MappingKind::Tree;
        self.cadence = Cadence::tree().from_env();
        self
    }

    // --- which index this actor is talking about ---------------------------
    //
    // Routed in one place rather than at each of the twenty call sites, for
    // the same reason `indexed_dir` exists: an actor that read the *flat*
    // index's state while owning the tree would be told it had no snapshot on
    // every wake, make every scan a `FirstRun`, and leave `min_scan_spacing`
    // as the only thing between the share and a continuous re-walk.

    fn is_tree(&self) -> bool {
        self.kind == MappingKind::Tree
    }

    /// Whether this actor's index holds anything yet.
    fn have_index(&self) -> bool {
        if self.is_tree() {
            self.store.tree().is_some()
        } else {
            self.store.flat().is_some()
        }
    }

    fn status(&self) -> Arc<super::store::IndexStatus> {
        if self.is_tree() {
            self.store.tree_status()
        } else {
            self.store.status()
        }
    }

    fn set_activity(&self, activity: Activity) {
        if self.is_tree() {
            self.store.set_tree_activity(activity);
        } else {
            self.store.set_activity(activity);
        }
    }

    fn note_scan_started(&self, reason: super::schedule::ScanReason) {
        if self.is_tree() {
            self.store.note_tree_scan_started(reason);
        } else {
            self.store.note_scan_started(reason);
        }
    }

    fn note_schedule(
        &self,
        counters: super::schedule::Counters,
        health: super::schedule::StampHealth,
    ) {
        if self.is_tree() {
            self.store.note_tree_schedule(counters, health);
        } else {
            self.store.note_schedule(counters, health);
        }
    }

    fn note_cache_rejected(&self, why: String) {
        if self.is_tree() {
            self.store.note_tree_cache_rejected(why);
        } else {
            self.store.note_cache_rejected(why);
        }
    }

    fn record_failure(&self, err: EnumError, attempt: u32, next_retry_at: Instant) {
        if self.is_tree() {
            self.store.record_tree_failure(err, attempt, next_retry_at);
        } else {
            self.store.record_failure(err, attempt, next_retry_at);
        }
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
pub fn spawn(ctx: IndexContext, events: Sender<AppEvent>) -> std::io::Result<IndexActor> {
    let (tx, rx) = crossbeam_channel::bounded(16);
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = CancelToken::from_flag(Arc::clone(&stop));
    let watcher = ctx.watcher.clone();
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
        tx,
        stop,
        handle: Some(handle),
        watcher,
        pump,
    })
}

/// Starts the thread that drains the watcher into the queue.
///
/// Its own thread because `next_event` blocks - over SMB it is an outstanding
/// `CHANGE_NOTIFY` that may not answer for hours - and the actor thread cannot
/// afford to be in it. All it ever does is fold events into a set and nudge
/// the actor, so a lost nudge costs a wake-up rather than a change.
fn spawn_pump(
    ctx: &IndexContext,
    tx: Sender<IndexCmd>,
    cancel: &CancelToken,
    events: &Sender<AppEvent>,
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
                    // Dropped on a full channel on purpose. The actor reads
                    // the queue, not the message, so the only thing lost is
                    // one early wake-up out of the sixteen already queued.
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

/// A failure the store has not been told about yet, because the retry time is
/// not known until the scheduler has decided.
#[derive(Debug, Clone, Copy)]
struct PendingFailure {
    err: EnumError,
    /// True when it was the cheap probe that failed rather than the scan.
    from_probe: bool,
}

fn run(ctx: IndexContext, rx: Receiver<IndexCmd>, events: &Sender<AppEvent>, cancel: &CancelToken) {
    let mut sched = Scheduler::new(ctx.cadence, ctx.rng_seed);
    let dir = ctx.settings.custpro_path.clone();
    let mut serial = ctx.volume_serial;
    let mut failures: u32 = 0;
    let mut empty_streak: u32 = 0;

    // Warm the SMB session before anything needs it. First contact with a
    // mapped drive can cost seconds of session setup and DFS resolution.
    ctx.source.prewarm(&dir);
    ctx.source.prewarm(&ctx.settings.base_path);

    // Serve the previous run's index immediately. This is what turns a cold
    // start from "unusable for seconds" into "usable now, refreshing".
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
        // The wait ends at whichever comes first: what the scheduler asked
        // for, or the moment a pending change batch becomes actionable. The
        // debounce is the queue's business, not the scheduler's, so it is
        // folded in here rather than taught to `schedule.rs`.
        let deadline = match ctx.watch.as_ref().and_then(|q| q.due_at()) {
            Some(due) => decision.next_action.min(due),
            None => decision.next_action,
        };
        let mut input = match next_wake(&rx, deadline) {
            Wake::Shutdown => return,
            Wake::Refresh { force } => Input::Woke { forced: force },
            Wake::Timer | Wake::Changed => match pending_changes(&ctx, Instant::now()) {
                Some(full) => Input::Changed { full },
                None => Input::Woke { forced: false },
            },
        };
        let forced = matches!(input, Input::Woke { forced: true });
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

            // Told to the store only now: the retry instant is part of the
            // decision, and the status line promises the user a countdown.
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
                    }
                    input = Input::Applied(outcome);
                    event = "patch";
                }
                Step::FullScan(reason) => {
                    // Whatever the watcher had pending is about to be covered
                    // by a pass over the whole share, so it is dropped rather
                    // than left to trigger a second one the moment this
                    // finishes.
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
                    if let Err(err) = outcome {
                        pending = Some(PendingFailure {
                            err,
                            from_probe: false,
                        });
                    } else {
                        failures = 0;
                    }
                    input = Input::Scanned(outcome);
                    event = "scan";
                }
                Step::ConfirmFresh => {
                    // The common case by far: nothing moved, so the listing is
                    // provably still correct and no enumeration is needed.
                    ctx.store
                        .confirm_fresh(sched.recorded_stamp(), SystemTime::now());
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

        // Re-asserted rather than set once. A walk that succeeds publishes a
        // fresh verdict on the tree's health, and a walk succeeding is not
        // evidence that a dead watch came back.
        if ctx
            .watch
            .as_ref()
            .is_some_and(|q| q.unavailable().is_some())
        {
            ctx.store.note_live_updates_unavailable();
        }

        // Belt and braces. A non-terminal decision carries `next_action ==
        // now`, so leaving one in place would spin this thread against the
        // file server - the single worst outcome available here.
        if !decision.step.is_terminal() {
            debug_assert!(false, "unterminated decision: {:?}", decision.step);
            decision.next_action = Instant::now() + ctx.cadence.probe_interval;
        }

        publish_status(&ctx, events);
    }
}

/// Whether the watch queue has an actionable batch, and whether it needs a
/// full re-walk.
///
/// A peek. The scheduler may still defer the update to its spacing floor, and
/// a set consumed before that decision would be a set of changes nobody ever
/// applies.
fn pending_changes(ctx: &IndexContext, now: Instant) -> Option<bool> {
    ctx.watch.as_ref()?.due(now)
}

/// Blocks until the deadline or a command, coalescing queued refreshes.
///
/// Holding F5 down used to enqueue up to sixteen `Refresh` messages, each of
/// which was drained and executed as its own full enumeration.
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
/// A probe the share *refused* is reported as degraded, not unreachable: the
/// drive is fine, it just will not answer the cheap freshness question.
/// Telling the user their drive is unreachable would be a lie they would act
/// on, and the only thing they could do about it is the one thing that cannot
/// help.
fn apply_failure(
    ctx: &IndexContext,
    failure: PendingFailure,
    failures: &mut u32,
    retry_at: Instant,
) {
    if failure.from_probe && is_probe_refusal(failure.err) {
        ctx.store.set_degraded(DegradeReason::StampUnreliable);
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

fn load_from_disk(ctx: &IndexContext, events: &Sender<AppEvent>) -> Option<DiskLoad> {
    if !ctx.settings.persist {
        return None;
    }
    let cache_dir = ctx.settings.cache_dir.clone()?;

    ctx.set_activity(Activity::LoadingDisk);
    publish_status(ctx, events);

    let dir = indexed_dir(ctx);
    let key = persist::MappingKey::of(dir);
    let expect = persist::Expect::new(dir, ctx.volume_serial);

    // A tree and a flat listing are different file layouts, and the loader
    // refuses to read one as the other - so a tree actor cannot decode the
    // flat share's cache and publish one directory as the whole share. The
    // cache key already differs, since it is derived from the directory; this
    // is the backstop for that, not a substitute.
    let result = if ctx.is_tree() {
        load_tree_from_disk(ctx, events, &cache_dir, key, expect)
    } else {
        load_flat_from_disk(ctx, events, &cache_dir, key, expect)
    };

    let loaded = match result {
        Ok(loaded) => Some(loaded),
        Err(err) => {
            // A missing cache is normal on first run. A *rejected* one is not,
            // and used to be indistinguishable from it - so a drive letter
            // pointing at a different volume looked exactly like a cold start,
            // every single launch, with nothing on screen to say why.
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
    events: &Sender<AppEvent>,
    cache_dir: &Path,
    key: persist::MappingKey,
    expect: persist::Expect<'_>,
) -> Result<DiskLoad, persist::LoadError> {
    let snapshot = persist::load(cache_dir, key, expect)?;
    let stamp = snapshot.stamp();
    let age = SystemTime::now()
        .duration_since(snapshot.captured_at())
        .ok();
    ctx.store
        .publish_flat(Arc::new(snapshot), Origin::DiskCache);
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    Ok(DiskLoad { stamp, age })
}

fn load_tree_from_disk(
    ctx: &IndexContext,
    events: &Sender<AppEvent>,
    cache_dir: &Path,
    key: persist::MappingKey,
    expect: persist::Expect<'_>,
) -> Result<DiskLoad, persist::LoadError> {
    let loaded = persist::load_tree(cache_dir, key, expect)?;
    let age = SystemTime::now()
        .duration_since(loaded.index.captured_at())
        .ok();
    // The coverage is published with the index, not withheld until the next
    // walk confirms it. A cache written from a walk that could not read part
    // of the share must come back still saying so; otherwise the one restart
    // between the walk and the re-walk is a window in which the status line
    // quietly claims complete coverage of a share it never had.
    ctx.store.publish_tree(
        Arc::new(loaded.index),
        Origin::DiskCache,
        Some(Arc::new(loaded.coverage)),
    );
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    // A tree records no directory stamp, by design: no single timestamp can
    // stand for a whole share. That is what settles the scheduler into
    // `Blind`, where the re-walk floor is the whole freshness guarantee.
    Ok(DiskLoad { stamp: None, age })
}

fn persist_current(ctx: &IndexContext, events: &Sender<AppEvent>) {
    if !ctx.settings.persist {
        return;
    }
    let Some(cache_dir) = ctx.settings.cache_dir.clone() else {
        return;
    };
    let key = persist::MappingKey::of(indexed_dir(ctx));

    ctx.set_activity(Activity::Persisting);
    publish_status(ctx, events);
    // Best effort throughout: failing to write the cache costs a slow cold
    // start next time and nothing else.
    if ctx.is_tree() {
        if let Some(index) = ctx.store.tree() {
            let coverage = ctx.store.tree_status().coverage.clone();
            let _ = persist::save_tree(&cache_dir, key, &index, coverage.as_deref());
        }
    } else if let Some(snapshot) = ctx.store.flat() {
        let _ = persist::save(&cache_dir, key, &snapshot);
    }
    ctx.set_activity(Activity::Idle);
    publish_status(ctx, events);
}

/// Enumerates, decides whether to trust the result, and publishes it.
///
/// Returns the stamp recorded alongside the listing, which is what the
/// scheduler needs to decide whether change detection is available at all.
fn scan_and_publish(
    ctx: &IndexContext,
    events: &Sender<AppEvent>,
    serial: &mut Option<u32>,
    empty_streak: &mut u32,
    forced: bool,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<Option<DirStamp>, EnumError> {
    if ctx.kind == MappingKind::Tree {
        return walk_and_publish(ctx, events, forced, detail, cancel);
    }
    let previous_entries = ctx.store.status().entries;
    let (snapshot, elapsed) = match full_scan(ctx, events, serial, cancel) {
        Ok(v) => v,
        Err(err) => {
            *detail = format!("scan failed: {err}");
            if forced {
                let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
                    entries: 0,
                    elapsed: Duration::ZERO,
                    error: Some(err),
                }));
            }
            // The previous snapshot is deliberately left in place: stale
            // results with an honest label beat an empty screen.
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
    ctx.store.publish_flat(Arc::new(snapshot), Origin::Network);
    if stamp.is_none() {
        // Set *after* the publish, which clears the health because a fresh
        // listing normally resolves whatever was wrong. Change detection is
        // unavailable here, so the scheduler drops to the rescan floor rather
        // than re-enumerating on the probe cadence - the whole of the bug this
        // module was rewritten for - and the user is told why.
        ctx.store.set_degraded(DegradeReason::StampUnreliable);
    }
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));

    if forced {
        let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
            entries,
            elapsed,
            error: None,
        }));
    }

    persist_current(ctx, events);
    Ok(stamp)
}

/// The directory this actor indexes.
///
/// One accessor rather than repeated field reads, so making the actor
/// per-mapping is a change in one place.
fn indexed_dir(ctx: &IndexContext) -> &Path {
    match ctx.kind {
        MappingKind::Tree => &ctx.settings.tree_path,
        _ => &ctx.settings.custpro_path,
    }
}

/// The volume serial for the persisted index's identity check.
///
/// Retried on the actor thread when startup could not resolve it: that attempt
/// races the SMB session warm-up, and a `None` there is written into the cache
/// as "unknown".
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
    events: &Sender<AppEvent>,
    serial: &mut Option<u32>,
    cancel: &CancelToken,
) -> Result<(Snapshot, Duration), EnumError> {
    let started = Instant::now();
    let dir = ctx.settings.custpro_path.clone();
    let prefix = dir.to_string_lossy().into_owned();

    // Probed *before* the listing, and that is the value recorded with it.
    //
    // Recording the stamp read afterwards would hide a change that landed
    // mid-scan: the listing would be missing the new entry while its stamp
    // said "current as of after the change", and the next probe would agree.
    // Taking it first is conservative in the only direction that is safe -
    // one extra rescan rather than a silently incomplete index.
    let stamp = ctx.source.probe_stamp(&dir).ok();

    // Size the arenas from what the previous run actually held, so from the
    // second run onward the buffers never grow.
    let (hint_entries, hint_avg) = ctx
        .store
        .flat()
        .map(|s| {
            let n = s.len().max(1);
            (s.len(), (s.lower().len() / n).saturating_sub(1))
        })
        .unwrap_or((0, 0));

    let mut builder = SnapshotBuilder::with_capacity(&prefix, hint_entries, hint_avg);
    let mut progress = ProgressSink {
        inner: &mut builder,
        events,
        store: &ctx.store,
        last_report: Instant::now(),
        last_count: 0,
    };

    ctx.set_activity(Activity::Scanning { seen: 0 });
    publish_status(ctx, events);

    let result = ctx
        .source
        .list(&dir, &mut progress, &ListOpts::default(), cancel);
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

fn publish_status(ctx: &IndexContext, events: &Sender<AppEvent>) {
    // Droppable by nature: the next status supersedes this one, so a full
    // channel is not worth blocking the scan for.
    let _ = events.try_send(AppEvent::Index(IndexMsg::Status(ctx.status())));
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
    events: &'a Sender<AppEvent>,
    store: &'a IndexStore,
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
        self.store.set_activity(Activity::Scanning { seen });
        let _ = self
            .events
            .try_send(AppEvent::Index(IndexMsg::Status(self.store.status())));
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

/// Convenience for `--doctor`: one full scan, no actor, no publishing.
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
/// Always reports `Ok(None)` - no stamp - and that is the honest answer, not a
/// convenience. A directory's timestamp moves when *its own* children change,
/// so the root of a three-hundred-thousand-directory tree says nothing about a
/// file added three levels down. If a walk recorded one, the scheduler would
/// go healthy, probe, see an unchanged root and report the index as *proven
/// current* while a thousand files had been added underneath - a worse version
/// of the bug this index exists to remove. Reporting no stamp settles the
/// scheduler into `Blind`, where the rescan floor is the whole guarantee,
/// which is exactly a tree's situation.
fn walk_and_publish(
    ctx: &IndexContext,
    events: &Sender<AppEvent>,
    forced: bool,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<Option<DirStamp>, EnumError> {
    let root = indexed_dir(ctx).to_path_buf();
    let sink = WalkSink::new(
        &root.to_string_lossy(),
        Arc::clone(&ctx.store),
        events.clone(),
    );

    ctx.store.set_tree_activity(Activity::Walking {
        dirs: 0,
        queued: 0,
        files: 0,
    });
    publish_status(ctx, events);

    let opts = walk::WalkOpts::default();
    let report = walk::walk_tree(ctx.source.as_ref(), &root, &opts, &sink, cancel);
    ctx.store.set_tree_activity(Activity::Idle);

    if report.cancelled {
        *detail = "walk cancelled".into();
        return Err(EnumError::Cancelled);
    }
    if let Some(err) = report.aborted {
        // The share stopped answering. Whatever was published mid-walk stays:
        // a partial tree with an honest label beats an empty screen.
        *detail = format!("walk aborted: {err}");
        if forced {
            let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
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
    ctx.store
        .publish_tree(index, Origin::Network, Some(coverage));
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));

    if forced {
        let _ = events.send(AppEvent::Index(IndexMsg::RefreshReport {
            entries,
            elapsed: report.elapsed,
            error: None,
        }));
    }
    // Persisted only here, at the end, never from the mid-walk publishes.
    // Writing a 300 MB file twenty-odd times during a walk would cost more
    // than the walk does, and every one of those files would be superseded
    // minutes later by the next.
    persist_current(ctx, events);

    // Deliberately no stamp; see this function's own doc comment.
    Ok(None)
}

/// Publishes a tree index as the walk seals each segment.
///
/// Wraps [`tree::SegmentSink`] with the two things the actor needs on top of
/// it: publication into the store, and progress the UI can render without
/// being flooded.
struct WalkSink {
    inner: tree::SegmentSink,
    store: Arc<IndexStore>,
    events: Sender<AppEvent>,
    gate: ProgressGate,
}

impl WalkSink {
    fn new(root: &str, store: Arc<IndexStore>, events: Sender<AppEvent>) -> Self {
        Self {
            inner: tree::SegmentSink::new(root),
            store,
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

impl walk::TreeSink for WalkSink {
    fn push_dir(&self, rel: &str, files: &[String]) -> bool {
        self.inner.push_dir(rel, files)
    }

    fn progress(&self, dirs_done: usize, queued: usize, files: usize) {
        if !self.gate.claim(Instant::now(), files) {
            return;
        }
        // Published as it goes, so the share is searchable about a second in
        // rather than after the whole walk. The index is rebuilt from the
        // sealed segments each time, which is a vector of refcounts rather
        // than a byte of the hundreds of megabytes behind them.
        self.store.publish_tree(
            Arc::new(self.inner.index()),
            Origin::Network,
            // No verdict yet: judging a half-finished walk would flag every
            // one of them as incomplete for the minutes it is running.
            None,
        );
        self.store.set_tree_activity(Activity::Walking {
            dirs: dirs_done,
            queued,
            files,
        });
        let _ = self
            .events
            .try_send(AppEvent::Index(IndexMsg::Status(self.store.tree_status())));
    }
}

/// The same rate limit `ProgressSink` applies, for a sink called from every
/// walk worker at once.
///
/// `ProgressSink` can keep an `Instant` and a count in plain fields because
/// the enumerator calls it from one thread. A walk calls `progress` once per
/// directory from eight, which over three hundred thousand directories is some
/// seventeen hundred calls a second into a 256-slot channel - so the limit has
/// to hold without taking a lock, or the limiter costs more than the sends it
/// suppresses.
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

    /// True for exactly one caller per interval.
    fn claim(&self, now: Instant, count: usize) -> bool {
        let t = now.saturating_duration_since(self.started).as_nanos() as u64;
        let previous = self.last_report_nanos.load(Ordering::Relaxed);
        let by_time = t.saturating_sub(previous) >= PROGRESS_INTERVAL.as_nanos() as u64;
        let by_count = (count as u64).saturating_sub(self.last_count.load(Ordering::Relaxed))
            >= PROGRESS_ENTRIES as u64;
        if !by_time && !by_count {
            return false;
        }
        // Compare-exchange rather than `fetch_max`: with `fetch_max` two
        // workers arriving in the same microsecond both move the value and
        // both believe they won, which turns a rate limit into a rate
        // multiplier under exactly the load it exists for.
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
/// Cheap where a walk is expensive: the batch names the folders that changed,
/// so this is a handful of round trips against the nine hundred thousand a
/// full pass costs. That difference is the entire reason live updates are
/// worth having - without it a change would either trigger a re-walk every few
/// minutes or wait for the floor.
fn apply_changes(
    ctx: &IndexContext,
    events: &Sender<AppEvent>,
    detail: &mut String,
    cancel: &CancelToken,
) -> Result<(), EnumError> {
    let (Some(queue), Some(index)) = (ctx.watch.as_ref(), ctx.store.tree()) else {
        return Ok(());
    };
    // Taken, not peeked. The batch is consumed whether or not it can be
    // applied: retaining it would have a share that refuses to answer retried
    // every few seconds for as long as it refuses, and the floor already
    // guarantees that a lost notification costs at most one floor.
    let Some(batch) = queue.take() else {
        return Ok(());
    };
    if batch.dirs.is_empty() {
        return Ok(());
    }

    let root = indexed_dir(ctx).to_path_buf();
    ctx.store.set_tree_activity(Activity::Walking {
        dirs: 0,
        queued: batch.dirs.len(),
        files: 0,
    });
    publish_status(ctx, events);

    let sink = tree::SegmentSink::new(&root.to_string_lossy());
    let opts = walk::WalkOpts::default();
    let report = walk::walk_subtrees(
        ctx.source.as_ref(),
        &root,
        &batch.dirs,
        &opts,
        &sink,
        cancel,
    );
    ctx.store.set_tree_activity(Activity::Idle);

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
        // folders that are still there and whose files are still findable -
        // turning a transient network error into exactly the silent
        // disappearance this index exists to prevent.
        *detail = format!("{} folders unreadable, left alone", batch.dirs.len());
        return Ok(());
    }

    let next = index.with_subtrees_replaced(&replaced, &sink.index());
    *detail = format!(
        "{} folders refreshed, {} files",
        report.dirs_visited,
        next.len()
    );
    // `coverage` stays `None`: a patch has nothing to say about whether the
    // *share* is fully covered, and overwriting the walk's verdict with the
    // opinion of a three-folder update would clear a warning it never checked.
    ctx.store
        .publish_tree(Arc::new(next), Origin::Network, None);
    let _ = events.send(AppEvent::Index(IndexMsg::SnapshotChanged));
    Ok(())
}

/// The seeds the walk actually covered, and which may therefore be replaced.
///
/// A folder that could not be read is left exactly as it was. The direction
/// matters and is the same one the whole rewrite turns on: a stale entry is a
/// result that might be wrong, while a dropped subtree is a file that cannot
/// be found at all.
///
/// A folder that has *gone* is different, and is replaced - by nothing. That
/// is a deletion, not a failure, and `WalkErrors` already separates the two.
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
    // More holes than examples kept, so which seeds they belong to cannot be
    // worked out. Replacing none of them is the only safe reading.
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
    use crate::config::CUSTPRO_PATH;
    use crate::index::DirStamp;
    use crate::index::fake_source::{Call, FakeDirSource};
    use crate::index::schedule::{ScanReason, StampHealth};
    use crossbeam_channel::bounded;

    /// The fast cadence puts an "hour" at 400ms, so the timer-driven paths
    /// this module exists to get right can actually be watched in a test.
    const FLOOR: Duration = Duration::from_millis(400);
    const SEED: u64 = 0x5EED_5EED_5EED_5EED;

    fn ctx(src: FakeDirSource, store: Arc<IndexStore>) -> IndexContext {
        let settings = Settings {
            persist: false,
            ..Default::default()
        };
        IndexContext::new(settings, store, Arc::new(src), Some(1))
    }

    /// As `ctx`, but with the compressed schedule and a fixed jitter seed.
    fn fast_ctx(src: FakeDirSource, store: Arc<IndexStore>) -> IndexContext {
        ctx(src, store)
            .with_cadence(Cadence::fast())
            .with_seed(SEED)
    }

    /// Drains the event channel in the background.
    ///
    /// `SnapshotChanged` is sent blocking, so a test that provokes many scans
    /// without a reader would wedge the actor once 256 events accumulated.
    fn draining_events() -> Sender<AppEvent> {
        let (tx, rx) = bounded(256);
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

    /// Waits for a line to reach the decision log.
    ///
    /// The log is appended from the actor thread, so "has it happened yet" is
    /// a question about another thread's progress, not about elapsed time.
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
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf", "b.pdf"]);
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        assert_eq!(store.flat().unwrap().len(), 2);
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn the_session_is_warmed_before_anything_needs_it() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        assert!(
            src.calls().iter().any(|c| matches!(c, Call::Prewarm(_))),
            "first contact with a mapped drive is slow; warm it early"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// The change that replaces a full rescan every five minutes.
    #[test]
    fn an_unchanged_stamp_avoids_re_enumeration() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        let scans_after_first = src.list_count(CUSTPRO_PATH);

        // Several explicit non-forced refreshes with an unchanged stamp.
        for _ in 0..3 {
            actor.refresh(false);
            std::thread::sleep(Duration::from_millis(40));
        }

        assert_eq!(
            src.list_count(CUSTPRO_PATH),
            scans_after_first,
            "an unchanged directory must not be re-enumerated"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_changed_stamp_triggers_a_rescan() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(fast_ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some_and(|f| f.len() == 1),
            Duration::from_secs(3)
        ));

        src.add_file(CUSTPRO_PATH, "b.pdf");
        src.set_stamp(Some(DirStamp::new(2, 2)));
        actor.refresh(false);

        assert!(
            wait_for(
                &store,
                |s| s.flat().is_some_and(|f| f.len() == 2),
                Duration::from_secs(3)
            ),
            "a moved timestamp should produce a fresh listing"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_forced_refresh_bypasses_the_stamp_check() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        let before = src.list_count(CUSTPRO_PATH);

        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |_| src.list_count(CUSTPRO_PATH) > before,
                Duration::from_secs(3)
            ),
            "F5 must re-enumerate regardless of the stamp"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// The behaviour the previous implementation got wrong: an error must
    /// never cost the user the data they already had.
    #[test]
    fn a_failing_refresh_keeps_serving_the_previous_snapshot() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf", "b.pdf"]);
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src.clone(), Arc::clone(&store)), tx).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));

        src.set_error(Some(EnumError::Transient(53)));
        actor.refresh(true);
        assert!(wait_for(
            &store,
            |s| s.status().health.is_unreachable(),
            Duration::from_secs(3)
        ));

        assert_eq!(
            store.flat().unwrap().len(),
            2,
            "the listing must survive the failure"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_refresh_report_reaches_the_ui() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::default());
        let (tx, rx) = bounded(256);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();
        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
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
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();
        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));

        let started = Instant::now();
        assert!(actor.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// Shutdown has to reach a scan that is already running.
    ///
    /// The command channel cannot: the actor is its only reader and the actor
    /// is the thing scanning. That never mattered while a scan was one
    /// millisecond-scale listing, but a recursive walk of a large share runs
    /// for minutes, and without the flag every exit during one would time out
    /// and leak the thread along with its outstanding queries.
    #[test]
    fn shutdown_reaches_a_scan_that_is_already_running() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        // The probe answers at once and the *listing* is what blocks, which is
        // where a walk spends its minutes. `probe_stamp` takes no cancellation
        // token at all, so hanging that instead would exercise a path nothing
        // can interrupt and prove nothing about this one.
        src.set_stamp_error(Some(EnumError::Unsupported(50)));
        src.set_hang(true);
        let store = Arc::new(IndexStore::default());
        let (tx, _rx) = bounded(256);
        let mut actor = spawn(ctx(src, Arc::clone(&store)), tx).unwrap();

        // Let the scan get properly under way, or this proves only that an
        // idle actor exits.
        assert!(
            wait_for(
                &store,
                |s| s.status().activity.is_busy(),
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

    // --- the reload storm, end to end --------------------------------------

    /// The reported bug. A share that enumerates fine but will not answer the
    /// freshness probe used to be re-enumerated on every probe interval,
    /// forever; it must instead fall back to the rescan floor.
    ///
    /// Reachable at all only because `FakeDirSource` can now fail the probe
    /// independently of the listing.
    #[test]
    fn a_share_that_will_not_report_a_stamp_is_rescanned_on_the_floor_not_the_probe_interval() {
        let src = FakeDirSource::new().with_synthetic(CUSTPRO_PATH, 50);
        src.set_stamp_supported(false);
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));

        // Three floors' worth of wall clock. On the probe cadence that would
        // be around sixty enumerations.
        std::thread::sleep(FLOOR * 3);
        let scans = src.list_count(CUSTPRO_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert!(
            scans <= 5,
            "expected about one enumeration per floor, got {scans}"
        );
        assert!(
            store.status().stamp_health.is_blind(),
            "and the UI must be able to say why"
        );
    }

    #[test]
    fn a_transient_stamp_failure_never_triggers_an_enumeration() {
        let src = FakeDirSource::new().with_synthetic(CUSTPRO_PATH, 50);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        let after_first = src.list_count(CUSTPRO_PATH);

        src.set_stamp_error(Some(EnumError::Transient(53)));
        // Well inside one floor, so nothing else may cause a scan.
        std::thread::sleep(FLOOR / 2);
        let scans = src.list_count(CUSTPRO_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert_eq!(
            scans, after_first,
            "a blip in the cheap probe must not cost a full enumeration"
        );
    }

    /// RC2: `last_full_scan` was only set on success, so a failing first scan
    /// left the floor permanently due and every full-jitter retry attempted
    /// another full enumeration - sometimes under a second apart.
    #[test]
    fn a_failed_first_scan_is_retried_no_faster_than_the_minimum_spacing() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_error(Some(EnumError::Transient(53)));
        let store = Arc::new(IndexStore::default());
        let cadence = Cadence::fast();
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        let window = Duration::from_millis(300);
        std::thread::sleep(window);
        let attempts = src.list_count(CUSTPRO_PATH);
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
        let src = FakeDirSource::new().with_synthetic(CUSTPRO_PATH, 50);
        src.set_stamp(Some(DirStamp::new(7, 7)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        let after_first = src.list_count(CUSTPRO_PATH);

        // Most of a floor: many probe intervals, no enumeration.
        std::thread::sleep(Duration::from_millis(300));
        let scans = src.list_count(CUSTPRO_PATH);
        let probes = src.stamp_count(CUSTPRO_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert_eq!(scans, after_first, "nothing changed, so nothing to rebuild");
        assert!(
            probes > scans * 2,
            "the cheap probe should be doing the work: {probes} probes, {scans} scans"
        );
    }

    #[test]
    fn sixteen_f5_presses_produce_one_enumeration() {
        let src = FakeDirSource::new().with_synthetic(CUSTPRO_PATH, 50);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        // Settle, so the initial scan is not still in flight.
        std::thread::sleep(Duration::from_millis(50));
        let before = src.list_count(CUSTPRO_PATH);

        for _ in 0..16 {
            actor.refresh(true);
        }
        std::thread::sleep(Duration::from_millis(100));
        let after = src.list_count(CUSTPRO_PATH);
        actor.shutdown(Duration::from_millis(500));

        assert!(after > before, "F5 must do something");
        assert!(
            after - before <= 2,
            "a held-down F5 queued {} enumerations",
            after - before
        );
    }

    /// A half-open session can report a directory as empty. Replacing a
    /// million entries with zero on one such answer is not something the user
    /// can recover from.
    #[test]
    fn an_empty_listing_does_not_immediately_replace_a_populated_index() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some_and(|f| f.len() == 2),
            Duration::from_secs(3)
        ));

        src.set_dir(CUSTPRO_PATH, &[]);
        actor.refresh(true);
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(
            store.flat().unwrap().len(),
            2,
            "the first empty answer must be treated as suspect"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_genuinely_emptied_directory_is_accepted_on_the_second_answer() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some_and(|f| f.len() == 2),
            Duration::from_secs(3)
        ));

        src.set_dir(CUSTPRO_PATH, &[]);
        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |s| s.flat().is_some_and(|f| f.is_empty()),
                Duration::from_secs(3)
            ),
            "a directory that is consistently empty really is empty"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn the_scan_reason_reaches_the_ui() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let mut actor =
            spawn(fast_ctx(src.clone(), Arc::clone(&store)), draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.status().last_scan_reason == Some(ScanReason::FirstRun),
            Duration::from_secs(3)
        ));

        actor.refresh(true);
        assert!(
            wait_for(
                &store,
                |s| s.status().last_scan_reason == Some(ScanReason::Forced),
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
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let store = Arc::new(IndexStore::default());
        let context =
            fast_ctx(src.clone(), Arc::clone(&store)).with_log(Arc::new(IndexLog::to_path(&path)));
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        // Polled rather than slept on. Fixed sleeps were long enough on an
        // idle machine and not on a loaded one, so this failed only when the
        // rest of the suite happened to be running beside it.
        assert!(wait_for_log(
            &path,
            "reason=first-run",
            Duration::from_secs(3)
        ));
        actor.refresh(true);
        assert!(wait_for_log(&path, "reason=f5", Duration::from_secs(3)));
        // The quiet path is the probe that follows the forced scan, so it is
        // waited for rather than assumed to have happened by now.
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

    /// The disk cache used to buy only the few seconds before the startup scan
    /// superseded it, because the rescan floor was measured from process start
    /// rather than from when the data was captured.
    #[test]
    fn a_warm_start_with_a_matching_stamp_serves_the_cache_without_enumerating() {
        let cache = tempfile::tempdir().unwrap();
        let stamp = DirStamp::new(11, 11);
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf", "b.pdf"]);
        src.set_stamp(Some(stamp));

        // First run: populate the cache.
        {
            let store = Arc::new(IndexStore::default());
            let settings = Settings {
                persist: true,
                cache_dir: Some(cache.path().to_path_buf()),
                ..Default::default()
            };
            let context =
                IndexContext::new(settings, Arc::clone(&store), Arc::new(src.clone()), Some(1))
                    .with_cadence(Cadence::fast())
                    .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // Second run: the stamp still matches, so no enumeration is needed.
        src.clear_calls();
        let store = Arc::new(IndexStore::default());
        let settings = Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Default::default()
        };
        let context =
            IndexContext::new(settings, Arc::clone(&store), Arc::new(src.clone()), Some(1))
                .with_cadence(Cadence::fast())
                .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(wait_for(
            &store,
            |s| s.flat().is_some(),
            Duration::from_secs(3)
        ));
        std::thread::sleep(Duration::from_millis(100));
        let scans = src.list_count(CUSTPRO_PATH);
        let origin = store.status().origin;
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
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(11, 11)));

        let settings = || Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Default::default()
        };

        {
            let store = Arc::new(IndexStore::default());
            let context = IndexContext::new(
                settings(),
                Arc::clone(&store),
                Arc::new(src.clone()),
                Some(1),
            )
            .with_cadence(Cadence::fast())
            .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // Something was added while the app was closed.
        src.add_file(CUSTPRO_PATH, "b.pdf");
        src.bump_stamp();
        src.clear_calls();

        let store = Arc::new(IndexStore::default());
        let context = IndexContext::new(
            settings(),
            Arc::clone(&store),
            Arc::new(src.clone()),
            Some(1),
        )
        .with_cadence(Cadence::fast())
        .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| s.flat().is_some_and(|f| f.len() == 2),
                Duration::from_secs(3)
            ),
            "a change made while the app was closed must still be picked up"
        );
        assert_eq!(
            store.status().last_scan_reason,
            Some(ScanReason::StampMoved)
        );
        actor.shutdown(Duration::from_millis(500));
    }

    /// Replaces a test that could not reach this path at all, and said so:
    /// it asserted against `IndexStore` directly because the fake source
    /// always answered the probe.
    #[test]
    fn a_source_with_no_stamp_support_is_marked_degraded() {
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp_supported(false);
        let store = Arc::new(IndexStore::default());
        let mut actor = spawn(fast_ctx(src, Arc::clone(&store)), draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| matches!(
                    s.status().health,
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
            store.status().stamp_health,
            StampHealth::Blind { failures: 1 }
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn a_rejected_cache_is_reported_rather_than_silently_ignored() {
        let cache = tempfile::tempdir().unwrap();
        let src = FakeDirSource::new().with_dir(CUSTPRO_PATH, &["a.pdf"]);
        src.set_stamp(Some(DirStamp::new(1, 1)));
        let settings = Settings {
            persist: true,
            cache_dir: Some(cache.path().to_path_buf()),
            ..Default::default()
        };

        // Write a cache under one volume identity...
        {
            let store = Arc::new(IndexStore::default());
            let context = IndexContext::new(
                settings.clone(),
                Arc::clone(&store),
                Arc::new(src.clone()),
                Some(0xAAAA),
            )
            .with_cadence(Cadence::fast())
            .with_seed(SEED);
            let mut actor = spawn(context, draining_events()).unwrap();
            assert!(wait_for(
                &store,
                |s| s.status().origin == Some(Origin::Network),
                Duration::from_secs(3)
            ));
            std::thread::sleep(Duration::from_millis(60));
            actor.shutdown(Duration::from_millis(500));
        }

        // ...then read it back as a different one.
        let store = Arc::new(IndexStore::default());
        let context = IndexContext::new(
            settings,
            Arc::clone(&store),
            Arc::new(src.clone()),
            Some(0xBBBB),
        )
        .with_cadence(Cadence::fast())
        .with_seed(SEED);
        let mut actor = spawn(context, draining_events()).unwrap();

        assert!(
            wait_for(
                &store,
                |s| s.status().cache_rejected.is_some(),
                Duration::from_secs(3)
            ),
            "a rejected cache must not look like a first run"
        );
        actor.shutdown(Duration::from_millis(500));
    }

    #[test]
    fn scan_once_returns_a_snapshot_and_its_stats() {
        let src = FakeDirSource::new().with_synthetic(CUSTPRO_PATH, 500);
        let (snapshot, stats) =
            scan_once(&src, Path::new(CUSTPRO_PATH), &ListOpts::default()).unwrap();
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
