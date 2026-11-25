//! The spot–perpetual basis and the funding carry: the other spread a crypto book trades (chapter 38).
//!
//! A perpetual future never expires, so nothing pulls its price to the spot's; the exchange does it with
//! FUNDING instead. Every settlement (every eight hours on Binance) the longs pay the shorts the funding
//! rate times their notional when the perpetual trades above the spot, and the shorts pay the longs when
//! it trades below. Hold the spot and short the same notional of the perpetual and the position has no
//! price risk to first order: it earns the funding while the perpetual is rich, and it earns or loses the
//! change of the BASIS (the perpetual over the spot) meanwhile.
//!
//! - [`log_basis`]: `ln(perpetual / spot)` per bar, a spread of two log prices with a hedge ratio of one, so
//!   the spread indicators and the reversion model of [`crate::spread`] read it as they read a pair's.
//! - [`funding_in_bars`]: the funding that SETTLES inside each bar: what a position held across the bar is paid.
//! - [`last_settled_funding`]: the funding a decision made at a bar's open can KNOW: the latest settlement
//!   before the open, `NaN` before the first. The feature to trade and to train on; [`funding_in_bars`] is the
//!   realised income, which at the decision is still in the future.
//! - [`funding_carry`]: the gross profit of the rule "hold the carry while the last funding is above a threshold",
//!   split into funding income and basis change. What its trades cost is a backtest's, charged the one cost type
//!   (`vike_model::Costs`), never a rate of this crate's.
//!
//! **Signs.** Funding is read as the venue prints it: positive when longs pay. The carry position is
//! long spot, short perpetual, so positive funding is income. The basis term of a bar is
//! `−(basis_t − basis_(t−1))`: the perpetual's return less the spot's, with the sign of a short perpetual.
//!
//! **Time.** A settlement belongs to the bar that CONTAINS its timestamp (`open ≤ time < open + interval`):
//! a venue stamps a settlement a few milliseconds after the hour, and a position taken at that bar's open is
//! the one that is held at the settlement. Bars and settlements are in milliseconds; both lists are in
//! ascending time.

use vike_math::time_series::log_ratio;

/// `ln(perpetual / spot)` per bar; `NaN` where either price is not a positive finite number.
///
/// `None` when the two series differ in length.
pub fn log_basis(perpetual: &[f64], spot: &[f64]) -> Option<Vec<f64>> {
    if perpetual.len() != spot.len() {
        return None;
    }
    // `log_ratio` refuses a price that is not positive; an infinite one is refused here.
    Some(
        perpetual
            .iter()
            .zip(spot)
            .map(|(&p, &s)| if p.is_finite() && s.is_finite() { log_ratio(p, s) } else { f64::NAN })
            .collect(),
    )
}

/// The sum of the settlements `(time, rate)` that fall inside each bar `[open, open + interval)`, `0` for
/// a bar with none (no settlement is no payment, which is a real zero here, unlike a missing price).
pub fn funding_in_bars(
    bar_opens: &[i64],
    interval_ms: i64,
    settlements: &[(i64, f64)],
) -> Vec<f64> {
    bar_opens
        .iter()
        .map(|&open| {
            let from = settlements.partition_point(|&(time, _)| time < open);
            let until = settlements.partition_point(|&(time, _)| time < open + interval_ms);
            let mut total = 0.0;
            for &(_, rate) in &settlements[from..until] {
                total += rate;
            }
            total
        })
        .collect()
}

/// The rate of the latest settlement strictly before each bar's open: what is known when a decision is
/// made at the open. `NaN` before the first settlement.
pub fn last_settled_funding(bar_opens: &[i64], settlements: &[(i64, f64)]) -> Vec<f64> {
    bar_opens
        .iter()
        .map(|&open| {
            let settled = settlements.partition_point(|&(time, _)| time < open);
            settled.checked_sub(1).map_or(f64::NAN, |last| settlements[last].1)
        })
        .collect()
}

/// When the carry is held: entered once the last settled funding is ABOVE `entry`, left once it is BELOW
/// `exit`. `exit` below `entry` gives a band, so a rate that hovers at one threshold does not trade every bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarryRule {
    pub entry: f64,
    pub exit: f64,
}

/// The carry's gross profit, per bar, in log-price units per unit of notional on each leg.
#[derive(Clone, Debug, PartialEq)]
pub struct CarryResult {
    /// `1` while the carry is held through the bar, else `0`.
    pub position: Vec<f64>,
    /// The funding paid to the short perpetual in the bar.
    pub funding: Vec<f64>,
    /// `−(basis_t − basis_(t−1))` while held: the perpetual's return less the spot's.
    pub basis: Vec<f64>,
}

impl CarryResult {
    /// The bar's gross profit: funding plus basis change.
    pub fn net(&self) -> Vec<f64> {
        (0..self.position.len()).map(|t| self.funding[t] + self.basis[t]).collect()
    }
}

/// The funding carry on `log_basis` (one value per bar), `funding_in_bar` (what settles in each bar,
/// [`funding_in_bars`]) and `last_funding` (what a bar's open knows, [`last_settled_funding`]), under `rule`,
/// charging nothing (the module doc). The decision for a bar reads `last_funding` of that bar only;
/// bar 0 has no previous basis and is flat; a bar whose basis or the previous one is missing keeps its
/// position and earns nothing, and a missing `last_funding` keeps the position too.
///
/// `None` when the three series differ in length.
pub fn funding_carry(
    log_basis: &[f64],
    funding_in_bar: &[f64],
    last_funding: &[f64],
    rule: CarryRule,
) -> Option<CarryResult> {
    let n = log_basis.len();
    if funding_in_bar.len() != n || last_funding.len() != n {
        return None;
    }
    let mut result =
        CarryResult { position: vec![0.0; n], funding: vec![0.0; n], basis: vec![0.0; n] };
    let mut held = 0.0;
    for t in 1..n {
        let last = last_funding[t];
        let wanted = if last.is_nan() {
            held
        } else if held == 0.0 {
            if last > rule.entry { 1.0 } else { 0.0 }
        } else if last < rule.exit {
            0.0
        } else {
            1.0
        };
        let (now, before) = (log_basis[t], log_basis[t - 1]);
        if !(now.is_finite() && before.is_finite()) {
            result.position[t] = held;
            continue;
        }
        held = wanted;
        result.position[t] = held;
        result.funding[t] = held * funding_in_bar[t];
        result.basis[t] = held * -(now - before);
    }
    Some(result)
}
