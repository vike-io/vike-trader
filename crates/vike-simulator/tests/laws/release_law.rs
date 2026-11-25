//! The release law, backtest side: an order sent inside [`vike_simulator::OrderBrokerSimulator::submit_released`]
//! carries a `vike_model::OrderRelease` and is HELD, off the book, until it holds (`OrderRelease::holds`,
//! which reads every condition a release states: the instant `at` and, for a CONDITIONAL order, a
//! `vike_model::OrderReleasePrice`), as the live core's held table holds one. Held, it takes part in
//! no event on any fill lane and commits no margin; from the first event at or after `at` it is
//! judged by the pre-trade gate, then takes part by its own kind's law. A bar's ts is its open and
//! a bar has no time inside it, so an instant inside a bar releases from the NEXT bar. Pinned here:
//!
//! 1. coarse bars: a market order at the first bar at or after the release; a limit and a stop
//!    not filled by a bar before it, though that bar crosses them;
//! 2. sub-bars, the cash-gated lane, the tick lane and the L2 lane: the first event at or after;
//! 3. the queue-model tick lane: a held limit keeps no queue state, and joins the BACK of its level
//!    when it is released (it was never on the book before);
//! 4. a release not in the future is refused in `vike_model::lifetime_refusal`'s words, a tagged
//!    quote sent inside one is refused, and the release ends with the verb;
//! 5. a price condition is read on the condition symbol's last seen price, the mark before each
//!    event (its own symbol's or another one's), so the order takes part from the event after both
//!    conditions hold; a condition on a symbol outside the run is refused;
//! 6. judged as the core judges it: a held order commits no margin, the pre-trade gate judges it at
//!    its release against the account as it is then, and a cancel drops it at once;
//! 7. `OrderBrokerSimulator::submit_market_at`, `LiveBroker::submit_market_at`'s twin, is the market verb inside
//!    a release.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_model::{
    OrderBroker, OrderDrop, OrderKind, OrderPlacement, OrderRelease, OrderReleasePrice,
    OrderReleasePriceSide, OrderType, RiskLimits, TimeInForce,
};
use vike_simulator::test_support::trade;
use vike_simulator::{
    FillModelKind, OrderBrokerSimulator, QueueModelKind, StrategyEngine, StrategyEngineConfig,
};

use crate::stop_limit_law::{
    FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
};

/// A time release at `at`.
fn time(at: i64) -> OrderRelease {
    OrderRelease { at, price: None }
}

/// `submit` sent inside a release at `at`, or plainly with none.
fn released(at: Option<i64>, submit: fn(&mut OrderBrokerSimulator)) -> Once {
    Once::new(move |b| match at {
        Some(at) => b.submit_released(time(at), submit),
        None => submit(b),
    })
}

fn buy_market(b: &mut OrderBrokerSimulator) {
    b.submit(SYM, 1, 1.0, 0.0, true, None);
}

fn tape() -> Vec<vike_marketdata::Bar> {
    vec![
        bar(1_000, FLAT),
        bar(2_000, [101.0, 101.5, 100.0, 101.0]),
        bar(3_000, [102.0, 102.5, 101.5, 102.0]),
        bar(4_000, [100.5, 101.0, 99.5, 100.0]),
    ]
}

// ---- 1. coarse bars ----

/// (case, submit, release, fill `(ts, price, maker)`); every order is sent on bar 0 (ts 1 000).
type Row = (&'static str, fn(&mut OrderBrokerSimulator), Option<i64>, (i64, f64, bool));

#[rustfmt::skip]
fn rows() -> Vec<Row> {
    let limit: fn(&mut OrderBrokerSimulator) = |b| b.submit_limit(SYM, 1, 1.0, 100.5, 0.0, true, None);
    let stop: fn(&mut OrderBrokerSimulator) = |b| b.submit_stop(SYM, 1, 1.0, 101.2, 0.0, true);
    let market: fn(&mut OrderBrokerSimulator) = buy_market;
    vec![
        ("market, no release: the next bar", market, None, (2_000, 101.0, false)),
        ("market, released at bar 1's ts", market, Some(2_000), (2_000, 101.0, false)),
        ("market, released inside bar 1: bar 2", market, Some(2_500), (3_000, 102.0, false)),
        ("market, released at bar 2's ts", market, Some(3_000), (3_000, 102.0, false)),
        ("limit, no release: bar 1 crosses it", limit, None, (2_000, 100.5, true)),
        ("limit, held past bar 1's cross: bar 3", limit, Some(3_000), (4_000, 100.5, true)),
        ("stop, no release: bar 1 reaches it", stop, None, (2_000, 101.2, false)),
        ("stop, held: bar 2's gapped open", stop, Some(3_000), (3_000, 102.0, false)),
    ]
}

#[test]
fn a_held_order_takes_part_from_the_first_bar_at_or_after_its_release() {
    for (case, submit, at, (ts, px, maker)) in rows() {
        let engine = run_bars(params(), tape(), released(at, submit));
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, maker);
    }
}

/// Before its release the order is held OFF the book, in the held table with its release, and
/// nothing has touched it.
#[test]
fn a_held_order_waits_off_the_book_with_its_release() {
    let submit = |b: &mut OrderBrokerSimulator| b.submit_limit(SYM, 1, 1.0, 100.5, 0.0, true, None);
    let engine =
        run_bars(params(), tape().into_iter().take(3).collect(), released(Some(3_500), submit));
    assert!(engine.strategy.fills.is_empty(), "{:?}", engine.strategy.fills);
    let held: Vec<_> =
        engine.core.symbol[0].held.iter().map(|h| (h.order.kind, h.release)).collect();
    assert_eq!(held, [(OrderKind::Limit, time(3_500))]);
    assert!(engine.core.pending_of(SYM).is_empty(), "not on the book");
}

// ---- 2. the other lanes ----

/// Bar 1's four sub-bars open at 101, 101.2, 101.4 and 101.6: released at 2 400, a market order
/// fills on the sub-bar at 2 500, the first at or after it.
#[test]
fn on_sub_bars_the_first_sub_bar_at_or_after_the_release() {
    let subs = [(2_000, 101.0), (2_250, 101.2), (2_500, 101.4), (2_750, 101.6)]
        .map(|(ts, px)| vike_simulator::test_support::bar(ts, px, px + 0.1, px - 0.1, px, 25.0))
        .to_vec();
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    let engine = run_bars(p, tape(), released(Some(2_400), buy_market));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the 2 500 sub-bar", &engine.strategy.fills[0], 2_500, 101.4, false);
}

#[test]
fn the_cash_gated_lane_holds_it_too() {
    let p = StrategyEngineConfig { cash_gate: true, ..params() };
    let engine = run_bars(p, tape(), released(Some(2_500), buy_market));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("bar 2", &engine.strategy.fills[0], 3_000, 102.0, false);
}

/// The tick lane: a market order released at 3 fills on the quote at 3, at its ask. The L2 lane:
/// held past the quote at 3 (where it would walk 100), it walks the book that stands at 5.
#[test]
fn the_tick_and_l2_lanes_hold_it_to_the_first_event_at_or_after() {
    let quotes = vec![q(1, 99.0, 100.0), q(2, 100.0, 101.0), q(3, 101.0, 102.0)];
    let engine = run_ticks(FillModelKind::Tick, quotes, released(Some(3), buy_market));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the quote at 3", &engine.strategy.fills[0], 3, 102.0, false);

    let l2 = |at: Option<i64>| {
        let tape = vec![
            book(1, &[(99.0, 5.0)], &[(100.0, 5.0)]),
            q(2, 99.0, 100.0),
            q(3, 99.0, 100.0),
            book(4, &[(100.0, 5.0)], &[(101.0, 5.0)]),
            q(5, 100.0, 101.0),
        ];
        run_ticks(FillModelKind::L2Book, tape, released(at, buy_market))
    };
    assert_fill("unheld: the book at 3", &l2(None).strategy.fills[0], 3, 100.0, false);
    let held = l2(Some(4));
    assert_eq!(held.strategy.fills.len(), 1, "{:?}", held.strategy.fills);
    assert_fill("held: the book at 5", &held.strategy.fills[0], 5, 101.0, false);
}

// ---- 3. the queue-model tick lane ----

/// The book shows 10 bid at 100; the order (a buy limit of 2 at 100) is sent on the trade at 2;
/// 20 trade at 100 at 3 and again at 5. Unheld, it joins behind 10 at 3 and that trade's excess
/// fills it. Held to 4, the trade at 3 cannot touch it and it has no queue identity; released, it
/// joins behind the 10 the book shows at 4 and fills on the trade at 5. `upto` cuts the tape.
fn queued_run(at: Option<i64>, upto: usize) -> StrategyEngine<Once> {
    let tape = vec![
        book(1, &[(100.0, 10.0)], &[(101.0, 5.0)]),
        trade(SYM, 2, 100.5, 1.0),
        trade(SYM, 3, 100.0, 20.0),
        trade(SYM, 4, 100.5, 1.0),
        trade(SYM, 5, 100.0, 20.0),
    ];
    let submit = |b: &mut OrderBrokerSimulator| b.submit_limit(SYM, 1, 2.0, 100.0, 0.0, true, None);
    let p = StrategyEngineConfig { queue_model: Some(QueueModelKind::RiskAverse), ..params() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], released(at, submit), p);
    engine.run_ticks(&[(SYM.to_string(), tape.into_iter().take(upto).collect::<Vec<_>>())]);
    engine
}

#[test]
fn the_queue_lane_seeds_a_held_limit_only_when_it_is_released() {
    let unheld = queued_run(None, 5);
    assert_fill("unheld: the trade at 3", &unheld.strategy.fills[0], 3, 100.0, true);

    let during = queued_run(Some(4), 3);
    assert!(during.strategy.fills.is_empty(), "{:?}", during.strategy.fills);
    assert!(during.core.pending_of(SYM).is_empty(), "not on the book, so no queue identity");
    assert_eq!(during.core.symbol[0].held.len(), 1);

    let after = queued_run(Some(4), 5);
    assert_eq!(after.strategy.fills.len(), 1, "{:?}", after.strategy.fills);
    assert_fill("released: the trade at 5", &after.strategy.fills[0], 5, 100.0, true);
}

// ---- 4. the intake ----

/// A release at or before now (the bar's ts, 1 000) is refused in the lifetime rule's words, and
/// nothing rests.
#[test]
fn a_release_not_in_the_future_is_refused_in_the_lifetime_rules_words() {
    for at in [1_000, 500] {
        let engine = run_bars(params(), tape(), released(Some(at), buy_market));
        let why = vike_model::lifetime_refusal(
            &OrderType::Market,
            TimeInForce::Gtc,
            None,
            Some(time(at)),
            1_000,
        );
        assert!(engine.strategy.fills.is_empty(), "{at}: {:?}", engine.strategy.fills);
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop {
                symbol: SYM.to_string(),
                reason: why.expect("refused"),
                size: 1.0,
                weight: 0.0
            }]
        );
        assert!(resting(&engine).is_empty());
    }
}

/// A tagged quote rests at once, so one sent inside a release is refused, in the words of every
/// order placed by tag, rather than resting with its release dropped; and the release ends with the
/// verb, so the order sent after it is plain.
#[test]
fn a_tagged_quote_is_refused_and_the_release_ends_with_the_verb() {
    let engine = run_bars(
        StrategyEngineConfig { optimistic_quotes: true, ..params() },
        tape(),
        Once::new(|b| {
            let quote = OrderPlacement::Limit { price: 99.0 };
            b.submit_released(time(3_000), |b| b.place("", "q", 1, 1.0, quote, false));
            buy_market(b);
        }),
    );
    let reasons: Vec<_> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["a tagged order takes no release"]);
    assert!(engine.core.tagged_of(SYM).is_empty(), "nothing quoted");
    assert_fill("the plain order after it", &engine.strategy.fills[0], 2_000, 101.0, false);
}

// ---- 5. the price condition ----

fn conditional(at: i64, symbol: &str, level: f64, side: OrderReleasePriceSide) -> OrderRelease {
    OrderRelease {
        at,
        price: Some(OrderReleasePrice { symbol: symbol.into(), level, side, venue: None }),
    }
}

/// A market order on the tape above, sent on bar 0 inside `release`. The marks the bars see are the
/// closes before them: 100 at bar 1, 101 at bar 2, 102 at bar 3.
fn conditional_market(release: OrderRelease) -> Once {
    Once::new(move |b| b.submit_released(release, buy_market))
}

/// (case, release, fill `(ts, price)`)
#[rustfmt::skip]
fn conditional_rows() -> Vec<(&'static str, OrderRelease, (i64, f64))> {
    vec![
        ("above 101.8: bar 3, the first whose mark (102) meets it", conditional(2_000, SYM, 101.8, OrderReleasePriceSide::Above), (4_000, 100.5)),
        ("below 100.5: bar 1, its mark (100) meets it at once", conditional(2_000, SYM, 100.5, OrderReleasePriceSide::Below), (2_000, 101.0)),
        ("above 100.5 from 3 000: bar 2, both hold there", conditional(3_000, SYM, 100.5, OrderReleasePriceSide::Above), (3_000, 102.0)),
    ]
}

#[test]
fn a_conditional_order_takes_part_once_its_time_and_its_price_both_hold() {
    for (case, release, (ts, px)) in conditional_rows() {
        let engine = run_bars(params(), tape(), conditional_market(release));
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, false);
    }
}

/// The condition reads ANOTHER symbol of the run: an index closing 50, 50, 60, 60 holds a market
/// order on the traded symbol until its mark reaches 55, which bar 3 sees.
#[test]
fn a_condition_on_another_symbol_reads_that_symbols_mark() {
    let index: Vec<_> = [50.0, 50.0, 60.0, 60.0]
        .iter()
        .enumerate()
        .map(|(i, &px)| bar(1_000 * (i as i64 + 1), [px; 4]))
        .collect();
    let release = conditional(2_000, "IDX", 55.0, OrderReleasePriceSide::Above);
    let symbols = vec![(SYM.to_string(), tape()), ("IDX".to_string(), index)];
    let mut engine = StrategyEngine::new(symbols, conditional_market(release), params());
    engine.run();
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("bar 3", &engine.strategy.fills[0], 4_000, 100.5, false);
}

/// A condition on a symbol the run does not carry could never be met: refused in words.
#[test]
fn a_condition_on_a_symbol_outside_the_run_is_refused() {
    let release = conditional(2_000, "NOPE", 1.0, OrderReleasePriceSide::Above);
    let engine = run_bars(params(), tape(), conditional_market(release));
    let why = "the price condition's symbol NOPE is not in this run".to_string();
    assert_eq!(
        engine.core.dropped,
        vec![OrderDrop { symbol: SYM.to_string(), reason: why, size: 1.0, weight: 0.0 }]
    );
    assert!(resting(&engine).is_empty());
}

// ---- 6. judged as the core judges it ----

/// A cash of 150 and a 100% margin requirement: one unit at about 100 fits, two do not.
fn margined() -> StrategyEngineConfig {
    let limits = RiskLimits { initial_margin_requirement: Some(1.0), ..RiskLimits::new() };
    StrategyEngineConfig {
        cash: 150.0,
        default_venue: Some("TEST".into()),
        risk_limits: Some(limits),
        ..params()
    }
}

/// A held order is off the book, so it commits no margin: the plain market order sent after it on
/// bar 0 passes the gate (it would not if the held unit counted). The held one is judged at ITS
/// release, bar 2, against the account as it is then: the unit bought on bar 1 leaves no room, and
/// the gate refuses it there, in its own words. Alone, it is released, passes and fills.
#[test]
fn a_held_order_commits_no_margin_and_is_judged_at_its_release() {
    let both = Once::new(|b| {
        b.submit_market_at(SYM, 1, 1.0, time(3_000));
        buy_market(b);
    });
    let engine = run_bars(margined(), tape(), both);
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the plain order", &engine.strategy.fills[0], 2_000, 101.0, false);
    let reasons: Vec<_> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["insufficient-margin"], "refused at its release");
    assert!(engine.core.symbol[0].held.is_empty() && resting(&engine).is_empty());

    let held_only = Once::new(|b| b.submit_market_at(SYM, 1, 1.0, time(3_000)));
    let alone = run_bars(margined(), tape(), held_only);
    assert_eq!(alone.strategy.fills.len(), 1, "{:?}", alone.strategy.fills);
    assert_fill("released and filled", &alone.strategy.fills[0], 3_000, 102.0, false);
}

/// A cancel drops a held order at once (no venue holds it), so nothing is released.
#[test]
fn a_cancel_drops_a_held_order() {
    let engine = run_bars(
        params(),
        tape(),
        Once::new(|b| {
            b.submit_market_at(SYM, 1, 1.0, time(3_000));
            b.cancel_all(SYM);
        }),
    );
    assert!(engine.strategy.fills.is_empty(), "{:?}", engine.strategy.fills);
    assert!(engine.core.symbol[0].held.is_empty());
}

// ---- 7. the core verb's twin ----

#[test]
fn submit_market_at_is_the_market_verb_inside_a_release() {
    let at =
        run_bars(params(), tape(), Once::new(|b| b.submit_market_at(SYM, 1, 1.0, time(2_500))));
    let inside = run_bars(params(), tape(), released(Some(2_500), buy_market));
    assert_eq!(at.strategy.fills, inside.strategy.fills);
    assert_fill("bar 2", &at.strategy.fills[0], 3_000, 102.0, false);
}
