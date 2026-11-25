//! The auction law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_at_open`] and
//! [`vike_simulator::OrderBrokerSimulator::submit_at_close`] (`TimeInForce::AtOpen` / `AtClose`) driven through the
//! fill lanes, over the ONE law `vike_model::order_fill_price` states for `OrderKind::AuctionOpen`
//! and `OrderKind::AuctionClose`.
//!
//! An auction order takes part ONCE, in the auction its symbol's session calendar names: the open
//! of the first bar of the NEXT session, or the close of the LAST bar of the first session that
//! closes while it is live. A market one fills at that price; a limit one when the price is no worse
//! than its limit, and otherwise the auction is over and the order ENDS (`auction_passed`), never
//! resting for a later price. Every fill is a taker. Pinned here, on the US regular session
//! (`vike_venues::session::US_EQUITY_REGULAR`, 13:30-21:00 UTC under the module's DST union):
//!
//! 1. every case on coarse bars, and `MarketClose` beside it (the close of the NEXT bar, whichever);
//!    a limit auction order is limit-bounded, so the fixed slippage never pushes it past its limit;
//! 2. the auction instant of an opening auction rides `OrderResting::release`;
//! 3. sub-bars: the closing auction is the LAST sub-bar's close, never an earlier one's;
//! 4. the tick lane: an opening auction at the session's first tick; a closing auction ended in
//!    words (a tape cannot name a session's last tick in time);
//! 5. a release (`OrderBrokerSimulator::submit_released`) moves the auction to the first one after it, and a
//!    price condition (a conditional release) is refused;
//! 6. a symbol with no calendar, or a 24/7 one, has no auction: refused in words.
//!
//! The helpers are `stop_limit_law.rs`'s.

use indexmap::IndexMap;
use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
use vike_simulator::test_support::ms;
use vike_simulator::{FillModelKind, StrategyEngine, StrategyEngineConfig};
use vike_venues::session::{CRYPTO_24_7, US_EQUITY_REGULAR};

use crate::stop_limit_law::{AMOUNT, FLAT, Once, SYM, assert_fill, bar, params, q, run_bars};

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

/// Monday's session from 14:00, Tuesday's, and Wednesday's first bar. Monday's last bar is b2,
/// Tuesday's first b3 and its last b5; Tuesday dips at b4 and closes low at b5, the prices a limit
/// that walked past its auction would take.
fn tape() -> Vec<Bar> {
    vec![
        bar(at(8, 14, 0), FLAT),
        bar(at(8, 15, 0), [101.0, 101.8, 100.6, 101.5]),
        bar(at(8, 20, 0), [102.0, 102.8, 101.6, 102.5]),
        bar(at(9, 13, 30), [103.0, 103.8, 102.6, 103.5]),
        bar(at(9, 15, 0), [98.0, 98.8, 97.6, 98.5]),
        bar(at(9, 20, 0), [104.0, 104.8, 100.6, 101.5]),
        bar(at(10, 13, 30), [105.0, 105.8, 104.6, 105.5]),
    ]
}

#[derive(Clone, Copy)]
enum Auction {
    Open,
    Close,
}

fn auction(which: Auction, side: i32, limit: Option<f64>) -> Once {
    Once::new(move |b| match which {
        Auction::Open => b.submit_at_open(SYM, side, 1.0, limit, 0.0, true),
        Auction::Close => b.submit_at_close(SYM, side, 1.0, limit, 0.0, true),
    })
}

/// The run's `dropped` list as (reason, size).
fn dropped(engine: &StrategyEngine<Once>) -> Vec<(String, f64)> {
    engine.core.dropped.iter().map(|d| (d.reason.clone(), d.size)).collect()
}

// ---- 1. the law on coarse bars ----

/// (case, auction, side, limit, fill `(ts, price)`)
type Row = (&'static str, Auction, i32, Option<f64>, Option<(i64, f64)>);

/// Submitted on Monday's 14:00 bar.
#[rustfmt::skip]
fn rows() -> Vec<Row> {
    let (tue_open, mon_close) = (at(9, 13, 30), at(8, 20, 0));
    vec![
        ("moo buy: Tuesday's open", Auction::Open, 1, None, Some((tue_open, 103.0))),
        ("moo sell", Auction::Open, -1, None, Some((tue_open, 103.0))),
        ("loo buy, the open within L", Auction::Open, 1, Some(103.5), Some((tue_open, 103.0))),
        ("loo buy, the open above L: passed, never the dip after", Auction::Open, 1, Some(102.0), None),
        ("loo sell, the open below L: passed", Auction::Open, -1, Some(104.0), None),
        ("moc buy: Monday's last close", Auction::Close, 1, None, Some((mon_close, 102.5))),
        ("loc buy, the close above L: passed, never Tuesday's", Auction::Close, 1, Some(102.0), None),
        ("loc sell, the close within L", Auction::Close, -1, Some(102.0), Some((mon_close, 102.5))),
    ]
}

#[test]
fn the_auction_law_on_bars() {
    for (case, which, side, limit, fill) in rows() {
        let engine = run_bars(us(), tape(), auction(which, side, limit));
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, false);
                assert!(engine.core.dropped.is_empty(), "{case}: {:?}", engine.core.dropped);
            }
            None => {
                assert!(fills.is_empty(), "{case}: {fills:?}");
                assert_eq!(dropped(&engine), [("auction_passed".to_string(), 1.0)], "{case}");
            }
        }
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}: it never rests");
    }
}

/// A LIMIT auction order is limit-bounded (`vike_model::OrderResting::is_limit_bounded`): with the
/// fixed slippage on, a fill AT its limit stays at the limit, never pushed through it, while a
/// market auction order pays the amount. Every fill stays a taker.
#[test]
fn a_limit_auction_order_never_fills_past_its_limit() {
    let (tue_open, mon_close) = (at(9, 13, 30), at(8, 20, 0));
    let cases = [
        ("loo buy at its limit, the open", Auction::Open, 1, Some(103.0), tue_open, 103.0),
        ("loc sell at its limit, the close", Auction::Close, -1, Some(102.5), mon_close, 102.5),
        ("moo buy pays the amount", Auction::Open, 1, None, tue_open, 103.0 + AMOUNT),
        ("moc sell pays the amount", Auction::Close, -1, None, mon_close, 102.5 - AMOUNT),
    ];
    for (case, which, side, limit, ts, px) in cases {
        let p = StrategyEngineConfig { slippage_amount: AMOUNT, ..us() };
        let engine = run_bars(p, tape(), auction(which, side, limit));
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, false);
    }
}

/// `MarketClose` is the close of the next bar, whichever it is (Monday 15:00); the closing auction
/// waits for the session's last bar (Monday 20:00).
#[test]
fn market_close_is_the_next_bar_and_the_closing_auction_the_sessions_last() {
    let mc = run_bars(us(), tape(), Once::new(|b| b.submit_market_close(SYM, 1, 1.0, 0.0, true)));
    assert_fill("market close", &mc.strategy.fills[0], at(8, 15, 0), 101.5, false);
    let moc = run_bars(us(), tape(), auction(Auction::Close, 1, None));
    assert_fill("the closing auction", &moc.strategy.fills[0], at(8, 20, 0), 102.5, false);
}

// ---- 2. the opening auction rides the release ----

#[test]
fn an_opening_auction_is_held_until_the_next_sessions_open() {
    let engine =
        run_bars(us(), tape().into_iter().take(3).collect(), auction(Auction::Open, 1, None));
    let held: Vec<_> = engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.release)).collect();
    let tue_open = OrderRelease { at: at(9, 13, 30), price: None };
    assert_eq!(held, [(OrderKind::AuctionOpen, Some(tue_open))]);
    let close =
        run_bars(us(), tape().into_iter().take(2).collect(), auction(Auction::Close, 1, None));
    let live: Vec<_> = close.core.pending_of(SYM).iter().map(|o| (o.kind, o.release)).collect();
    assert_eq!(live, [(OrderKind::AuctionClose, None)], "live, waiting for the last bar");
}

// ---- 3. sub-bars ----

/// Monday's last bar has two sub-bars: the first closes at 101.9, under a sell limit at 102.5, the
/// second at 102.8. The closing auction is the second's close: the limit fills there.
#[test]
fn on_sub_bars_the_closing_auction_is_the_last_sub_bars_close() {
    let subs = vec![
        vike_simulator::test_support::bar(at(8, 20, 0), 102.0, 102.5, 101.8, 101.9, 50.0),
        vike_simulator::test_support::bar(at(8, 20, 30), 101.9, 102.9, 101.8, 102.8, 50.0),
    ];
    let p =
        || StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs.clone())], ..us() };
    for (case, side, limit) in [("loc sell", -1, Some(102.5)), ("moc buy", 1, None)] {
        let engine = run_bars(p(), tape(), auction(Auction::Close, side, limit));
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], at(8, 20, 30), 102.8, false);
    }
}

// ---- 4. the tick lane ----

fn run_ticks(strategy: Once) -> StrategyEngine<Once> {
    let tape = vec![
        q(at(8, 14, 0), 99.0, 100.0),
        q(at(8, 20, 0), 100.0, 101.0),
        q(at(9, 13, 30), 102.0, 103.0),
        q(at(9, 15, 0), 97.0, 98.0),
    ];
    let p = StrategyEngineConfig { fill_model: FillModelKind::Tick, ..us() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strategy, p);
    engine.run_ticks(&[(SYM.to_string(), tape)]);
    engine
}

/// An opening auction fills at the session's first tick, at the price that tick trades (a quote's
/// mid, never across its spread). A closing auction is ended on the first tick it meets.
#[test]
fn the_tick_lane_opens_at_the_first_tick_and_ends_a_closing_auction_in_words() {
    let moo = run_ticks(auction(Auction::Open, 1, None));
    assert_eq!(moo.strategy.fills.len(), 1, "{:?}", moo.strategy.fills);
    assert_fill("the first tick's mid", &moo.strategy.fills[0], at(9, 13, 30), 102.5, false);

    let moc = run_ticks(auction(Auction::Close, 1, None));
    assert!(moc.strategy.fills.is_empty(), "{:?}", moc.strategy.fills);
    assert_eq!(dropped(&moc), [("auction_close_needs_bars".to_string(), 1.0)]);
    assert!(moc.core.pending_of(SYM).is_empty());
}

// ---- 5. a release ----

fn released(release: OrderRelease, which: Auction) -> Once {
    Once::new(move |b| {
        b.submit_released(release, |b| match which {
            Auction::Open => b.submit_at_open(SYM, 1, 1.0, None, 0.0, true),
            Auction::Close => b.submit_at_close(SYM, 1, 1.0, None, 0.0, true),
        })
    })
}

/// Released on Tuesday 14:00, inside Tuesday's session: the opening auction is Wednesday's, the
/// closing one Tuesday's last bar (whose low close a limit would otherwise never see).
#[test]
fn a_release_moves_the_auction_to_the_first_one_after_it() {
    let tue = OrderRelease { at: at(9, 14, 0), price: None };
    let moo = run_bars(us(), tape(), released(tue, Auction::Open));
    assert_eq!(moo.strategy.fills.len(), 1, "{:?}", moo.strategy.fills);
    assert_fill("Wednesday's open", &moo.strategy.fills[0], at(10, 13, 30), 105.0, false);

    let moc = run_bars(us(), tape(), released(tue, Auction::Close));
    assert_eq!(moc.strategy.fills.len(), 1, "{:?}", moc.strategy.fills);
    assert_fill("Tuesday's last close", &moc.strategy.fills[0], at(9, 20, 0), 101.5, false);
}

/// A price condition met only after the auction would hold the order onto a bar that is not its
/// auction, so an auction order takes none: refused in words, and nothing rests.
#[test]
fn an_auction_order_takes_no_price_condition() {
    let level = OrderReleasePrice {
        symbol: SYM.into(),
        level: 103.0,
        side: OrderReleasePriceSide::Above,
        venue: None,
    };
    let conditional = OrderRelease { at: at(9, 14, 0), price: Some(level) };
    for (which, tif) in [(Auction::Open, "at-open"), (Auction::Close, "at-close")] {
        let engine = run_bars(us(), tape(), released(conditional, which));
        let why = format!("time in force `{tif}` takes a time release, not a price condition");
        assert_eq!(dropped(&engine), [(why, 1.0)], "{tif}");
        assert!(
            engine.strategy.fills.is_empty() && engine.core.pending_of(SYM).is_empty(),
            "{tif}"
        );
    }
}

// ---- 6. no auction ----

/// The gate off (no calendar) and a 24/7 calendar both refuse, in words, and nothing rests.
#[test]
fn a_symbol_with_no_calendar_or_a_24_7_one_has_no_auction() {
    let words = |tif: &str| {
        format!(
            "time in force `{tif}`: {SYM} has no auction (no session calendar, or one that never \
             closes)"
        )
    };
    let cases = [
        ("the gate off", params(), Auction::Open, words("at-open")),
        ("24/7", session(CRYPTO_24_7), Auction::Close, words("at-close")),
    ];
    for (case, p, which, why) in cases {
        let engine = run_bars(p, tape(), auction(which, 1, None));
        assert!(engine.strategy.fills.is_empty(), "{case}: {:?}", engine.strategy.fills);
        assert_eq!(dropped(&engine), [(why, 1.0)], "{case}");
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
    }
}
