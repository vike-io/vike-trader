//! The language's reads beside a rule's own bars, on `vike-simulator`'s engine (a dev edge): an input as of
//! the bar's close and its history (`Tactics::input`, decision 0271), the rule's inputs reaching the run
//! (`Rule::inputs` through `RuleRun`), the closed trades as the engine's ledger booked them
//! (`Tactics::closed_trades`, `Tactics::closed_net`), and a frame read with lookahead on over an input's
//! whole series (`Tactics::input_whole`, decision 0271's amendment). The per-symbol reads by index: each
//! symbol at its own index in the broker's universe (by name where it has none), the simulator answering an
//! index as its name, the grid's tick asked only when a pass needs it. One long run, ignored, prints a
//! digest that two builds of the per-bar paths must agree on.

use std::cell::{Cell, RefCell};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::{
    ContractValue, Fill, Grid, OrderBroker, OrderEvent, OrderListLeg, OrderPlacement, RiskLimits,
    Strategy, TradeClosedRow,
};
use vike_simulator::{
    InputSeries, Inputs, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};
use vike_tactics::higher::{HigherTimeframeSourceAhead, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleRun, Tactics, TacticsTrail};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const SYMBOL: &str = "ETHUSDT";
const DAILY: &str = "1d";

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// What the rule saw of its daily input on each hourly bar: `(bar ts, last close, how many closed, the
/// closes of the last two)`.
type Seen = Vec<(i64, Option<f64>, usize, Vec<f64>)>;

/// What the rule read of its closed trades on each bar: `(bar ts, closed rows, closed net)`.
type Closed = Vec<(i64, usize, Option<f64>)>;

/// Records its input on every bar; buys 1 on bar 2 and closes it on bar 5, and records its closed trades.
struct Reader {
    seen: Seen,
    /// `(bar ts, closed rows, closed net)` on each bar.
    closed: Closed,
    /// The ledger's rows on the last bar.
    rows: Vec<TradeClosedRow>,
    k: usize,
}

impl Rule for Reader {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![DAILY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let input = strategy.input(DAILY);
        let last2 = input.bars(2).iter().map(|b| b.close).collect();
        self.seen.push((bar.timestamp, input.close(), input.bars(usize::MAX).len(), last2));
        assert!(strategy.input("4h").bar().is_none(), "an undeclared name reads nothing");
        let rows = strategy.closed_trades().expect("the simulator keeps a ledger");
        self.rows = rows.to_vec();
        self.closed.push((bar.timestamp, rows.len(), strategy.closed_net()));
        match self.k {
            2 => {
                strategy.entry("L", Long).size(1.0);
            }
            5 => {
                strategy.close("L");
            }
            _ => {}
        }
        self.k += 1;
    }
}

struct Driven<R>(RuleRun<R>);

impl<R: Rule> Strategy<OrderBrokerSimulator> for Driven<R> {
    fn inputs(&self) -> Vec<String> {
        Strategy::<OrderBrokerSimulator>::inputs(&self.0)
    }
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.0.on_bar(broker, bar);
    }
    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        self.0.on_fill(broker, fill);
    }
}

/// The daily input from day 0 (closes 100 to 104) and hourly bars from day 3 to day 4 05:00.
fn days_and_hours() -> (Vec<Bar>, Vec<Bar>) {
    let days: Vec<Bar> = (0..5).map(|d| bar(d * DAY, 100.0 + d as f64)).collect();
    let hours: Vec<Bar> = (0..30).map(|h| bar(3 * DAY + h * HOUR, 50.0 + h as f64)).collect();
    (days, hours)
}

/// Records what a day read with lookahead on hands out on each bar, `(day, close)`.
struct Ahead {
    days: HigherTimeframeSourceAhead,
    seen: Vec<Vec<(i64, f64)>>,
}

impl Rule for Ahead {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![self.days.input().to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let days = self.days.next(bar, strategy);
        self.seen.push(days.iter().map(|d| (d.timestamp, d.close)).collect());
    }
}

/// The `Ahead` rule over [`days_and_hours`], its run serving the whole series when `input_whole`.
fn run_ahead(input_whole: bool) -> Vec<Vec<(i64, f64)>> {
    let (days, hours) = days_and_hours();
    let rule = Ahead {
        days: HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Day),
        seen: Vec::new(),
    };
    let series = vec![InputSeries { name: DAILY.to_string(), interval_ms: DAY, bars: days }];
    let params = StrategyEngineConfig {
        inputs: Inputs { bar_ms: HOUR, series },
        input_whole,
        ..StrategyEngineConfig::default()
    };
    let symbols = vec![(SYMBOL.to_string(), hours)];
    let mut engine = StrategyEngine::new(symbols, Driven(RuleRun::new(rule, SYMBOL)), params);
    engine.run();
    engine.strategy.0.rule().seen.clone()
}

/// A day read with lookahead on (`vike_tactics::higher::HigherTimeframeSourceAhead`) is handed out on its
/// first bar at its FINAL values, every day before the window with it: day 3's close (103) on day 3's
/// first hour, which a closed-bar read shows only from its last. The whole series ends at the run's last
/// bar, so day 4, still in progress there, is never handed out.
#[test]
fn a_day_read_ahead_comes_on_its_first_bar_at_its_final_values() {
    let seen = run_ahead(true);
    assert_eq!(seen.len(), 30);
    assert_eq!(seen[0], [(0, 100.0), (DAY, 101.0), (2 * DAY, 102.0), (3 * DAY, 103.0)]);
    assert!(seen[1..].iter().all(Vec::is_empty), "{seen:?}");
}

/// A run that serves no whole series (a file not marked `runs_on = "backtest"`) is refused in words on
/// the frame's first bar, never run on a frame that hands out nothing.
#[test]
#[should_panic(expected = "runs_on = \"backtest\"")]
fn a_day_read_ahead_refuses_a_run_that_serves_no_whole_series() {
    run_ahead(false);
}

/// Hourly bars from day 3, a daily input from day 0, the rule run over them: what it saw, and the bars.
fn run() -> (Seen, Closed, Vec<TradeClosedRow>, Vec<Bar>) {
    let (days, hours) = days_and_hours();
    let rule = Reader { seen: Vec::new(), closed: Vec::new(), rows: Vec::new(), k: 0 };
    let run = RuleRun::new(rule, SYMBOL).trading(0, hours.last().expect("bars").timestamp);
    let declared = Strategy::<OrderBrokerSimulator>::inputs(&run);
    assert_eq!(declared, vec![DAILY.to_string()], "Run hands its rule's inputs on");
    let series = vec![InputSeries { name: DAILY.to_string(), interval_ms: DAY, bars: days }];
    let params = StrategyEngineConfig {
        cash: 1_000_000.0,
        inputs: Inputs { bar_ms: HOUR, series },
        ..StrategyEngineConfig::default()
    };
    let symbols = vec![(SYMBOL.to_string(), hours.clone())];
    let mut engine = StrategyEngine::new(symbols, Driven(run), params);
    engine.run();
    let r = engine.strategy.0.rule();
    (r.seen.clone(), r.closed.clone(), r.rows.clone(), hours)
}

#[test]
fn an_input_reads_its_last_bar_closed_by_the_bar_s_close_and_its_history() {
    let (seen, _, _, hours) = run();
    // Day 2 closed at the window's open: the first bar sees it and every day before it.
    assert_eq!(seen[0], (hours[0].timestamp, Some(102.0), 3, vec![101.0, 102.0]));
    // Day 3 closes at 4 * DAY, the close of the bar opened at 3 * DAY + 23 h: seen there, not a bar earlier.
    let (before, at) = (&seen[22], &seen[23]);
    assert_eq!((before.1, before.2), (Some(102.0), 3), "the day in progress is never read");
    assert_eq!(at.0, 3 * DAY + 23 * HOUR);
    assert_eq!((at.1, at.2, at.3.clone()), (Some(103.0), 4, vec![102.0, 103.0]));
}

#[test]
fn the_closed_trades_are_the_engine_s_ledger_and_their_net_its_rows_in_order() {
    let (_, closed, rows, hours) = run();
    // Bought at bar 3's open, closed at bar 6's: no row before that fill, one from it on.
    assert_eq!(closed[5].1, 0, "nothing closed before the close fills");
    assert_eq!(closed[5].2, Some(0.0));
    assert_eq!(closed[6].1, 1, "the close's row, booked before the rule's next bar");
    let [row] = rows.as_slice() else { panic!("one row: {rows:?}") };
    assert_eq!(
        (row.side, row.qty, row.entry_timestamp, row.exit_timestamp),
        (1, 1.0, hours[3].timestamp, hours[6].timestamp)
    );
    let net = closed.last().and_then(|c| c.2).expect("a net");
    assert_eq!(net.to_bits(), (0.0 + row.net).to_bits(), "closed_net adds the rows' nets in order");
}

const QUARTER: i64 = 900_000;
const OTHER: &str = "BTCUSDT";

/// Trades its days against its weeks (`HigherTimeframeSourceCalendar` over the daily input), sized from
/// money every bar, on stop entries and exits snapped to the grid's tick, one of them trailing, and holds
/// a tenth of its position against it on a second symbol: the language's per-bar reads, all of them.
struct DaysAgainstWeeks {
    days: HigherTimeframeSourceCalendar,
    weeks: HigherTimeframeSourceCalendar,
    day: Option<f64>,
    week: Option<f64>,
    handed: (usize, usize),
}

impl Rule for DaysAgainstWeeks {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![DAILY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if bar.symbol.as_deref() == Some(OTHER) {
            let want = -0.1 * strategy.position();
            let gap = want - strategy.on(OTHER).position();
            if gap.abs() > 1e-9 {
                strategy.order("hedge", if gap > 0.0 { Long } else { Short }).size(gap.abs());
            }
            return;
        }
        let sizing = strategy.sizing();
        for d in self.days.next(bar, strategy.input(DAILY)) {
            (self.day, self.handed.0) = (Some(d.close), self.handed.0 + 1);
        }
        for w in self.weeks.next(bar, strategy.input(DAILY)) {
            (self.week, self.handed.1) = (Some(w.close), self.handed.1 + 1);
        }
        let (Some(day), Some(week)) = (self.day, self.week) else { return };
        let (held, mark) = (strategy.position(), strategy.price());
        let size = share_of_worth(&sizing, 10.0, mark);
        if held <= 0.0 && mark > day && day > week {
            strategy.entry("long", Long).size(size).stop(bar.high + 0.013);
            strategy.exit("long guard").from("long").stop(bar.low - 1.07).take(mark + 2.03);
        } else if held >= 0.0 && mark < day && day < week {
            strategy.entry("short", Short).size(size).stop(bar.low - 0.013);
            let trail = TacticsTrail { after_ticks: 20.0, by_ticks: 10.0 };
            strategy.exit("short guard").from("short").stop(bar.high + 1.07).trail(trail);
        }
    }
}

/// [`Driven`] with every fill and order end it was delivered logged, in order.
struct Logged {
    run: RuleRun<DaysAgainstWeeks>,
    log: Vec<String>,
}

impl Strategy<OrderBrokerSimulator> for Logged {
    fn inputs(&self) -> Vec<String> {
        Strategy::<OrderBrokerSimulator>::inputs(&self.run)
    }
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.run.on_bar(broker, bar);
    }
    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, f: &Fill) {
        let (ts, symbol, tag) = (f.timestamp, &f.symbol, &f.tag);
        self.log.push(format!("{ts} {symbol} {tag:?} {} {:?} @ {:?}", f.side, f.size, f.price));
        self.run.on_fill(broker, f);
    }
    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.log.push(format!("end {:?} {:?}", event.tag, event.kind));
        self.run.on_order_event(broker, event);
    }
}

/// A walk from `price` in hundredths (integer arithmetic and one division: the same bits on every box),
/// one bar at each of `stamps`.
fn walk(stamps: &[i64], mut seed: u64, mut price: f64) -> Vec<Bar> {
    let mut out = Vec::with_capacity(stamps.len());
    for &ts in stamps {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        let open = price;
        price = (price + step).max(5.0);
        out.push(Bar {
            timestamp: ts,
            open,
            high: open.max(price) + 0.37,
            low: open.min(price) - 0.29,
            close: price,
            volume: 1_000.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    out
}

/// **One run held to the same bits on two builds** of the language's per-bar paths (the higher timeframe's
/// unseen days, the grid's tick, the own symbol's reads) and of the closed-trade ledger's marks: the rule
/// above over 40,000 15-minute bars of two symbols (a 3-day hole in them), its daily input from 60 days before
/// with every 17th day and a 10-day stretch missing, the grid's tick and the closed-trade ledger armed.
/// Prints a digest of the trades, the curve, the closed-trade report and every fill and order end; the same
/// command on both builds must print the same line.
#[test]
#[ignore = "a digest across two builds: cargo test -p vike-tactics --release --test reads -- --ignored --nocapture --exact a_long_run_prints_one_digest_on_every_build"]
fn a_long_run_prints_one_digest_on_every_build() {
    let start = 18_500 * DAY;
    let stamps: Vec<i64> = (0..40_288)
        .filter(|q| !(20_000..20_288).contains(q))
        .map(|q| start + q * QUARTER)
        .collect();
    let days: Vec<i64> = (-60..440)
        .filter(|d| d % 17 != 0 && !(150..160).contains(d))
        .map(|d| start + d * DAY)
        .collect();
    let rule = DaysAgainstWeeks {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        day: None,
        week: None,
        handed: (0, 0),
    };
    let series = vec![InputSeries {
        name: DAILY.to_string(),
        interval_ms: DAY,
        bars: walk(&days, 0x1A_9E7C_0FFE_E000, 100.0),
    }];
    let params = StrategyEngineConfig {
        cash: 100_000.0,
        fee_rate: 0.000_6,
        slippage: 0.000_4,
        risk_limits: Some(RiskLimits { tick_size: Some(0.05), ..RiskLimits::new() }),
        closed_trades: Some(Default::default()),
        inputs: Inputs { bar_ms: QUARTER, series },
        ..StrategyEngineConfig::default()
    };
    let symbols = vec![
        (SYMBOL.to_string(), walk(&stamps, 0x5EED_FACE_C0DE_1234, 100.0)),
        (OTHER.to_string(), walk(&stamps, 0x0DD5_EED0_2BAD_C0DE, 60.0)),
    ];
    let run = RuleRun::new(rule, SYMBOL).trades(&[OTHER]);
    let mut engine = StrategyEngine::new(symbols, Logged { run, log: Vec::new() }, params);
    let r = engine.run();
    let (log, handed) = (&engine.strategy.log, engine.strategy.run.rule().handed);
    let text = format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{log:?}",
        r.trades,
        r.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        r.equity_timestamp,
        r.final_equity.to_bits(),
        r.per_symbol_pnl,
        r.fees_paid.to_bits(),
        r.closed_trades,
    );
    let hedges = log.iter().filter(|l| l.contains("\"BTCUSDT/hedge#")).count();
    println!(
        "DIGEST {:016x}: {} trades, {} fills and ends ({hedges} on the hedge), {} days and {} weeks \
         handed out, final equity {:?}",
        vike_utility::fnv::fnv1a64(text.as_bytes()),
        r.trades.len(),
        log.len(),
        handed.0,
        handed.1,
        r.final_equity,
    );
    assert!(r.trades.len() > 50 && hedges > 0, "the rule trades all along, on both symbols");
    assert!(r.trades.iter().any(|t| t.is_long) && r.trades.iter().any(|t| !t.is_long));
    assert!(handed.0 > 300 && handed.1 > 40, "{handed:?}: days and weeks reach the rule");
    assert!(r.closed_trades.is_some(), "the closed-trade ledger marked every bar");
}

/// A broker that records the name each per-symbol read is asked with, and how often its universe is.
struct Asked {
    universe: Vec<String>,
    universe_asked: Cell<usize>,
    reads: RefCell<Vec<(&'static str, String)>>,
}

impl Asked {
    fn read(&self, what: &'static str, symbol: &str) {
        self.reads.borrow_mut().push((what, symbol.to_string()));
    }
}

impl OrderBroker for Asked {
    fn place(&mut self, _: &str, _: &str, _: i32, _: f64, _: OrderPlacement, _: bool) {}
    fn place_group(&mut self, _: &str, _: Vec<(String, OrderListLeg)>) {}
    fn amend(&mut self, _: &str, _: Option<f64>, _: Option<f64>, _: Option<f64>) {}
    fn cancel(&mut self, _: &str) {}
    fn deny(&mut self, _: &str, _: &str, _: f64, _: &str) {}
    fn average_price(&self, _: &str) -> f64 {
        0.0
    }
    fn symbols(&self) -> &[String] {
        self.universe_asked.set(self.universe_asked.get() + 1);
        &self.universe
    }
    fn position(&self, symbol: &str) -> f64 {
        self.read("position", symbol);
        0.0
    }
    fn price(&self, symbol: &str) -> f64 {
        self.read("price", symbol);
        100.0
    }
    fn equity(&self) -> f64 {
        1_000.0
    }
    fn bars(&self, _: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
    fn instrument_grid(&self, symbol: &str) -> Option<Grid> {
        self.read("grid", symbol);
        Some(Grid { tick_size: Some(0.5), lot_size: None, min_notional: None, min_qty: None })
    }
    fn contract_value(&self, symbol: &str) -> Option<ContractValue> {
        self.read("value", symbol);
        Some(ContractValue::linear(1.0))
    }
    // The index reads, recorded as `#<index>` (the trait's defaults would record the name they read).
    fn position_at(&self, index: usize) -> f64 {
        self.read("position", &format!("#{index}"));
        0.0
    }
    fn price_at(&self, index: usize) -> f64 {
        self.read("price", &format!("#{index}"));
        100.0
    }
    fn instrument_grid_at(&self, index: usize) -> Option<Grid> {
        self.read("grid", &format!("#{index}"));
        Some(Grid { tick_size: Some(0.5), lot_size: None, min_notional: None, min_qty: None })
    }
    fn contract_value_at(&self, index: usize) -> Option<ContractValue> {
        self.read("value", &format!("#{index}"));
        Some(ContractValue::linear(1.0))
    }
}

/// A rule that does `step` on bar `k`.
struct Steps(fn(&mut Tactics<'_>, i64));

impl Rule for Steps {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.0)(strategy, bar.timestamp / HOUR);
    }
}

/// The reads `run` asked a broker whose universe is `universe` over three bars, and how often it asked
/// for the universe.
fn asked(universe: &[&str], run: RuleRun<Steps>) -> (Vec<(&'static str, String)>, usize) {
    let mut broker = Asked {
        universe: universe.iter().map(|s| s.to_string()).collect(),
        universe_asked: Cell::new(0),
        reads: RefCell::default(),
    };
    let mut run = run.trades(&[OTHER]);
    for k in 0..3 {
        Strategy::<Asked>::on_bar(&mut run, &mut broker, &bar(k * HOUR, 100.0));
    }
    (broker.reads.take(), broker.universe_asked.get())
}

/// Each symbol is read by its index in the broker's universe (sparing a strategy file's host the name's
/// check and search), its OWN index whatever its place: the run's own symbol may stand after a leg's (a
/// script trading its first bar's symbol, which may be a leg's), and is read at its own index then, never
/// at 0. A broker with no universe (a live one) is read by name. The universe is asked once a book a run.
#[test]
fn every_symbol_is_read_by_its_own_index_where_the_universe_holds_it() {
    fn reads_all(s: &mut Tactics<'_>, _: i64) {
        s.position();
        s.price();
        s.sizing();
        s.tick();
        s.on(OTHER).position();
        s.price();
    }
    let three_bars = |own: &str, other: &str| {
        let own = ["position", "price", "value", "grid", "grid"].map(|w| (w, own.to_string()));
        let other = ["position", "price"].map(|w| (w, other.to_string()));
        let bar: Vec<_> = own.into_iter().chain(other).collect();
        bar.iter().chain(&bar).chain(&bar).cloned().collect::<Vec<_>>()
    };
    for (universe, (own, other), asked_for) in [
        (&[SYMBOL, OTHER][..], ("#0", "#1"), 2),
        (&[][..], (SYMBOL, OTHER), 2),
        (&[OTHER, SYMBOL][..], ("#1", "#0"), 2),
        (&[SYMBOL][..], ("#0", OTHER), 2),
    ] {
        let (reads, universe_asked) = asked(universe, RuleRun::new(Steps(reads_all), SYMBOL));
        assert_eq!(reads, three_bars(own, other), "universe {universe:?}");
        assert_eq!(universe_asked, asked_for, "universe {universe:?}: once a book");
    }
}

/// The grid is asked for its tick only by a pass that needs one, and once a pass for each symbol made
/// current: a bar that only reads its position asks nothing, a bar stating five levels asks once, and the
/// snapped levels are the ones the grid's tick gives. With the run's own tick (`RuleRun::tick`) the run's
/// own symbol asks the grid nothing at all.
#[test]
fn the_grid_is_asked_for_its_tick_once_a_pass_and_only_when_a_level_needs_it() {
    fn levels(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.position();
            }
            1 => {
                s.entry("A", Long).stop(101.3).limit(101.9);
                s.exit("A guard").from("A").stop(90.2).take(110.7);
                assert_eq!(s.snap_limit(Short, 99.1), 99.5);
            }
            _ => {
                let own = s.snap_stop(Long, 100.3);
                let other = s.on(OTHER).snap_stop(Long, 60.3);
                let back = s.on("").snap_stop(Long, 100.3);
                assert_eq!((own, other, back), (100.5, 60.5, 100.5));
            }
        }
    }
    let grid = |reads: Vec<(&'static str, String)>| -> Vec<String> {
        reads.into_iter().filter(|(what, _)| *what == "grid").map(|(_, symbol)| symbol).collect()
    };
    let (reads, _) = asked(&[SYMBOL, OTHER], RuleRun::new(Steps(levels), SYMBOL));
    assert_eq!(grid(reads), ["#0", "#0", "#1", "#0"], "bar 1 once, bar 2 per symbol made current");
    let (reads, _) = asked(&[SYMBOL, OTHER], RuleRun::new(Steps(levels), SYMBOL).tick(0.5));
    assert_eq!(grid(reads), ["#1"], "the run's own tick is the own symbol's");
}

/// Before each bar reaches the rule: the simulator answers every index read (`OrderBroker`'s `_at` reads)
/// exactly as the same read of the symbol or input at that index by its name, bit for bit, for each of
/// the two symbols the rule trades (so a read of the wrong one would differ: their positions, prices and
/// contract values do) and for its daily input; `""`, the mounted symbol's other name, reads as index 0.
struct IndexIsName {
    run: RuleRun<DaysAgainstWeeks>,
    checked: usize,
    held: usize,
}

impl Strategy<OrderBrokerSimulator> for IndexIsName {
    fn inputs(&self) -> Vec<String> {
        Strategy::<OrderBrokerSimulator>::inputs(&self.run)
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        // The trait's own answers: no inherent method of the simulator's broker stands in for one.
        let view: &OrderBrokerSimulator = b;
        let at = bar.timestamp;
        assert_eq!(OrderBroker::symbols(view), [SYMBOL, OTHER]);
        assert_eq!(OrderBroker::symbol_index(view, OTHER), Some(1));
        for (index, name) in [(0, SYMBOL), (1, OTHER), (0, "")] {
            let same = |by_index: f64, by_name: f64, what: &str| {
                assert_eq!(by_index.to_bits(), by_name.to_bits(), "{what} of {name:?} at {at}");
            };
            same(
                OrderBroker::position_at(view, index),
                OrderBroker::position(view, name),
                "position",
            );
            same(OrderBroker::price_at(view, index), OrderBroker::price(view, name), "price");
            let average = OrderBroker::average_price(view, name);
            same(OrderBroker::average_price_at(view, index), average, "average");
            let grid = OrderBroker::instrument_grid(view, name);
            assert_eq!(OrderBroker::instrument_grid_at(view, index), grid, "{name:?} at {at}");
            let value = OrderBroker::contract_value(view, name);
            assert_eq!(OrderBroker::contract_value_at(view, index), value, "{name:?} at {at}");
            let (by_index, by_name) =
                (OrderBroker::bars_at(view, index), OrderBroker::bars(view, name));
            assert_eq!((by_index.as_ptr(), by_index.len()), (by_name.as_ptr(), by_name.len()));
        }
        assert_ne!(
            OrderBroker::contract_value_at(view, 0),
            OrderBroker::contract_value_at(view, 1),
            "the two symbols' values differ"
        );
        let daily = OrderBroker::input_index(view, DAILY).expect("the daily input is listed");
        let input = (OrderBroker::input_bars_at(view, daily), OrderBroker::input_bars(view, DAILY));
        assert_eq!((input.0.as_ptr(), input.0.len()), (input.1.as_ptr(), input.1.len()), "at {at}");
        self.held += usize::from(OrderBroker::position_at(view, 0) != 0.0);
        self.checked += 1;
        self.run.on_bar(b, bar);
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, f: &Fill) {
        self.run.on_fill(b, f);
    }
    fn on_order_event(&mut self, b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.run.on_order_event(b, event);
    }
}

#[test]
fn the_simulator_answers_an_index_as_its_name() {
    let start = 18_500 * DAY;
    let stamps: Vec<i64> = (0..6_000).map(|q| start + q * QUARTER).collect();
    let days: Vec<i64> = (-60..70).filter(|d| d % 17 != 0).map(|d| start + d * DAY).collect();
    let rule = DaysAgainstWeeks {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        day: None,
        week: None,
        handed: (0, 0),
    };
    let series = vec![InputSeries {
        name: DAILY.to_string(),
        interval_ms: DAY,
        bars: walk(&days, 0x1A_9E7C_0FFE_E000, 100.0),
    }];
    let params = StrategyEngineConfig {
        cash: 100_000.0,
        risk_limits: Some(RiskLimits { tick_size: Some(0.05), ..RiskLimits::new() }),
        contract_values: vec![(OTHER.to_string(), ContractValue::linear(0.5))],
        inputs: Inputs { bar_ms: QUARTER, series },
        ..StrategyEngineConfig::default()
    };
    let symbols = vec![
        (SYMBOL.to_string(), walk(&stamps, 0x5EED_FACE_C0DE_1234, 100.0)),
        (OTHER.to_string(), walk(&stamps, 0x0DD5_EED0_2BAD_C0DE, 60.0)),
    ];
    let run = RuleRun::new(rule, SYMBOL).trades(&[OTHER]);
    let probe = IndexIsName { run, checked: 0, held: 0 };
    let mut engine = StrategyEngine::new(symbols, probe, params);
    let r = engine.run();
    let probe = &engine.strategy;
    assert_eq!(probe.checked, 2 * 6_000, "every bar of both symbols was checked");
    let (held, trades) = (probe.held, r.trades.len());
    assert!(held > 100 && trades > 1, "{held} bars held, {trades} trades: a read compared nothing");
}
