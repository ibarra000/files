//! Cooperative cancellation for work that has been superseded.
//!
//! Callers track generations (for example per query and per index snapshot).
//! A worker keeps the generation it started with and stops once it is stale.
//!
//! `Relaxed` ordering suffices: the guarded data (query string, snapshot `Arc`)
//! travels through a mutex or channel, which provides the happens-before edge.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing generation counter.
#[derive(Debug, Clone, Default)]
pub struct Epoch(Arc<AtomicU64>);

impl Epoch {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    /// Advances the generation and returns the new value.
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Moves the generation forward to `value`, never backwards.
    ///
    /// Lets a worker follow the caller's counter (such as the UI's query
    /// generation), so results and cancellation use the same number.
    pub fn advance_to(&self, value: u64) -> u64 {
        self.0.fetch_max(value, Ordering::Relaxed).max(value)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.current() == generation
    }

    /// A token that reports cancellation once the epoch moves past
    /// `generation`.
    pub fn token(&self, generation: u64) -> CancelToken {
        CancelToken {
            epoch: Some(self.clone()),
            generation,
            flag: None,
        }
    }
}

/// Cancellation flag checked periodically by long-running work.
///
/// Checked between units of work (a rayon chunk in the matcher, a buffer
/// refill in the enumerator); a blocking SMB call cannot be interrupted.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    epoch: Option<Epoch>,
    generation: u64,
    flag: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl CancelToken {
    /// A token that is never cancelled.
    pub fn never() -> Self {
        Self::default()
    }

    /// A token cancelled by setting a flag, for callers without an epoch.
    pub fn from_flag(flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            epoch: None,
            generation: 0,
            flag: Some(flag),
        }
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        if let Some(flag) = &self.flag
            && flag.load(Ordering::Relaxed)
        {
            return true;
        }
        match &self.epoch {
            Some(e) => !e.is_current(self.generation),
            None => false,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_fresh_token_is_live() {
        let e = Epoch::new();
        let t = e.token(e.current());
        assert!(!t.is_cancelled());
    }

    #[test]
    fn bumping_the_epoch_cancels_outstanding_tokens() {
        let e = Epoch::new();
        let t = e.token(e.current());
        e.bump();
        assert!(t.is_cancelled());
    }

    #[test]
    fn a_token_taken_after_the_bump_is_live_again() {
        let e = Epoch::new();
        let stale = e.token(e.current());
        let generation = e.bump();
        let fresh = e.token(generation);
        assert!(stale.is_cancelled());
        assert!(!fresh.is_cancelled());
    }

    #[test]
    fn advance_to_moves_forward_but_never_backward() {
        let e = Epoch::new();
        e.advance_to(10);
        assert_eq!(e.current(), 10);
        e.advance_to(4);
        assert_eq!(
            e.current(),
            10,
            "a late arrival must not rewind the generation"
        );
        e.advance_to(11);
        assert_eq!(e.current(), 11);
    }

    #[test]
    fn advancing_cancels_older_tokens() {
        let e = Epoch::new();
        let old = e.token(1);
        e.advance_to(5);
        assert!(old.is_cancelled());
        assert!(!e.token(5).is_cancelled());
    }

    #[test]
    fn bump_returns_increasing_generations() {
        let e = Epoch::new();
        assert_eq!(e.bump(), 1);
        assert_eq!(e.bump(), 2);
        assert_eq!(e.current(), 2);
    }

    #[test]
    fn the_never_token_is_never_cancelled() {
        assert!(!CancelToken::never().is_cancelled());
    }

    #[test]
    fn a_flag_token_reflects_the_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let t = CancelToken::from_flag(Arc::clone(&flag));
        assert!(!t.is_cancelled());
        flag.store(true, Ordering::Relaxed);
        assert!(t.is_cancelled());
    }

    #[test]
    fn epochs_are_shared_across_clones() {
        let a = Epoch::new();
        let b = a.clone();
        let t = a.token(a.current());
        b.bump();
        assert!(t.is_cancelled(), "clones must share one counter");
    }
}
