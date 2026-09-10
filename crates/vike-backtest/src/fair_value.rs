//! Fair-value 5-minute up/down primitives — PURE (no I/O, no engine types, no broker).
//!
//! Exact port of the live Polymarket bot's pure core,
//! `vike_db_data_jobs/trading/fair_value_bot/strategy.py` (the oracle; read-only), restricted to
//! what the **`cheap_np`** leg needs: `fee`, `p_up_scalar`, `wc_scalar`, `cheap_time_ok`,
//! `trailing_sigma`, `price_at`, and the gate constants. The `fav_1.1` / `penny` legs, the leg-band
//! router (`leg_of`), the delayed-fill helpers and the ClickHouse DDL are deliberately NOT ported —
//! they are live-recording plumbing, not strategy math.
//!
//! The contract this module holds:
//!
//! * **`fee(p) = 0.072·p·(1−p)`** — Polymarket's probability-scaled taker fee. Structurally the
//!   same shape as [`vike_model::FeeSchedule::ProbabilityScaled`], but that lane is priced by the
//!   engine; here it is the strategy's own EDGE accounting, so it stays a plain scalar.
//! * **`p_up`** — the Bachelier-style up-probability of a lognormal spot over the remaining window,
//!   `0.5·(1 + erf(x/√2))` with `x = ln(s_now/s_open)·β / (σ·√max(H−t, 1e-9))`. `erf` is
//!   `libm::erf`, called inside `vike_model::p_up` (which `p_up` below re-exports). The
//!   `vike-options` port already pins that as bit-identical to CPython's `math.erf` through this
//!   whole stack (`crates/vike-options/src/greeks.rs`'s `norm_cdf`).
//!
//!   ⚠ This bullet used to end "— this crate no longer depends on `libm` directly", and that
//!   stopped being true on 2026-08-26: `trailing_sigma` below now calls `libm::log`, so the
//!   manifest carries the dependency again. The parenthetical is kept as a correction rather than
//!   deleted, because it is the kind of claim that reads as settled and quietly rots.
//!
//!   ⚠ And the sibling half was WRONG the whole time, which is the more useful correction:
//!   `vike_model::p_up` called `libm::erf` on its last line and the PLATFORM's `.ln()` one line
//!   above it. Half a function converted reads as a converted function — `docs/decisions/0032`'s
//!   own site audit counted the `erf` here and missed the `ln`. It is `libm::log` now.
//! * **`wc`** — the DUAL-β **worst case**: the MINIMUM over `BETAS = (0.83, 1.36)` of
//!   `prob − ask − fee(ask)`, where `prob` is `p_up` for outcome 0 (Up) and `1 − p_up` for outcome 1
//!   (Down). Minimum, not mean: the two βs bracket the drift-sensitivity uncertainty and the gate
//!   only fires when BOTH agree the edge is there.
//! * **`trailing_sigma`** — the sample stddev of **1-second** log-returns over the last
//!   [`SIGMA_LOOKBACK_S`] seconds, computed on a CONTIGUOUS 1-second grid with **linear
//!   interpolation of missing seconds**. This is load-bearing, not incidental: the underlying
//!   `spot_1s` series is missing ~11% of its seconds, and the backtest's SQL closes them with
//!   `WITH FILL STEP 1 INTERPOLATE (px)`. σ computed on the raw sparse series does NOT match. The
//!   estimator returns `None` until [`SIGMA_MIN_SECONDS`] distinct seconds have actually been
//!   observed (a warmup floor, counted on OBSERVED seconds — not on the interpolated grid).
//!
//! Summation note: the Python uses builtin `sum()` for both the mean and the variance fold, and
//! CPython ≥ 3.12's `sum()` is Neumaier-compensated — so both folds go through
//! [`vike_model::py_sum`], per the repo's parity rule. The `min` over βs mirrors Python's `min()`
//! (keep-first-on-not-less) rather than [`f64::min`], so NaN handling matches the oracle exactly.

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

/// Probability the window closes UP — the shared fair-value primitive, hoisted to
/// [`vike_model::p_up`] so the live Avellaneda–Stoikov maker calls the SAME definition (no
/// duplication). The cheap_np parity gate still pins its bits through this re-export. Port of
/// `strategy.py:162` (`p_up_scalar`).
pub use vike_model::p_up;

/// Dual-β **worst-case** (minimum) edge for outcome `oidx` (0 = Up, 1 = Down) bought at `ask`.
/// Port of `strategy.py:168` (`wc_scalar`).
///
/// The fold mirrors Python's `min()` — the running best is replaced only on a strict `<`, so a NaN
/// candidate never displaces a real one (and a NaN first element is never displaced). That matters
/// only in degenerate inputs (σ = 0 with `s_now == s_open`), which the reference set does not
/// contain, but it keeps the port honest.
pub fn wc(oidx: u8, ask: f64, s_now: f64, s_open: f64, sigma: f64, t: f64, h: f64) -> f64 {
    wc_betas(oidx, ask, s_now, s_open, sigma, t, h, &BETAS)
}

/// [`wc`] over an explicit β set — the seam the unit tests pin a single-β value through.
// The argument list is the ORACLE's (`wc_scalar(oidx, ask, s_now, s_open, sigma, t, h, betas)`).
// Bundling it into a struct would break the 1:1 reading against `strategy.py` that makes this port
// checkable, for no call-site benefit — every caller passes all eight.
#[allow(clippy::too_many_arguments)]
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
/// Returns `Some(edge)` when the print is in the cheap band, inside the cheap time window, and its
/// dual-β worst-case edge STRICTLY exceeds `theta`; `None` otherwise. This is the single predicate
/// both the [`crate::cheap_np::CheapNp`] strategy and the parity harness go through, so the
/// harness's 12,639-entry proof is a proof of the strategy's own gate — not of a re-implementation.
// Same rationale as [`wc_betas`]: this is the oracle's argument list plus the two knobs (`theta`,
// `h`) the harness and the strategy both need to vary.
#[allow(clippy::too_many_arguments)]
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
/// 1. Keep only samples with `ts >= now − lookback_s` and `price > 0`, bucketed by their INTEGER
///    second, **last write wins** (so several prints inside one second collapse to the last one).
/// 2. If fewer than [`SIGMA_MIN_SECONDS`] distinct seconds were observed, return `None` (warmup).
/// 3. Build a CONTIGUOUS 1-second price grid spanning the first→last observed second, LINEARLY
///    INTERPOLATING every missing second between its two bracketing observations. A feed stall then
///    produces a smooth ramp instead of one spurious giant return — this is what makes the estimator
///    match the backtest's `WITH FILL STEP 1 INTERPOLATE (px)`.
/// 4. Sample (n−1) stddev of the consecutive log-returns of that grid; `None` if fewer than 2.
///
/// Both folds use [`py_sum`] because the oracle uses CPython's Neumaier-compensated `sum()`.
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

    // ⚠ `libm::log`, not `.ln()` — and this conversion was held as a STOP for a day before it was
    // measured, which is the part worth recording.
    //
    // The reasoning that stopped it: `crates/vike-backtest/tests/cheap_np_parity.rs`'s
    // `trailing_sigma_matches_the_python_oracle_bit_for_bit` compares `got.to_bits() ==
    // want.to_bits()` against SIX committed `sigma_bits` constants, and it is neither `#[ignore]`d
    // nor cfg-gated. σ is also the denominator of `p_up`'s `denom`, so an error here is AMPLIFIED
    // by `1/denom` rather than damped, and it accumulates across every element of `rets` before
    // `var.sqrt()`. Converting looked like it would move a frozen comparison.
    //
    // ⚠ **It moved nothing.** All six pinned values are bit-identical under `libm::log` — that
    // function and the platform's `ln` agree exactly on every input this fixture drives. The STOP
    // was hypothetical; one run settled it, and reading the code could not have.
    //
    // On the record that governs this: `docs/decisions/0021` retires the Python oracle as a SOURCE
    // but keeps every frozen fixture's comparison EXACT, on the ground that "a frozen fixture is
    // not a claim about Python; it is a claim about *this* code not changing its arithmetic
    // unnoticed". Re-recording a constant because the code deliberately changed is not the
    // widening that record forbids — but here not even that was needed, so the six constants are
    // the originals, untouched.
    //
    // ⚠ What DID change is what the test's NAME means. It no longer compares this port against
    // CPython through a shared platform libm — after this line, both sides of that historical
    // comparison would compute `log` differently, and the constants agree only because they
    // happened to. It is a self-consistency pin now. The name still says "python_oracle" and is
    // stale in the same way ~43 other files in this tree are; renaming them is its own change.
    let rets: Vec<f64> = prices.windows(2).map(|w| libm::log(w[1] / w[0])).collect();
    let n = rets.len();
    if n < 2 {
        return None;
    }
    let m = py_sum(rets.iter().copied()) / n as f64;
    let var = py_sum(rets.iter().map(|r| (r - m) * (r - m))) / (n - 1) as f64;
    Some(var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_matches_the_probability_curve() {
        assert_eq!(fee(0.25), 0.072 * 0.25 * 0.75);
        // symmetric about 0.5 (to fp, not bitwise: `a*b` and `b*a` round the same but
        // `0.072*0.3*0.7` and `0.072*0.7*0.3` do not), and zero at the boundaries
        assert!((fee(0.3) - fee(0.7)).abs() <= f64::EPSILON * fee(0.3));
        assert_eq!(fee(0.0), 0.0);
        assert_eq!(fee(1.0), 0.0);
    }

    #[test]
    fn p_up_is_a_half_when_spot_has_not_moved() {
        assert_eq!(p_up(0.83, 100.0, 100.0, 1e-4, 100.0, H), 0.5);
    }

    #[test]
    fn p_up_rises_with_an_up_move_and_falls_with_a_down_move() {
        let up = p_up(0.83, 100.5, 100.0, 1e-4, 100.0, H);
        let dn = p_up(0.83, 99.5, 100.0, 1e-4, 100.0, H);
        assert!(up > 0.5, "{up}");
        assert!(dn < 0.5, "{dn}");
        // The model is symmetric in LOG space (the mirror of s·k is s/k, not 2s − s·k), so pin the
        // symmetry the way the formula actually has it: p_up(s·k) + p_up(s/k) == 1.
        let k = 1.005_f64;
        let a = p_up(0.83, 100.0 * k, 100.0, 1e-4, 100.0, H);
        let b = p_up(0.83, 100.0 / k, 100.0, 1e-4, 100.0, H);
        assert!((a + b - 1.0).abs() < 1e-12, "{a} {b}");
    }

    #[test]
    fn wc_is_the_min_over_betas_not_the_mean() {
        let (s_now, s_open, sigma, t) = (100.3, 100.0, 1e-4, 100.0);
        let a = wc_betas(0, 0.2, s_now, s_open, sigma, t, H, &[BETAS[0]]);
        let b = wc_betas(0, 0.2, s_now, s_open, sigma, t, H, &[BETAS[1]]);
        let both = wc(0, 0.2, s_now, s_open, sigma, t, H);
        assert_eq!(both, a.min(b));
        assert_ne!(both, 0.5 * (a + b));
    }

    #[test]
    fn wc_down_side_uses_one_minus_p_up() {
        let (ask, s_now, s_open, sigma, t) = (0.2, 100.3, 100.0, 1e-4, 100.0);
        let up = wc_betas(0, ask, s_now, s_open, sigma, t, H, &[0.83]);
        let dn = wc_betas(1, ask, s_now, s_open, sigma, t, H, &[0.83]);
        // (p) − ask − f  and  (1−p) − ask − f  differ by exactly (2p − 1)
        let p = p_up(0.83, s_now, s_open, sigma, t, H);
        assert!(((up - dn) - (2.0 * p - 1.0)).abs() < 1e-15);
    }

    #[test]
    fn cheap_band_is_half_open() {
        assert!(in_cheap_band(0.10));
        assert!(in_cheap_band(0.3499999));
        assert!(!in_cheap_band(0.35));
        assert!(!in_cheap_band(0.0999999));
    }

    #[test]
    fn cheap_time_window_is_inclusive_on_both_tte_bounds() {
        // tte = 300 − t, allowed [15, 270]  <=>  t in [30, 285]
        assert!(!cheap_time_ok(29.0, H));
        assert!(cheap_time_ok(30.0, H));
        assert!(cheap_time_ok(285.0, H));
        assert!(!cheap_time_ok(286.0, H));
    }

    #[test]
    fn cheap_gate_rejects_out_of_band_out_of_time_and_thin_edge() {
        let (s_now, s_open, sigma, t) = (100.6, 100.0, 1e-4, 100.0);
        // in band + in time + fat edge -> fires
        assert!(cheap_gate(0, 0.20, s_now, s_open, sigma, t, THETA, H).is_some());
        // same print, out of band
        assert!(cheap_gate(0, 0.40, s_now, s_open, sigma, t, THETA, H).is_none());
        // same print, outside the time window (t = 10 -> tte 290 > 270)
        assert!(cheap_gate(0, 0.20, s_now, s_open, sigma, 10.0, THETA, H).is_none());
        // same print, wrong side (Down, after an up move) -> negative edge
        assert!(cheap_gate(1, 0.20, s_now, s_open, sigma, t, THETA, H).is_none());
    }

    #[test]
    fn cheap_gate_is_strictly_greater_than_theta() {
        // Construct a print whose edge is EXACTLY reproducible, then gate at that exact value.
        let (ask, s_now, s_open, sigma, t) = (0.20, 100.6, 100.0, 1e-4, 100.0);
        let e = wc(0, ask, s_now, s_open, sigma, t, H);
        assert!(cheap_gate(0, ask, s_now, s_open, sigma, t, e, H).is_none(), "equal must not fire");
        let just_under = f64::from_bits(e.to_bits() - 1);
        assert_eq!(cheap_gate(0, ask, s_now, s_open, sigma, t, just_under, H), Some(e));
    }

    fn ramp(n: usize) -> Vec<(f64, f64)> {
        // a deterministic non-degenerate series (a pure geometric ramp has zero return variance,
        // so alternate the step to give the estimator something to measure)
        (0..n)
            .map(|i| {
                let px = 100.0 * (1.0 + 0.001 * ((i % 3) as f64) + 0.0001 * i as f64);
                (1_000_000.0 + i as f64, px)
            })
            .collect()
    }

    #[test]
    fn trailing_sigma_is_none_below_the_warmup_floor() {
        let s = ramp(SIGMA_MIN_SECONDS - 1);
        assert_eq!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0), None);
        let s = ramp(SIGMA_MIN_SECONDS);
        assert!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0).is_some());
    }

    #[test]
    fn trailing_sigma_counts_observed_seconds_not_interpolated_ones() {
        // 29 observed seconds spread over 60 real seconds: the interpolated grid would be 60 long,
        // but the warmup floor counts OBSERVED seconds -> still None.
        let s: Vec<(f64, f64)> = ramp(60).into_iter().step_by(2).take(29).collect();
        assert_eq!(s.len(), 29);
        assert_eq!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0), None);
    }

    #[test]
    fn trailing_sigma_drops_samples_older_than_the_cutoff() {
        let all = ramp(120);
        let now = 1_000_119.0;
        // lookback 40 keeps ts >= now-40 = 1_000_079 -> 41 samples
        let windowed: Vec<(f64, f64)> =
            all.iter().copied().filter(|(ts, _)| *ts >= now - 40.0).collect();
        assert_eq!(windowed.len(), 41);
        assert_eq!(trailing_sigma(all, 40.0, now), trailing_sigma(windowed, 40.0, now));
    }

    #[test]
    fn trailing_sigma_last_write_wins_inside_one_second() {
        let base = ramp(40);
        // two prints per second; the LAST one must be the sample kept
        let mut dup: Vec<(f64, f64)> = Vec::new();
        for &(ts, px) in &base {
            dup.push((ts + 0.1, px * 1.5));
            dup.push((ts + 0.9, px));
        }
        assert_eq!(
            trailing_sigma(dup, SIGMA_LOOKBACK_S, 1_000_100.0),
            trailing_sigma(base, SIGMA_LOOKBACK_S, 1_000_100.0)
        );
    }

    #[test]
    fn trailing_sigma_interpolates_a_gap_into_a_ramp_not_a_jump() {
        // A dead-flat series with ONE step, seen either densely or with the step's interior
        // seconds missing. Interpolation must spread the step over the gap: the sparse series'
        // sigma equals that of the explicitly-interpolated dense series, and is far BELOW what a
        // single spurious jump return would produce.
        let mut dense: Vec<(f64, f64)> = Vec::new();
        for i in 0..40u32 {
            dense.push((1_000_000.0 + i as f64, 100.0));
        }
        for i in 40..50u32 {
            // linear ramp 100 -> 110 over 10 seconds
            dense.push((1_000_000.0 + i as f64, 100.0 + (i - 39) as f64));
        }
        let sparse: Vec<(f64, f64)> = dense
            .iter()
            .copied()
            .filter(|(ts, _)| !(1_000_040.0..1_000_049.0).contains(ts))
            .collect();
        let a = trailing_sigma(dense.clone(), SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
        let b = trailing_sigma(sparse, SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
        assert!((a - b).abs() < 1e-15, "{a} vs {b}");

        // ...and the un-interpolated (raw sparse-grid) estimator would be much larger: prove the
        // interpolation actually suppressed a jump by comparing against a same-length series where
        // the whole step lands on ONE return.
        let mut jump: Vec<(f64, f64)> = Vec::new();
        for i in 0..49u32 {
            jump.push((1_000_000.0 + i as f64, if i < 40 { 100.0 } else { 110.0 }));
        }
        // The whole 10 % step lands on ONE return instead of ten, so the jump's stddev is
        // ~sqrt(10)x the ramp's — the concrete damping the INTERPOLATE clause buys.
        let c = trailing_sigma(jump, SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
        assert!(c > 3.0 * a, "interpolation must damp the stall: ramp {a} vs jump {c}");
    }

    #[test]
    fn price_at_returns_the_most_recent_not_the_nearest() {
        let s = vec![(10.0, 1.0), (20.0, 2.0), (30.0, 3.0)];
        assert_eq!(price_at(s.clone(), 25.0), Some(2.0));
        assert_eq!(price_at(s.clone(), 30.0), Some(3.0));
        assert_eq!(price_at(s.clone(), 9.9), None);
        // duplicate stamps: the FIRST in iteration order wins (strict `>` in the scan)
        let d = vec![(10.0, 1.0), (10.0, 9.0)];
        assert_eq!(price_at(d, 15.0), Some(1.0));
    }
}
