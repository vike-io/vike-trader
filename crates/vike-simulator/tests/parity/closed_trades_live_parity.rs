//! **The closed trades of a strategy, in the backtest and on a paper mount** (decision 0271 item 8): the
//! simulator (`OrderBrokerSimulator`'s closed-trade ledger) and the live core (`vike_core`'s per-mount ledger, booked from
//! every fill delivered to the mount) hand a strategy the SAME rows at every decision, field for field, bar
//! indices included, because both book with the one ledger (`vike_trade_ledger::TradeClosedLedger`)
//! the same fills (`tests/r7_gate.rs` proves the paper fills bit-identical) at the same bar.
//!
//! One portable script (`impl<B: OrderBroker> Strategy<B>`) opens, adds, sells across both lots (a partial close of
//! the second), flips short, flips back long and closes; it records `OrderBroker::closed_trades()` at every bar and
//! every fill. The two runs must record the same list. The live run is the paper stack: the live core over a
//! `vike_paper::ExecutionClientPaper`, fed the same bars on the bar lane.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, Mount, spawn_core};
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderPlacement, RiskLimits, Strategy, TradeClosedRow};
use vike_paper::{ExecutionClientPaper, replay_bars};
use vike_simulator::{StrategyEngine, StrategyEngineConfig};

const VENUE: &str = "paper";
const SYMBOL: &str = "BTCUSDT";
/// `vike_simulator::test_support::gen_bars` steps a minute.
const INTERVAL: &str = "1m";
const BARS: usize = 20;
const SLIPPAGE: f64 = 0.0002;
const MAKER: f64 = 0.0002;
const TAKER: f64 = 0.0007;

/// The script: at the k-th decision, the market order sent. Long 2, long 3, long 0.5 (the first lot closed
/// whole, half the second), short 1 (the flip), long 1 (the flip back), flat.
const SCRIPT: &[(usize, i32, f64)] =
    &[(2, 1, 2.0), (4, 1, 1.0), (6, -1, 2.5), (8, -1, 1.5), (10, 1, 2.0), (13, -1, 1.0)];

/// What one hook was lent: the hook, its instant, the closed rows.
type Read = (&'static str, i64, Vec<TradeClosedRow>);

/// PORTABLE: the backtest and the live core mount this very code.
struct Script {
    decisions: usize,
    reads: Arc<Mutex<Vec<Read>>>,
}

impl Script {
    fn read<B: OrderBroker>(&self, hook: &'static str, ts: i64, b: &B) {
        let rows = b.closed_trades().expect("both keep a closed-trade ledger").to_vec();
        self.reads.lock().unwrap().push((hook, ts, rows));
    }
}

impl<B: OrderBroker> Strategy<B> for Script {
    fn on_bar(&mut self, b: &mut B, bar: &Bar) {
        self.read("bar", bar.timestamp, b);
        if let Some(&(_, side, qty)) = SCRIPT.iter().find(|s| s.0 == self.decisions) {
            let symbol = bar.symbol.as_deref().unwrap_or_default();
            b.place(symbol, "", side, qty, OrderPlacement::Market, false);
        }
        self.decisions += 1;
    }
    fn on_fill(&mut self, b: &mut B, fill: &Fill) {
        self.read("fill", fill.timestamp, b);
    }
}

fn script(reads: &Arc<Mutex<Vec<Read>>>) -> Script {
    Script { decisions: 0, reads: Arc::clone(reads) }
}

fn backtest(bars: &[Bar]) -> Vec<Read> {
    let reads = Arc::default();
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    StrategyEngine::new(vec![(SYMBOL.to_string(), bars.to_vec())], script(&reads), params).run();
    reads.lock().unwrap().clone()
}

/// The paper stack over the same bars, waited on until it has read `expect` times (or 30 s passed).
fn paper(bars: &[Bar], expect: usize) -> Vec<Read> {
    let reads = Arc::default();
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER),
        VENUE,
        SYMBOL,
    );
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(script(&reads)),
            }),
            ..CoreConfig::default()
        },
    );
    replay_bars(&handle.bar_sender(), VENUE, SYMBOL, INTERVAL, bars);
    let deadline = Instant::now() + Duration::from_secs(30);
    while reads.lock().unwrap().len() < expect && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    reads.lock().unwrap().clone()
}

/// **The backtest and the paper mount lend the same closed trades at every decision**, row for row and field
/// for field, the bar indices included.
#[test]
fn the_backtest_and_a_paper_mount_hand_a_strategy_the_same_closed_trades() {
    let bars = vike_simulator::test_support::gen_bars(BARS, None);
    let reference = backtest(&bars);
    // Every bar read once, every fill once: the six orders, each one fill.
    assert_eq!(reference.len(), BARS + SCRIPT.len(), "{reference:#?}");
    let rows = &reference.last().expect("read").2;
    assert_eq!(rows.len(), 5, "two rows for the sell across both lots, one per flip, one close");
    // The entry decided at the bar of index 2 fills at the next bar's open, so its bar is 3; its close
    // (decided at 6) is at 7.
    assert_eq!((rows[0].entry_bar, rows[0].exit_bar, rows[0].qty), (3, 7, 2.0));
    assert_eq!((rows[2].side, rows[3].side, rows[4].side), (1, -1, 1), "the flips");
    assert!(rows.iter().all(|r| r.commission > 0.0), "each row carries its fee shares");

    assert_eq!(paper(&bars, reference.len()), reference);
}
