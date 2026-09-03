//! Realized-volatility estimators + IV↔RV comparison helpers — NEW code, no Python twin
//! (the oracle `data/options/` has no realized-vol module), so no parity fixture governs it.
//! No internal-crate dependency (in particular no vike-indicators): two small streaming structs
//! with `update(price)` / `value()`, an annualization pair, and the IV−RV spread.
//!
//! ⚠ That line used to read "**Std-only**", and the 2026-08-26 `libm` conversion below made it
//! false — the folds now call `libm::log`/`libm::exp` rather than `f64`'s methods. The claim it
//! was making is still true and is what the replacement says: this module pulls in no OTHER crate
//! of this workspace. `libm` was already a dependency of `vike-options` (`greeks.rs`'s
//! `norm_cdf` needs `erf`, which `std` does not have at all), so nothing was added to reach it.
//!
//! Conventions:
//! - Returns are LOG returns `ln(p_i / p_{i-1})` folded from consecutive `update` calls, and the
//!   logarithm is **`libm::log` — the `libm` CRATE, never `f64::ln`** (see the cross-platform
//!   section below).
//! - `value()` is the PER-PERIOD vol (stdev of the per-update log returns); annualize with
//!   [`annualize`] and the sampling frequency, e.g. `annualize(v, DAYS_PER_YEAR)` for daily
//!   closes. The year basis is **365 calendar days** ([`DAYS_PER_YEAR`]), matching
//!   `greeks.rs`' 365-day theta/expiry basis (crypto trades every day) — an equities caller
//!   can pass 252.0 explicitly instead.
//! - Non-finite or non-positive prices are IGNORED (no state change): a log return does not
//!   exist for them, and a bad tick must not poison the fold.
//! - Plain naive f64 folds (this is not a ported parity site; no `py_sum` needed).
//!
//! ## ATM IV selection (for the IV−RV spread)
//!
//! [`atm_iv`] picks the at-the-money implied vol from an [`OptionChain`] as the IV at the
//! strike **nearest the forward** `F = S·e^{rT}` (nearest-to-spot when `r = 0` or `T = 0`),
//! averaging call and put IV when both are present (they should coincide by put-call parity;
//! averaging cancels one-sided quote noise). Rows carrying no IV on either side are skipped;
//! an exact-distance tie resolves to the LOWER strike (rows are ascending). This is the
//! standard "nearest-to-forward" ATM convention; smile interpolation (variance-linear in
//! strike between the two bracketing strikes) is a deliberate follow-up, not implemented.
//!
//! ## Every transcendental here comes from the `libm` CRATE (converted 2026-08-26)
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! ratified rule and carries the measurement. The short form: IEEE 754 requires `+ - * /` and
//! `sqrt` to be correctly rounded and requires NOTHING of `ln`/`exp`, so `f64::ln` and `f64::exp`
//! reach whichever libm the platform ships and MSVC's CRT and glibc are each entitled to a
//! different last bit. Three sites here were platform calls and now are not: the log return in
//! [`EwmaVol::update`] and in [`RollingVol::update`], and the forward's `e^{rT}` in [`atm_iv`].
//!
//! ⚠ **`sqrt` is deliberately NOT converted** — [`annualize`], [`deannualize`] and
//! [`EwmaVol::value`]/[`RollingVol::value`] keep `f64::sqrt`. IEEE 754 already requires it
//! correctly rounded, so it is portable on its own; routing it through `libm` would buy nothing
//! and would move values for no reason.
//!
//! ⚠ **The hand-unrolled expectations in this file's `mod tests` had to convert in the SAME edit,
//! and the finding that proposed this change claimed there was "nothing to re-baseline" here.
//! That claim was FALSE and is corrected rather than deleted.** Three tests rebuild the fold by
//! hand and compare through the local `assert_bits` — EXACT bits, not a tolerance —
//! `ewma_matches_hand_unrolled_recursion`, `ewma_ignores_bad_ticks` and
//! `rolling_matches_hand_computed_window`. Converting only the production sites would have left
//! them comparing a `libm::log` fold against an `f64::ln` unroll, which asserts that the running
//! box's CRT agrees with the portable implementation: a test of the platform, not of the
//! recursion, red on whichever box disagrees first. Nothing was RE-RECORDED, because every
//! expectation in this file is COMPUTED from the same primitives rather than pasted as a literal —
//! there is no digit here for a divergence to be frozen into.
//!
//! ⚠ **Materiality is low, and the record says so rather than implying urgency.** Nothing outside
//! this file calls [`EwmaVol`], [`RollingVol`], [`atm_iv`], [`iv_rv_spread`] or [`deannualize`] —
//! `git grep -n 'EwmaVol\|RollingVol\|atm_iv\|iv_rv_spread\|deannualize' -- crates/` answers with
//! this module, the `lib.rs` re-export and two doc references in `basket.rs`, and nothing else.
//! This is a pure core awaiting a consumer, so the conversion moves no stored number, no backtest
//! and no live quote today. It is done anyway, and done BEFORE the first consumer arrives, because
//! the cheap moment to make a value portable is while nothing has pinned it yet.

use std::collections::VecDeque;

use crate::model::OptionChain;

/// Calendar-day annualization basis — matches the 365-day year of `greeks.rs`.
pub const DAYS_PER_YEAR: f64 = 365.0;

/// The RiskMetrics EWMA decay default (daily data).
pub const RISKMETRICS_LAMBDA: f64 = 0.94;

/// Per-period vol → annual vol: `vol · √periods_per_year`.
pub fn annualize(vol_per_period: f64, periods_per_year: f64) -> f64 {
    vol_per_period * periods_per_year.sqrt()
}

/// Annual vol → per-period vol: the exact inverse of [`annualize`].
pub fn deannualize(vol_annual: f64, periods_per_year: f64) -> f64 {
    vol_annual / periods_per_year.sqrt()
}

/// IV−RV spread: positive when implied trades RICH to realized (the classic vol-premium
/// read), negative when implied is cheap. Both inputs must already be annualized on the
/// same basis (see [`annualize`]); pure difference, no scaling.
pub fn iv_rv_spread(iv_annual: f64, rv_annual: f64) -> f64 {
    iv_annual - rv_annual
}

/// Streaming EWMA variance of log returns — the RiskMetrics recursion
/// `v' = λ·v + (1-λ)·r²` (zero-mean), default `λ = 0.94` for daily data.
///
/// Seeding: the FIRST log return seeds the variance at `r₁²` (the common RiskMetrics
/// practice — a zero seed would understate vol for many periods); every later return folds
/// through the recursion. `value()` is `None` until one full return exists (i.e. until the
/// second accepted price).
#[derive(Debug, Clone)]
pub struct EwmaVol {
    lambda: f64,
    last_price: Option<f64>,
    variance: Option<f64>,
}

impl EwmaVol {
    /// A fold with decay `lambda` (must be in `(0, 1)`; panics otherwise — a config error,
    /// not bad data).
    pub fn new(lambda: f64) -> Self {
        assert!(lambda > 0.0 && lambda < 1.0, "EWMA lambda must be in (0, 1), got {lambda}");
        Self { lambda, last_price: None, variance: None }
    }

    /// The RiskMetrics daily default, `λ = 0.94` ([`RISKMETRICS_LAMBDA`]).
    pub fn riskmetrics() -> Self {
        Self::new(RISKMETRICS_LAMBDA)
    }

    /// Fold one price observation. Non-finite / non-positive prices are ignored.
    pub fn update(&mut self, price: f64) {
        if price <= 0.0 || !price.is_finite() {
            return;
        }
        if let Some(prev) = self.last_price {
            // `libm::log`, never `f64::ln` (ADR 0032). `price / prev` is a correctly-rounded
            // divide and is already the same f64 on every box; the logarithm is the one step
            // where the platform's CRT is free to choose its own last bit, and this fold is
            // recursive — a single differing bit persists through every later `update`.
            let r = libm::log(price / prev);
            let r2 = r * r;
            self.variance = Some(match self.variance {
                None => r2, // seed at r₁²
                Some(v) => self.lambda * v + (1.0 - self.lambda) * r2,
            });
        }
        self.last_price = Some(price);
    }

    /// Current per-period variance, `None` until the first return.
    pub fn variance(&self) -> Option<f64> {
        self.variance
    }

    /// Current per-period vol (`√variance`), `None` until the first return.
    pub fn value(&self) -> Option<f64> {
        self.variance.map(f64::sqrt)
    }

    /// Convenience: `annualize(value(), periods_per_year)`.
    pub fn annualized(&self, periods_per_year: f64) -> Option<f64> {
        self.value().map(|v| annualize(v, periods_per_year))
    }
}

impl Default for EwmaVol {
    fn default() -> Self {
        Self::riskmetrics()
    }
}

/// Plain rolling close-to-close estimator: SAMPLE standard deviation (`n-1` denominator,
/// demeaned) of the last `window` log returns.
///
/// `value()` is `None` until the window is FULL — a partially-filled window would silently
/// change the estimator's variance. `window` must be ≥ 2 (a sample stdev of one return is
/// undefined; panics — a config error).
#[derive(Debug, Clone)]
pub struct RollingVol {
    window: usize,
    returns: VecDeque<f64>,
    last_price: Option<f64>,
}

impl RollingVol {
    /// A rolling window over the last `window` log returns (panics if `window < 2`).
    pub fn new(window: usize) -> Self {
        assert!(window >= 2, "RollingVol window must be >= 2, got {window}");
        Self { window, returns: VecDeque::with_capacity(window), last_price: None }
    }

    /// Fold one price observation. Non-finite / non-positive prices are ignored.
    pub fn update(&mut self, price: f64) {
        if price <= 0.0 || !price.is_finite() {
            return;
        }
        if let Some(prev) = self.last_price {
            if self.returns.len() == self.window {
                self.returns.pop_front();
            }
            // `libm::log`, never `f64::ln` (ADR 0032) — the same reason as `EwmaVol::update`, and
            // the stored return is what the whole window's mean and sum-of-squares are built from.
            self.returns.push_back(libm::log(price / prev));
        }
        self.last_price = Some(price);
    }

    /// Per-period sample stdev of the windowed log returns, `None` until the window is full.
    pub fn value(&self) -> Option<f64> {
        if self.returns.len() < self.window {
            return None;
        }
        let n = self.returns.len() as f64;
        let mean = self.returns.iter().sum::<f64>() / n;
        let ss = self.returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>();
        Some((ss / (n - 1.0)).sqrt())
    }

    /// Convenience: `annualize(value(), periods_per_year)`.
    pub fn annualized(&self, periods_per_year: f64) -> Option<f64> {
        self.value().map(|v| annualize(v, periods_per_year))
    }
}

/// ATM implied vol from a chain snapshot: the IV at the strike nearest the forward
/// `F = S·e^{r·t}` — see the module doc for the exact selection contract. `None` when the
/// chain has no spot, `s <= 0`, `t < 0`, or no row carries an IV on either side.
pub fn atm_iv(chain: &OptionChain, t: f64, r: f64) -> Option<f64> {
    let s = chain.underlying_price?;
    if s <= 0.0 || t < 0.0 {
        return None;
    }
    // `libm::exp`, never `f64::exp` (ADR 0032). The forward only SELECTS a strike here, so a
    // last-bit difference is invisible unless it lands on an exact distance tie — but "invisible
    // unless" is precisely the class of divergence that surfaces once, in production, on the one
    // chain whose strikes straddle the forward evenly.
    let forward = s * libm::exp(r * t);
    let row = chain
        .rows
        .iter()
        .filter(|row| {
            row.call.as_ref().and_then(|q| q.iv).is_some()
                || row.put.as_ref().and_then(|q| q.iv).is_some()
        })
        .min_by(|a, b| (a.strike - forward).abs().total_cmp(&(b.strike - forward).abs()))?;
    match (row.call.as_ref().and_then(|q| q.iv), row.put.as_ref().and_then(|q| q.iv)) {
        (Some(c), Some(p)) => Some(0.5 * (c + p)),
        (Some(c), None) => Some(c),
        (None, Some(p)) => Some(p),
        (None, None) => None, // unreachable: the filter kept only IV-bearing rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssetClass, Expiry, OptionKind, OptionQuote, StrikeRow};

    fn assert_bits(what: &str, got: Option<f64>, expect: f64) {
        let got = got.unwrap_or_else(|| panic!("{what}: expected Some({expect}), got None"));
        assert_eq!(got.to_bits(), expect.to_bits(), "{what}: got {got:e}, expected {expect:e}");
    }

    #[test]
    fn ewma_matches_hand_unrolled_recursion() {
        // Prices 100 → 101 → 99 → 102 at λ = 0.94, unrolled by hand.
        // ⚠ The unroll spells `libm::log`, NOT `f64::ln`, and that is load-bearing rather than
        // stylistic: `assert_bits` below compares EXACT bits, so an `f64::ln` unroll would be
        // asserting that this box's platform CRT agrees with the portable implementation the fold
        // now uses. That is a test of the CRT (see the module doc's ADR-0032 section).
        let mut e = EwmaVol::riskmetrics();
        e.update(100.0);
        assert!(e.variance().is_none(), "no return yet");
        assert!(e.value().is_none());

        e.update(101.0);
        let r1 = libm::log(101.0f64 / 100.0);
        let mut v = r1 * r1; // seed
        assert_bits("seed variance", e.variance(), v);

        // NB: the recursion's weight is written `(1 - λ)` exactly as RiskMetrics states it —
        // in f64 `1.0 - 0.94` is NOT the literal `0.06` (2 ulp apart), so the unroll must
        // spell it the same way to land on the same bits.
        e.update(99.0);
        let r2 = libm::log(99.0f64 / 101.0);
        v = 0.94 * v + (1.0 - 0.94) * (r2 * r2);
        assert_bits("second fold", e.variance(), v);

        e.update(102.0);
        let r3 = libm::log(102.0f64 / 99.0);
        v = 0.94 * v + (1.0 - 0.94) * (r3 * r3);
        assert_bits("third fold", e.variance(), v);
        assert_bits("vol = sqrt(var)", e.value(), v.sqrt());
        assert_bits("annualized", e.annualized(DAYS_PER_YEAR), v.sqrt() * 365.0f64.sqrt());
    }

    #[test]
    fn ewma_ignores_bad_ticks() {
        let mut e = EwmaVol::riskmetrics();
        for p in [100.0, f64::NAN, 0.0, -5.0, f64::INFINITY] {
            e.update(p);
        }
        assert!(e.variance().is_none(), "bad ticks must not create a return");
        e.update(101.0);
        let r1 = libm::log(101.0f64 / 100.0); // return vs 100, not vs any rejected tick
        assert_bits("return spans the bad ticks", e.variance(), r1 * r1);
    }

    #[test]
    #[should_panic(expected = "lambda must be in (0, 1)")]
    fn ewma_rejects_bad_lambda() {
        let _ = EwmaVol::new(1.0);
    }

    #[test]
    fn rolling_matches_hand_computed_window() {
        // window = 3 over prices 100, 101, 99, 102, 103 → returns r1..r4; the live window
        // after the last update is [r2, r3, r4].
        let mut w = RollingVol::new(3);
        for p in [100.0, 101.0, 99.0, 102.0] {
            w.update(p);
        }
        // Only 3 returns exist after 4 prices — exactly full: [r1, r2, r3].
        let r1 = libm::log(101.0f64 / 100.0);
        let r2 = libm::log(99.0f64 / 101.0);
        let r3 = libm::log(102.0f64 / 99.0);
        let mean = (r1 + r2 + r3) / 3.0;
        let ss = (r1 - mean) * (r1 - mean) + (r2 - mean) * (r2 - mean) + (r3 - mean) * (r3 - mean);
        assert_bits("full window", w.value(), (ss / 2.0).sqrt());

        w.update(103.0); // r4 evicts r1
        let r4 = libm::log(103.0f64 / 102.0);
        let mean = (r2 + r3 + r4) / 3.0;
        let ss = (r2 - mean) * (r2 - mean) + (r3 - mean) * (r3 - mean) + (r4 - mean) * (r4 - mean);
        assert_bits("rolled window", w.value(), (ss / 2.0).sqrt());
    }

    #[test]
    fn rolling_none_until_full() {
        let mut w = RollingVol::new(3);
        w.update(100.0);
        assert!(w.value().is_none());
        w.update(101.0);
        assert!(w.value().is_none(), "1 return < window");
        w.update(102.0);
        assert!(w.value().is_none(), "2 returns < window");
        w.update(103.0);
        assert!(w.value().is_some(), "3 returns == window");
    }

    #[test]
    #[should_panic(expected = "window must be >= 2")]
    fn rolling_rejects_window_of_one() {
        let _ = RollingVol::new(1);
    }

    #[test]
    fn annualization_round_trip() {
        for basis in [365.0, 252.0, 365.0 * 24.0] {
            let v = 0.0123;
            let back = deannualize(annualize(v, basis), basis);
            let rel = ((back - v) / v).abs();
            assert!(rel < 1e-15, "basis {basis}: {back} vs {v} (rel {rel:e})");
        }
        // Known value on the crate basis: 1%/day → 19.105%/year (√365 ≈ 19.105).
        let ann = annualize(0.01, DAYS_PER_YEAR);
        assert!((ann - 0.191049731745428).abs() < 1e-15, "got {ann}");
    }

    #[test]
    fn spread_is_a_pure_difference() {
        assert_eq!(iv_rv_spread(0.62, 0.50).to_bits(), (0.62f64 - 0.50).to_bits());
        assert!(iv_rv_spread(0.40, 0.55) < 0.0);
    }

    // ---- atm_iv ----

    fn quote(strike: f64, kind: OptionKind, iv: Option<f64>) -> OptionQuote {
        OptionQuote { iv, ..OptionQuote::new(strike, kind) }
    }

    fn chain(spot: Option<f64>, rows: Vec<StrikeRow>) -> OptionChain {
        OptionChain {
            underlying: "BTC".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: spot,
            expiry: Expiry { date: "2026-08-28".into(), dte: 41, label: "28 Aug".into() },
            asof_ms: 0,
            source: "test".into(),
            rows,
        }
    }

    fn row(strike: f64, call_iv: Option<f64>, put_iv: Option<f64>) -> StrikeRow {
        StrikeRow {
            strike,
            call: Some(quote(strike, OptionKind::Call, call_iv)),
            put: Some(quote(strike, OptionKind::Put, put_iv)),
        }
    }

    #[test]
    fn atm_iv_picks_nearest_to_forward_and_averages() {
        let c = chain(
            Some(101.0),
            vec![
                row(90.0, Some(0.70), Some(0.72)),
                row(100.0, Some(0.60), Some(0.62)),
                row(110.0, Some(0.55), Some(0.57)),
            ],
        );
        // r = 0 → forward = spot = 101 → nearest strike 100 → mean(0.60, 0.62).
        assert_bits("mean of both sides", atm_iv(&c, 0.5, 0.0), 0.5 * (0.60 + 0.62));
        // Nonzero r pushes the forward up: F = 101·e^{0.18·0.5} ≈ 110.51 → nearest strike 110.
        assert_bits("forward-shifted pick", atm_iv(&c, 0.5, 0.18), 0.5 * (0.55 + 0.57));
    }

    #[test]
    fn atm_iv_one_sided_and_skips_ivless_rows() {
        let c = chain(
            Some(100.0),
            vec![
                row(100.0, None, None),
                row(105.0, Some(0.58), None),
                row(110.0, None, Some(0.54)),
            ],
        );
        // Strike 100 has no IV at all → skipped; nearest IV-bearing is 105 (call only).
        assert_bits("one-sided call", atm_iv(&c, 0.25, 0.0), 0.58);
    }

    #[test]
    fn atm_iv_edge_cases_are_none() {
        let no_spot = chain(None, vec![row(100.0, Some(0.6), Some(0.6))]);
        assert!(atm_iv(&no_spot, 0.5, 0.0).is_none());
        let no_iv = chain(Some(100.0), vec![row(100.0, None, None)]);
        assert!(atm_iv(&no_iv, 0.5, 0.0).is_none());
        let empty = chain(Some(100.0), vec![]);
        assert!(atm_iv(&empty, 0.5, 0.0).is_none());
        let bad_spot = chain(Some(0.0), vec![row(100.0, Some(0.6), None)]);
        assert!(atm_iv(&bad_spot, 0.5, 0.0).is_none());
        let neg_t = chain(Some(100.0), vec![row(100.0, Some(0.6), None)]);
        assert!(atm_iv(&neg_t, -0.1, 0.0).is_none());
    }

    #[test]
    fn atm_iv_tie_resolves_to_lower_strike() {
        // Spot 105 sits exactly between 100 and 110 → the LOWER strike wins (first in
        // ascending rows).
        let c =
            chain(Some(105.0), vec![row(100.0, Some(0.60), None), row(110.0, Some(0.50), None)]);
        assert_bits("tie → lower", atm_iv(&c, 0.5, 0.0), 0.60);
    }
}
