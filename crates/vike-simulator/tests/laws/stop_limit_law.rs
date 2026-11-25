//! The stop-limit law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_stop_limit`] driven through
//! every fill lane of the engine, over the ONE law `vike_model::order_fill_price` states (and
//! `vike_fills::fill_model`'s tick and L2 tiers carry).
//!
//! T is the trigger, L the limit, A the ACTIVATION price: T, or the open when the bar gapped past
//! T (a buy's `max(T, open)`, a sell's `min(T, open)`). A stop-limit triggers as a stop does. When
//! the trigger event reaches L it fills there at the better of L and A, as a TAKER; when it does
//! not, the order turns, in place, into the plain resting `Limit` at L, which fills by the limit
//! law on a later event as a MAKER. Pinned here:
//!
//! 1. every case of the law on coarse bars, each fill with its fee class;
//! 2. granular sub-bars: the law per sub-bar, and a conversion surviving the later sub-bars (where
//!    the coarse bar's range rule, assumption 2, fills a bar whose low came before its trigger);
//! 3. the tick lane (trigger and limit on the quote), the L2 lane (the walk capped at L, and
//!    depth L cannot cover resting the order as the `Limit`), and the queue-model tick lane (the
//!    converted `Limit` joins the BACK of its level when that lane first sees it, and fills by
//!    queue position from there);
//! 4. the SL/TP cap: a stop-limit exit is ADVERSE, so with a take-profit in the same bar it fills
//!    first and the take-profit is capped to what is left of the position;
//! 5. the session deferral: a shut market fills nothing, and the order fills at the reopen by the
//!    law on that bar;
//! 6. the intake: the order-terms rule refuses in its own words, and the pre-trade gate judges the
//!    order on its LIMIT, as live sends it.
//!
//! No slippage fraction is configured anywhere here: the flat `slippage` fraction applies to these
//! fills exactly as it does to a `Limit`'s, untouched by this law. The fixed amount
//! (`StrategyEngineConfig::slippage_amount`) is configured once, to pin that a limit-bounded fill pays none
//! of it (`a_stop_limit_pays_no_fixed_slippage_on_either_fee_class`). The take-profit twin, which
//! reuses the helpers below, is `take_profit_law.rs`.

use vike_fills::broker_sim::commission;
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::{ContractValue, Fill, OrderDrop, OrderKind, OrderType, RiskLimits, Strategy};
use vike_simulator::test_support::ms;
use vike_simulator::{
    FillModelKind, OrderBrokerSimulator, QueueModelKind, StrategyEngine, StrategyEngineConfig,
    TickEvent,
};

pub(crate) const SYM: &str = "TEST";
const MAKER: f64 = 0.001;
const TAKER: f64 = 0.002;
/// A quiet bar at 100: no order of these tests triggers or fills on it.
pub(crate) const FLAT: [f64; 4] = [100.0; 4];

pub(crate) fn bar(ts: i64, [open, high, low, close]: [f64; 4]) -> Bar {
    vike_simulator::test_support::bar(ts, open, high, low, close, 100.0)
}

/// No slippage and distinct maker and taker rates, so a fill's class reads off its fee.
pub(crate) fn params() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 1_000_000.0,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    }
}

/// Submits once, on the first bar, quote or trade it sees, and records every fill.
pub(crate) struct Once {
    submit: Box<dyn Fn(&mut OrderBrokerSimulator)>,
    done: bool,
    pub(crate) fills: Vec<Fill>,
}

impl Once {
    pub(crate) fn new(submit: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Self {
        Self { submit: Box::new(submit), done: false, fills: Vec::new() }
    }

    fn fire(&mut self, ctx: &mut OrderBrokerSimulator) {
        if !self.done {
            self.done = true;
            (self.submit)(ctx);
        }
    }
}

impl Strategy<OrderBrokerSimulator> for Once {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.fire(ctx);
    }
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.fire(ctx);
    }
    fn on_trade_tick(&mut self, ctx: &mut OrderBrokerSimulator, _t: &TickTrade) {
        self.fire(ctx);
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

pub(crate) fn run_bars(
    p: StrategyEngineConfig,
    bars: Vec<Bar>,
    strategy: Once,
) -> StrategyEngine<Once> {
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strategy, p);
    engine.run();
    engine
}

/// A tick replay on `fill_model` (the bar series is empty: the tape is the only price).
pub(crate) fn run_ticks(
    fill_model: FillModelKind,
    ticks: Vec<TickEvent>,
    strategy: Once,
) -> StrategyEngine<Once> {
    let p = StrategyEngineConfig { fill_model, ..params() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strategy, p);
    engine.run_ticks(&[(SYM.to_string(), ticks)]);
    engine
}

pub(crate) fn q(ts: i64, bid: f64, ask: f64) -> TickEvent {
    vike_simulator::test_support::quote(SYM, ts, bid, ask, 1_000.0, 1_000.0)
}

/// A book snapshot on a tick size of 1, so the integer prices of these tapes walk exactly.
pub(crate) fn book(ts: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> TickEvent {
    let levels = |side: &[(f64, f64)]| -> Vec<L2BookLevel> {
        side.iter().map(|&(px, qty)| L2BookLevel::new(px, qty)).collect()
    };
    TickEvent::Book(Box::new(L2BookUpdate {
        timestamp: ts,
        local_timestamp: 0,
        sequence: ts as u64,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 1.0,
        bids: levels(bids),
        asks: levels(asks),
        symbol: SYM.to_string(),
    }))
}

/// One fill at `(ts, price)`, a maker or a taker, charged that class's fee.
pub(crate) fn assert_fill(case: &str, f: &Fill, ts: i64, price: f64, maker: bool) {
    assert_eq!((f.timestamp, f.price, f.is_maker), (ts, price, maker), "{case}");
    let one = ContractValue::default();
    let fee =
        commission(None, MAKER, TAKER, maker, f.size, price, one, None, &mut Default::default());
    assert_eq!(f.fee, fee, "{case}: the fee of its class");
}

/// The one order resting at the end, as `(kind, price, limit)`.
pub(crate) fn resting(engine: &StrategyEngine<Once>) -> Vec<(OrderKind, Option<f64>, Option<f64>)> {
    engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.price, o.limit)).collect()
}

fn stop_limit(side: i32, size: f64, trigger: f64, limit: f64) -> Once {
    Once::new(move |b| b.submit_stop_limit(SYM, side, size, trigger, limit, 0.0, true))
}

// ---- 1. the law on coarse bars, with the fee class ----

/// (case, side, T, L, trigger bar, next bar, fill `(ts, price, maker)`, resting `(kind, price)`)
type Row = (
    &'static str,
    i32,
    f64,
    f64,
    [f64; 4],
    [f64; 4],
    Option<(i64, f64, bool)>,
    Option<(OrderKind, f64)>,
);

/// Submitted on bar 0 (ts 1 000), so bar 1 (ts 2 000) is the first that can trigger it. Row 2 is
/// the MEASURED case (`order_harbor_stop_limit`, 471 of 471: a limit above its trigger, filled at
/// the trigger, no gap); the gap row after it is assumption 1, the L-below-T rows assumption 2, the
/// gap-past-L rows assumption 3.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    ("buy, L one tick below T", 1, 101.0, 100.0, [99.0, 102.0, 98.0, 101.0], FLAT, Some((2_000, 100.0, false)), None),
    ("buy, L above T: A = T (the measured case)", 1, 101.0, 103.0, [99.0, 102.0, 98.0, 101.0], FLAT, Some((2_000, 101.0, false)), None),
    ("buy, L above T, gap past T: A = open", 1, 101.0, 103.0, [102.0, 104.0, 101.5, 103.0], FLAT, Some((2_000, 102.0, false)), None),
    ("buy, gap past L, back to L", 1, 101.0, 100.0, [103.0, 105.0, 99.5, 104.0], FLAT, Some((2_000, 100.0, false)), None),
    ("buy, gap past L: rests as the Limit, a maker next bar", 1, 101.0, 100.0, [103.0, 105.0, 102.0, 104.0], [101.0, 102.0, 99.0, 100.0], Some((3_000, 100.0, true)), None),
    ("buy, L below T not reached: the Limit's gap improvement next bar", 1, 101.0, 99.0, [100.0, 102.0, 99.5, 101.0], [98.5, 99.5, 98.0, 99.0], Some((3_000, 98.5, true)), None),
    ("buy, gap past L, never back: rests as the Limit at L", 1, 101.0, 100.0, [103.0, 105.0, 102.0, 104.0], [103.0, 104.0, 101.0, 103.0], None, Some((OrderKind::Limit, 100.0))),
    ("buy, not triggered: still the stop-limit", 1, 101.0, 100.0, [99.0, 100.5, 98.0, 100.0], [100.0, 100.8, 99.0, 100.0], None, Some((OrderKind::StopLimit, 101.0))),
    ("sell, L one tick above T", -1, 99.0, 100.0, [101.0, 102.0, 98.0, 99.0], FLAT, Some((2_000, 100.0, false)), None),
    ("sell, L below T: A = T", -1, 99.0, 97.0, [101.0, 102.0, 98.0, 99.0], FLAT, Some((2_000, 99.0, false)), None),
    ("sell, L below T, gap past T: A = open", -1, 99.0, 97.0, [98.0, 98.5, 96.0, 97.0], FLAT, Some((2_000, 98.0, false)), None),
    ("sell, gap past L: rests as the Limit, a maker next bar", -1, 99.0, 100.0, [97.0, 99.5, 96.0, 98.0], [99.0, 101.0, 98.5, 100.0], Some((3_000, 100.0, true)), None),
];

#[test]
fn the_stop_limit_law_on_bars_with_its_fee_class() {
    for &(case, side, trigger, limit, b1, b2, fill, rest) in ROWS {
        let bars = vec![bar(1_000, FLAT), bar(2_000, b1), bar(3_000, b2)];
        let engine = run_bars(params(), bars, stop_limit(side, 1.0, trigger, limit));
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
        let want: Vec<_> = rest
            .map(|(kind, px)| {
                let limit = (kind == OrderKind::StopLimit).then_some(limit);
                (kind, Some(px), limit)
            })
            .into_iter()
            .collect();
        assert_eq!(resting(&engine), want, "{case}: what rests");
    }
}

/// The fixed slippage the runs above leave at zero.
pub(crate) const AMOUNT: f64 = 0.5;

/// **A stop-limit pays no fixed slippage**, as a taker on its trigger event and as the maker it
/// turns into: every row fills at the price it fills at with no amount, to the bit. A plain stop at
/// the same trigger in the same run pays it, so the amount is live.
#[test]
fn a_stop_limit_pays_no_fixed_slippage_on_either_fee_class() {
    let amount = || StrategyEngineConfig { slippage_amount: AMOUNT, ..params() };
    for &(case, side, trigger, limit, b1, b2, fill, _) in ROWS {
        let bars = vec![bar(1_000, FLAT), bar(2_000, b1), bar(3_000, b2)];
        let engine = run_bars(amount(), bars, stop_limit(side, 1.0, trigger, limit));
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
    }
    let bars = vec![bar(1_000, FLAT), bar(2_000, ROWS[0].4), bar(3_000, FLAT)];
    let stop = Once::new(|b| b.submit_stop(SYM, 1, 1.0, 101.0, 0.0, true));
    let engine = run_bars(amount(), bars, stop);
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("a plain stop pays it", &engine.strategy.fills[0], 2_000, 101.0 + AMOUNT, false);
}

// ---- 2. granular sub-bars ----

/// The coarse bar 1 of [`granular_run`]: it triggers a buy at 101 and reaches 100, but its sub-bars
/// say the low came FIRST.
const B1: [f64; 4] = [100.5, 101.5, 99.5, 100.1];

/// A buy stop-limit (T 101, L 100) over bar 1, with `subs` as its detail tape (none: the coarse
/// lane). Bar 2 never comes back to 100.
fn granular_run(subs: Vec<Bar>) -> StrategyEngine<Once> {
    let bars = vec![bar(1_000, FLAT), bar(2_000, B1), bar(3_000, [100.1, 100.4, 100.05, 100.2])];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    run_bars(p, bars, stop_limit(1, 1.0, 101.0, 100.0))
}

/// The coarse bar fills on its range (assumption 2: the low counts though it came before the
/// trigger), a taker on the trigger bar. The sub-bars walk the path: the trigger sub-bar does not
/// reach L, so the order rests as the `Limit`, and that conversion survives into the next sub-bar,
/// which fills it as a maker. Without that next sub-bar it never fills.
#[test]
fn granular_sub_bars_walk_the_law_and_keep_the_conversion() {
    let coarse = granular_run(Vec::new());
    assert_eq!(coarse.strategy.fills.len(), 1, "{:?}", coarse.strategy.fills);
    assert_fill("coarse: the range rule", &coarse.strategy.fills[0], 2_000, 100.0, false);

    let low_first = bar(2_000, [100.5, 100.8, 99.5, 100.0]); // under L, before the trigger
    let trigger = bar(2_300, [100.2, 101.5, 100.2, 101.2]); // triggers, never reaches L
    let back = bar(2_600, [101.2, 101.3, 99.9, 100.1]); // the Limit at 100 fills
    let walked = granular_run(vec![low_first.clone(), trigger.clone(), back]);
    assert_eq!(walked.strategy.fills.len(), 1, "{:?}", walked.strategy.fills);
    assert_fill("granular: the Limit fills later", &walked.strategy.fills[0], 2_600, 100.0, true);

    let never = granular_run(vec![low_first, trigger]);
    assert!(never.strategy.fills.is_empty(), "{:?}", never.strategy.fills);
    assert_eq!(resting(&never), vec![(OrderKind::Limit, Some(100.0), None)]);
}

// ---- 3. the tick lane and the L2 lane ----

/// The quote law: a buy triggers on an ask at or above T, fills at the ask when it is within L,
/// else rests as the `Limit` at L and fills on a later ask within it, as a maker.
#[test]
fn the_tick_lane_triggers_and_limits_on_the_quote() {
    let cases = [
        (
            "buy, the trigger ask within L",
            stop_limit(1, 1.0, 101.0, 101.5),
            vec![q(1, 99.0, 100.0), q(2, 100.0, 101.0)],
            (2, 101.0, false),
        ),
        (
            "buy, the trigger ask past L: the Limit fills later",
            stop_limit(1, 1.0, 101.0, 100.5),
            vec![q(1, 99.0, 100.0), q(2, 100.0, 101.0), q(3, 100.5, 101.5), q(4, 99.5, 100.5)],
            (4, 100.5, true),
        ),
        (
            "sell, the trigger bid within L",
            stop_limit(-1, 1.0, 99.0, 98.5),
            vec![q(1, 100.0, 101.0), q(2, 98.8, 99.8)],
            (2, 98.8, false),
        ),
    ];
    for (case, strategy, ticks, (ts, px, maker)) in cases {
        let engine = run_ticks(FillModelKind::Tick, ticks, strategy);
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, maker);
    }
}

/// The L2 tape: a buy stop-limit (T 101, L 102) is submitted on the quote at ts 2; the quote at
/// ts 4 triggers it against asks of 1 at 101, 1 at 102 and 5 at 103; the book at ts 5 shows 5 at
/// 102. `upto` cuts the tape.
fn l2_run(size: f64, upto: usize) -> StrategyEngine<Once> {
    let tape = vec![
        book(1, &[(99.0, 5.0)], &[(100.0, 5.0)]),
        q(2, 99.0, 100.0),
        book(3, &[(100.0, 5.0)], &[(101.0, 1.0), (102.0, 1.0), (103.0, 5.0)]),
        q(4, 100.0, 101.0),
        book(5, &[(101.0, 5.0)], &[(102.0, 5.0)]),
        q(6, 101.0, 102.0),
    ];
    let tape = tape.into_iter().take(upto).collect();
    run_ticks(FillModelKind::L2Book, tape, stop_limit(1, size, 101.0, 102.0))
}

/// The trigger is the quote's, the price the book walk capped at L: two units walk 101 and 102.
/// Three units find only two within L, so the order rests as the `Limit` at L, and the next book
/// fills it there as a maker (the `Limit`'s own capped walk).
#[test]
fn the_l2_lane_walks_the_book_up_to_the_limit() {
    let fits = l2_run(2.0, 6);
    assert_eq!(fits.strategy.fills.len(), 1, "{:?}", fits.strategy.fills);
    assert_fill("two units within L", &fits.strategy.fills[0], 4, 101.5, false);

    let short = l2_run(3.0, 4);
    assert!(short.strategy.fills.is_empty(), "{:?}", short.strategy.fills);
    assert_eq!(resting(&short), vec![(OrderKind::Limit, Some(102.0), None)], "rests as the Limit");

    let later = l2_run(3.0, 6);
    assert_eq!(later.strategy.fills.len(), 1, "{:?}", later.strategy.fills);
    assert_fill("the Limit's walk", &later.strategy.fills[0], 6, 102.0, true);
}

/// The queued tape, all trades (so the bar law prices each one-price event): the book shows 10
/// bid at 100; the trade at 3 prints 101; the book at 4 grows the 100 level to 20; trades at 100
/// of 4, 8 and 10 follow. `queue` arms the risk-averse queue model; `upto` cuts the tape.
fn queued_run(
    queue: bool,
    upto: usize,
    submit: impl Fn(&mut OrderBrokerSimulator) + 'static,
) -> StrategyEngine<Once> {
    let tr = |ts: i64, px: f64, qty: f64| vike_simulator::test_support::trade(SYM, ts, px, qty);
    let tape = vec![
        book(1, &[(100.0, 10.0)], &[(101.0, 5.0)]),
        tr(2, 100.5, 1.0),
        tr(3, 101.0, 1.0),
        book(4, &[(100.0, 20.0)], &[(101.0, 5.0)]),
        tr(5, 100.0, 4.0),
        tr(6, 100.0, 8.0),
        tr(7, 100.0, 10.0),
    ];
    let p = StrategyEngineConfig {
        queue_model: queue.then_some(QueueModelKind::RiskAverse),
        ..params()
    };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], Once::new(submit), p);
    engine.run_ticks(&[(SYM.to_string(), tape.into_iter().take(upto).collect::<Vec<_>>())]);
    engine
}

/// The queue-model tick lane sorts an order by its kind when it reaches it, so a buy stop-limit
/// (T 101, L 100, two units) is a taker there: the trade at 3 converts it to the `Limit` at 100,
/// which has no queue identity yet (`qid` 0). The lane's queued branch first sees it at 5 and
/// seeds it at the BACK of the level as the book shows it then, 20: the trades at 100 consume 4
/// and 8 ahead of it, and the third trade's excess fills its two units, as a maker. A plain buy
/// limit at 100 sent on the same tick joined the level at 3, behind 10 (the growth at 4 is behind
/// it), and fills one trade earlier; without the queue model the converted `Limit` fills on the
/// first touch.
#[test]
fn the_queue_lane_seeds_a_converted_stop_limit_at_the_back_of_its_level() {
    let stop_limit =
        |b: &mut OrderBrokerSimulator| b.submit_stop_limit(SYM, 1, 2.0, 101.0, 100.0, 0.0, true);
    let shape = |engine: &StrategyEngine<Once>| -> Vec<(OrderKind, Option<f64>, bool)> {
        engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.price, o.qid != 0)).collect()
    };

    let converted = queued_run(true, 4, stop_limit);
    assert!(converted.strategy.fills.is_empty(), "{:?}", converted.strategy.fills);
    assert_eq!(shape(&converted), [(OrderKind::Limit, Some(100.0), false)], "no queue state yet");

    let seen = queued_run(true, 5, stop_limit);
    assert!(seen.strategy.fills.is_empty(), "4 traded against 20 ahead: {:?}", seen.strategy.fills);
    assert_eq!(shape(&seen), [(OrderKind::Limit, Some(100.0), true)], "seeded on first sight");

    let queued = queued_run(true, 7, stop_limit);
    assert_eq!(queued.strategy.fills.len(), 1, "{:?}", queued.strategy.fills);
    assert_fill("behind 20, the third trade", &queued.strategy.fills[0], 7, 100.0, true);
    assert_eq!(queued.strategy.fills[0].size, 2.0);

    let plain = queued_run(true, 7, |b| b.submit_limit(SYM, 1, 2.0, 100.0, 0.0, true, None));
    assert_eq!(plain.strategy.fills.len(), 1, "{:?}", plain.strategy.fills);
    assert_fill("a plain limit, behind 10", &plain.strategy.fills[0], 6, 100.0, true);

    let frozen = queued_run(false, 7, stop_limit);
    assert_eq!(frozen.strategy.fills.len(), 1, "{:?}", frozen.strategy.fills);
    assert_fill("no queue model: the first touch", &frozen.strategy.fills[0], 5, 100.0, true);
}

// ---- 4. the SL/TP cap with a stop-limit protective exit ----

/// Long 1 at bar 1's open, with two exits: a stop-limit sell (T 95, L 94) and a take-profit sell
/// limit at 110. Bar 2 spans both. The stop-limit is adverse, so it fills first (A = T = 95, L
/// reached) and the take-profit is capped to the nothing left: flat, never reversed, and the bar
/// counted as a guessed one.
#[test]
fn a_stop_limit_exit_fills_first_and_capabilities_the_take_profit() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [100.0, 101.0, 99.0, 100.0]),
        bar(3_000, [100.0, 115.0, 90.0, 100.0]),
        bar(4_000, FLAT),
    ];
    let bracket = Once::new(|b| {
        b.submit(SYM, 1, 1.0, 0.0, true, None);
        b.submit_stop_limit(SYM, -1, 1.0, 95.0, 94.0, 0.0, true);
        b.submit_limit(SYM, -1, 1.0, 110.0, 0.0, true, None);
    });
    let engine = run_bars(params(), bars, bracket);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "the entry and the stop-limit only: {fills:?}");
    assert_fill("entry", &fills[0], 2_000, 100.0, false);
    assert_fill("the stop-limit exit", &fills[1], 3_000, 95.0, false);
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(engine.core.pending_of(SYM).is_empty(), "the capped take-profit left the book");
    assert_eq!(engine.core.intrabar_both_hit, 1);
}

// ---- 5. the session deferral ----

/// The FX weekend (`session_law.rs`'s tape): bars 2 to 4 are shut, bar 5 is the Sunday reopen.
pub(crate) fn weekend_tape() -> Vec<Bar> {
    [
        (ms(2024, 1, 5, 20), 100.0),
        (ms(2024, 1, 5, 21), 110.0),
        (ms(2024, 1, 5, 22), 120.0),
        (ms(2024, 1, 6, 12), 130.0),
        (ms(2024, 1, 7, 20), 140.0),
        (ms(2024, 1, 7, 21), 150.0),
        (ms(2024, 1, 8, 0), 160.0),
    ]
    .into_iter()
    .map(|(ts, px)| bar(ts, [px, px + 1.0, px - 1.0, px]))
    .collect()
}

pub(crate) fn session_params(gate: bool) -> StrategyEngineConfig {
    StrategyEngineConfig { default_venue: Some("dukascopy".into()), session_gate: gate, ..params() }
}

/// A buy stop-limit (T 125, L 155) triggers on the shut Saturday bar; the gate defers it through
/// bars 2 to 4 and it fills at the reopen by the law on THAT bar (A = the gapped open 150, within
/// L), a taker. The gate off fills it on the Saturday.
#[test]
fn a_shut_market_defers_a_stop_limit_to_the_reopen() {
    let gated = run_bars(session_params(true), weekend_tape(), stop_limit(1, 1.0, 125.0, 155.0));
    assert_eq!(gated.strategy.fills.len(), 1, "{:?}", gated.strategy.fills);
    assert_fill("the reopen", &gated.strategy.fills[0], ms(2024, 1, 7, 21), 150.0, false);
    assert_eq!(gated.core.session_deferrals, 3);

    let open = run_bars(session_params(false), weekend_tape(), stop_limit(1, 1.0, 125.0, 155.0));
    assert_eq!(open.strategy.fills.len(), 1, "{:?}", open.strategy.fills);
    assert_fill("ungated: the Saturday", &open.strategy.fills[0], ms(2024, 1, 6, 12), 130.0, false);
    assert_eq!(open.core.session_deferrals, 0);
}

// ---- 6. the intake ----

/// Terms `vike_model::order_terms_refusal` refuses never rest: the verb drops them in that rule's
/// own words, as the live strategy drain refuses them.
#[test]
fn terms_the_order_rule_refuses_are_dropped_in_its_words() {
    for (case, trigger, limit) in
        [("no limit above zero", 101.0, 0.0), ("a trigger that is not a number", f64::NAN, 100.0)]
    {
        let bars = vec![bar(1_000, FLAT), bar(2_000, FLAT)];
        let engine = run_bars(params(), bars, stop_limit(1, 1.0, trigger, limit));
        let why = vike_model::order_terms_refusal(
            &OrderType::StopLimit,
            Some(limit),
            Some(trigger),
            None,
            None,
            None,
            None,
        );
        let why = why.expect(case);
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop { symbol: SYM.to_string(), reason: why, size: 1.0, weight: 0.0 }],
            "{case}"
        );
    }
}

/// The pre-trade gate judges the request live sends: its price is the LIMIT. One unit at L 300
/// (T 150, the mark 100) breaks a 200 notional cap that its trigger and the mark would pass.
#[test]
fn the_pre_trade_gate_judges_a_stop_limit_on_its_limit() {
    let run = |cap: Option<f64>| {
        let p = StrategyEngineConfig {
            default_venue: Some("TEST".into()),
            risk_limits: cap
                .map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() }),
            ..params()
        };
        run_bars(p, vec![bar(1_000, FLAT), bar(2_000, FLAT)], stop_limit(1, 1.0, 150.0, 300.0))
    };
    let refused = run(Some(200.0));
    assert!(refused.core.pending_of(SYM).is_empty());
    let reasons: Vec<_> = refused.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["over-max-notional"]);

    let rested = run(None);
    assert_eq!(resting(&rested), vec![(OrderKind::StopLimit, Some(150.0), Some(300.0))]);
}
