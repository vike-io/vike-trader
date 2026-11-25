//! Size math for the ticket — pure. The quick sizes are multiples of the instrument's lot step
//! (spec §3.6), not the DOM's fixed bitcoin quantities.
//!
//! A size is in the ORDER's units, and what one unit is worth is the contract's
//! ([`ContractValue`], the window's account's value of its symbol): a quote amount becomes a size by
//! [`ContractValue::units_worth`] and a size a quote amount by [`ContractValue::order_notional`], the
//! numbers the preview and the node's gate judge. 250,000 typed on an ES future (50 a point) at 5,000
//! is ONE contract, not 50; 1,000 typed on Deribit's `BTC-PERPETUAL` (one dollar a contract) is 1,000
//! contracts at any price. At the default unit every figure is the plain `value / price` it was.

use super::{TradeWindowSizingUnit, TradeWindowTicketExits};
use vike_model::{ContractValue, Grid};
use vike_ui_theme::components::input::parse_number;

/// The lot multiples the five quick-size buttons offer.
const MULTIPLES: [f64; 5] = [1.0, 5.0, 10.0, 50.0, 100.0];

/// `quantity` rounded DOWN to the lot grid: a size the trader typed is never enlarged. The epsilon keeps
/// an exact multiple from losing a lot to float noise (`0.01 / 0.001` is `9.999999999999998`).
/// A lot that is not a positive number (an instrument the catalog has no grid for) floors every
/// size to 0, and so does a size too large to be a number of lots (`1e306` on a 0.001 lot).
pub fn floor_to_lot(quantity: f64, lot: f64) -> f64 {
    if lot > 0.0 && lot.is_finite() && quantity > 0.0 && quantity.is_finite() {
        Some((quantity / lot + 1e-9).floor() * lot).filter(|q| q.is_finite()).unwrap_or(0.0)
    } else {
        0.0
    }
}

/// `price` on the tick grid (nearest).
pub(crate) fn on_tick(price: f64, tick: f64) -> f64 {
    if tick > 0.0 { (price / tick).round() * tick } else { price }
}

/// The five quick sizes for `grid`, in base units: lot multiples, starting at the smallest valid
/// size (one lot, or `min_qty` rounded up to the lot).
///
/// ⚠ ALL ZERO while the lot is not known (`None`: the catalog's row has not arrived). A size made
/// up from an invented lot of 1 is ten coins on a market whose lot is a thousandth, and the window
/// seeds its size field from these: a one-click ladder sends it as is.
pub(crate) fn quick_sizes(grid: Grid) -> [f64; 5] {
    let Some(lot) = grid.lot_size else { return [0.0; 5] };
    let smallest = grid.min_qty.map_or(lot, |min| min.max(lot));
    let units = (smallest / lot - 1e-9).ceil().max(1.0);
    MULTIPLES.map(|k| units * k * lot)
}

/// The order quantity a size field means: as typed, or a quote amount converted at `price` into
/// units of `value` ([`ContractValue::units_worth`]), floored to the lot. `None` when the lot is not
/// known, the text is not a positive number, a quote size has no price to convert at, or the result
/// is smaller than one lot.
pub(crate) fn base_quantity(
    text: &str,
    unit: TradeWindowSizingUnit,
    price: Option<f64>,
    lot: Option<f64>,
    value: ContractValue,
) -> Option<f64> {
    let v = parse_number(text).filter(|v| *v > 0.0)?;
    let base = match unit {
        TradeWindowSizingUnit::Base => v,
        TradeWindowSizingUnit::Quote => value.units_worth(v, price.filter(|p| *p > 0.0)?, 1.0)?,
    };
    let q = floor_to_lot(base, lot?);
    (q > 0.0).then_some(q)
}

/// The order quantity `pct` % of the buying power buys at `price` in units of `value`, floored to the
/// lot. `None` without a known lot, a positive price and buying power, or when the share is smaller
/// than one lot.
pub(crate) fn share_of_buying_power(
    pct: f64,
    buying_power: f64,
    price: f64,
    lot: Option<f64>,
    value: ContractValue,
) -> Option<f64> {
    if price > 0.0 && buying_power > 0.0 {
        let q = floor_to_lot(value.units_worth(buying_power * pct / 100.0, price, 1.0)?, lot?);
        (q > 0.0).then_some(q)
    } else {
        None
    }
}

/// A base quantity as the window prints it: to the lot's decimals, or, while the lot is not known
/// (`None`), to its own — so a size is never printed as `0` because the lot that would print it is
/// missing (final review A, minor 8). The ticket, the ladder's markers and the confirm prompt all
/// print with it.
pub(crate) fn quantity_text(q: f64, lot: Option<f64>) -> String {
    format!("{:.*}", decimals_of(lot.unwrap_or(q)), q)
}

/// What a size field shows in the quote currency when there is no price to convert at.
pub(crate) const NO_QUOTE: &str = "—";

/// The size field's text for an order quantity, in `unit`.
///
/// Base prints to the lot's decimals (none while the lot is not known). Quote prints what the
/// quantity is worth at `price` ([`ContractValue::order_notional`] of `value`), rounded UP to
/// `quote_decimals` (the cent while the lot is not known): [`base_quantity`]
/// floors to the lot on the way back, so the round trip returns exactly the quantity it started
/// from — never a lot less (`0.010` at `65,432.42` printed to the nearest cent, `654.32`, read back
/// as `0.009`), and never more. With no positive price to convert at, [`NO_QUOTE`].
pub(crate) fn size_text(
    base: f64,
    unit: TradeWindowSizingUnit,
    price: Option<f64>,
    lot: Option<f64>,
    value: ContractValue,
) -> String {
    match unit {
        TradeWindowSizingUnit::Base => format!("{:.*}", lot.map_or(0, decimals_of), base),
        TradeWindowSizingUnit::Quote => match price.filter(|p| *p > 0.0 && p.is_finite()) {
            Some(p) => {
                let d = quote_decimals(lot.map_or(0.0, |lot| value.order_notional(lot, p)));
                let scale = vike_math::powers::power_of_ten(d as i32)
                    .expect("quote_decimals answers at most MAX_DECIMALS (12), inside 10^±22");
                let worth = value.order_notional(base, p);
                format!("{:.*}", d, (worth * scale - 1e-9).ceil() / scale)
            }
            None => NO_QUOTE.to_string(),
        },
    }
}

/// The decimals a quote value prints to: the cent, or more where one lot is worth less than two
/// units in the last place — so rounding a value UP by under one such unit adds under half a lot,
/// and flooring to the lot takes it back off. `lot_value` is one lot's worth at the price.
fn quote_decimals(lot_value: f64) -> usize {
    let mut d = 2;
    while d < MAX_DECIMALS
        && lot_value > 0.0
        && vike_math::powers::power_of_ten(-(d as i32)).is_some_and(|unit| unit > lot_value / 2.0)
    {
        d += 1;
    }
    d
}

/// A bracket's exits for an entry at `entry`: the take-profit `take_profit_percent` % in the position's favour,
/// the stop-loss `stop_loss_percent` % against it, both on the tick. `side` is `+1` buy, `−1` sell.
///
/// ⚠ Each exit is at least ONE tick from the entry, on its own side. Rounded to the nearest tick
/// alone, a distance under half a tick lands ON the entry, and a stop there triggers at once.
///
/// `None` — no bracket — when there is no tick grid (`None`: an exit would sit off the grid), when
/// the entry or a distance is not a positive number, or when an exit would fall to zero or below.
pub fn exits(
    side: i32,
    entry: f64,
    take_profit_percent: f64,
    stop_loss_percent: f64,
    tick: Option<f64>,
) -> Option<TradeWindowTicketExits> {
    let tick = tick?;
    let positive = |v: f64| v > 0.0 && v.is_finite();
    if [entry, take_profit_percent, stop_loss_percent, tick].into_iter().any(|v| !positive(v)) {
        return None;
    }
    // The first tick strictly above, and strictly below, the entry. A price within float noise of a
    // tick counts as on it.
    let above = ((entry / tick + 1e-9).floor() + 1.0) * tick;
    let below = ((entry / tick - 1e-9).ceil() - 1.0) * tick;
    let gain = on_tick(entry * (1.0 + take_profit_percent / 100.0), tick);
    let loss = on_tick(entry * (1.0 - stop_loss_percent / 100.0), tick);
    let fall = on_tick(entry * (1.0 - take_profit_percent / 100.0), tick);
    let rise = on_tick(entry * (1.0 + stop_loss_percent / 100.0), tick);
    let (take_profit, stop_loss) = if side > 0 {
        (gain.max(above), loss.min(below))
    } else {
        (fall.min(below), rise.max(above))
    };
    (positive(take_profit) && positive(stop_loss))
        .then_some(TradeWindowTicketExits { take_profit, stop_loss })
}

/// The most decimals [`decimals_of`] answers: a step that is not a decimal fraction (a third) stops
/// here, and no venue lists a tick or a lot finer than this.
const MAX_DECIMALS: usize = 12;

/// The decimals that print a multiple of `step` exactly (`vike_math::steps::step_decimals`): `0.001`
/// → 3, `1` → 0, `1e-8` → 8, at most `MAX_DECIMALS`. A step that is zero or not a number answers 0,
/// so a price prints whole rather than not at all.
pub fn decimals_of(step: f64) -> usize {
    vike_math::steps::step_decimals(step).map_or(0, |decimals| decimals.min(MAX_DECIMALS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_window::grid_value;

    // The tests below that name no contract size at the default unit, one base unit per quantity,
    // and take the lot as a catalog spells it, through the window's grid builder (`0` is none).
    fn base_quantity(
        text: &str,
        unit: TradeWindowSizingUnit,
        price: Option<f64>,
        lot: f64,
    ) -> Option<f64> {
        super::base_quantity(text, unit, price, grid_value(lot), ContractValue::default())
    }
    fn share_of_buying_power(pct: f64, buying_power: f64, price: f64, lot: f64) -> Option<f64> {
        super::share_of_buying_power(
            pct,
            buying_power,
            price,
            grid_value(lot),
            ContractValue::default(),
        )
    }
    fn size_text(base: f64, unit: TradeWindowSizingUnit, price: Option<f64>, lot: f64) -> String {
        super::size_text(base, unit, price, grid_value(lot), ContractValue::default())
    }
    fn grid(tick: f64, lot: f64, min_quantity: f64) -> Grid {
        Grid {
            tick_size: grid_value(tick),
            lot_size: grid_value(lot),
            min_notional: None,
            min_qty: grid_value(min_quantity),
        }
    }

    /// THE wave-2 audit's ticket defect: a quote amount was sized as `value / price`, so 250,000
    /// typed on ES (50 a point) at 5,000 sent 50 contracts, 12.5 million of exposure. It is ONE
    /// contract; 1,000 typed on Deribit's `BTC-PERPETUAL` is 1,000 one-dollar contracts at any price
    /// (`value / price` made it 0.015, under one lot), and both print back as what they are worth.
    #[test]
    fn a_quote_amount_is_sized_in_the_contracts_own_units() {
        let es = ContractValue::linear(50.0);
        assert_eq!(
            super::base_quantity(
                "250000",
                TradeWindowSizingUnit::Quote,
                Some(5_000.0),
                Some(1.0),
                es
            ),
            Some(1.0)
        );
        assert_eq!(
            super::size_text(1.0, TradeWindowSizingUnit::Quote, Some(5_000.0), Some(1.0), es),
            "250000.00"
        );
        let half = super::share_of_buying_power(50.0, 1_000_000.0, 5_000.0, Some(1.0), es);
        assert_eq!(half, Some(2.0), "half of 1,000,000 buys two 250,000 contracts, not 100");
        let deribit = ContractValue::inverse(1.0);
        for price in [65_432.5, 100_000.0] {
            let q = super::base_quantity(
                "1000",
                TradeWindowSizingUnit::Quote,
                Some(price),
                Some(1.0),
                deribit,
            );
            assert_eq!(q, Some(1_000.0), "at {price}");
            assert_eq!(
                super::size_text(
                    1_000.0,
                    TradeWindowSizingUnit::Quote,
                    Some(price),
                    Some(1.0),
                    deribit
                ),
                "1000.00"
            );
        }
        let all = super::share_of_buying_power(100.0, 2_500.0, 65_432.5, Some(10.0), deribit);
        assert_eq!(all, Some(2_500.0));
        // the default unit is `value / price`, bit for bit
        for (v, p, lot) in [(654.3, 65_432.5, 0.001), (1_234.5, 0.98765, 1e-8)] {
            let q = super::base_quantity(
                &format!("{v}"),
                TradeWindowSizingUnit::Quote,
                Some(p),
                Some(lot),
                ContractValue::default(),
            );
            assert_eq!(q, Some(floor_to_lot(v / p, lot)));
        }
    }

    #[test]
    fn decimals_follow_the_step() {
        for (step, d) in
            [(1.0, 0), (0.5, 1), (0.1, 1), (0.01, 2), (0.001, 3), (0.0001, 4), (1e-8, 8)]
        {
            assert_eq!(decimals_of(step), d, "{step}");
        }
    }

    /// A step under a billionth keeps its decimals (pre-flight Minor 3: an ABSOLUTE tolerance of
    /// 1e-9 answered 0 for every such step, because the step itself was under it). A step that is
    /// not a number answers 0, so a price prints whole rather than not at all.
    #[test]
    fn a_step_under_a_billionth_keeps_its_decimals_and_a_non_step_has_none() {
        for (step, d) in [(1e-10, 10), (2.5e-11, 12), (1e-12, 12), (-0.01, 2)] {
            assert_eq!(decimals_of(step), d, "{step}");
        }
        for step in [0.0, f64::NAN, f64::INFINITY] {
            assert_eq!(decimals_of(step), 0, "{step}");
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn quick_sizes_are_lot_multiples_from_the_smallest_valid_size() {
        let btc = quick_sizes(grid(0.1, 0.001, 0.001));
        for (got, want) in btc.iter().zip([0.001, 0.005, 0.01, 0.05, 0.1]) {
            assert!(close(*got, want), "{btc:?}");
        }
        let xrp = quick_sizes(grid(0.0001, 1.0, 1.0));
        assert_eq!(xrp, [1.0, 5.0, 10.0, 50.0, 100.0]);
        let floor = quick_sizes(grid(0.1, 0.001, 0.005));
        assert!(close(floor[0], 0.005), "min_qty above the lot starts the row: {floor:?}");
    }

    /// Review fix 1: a lot the catalog has not given yet is NOT a lot of 1. The window seeds its
    /// size from the middle quick size, so a made-up lot sent ten coins where the lot is 0.001.
    #[test]
    fn an_unknown_lot_offers_no_quick_size() {
        for lot in [0.0, -0.001, f64::NAN, f64::INFINITY] {
            assert_eq!(quick_sizes(grid(0.1, lot, 0.001)), [0.0; 5], "{lot}");
        }
        let seeded =
            size_text(quick_sizes(grid(0.1, 0.0, 0.0))[2], TradeWindowSizingUnit::Base, None, 0.0);
        assert_eq!(
            base_quantity(&seeded, TradeWindowSizingUnit::Base, None, 0.001),
            None,
            "{seeded:?} trades nothing"
        );
    }

    #[test]
    fn a_size_is_floored_to_the_lot_and_never_enlarged() {
        assert!(close(floor_to_lot(0.0109, 0.001), 0.010));
        assert!(close(floor_to_lot(0.01, 0.001), 0.01), "an exact multiple survives float noise");
        assert_eq!(floor_to_lot(0.0004, 0.001), 0.0);
    }

    #[test]
    fn a_quote_size_converts_at_the_price_and_a_bad_one_is_none() {
        let q = base_quantity("654.3", TradeWindowSizingUnit::Quote, Some(65_432.5), 0.001)
            .expect("converts");
        assert!(close(q, 0.009), "{q}");
        assert!(close(
            base_quantity("0.01", TradeWindowSizingUnit::Base, None, 0.001).unwrap(),
            0.01
        ));
        assert_eq!(base_quantity("abc", TradeWindowSizingUnit::Base, None, 0.001), None);
        assert_eq!(
            base_quantity("100", TradeWindowSizingUnit::Quote, None, 0.001),
            None,
            "no price, no conversion"
        );
        assert_eq!(
            base_quantity("0.0001", TradeWindowSizingUnit::Base, None, 0.001),
            None,
            "below one lot"
        );
        // A lot of 0 is an instrument the catalog has no row for: no size is valid there.
        assert_eq!(
            base_quantity("1", TradeWindowSizingUnit::Base, None, 0.0),
            None,
            "no grid, no size"
        );
    }

    #[test]
    fn a_share_of_buying_power_is_floored_to_the_lot() {
        let q = share_of_buying_power(25.0, 65_400.0, 65_432.5, 0.001).unwrap();
        assert!(close(q, 0.249), "{q}");
        assert_eq!(share_of_buying_power(25.0, 0.0, 65_432.5, 0.001), None);
    }

    #[test]
    fn exits_sit_on_the_tick_in_the_positions_favour_and_against_it() {
        let buy = exits(1, 100.0, 0.5, 0.3, Some(0.1)).expect("a grid");
        assert!(close(buy.take_profit, 100.5) && close(buy.stop_loss, 99.7), "{buy:?}");
        let sell = exits(-1, 100.0, 0.5, 0.3, Some(0.1)).expect("a grid");
        assert!(close(sell.take_profit, 99.5) && close(sell.stop_loss, 100.3), "{sell:?}");
    }

    /// A distance under half a tick does not round onto the entry: each exit is at least one tick
    /// away on its own side (a stop ON the entry triggers at once). Off the entry's grid too: an
    /// entry between two ticks takes the next tick out.
    #[test]
    fn an_exit_is_never_closer_than_one_tick() {
        let e = |side, entry, take_profit, stop_loss| {
            let x = exits(side, entry, take_profit, stop_loss, Some(1.0)).expect("a grid");
            (x.take_profit, x.stop_loss)
        };
        assert_eq!(e(1, 100.0, 0.5, 0.3), (101.0, 99.0), "a 0.3 % stop rounded onto the entry");
        assert_eq!(e(1, 100.0, 0.1, 0.1), (101.0, 99.0));
        assert_eq!(e(-1, 100.0, 0.1, 0.1), (99.0, 101.0));
        assert_eq!(e(1, 100.3, 0.1, 0.1), (101.0, 100.0), "off the grid: the next tick out");
        assert_eq!(e(1, 100.0, 5.0, 3.0), (105.0, 97.0), "a wide bracket is unchanged");
    }

    /// No tick grid, no bracket: an exit would sit off the grid. Nor for a distance or an entry
    /// that is not a positive number, nor where an exit would fall to zero.
    #[test]
    fn no_grid_or_no_room_is_no_bracket() {
        for tick in [0.0, -0.1, f64::NAN, f64::INFINITY] {
            assert_eq!(exits(1, 100.0, 0.5, 0.3, grid_value(tick)), None, "tick {tick}");
        }
        assert_eq!(exits(1, 0.0, 0.5, 0.3, Some(0.1)), None, "no entry");
        assert_eq!(exits(1, 100.0, 0.0, 0.3, Some(0.1)), None, "no take-profit distance");
        assert_eq!(exits(1, 100.0, 0.5, f64::NAN, Some(0.1)), None, "no stop distance");
        assert_eq!(exits(1, 1.0, 1.0, 150.0, Some(0.01)), None, "a buy's stop below zero");
        assert_eq!(exits(-1, 1.0, 150.0, 1.0, Some(0.01)), None, "a sell's target below zero");
    }

    #[test]
    fn a_size_prints_in_its_unit() {
        assert_eq!(size_text(0.01, TradeWindowSizingUnit::Base, None, 0.001), "0.010");
        assert_eq!(size_text(0.01, TradeWindowSizingUnit::Quote, Some(65_432.5), 0.001), "654.33");
        for price in [None, Some(0.0), Some(-1.0), Some(f64::NAN)] {
            assert_eq!(
                size_text(0.01, TradeWindowSizingUnit::Quote, price, 0.001),
                NO_QUOTE,
                "{price:?}"
            );
        }
        assert_eq!(
            base_quantity(NO_QUOTE, TradeWindowSizingUnit::Quote, Some(65_432.5), 0.001),
            None
        );
    }

    /// A base size printed in the quote currency and read back is the SAME size: never a lot
    /// less (the cent rounded the value down) and never more. Where a lot is worth under a cent,
    /// the value prints to more decimals.
    #[test]
    fn a_quote_size_round_trips_to_the_same_base() {
        for (base, price, lot) in [
            (0.010, 65_432.42, 0.001),
            (0.010, 65_432.5, 0.001),
            (0.001, 99_999.99, 0.001),
            (0.123, 0.98765, 0.001),
            (0.000_123_45, 65_432.42, 1e-8),
            (37.0, 0.5231, 1.0),
            (1_000.0, 0.000_012_34, 1.0),
        ] {
            let text = size_text(base, TradeWindowSizingUnit::Quote, Some(price), lot);
            let back = base_quantity(&text, TradeWindowSizingUnit::Quote, Some(price), lot);
            assert!(back.is_some_and(|b| close(b, base)), "{base} @ {price}: {text:?} -> {back:?}");
        }
        // 8.07763… rounded UP at four decimals: one lot here is worth 0.00065, under a cent.
        assert_eq!(
            size_text(0.000_123_45, TradeWindowSizingUnit::Quote, Some(65_432.42), 1e-8),
            "8.0777"
        );
    }
}
