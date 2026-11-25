//! Seeded random draws: the SplitMix64 generator, the Fisher–Yates shuffle, the Box–Muller normal
//! and the inverse-CDF draw over a cumulative distribution.
//!
//! Nothing here draws on its own (the crate contract's rule 1): [`SplitMix64`] is a value the caller
//! seeds and holds, and [`box_muller`] and [`draw_from_cumulative`] take their uniforms as
//! arguments, so a caller on another generator (`rand`'s `StdRng`, a `ChaCha8Rng`) shares the
//! transform without sharing the stream.
//!
//! ⚠ A seeded stream is compared BIT FOR BIT by its callers (the TPE sampler's proposal sequence, a
//! permutation importance, a synthetic price path): one perturbed draw forks every later one rather
//! than nudging one number. The constants, the steps and the operand order are the contract.

/// A seeded SplitMix64 generator (Steele, Lea & Flood, 2014): exact `u64` arithmetic, so the same
/// stream on every platform and for ever, which `rand`'s `StdRng` does not promise across versions.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator seeded with `seed`; its first [`Self::next_u64`] mixes `seed + 0x9E37_79B9_7F4A_7C15`.
    #[inline]
    pub fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    /// The next 64 random bits: the canonical SplitMix64 step.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform on `[0, 1)` from the top 53 bits (the `f64` mantissa width), exact. Zero is a possible
    /// draw: a caller taking its logarithm floors it first.
    #[inline]
    pub fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Shuffles `values` in place by Fisher–Yates, from the last index down to 1, swapping index `i`
/// with `next_u64() % (i + 1)`. The modulo is biased by at most `(i + 1) / 2^64`, far below anything
/// a slice can resolve.
pub fn shuffle<T>(values: &mut [T], generator: &mut SplitMix64) {
    for i in (1..values.len()).rev() {
        let j = (generator.next_u64() % (i as u64 + 1)) as usize;
        values.swap(i, j);
    }
}

/// One standard normal from two uniforms by Box–Muller, the cosine half:
/// `√(−2 ln u1) · cos(τ u2)`, with `libm`'s `log` and `cos` (the `sqrt` is IEEE 754's, correctly
/// rounded everywhere). `u1` must lie in `(0, 1]`, and each caller prepares its own (a `[0, 1)` draw
/// floored at `f64::MIN_POSITIVE`, or a draw that is never zero); `u1 = 0` gives an infinite or `NaN`
/// result. Pass the uniforms in the order they were drawn, `u1` first.
#[inline]
pub fn box_muller(u1: f64, u2: f64) -> f64 {
    (-2.0 * libm::log(u1)).sqrt() * libm::cos(std::f64::consts::TAU * u2)
}

/// The inverse-CDF draw: the first index `i` with `u < cumulative[i]`, else the last index, which so
/// takes any rounding shortfall of a running total that ends below 1.
///
/// `cumulative` is a running sum of probabilities in index order and `u` a uniform on `[0, 1)`. The
/// comparison is STRICT: a `u` exactly on a boundary belongs to the next bin, so a bin of probability
/// zero is never drawn (the first one included) unless it is the last bin taking a shortfall. Panics
/// on an empty `cumulative`.
#[inline]
pub fn draw_from_cumulative(cumulative: &[f64], u: f64) -> usize {
    assert!(!cumulative.is_empty(), "an inverse-CDF draw needs at least one bin");
    cumulative.iter().position(|&total| u < total).unwrap_or(cumulative.len() - 1)
}

#[cfg(test)]
#[path = "random_tests.rs"]
mod tests;
