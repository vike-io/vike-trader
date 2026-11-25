//! The data `forward_report`'s tests run it on, in ONE place for both test binaries that compile the study
//! (`crates/vike-user-research/tests/pipeline.rs` and `crates/vike-strategy-sandbox/tests/study_child.rs`).
//! `#[cfg(test)]` in the study, so the study's own build never compiles it.
//!
//! A five-day forward test of one strategy, four round trips a day, recorded twice:
//! - the saved run `backtest` (a `StudyRuns` double): twenty round trips inside the window, plus one that
//!   opened before it and one that closes after it (neither counts);
//! - the account's fills on `paper-1` (the workspace's own store double): each signal entered 30 s late
//!   and left 45 s late, each at a few basis points worse ([`slippage_bps`]), except signal [`MISSED`],
//!   which the forward never took; one exit filled in two parts; one trade the backtest never made ([`EXTRA`]); and a position
//!   opened before the window that closes inside it. A stray round trip on another account (`demo-2`)
//!   is what the recipe's `account` keeps out.
//!
//! The run `no_ledger` kept none, as a run whose producer saves no trade ledger.

use std::path::PathBuf;
use std::sync::Arc;

use vike_data::{ExecFillRow, ExecForecastRow, HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_model::Trade;
use vike_user_research::{StudyContext, StudyForwardRun, StudyRuns};

pub const VENUE: &str = "binance";
pub const SYMBOL: &str = "BTCUSDT";
pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 24 * HOUR;
/// The window opens on a day boundary.
pub const START: i64 = 20_000 * DAY;
pub const DAYS: i64 = 5;
pub const SIGNALS: usize = 20;
/// The signal the forward test never traded.
pub const MISSED: usize = 7;
/// The forward test's own trade, in signal 9's slot, three hours after its exit.
pub const EXTRA: i64 = START + 9 * 6 * HOUR + 4 * HOUR;
pub const FEE: f64 = 0.0004;
pub const RECIPE: &str = "backtest_run = \"backtest\"\nvenue = \"binance\"\nsymbol = \"BTCUSDT\"\n\
                          capital = 10000.0\naccount = \"paper-1\"\n";
/// The same report named by the forward run alone: its window, its backtest, its venue, symbol and account.
pub const FORWARD_RECIPE: &str = "forward_run = \"forward\"\ncapital = 10000.0\n";
/// The mount the forward test ran under: every `paper-1` fill and live forecast of the strategy names it.
pub const MOUNT: &str = "binance__BTCUSDT__1m";
/// The forward run with every opt-in key: its mount's fills, the forecasts compared, the drift alarms with a
/// slippage threshold the fixture's drift crosses and a return threshold of one hundredth of a basis point.
pub const EVERY_KEY_RECIPE: &str = "forward_run = \"forward\"\ncapital = 10000.0\ncompare_forecasts = true\n\
                                    [drift]\nslippage_tolerance_bps = 1.0\nslippage_threshold_bps = 5.0\n\
                                    return_tolerance = 0.0\nreturn_threshold = 0.000001\n";
/// What the live forecasts add to the backtest's from signal 10 on (the model drifts with the fills).
pub const LIVE_FORECAST_SHIFT: f64 = 0.001;
/// Another mount on `paper-1` (its round trip in signal 12's slot) and a hand-placed one naming no mount (in
/// signal 14's), only in [`store_with_another_mount`].
pub const OTHER_MOUNT: &str = "binance__BTCUSDT__5m";

/// The window: the five days.
pub fn window() -> TimestampRange {
    TimestampRange::of(START, START + DAYS * DAY - 1)
}

/// Signal `k`: long or short, its entry time and price, and the move in its favour by its exit.
fn signal(k: usize) -> (bool, i64, f64, f64) {
    const MOVES: [f64; 7] = [0.012, -0.006, 0.009, 0.004, -0.008, 0.015, 0.002];
    (k % 3 != 2, START + k as i64 * 6 * HOUR + HOUR, 100.0 + (k % 7) as f64, MOVES[k % 7])
}

/// A size-1 round trip from `entry` moving `favour` by `exit`, charged [`FEE`] on both fills.
fn round_trip(is_long: bool, entry: (i64, f64), exit_at: i64, favour: f64) -> Trade {
    let side = if is_long { 1.0 } else { -1.0 };
    let exit = entry.1 * (1.0 + side * favour);
    Trade {
        entry_price: entry.1,
        exit_price: exit,
        size: 1.0,
        pnl: side * (exit - entry.1),
        fees: FEE * (entry.1 + exit),
        entry_timestamp: entry.0,
        exit_timestamp: exit_at,
        symbol: String::new(),
        mae: 0.0,
        mfe: 0.0,
        is_long,
    }
}

/// The backtest's ledger, in closing order.
fn ledger() -> Vec<Trade> {
    let mut trades = vec![round_trip(true, (START - 3 * HOUR, 99.0), START - HOUR, 0.01)];
    for k in 0..SIGNALS {
        let (is_long, at, price, favour) = signal(k);
        trades.push(round_trip(is_long, (at, price), at + 2 * HOUR, favour));
    }
    let late = START + DAYS * DAY - HOUR;
    trades.push(round_trip(true, (late, 101.0), late + 2 * HOUR, 0.01));
    trades
}

/// One account fill, as the journal materializer writes it.
fn fill(n: usize, ts: i64, side: i32, qty: f64, price: f64, account: &str) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{n}"),
        order_client_id: format!("c{n}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        px: price,
        commission: FEE * price * qty,
        mark_price: None,
        liquidity_side: "taker".into(),
        commission_asset: "USDT".into(),
        venue_order_id: None,
        account: Some(account.into()),
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        // The strategy's own fills name its mount; the stray on `demo-2` is another account's, named by none.
        mount_id: (account == "paper-1").then(|| MOUNT.to_string()),
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

/// When the forward test closed signal `k`'s round trip: its exit fill, 2 h 45 s after the backtest's entry
/// (signal 3, closed in two fills, ends a second later).
pub fn forward_exit(k: usize) -> i64 {
    signal(k).1 + 2 * HOUR + 45_000
}

/// The forecast signal `k`'s bar made in the backtest; the live one adds [`LIVE_FORECAST_SHIFT`] from signal 10.
pub fn forecast_value(k: usize) -> f64 {
    (k % 5) as f64 * 0.002 - 0.004
}

/// The live forecasts the materializer wrote: the mount's at every signal's decision time (the missed one too:
/// a missed order is no missed forecast), one after the window (left out), and another mount's (left out when the
/// mount is named).
fn live_forecasts() -> Vec<ExecForecastRow> {
    let row = |ts: i64, mount_id: &str, value: f64| ExecForecastRow {
        ts,
        mount_id: mount_id.to_string(),
        value,
        horizon_ms: HOUR,
        confidence: None,
    };
    let mut rows = Vec::new();
    for k in 0..SIGNALS {
        let at = signal(k).1;
        let shift = if k >= 10 { LIVE_FORECAST_SHIFT } else { 0.0 };
        rows.push(row(at, MOUNT, forecast_value(k) + shift));
        rows.push(row(at, OTHER_MOUNT, 5.0));
    }
    rows.push(row(START + DAYS * DAY + HOUR, MOUNT, 0.5));
    rows
}

/// The backtest's forecasts (`forecasts.json`): one at each signal's bar, and one before the window (left out).
fn saved_forecasts() -> Vec<vike_model::ForecastRow> {
    let row = |timestamp: i64, value: f64| vike_model::ForecastRow {
        timestamp,
        symbol: String::new(),
        forecast: vike_model::Forecast::new(value, HOUR, None).expect("a finite forecast"),
    };
    let mut rows = vec![row(START - HOUR, 0.5)];
    rows.extend((0..SIGNALS).map(|k| row(signal(k).1, forecast_value(k))));
    rows
}

/// What signal `k` cost the forward over the backtest, in basis points, at entry and at exit: the exit
/// cost doubles from signal 10 on (the fills drift).
pub fn slippage_bps(k: usize) -> (f64, f64) {
    (2.0 + (k % 3) as f64, if k < 10 { 3.0 } else { 6.0 })
}

/// The forward test's fills, in time order.
fn fills() -> Vec<ExecFillRow> {
    let mut rows = Vec::new();
    let mut push = |ts: i64, side: i32, qty: f64, price: f64, account: &str| {
        rows.push(fill(rows.len(), ts, side, qty, price, account));
    };
    // Opened before the window, closed inside it: no round trip of the window.
    push(START - 2 * HOUR, 1, 1.0, 99.0, "paper-1");
    push(START + HOUR / 6, -1, 1.0, 99.5, "paper-1");
    for k in 0..SIGNALS {
        let (is_long, at, price, favour) = signal(k);
        if k == MISSED {
            continue;
        }
        let side: i32 = if is_long { 1 } else { -1 };
        let s = f64::from(side);
        let exit = price * (1.0 + s * favour);
        let (entry_bps, exit_bps) = slippage_bps(k);
        push(at + 30_000, side, 1.0, price * (1.0 + s * entry_bps / 10_000.0), "paper-1");
        let exit_price = exit * (1.0 - s * exit_bps / 10_000.0);
        let exit_at = at + 2 * HOUR + 45_000;
        if k == 3 {
            push(exit_at, -side, 0.4, exit_price, "paper-1");
            push(exit_at + 1_000, -side, 0.6, exit_price, "paper-1");
        } else {
            push(exit_at, -side, 1.0, exit_price, "paper-1");
        }
        if k == 9 {
            push(EXTRA, 1, 1.0, 103.0, "paper-1");
            push(EXTRA + HOUR / 2, -1, 1.0, 102.5, "paper-1");
        }
        if k == 17 {
            let stray = at + 3 * HOUR;
            push(stray, 1, 1.0, 104.0, "demo-2");
            push(stray + HOUR / 2, -1, 1.0, 104.2, "demo-2");
        }
    }
    rows
}

/// The fills in the workspace's own store double, and the window.
pub fn store() -> Arc<dyn HistoryStore + Send + Sync> {
    Arc::new(memory_store())
}

/// [`store`] whose kept instrument list is `rows` instead of [`kept_row`] (the study's contract check reads the
/// symbol's row).
pub fn store_keeping(rows: Vec<vike_model::Instrument>) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = memory_store();
    store.serve_kept_instruments(rows);
    Arc::new(store)
}

/// The symbol's kept row: the spot pair, one linear unit a unit.
pub fn kept_row() -> vike_model::Instrument {
    vike_model::Instrument::new(VENUE, SYMBOL, "BTC", "USDT", vike_model::Contract::CryptoSpot)
}

fn memory_store() -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    store.serve_kept_instruments(vec![kept_row()]);
    let rows = fills();
    let n = store.append_exec_fills(VENUE, SYMBOL, &rows, Some("forward-fills")).expect("fills");
    assert_eq!(n, rows.len());
    let forecasts = live_forecasts();
    store
        .append_forecasts(VENUE, SYMBOL, &forecasts, Some("forward-forecasts"))
        .expect("forecasts");
    store
}

/// [`store`] plus two forward-only round trips on `paper-1` while the strategy is flat: one of [`OTHER_MOUNT`]
/// (signal 12's slot) and one hand-placed, naming no mount (signal 14's).
pub fn store_with_another_mount() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = store();
    let mut rows = Vec::new();
    for (n, (k, mount)) in [(12, Some(OTHER_MOUNT)), (14, None)].into_iter().enumerate() {
        let at = signal(k).1 + 3 * HOUR;
        for (i, (ts, side, price)) in
            [(at, 1, 104.0), (at + HOUR / 2, -1, 104.3)].into_iter().enumerate()
        {
            let mut row = fill(1_000 + 2 * n + i, ts, side, 1.0, price, "paper-1");
            row.mount_id = mount.map(str::to_string);
            rows.push(row);
        }
    }
    store.append_exec_fills(VENUE, SYMBOL, &rows, Some("other-fills")).expect("the other fills");
    store
}

/// The saved runs: `backtest` and its ledger, `no_ledger` without one, `ruined` whose one round trip loses
/// [`RUINED_LOSS`] (more than a small capital, so its equity reaches zero); any other id names no run.
pub struct Runs;

/// What `ruined`'s one round trip, a long in signal 0's slot that falls by half, loses before its fees.
pub const RUINED_LOSS: f64 = 50.0;

impl StudyRuns for Runs {
    fn forecasts(&self, run_id: &str) -> Result<Vec<vike_model::ForecastRow>, String> {
        match run_id {
            "backtest" => Ok(saved_forecasts()),
            "no_ledger" => Ok(Vec::new()),
            _ => Err(format!("no run {run_id:?} under the runs root")),
        }
    }

    fn trades(&self, run_id: &str) -> Result<Vec<Trade>, String> {
        match run_id {
            "backtest" => Ok(ledger()),
            "ruined" => {
                let at = signal(0).1;
                Ok(vec![round_trip(true, (at, 2.0 * RUINED_LOSS), at + 2 * HOUR, -0.5)])
            }
            "no_ledger" => Err(format!("run {run_id} kept no trade ledger (trades.json)")),
            _ => Err(format!("no run {run_id:?} under the runs root")),
        }
    }

    /// `forward`: the forward test of the five days on `paper-1`, held against `backtest`; `open`: the same mount
    /// still running, naming no backtest; `backtest` is no forward run.
    fn forward(&self, run_id: &str) -> Result<StudyForwardRun, String> {
        let window = window();
        let run = StudyForwardRun {
            mount_id: MOUNT.to_string(),
            venue: VENUE.to_string(),
            symbol: SYMBOL.to_string(),
            interval: "1m".to_string(),
            account: format!("{VENUE}/paper-1"),
            strategy_file: "trend/ema.rhai".to_string(),
            strategy_sha256: "ab".repeat(32),
            started_ms: START,
            ended_ms: window.end,
            reference_run: Some("backtest".to_string()),
        };
        match run_id {
            "forward" => Ok(run),
            "open" => Ok(StudyForwardRun { ended_ms: None, reference_run: None, ..run }),
            "backtest" | "no_ledger" => Err(format!("run {run_id} is not a `forward` run")),
            _ => Err(format!("no run {run_id:?} under the runs root")),
        }
    }
}

/// The context the study runs in: the fills and the kept row, the saved runs, the window, a scratch path, and the
/// catalog's valuation, as `crates/vike-study-runner/src/study_run.rs`'s `run_study_with` hands every study.
pub fn context(scratch: PathBuf) -> StudyContext {
    valued(StudyContext::new(store(), window(), scratch).with_runs(Arc::new(Runs)))
}

/// `ctx` valuing its kept contracts with the catalog's answer, as a host does.
pub fn valued(ctx: StudyContext) -> StudyContext {
    ctx.with_contract_valuation(vike_catalog::kept_contract_value)
}
