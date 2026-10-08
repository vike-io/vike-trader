//! Black–Scholes greeks computed locally from implied volatility — ports
//! `vike-trader-app data/options/greeks.py`.
//!
//! Feeds give IV but not full greeks, so Δ/Γ/Θ/V are derived uniformly. European exercise,
//! continuous compounding, no dividends; Θ is per calendar day, V per 1 vol-point; the year
//! basis is 365 calendar days (matching the per-day Θ denominator). Contract notes:
//! - **`r` is a plain parameter.** The Python twin resolves env `options_risk_free` at import
//!   (default 0.0); env reads stay in the consuming binary — pass `0.0` to match the default.
//! - **EVERY transcendental here comes from the `libm` CRATE — `erf`, `exp` AND `log`** (pure-Rust
//!   MUSL/FDLIBM). `libm::erf` is empirically ≤1 ulp of CPython 3.14's `math.erf`, and it replaces
//!   vike-app's old A&S-7.1.26 polynomial (≈1e-7 — the known divergence).
//!
//!   ⚠ **This bullet used to read "erf comes from `libm`" and stopped there — and for as long as
//!   it did, it was describing a HALF-converted file.** `norm_pdf`, `d1_d2`, BOTH theta arms of
//!   [`black_scholes_greeks`] and [`black_scholes_price`]'s discount factor went on calling
//!   `f64::exp` and `f64::ln`, i.e. the PLATFORM's libm, on the lines directly beneath a
//!   `libm::erf`. **Say plainly what that costs, because it is the whole lesson: a half-converted
//!   file is worse than an unconverted one.** An unconverted file invites the question; a
//!   `libm::erf` sitting two lines above an `x.ln()` ANSWERS it, wrongly, and the next reader does
//!   not check. It had only ever been settled for `erf`, and for a reason that generalises to
//!   nothing — `std` has no `erf` at all, so there was no choice to make there. The five remaining
//!   calls were converted 2026-08-26 under
//!   `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//!   requires `+ - * /` and `sqrt` correctly rounded and requires NOTHING of `exp`/`ln`, so MSVC's
//!   CRT and glibc are each entitled to a different last bit on those. ⚠ **Whether they actually
//!   diverged on THIS crate's domain was never measured** — `vike-options` has no
//!   `libm_platform_probe` twin of the ones in `vike-analytics`/`vike-indicators`/`vike-mm`, and
//!   this conversion did not add one. That is a reason to convert rather than a reason to wait:
//!   the sibling crates measured `exp` and `ln` diverging on every domain they probed, and an
//!   options price feeds a greek that feeds a hedge. `t.sqrt()` and `(2·π).sqrt()` are deliberately
//!   left on `f64::sqrt` — that one IS required correctly rounded, so it is portable already.
//! - **Parity tier** per `fixtures/README.md`: these are erf/exp/log-derived outputs, gated at
//!   ≤1e-12 relative against CPython-pinned hex bits — NEVER widened. Invalid-input `None`s
//!   and the erf-saturated deep-OTM exact-0.0 edge stay exact.
//!
//!   ⚠ **Nothing in `crates/vike-options/tests/oracle_parity.rs` was re-recorded for the `libm`
//!   conversion, and that is a claim about the TIER rather than about the arithmetic.** Every
//!   price/greek/IV pin there goes through its `assert_rel` at ≤1e-12 relative; a ≤1-ulp move is
//!   ~2e-16 relative, four orders inside the gate. The one `assert_bits` pin over a
//!   transcendental-derived value, that file's
//!   `deep_otm_price_is_exactly_zero`, is exactly 0.0 because `erf` SATURATES to ±1 at those
//!   `d1`/`d2` — a last-bit change in `d1` leaves it saturated, so the exact edge survives by
//!   construction. Those constants are the frozen CPython oracle
//!   (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`), which ADR 0032 says
//!   may NOT be re-recorded: if one of them ever moves out of tier, that is a STOP to be argued,
//!   not a number to repaste.
//! - Expression shapes/association mirror the Python source exactly (parity is load-bearing) —
//!   `libm::exp(-r * t)` for `(-r * t).exp()` keeps the operand and the association identical, and
//!   is the only kind of rewrite this file permits.

use crate::model::{OptionKind, OptionQuote, expiry_ms};

const MS_PER_YEAR: f64 = 365.0 * 86_400.0 * 1000.0;

pub(crate) fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

pub(crate) fn norm_pdf(x: f64) -> f64 {
    // `libm::exp`, never `f64::exp` (ADR 0032) — the standard-normal density feeds gamma, vega and
    // both theta arms, and every one of `crate::second_order`'s five greeks through that module's
    // `ctx`, so one platform-chosen bit here reaches most of the crate's output surface at once.
    // (Delta is the exception: it comes from `norm_cdf`, i.e. from `libm::erf`.) The `sqrt` stays
    // on `f64` — IEEE 754 requires it correctly rounded, so `(2·π).sqrt()` is already the same
    // constant on every box, and routing it through `libm` would move a value for nothing.
    libm::exp(-0.5 * x * x) / (2.0 * std::f64::consts::PI).sqrt()
}

/// Black–Scholes `d1`/`d2` intermediate terms (plus `sqrt(t)`, reused by the greeks for
/// theta/gamma/vega). The ONE site computing these — `black_scholes_greeks`,
/// `black_scholes_price`, and the second-order greeks (`crate::second_order`) share it
/// instead of each re-deriving the identical expression.
#[allow(clippy::many_single_char_names)] // s/k/t/σ/r are the domain vocabulary
pub(crate) fn d1_d2(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> (f64, f64, f64) {
    let sqrt_t = t.sqrt();
    // `libm::log`, never `f64::ln` (ADR 0032). This is the single most load-bearing conversion in
    // the crate: `d1_d2` is the ONE site computing these terms, so price, greeks, the 64-step IV
    // bisection and every second-order greek all inherit whatever last bit this line produces.
    // `t.sqrt()` above stays on `f64` — correctly rounded by IEEE 754, hence already portable.
    let d1 = (libm::log(s / k) + (r + 0.5 * sigma * sigma) * t) / (sigma * sqrt_t);
    let d2 = d1 - sigma * sqrt_t;
    (d1, d2, sqrt_t)
}

/// (delta, gamma, theta_per_day, vega_per_point), or `None` if inputs are invalid — twin of
/// `greeks.black_scholes_greeks`.
#[allow(clippy::many_single_char_names)] // s/k/t/r are the domain vocabulary (S, K, t, σ, r)
pub fn black_scholes_greeks(
    s: f64,
    k: f64,
    t: f64,
    sigma: f64,
    kind: OptionKind,
    r: f64,
) -> Option<(f64, f64, f64, f64)> {
    if s <= 0.0 || k <= 0.0 || t <= 0.0 || sigma <= 0.0 {
        return None;
    }
    let (d1, d2, sqrt_t) = d1_d2(s, k, t, sigma, r);
    let pdf_d1 = norm_pdf(d1);
    // The `e^{-rt}` discount in BOTH theta arms is `libm::exp`, never `f64::exp` (ADR 0032). Both
    // arms carry it because the expression shape mirrors the Python source arm-for-arm and a
    // hoisted local would change that; they are the same value, computed twice, as before.
    let (delta, theta) = match kind {
        OptionKind::Call => (
            norm_cdf(d1),
            -(s * pdf_d1 * sigma) / (2.0 * sqrt_t) - r * k * libm::exp(-r * t) * norm_cdf(d2),
        ),
        OptionKind::Put => (
            norm_cdf(d1) - 1.0,
            -(s * pdf_d1 * sigma) / (2.0 * sqrt_t) + r * k * libm::exp(-r * t) * norm_cdf(-d2),
        ),
    };
    let gamma = pdf_d1 / (s * sigma * sqrt_t);
    let vega = s * pdf_d1 * sqrt_t;
    Some((delta, gamma, theta / 365.0, vega / 100.0))
}

/// Black–Scholes option price, or `None` if inputs are invalid — twin of
/// `greeks.black_scholes_price`.
#[allow(clippy::many_single_char_names)]
pub fn black_scholes_price(
    s: f64,
    k: f64,
    t: f64,
    sigma: f64,
    kind: OptionKind,
    r: f64,
) -> Option<f64> {
    if s <= 0.0 || k <= 0.0 || t <= 0.0 || sigma <= 0.0 {
        return None;
    }
    let (d1, d2, _sqrt_t) = d1_d2(s, k, t, sigma, r);
    // `libm::exp`, never `f64::exp` (ADR 0032) — and this one is the discount factor that
    // `implied_vol`'s bisection re-evaluates up to 64 times per solve, so its portability is what
    // makes a solved IV the same number on the desktop and on the CI box.
    let disc = libm::exp(-r * t);
    Some(match kind {
        OptionKind::Call => s * norm_cdf(d1) - k * disc * norm_cdf(d2),
        OptionKind::Put => k * disc * norm_cdf(-d2) - s * norm_cdf(-d1),
    })
}

/// Invert Black–Scholes for sigma via bisection (sigma in [1e-4, 5.0]) — twin of
/// `greeks.implied_vol`.
///
/// Returns `None` when price/inputs are invalid or the price is outside the no-arbitrage band
/// (e.g. below intrinsic) — i.e. not solvable. 64 fixed steps with a 1e-6 absolute early-exit,
/// ported verbatim (the iteration/exit shape is part of the oracle contract).
///
/// ⚠ **This is the ONE place in the crate where a last-bit change in `black_scholes_price` could
/// amplify rather than stay a last bit, and the 2026-08-26 `libm` conversion is exactly such a
/// change — so state the bound rather than waving at it.** The loop is a DISCRETE search: an
/// amplification needs a comparison to flip, and there are two. `pm < price` decides the bracket,
/// and at every step before the exit the two are separated by far more than an ulp, so it cannot
/// flip. `(pm - price).abs() < 1e-6` decides WHICH step returns, and flipping it would return a
/// neighbouring `mid` — a jump of one bisection width, which at the exit depth is ~1e-10 to 1e-8 in
/// sigma depending on the probe's vega, i.e. orders of magnitude outside the ≤1e-12 relative parity
/// tier. That flip requires `|pm - price|` to land
/// within ~1 ulp of the 1e-6 threshold itself on the pinned probe; it does not, but that is an
/// argument from the arithmetic and **not a measurement**. The parity suite is the instrument:
/// `crates/vike-options/tests/oracle_parity.rs`'s `implied_vol_bisection_reference` is the test
/// that would catch it, and a failure there is a STOP under ADR 0032's oracle rule rather than a
/// value to re-record.
#[allow(clippy::many_single_char_names)]
pub fn implied_vol(price: f64, s: f64, k: f64, t: f64, kind: OptionKind, r: f64) -> Option<f64> {
    if price <= 0.0 || s <= 0.0 || k <= 0.0 || t <= 0.0 {
        return None;
    }
    let (mut lo, mut hi) = (1e-4, 5.0);
    let p_lo = black_scholes_price(s, k, t, lo, kind, r)?;
    let p_hi = black_scholes_price(s, k, t, hi, kind, r)?;
    if !(p_lo <= price && price <= p_hi) {
        return None;
    }
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        let pm = black_scholes_price(s, k, t, mid, kind, r)?;
        if (pm - price).abs() < 1e-6 {
            return Some(mid);
        }
        if pm < price {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Time to expiry in years (clamped to >= 0), expiry assumed ~08:00 UTC — twin of
/// `greeks.years_to_expiry`.
pub fn years_to_expiry(expiry_iso: &str, now_ms: i64) -> f64 {
    (((expiry_ms(expiry_iso) - now_ms) as f64) / MS_PER_YEAR).max(0.0)
}

/// A copy of `q` with greeks filled from its IV — twin of `greeks.enrich_quote`.
///
/// Unchanged if greeks aren't computable (no spot, no IV, or `t <= 0` for an expired option).
/// `r` is explicit here too (the Python twin uses its import-time default).
pub fn enrich_quote(q: OptionQuote, s: Option<f64>, t: f64, r: f64) -> OptionQuote {
    let (Some(s), Some(iv)) = (s, q.iv) else { return q };
    let Some((delta, gamma, theta, vega)) = black_scholes_greeks(s, q.strike, t, iv, q.kind, r)
    else {
        return q;
    };
    OptionQuote {
        delta: Some(delta),
        gamma: Some(gamma),
        theta: Some(theta),
        vega: Some(vega),
        ..q
    }
}
