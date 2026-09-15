//! Gate for `EngineParams::fee_schedule` (fee model follow-up 3: wires the previously-dead
//! `vike_model::FeeSchedule::maker_taker_rates()` bridge into `StrategyEngine`/`SimBroker`).
//!
//! Two properties:
//! 1. **`Some(schedule)` OVERRIDES the flat `fee_rate`/`maker_fee`/`taker_fee` precedence** —
//!    `SimBroker::maker_fee`/`taker_fee` are derived from the schedule's `maker_taker_rates()`
//!    fractions verbatim, not from the (conflicting, in this test) flat fields.
//! 2. **The derived pair actually reaches the fill** — a taker (market) fill and a maker
//!    (resting-limit) fill on the SAME price/size are charged the schedule's DISTINCT
//!    maker/taker rates, read off `SymbolState::entry_fee` (the opening fill's fee, folded
//!    verbatim into cost basis by `TradeFold`).
//!
//! `None` (the default) is exercised by every pre-existing `EngineParams` test in this crate —
//! this file only proves the NEW knob, not the frozen default.

use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_model::{Bar, FeeSchedule, Strategy};

const SYM: &str = "SYM";

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Two flat bars at 100: the first is the decision bar, the second is where the order fills
/// (next-open). A wide low/high window on bar 1 means a marketable limit fills at its own
/// price-vs-open floor, landing on the SAME price a market order would.
fn bars() -> Vec<Bar> {
    vec![bar(0, 100.0, 100.0, 100.0, 100.0), bar(1, 100.0, 101.0, 99.0, 100.0)]
}

struct MarketBuyOnce;
impl Strategy<SimBroker> for MarketBuyOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, 10.0, 0.0, true, None);
        }
    }
}

struct MarketableLimitBuyOnce;
impl Strategy<SimBroker> for MarketableLimitBuyOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            // Priced far above the fill bar's open, so it is guaranteed marketable and fills
            // at `price.min(open) == open` — the exact same price the market order gets.
            ctx.submit_limit(SYM, 1, 10.0, 1_000.0, 0.0, true, None);
        }
    }
}

/// `fee_schedule: Some` wins over `fee_rate`/`maker_fee`/`taker_fee` — set here to CONFLICTING
/// values so agreement with the schedule's own rates (not the flat fields) is unambiguous.
#[test]
fn fee_schedule_overrides_the_flat_rate_precedence() {
    let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 };
    let params = EngineParams {
        fee_rate: 0.5,
        maker_fee: Some(0.9),
        taker_fee: Some(0.9),
        fee_schedule: Some(schedule),
        ..Default::default()
    };
    let engine = StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, params);
    let (want_maker, want_taker) = schedule.maker_taker_rates();
    assert_eq!(engine.core.maker_fee, want_maker);
    assert_eq!(engine.core.taker_fee, want_taker);
    // sanity: the conflicting flat fields were NOT what won
    assert_ne!(engine.core.maker_fee, 0.9);
    assert_ne!(engine.core.taker_fee, 0.9);
}

/// `fee_schedule: None` (the default) is the frozen `maker_fee.unwrap_or(fee_rate)` /
/// `taker_fee.unwrap_or(fee_rate)` chain, untouched.
#[test]
fn no_fee_schedule_keeps_the_frozen_precedence() {
    let params = EngineParams { fee_rate: 0.5, maker_fee: Some(0.9), ..Default::default() };
    let engine = StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, params);
    assert_eq!(engine.core.maker_fee, 0.9); // maker_fee.unwrap_or(fee_rate)
    assert_eq!(engine.core.taker_fee, 0.5); // taker_fee (None) -> fee_rate
}

/// End-to-end: a taker (market) fill and a maker (resting-limit) fill at the IDENTICAL
/// price/qty are charged the schedule's DISTINCT rates — proving the derived pair actually
/// reaches `apply_fill`, not just `SimBroker`'s stored fields.
#[test]
fn maker_and_taker_fills_are_charged_their_distinct_schedule_rate() {
    let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 };
    let mk_params =
        || EngineParams { cash: 100_000.0, fee_schedule: Some(schedule), ..Default::default() };

    let mut market_engine =
        StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, mk_params());
    market_engine.run();
    let si = market_engine.core.symbols.iter().position(|s| s == SYM).expect("registered");
    let market_pos = market_engine.core.sym[si].pos;
    assert_eq!(market_pos.size, 10.0, "market buy must have filled");
    let market_fee = market_engine.core.sym[si].entry_fee;

    let mut limit_engine =
        StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketableLimitBuyOnce, mk_params());
    limit_engine.run();
    let limit_pos = limit_engine.core.sym[si].pos;
    assert_eq!(limit_pos.size, 10.0, "marketable limit buy must have filled");
    let limit_fee = limit_engine.core.sym[si].entry_fee;

    // both filled at the same price/qty (bar 1's open, 100.0) — only the maker/taker
    // classification differs, so the fee difference isolates the rate.
    assert_eq!(market_pos.avg_price, limit_pos.avg_price);
    assert_eq!(market_fee, schedule.commission(false, 10.0, 100.0));
    assert_eq!(limit_fee, schedule.commission(true, 10.0, 100.0));
    assert_ne!(market_fee, limit_fee, "maker/taker rates differ — their fees must too");
}
