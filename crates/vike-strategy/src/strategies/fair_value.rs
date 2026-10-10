//! Fair-value 5-minute up/down primitives — PURE (no I/O, no engine types, no broker).
//!
//! Exact port of the live Polymarket bot's pure core,
//! `vike_db_data_jobs/trading/fair_value_bot/strategy.py` (the oracle; read-only), restricted to
//! what the **`cheap_np`** leg needs: `fee`, `p_up_scalar`, `wc_scalar`, `cheap_time_ok`,
//! `trailing_sigma`, `price_at`, and the gate constants. The other legs, the leg-band router
//! (`leg_of`), the delayed-fill helpers and the DDL are live-recording plumbing, NOT ported.
//!
//! The contract this module holds:
//!
//! * **`fee(p) = 0.072·p·(1−p)`** — Polymarket's probability-scaled taker fee, the strategy's own
//!   EDGE accounting (the engine prices the same shape via
//!   [`vike_model::FeeSchedule::ProbabilityScaled`]), so a plain scalar here.
//! * **`p_up`** — the Bachelier-style up-probability of a lognormal spot over the remaining window,
//!   `0.5·(1 + erf(x/√2))` with `x = ln(s_now/s_open)·β / (σ·√max(H−t, 1e-9))`. `erf` is
//!   `libm::erf`, called inside `vike_model::p_up` (which `p_up` below re-exports), pinned
//!   bit-identical to CPython's `math.erf` (`crates/vike-options/src/greeks.rs`'s `norm_cdf`).
//!   ⚠ `libm` is a direct dependency again: `trailing_sigma` calls `libm::log`
//!   (`docs/decisions/0032`).
//! * **`wc`** — the DUAL-β **worst case**: the MINIMUM over `BETAS = (0.83, 1.36)` of
//!   `prob − ask − fee(ask)` (`prob` = `p_up` for outcome 0 (Up), `1 − p_up` for 1 (Down)).
//!   Minimum, not mean: the gate fires only when BOTH βs agree the edge is there.
//! * **`trailing_sigma`** — the sample stddev of **1-second** log-returns over the last
//!   [`SIGMA_LOOKBACK_S`] seconds on a CONTIGUOUS 1-second grid with **linear interpolation of
//!   missing seconds** — load-bearing: `spot_1s` misses ~11% of its seconds, the backtest's SQL
//!   closes them with `WITH FILL STEP 1 INTERPOLATE (px)`, and σ on the raw sparse series does NOT
//!   match. `None` until [`SIGMA_MIN_SECONDS`] OBSERVED (not interpolated) seconds exist.
//!
//! Parity: both folds go through [`vike_model::py_sum`] (CPython ≥ 3.12's `sum()` is
//! Neumaier-compensated), and the `min` over βs mirrors Python's `min()` (keep-first-on-not-less)
//! rather than [`f64::min`], so NaN handling matches the oracle exactly.

use std::collections::BTreeMap;

use vike_model::py_sum;

/// Edge bar the `cheap_np` leg must clear (`strategy.py:12` `THETA`).
pub const THETA: f64 = 0.055;
/// The dual drift-sensitivity betas the worst case is taken over (`strategy.py:13` `BETAS`).
pub const BETAS: [f64; 2] = [0.83, 1.36];
/// Window length in seconds — the 5-minute up/down market (`strategy.py:14` `H`).
pub const H: f64 = 300.0;
/// Polymarket taker fee rate in the probability-scaled curve (`strategy.py:158` `fee`).
pub const FEE_RATE: f64 = 0.072;
/// Cheap band, inclusive lower bound (`strategy.py:28` `CHEAP_LO`).
pub const CHEAP_LO: f64 = 0.10;
/// Cheap band, EXCLUSIVE upper bound (`strategy.py:28` `CHEAP_HI`).
pub const CHEAP_HI: f64 = 0.35;
/// Cheap time gate: minimum time-till-expiry in seconds (`strategy.py:38` `CHEAP_TTE_LO`).
pub const CHEAP_TTE_LO: f64 = 15.0;
/// Cheap time gate: maximum time-till-expiry in seconds (`strategy.py:38` `CHEAP_TTE_HI`).
pub const CHEAP_TTE_HI: f64 = 270.0;
/// σ estimation window in seconds — 30 min, fixed to match the backtest (`strategy.py:17`).
pub const SIGMA_LOOKBACK_S: f64 = 1800.0;
/// Warmup floor: distinct OBSERVED seconds needed before σ is valid (`strategy.py:18`).
pub const SIGMA_MIN_SECONDS: usize = 30;

/// Polymarket's probability-scaled taker fee at price `p`. Port of `strategy.py:158`.
#[inline]
pub fn fee(p: f64) -> f64 {
    FEE_RATE * p * (1.0 - p)
}

/// Probability the window closes UP — hoisted to [`vike_model::p_up`] so the live
/// Avellaneda–Stoikov maker calls the SAME definition; the cheap_np parity gate pins its bits
/// through this re-export. Port of `strategy.py:162` (`p_up_scalar`).
pub use vike_model::p_up;

/// Dual-β **worst-case** (minimum) edge for outcome `oidx` (0 = Up, 1 = Down) bought at `ask`.
/// Port of `strategy.py:168` (`wc_scalar`).
///
/// The fold mirrors Python's `min()`: the running best is replaced only on a strict `<`, so a NaN
/// candidate never displaces a real one (nor is a NaN first element displaced). Only degenerate
/// inputs (σ = 0 with `s_now == s_open`) reach it.
pub fn wc(oidx: u8, ask: f64, s_now: f64, s_open: f64, sigma: f64, t: f64, h: f64) -> f64 {
    wc_betas(oidx, ask, s_now, s_open, sigma, t, h, &BETAS)
}

/// [`wc`] over an explicit β set — the seam the unit tests pin a single-β value through.
pub fn wc_betas(
    oidx: u8,
    ask: f64,
    s_now: f64,
    s_open: f64,
    sigma: f64,
    t: f64,
    h: f64,
    betas: &[f64],
) -> f64 {
    let f = fee(ask);
    let mut out: Option<f64> = None;
    for &b in betas {
        let pu = p_up(b, s_now, s_open, sigma, t, h);
        let prob = if oidx == 0 { pu } else { 1.0 - pu };
        let e = prob - ask - f;
        out = Some(match out {
            None => e,
            Some(cur) if e < cur => e,
            Some(cur) => cur,
        });
    }
    out.unwrap_or(f64::NAN)
}

/// Is `ask` inside the cheap band `[CHEAP_LO, CHEAP_HI)`? Port of the `leg_of` cheap arm
/// (`strategy.py:180`) — half-open, so 0.35 is NOT cheap.
#[inline]
pub fn in_cheap_band(ask: f64) -> bool {
    (CHEAP_LO..CHEAP_HI).contains(&ask)
}

/// May the cheap leg trade at `t` seconds into a window of length `h`? Port of `strategy.py:188`
/// (`cheap_time_ok`): time-till-expiry `h − t` must lie in `[CHEAP_TTE_LO, CHEAP_TTE_HI]`
/// (both bounds INCLUSIVE) — i.e. `t ∈ [30, 285]` for the 300 s window.
#[inline]
pub fn cheap_time_ok(t: f64, h: f64) -> bool {
    let tte = h - t;
    (CHEAP_TTE_LO..=CHEAP_TTE_HI).contains(&tte)
}

/// The whole `cheap_np` entry gate for ONE taker-BUY print, as one decision.
///
/// `Some(edge)` when the print is in the cheap band, inside the cheap time window, and its dual-β
/// worst-case edge STRICTLY exceeds `theta`; `None` otherwise. The ONE predicate both
/// [`crate::strategies::cheap_np::CheapNp`] and the parity harness call, so the harness's
/// 12,639-entry proof is a proof of the strategy's own gate, not of a re-implementation.
pub fn cheap_gate(
    oidx: u8,
    ask: f64,
    s_now: f64,
    s_open: f64,
    sigma: f64,
    t: f64,
    theta: f64,
    h: f64,
) -> Option<f64> {
    if !in_cheap_band(ask) || !cheap_time_ok(t, h) {
        return None;
    }
    let e = wc(oidx, ask, s_now, s_open, sigma, t, h);
    (e > theta).then_some(e)
}

/// Most recent price with `ts <= target_ts`, or `None` if no sample is old enough. Port of
/// `strategy.py:239` (`price_at`) — including its tie rule: the scan keeps a candidate only on a
/// STRICTLY greater `ts`, so among several samples stamped the same second the FIRST in iteration
/// order wins.
pub fn price_at<I: IntoIterator<Item = (f64, f64)>>(samples: I, target_ts: f64) -> Option<f64> {
    let mut best: Option<(f64, f64)> = None;
    for (ts, px) in samples {
        if ts <= target_ts && best.is_none_or(|(bt, _)| ts > bt) {
            best = Some((ts, px));
        }
    }
    best.map(|(_, px)| px)
}

/// Sample stddev of 1-second log-returns over the last `lookback_s` seconds ending at `now`.
/// Port of `strategy.py:202` (`trailing_sigma`).
///
/// `samples` is an iterable of `(ts_seconds, price)`. The algorithm, verbatim:
///
/// 1. Keep samples with `ts >= now − lookback_s` and `price > 0`, bucketed by INTEGER second,
///    **last write wins**.
/// 2. Fewer than [`SIGMA_MIN_SECONDS`] distinct observed seconds → `None` (warmup).
/// 3. Build a CONTIGUOUS 1-second grid first→last observed second, LINEARLY INTERPOLATING every
///    missing second, so a feed stall is a smooth ramp, not one spurious giant return (the
///    backtest's `WITH FILL STEP 1 INTERPOLATE (px)`).
/// 4. Sample (n−1) stddev of the grid's consecutive log-returns; `None` if fewer than 2.
///
/// Both folds use [`py_sum`] (the oracle's CPython `sum()` is Neumaier-compensated).
pub fn trailing_sigma<I: IntoIterator<Item = (f64, f64)>>(
    samples: I,
    lookback_s: f64,
    now: f64,
) -> Option<f64> {
    let cutoff = now - lookback_s;
    // BTreeMap == Python's `dict` + `sorted(by_sec)`: insertion overwrites (last write wins) and
    // iteration is by ascending second.
    let mut by_sec: BTreeMap<i64, f64> = BTreeMap::new();
    for (ts, px) in samples {
        if ts >= cutoff && px > 0.0 {
            by_sec.insert(ts as i64, px); // `as i64` truncates toward zero, like Python's int()
        }
    }
    if by_sec.len() < SIGMA_MIN_SECONDS {
        return None;
    }
    let secs: Vec<(i64, f64)> = by_sec.into_iter().collect();
    let mut prices: Vec<f64> = Vec::with_capacity(secs.len());
    for w in secs.windows(2) {
        let (a, pa) = w[0];
        let (b, pb) = w[1];
        prices.push(pa);
        let gap = b - a;
        if gap > 1 {
            for k in 1..gap {
                prices.push(pa + (pb - pa) * k as f64 / gap as f64);
            }
        }
    }
    prices.push(secs[secs.len() - 1].1);

    // ⚠ `libm::log`, not `.ln()` (`docs/decisions/0032`): the platform `ln` may differ between
    // the Windows dev box and Linux CI in the last bit, and σ is `p_up`'s denominator, so an error
    // is AMPLIFIED. The six `sigma_bits` pinned EXACTLY by
    // `crates/vike-strategy/tests/cheap_np_parity.rs`'s
    // `trailing_sigma_matches_the_python_oracle_bit_for_bit` did not move and are the originals —
    // now a self-consistency pin, whatever the name says. Never widen it (`docs/decisions/0021`).
    let rets: Vec<f64> = prices.windows(2).map(|w| libm::log(w[1] / w[0])).collect();
    let n = rets.len();
    if n < 2 {
        return None;
    }
    let m = py_sum(rets.iter().copied()) / n as f64;
    let var = py_sum(rets.iter().map(|r| (r - m) * (r - m))) / (n - 1) as f64;
    Some(var.sqrt())
}

#[path = "fair_value_tests.rs"]
#[cfg(test)]
mod fair_value_tests;
