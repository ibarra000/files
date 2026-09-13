//! A cap on how many shares are walked at once.
//!
//! One tree walk already runs [`WALK_CONCURRENCY`] directory reads in flight,
//! a number chosen because a share is someone else's production file server;
//! see that constant's own comment. Ten configured mappings without a cap is
//! ten walks at once, eighty outstanding SMB requests, and a first run that
//! looks to the server like an attack rather than a client.
//!
//! # Why a channel rather than a mutex and a condition variable
//!
//! The actor waiting here must still answer a shutdown. A `Condvar` would need
//! a timed poll loop to notice one; a bounded channel pre-filled with `n`
//! units composes with `select!` over the actor's existing command receiver,
//! which is exactly how [`super::actor`] already blocks between wakes. A
//! walker parked for a permit therefore leaves as promptly as an idle one.
//!
//! # Why the count can never drift
//!
//! Exactly `n` units exist for the life of the process. [`WalkPermit`] returns
//! its unit on `Drop`, into a channel with capacity `n` that by construction
//! cannot be full, so the send cannot fail and cannot block. `panic` is
//! deliberately left at `unwind` (see `Cargo.toml`), so a walk that panics
//! still runs `Drop` before the actor's `catch_unwind` sees the payload.

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
    /// `n` is clamped to at least one: a cap of zero would mean no share is
    /// ever indexed, which is not a configuration anyone means.
    pub fn new(n: usize) -> Self {
        let n = n.max(1);
        let (tx, rx) = crossbeam_channel::bounded(n);
        for _ in 0..n {
            tx.send(()).expect("a fresh bounded channel has room");
        }
        Self { tx, rx }
    }

    /// Permits not currently held. For tests and `--doctor`.
    pub fn available(&self) -> usize {
        self.rx.len()
    }

    /// Takes a permit, or reports that the actor should stop.
    ///
    /// A `Refresh` arriving while queued is folded into `forced` rather than
    /// dropped: the scan this permit is for has not started, so the force
    /// still applies to it. A `Changed` is ignored for the opposite reason -
    /// the full pass about to run covers whatever the watcher saw.
    pub fn acquire(&self, cmds: &Receiver<IndexCmd>, forced: &mut bool) -> Acquired<'_> {
        // The fast path, so an uncontended walk pays nothing for the cap.
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

/// Held for exactly as long as one full pass.
///
/// Deliberately not `Clone`: one guard is one unit.
pub struct WalkPermit<'a> {
    tx: &'a Sender<()>,
}

impl Drop for WalkPermit<'_> {
    fn drop(&mut self) {
        // Cannot fail: the channel has capacity for every unit that exists,
        // and this is one of them coming home.
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

    /// The property the whole design exists for: an actor queued behind a
    /// walk must still be able to shut down. Without it, quitting during a
    /// cold start waits out every walk ahead of it.
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

    /// A forced refresh that lands while actually queued still applies to the
    /// scan the permit is being taken for - it has not started yet.
    ///
    /// The permit must genuinely be contended for this to exercise anything:
    /// with one free, `acquire` takes the fast path and never looks at the
    /// command channel, which is correct - the actor's own wake loop picks the
    /// refresh up instead.
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

    /// The fast path is a fast path: an uncontended walk must not pay for the
    /// cap, and must not steal commands the actor's own loop needs to see.
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
