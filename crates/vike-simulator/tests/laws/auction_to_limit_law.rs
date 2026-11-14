//! The opening auction's market-to-limit, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_market_to_limit_at_open`]
//! (`market_to_limit` at `at-open`, IB `MTL` + `AUC`) driven through the fill lanes, over the ONE law
//! `vike_model::order_fill_price` states for `OrderKind::AuctionOpenToLimit` (decision 0267, 2.8).
//!
//! It is the market opening-auction order of `auction_law.rs` (held until the open of the symbol's
//! next session, filled there as a market order, a TAKER), except for what the lane's volume cap
//! leaves of it: that rests IN PLACE as the plain `Limit` at the price the auction booked, a MAKER from
//! then on, and an auction that fills none of it rests it whole as the `Limit` at that open. Assumption:
//! IB re-submits "at the COP or the best bid/ask after the market opens"; this takes the open. Pinned
//! here, on the US regular session (`vike_venues::session::US_EQUITY_REGULAR`):
//!
//! 1. no cap: exactly the market opening-auction order, on bars, sub-bars, the tick lane and the L2
//!    lane (the auction clears at the traded price, never by walking the book);
//! 2. the volume cap: the remainder rests as the `Limit` at the booked price (a fixed slippage
//!    included), never dropped; an auction that fills none of it rests it whole at the open; a market
//!    opening-auction order loses the same units to `volume_cap`;
//! 3. a symbol with no auction and a price condition are refused in words, as the auction verbs refuse.
//!
//! The helpers are `stop_limit_law.rs`'s; the tape and the calendar are `auction_law.rs`'s.

use indexmap::IndexMap;
use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
use vike_simulator::test_support::ms;
use vike_simulator::{FillModelKind, StrategyEngine, StrategyEngineConfig};
use vike_venues::session::{CRYPTO_24_7, US_EQUITY_REGULAR};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars,
};

/// 2024-01-`day` (Monday the 8th) at `h:m` UTC.
fn at(day: u32, h: i64, m: i64) -> i64 {
    ms(2024, 1, day, h) + m * 60_000
}

/// The gate on, the symbol on `calendar`.
fn session(calendar: vike_venues::SessionCalendar) -> StrategyEngineConfig {
    let mut session_calendars = IndexMap::new();
    session_calendars.insert(SYM.to_string(), calendar);
    StrategyEngineConfig { session_gate: true, session_calendars, ..params() }
}

fn us() -> StrategyEngineConfig {
    session(US_EQUITY_REGULAR)
}

/// One unit per bar of 100.
fn capped() -> StrategyEngineConfig {
    StrategyEngineConfig { volume_limit: Some(0.01), ..us() }
}

/// `auction_law.rs`'s tape: Monday from 14:00, Tuesday (its first bar, b3, the opening auction at
/// 103; a dip to 97.6 at b4; a high of 104.8 at b5), and Wednesday's first bar. `auction_volume` is
/// b3's volume and `after_volume` b4's (the helper's 100 elsewhere).
fn tape_with(auction_volume: f64, after_volume: f64) -> Vec<Bar> {
    let b = vike_simulator::test_support::bar;
    vec![
        bar(at(8, 14, 0), FLAT),
        bar(at(8, 15, 0), [101.0, 101.8, 100.6, 101.5]),
        bar(at(8, 20, 0), [102.0, 102.8, 101.6, 102.5]),
        b(at(9, 13, 30), 103.0, 103.8, 102.6, 103.5, auction_volume),
        b(at(9, 15, 0), 98.0, 98.8, 97.6, 98.5, after_volume),
        bar(at(9, 20, 0), [104.0, 104.8, 100.6, 101.5]),
        bar(at(10, 13, 30), [105.0, 105.8, 104.6, 105.5]),
    ]
}

fn tape() -> Vec<Bar> {
    tape_with(100.0, 100.0)
}

fn mtl_open(side: i32, size: f64) -> Once {
    Once::new(move |b| b.submit_market_to_limit_at_open(SYM, side, size, 0.0, true))
}

fn moo(side: i32, size: f64) -> Once {
    Once::new(move |b| b.submit_at_open(SYM, side, size, None, 0.0, true))
}

/// The run's `dropped` list as (reason, size).
fn dropped(engine: &StrategyEngine<Once>) -> Vec<(String, f64)> {
    engine.core.dropped.iter().map(|d| (d.reason.clone(), d.size)).collect()
}

// ---- 1. no cap: the market opening-auction order ----

/// Held until Tuesday's open, filled whole there, a taker, exactly as the market at-open order is.
#[test]
fn with_no_cap_it_is_the_market_opening_auction_order_on_bars() {
    let held = run_bars(us(), tape().into_iter().take(3).collect(), mtl_open(1, 2.0));
    let rows: Vec<_> = held.core.pending_of(SYM).iter().map(|o| (o.kind, o.release)).collect();
    let tue_open = OrderRelease { at: at(9, 13, 30), price: None };
    assert_eq!(rows, [(OrderKind::AuctionOpenToLimit, Some(tue_open))]);
    for side in [1, -1] {
        let a = run_bars(us(), tape(), mtl_open(side, 2.0));
        assert_eq!(a.strategy.fills.len(), 1, "{side}: {:?}", a.strategy.fills);
        assert_fill("Tuesday's open", &a.strategy.fills[0], at(9, 13, 30), 103.0, false);
        assert_eq!(
            a.strategy.fills,
            run_bars(us(), tape(), moo(side, 2.0)).strategy.fills,
            "{side}"
        );
        assert!(resting(&a).is_empty() && a.core.dropped.is_empty(), "{side}");
    }
}

/// On sub-bars the opening auction is the FIRST sub-bar's open; the tick lane fills it at the session's
/// first tick, at the price that tick trades (the quote's mid); the L2 lane clears it there too, never
/// at the book's far ask of 110. Each is the market at-open order's fill.
#[test]
fn with_no_cap_it_is_the_market_opening_auction_order_on_sub_bars_ticks_and_l2() {
    let subs = vec![
        vike_simulator::test_support::bar(at(9, 13, 30), 103.2, 103.5, 102.8, 103.0, 50.0),
        vike_simulator::test_support::bar(at(9, 14, 0), 103.0, 103.8, 102.6, 103.5, 50.0),
    ];
    let granular =
        || StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs.clone())], ..us() };
    let a = run_bars(granular(), tape(), mtl_open(1, 1.0));
    assert_eq!(a.strategy.fills.len(), 1, "{:?}", a.strategy.fills);
    assert_fill("the first sub-bar's open", &a.strategy.fills[0], at(9, 13, 30), 103.2, false);
    assert_eq!(a.strategy.fills, run_bars(granular(), tape(), moo(1, 1.0)).strategy.fills);

    let quotes = vec![
        q(at(8, 14, 0), 99.0, 100.0),
        q(at(8, 20, 0), 100.0, 101.0),
        q(at(9, 13, 30), 102.0, 103.0),
        q(at(9, 15, 0), 97.0, 98.0),
    ];
    for fill_model in [FillModelKind::Tick, FillModelKind::L2Book] {
        let ticks = || {
            let mut t = vec![book(at(8, 13, 0), &[(99.0, 5.0)], &[(110.0, 5.0)])];
            t.extend(quotes.clone());
            t
        };
        let run = |strategy: Once| {
            let p = StrategyEngineConfig { fill_model, ..us() };
            let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strategy, p);
            engine.run_ticks(&[(SYM.to_string(), ticks())]);
            engine
        };
        let a = run(mtl_open(1, 1.0));
        assert_eq!(a.strategy.fills.len(), 1, "{fill_model:?}: {:?}", a.strategy.fills);
        assert_fill("the first tick's mid", &a.strategy.fills[0], at(9, 13, 30), 102.5, false);
        assert_eq!(a.strategy.fills, run(moo(1, 1.0)).strategy.fills, "{fill_model:?}");
    }
}

// ---- 2. the volume cap ----

/// The auction lets one of two units through, a taker at 103; the other rests as the `Limit` at 103,
/// not dropped. A buy's `Limit` fills on the dip after it (the open 98, a maker); a sell's on the bar
/// that comes back above it (the open 104). A market at-open order loses that unit to `volume_cap`.
#[test]
fn the_capped_remainder_rests_as_the_limit_at_the_auctions_price() {
    let rested = run_bars(capped(), tape().into_iter().take(4).collect(), mtl_open(1, 2.0));
    assert_eq!(rested.strategy.fills.len(), 1, "{:?}", rested.strategy.fills);
    assert_eq!(rested.strategy.fills[0].size, 1.0);
    assert_eq!(resting(&rested), vec![(OrderKind::Limit, Some(103.0), None)]);
    assert!(rested.core.dropped.is_empty(), "{:?}", rested.core.dropped);

    for (side, ts, px) in [(1, at(9, 15, 0), 98.0), (-1, at(9, 20, 0), 104.0)] {
        let engine = run_bars(capped(), tape(), mtl_open(side, 2.0));
        let fills = &engine.strategy.fills;
        assert_eq!(fills.len(), 2, "{side}: {fills:?}");
        assert_fill("the auction", &fills[0], at(9, 13, 30), 103.0, false);
        assert_fill("the Limit at 103", &fills[1], ts, px, true);
        assert_eq!((fills[0].size, fills[1].size), (1.0, 1.0));
        assert!(engine.core.dropped.is_empty() && resting(&engine).is_empty(), "{side}");
    }

    let market = run_bars(capped(), tape(), moo(1, 2.0));
    assert_eq!(market.strategy.fills.len(), 1, "{:?}", market.strategy.fills);
    assert_eq!(dropped(&market), [("volume_cap".to_string(), 1.0)]);
}

/// The limit is the price the auction BOOKED: with a fixed slippage the taker fill pays it (the order
/// is not limit-bounded at the auction) and the `Limit` rests there, then fills paying none.
#[test]
fn the_limit_is_the_booked_price_of_the_auction_fill() {
    let p = StrategyEngineConfig { slippage_amount: AMOUNT, ..capped() };
    let engine = run_bars(p, tape(), mtl_open(1, 2.0));
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_fill("the auction plus the amount", &fills[0], at(9, 13, 30), 103.0 + AMOUNT, false);
    assert_fill("the Limit's gap improvement, no amount", &fills[1], at(9, 15, 0), 98.0, true);
}

/// An auction bar the cap lets nothing through (no volume) fills none of it: the auction is over, so
/// the whole order rests as the `Limit` at the auction's open, and fills on the dip, a maker. A market
/// at-open order loses both units to `volume_cap` there.
#[test]
fn an_auction_that_fills_none_of_it_rests_it_whole_at_the_open() {
    let quiet = || tape_with(0.0, 1_000.0);
    let rested = run_bars(capped(), quiet().into_iter().take(4).collect(), mtl_open(1, 2.0));
    assert!(rested.strategy.fills.is_empty(), "{:?}", rested.strategy.fills);
    assert_eq!(resting(&rested), vec![(OrderKind::Limit, Some(103.0), None)]);
    assert_eq!(rested.core.pending_of(SYM)[0].size, 2.0, "whole");
    assert!(rested.core.dropped.is_empty(), "{:?}", rested.core.dropped);

    let filled = run_bars(capped(), quiet(), mtl_open(1, 2.0));
    assert_eq!(filled.strategy.fills.len(), 1, "{:?}", filled.strategy.fills);
    assert_fill("the dip, whole", &filled.strategy.fills[0], at(9, 15, 0), 98.0, true);
    assert_eq!(filled.strategy.fills[0].size, 2.0);

    let market = run_bars(capped(), quiet(), moo(1, 2.0));
    assert!(market.strategy.fills.is_empty(), "{:?}", market.strategy.fills);
    assert_eq!(dropped(&market), [("volume_cap".to_string(), 2.0)]);
}

// ---- 3. the intake ----

/// No calendar and a 24/7 calendar have no auction, and an opening auction takes a time release, not
/// a price condition: each refused in the auction verbs' words, and nothing rests.
#[test]
fn a_symbol_with_no_auction_and_a_price_condition_are_refused_in_words() {
    let no_auction = format!(
        "time in force `at-open`: {SYM} has no auction (no session calendar, or one that never closes)"
    );
    for (case, p) in [("the gate off", params()), ("24/7", session(CRYPTO_24_7))] {
        let engine = run_bars(p, tape(), mtl_open(1, 1.0));
        assert_eq!(dropped(&engine), [(no_auction.clone(), 1.0)], "{case}");
        assert!(
            engine.strategy.fills.is_empty() && engine.core.pending_of(SYM).is_empty(),
            "{case}"
        );
    }
    let level = OrderReleasePrice {
        symbol: SYM.into(),
        level: 103.0,
        side: OrderReleasePriceSide::Above,
        venue: None,
    };
    let conditional = OrderRelease { at: at(9, 14, 0), price: Some(level) };
    let once = Once::new(move |b| {
        b.submit_released(conditional, |b| b.submit_market_to_limit_at_open(SYM, 1, 1.0, 0.0, true))
    });
    let engine = run_bars(us(), tape(), once);
    let why = "time in force `at-open` takes a time release, not a price condition";
    assert_eq!(dropped(&engine), [(why.to_string(), 1.0)]);
    assert!(engine.strategy.fills.is_empty() && engine.core.pending_of(SYM).is_empty());
}
