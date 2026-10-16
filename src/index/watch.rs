//! Platform-independent change-notification policy.
//!
//! Over SMB, `ReadDirectoryChangesW` with `bWatchSubtree` becomes one
//! `SMB2 CHANGE_NOTIFY` for the whole share, which scales where one watch per
//! directory would not. The decision logic lives here behind the
//! [`ChangeWatcher`] trait and a scripted fake, so only the OS call in
//! `win_watch` is untested.
//!
//! # Failure modes
//!
//! * **Overflow.** The remote buffer is limited to 64 KB, so a bulk change
//!   overruns it. The API completes with zero bytes, the lost events cannot
//!   be recovered, and only a full re-walk restores correctness.
//! * **Unavailable.** The call fails or the watch dies. The rescan floor is
//!   then the only freshness guarantee, and the status line must say so.
//! * **Silent.** The server accepts the request but never fires. This is why
//!   the periodic full re-walk is mandatory and nothing here may extend it.
//!
//! # Debounce from the first event
//!
//! A continuous stream of events (an upload, an application saving many
//! files) would postpone a trailing debounce indefinitely. Debouncing from the
//! first event bounds the delay to one interval.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::config::{WATCH_DEBOUNCE, WATCH_DIRTY_CAP};
use crate::util::cancel::CancelToken;

/// What a watcher reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// These directories, relative to the watched root, may have changed.
    ///
    /// Notifications name files, and a rename reports two. Re-reading an
    /// unchanged directory costs about three round trips, so false positives
    /// are acceptable.
    Changed(Vec<String>),
    /// Events were lost and cannot be recovered.
    Overflow,
    /// The watch could not be established, or has died.
    Unavailable(String),
}

/// A source of [`WatchEvent`]s.
///
/// `Sync` because stopping a blocked `ReadDirectoryChangesW` requires calling
/// `CancelIoEx` from another thread through a shared reference.
pub trait ChangeWatcher: Send + Sync {
    /// Blocks until the next event arrives, the watcher stops, or `cancel`
    /// fires.
    ///
    /// `None` means the watcher is finished; the pump exits.
    fn next_event(&self, cancel: &CancelToken) -> Option<WatchEvent>;

    /// Unblocks a call to [`Self::next_event`] from another thread.
    ///
    /// Needed in addition to the cancel token because a thread blocked in a
    /// syscall cannot poll the token.
    fn stop(&self);
}

/// A batch of changes for the index actor to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchBatch {
    /// Directories to re-read, sorted and deduplicated. Empty when `full` is
    /// set.
    pub dirs: Vec<String>,
    /// The dirty set overflowed or exceeded the cap, so only a full re-walk
    /// restores correctness.
    pub full: bool,
}

/// The pending change set, shared between the watcher thread and the actor.
///
/// A set rather than a channel of events: if the actor misses a wake-up or is
/// busy in a walk, nothing is lost, because the next read sees every change.
#[derive(Debug)]
pub struct WatchQueue {
    inner: Mutex<Pending>,
    debounce: Duration,
    /// Dirty-directory count above which a full re-walk is cheaper than
    /// re-reading them individually.
    cap: usize,
}

#[derive(Debug, Default)]
struct Pending {
    dirs: BTreeSet<String>,
    full: bool,
    /// When the oldest pending event arrived. The debounce runs from here.
    since: Option<Instant>,
    unavailable: Option<String>,
}

impl Default for WatchQueue {
    fn default() -> Self {
        Self::new(WATCH_DEBOUNCE, WATCH_DIRTY_CAP)
    }
}

impl WatchQueue {
    pub fn new(debounce: Duration, cap: usize) -> Self {
        Self {
            inner: Mutex::new(Pending::default()),
            debounce,
            cap,
        }
    }

    /// Folds an event into the pending set.
    ///
    /// Returns whether the actor needs waking. Only the first event of a burst
    /// does; the rest are picked up when the scheduled debounce expires.
    pub fn record(&self, event: WatchEvent, now: Instant) -> bool {
        let mut p = self.inner.lock();
        match event {
            WatchEvent::Changed(dirs) => {
                // A pending full re-walk supersedes individual directories.
                if !p.full {
                    p.dirs.extend(dirs);
                    if p.dirs.len() > self.cap {
                        p.dirs.clear();
                        p.full = true;
                    }
                }
            }
            WatchEvent::Overflow => {
                p.dirs.clear();
                p.full = true;
            }
            WatchEvent::Unavailable(why) => {
                // Not a reason to re-walk: if the watch never worked, the
                // preceding walk already covered everything; if it died, the
                // floor catches up. Forcing a walk would double the startup
                // cost on shares without change notification.
                let first = p.unavailable.is_none();
                p.unavailable = Some(why);
                return first;
            }
        }
        let first = p.since.is_none();
        p.since.get_or_insert(now);
        first
    }

    /// Returns when the pending set becomes actionable, if anything is pending.
    pub fn due_at(&self) -> Option<Instant> {
        let p = self.inner.lock();
        p.since.map(|since| since + self.debounce)
    }

    /// Takes the pending set if its debounce has elapsed.
    pub fn take_due(&self, now: Instant) -> Option<WatchBatch> {
        let mut p = self.inner.lock();
        if p.since.is_none_or(|since| now < since + self.debounce) {
            return None;
        }
        Some(Self::drain(&mut p))
    }

    /// Takes the pending set regardless of its debounce.
    ///
    /// Used when a scan is about to run anyway (F5 or the floor), so the
    /// pending changes do not trigger a redundant second pass.
    pub fn take(&self) -> Option<WatchBatch> {
        let mut p = self.inner.lock();
        p.since?;
        Some(Self::drain(&mut p))
    }

    fn drain(p: &mut Pending) -> WatchBatch {
        p.since = None;
        WatchBatch {
            dirs: std::mem::take(&mut p.dirs).into_iter().collect(),
            full: std::mem::take(&mut p.full),
        }
    }

    /// Returns whether a batch is actionable now, and whether it needs a full
    /// re-walk.
    ///
    /// Does not consume the set: the scheduler may defer the update to its
    /// spacing floor, and a consumed set would then be lost.
    pub fn due(&self, now: Instant) -> Option<bool> {
        let p = self.inner.lock();
        let since = p.since?;
        (now >= since + self.debounce).then_some(p.full)
    }

    /// Returns why live updates are not running, if they are not.
    pub fn unavailable(&self) -> Option<String> {
        self.inner.lock().unavailable.clone()
    }

    /// Marks the watch as live again, clearing any previous failure.
    pub fn mark_available(&self) {
        self.inner.lock().unavailable = None;
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().since.is_none()
    }
}

/// Drains a watcher into a queue until it stops.
///
/// Separate from the thread that runs it so tests can drive it with a scripted
/// fake.
pub fn pump(
    watcher: &dyn ChangeWatcher,
    queue: &WatchQueue,
    cancel: &CancelToken,
    mut notify: impl FnMut(),
) {
    while !cancel.is_cancelled() {
        let Some(event) = watcher.next_event(cancel) else {
            return;
        };
        if queue.record(event, Instant::now()) {
            notify();
        }
    }
}

/// A watcher driven by a script rather than the operating system.
///
/// Public, like [`crate::index::fake_source`], so integration tests outside
/// this crate's unit tests share one fake.
pub mod fake {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crossbeam_channel::{Receiver, Sender, unbounded};

    use super::{ChangeWatcher, WatchEvent};
    use crate::util::cancel::CancelToken;

    /// Replays events pushed into it, then blocks until stopped.
    #[derive(Debug, Clone)]
    pub struct ScriptedWatcher {
        tx: Sender<WatchEvent>,
        rx: Receiver<WatchEvent>,
        stopped: Arc<AtomicBool>,
    }

    impl Default for ScriptedWatcher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ScriptedWatcher {
        pub fn new() -> Self {
            let (tx, rx) = unbounded();
            Self {
                tx,
                rx,
                stopped: Arc::new(AtomicBool::new(false)),
            }
        }

        /// Queues an event for the next [`ChangeWatcher::next_event`].
        pub fn push(&self, event: WatchEvent) -> &Self {
            let _ = self.tx.send(event);
            self
        }

        pub fn changed(&self, dirs: &[&str]) -> &Self {
            self.push(WatchEvent::Changed(
                dirs.iter().map(|d| (*d).to_string()).collect(),
            ))
        }
    }

    impl ChangeWatcher for ScriptedWatcher {
        fn next_event(&self, cancel: &CancelToken) -> Option<WatchEvent> {
            loop {
                // Return queued events before honouring stop, so a test can
                // queue a burst and stop without racing. `CancelIoEx` likewise
                // does not discard a completion that has already landed.
                if let Ok(event) = self.rx.try_recv() {
                    return Some(event);
                }
                if self.stopped.load(Ordering::Relaxed) || cancel.is_cancelled() {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        fn stop(&self) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::ScriptedWatcher;
    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(100);

    fn queue() -> WatchQueue {
        WatchQueue::new(DEBOUNCE, 8)
    }

    fn changed(dirs: &[&str]) -> WatchEvent {
        WatchEvent::Changed(dirs.iter().map(|d| (*d).to_string()).collect())
    }

    #[test]
    fn nothing_is_due_until_the_debounce_elapses() {
        let q = queue();
        let t0 = Instant::now();
        assert!(q.record(changed(&["11d"]), t0));

        assert_eq!(q.take_due(t0 + DEBOUNCE / 2), None);
        assert_eq!(
            q.take_due(t0 + DEBOUNCE),
            Some(WatchBatch {
                dirs: vec!["11d".into()],
                full: false
            })
        );
    }

    /// A burst lasting ten debounce intervals is applied one interval after
    /// it started.
    #[test]
    fn a_continuous_burst_is_applied_one_debounce_after_its_first_event() {
        let q = WatchQueue::new(DEBOUNCE, 100);
        let t0 = Instant::now();
        q.record(changed(&["a"]), t0);
        for i in 1..10 {
            q.record(changed(&[&format!("d{i}")]), t0 + DEBOUNCE * i);
        }
        let batch = q
            .take_due(t0 + DEBOUNCE)
            .expect("due one debounce after the first event, not the last");
        assert!(batch.dirs.contains(&"a".to_string()));
    }

    #[test]
    fn duplicate_directories_are_coalesced() {
        let q = queue();
        let t0 = Instant::now();
        q.record(changed(&["11d", "11d", "ab12"]), t0);
        q.record(changed(&["11d"]), t0);
        let batch = q.take_due(t0 + DEBOUNCE).unwrap();
        assert_eq!(batch.dirs, vec!["11d".to_string(), "ab12".to_string()]);
    }

    #[test]
    fn only_the_first_event_of_a_burst_wakes_the_actor() {
        let q = queue();
        let t0 = Instant::now();
        assert!(q.record(changed(&["a"]), t0));
        assert!(!q.record(changed(&["b"]), t0));
        assert!(!q.record(WatchEvent::Overflow, t0));
    }

    #[test]
    fn overflow_asks_for_a_full_rewalk_and_discards_the_set() {
        let q = queue();
        let t0 = Instant::now();
        q.record(changed(&["11d"]), t0);
        q.record(WatchEvent::Overflow, t0);

        let batch = q.take_due(t0 + DEBOUNCE).unwrap();
        assert!(batch.full);
        assert!(
            batch.dirs.is_empty(),
            "a partial set alongside a full re-walk invites acting on both"
        );
    }

    /// After an overflow, individual directories are no longer accumulated.
    #[test]
    fn changes_after_an_overflow_do_not_reopen_the_set() {
        let q = queue();
        let t0 = Instant::now();
        q.record(WatchEvent::Overflow, t0);
        q.record(changed(&["11d"]), t0);

        let batch = q.take_due(t0 + DEBOUNCE).unwrap();
        assert!(batch.full);
        assert!(batch.dirs.is_empty());
    }

    #[test]
    fn too_many_dirty_directories_becomes_a_full_rewalk() {
        let q = WatchQueue::new(DEBOUNCE, 3);
        let t0 = Instant::now();
        q.record(changed(&["a", "b", "c", "d"]), t0);

        let batch = q.take_due(t0 + DEBOUNCE).unwrap();
        assert!(batch.full);
        assert!(batch.dirs.is_empty());
    }

    #[test]
    fn the_cap_itself_is_still_an_incremental_update() {
        let q = WatchQueue::new(DEBOUNCE, 3);
        let t0 = Instant::now();
        q.record(changed(&["a", "b", "c"]), t0);
        assert!(!q.take_due(t0 + DEBOUNCE).unwrap().full);
    }

    #[test]
    fn taking_the_set_resets_it() {
        let q = queue();
        let t0 = Instant::now();
        q.record(changed(&["a"]), t0);
        assert!(q.take_due(t0 + DEBOUNCE).is_some());
        assert!(q.is_empty());
        assert_eq!(q.take_due(t0 + DEBOUNCE * 2), None);
    }

    #[test]
    fn a_forced_take_ignores_the_debounce() {
        let q = queue();
        let t0 = Instant::now();
        q.record(changed(&["a"]), t0);
        assert!(q.take().is_some());
        assert!(q.is_empty());
    }

    #[test]
    fn a_forced_take_of_an_empty_queue_is_none() {
        assert_eq!(queue().take(), None);
    }

    /// Unavailability is recorded and wakes the actor once.
    #[test]
    fn unavailability_is_recorded_and_reported_once() {
        let q = queue();
        let t0 = Instant::now();
        assert!(q.record(WatchEvent::Unavailable("no CHANGE_NOTIFY".into()), t0));
        assert!(!q.record(WatchEvent::Unavailable("still none".into()), t0));
        assert_eq!(q.unavailable().as_deref(), Some("still none"));
    }

    /// Unavailability alone schedules no work.
    #[test]
    fn unavailability_alone_does_not_schedule_any_work() {
        let q = queue();
        let t0 = Instant::now();
        q.record(WatchEvent::Unavailable("nope".into()), t0);
        assert!(q.is_empty());
        assert_eq!(q.due_at(), None);
        assert_eq!(q.take_due(t0 + DEBOUNCE * 10), None);
    }

    #[test]
    fn due_at_is_one_debounce_after_the_first_event() {
        let q = queue();
        let t0 = Instant::now();
        q.record(changed(&["a"]), t0);
        q.record(changed(&["b"]), t0 + DEBOUNCE * 5);
        assert_eq!(q.due_at(), Some(t0 + DEBOUNCE));
    }

    #[test]
    fn the_pump_folds_every_event_into_the_queue() {
        let watcher = ScriptedWatcher::new();
        watcher.changed(&["a"]).changed(&["b"]);
        watcher.push(WatchEvent::Unavailable("died".into()));
        watcher.stop();

        let q = queue();
        let mut wakes = 0;
        pump(&watcher, &q, &CancelToken::never(), || wakes += 1);

        assert_eq!(
            q.take().unwrap().dirs,
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(q.unavailable().as_deref(), Some("died"));
        assert_eq!(wakes, 2, "one for the first change, one for the failure");
    }

    #[test]
    fn the_pump_returns_when_the_watcher_stops() {
        let watcher = ScriptedWatcher::new();
        watcher.stop();
        pump(&watcher, &queue(), &CancelToken::never(), || {});
    }
}
