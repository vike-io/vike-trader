//! `OrderBroker::input` on the simulator (decision 0271): a strategy reads the last bar of an input that had
//! CLOSED by the event, never the bar in progress. A bar of interval `I` opened at `ts` closes at
//! `ts + I`; on the bar lane the decision is at the run's bar's close and its fills at its open, on the
//! tick lane the event is the tick. The boundary (an input bar closing exactly at the event) is seen.

use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy};
use vike_simulator::test_support::{bar, quote};
use vike_simulator::{
    InputSeries, Inputs, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};

const SYM: &str = "SYM0";
const NAME: &str = "binance:BTCUSDT:1h";
const HALF_HOUR: i64 = 1_800_000;
const HOUR: i64 = 3_600_000;

/// Hourly input bars from 0: the close of each names its index (10, 20, 30, 40), and each HIGH is a
/// value no closed bar carries, so a read of the bar in progress could not pass for a closed one.
fn hourly() -> Vec<Bar> {
    (0..4).map(|k| bar(k * HOUR, 1.0, 1e9, 0.5, 10.0 * (k + 1) as f64, 1.0)).collect()
}

fn inputs(bar_ms: i64) -> Inputs {
    Inputs {
        bar_ms,
        series: vec![InputSeries { name: NAME.to_string(), interval_ms: HOUR, bars: hourly() }],
    }
}

/// Records what the input read at each decision and each fill, and buys once at its third decision.
#[derive(Default)]
struct Reader {
    decisions: Vec<(i64, Option<f64>)>,
    fills: Vec<(i64, Option<f64>)>,
    ticks: Vec<(i64, Option<f64>)>,
}

impl Reader {
    fn read(b: &OrderBrokerSimulator) -> Option<f64> {
        let seen = b.input(NAME)?;
        // Never a bar that had not closed by now (the bar lane's decision is checked by the caller).
        assert!(
            seen.timestamp + HOUR <= b.now() + HALF_HOUR,
            "a bar in progress was read: {seen:?}"
        );
        Some(seen.close)
    }
}

impl Strategy<OrderBrokerSimulator> for Reader {
    fn inputs(&self) -> Vec<String> {
        vec![NAME.to_string()]
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        self.decisions.push((bar.timestamp, Reader::read(b)));
        assert_eq!(b.input("binance:ETHUSDT:1h"), None, "an undeclared name reads nothing");
        if self.decisions.len() == 3 {
            b.place(SYM, "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push((fill.timestamp, b.input(NAME).map(|x| x.close)));
    }
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, q: &TickQuote) {
        self.ticks.push((q.timestamp, b.input(NAME).map(|x| x.close)));
    }
}

/// The bar lane: half-hour bars against an hourly input. A decision at the close of a run bar sees the
/// input bar that closed at or before it, so the input bar closing WITH it (every second decision) is
/// seen and the one in progress never is.
#[test]
fn a_bar_decision_reads_the_last_input_bar_closed_by_its_close() {
    let bars: Vec<Bar> = (0..8).map(|i| bar(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let params = StrategyEngineConfig { inputs: inputs(HALF_HOUR), ..Default::default() };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), bars)], Reader::default(), params);
    engine.run();
    let seen: Vec<Option<f64>> = engine.strategy.decisions.iter().map(|(_, v)| *v).collect();
    assert_eq!(
        seen,
        vec![
            None,       // closes 0:30: the first input bar closes at 1:00
            Some(10.0), // closes 1:00: the input bar 0:00-1:00 closes at exactly this time, seen
            Some(10.0), // closes 1:30: the bar 1:00-2:00 is in progress, never read
            Some(20.0),
            Some(20.0),
            Some(30.0),
            Some(30.0),
            Some(40.0),
        ]
    );
    // The buy sent at the third decision (close 1:30) fills at the next bar's OPEN, 1:30: its hook sees
    // the input as of the open (10), while the decision of that same bar, at its close 2:00, sees 20.
    assert_eq!(engine.strategy.fills, vec![(3 * HALF_HOUR, Some(10.0))]);
    assert_eq!(engine.strategy.decisions[3], (3 * HALF_HOUR, Some(20.0)));
}

/// A run whose bar lane states no bar length decides at the bar's open: it can only see LESS, never a
/// bar that had not closed (the harness always states it).
#[test]
fn an_unstated_bar_length_never_reads_ahead() {
    let bars: Vec<Bar> = (0..4).map(|i| bar(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let params = StrategyEngineConfig { inputs: inputs(0), ..Default::default() };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), bars)], Reader::default(), params);
    engine.run();
    let seen: Vec<Option<f64>> = engine.strategy.decisions.iter().map(|(_, v)| *v).collect();
    assert_eq!(seen, vec![None, None, Some(10.0), Some(10.0)]);
}

/// The tick lane: a tick sees the input bar that closed at or before its own time. One millisecond
/// before the close it does not; at the close it does.
#[test]
fn a_tick_reads_the_last_input_bar_closed_by_the_tick() {
    let at = [HOUR - 1, HOUR, HOUR + 1, 2 * HOUR - 1, 2 * HOUR, 5 * HOUR];
    let tape: Vec<_> = at.iter().map(|&ts| quote(SYM, ts, 1.0, 1.1, 1.0, 1.0)).collect();
    let params = StrategyEngineConfig { inputs: inputs(0), ..Default::default() };
    let mut engine =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], Reader::default(), params);
    engine.run_ticks(&[(SYM.to_string(), tape)]);
    assert_eq!(
        engine.strategy.ticks,
        vec![
            (HOUR - 1, None),
            (HOUR, Some(10.0)),
            (HOUR + 1, Some(10.0)),
            (2 * HOUR - 1, Some(10.0)),
            (2 * HOUR, Some(20.0)),
            (5 * HOUR, Some(40.0)),
        ]
    );
}

/// A run with no input reads nothing and the default params carry none.
#[test]
fn no_input_reads_nothing() {
    assert_eq!(StrategyEngineConfig::default().inputs, Inputs::default());
    let bars: Vec<Bar> = (0..2).map(|i| bar(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), bars)],
        Reader::default(),
        StrategyEngineConfig::default(),
    );
    engine.run();
    assert!(engine.strategy.decisions.iter().all(|(_, v)| v.is_none()));
}
