//! The language's reads beside a rule's own bars, on `vike-simulator`'s engine (a dev edge): an input as of
//! the bar's close and its history (`Tactics::input`, decision 0271), the rule's inputs reaching the run
//! (`Rule::inputs` through `RuleRun`), and the closed trades as the engine's ledger booked them
//! (`Tactics::closed_trades`, `Tactics::closed_net`).

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::{Fill, Strategy, TradeClosedRow};
use vike_simulator::{
    InputSeries, Inputs, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics};

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

struct Driven(RuleRun<Reader>);

impl Strategy<OrderBrokerSimulator> for Driven {
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

/// Hourly bars from day 3, a daily input from day 0, the rule run over them: what it saw, and the bars.
fn run() -> (Seen, Closed, Vec<TradeClosedRow>, Vec<Bar>) {
    let days: Vec<Bar> = (0..5).map(|d| bar(d * DAY, 100.0 + d as f64)).collect();
    let hours: Vec<Bar> = (0..30).map(|h| bar(3 * DAY + h * HOUR, 50.0 + h as f64)).collect();
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
