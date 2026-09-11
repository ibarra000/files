//! Retry pacing for the index refresher.
//!
//! Full jitter, not equal jitter: with a single client the only job jitter has
//! is to avoid re-synchronising with a server-side hiccup, and full jitter
//! occasionally retries fast - which is exactly what you want when a VPN
//! reconnects.
//!
//! The healthy cadence gets its own +/-10% jitter too, so a fleet of these
//! apps does not hammer the file server in lockstep on the minute.

use std::time::Duration;

use crate::util::rng::Rng;

/// Backoff schedule for a repeatedly failing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub cap: Duration,
}

impl Backoff {
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// Full-jitter delay for `attempt` (0-based): uniform in
    /// `0 ..= min(cap, base << attempt)`.
    pub fn delay(&self, attempt: u32, rng: &mut Rng) -> Duration {
        let ceiling = self.ceiling(attempt);
        Duration::from_nanos(rng.next_in(ceiling.as_nanos().min(u128::from(u64::MAX)) as u64))
    }

    /// The exponential ceiling for `attempt`, before jitter.
    pub fn ceiling(&self, attempt: u32) -> Duration {
        // `checked_mul` saturates the shift instead of wrapping on a long
        // outage - an unreachable drive must not produce a zero-length sleep.
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.base
            .checked_mul(factor)
            .unwrap_or(self.cap)
            .min(self.cap)
    }
}

/// Applies +/-`percent` jitter to a nominal interval.
pub fn jitter(interval: Duration, percent: u32, rng: &mut Rng) -> Duration {
    if percent == 0 || interval.is_zero() {
        return interval;
    }
    let nanos = interval.as_nanos().min(u128::from(u64::MAX)) as u64;
    let spread = nanos / 100 * u64::from(percent.min(100));
    if spread == 0 {
        return interval;
    }
    // Uniform in [nanos - spread, nanos + spread].
    let offset = rng.next_in(spread * 2);
    Duration::from_nanos(nanos.saturating_sub(spread).saturating_add(offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(300));

    #[test]
    fn ceiling_doubles_then_saturates_at_the_cap() {
        assert_eq!(B.ceiling(0), Duration::from_secs(5));
        assert_eq!(B.ceiling(1), Duration::from_secs(10));
        assert_eq!(B.ceiling(2), Duration::from_secs(20));
        assert_eq!(B.ceiling(6), Duration::from_secs(300)); // 5*64=320 -> capped
        assert_eq!(B.ceiling(60), Duration::from_secs(300));
        assert_eq!(B.ceiling(u32::MAX), Duration::from_secs(300));
    }

    #[test]
    fn delay_never_exceeds_the_ceiling() {
        let mut rng = Rng::from_seed(1234);
        for attempt in 0..40 {
            for _ in 0..100 {
                assert!(B.delay(attempt, &mut rng) <= B.ceiling(attempt));
            }
        }
    }

    #[test]
    fn full_jitter_sometimes_retries_quickly() {
        let mut rng = Rng::from_seed(77);
        let quick = (0..500)
            .filter(|_| B.delay(8, &mut rng) < Duration::from_secs(30))
            .count();
        assert!(
            quick > 0,
            "full jitter should sometimes produce a short delay"
        );
    }

    #[test]
    fn delay_is_deterministic_for_a_given_seed() {
        let a = B.delay(3, &mut Rng::from_seed(5));
        let b = B.delay(3, &mut Rng::from_seed(5));
        assert_eq!(a, b);
    }

    #[test]
    fn jitter_stays_within_the_requested_band() {
        let mut rng = Rng::from_seed(9);
        let nominal = Duration::from_secs(60);
        for _ in 0..500 {
            let d = jitter(nominal, 10, &mut rng);
            assert!(d >= Duration::from_secs(54), "{d:?} below band");
            assert!(d <= Duration::from_secs(66), "{d:?} above band");
        }
    }

    #[test]
    fn zero_percent_jitter_is_the_identity() {
        let mut rng = Rng::from_seed(9);
        assert_eq!(
            jitter(Duration::from_secs(60), 0, &mut rng),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn jitter_of_a_zero_interval_is_zero() {
        let mut rng = Rng::from_seed(9);
        assert_eq!(jitter(Duration::ZERO, 10, &mut rng), Duration::ZERO);
    }
}
