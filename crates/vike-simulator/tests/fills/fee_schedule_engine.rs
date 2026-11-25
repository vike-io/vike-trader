//! Gate for `StrategyEngineConfig::fee_schedule` (fee model follow-up 3: wires the previously-dead
//! `vike_model::FeeSchedule::maker_taker_rates()` bridge into `StrategyEngine`/`OrderBrokerSimulator`).
//!
//! Three properties:
//! 1. **`Some(schedule)` OVERRIDES the flat `fee_rate`/`maker_fee`/`taker_fee` precedence** —
//!    `OrderBrokerSimulator::maker_fee`/`taker_fee` are derived from the schedule's `maker_taker_rates()`
//!    fractions verbatim, not from the (conflicting, in this test) flat fields.
//! 2. **The derived pair actually reaches the fill** — a taker (market) fill and a maker
//!    (resting-limit) fill on the SAME price/size are charged the schedule's DISTINCT
//!    maker/taker rates, read off `OrderBrokerSimulatorSymbolState::entry_fee` (the opening fill's fee, folded
//!    verbatim into cost basis by `TradeFold`).
//! 3. **A shape that CANNOT be flattened is charged exactly instead of charged nothing** — the
//!    per-share-with-floor (IBKR equities) schedule, whose `maker_taker_rates()` answers
//!    `(0.0, 0.0)`. `StrategyEngine::new` used to flatten every shape but the prediction-market
//!    curve, so arming this one priced every fill at ZERO while the config read as fee-modelled;
//!    that routing is now inverted (the two FLAT shapes are named, everything else is exact) and
//!    this is the test that fails if it ever goes back.
//!
//! `None` (the default) is exercised by every pre-existing `StrategyEngineConfig` test in this crate —
//! this file only proves the NEW knob, not the frozen default.

use vike_marketdata::Bar;
use vike_model::{FeeSchedule, OrderBroker, OrderDrop, OrderPlacement, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM";

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    vike_simulator::test_support::bar(ts, open, high, low, close, 0.0)
}

/// Two flat bars at 100: the first is the decision bar, the second is where the order fills
/// (next-open). A wide low/high window on bar 1 means a marketable limit fills at its own
/// price-vs-open floor, landing on the SAME price a market order would.
fn bars() -> Vec<Bar> {
    vec![bar(0, 100.0, 100.0, 100.0, 100.0), bar(1, 100.0, 101.0, 99.0, 100.0)]
}

struct MarketBuyOnce;
impl Strategy<OrderBrokerSimulator> for MarketBuyOnce {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, 10.0, 0.0, true, None);
        }
    }
}

struct MarketableLimitBuyOnce;
impl Strategy<OrderBrokerSimulator> for MarketableLimitBuyOnce {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
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
    let params = StrategyEngineConfig {
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
    let params = StrategyEngineConfig { fee_rate: 0.5, maker_fee: Some(0.9), ..Default::default() };
    let engine = StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, params);
    assert_eq!(engine.core.maker_fee, 0.9); // maker_fee.unwrap_or(fee_rate)
    assert_eq!(engine.core.taker_fee, 0.5); // taker_fee (None) -> fee_rate
}

/// End-to-end: a taker (market) fill and a maker (resting-limit) fill at the IDENTICAL
/// price/qty are charged the schedule's DISTINCT rates — proving the derived pair actually
/// reaches `apply_fill`, not just `OrderBrokerSimulator`'s stored fields.
#[test]
fn maker_and_taker_fills_are_charged_their_distinct_schedule_rate() {
    let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 };
    let mk_params = || StrategyEngineConfig {
        cash: 100_000.0,
        fee_schedule: Some(schedule),
        ..Default::default()
    };

    let mut market_engine =
        StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, mk_params());
    market_engine.run();
    let si = market_engine.core.symbols.iter().position(|s| s == SYM).expect("registered");
    let market_pos = market_engine.core.symbol[si].pos;
    assert_eq!(market_pos.size, 10.0, "market buy must have filled");
    let market_fee = market_engine.core.symbol[si].entry_fee;

    let mut limit_engine =
        StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketableLimitBuyOnce, mk_params());
    limit_engine.run();
    let limit_pos = limit_engine.core.symbol[si].pos;
    assert_eq!(limit_pos.size, 10.0, "marketable limit buy must have filled");
    let limit_fee = limit_engine.core.symbol[si].entry_fee;

    // both filled at the same price/qty (bar 1's open, 100.0) — only the maker/taker
    // classification differs, so the fee difference isolates the rate.
    assert_eq!(market_pos.average_price, limit_pos.average_price);
    assert_eq!(market_fee, schedule.commission(false, 10.0, 100.0));
    assert_eq!(limit_fee, schedule.commission(true, 10.0, 100.0));
    assert_ne!(market_fee, limit_fee, "maker/taker rates differ — their fees must too");
}

/// The per-share-with-floor shape reaches the fill EXACTLY, and the number is the FLOOR.
///
/// 10 shares × `$0.005` is `$0.05` of per-share fee, under a `$1.00` per-order minimum and a
/// `0.5 %`-of-notional cap (`$5.00` here), so the order pays `$1.00`. That is the whole point of
/// the shape: the cost does NOT scale to zero with size, which is what makes a small-size
/// high-frequency configuration lose to the broker minimum live after backtesting as viable.
///
/// ⚠ **Under the routing this replaces the number was `0.0`.** `maker_taker_rates()` answers
/// `(0.0, 0.0)` for this shape, and `StrategyEngine::new` flattened every schedule but the
/// prediction-market curve through it — so an armed IBKR schedule charged nothing at all while
/// `StrategyEngineConfig::fee_schedule` was visibly set. The zero assertion below is the regression
/// guard, not decoration.
#[test]
fn a_per_share_floor_schedule_charges_the_floor_rather_than_flattening_to_zero() {
    let schedule =
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.005 };
    // The flattened pair this shape has no honest value for — the number the old routing used.
    assert_eq!(schedule.maker_taker_rates(), (0.0, 0.0));

    let params = StrategyEngineConfig {
        cash: 100_000.0,
        fee_schedule: Some(schedule),
        ..Default::default()
    };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, params);
    engine.run();
    let si = engine.core.symbols.iter().position(|s| s == SYM).expect("registered");
    assert_eq!(engine.core.symbol[si].pos.size, 10.0, "market buy must have filled");

    let fee = engine.core.symbol[si].entry_fee;
    assert_eq!(fee, schedule.commission(false, 10.0, 100.0), "the exact schedule reaches the fill");
    assert_eq!(fee, 1.0, "the $1.00 per-order minimum, not the $0.05 of per-share fee");
    assert_ne!(fee, 0.0, "a flattened (0.0, 0.0) would have charged nothing");
}

/// ⚠ **The cash gate counts the fee the fill will charge.** `maker_taker_rates()` answers
/// `(0.0, 0.0)` for this shape, so a pre-check on the flat rates counted no fee at all: 10 × 100
/// fits 1 000 of cash exactly, the order passed, and the `$1.00` minimum took cash to -1.0.
///
/// NON-VACUOUS: one dollar more of cash and the same order fills whole and leaves exactly nothing.
#[test]
fn the_cash_gate_counts_the_fee_the_fill_will_charge() {
    let schedule = FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.0 };
    let run = |cash: f64| {
        let params = StrategyEngineConfig {
            cash,
            cash_gate: true,
            fee_schedule: Some(schedule),
            ..Default::default()
        };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars())], MarketBuyOnce, params);
        e.run();
        (e.core.cash, e.core.symbol[0].pos.size, e.core.dropped.clone())
    };

    let (cash, pos, dropped) = run(1_000.0);
    assert_eq!(cash, 1_000.0, "the last order must not overdraw by the fee");
    assert_eq!(pos, 0.0, "it is dropped whole");
    assert_eq!(
        dropped,
        vec![OrderDrop {
            symbol: SYM.to_string(),
            reason: "market".to_string(),
            size: 10.0,
            weight: 0.0
        }]
    );

    let (cash, pos, dropped) = run(1_001.0);
    assert_eq!(cash, 0.0, "notional 1 000 plus the fee 1");
    assert_eq!(pos, 10.0);
    assert!(dropped.is_empty(), "{dropped:?}");
}

/// IBKR's stock minimum ($1.00) and FX minimum ($2.00) are per ORDER.
const IBKR_STOCK: FeeSchedule =
    FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.01 };
const IBKR_FX: FeeSchedule = FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 };

/// Rests one buy limit of 100 at 99 on the first book it sees; records each fill's size and fee.
#[derive(Default)]
struct LimitOnBook {
    sent: bool,
    fills: Vec<(f64, f64)>,
}
impl Strategy<OrderBrokerSimulator> for LimitOnBook {
    fn on_order_book(&mut self, ctx: &mut OrderBrokerSimulator, _book: &vike_marketdata::L2Book) {
        if !std::mem::replace(&mut self.sent, true) {
            OrderBroker::place(
                ctx,
                SYM,
                "",
                1,
                100.0,
                OrderPlacement::Limit { price: 99.0 },
                false,
            );
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, f: &vike_model::Fill) {
        self.fills.push((f.size, f.fee));
    }
}

/// A book of 5 bid at 99 (the queue ahead of a bid there), then a trade at 99 of each size, from ts 2.
fn queue_tape(trades: &[f64]) -> Vec<vike_simulator::TickEvent> {
    let book = vike_marketdata::L2BookUpdate {
        timestamp: 1,
        local_timestamp: 0,
        sequence: 1,
        kind: vike_marketdata::L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![vike_marketdata::L2BookLevel::new(99.0, 5.0)],
        asks: vec![vike_marketdata::L2BookLevel::new(101.0, 5.0)],
        symbol: SYM.to_string(),
    };
    let trade = |(i, size): (usize, &f64)| {
        vike_simulator::test_support::trade(SYM, 2 + i as i64, 99.0, *size)
    };
    std::iter::once(vike_simulator::TickEvent::Book(Box::new(book)))
        .chain(trades.iter().enumerate().map(trade))
        .collect()
}

/// **A per-order minimum is charged once however many pieces fill the order.** The queue model fills
/// the limit of 100 in three pieces, 33, 33 and 34 (each trade's excess over the 5 resting ahead), and
/// the order pays the schedule on 100 shares: 0.50 of per-share fee is under the $1.00 minimum, so
/// $1.00 in all, on the first piece. The FX row likewise pays its $2.00 once. Base: each piece paid the
/// minimum, $3.00 and $6.00.
#[test]
fn an_order_filled_in_pieces_pays_the_per_order_minimum_once() {
    let tape = queue_tape(&[38.0, 33.0, 34.0]);
    for (schedule, minimum) in [(IBKR_STOCK, 1.0), (IBKR_FX, 2.0)] {
        let params = StrategyEngineConfig {
            cash: 100_000.0,
            fee_schedule: Some(schedule),
            queue_model: Some(vike_simulator::QueueModelKind::RiskAverse),
            ..Default::default()
        };
        let series = vec![(SYM.to_string(), Vec::new())];
        let mut e = StrategyEngine::new(series, LimitOnBook::default(), params);
        e.run_ticks(&[(SYM.to_string(), tape.clone())]);
        let fills = &e.strategy.fills;
        assert_eq!(
            fills.iter().map(|f| f.0).collect::<Vec<_>>(),
            [33.0, 33.0, 34.0],
            "{schedule:?}"
        );
        assert_eq!(fills.iter().map(|f| f.1).collect::<Vec<_>>(), [minimum, 0.0, 0.0]);
        assert_eq!(e.core.fees_paid, schedule.commission(false, 100.0, 99.0), "{schedule:?}");
    }
}

/// Quotes a tagged bid of 100 at 99 on the first book; on its first fill amends it UP to 100 again.
#[derive(Default)]
struct AmendedQuote {
    sent: bool,
    amended: bool,
    fees: Vec<f64>,
}
impl Strategy<OrderBrokerSimulator> for AmendedQuote {
    fn on_order_book(&mut self, ctx: &mut OrderBrokerSimulator, _book: &vike_marketdata::L2Book) {
        if !std::mem::replace(&mut self.sent, true) {
            OrderBroker::place(
                ctx,
                "",
                "bid",
                1,
                100.0,
                OrderPlacement::Limit { price: 99.0 },
                false,
            );
        }
    }
    fn on_fill(&mut self, ctx: &mut OrderBrokerSimulator, f: &vike_model::Fill) {
        self.fees.push(f.fee);
        if !std::mem::replace(&mut self.amended, true) {
            OrderBroker::amend(ctx, "bid", Some(100.0), None, None);
        }
    }
}

/// **An amended quote is the same order and pays the minimum once.** The queue model fills 33 of
/// the tagged bid (38 traded over the 5 ahead); the maker amends the 67 left UP to 100, which costs
/// it its queue place (`OrderBrokerSimulator::amend_queue_id` gives it a new identity) but not the order: the 100
/// filled next pays nothing more, $1.00 in all for 133 shares. Head before the fix: $2.00, the
/// amended quote charged as a new order.
#[test]
fn an_amended_quote_pays_the_per_order_minimum_once() {
    let params = StrategyEngineConfig {
        cash: 100_000.0,
        fee_schedule: Some(IBKR_STOCK),
        queue_model: Some(vike_simulator::QueueModelKind::RiskAverse),
        optimistic_quotes: true,
        ..Default::default()
    };
    let series = vec![(SYM.to_string(), Vec::new())];
    let mut e = StrategyEngine::new(series, AmendedQuote::default(), params);
    e.run_ticks(&[(SYM.to_string(), queue_tape(&[38.0, 105.0]))]);
    assert_eq!(e.core.symbol[0].pos.size, 133.0, "33, then the amended 100");
    assert_eq!(e.strategy.fees, [1.0, 0.0]);
    assert_eq!(e.core.fees_paid, 1.0);
}

/// Buys 100 as a market-to-limit once, on its first bar.
struct MarketToLimitBuyOnce;
impl Strategy<OrderBrokerSimulator> for MarketToLimitBuyOnce {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit_market_to_limit(SYM, 1, 100.0, 0.0, true);
        }
    }
}

/// The market-to-limit's remainder is the SAME order: the volume cap fills 50 at the first fill bar's
/// open and rests 50 as the `Limit` there, which the next bar fills. The order pays the $1.00 minimum
/// once (base: $2.00, once per piece).
#[test]
fn a_market_to_limit_remainder_pays_no_second_minimum() {
    let bar = |ts, low| vike_simulator::test_support::bar(ts, 100.0, 100.0, low, 100.0, 50.0);
    let params = StrategyEngineConfig {
        cash: 100_000.0,
        fee_schedule: Some(IBKR_STOCK),
        volume_limit: Some(1.0),
        ..Default::default()
    };
    let series = vec![(SYM.to_string(), vec![bar(0, 100.0), bar(1, 100.0), bar(2, 99.0)])];
    let mut e = StrategyEngine::new(series, MarketToLimitBuyOnce, params);
    e.run();
    assert_eq!(e.core.symbol[0].pos.size, 100.0, "two pieces of 50");
    assert_eq!((e.core.taker_fills, e.core.maker_fills), (1, 1));
    assert_eq!(e.core.fees_paid, 1.0);
}

/// Buys `.1` of `.0` once, on its own first bar (a multi-symbol run calls `on_bar` per symbol).
struct BuyOnce(&'static str, f64);
impl Strategy<OrderBrokerSimulator> for BuyOnce {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        if ctx.index == 0 && bar.symbol.as_deref() == Some(self.0) {
            ctx.submit(self.0, 1, self.1, 0.0, true, None);
        }
    }
}

/// An option of a `PercentOfUnderlying` run that lists its underlying is charged on the UNDERLYING,
/// `min(3 bps × underlying, 12.5 % × premium)`. The numbers are a measured deribit demo fill: 0.01 at a
/// premium of 1,900 paid 0.2318 USDC, 3 bps of 0.01 × 77,266.67; the premium reading this replaces
/// charges 0.0057, 41 times less. A cheap option pays the premium cap, and the underlying, which is no
/// option, keeps the plain schedule.
#[test]
fn an_option_listing_its_underlying_is_charged_on_it() {
    const OPT: &str = "OPT";
    const UND: &str = "UND";
    let schedule = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 };
    let flat = |px: f64| vec![bar(0, px, px, px, px), bar(1, px, px, px, px)];
    let run = |buy: &'static str, premium: f64, fee_underlyings: Vec<(String, String)>| {
        let params = StrategyEngineConfig {
            cash: 1_000_000.0,
            fee_schedule: Some(schedule),
            fee_underlyings,
            ..Default::default()
        };
        let series = vec![(OPT.to_string(), flat(premium)), (UND.to_string(), flat(77_266.67))];
        let mut e = StrategyEngine::new(series, BuyOnce(buy, 0.01), params);
        e.run();
        let si = e.core.symbols.iter().position(|s| s == buy).expect("registered");
        assert_eq!(e.core.symbol[si].pos.size, 0.01, "{buy} must have filled");
        e.core.symbol[si].entry_fee
    };
    let listed = || vec![(OPT.to_string(), UND.to_string())];

    let fee = run(OPT, 1_900.0, listed());
    assert_eq!(fee, schedule.commission_with_underlying(0.01, 1_900.0, 77_266.67));
    assert!((fee - 0.2318).abs() < 1e-6, "the venue's 0.2318: {fee}");
    let premium_only = run(OPT, 1_900.0, Vec::new());
    assert!((premium_only - 0.0057).abs() < 1e-12, "unlisted: the premium reading, {premium_only}");
    let cheap = run(OPT, 10.0, listed());
    assert_eq!(cheap, schedule.commission_with_underlying(0.01, 10.0, 77_266.67));
    assert!((cheap - 0.0125).abs() < 1e-12, "12.5 % of a 0.10 premium: {cheap}");
    let underlying = run(UND, 1_900.0, listed());
    assert_eq!(underlying, schedule.commission(false, 0.01, 77_266.67), "no option, no change");
}

/// **Each symbol pays its own row** (`StrategyEngineConfig::fee_schedules`): a run over a spot pair and its
/// perpetual charges the spot fill at the spot row and the perp fill at the perp row, where one
/// run-wide schedule charged both the same. Listing the run-wide schedule for every symbol changes
/// no bit, which is what keeps a single-lane run identical.
#[test]
fn each_symbol_pays_its_own_schedule() {
    let spot = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let perp = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 };
    let flat = |px: f64| vec![bar(0, px, px, px, px), bar(1, px, px, px, px)];
    let run = |buy: &'static str, fee_schedules: Vec<(String, FeeSchedule)>| {
        let params = StrategyEngineConfig {
            cash: 1_000_000.0,
            fee_schedule: Some(spot),
            fee_schedules,
            ..Default::default()
        };
        let series = vec![("SPOT".to_string(), flat(100.0)), ("PERP".to_string(), flat(100.0))];
        let mut e = StrategyEngine::new(series, BuyOnce(buy, 10.0), params);
        e.run();
        let si = e.core.symbols.iter().position(|s| s == buy).expect("registered");
        assert_eq!(e.core.symbol[si].pos.size, 10.0, "{buy} must have filled");
        e.core.symbol[si].entry_fee
    };
    let own = || vec![("SPOT".to_string(), spot), ("PERP".to_string(), perp)];
    assert_eq!(run("SPOT", own()), spot.commission(false, 10.0, 100.0));
    assert_eq!(run("PERP", own()), perp.commission(false, 10.0, 100.0));
    assert_ne!(run("SPOT", own()), run("PERP", own()), "two rows, two fees");
    let same = vec![("SPOT".to_string(), spot), ("PERP".to_string(), spot)];
    for buy in ["SPOT", "PERP"] {
        assert_eq!(run(buy, same.clone()).to_bits(), run(buy, Vec::new()).to_bits(), "{buy}");
    }
}
