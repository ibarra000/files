//! Live change notification, minus the operating system.
//!
//! Over SMB, `ReadDirectoryChangesW` with `bWatchSubtree` becomes a single
//! `SMB2 CHANGE_NOTIFY` covering a whole share - the only mechanism that
//! scales to three hundred thousand directories, since one watch per
//! directory would be three hundred thousand outstanding requests.
//!
//! None of that can be exercised on the machine this is written on, so the
//! split follows [`crate::index::win_enum`]'s precedent: everything that
//! decides *what to do* lives here, behind a [`ChangeWatcher`] trait and a
//! scripted fake, and only the call itself goes untested.
//!
//! # The three failure modes, and why each is designed for
//!
//! * **Overflow.** The remote buffer cannot exceed 64 KB, so a bulk change -
//!   a job folder being copied in - overruns it. The API signals that by
//!   completing with *zero bytes* rather than by failing, and nothing about
//!   what was lost can be recovered. Only a full re-walk restores the truth.
//! * **Unavailable.** The call fails outright, or the watch dies. The rescan
//!   floor becomes the whole freshness guarantee, and the status line has to
//!   say so, because believing you are live when you are not is worse than
//!   knowing you are not.
//! * **Silent.** The server accepts the request and then never fires. There is
//!   no symptom at all. This is why the periodic full re-walk is not optional
//!   and why nothing here is allowed to extend the floor.
//!
//! # Debounce from the first event, not the last
//!
//! A folder being written to continuously - an upload in progress, a CAD
//! package saving thirty files - produces a steady stream of events. Debouncing
//! from the *last* one postpones the update for as long as the stream lasts,
//! which is precisely the period during which someone is most likely to be
//! looking for the file. Debouncing from the first bounds the delay at exactly
//! one debounce interval no matter how long the burst runs.

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
    /// "May", not "did": the notification names a *file*, and a rename inside
    /// one folder reports two. Re-reading a directory that turns out to be
    /// unchanged costs three round trips and is not worth avoiding.
    Changed(Vec<String>),
    /// Events were lost and cannot be recovered.
    Overflow,
    /// The watch could not be established, or has died.
    Unavailable(String),
}

/// A source of [`WatchEvent`]s.
///
/// `Sync` as well as `Send` because the watcher is shared with the thread that
/// cancels it: stopping a blocked `ReadDirectoryChangesW` means calling
/// `CancelIoEx` from somewhere else, which cannot be done through a `&mut`.
pub trait ChangeWatcher: Send + Sync {
    /// Blocks until the next event arrives, the watcher stops, or `cancel`
    /// fires.
    ///
    /// `None` means this watcher is finished and will produce nothing further;
    /// the pump exits rather than spinning on it.
    fn next_event(&self, cancel: &CancelToken) -> Option<WatchEvent>;

    /// Unblocks a call to [`Self::next_event`] from another thread.
    ///
    /// Separate from the cancel token because the token can only be *polled*,
    /// and a thread parked in a blocking syscall never gets to poll anything.
    fn stop(&self);
}

/// What the index actor should act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchBatch {
    /// Directories to re-read, sorted and deduplicated. Empty when `full` is
    /// set.
    pub dirs: Vec<String>,
    /// The dirty set is not trustworthy - it overflowed, or grew past the
    /// point where re-reading it one directory at a time would cost more than
    /// walking the share. Only a full re-walk restores the truth.
    pub full: bool,
}

/// The pending change set, shared between the watcher thread and the actor.
///
/// A set rather than a channel of events, and that is the whole point: if the
/// actor misses a wake-up, or is busy for a minute inside a walk, nothing is
/// lost. The set is already populated and the next look at it sees everything.
/// A channel would make a dropped message a permanently invisible file.
#[derive(Debug)]
pub struct WatchQueue {
    inner: Mutex<Pending>,
    debounce: Duration,
    /// Dirty directories past which a full re-walk is cheaper than re-reading
    /// them individually.
    cap: usize,
}

#[derive(Debug, Default)]
struct Pending {
    dirs: BTreeSet<String>,
    full: bool,
    /// When the oldest still-pending event arrived. The debounce runs from
    /// here; see the module docs for why not from the newest.
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
    /// Returns whether the actor needs waking. Only the *first* event of a
    /// burst does: the rest land in a set the actor will read anyway when the
    /// debounce it already scheduled expires, so waking it again would just
    /// pay for a round trip through the channel to learn nothing new.
    pub fn record(&self, event: WatchEvent, now: Instant) -> bool {
        let mut p = self.inner.lock();
        match event {
            WatchEvent::Changed(dirs) => {
                // A pending full re-walk already supersedes any individual
                // directory, so accumulating them would only grow a set that
                // is about to be discarded.
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
                // Deliberately *not* a reason to re-walk. Either the watch
                // never worked, in which case the walk that just ran already
                // covered everything, or it died, in which case the floor
                // catches what was missed within half an hour. Forcing a walk
                // here would mean every launch against a share that does not
                // support change notification paid for a second full walk it
                // was never going to learn anything from.
                let first = p.unavailable.is_none();
                p.unavailable = Some(why);
                return first;
            }
        }
        let first = p.since.is_none();
        p.since.get_or_insert(now);
        first
    }

    /// When the pending set becomes actionable, if anything is pending.
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
    /// Used when a scan is about to run anyway - an F5, or the floor coming
    /// due. Leaving the set in place would mean the change that arrived a
    /// moment ago triggered a second pass over a share the first pass had
    /// already covered.
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

    /// Whether a batch is actionable now, and whether it needs a full re-walk.
    ///
    /// A peek, not a take. The scheduler may decide the update has to wait for
    /// its spacing floor, and a set consumed before that decision would be a
    /// set of changes nobody ever applies.
    pub fn due(&self, now: Instant) -> Option<bool> {
        let p = self.inner.lock();
        let since = p.since?;
        (now >= since + self.debounce).then_some(p.full)
    }

    /// Why live updates are not running here, if they are not.
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
/// Split out from the thread that runs it so the policy above can be driven by
/// a scripted fake in a test without a thread being involved at all.
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

/// A watcher driven by a script rather than by an operating system.
///
/// Public, not test-gated, for the same reason [`crate::index::fake_source`]
/// is: the integration tests that matter here live outside this crate's unit
/// tests, and the alternative is a second copy of the fake that drifts.
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
                // Scripted events first, so a test can queue a burst and then
                // stop the watcher in one breath without racing itself. A real
                // watcher behaves the same way: `CancelIoEx` does not discard a
                // completion that has already landed.
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

    /// The property the module exists to get right. A burst that runs for ten
    /// debounce intervals must still be applied one interval after it started.
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

    /// After an overflow the individual directories mean nothing, so
    /// accumulating them would only grow a set about to be thrown away.
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

    /// Past the cap, re-reading one directory at a time costs more than
    /// walking the share - so it stops pretending otherwise.
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

    /// A scan that is about to run anyway covers whatever is pending, so
    /// leaving it queued would buy a second pass over the same share.
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

    /// Believing you are live when you are not is worse than knowing you are
    /// not - so it is recorded, and it wakes the actor to say so.
    #[test]
    fn unavailability_is_recorded_and_reported_once() {
        let q = queue();
        let t0 = Instant::now();
        assert!(q.record(WatchEvent::Unavailable("no CHANGE_NOTIFY".into()), t0));
        assert!(!q.record(WatchEvent::Unavailable("still none".into()), t0));
        assert_eq!(q.unavailable().as_deref(), Some("still none"));
    }

    /// A share that cannot be watched must not pay for a second full walk it
    /// has nothing to learn from.
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
