//! Deterministic PRNG for the proxy's decision engine.
//!
//! Implements splitmix64 (no external `rand` dependency) so that every
//! profile decision sequence is a pure function of the seed.

/// SplitMix64: a small, fast, well-mixed 64-bit PRNG.
///
/// The same seed always produces the identical stream, which is the basis
/// of the proxy's reproducibility guarantee.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Create a generator from a seed (0 is a valid seed).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        // The standard splitmix64 step: add a golden-ratio constant, then
        // mix with two multiply-xor rounds and a final xor-shift.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Next uniform value in `[0, 1)` with 53 bits of precision.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_exact_stream() {
        let mut a = SplitMix64::new(0xA7B0_094A_5EED_A11C);
        let mut b = SplitMix64::new(0xA7B0_094A_5EED_A11C);
        for _ in 0..10_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_differ() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn next_f64_stays_in_unit_interval() {
        let mut rng = SplitMix64::new(42);
        for _ in 0..10_000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "value out of range: {v}");
        }
    }
}
