//! `OrderBroker::input` on the simulator (decision 0271): a strategy reads the last bar of an input that had
//! CLOSED by the event, never the bar in progress. A bar of interval `I` opened at `ts` closes at
//! `ts + I`; on the bar lane the decision is at the run's bar's close and its fills at its open, on the
//! tick lane the event is the tick. The boundary (an input bar closing exactly at the event) is seen.
//! `OrderBroker::input_whole`, the one read ahead (decision 0271's amendment), is the view at the run's
//! last event, and only where the run serves it.

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

/// Records each input's whole series (`OrderBroker::input_whole`, the bars' open times) at every
/// decision and every tick.
#[derive(Default)]
struct WholeReader {
    seen: Vec<Option<Vec<i64>>>,
}

impl WholeReader {
    fn read(&mut self, b: &OrderBrokerSimulator) {
        let whole = b.input_whole(NAME).map(|w| w.iter().map(|x| x.timestamp).collect());
        let undeclared = b.input_whole("binance:ETHUSDT:1h");
        assert_eq!(undeclared.map(<[Bar]>::len), whole.as_ref().map(|_| 0), "an undeclared name");
        self.seen.push(whole);
    }
}

impl Strategy<OrderBrokerSimulator> for WholeReader {
    fn inputs(&self) -> Vec<String> {
        vec![NAME.to_string()]
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.read(b);
    }
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.read(b);
    }
}

/// The one read that looks ahead (decision 0271's amendment of 2026-11-14): from the first event on,
/// `input_whole` holds every input bar that closes by the run's LAST event, the ones still to come
/// included, and never one the run loaded past it. Served only when the run says so
/// (`StrategyEngineConfig::input_whole`, a backtest-only strategy file's); otherwise `None`, as on paper
/// and live, and `input` keeps its as-of rule either way.
#[test]
fn the_whole_series_ends_at_the_runs_last_event_and_is_served_only_when_the_run_says_so() {
    // Five half-hour bars: the last closes at 2:30, by which the hourly bars of 0:00 and 1:00 have
    // closed and those of 2:00 and 3:00, loaded too, have not.
    let bars: Vec<Bar> = (0..5).map(|i| bar(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let on_bars = |input_whole| {
        let params =
            StrategyEngineConfig { inputs: inputs(HALF_HOUR), input_whole, ..Default::default() };
        let mut engine = StrategyEngine::new(
            vec![(SYM.to_string(), bars.clone())],
            WholeReader::default(),
            params,
        );
        engine.run();
        engine.strategy.seen
    };
    let served = on_bars(true);
    assert_eq!(served.len(), 5);
    assert!(
        served.iter().all(|w| w.as_deref() == Some(&[0, HOUR][..])),
        "the same whole series at every decision, the first included: {served:?}"
    );
    assert!(on_bars(false).iter().all(Option::is_none), "not served: none");
    assert!(
        !StrategyEngineConfig::default().input_whole,
        "off by default: no run reads ahead unless its harness says so"
    );

    // The tick lane ends at its last tick, 2:00 + 1 ms: the 1:00 bar closed at 2:00, the 2:00 bar has not.
    let tape: Vec<_> =
        [HOUR - 1, 2 * HOUR + 1].iter().map(|&ts| quote(SYM, ts, 1.0, 1.1, 1.0, 1.0)).collect();
    let params =
        StrategyEngineConfig { inputs: inputs(0), input_whole: true, ..Default::default() };
    let mut engine =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], WholeReader::default(), params);
    engine.run_ticks(&[(SYM.to_string(), tape)]);
    let seen = &engine.strategy.seen;
    assert!(
        !seen.is_empty() && seen.iter().all(|w| w.as_deref() == Some(&[0, HOUR][..])),
        "{seen:?}"
    );
}

/// Publishes a forecast at every decision and buys at the first.
struct Forecaster;

impl Strategy<OrderBrokerSimulator> for Forecaster {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        b.publish_forecast("", vike_model::Forecast::new(bar.close, HALF_HOUR, None).unwrap());
        if bar.timestamp == 0 {
            b.place(SYM, "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
}

/// A bar step DECIDES at its close: its forecast is stamped there (a study deciding one millisecond
/// earlier must not read it), while its order still fills at the next bar's open. A run whose harness
/// states no bar length stamps the open, as before.
#[test]
fn a_bar_steps_forecast_is_stamped_at_its_close_and_its_order_is_not_moved() {
    let bars: Vec<Bar> = (0..3).map(|i| bar(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let run = |bar_ms| {
        let params = StrategyEngineConfig {
            inputs: Inputs { bar_ms, series: Vec::new() },
            ..Default::default()
        };
        StrategyEngine::new(vec![(SYM.to_string(), bars.clone())], Forecaster, params).run()
    };
    let stated = run(HALF_HOUR);
    let at: Vec<i64> = stated.forecasts.iter().map(|r| r.timestamp).collect();
    assert_eq!(at, [HALF_HOUR, 2 * HALF_HOUR, 3 * HALF_HOUR], "each at its bar's close");
    let unstated = run(0);
    let at: Vec<i64> = unstated.forecasts.iter().map(|r| r.timestamp).collect();
    assert_eq!(at, [0, HALF_HOUR, 2 * HALF_HOUR], "no bar length: the open");
    assert_eq!(stated.trades, unstated.trades, "the order's timing does not move");
    assert_eq!(stated.final_equity.to_bits(), unstated.final_equity.to_bits());
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
