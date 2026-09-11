//! A one-item, replace-on-write mailbox: "latest request wins".
//!
//! Used to feed the search, verify and prefetch workers. Chosen over an
//! unbounded channel plus drain-to-latest for one specific reason: storing the
//! request and bumping the epoch happen under the *same* mutex acquisition, so
//! there is no window in which a worker can observe a request whose epoch is
//! already stale. "At most one request pending" becomes a type invariant rather
//! than a loop every consumer has to remember to write.
//!
//! # No request is ever lost
//!
//! Workers run `while let Some(req) = slot.take_blocking() { run(req) }`. A
//! [`LatestSlot::put`] that lands *during* `run` is picked up by the very next
//! `take_blocking`, which returns immediately. The user's final keystroke is
//! therefore always serviced.

use parking_lot::{Condvar, Mutex};

/// A mailbox holding at most one pending value.
pub struct LatestSlot<T> {
    inner: Mutex<Inner<T>>,
    ready: Condvar,
}

struct Inner<T> {
    pending: Option<T>,
    closed: bool,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LatestSlot<T> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                pending: None,
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Stores `value`, discarding any previously pending value un-run.
    ///
    /// Returns the displaced value, if there was one, so callers can account
    /// for coalesced work in diagnostics.
    pub fn put(&self, value: T) -> Option<T> {
        let mut guard = self.inner.lock();
        if guard.closed {
            return Some(value);
        }
        let displaced = guard.pending.replace(value);
        drop(guard);
        self.ready.notify_one();
        displaced
    }

    /// Blocks until a value is available or the slot is closed.
    ///
    /// Returns `None` only when closed and drained.
    pub fn take_blocking(&self) -> Option<T> {
        let mut guard = self.inner.lock();
        loop {
            if let Some(value) = guard.pending.take() {
                return Some(value);
            }
            if guard.closed {
                return None;
            }
            // Predicate loop, not a bare wait: guards against spurious wakeups
            // and against a notify that races with a take.
            self.ready.wait(&mut guard);
        }
    }

    /// Takes any pending value without blocking.
    pub fn try_take(&self) -> Option<T> {
        self.inner.lock().pending.take()
    }

    /// Wakes every waiter and makes all future `take_blocking` calls return
    /// `None` once drained.
    pub fn close(&self) {
        self.inner.lock().closed = true;
        self.ready.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn take_returns_the_value_that_was_put() {
        let slot = LatestSlot::new();
        assert!(slot.put(7).is_none());
        assert_eq!(slot.take_blocking(), Some(7));
    }

    #[test]
    fn put_replaces_and_reports_the_displaced_value() {
        let slot = LatestSlot::new();
        slot.put(1);
        assert_eq!(slot.put(2), Some(1));
        assert_eq!(slot.put(3), Some(2));
        assert_eq!(slot.take_blocking(), Some(3));
        assert_eq!(slot.try_take(), None);
    }

    #[test]
    fn close_unblocks_a_waiting_taker() {
        let slot = Arc::new(LatestSlot::<u32>::new());
        let s = Arc::clone(&slot);
        let h = std::thread::spawn(move || s.take_blocking());
        std::thread::sleep(Duration::from_millis(50));
        slot.close();
        assert_eq!(h.join().unwrap(), None);
    }

    #[test]
    fn close_still_drains_a_pending_value_first() {
        let slot = LatestSlot::new();
        slot.put(9);
        slot.close();
        assert_eq!(slot.take_blocking(), Some(9));
        assert_eq!(slot.take_blocking(), None);
    }

    #[test]
    fn put_after_close_is_rejected_and_returned() {
        let slot = LatestSlot::new();
        slot.close();
        assert_eq!(slot.put(4), Some(4));
    }

    /// The property the whole design rests on: a value stored while the worker
    /// is busy is not lost, it is serviced on the next iteration.
    #[test]
    fn a_put_during_a_run_is_serviced_next() {
        let slot = Arc::new(LatestSlot::<u32>::new());
        let seen = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(AtomicUsize::new(0));

        let worker = {
            let slot = Arc::clone(&slot);
            let seen = Arc::clone(&seen);
            let last = Arc::clone(&last);
            std::thread::spawn(move || {
                while let Some(v) = slot.take_blocking() {
                    // Simulate work long enough for more puts to land.
                    std::thread::sleep(Duration::from_millis(20));
                    seen.fetch_add(1, Ordering::SeqCst);
                    last.store(v as usize, Ordering::SeqCst);
                }
            })
        };

        for v in 1..=10 {
            slot.put(v);
            std::thread::sleep(Duration::from_millis(5));
        }
        // Give the worker time to drain the final value.
        std::thread::sleep(Duration::from_millis(200));
        slot.close();
        worker.join().unwrap();

        assert_eq!(
            last.load(Ordering::SeqCst),
            10,
            "final value must be serviced"
        );
        assert!(
            seen.load(Ordering::SeqCst) < 10,
            "intermediate values should coalesce"
        );
    }
}
