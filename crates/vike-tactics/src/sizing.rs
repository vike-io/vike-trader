//! [`TacticsSizing`], what a rule works a size out from (decision 0269, W5): the account's equity, what one
//! unit of the symbol is worth, the header's commission, and the slippage and quantity step the run
//! states. It is read once ([`crate::Tactics::sizing`]) and is a value, so a size worked out from it
//! borrows nothing and can sit inside an order verb's builder.
//!
//! The money law is the order layer's: one unit's worth is `vike_model::ContractValue::unit_value`,
//! a share of the equity `vike_model::share_quantity`, and a floor to the quantity step
//! `vike_model::floor_to_step`, the three a backtest's own sizers call.
//!
//! Beside it, how strategy files turn money and stated quantities into sizes on the step, as their
//! originals floor ([`share_of_worth`], [`cash_worth`], [`floor_printed`], [`floor_share`]; copied
//! from the strategy gallery, plan 2b, C3). `TacticsSizing` is named at the crate root.

use vike_model::{ContractValue, floor_to_step, share_quantity};

use crate::header::RuleDefaultsFee;
use vike_model::OrderSide;

/// The inputs of a size, as they stood when read. Reads change only on fills, so one taken at the
/// start of a hook holds for the whole hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TacticsSizing {
    pub(crate) equity: f64,
    pub(crate) value: ContractValue,
    pub(crate) commission: RuleDefaultsFee,
    pub(crate) slippage: f64,
    pub(crate) qty_step: Option<f64>,
}

impl TacticsSizing {
    /// The account's equity (`vike_model::OrderBroker::equity`).
    pub fn equity(&self) -> f64 {
        self.equity
    }

    /// The quantity step a size is floored to: the run's ([`crate::RuleRun::qty_step`]), else the lot size
    /// of the broker's grid for the symbol; none when neither states a positive one.
    pub fn qty_step(&self) -> Option<f64> {
        self.qty_step.filter(|step| *step > 0.0)
    }

    /// `price` moved against the trader by the slippage the run expects on a market or stop fill
    /// ([`crate::RuleRun::slippage`]; none unless it states one), the price such a fill is sized at. The
    /// broker charges what it charges: this is a read for a size, never a fill.
    pub fn slipped(&self, side: OrderSide, price: f64) -> f64 {
        price + f64::from(side) * self.slippage
    }

    /// The size that spends `share` of the equity at `price` with the header's commission in it
    /// (`vike_model::share_quantity`, one unit worth `vike_model::ContractValue::unit_value` at `price`),
    /// floored to the quantity step when there is one.
    pub fn size_for_share(&self, share: f64, price: f64) -> f64 {
        let (rate, per_unit, per_order) = match self.commission {
            RuleDefaultsFee::Free => (0.0, 0.0, 0.0),
            RuleDefaultsFee::Percent(pct) => (pct / 100.0, 0.0, 0.0),
            RuleDefaultsFee::PerContract(c) => (0.0, c, 0.0),
            RuleDefaultsFee::PerOrder(c) => (0.0, 0.0, c),
        };
        self.floor(share_quantity(share, self.equity, self.unit(price), rate, per_unit, per_order))
    }

    /// The size `cash` is worth at `price`, with no commission in it, floored to the quantity step
    /// when there is one. `0.0` when one unit is worth nothing.
    pub fn size_for_cash(&self, cash: f64, price: f64) -> f64 {
        let unit = self.unit(price);
        self.floor(if unit != 0.0 { cash / unit } else { 0.0 })
    }

    /// `qty` floored to the quantity step (`vike_model::floor_to_step`), as it is with none.
    pub fn floor(&self, qty: f64) -> f64 {
        self.qty_step().map_or(qty, |step| floor_to_step(qty, step))
    }

    /// What one unit is worth at `price`, in the money the equity is in; `0.0` when the value has
    /// no usable answer.
    fn unit(&self, price: f64) -> f64 {
        self.value.unit_value(price, 1.0).unwrap_or(0.0)
    }
}

/// `percent` of the account's equity at `price`, the commission in it, floored to the step.
pub fn share_of_worth(sizing: &TacticsSizing, percent: f64, price: f64) -> f64 {
    sizing.size_for_share(percent / 100.0, price)
}

/// `cash` worth at `price`, with no commission in it, floored to the step.
pub fn cash_worth(sizing: &TacticsSizing, cash: f64, price: f64) -> f64 {
    sizing.size_for_cash(cash, price)
}

/// A decimal step's count of decimals and its lots per unit (0.00001 is 5 and 100,000; 1 is 0 and 1),
/// `None` for a step that is not one over a power of ten.
fn decimals(step: f64) -> Option<(usize, f64)> {
    if !(step.is_finite() && step > 0.0) {
        return None;
    }
    let lots = (1.0 / step).round();
    let d = libm::log10(lots).round();
    (d >= 0.0 && libm::pow(10.0, d) == lots).then_some((d as usize, lots))
}

/// A size the rule states, floored to the step as its shortest printed decimal reads, as the
/// originals floor an explicit quantity, where the double may lie a hair under it: 0.155 * 3 prints
/// 0.46499999999999997 and enters as 0.46499 on a step of 0.00001. 0 for a size that is not a positive
/// number. With no decimal step it is left as it is.
pub fn floor_printed(qty: f64, step: Option<f64>) -> f64 {
    if !qty.is_finite() || qty <= 0.0 {
        return 0.0;
    }
    let Some((decimals, lots_per_unit)) = step.and_then(decimals) else {
        return qty;
    };
    let text = format!("{qty}");
    let (whole, frac) = text.split_once('.').unwrap_or((&text, ""));
    let frac: String = frac.chars().chain(std::iter::repeat('0')).take(decimals).collect();
    let lots: f64 = format!("{whole}{frac}").parse().expect("a printed number is digits");
    lots / lots_per_unit
}

/// A share of `of` that a rule closes or exits (a percentage of a position or of an entry): floored
/// to the step, a product within a few ulps of a whole step taken as that step, and never under one
/// step while `of` holds one. With no decimal step it is left as it is.
pub fn floor_share(qty: f64, of: f64, step: Option<f64>) -> f64 {
    let Some((_, lots_per_unit)) = step.and_then(decimals) else {
        return qty;
    };
    let snapped = |q: f64| {
        let steps = q.abs() * lots_per_unit;
        let nearest = steps.round();
        let ulp = f64::from_bits(steps.to_bits() + 1) - steps;
        let lots =
            if (steps - nearest).abs() <= 8.0 * ulp + 1e-9 { nearest } else { steps.floor() };
        lots / lots_per_unit
    };
    let size = snapped(qty);
    if size == 0.0 && qty != 0.0 && snapped(of) != 0.0 { 1.0 / lots_per_unit } else { size }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decimal step as a run states it: one over its lots per unit.
    fn step(decimals: i32) -> Option<f64> {
        Some(1.0 / 10f64.powi(decimals))
    }

    #[test]
    fn a_stated_size_and_a_share_floor_as_their_originals_do() {
        let btc = step(5);
        assert_eq!(btc.and_then(decimals), Some((5, 100_000.0)));
        assert_eq!(
            floor_printed(0.155 + 0.155 + 0.155, btc),
            0.46499,
            "the double under its decimal loses a step"
        );
        assert_eq!(floor_printed(0.16 + 0.16 + 0.16, btc), 0.48);
        assert_eq!(
            (floor_printed(2.0, btc), floor_printed(0.000019, btc), floor_printed(0.0, btc)),
            (2.0, 0.00001, 0.0)
        );
        assert_eq!(floor_share(0.46499 * 0.3, 0.46499, btc), 0.13949);
        assert_eq!(
            floor_share(0.48 * 0.3, 0.48, btc),
            0.144,
            "a few ulps under a step is that step"
        );
        assert_eq!(floor_share(0.00002 * 0.3, 0.00002, btc), 0.00001, "never under one step");
        // A symbol with another step: DOGEUSDT trades whole coins, ETHUSDT in 0.0001.
        let doge = step(0);
        assert_eq!((floor_printed(2.7, doge), floor_printed(0.9, doge)), (2.0, 0.0));
        assert_eq!(
            (floor_share(0.3 * 2.0, 2.0, doge), floor_share(0.3 * 1.0, 1.0, doge)),
            (1.0, 1.0)
        );
        assert_eq!(floor_printed(0.123_49, step(4)), 0.1234);
        assert_eq!(
            (floor_printed(0.123_49, None), floor_share(0.3, 1.0, None)),
            (0.123_49, 0.3),
            "no step"
        );
    }
}
