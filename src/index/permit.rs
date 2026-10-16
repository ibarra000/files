//! A cap on how many shares are walked at once.
//!
//! Each walk already keeps [`crate::config::WALK_CONCURRENCY`] directory reads
//! in flight, so uncapped walks across many mappings would flood the file
//! server.
//!
//! A bounded channel pre-filled with `n` units is used instead of a `Condvar`
//! so the wait composes with `select!` over the actor's command receiver and
//! a queued walker still answers shutdown promptly.
//!
//! Exactly `n` units exist. [`WalkPermit`] returns its unit on `Drop` into a
//! channel that cannot be full, so the send never fails or blocks. `panic`
//! must stay at `unwind` (see `Cargo.toml`) so a panicking walk still runs
//! `Drop` before the actor's `catch_unwind`.

use crossbeam_channel::{Receiver, Sender, select};

use super::actor::IndexCmd;

/// The outcome of waiting for a permit.
pub enum Acquired<'a> {
    /// Held until the returned guard is dropped.
    Held(WalkPermit<'a>),
    /// The actor was told to stop while queueing. No permit was taken.
    Shutdown,
}

/// A shared cap on concurrent full passes.
pub struct WalkPermits {
    tx: Sender<()>,
    rx: Receiver<()>,
}

impl WalkPermits {
    /// Creates `n` permits, clamped to at least one.
    pub fn new(n: usize) -> Self {
        let n = n.max(1);
        let (tx, rx) = crossbeam_channel::bounded(n);
        for _ in 0..n {
            tx.send(()).expect("a fresh bounded channel has room");
        }
        Self { tx, rx }
    }

    /// Returns the number of permits not currently held.
    pub fn available(&self) -> usize {
        self.rx.len()
    }

    /// Takes a permit, or reports that the actor should stop.
    ///
    /// A `Refresh` received while queued is folded into `forced`, since the
    /// scan has not started. A `Changed` is ignored because the upcoming full
    /// pass covers it.
    pub fn acquire(&self, cmds: &Receiver<IndexCmd>, forced: &mut bool) -> Acquired<'_> {
        // Uncontended fast path; does not read the command channel.
        if self.rx.try_recv().is_ok() {
            return Acquired::Held(WalkPermit { tx: &self.tx });
        }
        loop {
            select! {
                recv(self.rx) -> got => {
                    return match got {
                        Ok(()) => Acquired::Held(WalkPermit { tx: &self.tx }),
                        // Every sender gone means the process is going down.
                        Err(_) => Acquired::Shutdown,
                    };
                }
                recv(cmds) -> cmd => match cmd {
                    Ok(IndexCmd::Refresh { force }) => *forced |= force,
                    Ok(IndexCmd::Changed) => {}
                    Ok(IndexCmd::Shutdown) | Err(_) => return Acquired::Shutdown,
                },
            }
        }
    }
}

/// Held for the duration of one full pass. Not `Clone`: one guard is one unit.
pub struct WalkPermit<'a> {
    tx: &'a Sender<()>,
}

impl Drop for WalkPermit<'_> {
    fn drop(&mut self) {
        // Cannot fail: the channel has capacity for every unit that exists.
        let _ = self.tx.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_cmds() -> (Sender<IndexCmd>, Receiver<IndexCmd>) {
        crossbeam_channel::unbounded()
    }

    #[test]
    fn a_fresh_set_hands_out_exactly_its_cap() {
        let p = WalkPermits::new(2);
        assert_eq!(p.available(), 2);
        let (_tx, rx) = idle_cmds();
        let mut forced = false;
        let _a = p.acquire(&rx, &mut forced);
        let _b = p.acquire(&rx, &mut forced);
        assert_eq!(p.available(), 0, "the cap is the whole point");
    }

    #[test]
    fn a_permit_comes_back_when_the_walk_ends() {
        let p = WalkPermits::new(1);
        let (_tx, rx) = idle_cmds();
        let mut forced = false;
        {
            let _held = p.acquire(&rx, &mut forced);
            assert_eq!(p.available(), 0);
        }
        assert_eq!(p.available(), 1, "or the cap leaks to zero and never walks");
    }

    #[test]
    fn a_zero_cap_still_walks_one_share_at_a_time() {
        assert_eq!(WalkPermits::new(0).available(), 1);
    }

    /// An actor queued behind a walk must still be able to shut down.
    #[test]
    fn shutdown_reaches_an_actor_that_is_queueing() {
        let p = WalkPermits::new(1);
        let (tx, rx) = idle_cmds();
        let mut forced = false;
        let _held = p.acquire(&rx, &mut forced);

        tx.send(IndexCmd::Shutdown).unwrap();
        assert!(matches!(p.acquire(&rx, &mut forced), Acquired::Shutdown));
        assert_eq!(p.available(), 0, "a refused acquire must not take a unit");
    }

    /// A forced refresh received while queued applies to the pending scan.
    ///
    /// The permit must be contended: with one free, `acquire` takes the fast
    /// path and the actor's wake loop handles the refresh instead.
    #[test]
    fn a_forced_refresh_while_queueing_is_not_lost() {
        let p = WalkPermits::new(1);
        let (tx, rx) = idle_cmds();
        let mut forced = false;
        let _held = p.acquire(&rx, &mut forced);
        assert_eq!(p.available(), 0, "the next acquire must actually queue");

        tx.send(IndexCmd::Refresh { force: true }).unwrap();
        // Ends the wait; without it the queueing acquire blocks forever.
        tx.send(IndexCmd::Shutdown).unwrap();

        assert!(matches!(p.acquire(&rx, &mut forced), Acquired::Shutdown));
        assert!(forced, "F5 pressed during a queue must not be swallowed");
    }

    /// The fast path must not consume commands meant for the actor's loop.
    #[test]
    fn an_uncontended_walk_does_not_touch_the_command_channel() {
        let p = WalkPermits::new(1);
        let (tx, rx) = idle_cmds();
        let mut forced = false;
        tx.send(IndexCmd::Refresh { force: true }).unwrap();

        let _held = p.acquire(&rx, &mut forced);
        assert!(!forced);
        assert_eq!(rx.len(), 1, "the refresh is still there for the wake loop");
    }
}
