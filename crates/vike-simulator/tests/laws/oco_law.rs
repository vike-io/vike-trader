//! The OCO law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_oco`] (and [`OrderBrokerSimulator::submit_group`]
//! under it), the order-client-id-linked book the live core and the paper book resolve with
//! (`vike_execution::ContingencyBook::on_fill`), keyed here by each leg's handle. Pinned:
//!
//! 1. one leg's fill cancels the other, which then never fills;
//! 2. TWO ENTRIES triggered by one bar (a straddle) fill in the bar's PATH order, open, the nearer
//!    extreme, the farther one, close (an exact tie goes low first, as the trailing walk does), so
//!    the first one's fill cancels the other: the plan's assumption for entries, on the coarse lane
//!    and on the shared-cash gated lane;
//! 3. two EXITS over a position triggered by one bar keep `resolve_intrabar_fills`' order, the stop
//!    first: the path order is for entries only;
//! 4. two ENTRIES that both reduce a position in one bar take the one-bar cap in the PATH order
//!    (`vike_fills::fill_resolution::resolve_intrabar_fills_path_first`): the leg the path reaches
//!    first gets the whole cap, never the stop's leftover;
//! 5. the intake: the handles in leg order, a plain order's qid untouched (0), and a list refused
//!    whole, in words, with nothing resting.

use vike_marketdata::Bar;
use vike_model::{
    ContingencyType, Fill, OrderKind, OrderList, OrderListLeg, OrderType, Strategy, TimeInForce,
};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{FLAT, Once, SYM, bar, params, run_bars};

/// A GTC leg with no link yet (`submit_oco` sets the links).
pub(crate) fn leg(
    side: i32,
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
    trigger: Option<f64>,
) -> OrderListLeg {
    OrderListLeg {
        side,
        qty,
        order_type,
        price,
        trigger_price: trigger,
        trail: None,
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// What rests at the end, as `(kind, side, price, size)`.
pub(crate) fn book_of(engine: &StrategyEngine<Once>) -> Vec<(OrderKind, i32, Option<f64>, f64)> {
    engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.side, o.price, o.size)).collect()
}

/// A straddle: a buy stop at 105 and a sell stop at 95, one cancelling the other.
fn straddle() -> Once {
    Once::new(|b| {
        let buy = leg(1, 1.0, OrderType::Stop, None, Some(105.0));
        let sell = leg(-1, 1.0, OrderType::Stop, None, Some(95.0));
        assert_eq!(b.submit_oco(SYM, buy, sell), vec!["g1".to_string(), "g2".to_string()]);
    })
}

/// The fills as `(side, price)`.
fn fills(engine: &StrategyEngine<Once>) -> Vec<(i32, f64)> {
    engine.strategy.fills.iter().map(|f| (f.side, f.price)).collect()
}

#[test]
fn one_legs_fill_cancels_the_other_which_never_fills() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [100.0, 106.0, 99.0, 104.0]), // the buy stop only
        bar(3_000, [104.0, 104.0, 90.0, 91.0]),  // would cross the sell stop, which is gone
    ];
    let engine = run_bars(params(), bars, straddle());
    assert_eq!(fills(&engine), vec![(1, 105.0)], "the buy stop alone");
    assert!(book_of(&engine).is_empty(), "the sell stop was cancelled on the buy's fill");
    assert_eq!(engine.core.position_of(SYM).size, 1.0);
}

/// `(bar, the side that fills)`: the nearer extreme first, a tie low first.
const PATHS: &[([f64; 4], i32)] = &[
    ([100.0, 106.0, 90.0, 100.0], 1), // the high is nearer: the buy stop first
    ([100.0, 110.0, 94.0, 100.0], -1), // the low is nearer: the sell stop first
    ([100.0, 106.0, 94.0, 100.0], -1), // an exact tie: low first
];

#[test]
fn two_entries_in_one_bar_fill_in_the_bars_path_order_and_the_first_cancels_the_other() {
    for &(ohlc, side) in PATHS {
        for cash_gate in [false, true] {
            let p = StrategyEngineConfig { cash_gate, ..params() };
            let engine = run_bars(p, vec![bar(1_000, FLAT), bar(2_000, ohlc)], straddle());
            let px = if side > 0 { 105.0 } else { 95.0 };
            let case = format!("{ohlc:?}, cash gate {cash_gate}");
            assert_eq!(fills(&engine), vec![(side, px)], "{case}: one entry, the path's first");
            assert!(book_of(&engine).is_empty(), "{case}: the other is cancelled, not resting");
            assert_eq!(engine.core.intrabar_both_hit, 0, "{case}: two entries cap nothing");
        }
    }
}

/// A market buy of `qty` on the first event, then `then` from its fill (exits over that long).
pub(crate) struct LongThen {
    pub(crate) once: Once,
    then: Box<dyn Fn(&mut OrderBrokerSimulator)>,
    placed: bool,
}

impl LongThen {
    pub(crate) fn new(qty: f64, then: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Self {
        let once = Once::new(move |b| b.submit(SYM, 1, qty, 0.0, true, None));
        Self { once, then: Box::new(then), placed: false }
    }

    /// The fills as `(side, price, size)`.
    pub(crate) fn fills(&self) -> Vec<(i32, f64, f64)> {
        self.once.fills.iter().map(|f| (f.side, f.price, f.size)).collect()
    }
}

impl Strategy<OrderBrokerSimulator> for LongThen {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        self.once.on_bar(ctx, bar);
    }
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, q: &vike_marketdata::TickQuote) {
        self.once.on_quote_tick(ctx, q);
    }
    fn on_fill(&mut self, ctx: &mut OrderBrokerSimulator, fill: &Fill) {
        self.once.on_fill(ctx, fill);
        if !self.placed {
            self.placed = true;
            (self.then)(ctx);
        }
    }
}

/// A reduce-only exit leg of a long.
pub(crate) fn exit(
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
    trigger: Option<f64>,
) -> OrderListLeg {
    OrderListLeg { reduce_only: true, ..leg(-1, qty, order_type, price, trigger) }
}

#[test]
fn two_exits_in_one_bar_keep_the_adverse_first_order_and_the_stop_cancels_the_target() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, FLAT), // the long fills at 100, the exits rest after it
        bar(3_000, [100.0, 111.0, 94.0, 100.0]), // the high is nearer, yet the stop goes first
    ];
    let strategy = LongThen::new(1.0, |b| {
        let stop = exit(1.0, OrderType::Stop, None, Some(95.0));
        let target = exit(1.0, OrderType::Limit, Some(110.0), None);
        assert_eq!(b.submit_oco(SYM, stop, target).len(), 2);
    });
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strategy, params());
    engine.run();
    assert_eq!(engine.strategy.fills(), vec![(1, 100.0, 1.0), (-1, 95.0, 1.0)], "never the target");
    assert!(engine.core.pending_of(SYM).is_empty(), "the target was cancelled");
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    assert_eq!(engine.core.intrabar_both_hit, 1, "the bracket cap's bar, counted as before");
}

#[test]
fn a_straddles_path_first_leg_takes_the_whole_cap() {
    // over a long of 1, two OCO entries of 2 that both reduce it: a sell limit at 110 and a sell
    // stop at 95; the bar's high is nearer, so the path reaches 110 first
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, FLAT), // the long fills at 100, the pair rests after it
        bar(3_000, [100.0, 111.0, 88.0, 100.0]),
    ];
    let strategy = LongThen::new(1.0, |b| {
        let limit = leg(-1, 2.0, OrderType::Limit, Some(110.0), None);
        let stop = leg(-1, 2.0, OrderType::Stop, None, Some(95.0));
        assert_eq!(b.submit_oco(SYM, limit, stop).len(), 2);
    });
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strategy, params());
    engine.run();
    // adverse-first would give the stop the cap of 1 and the limit nothing
    assert_eq!(
        engine.strategy.fills(),
        vec![(1, 100.0, 1.0), (-1, 110.0, 1.0)],
        "the limit, whole"
    );
    assert!(engine.core.pending_of(SYM).is_empty(), "the stop was cancelled by the limit's fill");
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    assert_eq!(engine.core.intrabar_both_hit, 1, "the cap applied, in the path's order");
}

#[test]
fn a_list_refused_in_words_rests_nothing_and_a_plain_order_keeps_queue_id_zero() {
    // a stop leg with no trigger: the order-terms rule's words, and the OTHER leg does not rest
    let engine = run_bars(
        params(),
        vec![bar(1_000, FLAT), bar(2_000, FLAT)],
        Once::new(|b| {
            b.submit_limit(SYM, 1, 1.0, 90.0, 0.0, true, None);
            let buy = leg(1, 1.0, OrderType::Stop, None, None);
            let sell = leg(-1, 1.0, OrderType::Limit, Some(110.0), None);
            assert!(b.submit_oco(SYM, buy, sell).is_empty());
            // links the list cannot resolve: build_order_list's words
            let mut lone = leg(1, 1.0, OrderType::Limit, Some(99.0), None);
            lone.contingency = Some(ContingencyType::Oco);
            let spec = OrderList {
                venue: String::new(),
                symbol: SYM.to_string(),
                legs: vec![lone, leg(-1, 1.0, OrderType::Limit, Some(101.0), None)],
                account: None,
            };
            assert!(b.submit_group(&spec).is_empty());
        }),
    );
    let why: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(
        why,
        vec![
            "a stop order needs a trigger price above zero",
            "leg 0 needs OCO or OUO exactly when it links other legs",
        ]
    );
    let resting = engine.core.pending_of(SYM);
    assert_eq!(resting.len(), 1, "only the plain limit rests");
    assert_eq!((resting[0].price, resting[0].qid), (Some(90.0), 0), "a plain order keeps qid 0");
}
