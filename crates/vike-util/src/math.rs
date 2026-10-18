//! Numeric helpers two or more crates copied (decision 0141).
//!
//! ⚠ Results here are compared BIT FOR BIT (the options pricing parity fixtures, `p_up`'s cheap_np
//! parity gate, the per-crate libm platform probes), so an expression's operand order and
//! association are part of its contract: a rewrite that is algebraically equal is still a change.
//! Every transcendental comes from the `libm` crate, never from `f64`'s platform-libm methods
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`).

/// The standard-normal CDF `Φ(x) = ½·(1 + erf(x/√2))`, with `erf` from `libm` (pure-Rust FDLIBM,
/// bit-identical to CPython's `math.erf`).
///
/// Saturates to exactly `0.0`/`1.0` in the far tails (`erf` reaches ±1), which the options parity
/// fixtures' deep out-of-the-money prices rely on. Not the `erfc` form `0.5·erfc(−x/√2)`: that one
/// keeps precision in the lower tail and so returns DIFFERENT bits there.
#[inline]
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

#[cfg(test)]
#[path = "math_tests.rs"]
mod tests;
