//! **The as-of agreement of a strategy's INPUTS** (decision 0271): the backtest (`OrderBrokerSimulator`'s view of
//! `StrategyEngineConfig::inputs`) and the live core (a mount's inputs, opened through `CoreConfig::input_opener`)
//! read the same input bar at every decision, because both ask the one rule, `vike_marketdata::Bar::closed_at`,
//! at the same instant: a bar step's CLOSE.
//!
//! One portable probe (`impl<B: OrderBroker> Strategy<B>`) records what it read at each decision; the two runs
//! must record the same list, tuple for tuple. The inputs are the traded symbol's own higher timeframe and a
//! user's daily lane, and the decisions cross every boundary: an input bar closing exactly at the decision
//! (seen), the bar in progress (never seen), and the decisions before the first input bar closes (nothing).
//! Each decision also records the input's whole closed history (`OrderBroker::input_bars`), which the two must hold
//! alike, bar for bar. The live run is checked twice: with every input bar in the stored history the opener answers, and with
//! the history holding only the first bar while the rest ARRIVE on the bar lane, each closing before the
//! decision that shares its close, and once more with each such bar arriving just AFTER that decision's own bar
//! (the feed promises neither order; the shared close waits, decision 0271's limit (a) closed).

use std::sync::{Arc, Mutex};

use vike_core::{CoreConfig, InputRead, Mount, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, BarUpdate, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, RiskLimits, Strategy};
use vike_simulator::{InputSeries, Inputs, StrategyEngine, StrategyEngineConfig};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const H: i64 = 3_600_000;
const H4: i64 = 4 * H;
const DAY: i64 = 24 * H;
const HIGHER: &str = "binance:BTCUSDT:4h";
const LANE: &str = "user-mine:binance:USDT-USD:1d";
/// Thirty hourly decisions: past the day bar's close, across seven 4h closes.
const HOURS: i64 = 30;

fn bar(ts: i64, close: f64) -> Bar {
    vike_simulator::test_support::bar(ts, close, close, close, close, 1.0)
}

/// The run's own hourly bars, each close naming its hour.
fn own() -> Vec<Bar> {
    (0..HOURS).map(|k| bar(k * H, 100.0 + k as f64)).collect()
}

/// The 4h input: one bar per four hours, each close naming its index.
fn higher() -> Vec<Bar> {
    (0..8).map(|k| bar(k * H4, 10.0 * (k + 1) as f64)).collect()
}

/// The lane: two day bars.
fn lane() -> Vec<Bar> {
    vec![bar(0, 1.0), bar(DAY, 2.0)]
}

/// A decision's `(ts, the 4h input's last close, the lane's, the 4h input's closed history as closes)`.
type Look = (i64, Option<f64>, Option<f64>, Vec<f64>);

/// A look without its history.
fn head(look: &Look) -> (i64, Option<f64>, Option<f64>) {
    (look.0, look.1, look.2)
}

/// PORTABLE: the backtest and the live core mount this very code.
struct Probe(Arc<Mutex<Vec<Look>>>);

impl<B: OrderBroker> Strategy<B> for Probe {
    fn inputs(&self) -> Vec<String> {
        vec![HIGHER.to_string(), LANE.to_string()]
    }
    fn on_bar(&mut self, b: &mut B, bar: &Bar) {
        let history = b.input_bars(HIGHER).iter().map(|x| x.close).collect();
        let look = (
            bar.timestamp,
            b.input(HIGHER).map(|x| x.close),
            b.input(LANE).map(|x| x.close),
            history,
        );
        self.0.lock().unwrap().push(look);
    }
}

fn backtest() -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let series =
        |name: &str, interval_ms, bars| InputSeries { name: name.to_string(), interval_ms, bars };
    let params = StrategyEngineConfig {
        cash: 1_000_000.0,
        inputs: Inputs {
            bar_ms: H,
            series: vec![series(HIGHER, H4, higher()), series(LANE, DAY, lane())],
        },
        ..StrategyEngineConfig::default()
    };
    StrategyEngine::new(vec![(SYMBOL.to_string(), own())], Probe(Arc::clone(&looks)), params).run();
    looks.lock().unwrap().clone()
}

/// The live core over the same bars: the opener answers `stored` of each input, and every bar of the 4h
/// input it did not is sent on the bar lane just before the own bar whose close it shares, or just AFTER it
/// when `late` (the order the feed may deliver them in: the shared close waits for it).
fn live(stored: usize, late: bool) -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: "1h".into(),
            strategy: Box::new(Probe(Arc::clone(&looks))),
        }),
        input_opener: Some(Box::new(
            move |name: &str, _venue: &str, _symbol: &str, _interval: &str| match name {
                HIGHER => Ok(InputRead {
                    key: (VENUE.into(), SYMBOL.into(), "4h".into()),
                    interval_ms: H4,
                    bars: higher().into_iter().take(stored).collect(),
                    calendar: None,
                }),
                LANE => Ok(InputRead {
                    key: ("user-mine:binance".into(), "USDT-USD".into(), "1d".into()),
                    interval_ms: DAY,
                    bars: lane(),
                    calendar: None,
                }),
                other => Err(format!("no series {other}")),
            },
        )),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let send = |interval: &str, bar: Bar| {
        let update = BarUpdate {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: interval.into(),
            bar,
            sides: None,
        };
        handle.bar_sender().close(update).unwrap();
    };
    let pending: Vec<Bar> = higher().into_iter().skip(stored).collect();
    for b in own() {
        let decision = b.timestamp + H;
        let shared: Vec<Bar> =
            pending.iter().filter(|p| p.timestamp + H4 == decision).cloned().collect();
        if late {
            send("1h", b);
            shared.into_iter().for_each(|p| send("4h", p));
        } else {
            shared.into_iter().for_each(|p| send("4h", p));
            send("1h", b);
        }
    }
    handle.shutdown_and_join();
    looks.lock().unwrap().clone()
}

/// **The two runs read the same input bar at every decision**, the stored history and the streamed bars
/// alike, and the boundary is where the rule puts it.
#[test]
fn the_backtest_and_the_live_core_read_an_input_as_of_the_same_close() {
    let reference = backtest();
    assert_eq!(reference.len(), HOURS as usize);
    // The boundary, spelled once: the decision at 04:00 (the bar opened at 03:00) sees the 4h bar that
    // closes then; the one at 03:00 does not, and nothing is seen before the first 4h close.
    assert_eq!(head(&reference[2]), (2 * H, None, None));
    assert_eq!(head(&reference[3]), (3 * H, Some(10.0), None));
    assert_eq!(head(&reference[6]), (6 * H, Some(10.0), None));
    assert_eq!(head(&reference[7]), (7 * H, Some(20.0), None));
    assert_eq!(reference[22].2, None, "the day bar in progress at 23:00 is not seen");
    assert_eq!(reference[23].2, Some(1.0), "the day bar closing at 24:00 is");
    assert_eq!(
        reference[7].3,
        vec![10.0, 20.0],
        "the history: every 4h bar closed by 08:00, oldest first"
    );
    assert_eq!(live(8, false), reference, "every input bar in the stored history");
    assert_eq!(live(1, false), reference, "the history's first bar, the rest on the bar lane");
    // Decision 0271's limit (a), closed: the input bar of a shared close arriving AFTER the decision's own bar
    // is still read at that decision (the step waits for it, `vike_core::SHARED_CLOSE_GRACE_MS` at most).
    assert_eq!(live(1, true), reference, "each input bar arriving just after the own bar");
}
