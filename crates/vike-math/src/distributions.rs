//! The normal distribution: its CDF in two forms, its quantile and its density; and, over its CDF,
//! the probability a lognormal spot closes a window above its open ([`up_probability`]).
//!
//! ⚠ Results here are compared BIT FOR BIT (the options pricing parity fixtures, `up_probability`'s
//! parity gate, the per-crate libm platform probes), so an expression's operand order and association are
//! part of its contract: a rewrite that is algebraically equal is still a change. Every transcendental
//! comes from the `libm` crate, never from `f64`'s platform-libm methods
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`).
//!
//! The standard normal has THREE functions here and they are not interchangeable: [`normal_cdf`]
//! (the `erf` form, saturating in the tails), [`normal_cdf_complementary_form`] (the `erfc` form,
//! precise in the lower tail) and [`normal_quantile`] (Φ⁻¹, refined against the `erfc` form). They
//! are a documented pair, not a duplicate: their tail bits differ and each is pinned (the options
//! fixtures need the saturating one, the probabilistic Sharpe ratio the precise one).

/// The standard-normal CDF `Φ(x) = ½·(1 + erf(x/√2))`, with `erf` from `libm` (pure-Rust FDLIBM,
/// bit-identical to CPython's `math.erf`).
///
/// Saturates to exactly `0.0`/`1.0` in the far tails (`erf` reaches ±1), which the options parity
/// fixtures' deep out-of-the-money prices rely on. Not the `erfc` form `0.5·erfc(−x/√2)`: that one
/// keeps precision in the lower tail and so returns DIFFERENT bits there.
#[inline]
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

/// The standard-normal CDF in its `erfc` form, `Φ(x) = ½·erfc(−x/√2)`, with `erfc` from `libm`.
///
/// Not [`normal_cdf`]: `erfc` never cancels, so this form keeps its precision in the lower tail where
/// `1 + erf` loses it, and the two return DIFFERENT bits there. It is the CDF of `vike-analytics`'
/// probabilistic Sharpe ratio and normal p-values, and the one [`normal_quantile`]'s refinement step
/// inverts. Moved here unchanged from `vike-analytics`' `overfit` module (decision 0141) when
/// `vike-ml-training`'s double machine learning needed the quantile.
pub fn normal_cdf_complementary_form(x: f64) -> f64 {
    0.5 * libm::erfc(-x / std::f64::consts::SQRT_2)
}

/// The inverse standard-normal CDF Φ⁻¹ (the quantile): `normal_quantile(0.975)` is the 1.96 of a
/// two-sided 95% interval; `p <= 0` is `-∞` and `p >= 1` is `+∞`.
///
/// Acklam's rational approximation (|rel err| ≲ 1.15e-9 on its own), refined by one step of
/// Halley's rational method — Acklam's own documented enhancement
/// (<http://home.online.no/~pjacklam/notes/invnorm/>) — to push accuracy to ~1e-15 (full double
/// precision). The bare rational approximation is a *relative*-error bound: at |z| appreciably
/// above 1 that is a few ULP shy of 1e-9 in absolute terms, which is too tight for the Φ/Φ⁻¹
/// roundtrip test (measured -2.57e-9 at x=-2.5 pre-refinement); the refinement closes that gap
/// without touching the test's tolerance. Moved here unchanged from `vike-analytics`' `overfit`
/// module, where `expected_max_sharpe` calls it.
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.38357751867269e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const PLOW: f64 = 0.02425;
    const PHIGH: f64 = 1.0 - PLOW;
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    let x = if p < PLOW {
        let q = (-2.0 * libm::log(p)).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= PHIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * libm::log(1.0 - p)).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // Halley step: e = Φ(x) - p; u = e * sqrt(2π) * exp(x²/2); x -= u / (1 + x*u/2).
    let e = normal_cdf_complementary_form(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * libm::exp(x * x / 2.0);
    x - u / (1.0 + x * u / 2.0)
}

/// The normal density `N(x; mean, deviation) = exp(−z²/2) / (deviation · √τ)`, `z = (x − mean) /
/// deviation`, with `exp` from `libm` (the `sqrt` is IEEE 754's, correctly rounded everywhere).
///
/// The standard normal is `normal_density(x, 0.0, 1.0)`, the same bits as the textbook
/// `exp(−x²/2) / √(2π)`: `(x − 0.0) / 1.0` is `x` exactly (`-0.0` included), `1.0 · √τ` is `√τ`,
/// and `τ` is `2π` exactly. `deviation > 0` is the caller's: zero gives an infinite or `NaN` density.
#[inline]
pub fn normal_density(x: f64, mean: f64, deviation: f64) -> f64 {
    let z = (x - mean) / deviation;
    libm::exp(-0.5 * z * z) / (deviation * std::f64::consts::TAU.sqrt())
}

/// The probability that a lognormal spot closes a window ABOVE where it opened, Bachelier-style under
/// drift-sensitivity `beta`: `Φ(β · ln(spot_now / spot_open) / (σ · √max(window − elapsed, 1e-9)))`.
/// The port of the retired Python oracle's `p_up_scalar`, moved here from `vike_model::fair::p_up`
/// expression for expression (decision 0294), so its bits are that function's.
///
/// `spot_now`/`spot_open` are prices, `sigma` the per-second log-return deviation, `elapsed` the
/// seconds into the window and `window` its length. The `max(window − elapsed, 1e-9)` floor is the
/// oracle's: it keeps the denominator finite in the window's last instant instead of dividing by zero.
///
/// ⚠ **`libm::log`, never `f64::ln`, and the `erf` inside [`normal_cdf`] is `libm`'s too**
/// (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`): this function was
/// half-converted for weeks, `erf` through `libm` and the logarithm one line above it still the
/// platform's, and a half-converted function reads as done. A last bit here is never absorbed
/// downstream: it reaches a posted price by a whole tick (`crates/vike-market-making/src/avellaneda.rs`'s
/// `model_fair`, ending in a snap to the tick), turns a quote into no quote at the strict three-way
/// straddle guard, picks a different beta at a strict-`<` minimum
/// (`crates/vike-strategy/src/strategies/fair_value.rs`'s `wc_betas`), and moves the TRADE SET at a
/// strict-`>` entry gate whose one-ulp sensitivity is pinned
/// (`crates/vike-strategy/src/strategies/cheap_np.rs`). `crates/vike-strategy/tests/cheap_np_parity.rs`
/// asserts the oracle's exact entry COUNTS through it, so a rewrite that is equal algebraically is
/// still a change (this module's doc).
///
/// MEASURED 2026-08-26, when the logarithm was converted: that suite's
/// `sample_windows_reproduce_the_python_gate_entry_for_entry` and `sample_fixture_holds_whole_windows`
/// passed unchanged over the committed 10,973-row fixture. ⚠ DECLARED LIMIT: its 7.1M-row case,
/// `full_reference_set_reproduces_every_published_number`, is `#[ignore]`d and reads
/// `.cheapnp_ref/signals.bin`, which no checkout has, so a one-ulp shift there is UNVERIFIED rather
/// than passing. A count that moves when it is run is decision 0021's question, never a re-record.
#[inline]
pub fn up_probability(
    beta: f64,
    spot_now: f64,
    spot_open: f64,
    sigma: f64,
    elapsed: f64,
    window: f64,
) -> f64 {
    let denominator = sigma * (window - elapsed).max(1e-9).sqrt();
    // `.sqrt()` stays IEEE 754's: correctly rounded on every platform, it never diverged.
    let x = libm::log(spot_now / spot_open) * beta / denominator;
    normal_cdf(x)
}

#[cfg(test)]
#[path = "distributions_tests.rs"]
mod tests;
