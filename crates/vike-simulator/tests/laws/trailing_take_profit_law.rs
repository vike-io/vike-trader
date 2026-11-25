//! The trailing take-profit law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_trailing_take_profit`]
//! (IB `TRAIL MIT`, and `TRAIL LIT` with a limit offset) driven through the engine's fill lanes, over
//! the ONE law `vike_model::order_fill_price` states (decision 0267, 2.9; IB's description, no
//! evidence).
//!
//! The if-touched mirror of the trailing stop: a SELL tracks the LOWEST price since it rests, its
//! trigger T = low + trail, and fires when the price rises to T, at A = T, or the open when the bar
//! gapped above T (an improvement); a BUY tracks the highest price, T = high − trail, and fires on a
//! fall to T, at A = min(T, open). It fires on the T the PRIOR extreme implies, and the extreme
//! ratchets only on an event that does not fire it. With no offset it fills at A plus slippage, a
//! TAKER; with one its limit L is T − offset for a sell and T + offset for a buy, then the stop-limit's
//! limit part. That limit is on the far side of T, so a bar or a quote that fires the order always
//! reaches it: only the L2 book's depth can leave it resting as the `Limit` at L. Pinned here:
//!
//! 1. the law on coarse bars, both sides, with and without an offset, each fill a taker; an offset makes
//!    the order limit-bounded, so it pays no fixed slippage, and one without pays it;
//! 2. sub-bars walk the path, the extreme ratcheting per sub-bar;
//! 3. the tick lane (the bid for a sell, the ask for a buy) and the L2 lane (the walk capped at L,
//!    depth L cannot cover resting it as the `Limit` there, a maker on a later book);
//! 4. it is FAVOURABLE: a stop exit in the same bar fills first and caps it; and it is a protective
//!    exit of the implicit bracket (`vike_execution::is_protective_exit_sibling`), cancelled with the
//!    protective stop's fill;
//! 5. the intake: the order-terms rule refuses in its own words, and a symbol with no price to trail
//!    from is refused.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_model::{OrderDrop, OrderKind, OrderType};
use vike_simulator::{FillModelKind, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
};

const TRAIL: f64 = 2.0;

fn ttp(side: i32, size: f64, offset: Option<f64>) -> Once {
    Once::new(move |b| b.submit_trailing_take_profit(SYM, side, size, TRAIL, offset, 0.0, true))
}

// ---- 1. the law on coarse bars ----

/// (case, side, offset, bars 1 and 2, fill `(ts, price)`)
type Row = (&'static str, i32, Option<f64>, [[f64; 4]; 2], (i64, f64));

/// Trail 2, submitted on bar 0 (the mark 100): a sell's first trigger is 102, a buy's 98. Every row is
/// the law's own consequence, not a measurement.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    // the low ratchets to 98 on bar 1 (no touch of 102); bar 2's high reaches the new T, 100
    ("sell: not touched, the low ratchets, the next T fills", -1, None, [[100.0, 101.5, 98.0, 99.0], FLAT], (3_000, 100.0)),
    ("sell: touched, no gap: A = T", -1, None, [[100.0, 103.0, 99.5, 102.0], FLAT], (2_000, 102.0)),
    ("sell: a gap up past T: A = the open", -1, None, [[104.0, 105.0, 103.5, 104.0], FLAT], (2_000, 104.0)),
    ("sell, offset 1: L 101 is below A, filled at A = T", -1, Some(1.0), [[100.0, 103.0, 99.5, 102.0], FLAT], (2_000, 102.0)),
    ("sell, offset 1, a gap up: A = the open", -1, Some(1.0), [[104.0, 105.0, 103.5, 104.0], FLAT], (2_000, 104.0)),
    // the high ratchets to 103 on bar 1 (no touch of 98); bar 2's low reaches the new T, 101
    ("buy: not touched, the high ratchets, the next T fills", 1, None, [[100.0, 103.0, 98.5, 102.0], FLAT], (3_000, 100.0)),
    ("buy: touched, no gap: A = T", 1, None, [[100.0, 100.5, 97.0, 98.0], FLAT], (2_000, 98.0)),
    ("buy: a gap down past T: A = the open", 1, None, [[96.0, 97.0, 95.0, 96.0], FLAT], (2_000, 96.0)),
    ("buy, offset 0.5: L 98.5 is above A, filled at A = T", 1, Some(0.5), [[100.0, 100.5, 97.0, 98.0], FLAT], (2_000, 98.0)),
];

fn run_row(p: StrategyEngineConfig, &(_, side, offset, [b1, b2], _): &Row) -> StrategyEngine<Once> {
    let bars = vec![bar(1_000, FLAT), bar(2_000, b1), bar(3_000, b2)];
    run_bars(p, bars, ttp(side, 1.0, offset))
}

#[test]
fn the_trailing_take_profit_law_on_bars_with_its_fee_class() {
    for row @ &(case, .., (ts, px)) in ROWS {
        let engine = run_row(params(), row);
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, false);
        assert!(resting(&engine).is_empty(), "{case}");
    }
}

/// With an offset the order is limit-bounded (IB `TRAIL LIT`): the fixed slippage moves none of its
/// fills. Without one (`TRAIL MIT`) every fill pays it, against the order's side.
#[test]
fn an_offset_makes_it_limit_bounded_and_none_pays_the_fixed_slippage() {
    for row @ &(case, side, offset, .., (ts, px)) in ROWS {
        let engine = run_row(StrategyEngineConfig { slippage_amount: AMOUNT, ..params() }, row);
        let want = if offset.is_some() { px } else { px + f64::from(side) * AMOUNT };
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, want, false);
    }
}

// ---- 2. sub-bars ----

/// A sell over the coarse bar `[100, 102.5, 97, 101]`: on the coarse bar its high touches T 102 and it
/// fills there. Its sub-bars say the low came FIRST: the first sub-bar ratchets the low to 97 (T 99),
/// and the second rises through 99 from its open of 97.5, filling at 99.
#[test]
fn sub_bars_walk_the_path_and_ratchet_per_sub_bar() {
    let run = |subs: Vec<vike_marketdata::Bar>| {
        let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
        let bars = vec![bar(1_000, FLAT), bar(2_000, [100.0, 102.5, 97.0, 101.0])];
        run_bars(p, bars, ttp(-1, 1.0, None))
    };
    let coarse = run(Vec::new());
    assert_eq!(coarse.strategy.fills.len(), 1, "{:?}", coarse.strategy.fills);
    assert_fill("coarse: the high touches 102", &coarse.strategy.fills[0], 2_000, 102.0, false);

    let subs = vec![
        vike_simulator::test_support::bar(2_000, 100.0, 100.5, 97.0, 97.5, 50.0),
        vike_simulator::test_support::bar(2_500, 97.5, 102.5, 97.5, 101.0, 50.0),
    ];
    let walked = run(subs);
    assert_eq!(walked.strategy.fills.len(), 1, "{:?}", walked.strategy.fills);
    assert_fill(
        "the path: the low first, then T 99",
        &walked.strategy.fills[0],
        2_500,
        99.0,
        false,
    );
}

// ---- 3. the tick lane and the L2 lane ----

/// A sell trails the lowest BID from the mark (the first quote's mid, 99.5) and fires on a bid at or
/// above low + trail, at that bid; a buy trails the highest ASK and fires on an ask at or below
/// high − trail, at that ask.
#[test]
fn the_tick_lane_trails_and_fires_on_the_quote() {
    let sell = vec![q(1, 99.0, 100.0), q(2, 98.0, 99.0), q(3, 99.5, 100.5), q(4, 100.5, 101.5)];
    for offset in [None, Some(1.0)] {
        let engine = run_ticks(FillModelKind::Tick, sell.clone(), ttp(-1, 1.0, offset));
        assert_eq!(engine.strategy.fills.len(), 1, "{offset:?}: {:?}", engine.strategy.fills);
        assert_fill(
            "sell: low 98, T 100, the bid 100.5",
            &engine.strategy.fills[0],
            4,
            100.5,
            false,
        );
    }
    let buy = vec![q(1, 99.0, 100.0), q(2, 101.0, 102.0), q(3, 100.5, 101.5), q(4, 99.0, 99.8)];
    let engine = run_ticks(FillModelKind::Tick, buy, ttp(1, 1.0, None));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("buy: high 102, T 100, the ask 99.8", &engine.strategy.fills[0], 4, 99.8, false);
}

/// The L2 tape: a buy (offset 0.5) is submitted on the quote at ts 2 (mark 99.5), the ask of 102 at ts
/// 3 ratchets its high, and the quote at ts 5 fires it at T 100 (L 100.5) against asks of 1 at 99, 1 at
/// 100 and 5 at 101. Two units walk 99 and 100; three find only two within L and rest as the `Limit`
/// at 100.5, which the book at ts 6 fills as a maker. `upto` cuts the tape.
fn l2_run(size: f64, upto: usize) -> StrategyEngine<Once> {
    let tape = vec![
        book(1, &[(99.0, 5.0)], &[(100.0, 5.0)]),
        q(2, 99.0, 100.0),
        q(3, 101.0, 102.0),
        book(4, &[(98.0, 5.0)], &[(99.0, 1.0), (100.0, 1.0), (101.0, 5.0)]),
        q(5, 98.0, 99.0),
        book(6, &[(98.0, 5.0)], &[(100.0, 5.0)]),
        q(7, 99.0, 100.0),
    ];
    let tape = tape.into_iter().take(upto).collect();
    run_ticks(FillModelKind::L2Book, tape, ttp(1, size, Some(0.5)))
}

#[test]
fn the_l2_lane_walks_the_book_up_to_its_limit() {
    let fits = l2_run(2.0, 5);
    assert_eq!(fits.strategy.fills.len(), 1, "{:?}", fits.strategy.fills);
    assert_fill("two units within L", &fits.strategy.fills[0], 5, 99.5, false);

    let short = l2_run(3.0, 5);
    assert!(short.strategy.fills.is_empty(), "{:?}", short.strategy.fills);
    assert_eq!(resting(&short), vec![(OrderKind::Limit, Some(100.5), None)], "rests as the Limit");

    let later = l2_run(3.0, 7);
    assert_eq!(later.strategy.fills.len(), 1, "{:?}", later.strategy.fills);
    assert_fill("the Limit's walk", &later.strategy.fills[0], 7, 100.0, true);
}

// ---- 4. a favourable exit, and a protective one ----

/// Long 1 from bar 1 with two exits: a stop selling at 95 and a trailing take-profit sell. Bar 2 spans
/// both: the stop is adverse and fills first, the take-profit (low 99 on bar 1, T 101) is capped to the
/// nothing left: flat, never reversed, and the bar counted as a guessed one.
#[test]
fn a_stop_exit_fills_first_and_capabilities_a_trailing_take_profit() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [100.0, 101.0, 99.0, 100.0]),
        bar(3_000, [100.0, 115.0, 90.0, 100.0]),
        bar(4_000, FLAT),
    ];
    let bracket = Once::new(|b| {
        b.submit(SYM, 1, 1.0, 0.0, true, None);
        b.submit_stop(SYM, -1, 1.0, 95.0, 0.0, true);
        b.submit_trailing_take_profit(SYM, -1, 1.0, TRAIL, None, 0.0, true);
    });
    let engine = run_bars(params(), bars, bracket);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "the entry and the stop only: {fills:?}");
    assert_fill("the stop exit", &fills[1], 3_000, 95.0, false);
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(engine.core.pending_of(SYM).is_empty(), "the capped take-profit left the book");
    assert_eq!(engine.core.intrabar_both_hit, 1);
}

/// The implicit bracket: an entry with a protective stop at 95 and a trailing take-profit sell far
/// away (trail 20). Bar 2 fires the protective stop; the take-profit is its kind-linked OCO sibling and
/// is cancelled with it, so bar 3's rally (T 110 off bar 2's low) opens no short.
#[test]
fn a_protective_stop_fill_cancels_a_closing_trailing_take_profit() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, FLAT),
        bar(3_000, [100.0, 100.0, 90.0, 92.0]),
        bar(4_000, [92.0, 125.0, 92.0, 120.0]),
    ];
    let bracket = Once::new(|b| {
        b.submit(SYM, 1, 1.0, 0.0, true, Some(95.0));
        b.submit_trailing_take_profit(SYM, -1, 1.0, 20.0, None, 0.0, true);
    });
    let engine = run_bars(params(), bars, bracket);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "the entry and the protective stop: {fills:?}");
    assert_fill("the protective stop", &fills[1], 3_000, 95.0, false);
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "no short opened");
    assert!(engine.core.pending_of(SYM).is_empty(), "the take-profit was cancelled");
}

// ---- 5. the intake ----

/// Terms `vike_model::order_terms_refusal` refuses never rest: the verb drops them in that rule's own
/// words, with its trail and its offset.
#[test]
fn trailing_take_profit_terms_the_order_rule_refuses_are_dropped_in_its_words() {
    for (case, trail, offset) in [
        ("a trail of zero", 0.0, None),
        ("a trail that is not a number", f64::NAN, None),
        ("an offset below zero", TRAIL, Some(-1.0)),
    ] {
        let once = Once::new(move |b| {
            b.submit_trailing_take_profit(SYM, -1, 1.0, trail, offset, 0.0, true);
        });
        let engine = run_bars(params(), vec![bar(1_000, FLAT), bar(2_000, FLAT)], once);
        let rule = &OrderType::TrailingTakeProfit;
        let why =
            vike_model::order_terms_refusal(rule, None, None, Some(trail), offset, None, None);
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop {
                symbol: SYM.to_string(),
                reason: why.expect(case),
                size: 1.0,
                weight: 0.0
            }],
            "{case}"
        );
    }
}

/// A symbol that has not printed has no price to trail from: refused in words (live refuses a trailing
/// order without a mark). Submitted on the first quote of ANOTHER symbol.
#[test]
fn a_symbol_with_no_price_yet_is_refused() {
    let once =
        Once::new(|b| b.submit_trailing_take_profit("LATE", -1, 1.0, TRAIL, None, 0.0, true));
    let symbols = vec![(SYM.to_string(), Vec::new()), ("LATE".to_string(), Vec::new())];
    let mut engine = StrategyEngine::new(symbols, once, params());
    let late = vike_simulator::test_support::quote("LATE", 5, 99.0, 100.0, 1.0, 1.0);
    engine
        .run_ticks(&[(SYM.to_string(), vec![q(1, 99.0, 100.0)]), ("LATE".to_string(), vec![late])]);
    let why = "a trailing_take_profit order trails from LATE's last price, and it has none yet";
    assert_eq!(
        engine.core.dropped,
        vec![OrderDrop {
            symbol: "LATE".to_string(),
            reason: why.to_string(),
            size: 1.0,
            weight: 0.0
        }]
    );
    assert!(engine.core.pending_of("LATE").is_empty());
}
