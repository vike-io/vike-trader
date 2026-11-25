//! A PORTFOLIO run (`StrategyEngine::new_mounted`): several strategy files over ONE `OrderBrokerSimulator`,
//! each hearing its own bars, fills and order ends, in the live drain's order (`crates/vike-simulator/src/mounted.rs`'s
//! module doc).
//!
//! What each test pins:
//!
//! - ONE mount over every slot IS the plain run, bit for bit (the record, `dropped`, the closed rows, the
//!   ledger log and every hook the strategy heard with what it read), for an untagged moving-average
//!   strategy and for one that tags, amends, cancels, places groups, exit legs and at-close orders
//!   through the order seam, under a fee schedule, a volume cap and in-bar rules; and that mount's book
//!   is the account's: its rows the positions bit for bit, its closed trades the record's, its PnL the
//!   account's;
//! - each mount books its own fills (its rows, the closed trades it reads and reports, its PnL curve),
//!   the account nets them, and with every fill a mount's the residual is nothing;
//! - two mounts may name the same tag: each hears its own order's fill and end, and a cancel by tag
//!   reaches its own order alone;
//! - an untagged order's fill reaches the mount that placed it alone, on a slot both trade;
//! - a mount's leverage cap (rule v3) is judged on its own book (a sibling's holding does not count, a
//!   sibling with no cap is unaffected), the covered reduce passes, each refusal counted for its report; a
//!   reversal is netted on the order's row; a mount closes its own position whatever the account holds
//!   and cannot "reduce" a sibling's; each order of one hook is judged ALONE; a reduce-only order is
//!   judged on its STATED size unless it covers the mount's own row (a stop sent beside its entry is
//!   judged alone); an amend on its growth, a newly named price on all it rests, a parked child's with its
//!   parent; a timed order at its release, ending `Canceled` as live's, its refusal leaving the mount's
//!   `""` list leg filed; an order list as one order: an OTO child with its parent when the list is placed
//!   (on both bar lanes), each leg at its own price, OCO legs as alternatives, a far take-profit no
//!   exposure; a combo as one unit; an order priced through the market at the market;
//! - an order-list leg under `""` is filed under its mount's untagged key: its fill reaches and books into
//!   its own mount, and the mount's own `cancel("")` reaches it, no sibling's;
//! - a mount's loss budget latches it alone, before it steps: its orders cancelled (a tagged one's end
//!   heard), with `Flatten` its own book closed by a fill it hears, with `CancelOnly` its position
//!   left; its later intents dropped unheard while its sibling trades on;
//! - under an armed `RiskGate` the mount listed first faces the budget first, and the later one hears
//!   the gate's denial, whichever strategy it is;
//! - `""` names the mount's own symbol, and a symbol of the run the mount does not trade is refused by
//!   name, while a read by name stays an account read;
//! - the index reads are the mount's own, its own symbol first;
//! - no broker key (`\u{1f}`) reaches a strategy, `dropped` or the record.

use std::cell::RefCell;
use std::rc::Rc;

use vike_analytics::TradingRecord;
use vike_marketdata::Bar;
use vike_model::{
    ContingencyType, FeeSchedule, Fill, OrderBroker, OrderEvent, OrderEventKind, OrderListLeg,
    OrderPlacement, OrderType, Strategy, TimeInForce,
};
use vike_simulator::test_support::gen_bars;
use vike_simulator::{
    InBarRules, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, StrategyEngineMount,
    StrategyEngineMountReport,
};

/// What a strategy heard and read, one line a hook, shared with the test after the run.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

/// No broker key, raw or as `Debug` escapes it.
fn assert_no_key(what: &str, text: &str) {
    let found = text.contains('\u{1f}') || text.contains("\\u{1f}");
    assert!(!found, "a broker key in {what}: {text}");
}

/// Two symbols over one 40-bar walk, `B` its mirror, so the two move apart.
fn two_symbols() -> Vec<(String, Vec<Bar>)> {
    let a = gen_bars(40, None);
    let mirror = |b: &Bar| Bar {
        open: 300.0 - b.open,
        high: 300.0 - b.low,
        low: 300.0 - b.high,
        close: 300.0 - b.close,
        ..b.clone()
    };
    let b = a.iter().map(mirror).collect();
    vec![("A".to_string(), a), ("B".to_string(), b)]
}

/// `n` flat bars at 100 on `A`, volume 1 000.
fn flat_a(n: usize) -> Vec<(String, Vec<Bar>)> {
    vec![("A".to_string(), vike_simulator::test_support::flat_minute_bars(n, 100.0, 1_000.0))]
}

fn mount(
    strategy: impl Strategy<OrderBrokerSimulator> + 'static,
    symbols: &[&str],
) -> StrategyEngineMount {
    StrategyEngineMount {
        strategy: Box::new(strategy),
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
        budget: None,
    }
}

/// Every hook and read a one-mount == plain comparison holds equal.
fn heard(log: &Log, b: &OrderBrokerSimulator, line: String) {
    let reads = format!(
        "{:?} {:?} {:?} {:?}",
        OrderBroker::symbols(b),
        OrderBroker::position(b, ""),
        OrderBroker::equity(b),
        OrderBroker::closed_trades(b).map(<[_]>::len)
    );
    log.borrow_mut().push(format!("{line} | {reads}"));
}

/// Long 2 while the 3-bar mean of a bar's symbol is above its 8-bar mean, flat otherwise, by untagged
/// market orders.
struct MeanCross {
    log: Log,
}

impl Strategy<OrderBrokerSimulator> for MeanCross {
    fn warmup(&self) -> usize {
        8
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.unwrap_or_default();
        let closes: Vec<f64> = OrderBroker::bars(b, &symbol).iter().map(|x| x.close).collect();
        let mean = |k: usize| closes[closes.len() - k..].iter().sum::<f64>() / k as f64;
        let held = OrderBroker::position(b, &symbol);
        let want = if mean(3) > mean(8) { 2.0 } else { 0.0 };
        if want != held {
            let side = if want > held { 1 } else { -1 };
            let qty = (want - held).abs();
            OrderBroker::place(b, &symbol, "", side, qty, OrderPlacement::Market, false);
        }
        heard(&self.log, b, format!("bar {symbol} {} {held:?}", b.index));
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, fill: &Fill) {
        heard(&self.log, b, format!("fill {fill:?}"));
    }
    fn on_order_event(&mut self, b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        heard(&self.log, b, format!("end {event:?}"));
    }
}

/// A reduce-only sell leg of 1 linked OCO to `other`.
fn exit(
    order_type: OrderType,
    price: Option<f64>,
    trigger: Option<f64>,
    other: usize,
) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type,
        price,
        trigger_price: trigger,
        trail: None,
        reduce_only: true,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: Some(ContingencyType::Oco),
        linked: vec![other],
        order_client_id: None,
    }
}

/// Every seam verb on each symbol's bars, by tag: a limit amended up and through the market, a far
/// limit cancelled, an OCO pair of exits one leg of which is cancelled, an exit leg, an order at the
/// close and a reduce-only market on `""`.
struct SeamWork {
    log: Log,
}

impl Strategy<OrderBrokerSimulator> for SeamWork {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        let s = bar.symbol.unwrap_or_default();
        let close = bar.close;
        let t = |name: &str| format!("{name}-{s}");
        match b.index {
            8 => {
                let order = OrderPlacement::Limit { price: close * 0.995 };
                OrderBroker::place(b, &s, &t("entry"), 1, 2.0, order, false);
            }
            9 => OrderBroker::amend(b, &t("entry"), Some(3.0), Some(close * 1.01), None),
            10 => {
                let order = OrderPlacement::Limit { price: close * 0.5 };
                OrderBroker::place(b, &s, &t("wait"), 1, 1.0, order, false);
            }
            11 => {
                let legs = vec![
                    (t("sl"), exit(OrderType::Stop, None, Some(close * 0.97), 1)),
                    (t("tp"), exit(OrderType::Limit, Some(close * 1.03), None, 0)),
                ];
                OrderBroker::place_group(b, &s, legs);
                OrderBroker::cancel(b, &t("wait"));
            }
            14 => OrderBroker::cancel(b, &t("tp")),
            16 => {
                let order = OrderPlacement::TakeProfit { trigger: close * 1.02, limit: None };
                OrderBroker::place_exit_leg(b, &s, &t("x"), &t("xt"), -1, 1.0, order);
            }
            18 => OrderBroker::place_at_close(b, &s, &t("late"), 1, 1.0, false),
            20 => OrderBroker::place(b, "", &t("own"), -1, 1.0, OrderPlacement::Market, true),
            _ => {}
        }
        heard(&self.log, b, format!("bar {s} {}", b.index));
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, fill: &Fill) {
        heard(&self.log, b, format!("fill {fill:?}"));
    }
    fn on_order_event(&mut self, b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        heard(&self.log, b, format!("end {event:?}"));
    }
}

/// A fee schedule, a volume cap that clips the 2- and 3-unit orders, three in-bar rules and the
/// closed-trade report.
fn costed() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 100_000.0,
        fee_schedule: Some(FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 }),
        volume_limit: Some(0.1),
        in_bar: InBarRules {
            path_walk: true,
            queue_order: true,
            price_order: true,
            ..InBarRules::default()
        },
        closed_trades: Some(vike_trade_ledger::TradeClosedTerms::default()),
        ..StrategyEngineConfig::default()
    }
}

/// What a run leaves that one mount must leave as the plain run does, and its record.
fn outcome<S: Strategy<OrderBrokerSimulator>>(
    engine: &mut StrategyEngine<S>,
    log: &Log,
) -> (Vec<String>, TradingRecord) {
    let record = engine.run();
    let seen = vec![
        format!("{record:?}"),
        format!("{:?}", engine.core.dropped),
        format!("{:?}", OrderBroker::closed_trades(&engine.core)),
        format!("{:?}", engine.core.ledger_events),
        format!("{:?}", log.borrow()),
    ];
    (seen, record)
}

/// The run's PnL against the mounts' sum at the end: what no mount owns.
fn residual(record: &TradingRecord, cash: f64, reports: &[StrategyEngineMountReport]) -> f64 {
    let mounts: f64 = reports.iter().map(|r| r.pnl_curve.last().copied().unwrap_or(0.0)).sum();
    record.final_equity - cash - mounts
}

fn assert_one_mount_is_the_plain_run<S: Strategy<OrderBrokerSimulator> + 'static>(
    strategy: impl Fn(Log) -> S,
) {
    let plain_log = log();
    let mut plain = StrategyEngine::new(two_symbols(), strategy(plain_log.clone()), costed());
    let (plain, _) = outcome(&mut plain, &plain_log);
    let mounted_log = log();
    let mounts = vec![mount(strategy(mounted_log.clone()), &["A", "B"])];
    let mut engine = StrategyEngine::new_mounted(two_symbols(), mounts, costed());
    let (mounted, record) = outcome(&mut engine, &mounted_log);
    let names = ["the record", "dropped", "the closed rows", "the ledger log", "what it heard"];
    for ((name, want), got) in names.iter().zip(&plain).zip(&mounted) {
        assert_eq!(want, got, "{name}: one mount moved from the plain run");
        assert_no_key(name, got);
    }
    assert!(plain_log.borrow().iter().any(|l| l.starts_with("fill")), "the fixture never fills");
    // ...and the one mount's book IS the account's: its rows the positions, bit for bit, its closed
    // trades the record's, its PnL the account's
    let reports = engine.mount_reports();
    let [report] = reports.as_slice() else { panic!("one mount, one report: {reports:?}") };
    for (name, row) in &report.rows {
        let held = engine.core.position_of(name).size;
        assert_eq!(row.size.to_bits(), held.to_bits(), "{name}: {} against {held}", row.size);
    }
    let closed = (format!("{:?}", report.closed_trades), format!("{:?}", record.closed_trades));
    assert_eq!(closed.0, closed.1, "the mount's closed trades are the account's");
    assert_eq!(report.pnl_curve.len(), record.equity_timestamp.len());
    let off = residual(&record, costed().cash, &reports);
    assert!(off.abs() <= 1e-9 * record.final_equity.abs(), "the residual of one mount: {off}");
}

#[test]
fn one_mount_of_an_untagged_strategy_is_the_plain_run_bit_for_bit() {
    assert_one_mount_is_the_plain_run(|log| MeanCross { log });
}

#[test]
fn one_mount_of_a_strategy_on_the_order_seam_is_the_plain_run_bit_for_bit() {
    assert_one_mount_is_the_plain_run(|log| SeamWork { log });
    // the fixture reaches what it claims: tagged fills, a cancel, a group leg's fill or end
    let log = log();
    let mounts = vec![mount(SeamWork { log: log.clone() }, &["A", "B"])];
    StrategyEngine::new_mounted(two_symbols(), mounts, costed()).run();
    let lines = log.borrow().join("\n");
    for needle in ["Some(\"entry-A\")", "Canceled", "Some(\"tp-A\")", "Some(\"late-B\")"] {
        assert!(lines.contains(needle), "the fixture never hears {needle}");
    }
}

/// Places, by tag, at one bar: each `(index, tag, side, qty, order)`; then cancels each `(index, tag)`.
struct Tagged {
    log: Log,
    places: Vec<(usize, &'static str, f64, OrderPlacement)>,
    cancels: Vec<(usize, &'static str)>,
}

impl Strategy<OrderBrokerSimulator> for Tagged {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(at, tag, qty, order) in &self.places {
            if at == b.index {
                OrderBroker::place(b, "", tag, 1, qty, order, false);
            }
        }
        for &(at, tag) in &self.cancels {
            if at == b.index {
                OrderBroker::cancel(b, tag);
            }
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        let line = format!("fill {:?} {} {}", fill.tag, fill.symbol, fill.size);
        self.log.borrow_mut().push(line);
    }
    fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        let kind = match &event.kind {
            OrderEventKind::Filled => "Filled".to_string(),
            OrderEventKind::Canceled { .. } => "Canceled".to_string(),
            OrderEventKind::Denied { reason } => format!("Denied {reason}"),
            other => format!("{other:?}"),
        };
        self.log.borrow_mut().push(format!("end {:?} {kind}", event.tag));
    }
}

#[test]
fn two_mounts_may_name_one_tag_and_each_hears_its_own_order() {
    let market = OrderPlacement::Market;
    let far = OrderPlacement::Limit { price: 50.0 };
    let (first, second) = (log(), log());
    let tagged = |log: &Log, qty: f64, cancels: Vec<(usize, &'static str)>| Tagged {
        log: log.clone(),
        places: vec![(1, "entry", qty, market), (1, "wait", 1.0, far)],
        cancels,
    };
    let mounts = vec![
        mount(tagged(&first, 1.0, vec![(2, "wait")]), &["A"]),
        mount(tagged(&second, 2.0, Vec::new()), &["A"]),
    ];
    let mut engine = StrategyEngine::new_mounted(flat_a(5), mounts, Default::default());
    let record = engine.run();
    let (fill, end) = ("fill Some(\"entry\") A", "end Some(\"entry\") Filled");
    let wait = "end Some(\"wait\") Canceled";
    assert_eq!(*first.borrow(), [format!("{fill} 1"), end.to_string(), wait.to_string()]);
    assert_eq!(*second.borrow(), [format!("{fill} 2"), end.to_string()]);
    // the second mount's far limit is the one resting order: the first's cancel reached its own alone
    let resting = engine.core.pending_of("A");
    assert_eq!(resting.len(), 1, "{resting:?}");
    assert_eq!((resting[0].size, resting[0].price), (1.0, Some(50.0)));
    assert!(engine.core.dropped.is_empty(), "{:?}", engine.core.dropped);
    assert_no_key("the record", &format!("{record:?}"));
}

/// An untagged market order of `qty` at bar 1 on its own symbol.
struct Untagged {
    log: Log,
    qty: f64,
}

impl Strategy<OrderBrokerSimulator> for Untagged {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if b.index == 1 {
            OrderBroker::place(b, "", "", 1, self.qty, OrderPlacement::Market, false);
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.log.borrow_mut().push(format!("{:?} {} {}", fill.tag, fill.symbol, fill.size));
    }
}

#[test]
fn an_untagged_fill_reaches_the_mount_that_placed_it_alone() {
    let (first, second) = (log(), log());
    let mounts = vec![
        mount(Untagged { log: first.clone(), qty: 1.0 }, &["A"]),
        mount(Untagged { log: second.clone(), qty: 2.0 }, &["A"]),
    ];
    let mut engine = StrategyEngine::new_mounted(flat_a(4), mounts, Default::default());
    engine.run();
    assert_eq!(*first.borrow(), ["None A 1"]);
    assert_eq!(*second.borrow(), ["None A 2"]);
    assert_eq!(engine.core.position_of("A").size, 3.0, "the account nets both mounts");
}

/// Buys `qty` untagged at bar `buy`, sells it back at bar `sell` when given; at bar 9 reads how many
/// closed trades it has.
struct RoundTrip {
    log: Log,
    qty: f64,
    buy: usize,
    sell: Option<usize>,
}

impl Strategy<OrderBrokerSimulator> for RoundTrip {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if b.index == self.buy {
            OrderBroker::place(b, "", "", 1, self.qty, OrderPlacement::Market, false);
        }
        if Some(b.index) == self.sell {
            OrderBroker::place(b, "", "", -1, self.qty, OrderPlacement::Market, false);
        }
        if b.index == 9 {
            let closed = OrderBroker::closed_trades(b).map(<[_]>::len);
            self.log.borrow_mut().push(format!("{closed:?}"));
        }
    }
}

#[test]
fn each_mount_books_its_own_fills_and_the_account_nets_them() {
    let (first, second) = (log(), log());
    let mounts = vec![
        mount(RoundTrip { log: first.clone(), qty: 1.0, buy: 1, sell: Some(4) }, &["A"]),
        mount(RoundTrip { log: second.clone(), qty: 2.0, buy: 2, sell: None }, &["A"]),
    ];
    let p = StrategyEngineConfig {
        cash: 10_000.0,
        closed_trades: Some(vike_trade_ledger::TradeClosedTerms::default()),
        ..Default::default()
    };
    let bars = vec![("A".to_string(), gen_bars(10, None))];
    let mut engine = StrategyEngine::new_mounted(bars, mounts, p);
    let record = engine.run();
    // each reads its own closed trades, as a live mount does
    assert_eq!(first.take(), ["Some(1)"]);
    assert_eq!(second.take(), ["Some(0)"]);
    let reports = engine.mount_reports();
    let sizes: Vec<f64> = reports.iter().map(|r| r.rows[0].1.size).collect();
    assert_eq!(sizes, [0.0, 2.0]);
    assert_eq!(engine.core.position_of("A").size, 2.0, "the account nets the mounts");
    let trades = |r: &StrategyEngineMountReport| r.closed_trades.as_ref().map(|m| m.all.trades);
    assert_eq!(reports.iter().map(trades).collect::<Vec<_>>(), [Some(1), Some(0)]);
    assert_eq!(record.closed_trades.as_ref().map(|m| m.all.trades), Some(1));
    for r in &reports {
        assert_eq!(r.pnl_curve.len(), record.equity_timestamp.len());
        assert_eq!((r.latched_at, r.leverage_refusals), (None, 0));
    }
    assert!(reports[0].pnl_curve.last() != reports[1].pnl_curve.last(), "{reports:?}");
    let off = residual(&record, 10_000.0, &reports);
    assert!(off.abs() <= 1e-9, "every fill is a mount's: the residual is {off}");
}

/// Untagged market orders on its own symbol: `(bar, side, qty)` each.
struct Orders(Vec<(usize, i32, f64)>);

impl Strategy<OrderBrokerSimulator> for Orders {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(at, side, qty) in &self.0 {
            if at == b.index {
                OrderBroker::place(b, "", "", side, qty, OrderPlacement::Market, false);
            }
        }
    }
}

fn budget(max_leverage: Option<f64>, max_loss: Option<f64>) -> vike_execution::MountBudget {
    vike_execution::MountBudget {
        max_loss,
        max_notional: None,
        loss_action: vike_model::MountLossAction::default(),
        max_leverage,
    }
}

#[test]
fn a_mounts_leverage_cap_is_judged_on_its_own_book() {
    // equity 10 000 at a flat 100: the first mount may hold 15 000, its sibling has no cap
    let mounts = vec![
        StrategyEngineMount {
            budget: Some(budget(Some(1.5), None)),
            ..mount(Orders(vec![(3, 1, 100.0), (5, 1, 60.0), (7, -1, 50.0)]), &["A"])
        },
        mount(Orders(vec![(1, 1, 100.0), (5, 1, 60.0)]), &["A"]),
    ];
    let p = StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let mut engine = StrategyEngine::new_mounted(flat_a(10), mounts, p);
    engine.run();
    // its 100 passes on its own book though the account already holds 100 (20 000 on the account's);
    // its 60 more is refused, its sibling's 60 is not; the reduce is never judged
    let dropped: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].starts_with(vike_execution::risk::MOUNT_LEVERAGE_REFUSAL), "{dropped:?}");
    let reports = engine.mount_reports();
    let books: Vec<(f64, usize)> =
        reports.iter().map(|r| (r.rows[0].1.size, r.leverage_refusals)).collect();
    assert_eq!(books, [(50.0, 1), (160.0, 0)]);
    assert_eq!(engine.core.position_of("A").size, 210.0);
}

/// `A` at 100 for three bars, closing 95 on the fourth and 90 on the fifth, then 90 to bar 9.
fn falling_a() -> Vec<(String, Vec<Bar>)> {
    let bar = |k: i64, open: f64, close: f64| {
        let (high, low) = (open.max(close), open.min(close));
        vike_simulator::test_support::bar(60_000 * (k + 1), open, high, low, close, 1_000.0)
    };
    let closes = [100.0, 100.0, 100.0, 95.0, 90.0, 90.0, 90.0, 90.0, 90.0, 90.0];
    let mut open = 100.0;
    let bars = (0..10)
        .map(|k| {
            let b = bar(k as i64, open, closes[k]);
            open = closes[k];
            b
        })
        .collect();
    vec![("A".to_string(), bars)]
}

/// The first mount long 100 from bar 2's open at 100 with a far limit resting, its loss budget 500
/// under `action`; its sibling, no budget, buys 10 at bar 6. Both mounts' logs and reports.
fn latched(
    action: vike_model::MountLossAction,
) -> ([Vec<String>; 2], Vec<StrategyEngineMountReport>) {
    let logs = [log(), log()];
    let first = Tagged {
        log: logs[0].clone(),
        places: vec![
            (1, "entry", 100.0, OrderPlacement::Market),
            (1, "wait", 1.0, OrderPlacement::Limit { price: 50.0 }),
            (6, "again", 10.0, OrderPlacement::Market),
        ],
        cancels: Vec::new(),
    };
    let second = Tagged {
        log: logs[1].clone(),
        places: vec![(6, "b", 10.0, OrderPlacement::Market)],
        cancels: Vec::new(),
    };
    let loss = vike_execution::MountBudget { loss_action: action, ..budget(None, Some(500.0)) };
    let mounts = vec![
        StrategyEngineMount { budget: Some(loss), ..mount(first, &["A"]) },
        mount(second, &["A"]),
    ];
    let p = StrategyEngineConfig { cash: 100_000.0, ..Default::default() };
    let mut engine = StrategyEngine::new_mounted(falling_a(), mounts, p);
    engine.run();
    let dropped: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(dropped, ["the mount's loss budget latched it"], "the later intent alone");
    (logs.map(|l| l.take()), engine.mount_reports())
}

#[test]
fn a_mounts_loss_budget_latches_it_alone_and_flatten_closes_its_own_book() {
    let entry = ["fill Some(\"entry\") A 100", "end Some(\"entry\") Filled"];
    let wait = "end Some(\"wait\") Canceled";
    // loss 500 at bar 3's close is not past 500; 1 000 at bar 4's is: latched there, before it steps
    let ([first, second], reports) = latched(vike_model::MountLossAction::Flatten);
    assert_eq!(first, [entry[0], entry[1], wait, "fill None A 100"], "its flatten is its own fill");
    let sibling = ["fill Some(\"b\") A 10", "end Some(\"b\") Filled"];
    assert_eq!(second, sibling, "its sibling trades on");
    let at_bar_4 = Some(60_000 * 5);
    let books: Vec<(f64, Option<i64>)> =
        reports.iter().map(|r| (r.rows[0].1.size, r.latched_at)).collect();
    assert_eq!(books, [(0.0, at_bar_4), (10.0, None)]);

    let ([first, _], reports) = latched(vike_model::MountLossAction::CancelOnly);
    assert_eq!(first, [entry[0], entry[1], wait], "cancel only leaves its position");
    assert_eq!((reports[0].rows[0].1.size, reports[0].latched_at), (100.0, at_bar_4));
}

#[test]
fn under_an_armed_gate_the_first_mount_faces_the_budget_first() {
    // buying power 10 000 at 1x: 60 and 50 at 100 cannot both be working
    let run = |sizes: [f64; 2]| {
        let logs = [log(), log()];
        let buy = |log: &Log, qty: f64| Tagged {
            log: log.clone(),
            places: vec![(1, "buy", qty, OrderPlacement::Market)],
            cancels: Vec::new(),
        };
        let mounts =
            vec![mount(buy(&logs[0], sizes[0]), &["A"]), mount(buy(&logs[1], sizes[1]), &["A"])];
        let p = StrategyEngineConfig { cash: 10_000.0, leverage: Some(1.0), ..Default::default() };
        let mut engine = StrategyEngine::new_mounted(flat_a(4), mounts, p);
        engine.run();
        let dropped: Vec<String> = engine.core.dropped.iter().map(|d| d.reason.clone()).collect();
        (logs.map(|l| l.take()), dropped)
    };
    for sizes in [[60.0, 50.0], [50.0, 60.0]] {
        let ([first, second], dropped) = run(sizes);
        let buy = Some("buy".to_string());
        assert_eq!(first, [format!("fill {buy:?} A {}", sizes[0]), format!("end {buy:?} Filled")]);
        assert_eq!(second, [format!("end {buy:?} Denied refused by the pre-trade gate")]);
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert_no_key("dropped", &dropped[0]);
    }
}

/// At bar 1: a market buy on `""` and one on `A`; records the reads by name.
struct OwnAndForeign {
    log: Log,
}

impl Strategy<OrderBrokerSimulator> for OwnAndForeign {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        if b.index == 1 {
            OrderBroker::place(b, "", "own", 1, 1.0, OrderPlacement::Market, false);
            OrderBroker::place(b, "A", "foreign", 1, 1.0, OrderPlacement::Market, false);
        }
        if b.index == 3 {
            let (own, a) = (OrderBroker::position(b, ""), OrderBroker::position(b, "A"));
            let line = format!("{:?} {own} {a}", bar.symbol);
            self.log.borrow_mut().push(line);
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.log.borrow_mut().push(format!("fill {:?} {}", fill.tag, fill.symbol));
    }
    fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        if let OrderEventKind::Denied { reason } = &event.kind {
            self.log.borrow_mut().push(format!("denied {:?} {reason}", event.tag));
        }
    }
}

#[test]
fn empty_names_the_mounts_own_symbol_and_a_symbol_it_does_not_trade_is_refused() {
    let log = log();
    let mounts = vec![mount(OwnAndForeign { log: log.clone() }, &["B"])];
    let mut engine = StrategyEngine::new_mounted(two_symbols(), mounts, Default::default());
    engine.run();
    let refusal = "A is not a symbol of this mount";
    assert_eq!(
        *log.borrow(),
        [
            "fill Some(\"own\") B".to_string(),
            format!("denied Some(\"foreign\") {refusal}"),
            "Some(\"B\") 1 0".to_string(),
        ]
    );
    let dropped: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(dropped, [refusal]);
}

/// Records the index reads at bar 2 on its own symbol's bar.
struct IndexReads {
    log: Log,
}

impl Strategy<OrderBrokerSimulator> for IndexReads {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        if b.index != 2 || bar.symbol.as_deref() != Some("B") {
            return;
        }
        let close_at = |k: usize| OrderBroker::bars_at(b, k).last().map(|x| x.close);
        let line = format!(
            "{:?} {:?} {:?} {:?} {:?} {:?}",
            OrderBroker::symbols(b),
            OrderBroker::symbol_index(b, "A"),
            OrderBroker::price_at(b, 0) == OrderBroker::price(b, "B"),
            OrderBroker::price_at(b, 1) == OrderBroker::price(b, "A"),
            close_at(0) == Some(OrderBroker::price(b, "B")),
            close_at(1) == Some(OrderBroker::price(b, "A")),
        );
        self.log.borrow_mut().push(line);
    }
}

#[test]
fn the_index_reads_are_the_mounts_own_symbols_its_own_first() {
    let log = log();
    let mounts = vec![mount(IndexReads { log: log.clone() }, &["B", "A"])];
    StrategyEngine::new_mounted(two_symbols(), mounts, StrategyEngineConfig::default()).run();
    assert_eq!(*log.borrow(), ["[\"B\", \"A\"] Some(1) true true true true"]);
}

// ---- the mount leverage cap: one judge (`judge_mount_leverage`), each verb alone ----

fn reasons<S: Strategy<OrderBrokerSimulator>>(engine: &StrategyEngine<S>) -> Vec<String> {
    engine.core.dropped.iter().map(|d| d.reason.clone()).collect()
}

/// The refusals the cap made, of `reasons`.
fn capped(reasons: &[String]) -> usize {
    reasons.iter().filter(|r| r.starts_with(vike_execution::risk::MOUNT_LEVERAGE_REFUSAL)).count()
}

/// A REVERSAL is netted on the order's row, a plain run's (its one mount) and a mount's own: long 100, a
/// sell of 200 is 100 short after it (10 000 under the cap of 20 000) and fills; one of 400 more (500 short)
/// is refused. Before, the sell of 200 was judged 10 000 + 20 000 and refused: an always-in-market strategy
/// never reversed.
#[test]
fn a_reversal_is_netted_on_the_mounts_own_row() {
    let orders = || Orders(vec![(1, 1, 100.0), (3, -1, 200.0), (5, -1, 400.0)]);
    let p = || StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let plain_p = StrategyEngineConfig { max_mount_leverage: Some(2.0), ..p() };
    let mut plain = StrategyEngine::new(flat_a(8), orders(), plain_p);
    plain.run();
    assert_eq!((plain.core.position_of("A").size, capped(&reasons(&plain))), (-100.0, 1));
    let mounts = vec![StrategyEngineMount {
        budget: Some(budget(Some(2.0), None)),
        ..mount(orders(), &["A"])
    }];
    let mut engine = StrategyEngine::new_mounted(flat_a(8), mounts, p());
    engine.run();
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
    let report = &engine.mount_reports()[0];
    assert_eq!((report.rows[0].1.size, report.leverage_refusals), (-100.0, 1));
}

/// The MOUNT's own row decides what opens, never the account's position: `first` (cap 2) long 150 closes it
/// while its sibling's short 300 leaves the account short 150 (before: an add to that short, refused), and a
/// flat `first` cannot "reduce" its sibling's long 300 by 250 (before: covered on the account, never judged;
/// now 25 000 on its own book, refused).
#[test]
fn a_mounts_cap_reads_its_own_row_for_a_close_and_a_reduce() {
    let p = || StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let run = |first: Vec<(usize, i32, f64)>, second: Vec<(usize, i32, f64)>| {
        let mounts = vec![
            StrategyEngineMount {
                budget: Some(budget(Some(2.0), None)),
                ..mount(Orders(first), &["A"])
            },
            mount(Orders(second), &["A"]),
        ];
        let mut engine = StrategyEngine::new_mounted(flat_a(8), mounts, p());
        engine.run();
        let reports = engine.mount_reports();
        let books: Vec<(f64, usize)> =
            reports.iter().map(|r| (r.rows[0].1.size, r.leverage_refusals)).collect();
        (books, reasons(&engine))
    };
    let (books, reasons) = run(vec![(1, 1, 150.0), (5, -1, 150.0)], vec![(3, -1, 300.0)]);
    assert_eq!(books, [(0.0, 0), (-300.0, 0)], "{reasons:?}");
    let (books, reasons) = run(vec![(3, -1, 250.0)], vec![(1, 1, 300.0)]);
    assert_eq!(books, [(0.0, 1), (300.0, 0)]);
    assert_eq!(capped(&reasons), 1, "{reasons:?}");
}

/// Tagged orders on its own symbol at a bar: `(bar, tag, side, qty, order, reduce_only)` each (`""` untagged).
struct Placed(Vec<(usize, &'static str, i32, f64, OrderPlacement, bool)>);

impl Strategy<OrderBrokerSimulator> for Placed {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(at, tag, side, qty, order, reduce_only) in &self.0 {
            if at == b.index {
                OrderBroker::place(b, "", tag, side, qty, order, reduce_only);
            }
        }
    }
}

/// EACH ORDER ALONE, as live's drain judges each intent (review #3/#6): two opening orders of one `on_bar`
/// each fit and both fill, whichever comes first, though their sum (25 000) would not; the mount cap does
/// not sum a hook's orders (an armed `RiskGate` bounds the account).
#[test]
fn a_hooks_orders_are_each_judged_alone() {
    for orders in [vec![(1, 1, 150.0), (1, 1, 100.0)], vec![(1, 1, 100.0), (1, 1, 150.0)]] {
        let p = StrategyEngineConfig {
            cash: 10_000.0,
            max_mount_leverage: Some(2.0),
            ..Default::default()
        };
        let mut engine = StrategyEngine::new(flat_a(4), Orders(orders), p);
        engine.run();
        assert_eq!((engine.core.position_of("A").size, capped(&reasons(&engine))), (250.0, 0));
    }
}

/// A REDUCE-ONLY order passes unjudged only as a covered reduce of the mount's own row: a protective stop
/// placed beside its entry (`InBarRules::reduce_only_at_fill` caps it at its fill) is judged ALONE on the
/// flat row it is sent over, at the worse of its trigger and the mark, so 100 down (10 000) rests and 250
/// down (25 000) is refused; and a flat
/// mount's reduce-only sell of 250 against its sibling's long 300, which the ACCOUNT's position would let
/// fill, is refused on its own book. Before, every reduce-only order passed.
#[test]
fn a_reduce_only_order_is_judged_unless_it_covers_the_mounts_own_row() {
    let run = |stop: f64| {
        let p = StrategyEngineConfig {
            cash: 10_000.0,
            max_mount_leverage: Some(2.0),
            in_bar: InBarRules { reduce_only_at_fill: true, ..InBarRules::default() },
            ..Default::default()
        };
        let trigger = OrderPlacement::Stop { trigger: 90.0 };
        let market = OrderPlacement::Market;
        let script =
            Placed(vec![(1, "in", 1, 100.0, market, false), (1, "sl", -1, stop, trigger, true)]);
        let mut engine = StrategyEngine::new(flat_a(4), script, p);
        engine.run();
        (capped(&reasons(&engine)), engine.core.pending_of("A").len())
    };
    assert_eq!(run(100.0), (0, 1), "the stop of its entry rests");
    assert_eq!(run(250.0), (1, 0), "250 down from a flat book");
    let mounts = vec![
        StrategyEngineMount {
            budget: Some(budget(Some(2.0), None)),
            ..mount(Placed(vec![(3, "x", -1, 250.0, OrderPlacement::Market, true)]), &["A"])
        },
        mount(Orders(vec![(1, 1, 300.0)]), &["A"]),
    ];
    let p = StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let mut engine = StrategyEngine::new_mounted(flat_a(6), mounts, p);
    engine.run();
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
    let rows: Vec<f64> = engine.mount_reports().iter().map(|r| r.rows[0].1.size).collect();
    assert_eq!(rows, [0.0, 300.0]);
}

/// A reduce-only order is judged on its STATED size, as live's drain judges it before its engine clamps it to
/// the account's position (review #8): a flat mount's reduce-only sell of 250 against its sibling's long 150 is
/// 25 000 on its own book, refused, where the clamp to the account's 150 (15 000) let it through before.
#[test]
fn a_reduce_only_order_is_judged_on_its_stated_size() {
    let mounts = vec![
        StrategyEngineMount {
            budget: Some(budget(Some(2.0), None)),
            ..mount(Placed(vec![(3, "x", -1, 250.0, OrderPlacement::Market, true)]), &["A"])
        },
        mount(Orders(vec![(1, 1, 150.0)]), &["A"]),
    ];
    let p = StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let mut engine = StrategyEngine::new_mounted(flat_a(6), mounts, p);
    engine.run();
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
    let rows: Vec<f64> = engine.mount_reports().iter().map(|r| r.rows[0].1.size).collect();
    assert_eq!(rows, [0.0, 150.0]);
}

/// An AMEND that grows an order is judged on its growth over what it still rests, ALONE, at the worse of its
/// price and the mark: a resting buy of 100 at 99 grown to 350 adds 250 (25 000), refused with its size kept;
/// grown to 300 it adds 200 (20 000, at the cap) and goes. Before, every amend passed while no `RiskGate` was
/// armed.
#[test]
fn an_amend_is_judged_on_its_growth() {
    struct Amends;
    impl Strategy<OrderBrokerSimulator> for Amends {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let rest = OrderPlacement::Limit { price: 99.0 };
            match b.index {
                1 => OrderBroker::place(b, "", "q", 1, 100.0, rest, false),
                2 => OrderBroker::amend(b, "q", Some(350.0), None, None),
                3 => OrderBroker::amend(b, "q", Some(300.0), None, None),
                _ => {}
            }
        }
    }
    let p = StrategyEngineConfig {
        cash: 10_000.0,
        max_mount_leverage: Some(2.0),
        ..Default::default()
    };
    let mut engine = StrategyEngine::new(flat_a(5), Amends, p);
    engine.run();
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
    let resting: Vec<f64> = engine.core.pending_of("A").iter().map(|o| o.size).collect();
    assert_eq!(resting, [300.0]);
}

/// A TIMED order is judged at its RELEASE (`release_held`), the moment the live core judges it too: a buy of
/// 150 released at bar 5 fills on a flat book, and is refused once the run bought 100 meanwhile (25 000),
/// ENDING as live ends it: `Canceled` in `vike_model::RELEASE_CANCELED_REASON`'s words, untagged (before, the
/// strategy heard nothing).
#[test]
fn an_order_held_for_its_release_is_judged_at_the_release() {
    struct Timed(bool, Log);
    impl Strategy<OrderBrokerSimulator> for Timed {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            if b.index == 1 {
                if self.0 {
                    b.submit("A", 1, 100.0, 0.0, true, None);
                }
                let release = vike_model::OrderRelease { at: 60_000 * 6, price: None };
                b.submit_market_at("A", 1, 150.0, release);
            }
        }
        fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
            self.1.borrow_mut().push(format!("{:?} {:?}", event.tag, event.kind));
        }
    }
    for (bought, held) in [(false, 150.0), (true, 100.0)] {
        let p = StrategyEngineConfig {
            cash: 10_000.0,
            max_mount_leverage: Some(2.0),
            ..Default::default()
        };
        let heard = log();
        let mut engine = StrategyEngine::new(flat_a(8), Timed(bought, heard.clone()), p);
        engine.run();
        let refused = capped(&reasons(&engine));
        assert_eq!((engine.core.position_of("A").size, refused), (held, usize::from(bought)));
        let canceled =
            OrderEventKind::Canceled { reason: vike_model::RELEASE_CANCELED_REASON.to_string() };
        let ended = bought.then(|| format!("None {canceled:?}"));
        assert_eq!(heard.take(), ended.into_iter().collect::<Vec<_>>(), "{bought}");
    }
}

/// Order lists by tag at a bar: `(bar, legs)` each, every leg `(tag, leg)` (`""` untagged); cancels by tag
/// at a bar. Hears each fill's tag and size.
struct Grouped {
    log: Log,
    groups: Vec<(usize, Vec<(&'static str, OrderListLeg)>)>,
    cancels: Vec<(usize, &'static str)>,
}

impl Strategy<OrderBrokerSimulator> for Grouped {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, legs) in &self.groups {
            if *at == b.index {
                let legs = legs.iter().map(|(t, l)| (t.to_string(), l.clone())).collect();
                OrderBroker::place_group(b, "", legs);
            }
        }
        for &(at, tag) in &self.cancels {
            if at == b.index {
                OrderBroker::cancel(b, tag);
            }
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.log.borrow_mut().push(format!("fill {:?} {}", fill.tag, fill.size));
    }
}

/// A GTC list leg of `side` · `qty` as `order` states it, held for leg `parent`'s fill when given.
fn list_leg(side: i32, qty: f64, order: OrderPlacement, parent: Option<usize>) -> OrderListLeg {
    let (order_type, price, trigger_price, trail) = order.request_terms();
    OrderListLeg {
        side,
        qty,
        order_type,
        price,
        trigger_price,
        trail,
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        parent,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// An OTO child is judged WITH its parent when the list is placed, as live's drain judges it, a plain run's
/// and a mount's alike, on the bar lane and the cash-gated one: cap 2 on 10 000, a market buy of 1 arming a
/// breakout buy of 1 000 at 101 reaches 1 001 (100 100), refused whole, so nothing fills. Before, the child
/// was judged only at its parent's fill, while live never judged it at all.
#[test]
fn an_oto_child_is_judged_with_its_parent_when_the_list_is_placed() {
    let legs = vec![
        ("p", list_leg(1, 1.0, OrderPlacement::Market, None)),
        ("c", list_leg(1, 1_000.0, OrderPlacement::Stop { trigger: 101.0 }, Some(0))),
    ];
    for cash_gate in [false, true] {
        let group = || Grouped { log: log(), groups: vec![(1, legs.clone())], cancels: Vec::new() };
        let p = || StrategyEngineConfig { cash: 10_000.0, cash_gate, ..Default::default() };
        let plain_p = StrategyEngineConfig { max_mount_leverage: Some(2.0), ..p() };
        let mut plain = StrategyEngine::new(flat_a(5), group(), plain_p);
        plain.run();
        assert_eq!(capped(&reasons(&plain)), 1, "{cash_gate}: {:?}", reasons(&plain));
        assert_eq!(plain.core.position_of("A").size, 0.0, "{cash_gate}: nothing filled");
        let mounts = vec![StrategyEngineMount {
            budget: Some(budget(Some(2.0), None)),
            ..mount(group(), &["A"])
        }];
        let mut engine = StrategyEngine::new_mounted(flat_a(5), mounts, p());
        engine.run();
        assert_eq!(capped(&reasons(&engine)), 1, "{cash_gate}: {:?}", reasons(&engine));
        let report = &engine.mount_reports()[0];
        assert_eq!((report.rows[0].1.size, report.leverage_refusals), (0.0, 1), "{cash_gate}");
    }
}

/// `leg` linked OCO to the legs `others`.
fn oco_leg(mut leg: OrderListLeg, others: &[usize]) -> OrderListLeg {
    leg.contingency = Some(ContingencyType::Oco);
    leg.linked = others.to_vec();
    leg
}

/// Cap 2 on 10 000 at a flat 100, a plain run: how many orders the cap refused and what rests, after `legs`
/// are placed as one list at bar 1.
fn one_list(legs: Vec<(&'static str, OrderListLeg)>) -> (usize, usize) {
    let p = StrategyEngineConfig {
        cash: 10_000.0,
        max_mount_leverage: Some(2.0),
        ..Default::default()
    };
    let group = Grouped { log: log(), groups: vec![(1, legs)], cancels: Vec::new() };
    let mut engine = StrategyEngine::new(flat_a(4), group, p);
    engine.run();
    (capped(&reasons(&engine)), engine.core.pending_of("A").len())
}

/// OCO legs are ALTERNATIVES (review #2): a buy limit of 150 at 99 OCO a breakout buy of 150 at 101 reaches
/// the worst one (15 150), never their sum (30 000), so both rest.
#[test]
fn an_oco_pairs_legs_are_judged_as_alternatives() {
    let legs = vec![
        ("lo", oco_leg(list_leg(1, 150.0, OrderPlacement::Limit { price: 99.0 }, None), &[1])),
        ("hi", oco_leg(list_leg(1, 150.0, OrderPlacement::Stop { trigger: 101.0 }, None), &[0])),
    ];
    assert_eq!(one_list(legs), (0, 2));
}

/// A list's legs are valued at their OWN prices (review #4): a market buy of 1 arming a breakout buy of 150 at
/// 140 reaches 100 + 21 000, refused whole, where valuing the child at the first leg's price (15 100) let it in.
#[test]
fn a_lists_child_priced_above_its_first_leg_is_valued_at_its_own_price() {
    let legs = vec![
        ("p", list_leg(1, 1.0, OrderPlacement::Market, None)),
        ("c", list_leg(1, 150.0, OrderPlacement::Stop { trigger: 140.0 }, Some(0))),
    ];
    assert_eq!(one_list(legs), (1, 0));
}

/// A timed order the cap refuses at its release leaves the mount's `""` list leg filed (review #10): the
/// refused order has no tag, so `tag_of` names it by the mount's untagged key, which files the mount's last
/// `""` list leg; the end removed that entry before, so the mount's own `cancel("")` then missed its leg.
#[test]
fn a_refused_release_leaves_the_mounts_untagged_list_leg_filed() {
    struct Timed;
    impl Strategy<OrderBrokerSimulator> for Timed {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let far = |price: f64| OrderPlacement::Limit { price };
            match b.index {
                1 => {
                    b.submit("A", 1, 100.0, 0.0, true, None);
                    let release = vike_model::OrderRelease { at: 60_000 * 6, price: None };
                    b.submit_market_at("A", 1, 150.0, release);
                }
                2 => {
                    let legs = vec![
                        ("".to_string(), oco_leg(list_leg(1, 1.0, far(50.0), None), &[1])),
                        ("".to_string(), oco_leg(list_leg(1, 1.0, far(40.0), None), &[0])),
                    ];
                    OrderBroker::place_group(b, "", legs);
                }
                6 => OrderBroker::cancel(b, ""),
                _ => {}
            }
        }
    }
    let budget = Some(budget(Some(2.0), None));
    let mounts = vec![StrategyEngineMount { budget, ..mount(Timed, &["A"]) }];
    let p = StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
    let mut engine = StrategyEngine::new_mounted(flat_a(8), mounts, p);
    engine.run();
    assert_eq!(capped(&reasons(&engine)), 1, "the release: {:?}", reasons(&engine));
    let resting: Vec<f64> = engine.core.pending_of("A").iter().filter_map(|o| o.price).collect();
    assert!(!resting.contains(&40.0), "its own cancel reached its leg: {resting:?}");
}

/// Cap 2 on 10 000 at a flat 100, a plain run of `strategy` over `bars`.
fn capped_run<S: Strategy<OrderBrokerSimulator>>(
    bars: Vec<(String, Vec<Bar>)>,
    strategy: S,
) -> StrategyEngine<S> {
    let p = StrategyEngineConfig {
        cash: 10_000.0,
        max_mount_leverage: Some(2.0),
        ..Default::default()
    };
    let mut engine = StrategyEngine::new(bars, strategy, p);
    engine.run();
    engine
}

/// An OPENING order is valued at the WORSE of its own price and the mark (review round 4 #1): a sell limit of
/// 300 at 1 fills near the mark (30 000, refused; it counted 300 and filled before), and a resting buy of 150
/// at 50 (15 000 at the mark) re-priced to 140, through the market, is judged on all it rests at the new
/// price (21 000, refused with its price kept; a price-only amend passed and filled before).
#[test]
fn an_order_priced_through_the_market_counts_at_the_market() {
    struct Through;
    impl Strategy<OrderBrokerSimulator> for Through {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let low = OrderPlacement::Limit { price: 1.0 };
            let below = OrderPlacement::Limit { price: 50.0 };
            match b.index {
                1 => {
                    OrderBroker::place(b, "", "s", -1, 300.0, low, false);
                    OrderBroker::place(b, "", "q", 1, 150.0, below, false);
                }
                2 => OrderBroker::amend(b, "q", None, Some(140.0), None),
                _ => {}
            }
        }
    }
    let engine = capped_run(flat_a(5), Through);
    assert_eq!(capped(&reasons(&engine)), 2, "{:?}", reasons(&engine));
    assert_eq!(engine.core.position_of("A").size, 0.0, "nothing filled");
    let resting: Vec<Option<f64>> = engine.core.pending_of("A").iter().map(|o| o.price).collect();
    assert_eq!(resting, [Some(50.0)], "the buy keeps its price");
}

/// An amend that grows a PARKED OTO child is judged with the parent that arms it, as its list was (review
/// round 4 #2): a buy of 1 at 50 arming a buy of 100 at 101 passes (10 201); growing the parked child to 300
/// adds 200 to the parent's 1 (20 301), refused. Before, the amend found no resting order and resized the
/// parked leg unjudged.
#[test]
fn an_amend_of_a_parked_child_is_judged_with_its_parent() {
    struct Grow;
    impl Strategy<OrderBrokerSimulator> for Grow {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let at = |price: f64| OrderPlacement::Limit { price };
            match b.index {
                1 => {
                    let parent = list_leg(1, 1.0, at(50.0), None);
                    let child = list_leg(1, 100.0, at(101.0), Some(0));
                    let legs = vec![("p".into(), parent), ("c".into(), child)];
                    OrderBroker::place_group(b, "", legs);
                }
                2 => OrderBroker::amend(b, "c", Some(300.0), None, None),
                _ => {}
            }
        }
    }
    let engine = capped_run(flat_a(5), Grow);
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
}

/// A size-only grow of a PARKED child on the OPPOSITE side of its parent is judged on its WHOLE new size after
/// the parent, exactly as its list was (review round 5 #1): a buy of 100 at 99 arming a sell of 100 at 110,
/// grown to 350, reaches 250 short at 110 (27 500), refused, where its growth alone (16 500) passed; a
/// size-only decrease passes.
#[test]
fn a_grow_of_a_parked_child_on_the_far_side_is_judged_on_its_whole_size() {
    struct Far;
    impl Strategy<OrderBrokerSimulator> for Far {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let at = |price: f64| OrderPlacement::Limit { price };
            match b.index {
                1 => {
                    let parent = list_leg(1, 100.0, at(99.0), None);
                    let child = list_leg(-1, 100.0, at(110.0), Some(0));
                    let legs = vec![("p".into(), parent), ("c".into(), child)];
                    OrderBroker::place_group(b, "", legs);
                }
                2 => OrderBroker::amend(b, "c", Some(350.0), None, None),
                3 => OrderBroker::amend(b, "c", Some(50.0), None, None),
                _ => {}
            }
        }
    }
    let engine = capped_run(flat_a(5), Far);
    assert_eq!(capped(&reasons(&engine)), 1, "the grow alone: {:?}", reasons(&engine));
}

/// A PRICE-ONLY amend of a parked exit its own list covers passes (review round 5 #2): a resting buy of 190
/// at 99 with its reduce-only stop and target parked under it, the run long 20 since, re-prices its stop; the
/// entry puts 210 on and the stop takes 190 off, so it adds nothing (judged as if the entry filled it read
/// 21 000).
#[test]
fn a_repriced_parked_exit_its_list_covers_passes() {
    struct Reprice;
    impl Strategy<OrderBrokerSimulator> for Reprice {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let exit = |order: OrderPlacement, other: usize| {
                let leg = OrderListLeg { reduce_only: true, ..list_leg(-1, 190.0, order, Some(0)) };
                oco_leg(leg, &[other])
            };
            let below = OrderPlacement::Limit { price: 99.0 };
            match b.index {
                1 => {
                    let legs = vec![
                        ("in".into(), list_leg(1, 190.0, below, None)),
                        ("sl".into(), exit(OrderPlacement::Stop { trigger: 90.0 }, 2)),
                        ("tp".into(), exit(OrderPlacement::Limit { price: 110.0 }, 1)),
                    ];
                    OrderBroker::place_group(b, "", legs);
                }
                2 => OrderBroker::place(b, "", "m", 1, 20.0, OrderPlacement::Market, false),
                4 => OrderBroker::amend(b, "sl", None, Some(85.0), None),
                _ => {}
            }
        }
    }
    let engine = capped_run(flat_a(6), Reprice);
    assert_eq!(capped(&reasons(&engine)), 0, "{:?}", reasons(&engine));
    assert_eq!(engine.core.position_of("A").size, 20.0);
}

/// A STOP-LIMIT keeps its LIMIT as its price when an amend names only a trigger (review round 5 #6): a buy of
/// 140 stopping at 120 with a limit of 135 (18 900) grown to 160 with a new trigger of 101 rests 160 at 135
/// (21 600), refused; valued at the new trigger it read 16 160 and passed.
#[test]
fn a_stop_limits_amend_keeps_its_limit_as_its_price() {
    struct Trigger;
    impl Strategy<OrderBrokerSimulator> for Trigger {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            let stop_limit = OrderPlacement::StopLimit { trigger: 120.0, limit: 135.0 };
            match b.index {
                1 => OrderBroker::place(b, "", "s", 1, 140.0, stop_limit, false),
                2 => OrderBroker::amend(b, "s", Some(160.0), None, Some(101.0)),
                _ => {}
            }
        }
    }
    let engine = capped_run(flat_a(5), Trigger);
    assert_eq!(capped(&reasons(&engine)), 1, "{:?}", reasons(&engine));
}

/// A COMBO is judged as ONE unit, all its legs or none (review round 4 #3): a roll of a long 150 of `A` into
/// 150 of `B` holds 15 000 either way and rests (judged leg by leg, `B`'s leg counted 30 000 beside the long
/// and was refused), while a combo buying 110 of each on a flat book reaches 22 000, refused (each leg alone
/// was 11 000).
#[test]
fn a_combo_is_judged_as_one_unit() {
    struct Combo {
        bought: f64,
        ratios: [i32; 2],
        qty: f64,
    }
    impl Strategy<OrderBrokerSimulator> for Combo {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
            if bar.symbol.as_deref() != Some("B") {
                return;
            }
            if b.index == 1 && self.bought > 0.0 {
                OrderBroker::place(b, "A", "", 1, self.bought, OrderPlacement::Market, false);
            }
            if b.index == 3 {
                let leg = |symbol: &str, ratio| vike_model::OrderComboLeg {
                    symbol: symbol.into(),
                    ratio,
                    limit: None,
                };
                b.submit_combo(&vike_model::OrderCombo {
                    venue: String::new(),
                    side: 1,
                    qty: self.qty,
                    legs: vec![leg("A", self.ratios[0]), leg("B", self.ratios[1])],
                    net_limit: None,
                    time_in_force: TimeInForce::default(),
                    account: None,
                    order_client_id: None,
                });
            }
        }
    }
    for (bought, ratios, qty, refused) in [(150.0, [-1, 1], 150.0, 0), (0.0, [1, 1], 110.0, 1)] {
        let flat = vike_simulator::test_support::flat_minute_bars(6, 100.0, 1_000.0);
        let bars = vec![("A".to_string(), flat.clone()), ("B".to_string(), flat)];
        let engine = capped_run(bars, Combo { bought, ratios, qty });
        assert_eq!(capped(&reasons(&engine)), refused, "{bought}: {:?}", reasons(&engine));
    }
}

/// A bracket's exits net its row in QUANTITY (review round 4 #4): flat, a market buy of 190 with reduce-only
/// exits OCO under it, a stop at 90 and a take-profit at 300, is its entry (19 000) and goes; netting in
/// notional counted the far target as 57 000 of profit against the entry and refused it.
#[test]
fn a_brackets_far_take_profit_is_no_exposure() {
    let exit = |order: OrderPlacement, other: usize| {
        let leg = OrderListLeg { reduce_only: true, ..list_leg(-1, 190.0, order, Some(0)) };
        oco_leg(leg, &[other])
    };
    let legs = vec![
        ("in", list_leg(1, 190.0, OrderPlacement::Market, None)),
        ("sl", exit(OrderPlacement::Stop { trigger: 90.0 }, 2)),
        ("tp", exit(OrderPlacement::Limit { price: 300.0 }, 1)),
    ];
    assert_eq!(one_list(legs).0, 0);
}

/// A list leg under `""` in a portfolio run is filed under its MOUNT's untagged key, as live keys `""` by the
/// mount: its fill reaches the mount that placed it and books into that mount's row, and a mount's `cancel("")`
/// reaches its own `""` leg (the last it filed) and no sibling's. Before round 2 the leg filed no tag, so the
/// mount's own cancel missed it; before round 1, `""` was one entry two mounts shared.
#[test]
fn an_untagged_list_leg_stays_its_own_mounts() {
    // an OCO pair, both legs under `""`: a far limit, then the leg that fills (the last `""` filed)
    let oco = |a: OrderPlacement, b: OrderPlacement, qty: f64| {
        let link = |mut leg: OrderListLeg, other: usize| {
            leg.contingency = Some(ContingencyType::Oco);
            leg.linked = vec![other];
            leg
        };
        let first = link(list_leg(1, 1.0, a, None), 1);
        vec![("", first), ("", link(list_leg(1, qty, b, None), 0))]
    };
    let far = |price: f64| OrderPlacement::Limit { price };
    let logs = [log(), log()];
    let buy = |log: &Log, qty: f64| Grouped {
        log: log.clone(),
        groups: vec![(1, oco(far(50.0), OrderPlacement::Market, qty))],
        cancels: Vec::new(),
    };
    let mounts = vec![mount(buy(&logs[0], 1.0), &["A"]), mount(buy(&logs[1], 2.0), &["A"])];
    let mut engine = StrategyEngine::new_mounted(flat_a(4), mounts, Default::default());
    engine.run();
    let heard = logs.map(|l| l.take());
    assert_eq!(heard, [["fill None 1"], ["fill None 2"]], "each mount hears its own leg's fill");
    let rows: Vec<f64> = engine.mount_reports().iter().map(|r| r.rows[0].1.size).collect();
    assert_eq!(rows, [1.0, 2.0]);

    let rest = |prices: [f64; 2], cancels: Vec<(usize, &'static str)>| Grouped {
        log: log(),
        groups: vec![(1, oco(far(prices[0]), far(prices[1]), 1.0))],
        cancels,
    };
    let first = rest([50.0, 40.0], vec![(2, "")]);
    let mounts = vec![mount(first, &["A"]), mount(rest([45.0, 35.0], Vec::new()), &["A"])];
    let mut engine = StrategyEngine::new_mounted(flat_a(4), mounts, Default::default());
    engine.run();
    let resting: Vec<f64> = engine.core.pending_of("A").iter().filter_map(|o| o.price).collect();
    let its_own_went = !resting.contains(&40.0);
    let siblings_stay = resting.contains(&45.0) && resting.contains(&35.0);
    assert!(its_own_went && siblings_stay, "{resting:?}");
}
