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
//! standard "nearest-to-forward" ATM convention.
//!
//! The selection is two steps any quote list can share, so a caller without an [`OptionChain`]
//! (a stored chain's flat rows, `crate::features::atm_iv_by_expiry`) reads the SAME rule:
//! [`smile_points`] folds the quotes into one `(strike, iv)` point per strike (the mean of the
//! IVs quoted there, so the call and put of one strike average as above), and
//! [`atm_iv_of_smile`] takes the point nearest the forward (the first on a tie, so the lower
//! strike of ascending rows). [`atm_iv`] is those two over a chain's rows.
//!
//! ## Smile interpolation
//!
//! [`smile_iv`] reads the IV at any strike inside the quoted range: linear in VARIANCE (`σ²`) in
//! strike between the two quoted strikes that bracket it (one expiry, so linear in total
//! variance too), the quoted IV at a quoted strike, and `None` outside the range (no
//! extrapolation: a wing beyond the last quote is the caller's model, an SVI slice for one,
//! `crate::svi`). The interpolated at-the-money IV is `smile_iv(&points, forward)`; [`atm_iv`]
//! stays the nearest-strike read.
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
//! ⚠ **Materiality was low when it was converted, and the record says so rather than implying
//! urgency.** On 2026-08-26 nothing outside this file called [`atm_iv`] or [`iv_rv_spread`], so the
//! conversion moved no stored number, no backtest and no live quote. It was done anyway, BEFORE the
//! first consumer arrived, because the cheap moment to make a value portable is while nothing has
//! pinned it yet. The first consumer is `crate::features::atm_iv_by_expiry` (2026-11-17), the
//! at-the-money feature a research study reads per expiry; the forward is now in [`atm_iv_of_smile`].

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
    let quotes = chain.rows.iter().flat_map(|row| {
        [row.call.as_ref(), row.put.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(move |q| q.iv.map(|iv| (row.strike, iv)))
    });
    atm_iv_of_smile(&smile_points(quotes), s, t, r)
}

/// One point per strike of a smile from `(strike, iv)` quotes: the mean of the IVs quoted at that
/// strike (a call and a put of one strike, `(c + p) / 2`), in the order each strike first appears.
pub fn smile_points(quotes: impl IntoIterator<Item = (f64, f64)>) -> Vec<(f64, f64)> {
    // (strike, sum of its IVs, how many): the sum starts at the first IV, so one quote is itself.
    let mut points: Vec<(f64, f64, f64)> = Vec::new();
    for (strike, iv) in quotes {
        match points.iter_mut().find(|point| point.0 == strike) {
            Some(point) => {
                point.1 += iv;
                point.2 += 1.0;
            }
            None => points.push((strike, iv, 1.0)),
        }
    }
    points.into_iter().map(|(strike, sum, count)| (strike, sum / count)).collect()
}

/// The at-the-money IV of one expiry's smile ([`smile_points`]): the point nearest the forward
/// `F = spot·e^{r·t}`, the first one on an exact tie. `None` when `spot <= 0`, `t < 0`, or the
/// smile is empty.
pub fn atm_iv_of_smile(points: &[(f64, f64)], spot: f64, t: f64, r: f64) -> Option<f64> {
    if spot <= 0.0 || t < 0.0 {
        return None;
    }
    // `libm::exp`, never `f64::exp` (ADR 0032). The forward only SELECTS a strike here, so a
    // last-bit difference is invisible unless it lands on an exact distance tie — but "invisible
    // unless" is precisely the class of divergence that surfaces once, in production, on the one
    // chain whose strikes straddle the forward evenly.
    let forward = spot * libm::exp(r * t);
    points
        .iter()
        .min_by(|a, b| (a.0 - forward).abs().total_cmp(&(b.0 - forward).abs()))
        .map(|point| point.1)
}

/// The IV at `strike` on one expiry's smile ([`smile_points`], one point per strike, any order):
/// linear in variance in strike between the two quoted strikes that bracket it, the quoted IV at a
/// quoted strike. `None` outside the quoted range or on an empty smile (see the module doc).
pub fn smile_iv(points: &[(f64, f64)], strike: f64) -> Option<f64> {
    let below = points.iter().filter(|p| p.0 <= strike).max_by(|a, b| a.0.total_cmp(&b.0))?;
    let above = points.iter().filter(|p| p.0 >= strike).min_by(|a, b| a.0.total_cmp(&b.0))?;
    if below.0 == above.0 {
        return Some(below.1);
    }
    let weight = (strike - below.0) / (above.0 - below.0);
    let variance = below.1 * below.1 + weight * (above.1 * above.1 - below.1 * below.1);
    // `sqrt` stays on `f64`: IEEE 754 rounds it correctly, so it is the same on every box.
    Some(variance.sqrt())
}

#[cfg(test)]
mod tests;
