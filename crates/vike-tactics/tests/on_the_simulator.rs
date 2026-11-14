//! Rules written in the language, run on `vike-simulator`'s engine with the in-bar rules the strategy lab
//! runs (`vike_simulator::InBarRules`): each drill is one of the lab's own (`desk.rs`'s self-check), and its
//! fills are the ones the lab's rows were booked from. The runner holds the bars, as the lab's does,
//! so the rule reads the bar under way after its own fill (`RuleRun::on_fill_within`). A run of several
//! symbols trades another through `Tactics::on` as it would trade it as its own.

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::{Fill, OrderBroker, OrderEvent, OrderEventKind, Strategy};
use vike_simulator::{InBarRules, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics, TacticsTrail};

const BAR_MS: i64 = 900_000;
const SYMBOL: &str = "ETHUSDT";

fn bar(k: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        timestamp: k * BAR_MS,
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

fn flat(k: i64, price: f64) -> Bar {
    bar(k, price, price, price, price)
}

/// A rule written as what it does on bar `k`, and after a fill inside bar `k`.
struct Script {
    on_bar: fn(&mut Tactics<'_>, i64),
    after_fill: fn(&mut Tactics<'_>, i64),
}

fn quiet(_: &mut Tactics<'_>, _: i64) {}

impl Rule for Script {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.on_bar)(strategy, bar.timestamp / BAR_MS);
    }
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.after_fill)(strategy, bar.timestamp / BAR_MS);
    }
}

/// The run, the fills it got, `(ts, side, size, price)`, and every order event it heard.
struct Taped {
    run: RuleRun<Script>,
    bars: Vec<Bar>,
    fills: Vec<(i64, i32, f64, f64)>,
    events: Vec<OrderEvent>,
}

impl Strategy<OrderBrokerSimulator> for Taped {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.run.on_bar(broker, bar);
    }
    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push((fill.timestamp, fill.side, fill.size, fill.price));
        let at = self.bars.partition_point(|b| b.timestamp <= broker.now()).saturating_sub(1);
        let under_way = self.bars[at].clone();
        self.run.on_fill_within(broker, fill, &under_way);
    }
    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.events.push(event.clone());
        self.run.on_order_event(broker, event);
    }
}

/// The fills of `rule` over `bars`, trading up to the last bar, the lab's in-bar rules on.
fn tape(
    bars: &[Bar],
    rule: Script,
    run: impl FnOnce(RuleRun<Script>) -> RuleRun<Script>,
) -> Vec<(i64, i32, f64, f64)> {
    taped(bars, rule, run).fills
}

/// The lab's in-bar rules, with cash enough for any drill.
fn lab_params() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 1_000_000.0,
        in_bar: InBarRules {
            path_walk: true,
            queue_order: true,
            trail_from_fill: true,
            reduce_only_at_fill: true,
            ..InBarRules::default()
        },
        ..StrategyEngineConfig::default()
    }
}

/// [`tape`]'s whole run: its fills and the order events it heard.
fn taped(
    bars: &[Bar],
    rule: Script,
    run: impl FnOnce(RuleRun<Script>) -> RuleRun<Script>,
) -> Taped {
    let last = bars.last().expect("bars").timestamp;
    let run = run(RuleRun::new(rule, SYMBOL).tick(0.01).trading(0, last));
    let params = lab_params();
    let taped = Taped { run, bars: bars.to_vec(), fills: Vec::new(), events: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYMBOL.to_string(), bars.to_vec())], taped, params);
    let result = engine.run();
    assert_eq!(result.intrabar_both_hit, 0, "the coarse lane settled an order");
    engine.strategy
}

fn plain(on_bar: fn(&mut Tactics<'_>, i64)) -> Script {
    Script { on_bar, after_fill: quiet }
}

const B: i64 = BAR_MS;

#[test]
fn a_trail_issued_with_its_entry_activates_and_fires_in_the_entry_bar() {
    fn trailing(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
            s.exit("X").from("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 50.0 });
        }
    }
    fn touching(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
            s.exit("X").from("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 0.0 });
        }
    }
    let trailed = [flat(0, 100.0), bar(1, 100.0, 102.0, 99.9, 100.5), flat(2, 100.5)];
    assert_eq!(
        tape(&trailed, plain(trailing), |r| r),
        [(B, 1, 1.0, 100.0), (B, -1, 1.0, 101.5)],
        "it activates at 101 and fires 0.5 under the high"
    );
    assert_eq!(
        tape(&trailed, plain(touching), |r| r),
        [(B, 1, 1.0, 100.0), (B, -1, 1.0, 101.0)],
        "with no offset it leaves at 101 itself"
    );
}

#[test]
fn the_rule_run_again_after_its_target_turns_short_at_the_next_point_of_the_walk() {
    fn enter(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
            s.exit("X").from("A").take(101.0);
        }
    }
    fn turn(s: &mut Tactics<'_>, k: i64) {
        if k == 1 && s.position() == 0.0 {
            s.entry("B", Short);
        }
    }
    let rerun = [flat(0, 100.0), bar(1, 100.0, 102.0, 99.5, 101.5), flat(2, 101.5)];
    assert_eq!(
        tape(&rerun, Script { on_bar: enter, after_fill: turn }, |r| r),
        [(B, 1, 1.0, 100.0), (B, -1, 1.0, 101.0), (B, -1, 1.0, 102.0)]
    );
}

#[test]
fn a_buy_stop_and_a_sell_stop_at_one_level_go_buy_first() {
    fn tie(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("S", Short).stop(100.0);
            s.entry("B", Long).stop(100.0);
        }
    }
    let bars = [flat(0, 100.0), flat(1, 100.0), flat(2, 100.0)];
    assert_eq!(tape(&bars, plain(tie), |r| r), [(B, 1, 1.0, 100.0), (B, -1, 1.0, 100.0)]);
}

#[test]
fn a_free_stop_closes_and_opens_and_a_free_limit_closes_that() {
    fn free(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
                s.order("S", Short).size(3.0).stop(95.0);
            }
            3 => {
                s.order("B", Long).size(2.0).limit(93.0);
            }
            _ => {}
        }
    }
    let bars = [
        flat(0, 100.0),
        flat(1, 100.0),
        bar(2, 100.0, 100.0, 94.0, 94.0),
        flat(3, 94.0),
        bar(4, 94.0, 94.0, 92.0, 92.0),
        flat(5, 92.0),
    ];
    assert_eq!(
        tape(&bars, plain(free), |r| r),
        [(B, 1, 1.0, 100.0), (2 * B, -1, 3.0, 95.0), (4 * B, 1, 2.0, 93.0)]
    );
}

#[test]
fn the_fill_that_reaches_the_cap_closes_and_no_entry_follows() {
    fn trade(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.entry("B", Short);
            }
            2 => {
                s.entry("C", Long);
            }
            _ => {}
        }
    }
    let bars = [flat(0, 100.0), flat(1, 100.0), flat(2, 101.0), flat(3, 102.0), flat(4, 102.0)];
    assert_eq!(
        tape(&bars, plain(trade), |r| r.fills_a_day(2)),
        [(B, 1, 1.0, 100.0), (2 * B, -1, 2.0, 101.0), (2 * B, 1, 1.0, 101.0)]
    );
}

#[test]
fn an_exit_issued_with_its_resting_entry_by_both_rules() {
    fn resting(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("E", Long).stop(105.0);
            s.exit("X").from("E").stop(95.0);
        }
    }
    let dip_then_rise =
        [flat(0, 100.0), bar(1, 100.0, 106.0, 94.0, 100.0), bar(2, 100.0, 100.0, 90.0, 91.0)];
    assert_eq!(
        tape(&dip_then_rise, plain(resting), |r| r),
        [(B, 1, 1.0, 105.0)],
        "the reference's backtest rule: the dip spends the stop before the entry fills"
    );
    assert_eq!(
        tape(&dip_then_rise, plain(resting), |r| r.bound_exits(true)),
        [(B, 1, 1.0, 105.0), (2 * B, -1, 1.0, 95.0)],
        "bound to its entry, the stop works from the entry's fill"
    );
}

#[test]
fn on_the_simulator_an_iceberg_fills_as_its_whole_limit_and_a_chase_on_bars_ends_unfilled() {
    fn limit(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(3.0).limit(99.5);
        }
    }
    fn iceberg(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(3.0).limit(99.5).iceberg(1.0);
        }
    }
    fn chase(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).chase();
        }
    }
    let dip = [flat(0, 100.0), bar(1, 100.0, 100.0, 99.0, 99.5), flat(2, 99.5)];
    let whole = tape(&dip, plain(limit), |r| r);
    assert_eq!(whole, [(B, 1, 3.0, 99.5)]);
    assert_eq!(
        tape(&dip, plain(iceberg), |r| r),
        whole,
        "no bar shows a queue: the whole limit's fills"
    );
    // the one emulation law (decision 0275) follows a book's touch, so a bar run ends it unfilled,
    // and the strategy hears that end under the order's tag
    let chased = taped(&dip, plain(chase), |r| r);
    assert!(chased.fills.is_empty(), "{:?}", chased.fills);
    let ends: Vec<_> = chased.events.iter().filter(|e| e.kind.is_terminal()).collect();
    assert!(
        matches!(ends.as_slice(), [e] if e.tag.as_deref() == Some("A#1")),
        "one end, under its tag: {:?}",
        chased.events
    );
}

#[test]
fn on_the_simulator_the_strategy_hears_a_refused_iceberg_as_a_denied_order() {
    fn hidden(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).limit(99.5).iceberg(0.0);
        }
    }
    let dip = [flat(0, 100.0), bar(1, 100.0, 100.0, 99.0, 99.5), flat(2, 99.5)];
    let run = taped(&dip, plain(hidden), |r| r);
    assert!(run.fills.is_empty(), "{:?}", run.fills);
    let heard: Vec<(Option<&str>, &OrderEventKind)> =
        run.events.iter().map(|e| (e.tag.as_deref(), &e.kind)).collect();
    assert!(
        matches!(heard.as_slice(), [(Some("A#1"), OrderEventKind::Denied { reason })]
            if reason.contains(".iceberg(0)") && reason.contains("above zero")),
        "one terminal Denied, under its tag, by name: {heard:?}"
    );
}

// ---- several symbols (`RuleRun::trades`, `Tactics::on`) ----

/// The symbol the two-symbol drill reads its signal on, and the run's own there.
const SIGNAL: &str = "BTCUSDT";

/// Acts on the bars of `signal`, trading `trade` through `Tactics::on` (the run's own symbol when
/// the two are one), and keeps `(k, position, position_of(trade))` as each of its hooks starts.
struct Signal {
    signal: &'static str,
    trade: &'static str,
    held: Vec<(i64, f64, f64)>,
}

impl Rule for Signal {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }
    fn on_bar(&mut self, s: &mut Tactics<'_>, bar: &Bar) {
        if bar.symbol.as_deref() != Some(self.signal) {
            return;
        }
        let k = bar.timestamp / BAR_MS;
        self.held.push((k, s.position(), s.position_of(self.trade)));
        let s = s.on(self.trade);
        match k {
            0 => {
                s.entry("L", Long);
                s.exit("L").stop(95.0);
            }
            3 => {
                s.entry("S", Short);
            }
            4 => {
                s.close("S");
            }
            _ => {}
        }
    }
}

/// A run over every series in `bars`, and each fill it got: `(ts, symbol, side, size, price, tag)`.
struct Several {
    run: RuleRun<Signal>,
    fills: Vec<(i64, String, i32, f64, f64, String)>,
}

impl Strategy<OrderBrokerSimulator> for Several {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.run.on_bar(broker, bar);
    }
    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        let (symbol, tag) = (fill.symbol.clone(), fill.tag.clone().unwrap_or_default());
        self.fills.push((fill.timestamp, symbol, fill.side, fill.size, fill.price, tag));
        self.run.on_fill(broker, fill);
    }
    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.run.on_order_event(broker, event);
    }
}

/// `run` over `bars`, on the lab's in-bar rules.
fn several(bars: Vec<(String, Vec<Bar>)>, run: RuleRun<Signal>) -> Several {
    let several = Several { run, fills: Vec::new() };
    let mut engine = StrategyEngine::new(bars, several, lab_params());
    let result = engine.run();
    assert_eq!(result.intrabar_both_hit, 0, "the coarse lane settled an order");
    engine.strategy
}

/// The plan's example: the signal is BTC, the trade is ETH, one rule. ETH's entry, its stop, a short
/// and its close fill exactly as the same rule trading ETH as its own symbol, under
/// `<symbol>/<name>#<n>`, the position read through `position_of` while the run's own stays flat.
#[test]
fn on_the_simulator_a_signal_on_one_symbol_trades_another_as_its_own_would_be() {
    let eth = vec![
        flat(0, 100.0),
        flat(1, 101.0),
        bar(2, 101.0, 101.0, 94.0, 96.0),
        flat(3, 96.0),
        flat(4, 97.0),
        flat(5, 98.0),
        flat(6, 98.0),
    ];
    let btc: Vec<Bar> = (0..7).map(|k| flat(k, 50_000.0)).collect();
    let rule = |signal| Signal { signal, trade: SYMBOL, held: Vec::new() };

    let two = several(
        vec![(SIGNAL.to_string(), btc), (SYMBOL.to_string(), eth.clone())],
        RuleRun::new(rule(SIGNAL), SIGNAL).tick(0.01).trades(&[SYMBOL]),
    );
    let at = |k: i64, side: i32, price: f64, tag: &str| {
        (k * B, SYMBOL.to_string(), side, 1.0, price, format!("{SYMBOL}/{tag}"))
    };
    assert_eq!(
        two.fills,
        [
            at(1, 1, 101.0, "L#1"),
            at(2, -1, 95.0, "L#2"),
            at(4, -1, 97.0, "S#3"),
            at(5, 1, 98.0, "drop S#4")
        ]
    );
    let held = &two.run.rule().held;
    assert!(held.iter().all(|h| h.1 == 0.0), "the run's own symbol stays flat: {held:?}");
    let eth_held: Vec<f64> = held.iter().map(|h| h.2).collect();
    assert_eq!(eth_held, [0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0]);

    let one =
        several(vec![(SYMBOL.to_string(), eth)], RuleRun::new(rule(SYMBOL), SYMBOL).tick(0.01));
    let bare: Vec<_> = two
        .fills
        .iter()
        .map(|(ts, symbol, side, size, price, tag)| {
            let tag = tag.strip_prefix(&format!("{SYMBOL}/")).expect("tagged on ETH").to_string();
            (*ts, symbol.clone(), *side, *size, *price, tag)
        })
        .collect();
    assert_eq!(
        one.fills, bare,
        "on its own symbol the same rule fills the same, tagged <name>#<n>"
    );
}
