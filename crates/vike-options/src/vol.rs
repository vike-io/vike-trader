//! IV↔RV comparison helpers over an option chain — NEW code, no Python twin (the oracle
//! `data/options/` has no realized-vol module), so no parity fixture governs it: the
//! nearest-to-forward [`atm_iv`] read and the [`iv_rv_spread`] difference.
//!
//! ⚠ **The realized-volatility ESTIMATORS are not here.** The streaming EWMA and rolling
//! close-to-close folds, their annualization pair and the 365-day basis constant left this crate
//! for `vike-indicators`' realized-volatility indicator family, which is where every other fold
//! over a price series lives. The two crates sit at the same layer and neither may name the other,
//! so the realized vol a caller passes to [`iv_rv_spread`] is computed there and handed in as a
//! plain `f64`; what stays here is what reads an [`OptionChain`].
//!
//! Conventions:
//! - Both sides of [`iv_rv_spread`] are ANNUALIZED vols on the same basis. This crate's year is
//!   **365 calendar days** (`greeks.rs`' theta/expiry basis — crypto trades every day), so a
//!   realized vol from daily closes is annualized by `√365` before it is compared.
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
//! ## The one transcendental here comes from the `libm` CRATE (converted 2026-08-26)
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! ratified rule and carries the measurement. The short form: IEEE 754 requires `+ - * /` and
//! `sqrt` to be correctly rounded and requires NOTHING of `ln`/`exp`, so `f64::exp` reaches
//! whichever libm the platform ships and MSVC's CRT and glibc are each entitled to a different
//! last bit. The forward's `e^{rT}` in [`atm_iv`] is therefore `libm::exp`. It has no row in this
//! crate's `tests/libm_platform_probe.rs` (a declared gap there); the source gate in that probe is
//! what stops it being respelled as `(r * t).exp()`.
//!
//! ⚠ **Materiality is low, and the record says so rather than implying urgency.** Nothing outside
//! this file calls [`atm_iv`] or [`iv_rv_spread`] — `git grep -n 'atm_iv\|iv_rv_spread' -- crates/`
//! answers with this module, the `lib.rs` re-export and a doc reference in `basket/prob.rs`. This
//! is a pure core awaiting a consumer, so the conversion moved no stored number, no backtest and no
//! live quote. It was done anyway, BEFORE the first consumer arrives, because the cheap moment to
//! make a value portable is while nothing has pinned it yet.

use vike_marketdata::OptionChain;

/// IV−RV spread: positive when implied trades RICH to realized (the classic vol-premium
/// read), negative when implied is cheap. Both inputs must already be annualized on the
/// same basis (see the module doc); pure difference, no scaling.
pub fn iv_rv_spread(iv_annual: f64, rv_annual: f64) -> f64 {
    iv_annual - rv_annual
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
mod tests;
