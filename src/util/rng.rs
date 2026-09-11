//! A tiny seedable PRNG, used only for backoff jitter.
//!
//! Hand-rolled rather than pulling in `rand`: jitter is the only randomness in
//! the program, and a seedable generator makes the backoff schedule
//! deterministic under test.

/// SplitMix64. Fast, small, and good enough for jitter.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub const fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Seeds from the wall clock and the process id.
    pub fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::from_seed(nanos ^ (u64::from(std::process::id()) << 32) ^ 0xA076_1D64_78BD_642F)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..=max`.
    pub fn next_in(&mut self, max: u64) -> u64 {
        if max == u64::MAX {
            return self.next_u64();
        }
        let bound = max + 1;
        // Rejection sampling, to avoid modulo bias.
        let zone = u64::MAX - (u64::MAX % bound) - 1;
        loop {
            let v = self.next_u64();
            if v <= zone {
                return v % bound;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_for_a_given_seed() {
        let mut x = Rng::from_seed(7);
        let mut y = Rng::from_seed(7);
        for _ in 0..64 {
            assert_eq!(x.next_u64(), y.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        assert_ne!(Rng::from_seed(1).next_u64(), Rng::from_seed(2).next_u64());
    }

    #[test]
    fn next_in_stays_within_bounds() {
        let mut r = Rng::from_seed(99);
        for max in [0u64, 1, 5, 1000, u64::MAX] {
            for _ in 0..200 {
                assert!(r.next_in(max) <= max, "exceeded {max}");
            }
        }
    }

    #[test]
    fn next_in_zero_is_always_zero() {
        let mut r = Rng::from_seed(3);
        for _ in 0..50 {
            assert_eq!(r.next_in(0), 0);
        }
    }

    #[test]
    fn covers_the_whole_small_range() {
        let mut r = Rng::from_seed(5);
        let mut seen = [false; 4];
        for _ in 0..500 {
            seen[r.next_in(3) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "distribution should cover 0..=3");
    }
}
