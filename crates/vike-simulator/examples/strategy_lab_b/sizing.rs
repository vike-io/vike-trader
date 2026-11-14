//! The reference's quantity step on a row's market, which a run states (`desk::Terms::qty_step`, the rule reading it through
//! `vike_tactics::TacticsSizing`), and how the reference margin-calls a position. The symbol's quantity step is the run's market's
//! (`markets.rs`): BTCUSDT's is 0.00001. How a size floors to it is the gallery's (`vike_tactics::sizing`, and
//! `vike_tactics::TacticsSizing::size_for_share` / `size_for_cash` for a size from money).

use crate::markets;
use vike_simulator::ShortfallCover;

/// Lots per unit of the base asset on the run's market: a size floors to the symbol's quantity step, 1 / this.
fn lots_per_unit() -> f64 {
    10f64.powi(markets::current().market.step_decimals as i32)
}

/// The multiple of the shortfall the reference's margin call closes.
const COVER: f64 = 4.0;

/// The run's market's quantity step, which a run states (`desk::Terms::qty_step`). The control `unrounded sizes` in `main.rs`
/// runs without it.
pub fn qty_step() -> f64 {
    1.0 / lots_per_unit()
}

pub fn self_check() {
    assert_eq!(qty_step(), 0.00001, "a run is on BTCUSDT until `markets::set` says otherwise");
    // A symbol with another step: DOGEUSDT trades whole coins, ETHUSDT in 0.0001.
    let on = |market| markets::set(markets::Chart { market, ..markets::Chart::BTC_30M });
    on(markets::DOGEUSDT);
    assert_eq!(qty_step(), 1.0);
    on(markets::ETHUSDT);
    assert_eq!(margin_call(100.0, 100.0).map(|c| c.step), Some(0.0001));
    markets::set(markets::Chart::BTC_30M);
}

/// The reference's margin call at the margins a rule states, in percent of a position's value: four times the shortfall,
/// floored to the quantity step, cut at the bar's open or adverse extreme, and an entry the account cannot margin refused.
pub fn margin_call(long_percent: f64, short_percent: f64) -> Option<ShortfallCover> {
    Some(ShortfallCover {
        cover: COVER,
        margin_long: long_percent / 100.0,
        margin_short: short_percent / 100.0,
        step: 1.0 / lots_per_unit(),
    })
}
