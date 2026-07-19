//! Deterministic generator for the fixtures
//!
//! Wraps `rand_pcg::Pcg32`, the rand project's PCG-XSH-RR 64/32
//! Adds a bounded-draw helper method as `rand_core` exposes no
//! range method, and pulling in the whole `rand` crate for one
//! is unnecessary

// Bring the generator trait's methods into scope without a name to avoid a clash
// with this module's newtype `Rng`
use rand_core::{Rng as _, SeedableRng};
use rand_pcg::Pcg32;

/// The fixtures' deterministic random source
#[derive(Debug)]
pub(crate) struct Rng(Pcg32);

impl Rng {
    /// Seed the generator from the committed seed
    ///
    /// `seed_from_u64` is a fixed, portable expansion, so the same seed yields
    /// the same stream regardless of host
    pub(crate) fn new(seed: i64) -> Self {
        Self(Pcg32::seed_from_u64(seed.cast_unsigned()))
    }

    /// Draw the next 32-bit value
    pub(crate) fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    /// Draw a value in `0..bound`
    ///
    /// Maps the draw into range by taking the high word of the 64-bit product,
    /// so there is no division and no rejection loop. The bias this leaves is
    /// negligible for the small bounds used here. A `bound` of zero has no valid
    /// output and returns zero
    // The high word of the product is always below `bound` and so fits a u32,
    // the shift leaves at most 32 significant bits
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn below(&mut self, bound: u32) -> u32 {
        let draw = self.next_u32();
        (u64::from(draw).wrapping_mul(u64::from(bound)) >> 32) as u32
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::Rng;
    use crate::manifest::DEFAULT_SEED;

    /// The first draws for the committed seed, locked so a change to the seed or
    /// the pinned generator version is caught here rather than silently
    /// re-rolling every fixture
    const LOCKED_DRAWS: [u32; 8] = [
        3_400_036_912,
        3_349_765_444,
        515_166_382,
        4_224_707_908,
        1_556_152_254,
        2_777_144_205,
        3_631_167_442,
        1_399_683_183,
    ];

    #[test]
    fn first_draws_for_the_committed_seed_are_locked() {
        let mut rng = Rng::new(DEFAULT_SEED);
        let draws: Vec<u32> = (0..LOCKED_DRAWS.len()).map(|_| rng.next_u32()).collect();
        assert_eq!(draws, LOCKED_DRAWS);
    }

    #[test]
    fn the_same_seed_reproduces_the_same_stream() {
        let mut first = Rng::new(DEFAULT_SEED);
        let mut second = Rng::new(DEFAULT_SEED);
        for _ in 0..64 {
            assert_eq!(first.next_u32(), second.next_u32());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(DEFAULT_SEED);
        let mut b = Rng::new(DEFAULT_SEED.wrapping_add(1));
        let a_draws: Vec<u32> = (0..8).map(|_| a.next_u32()).collect();
        let b_draws: Vec<u32> = (0..8).map(|_| b.next_u32()).collect();
        assert_ne!(a_draws, b_draws);
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = Rng::new(DEFAULT_SEED);
        for _ in 0..1000 {
            assert!(rng.below(7) < 7);
        }
    }

    #[test]
    fn below_zero_bound_yields_zero() {
        let mut rng = Rng::new(DEFAULT_SEED);
        assert_eq!(rng.below(0), 0);
    }
}
