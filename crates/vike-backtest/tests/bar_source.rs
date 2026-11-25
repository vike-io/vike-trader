//! `[data] file` and `[data] source` end to end through `run_backtest` (decision 0268).
//!
//! The file read's claim is that it changes WHERE the bars come from and nothing else: a run over a CSV
//! file gives exactly the result of the same bars held in a store. The lane read's claim is the same
//! against a lane of a real `HistoryStoreDataFusion`, beside candles that differ, plus the coverage gate judging
//! the lane rather than the candles. Both runs say on the report where their bars came from. A synthetic
//! path held by `HistoryStoreSynthetic` makes the same claim, under its own venue only (book W14).

use std::sync::Arc;

use vike_analytics::TradingRecord;
use vike_analytics::synthetic::{
    SyntheticGrid, SyntheticModel, learned_bars, model_bars, next_step_rows,
};
use vike_backtest::run::report::realism_stamp;
use vike_backtest::run::{profile::ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreMemory, HistoryStoreSynthetic, SYNTHETIC_VENUE};
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";
const STEP_MS: i64 = 3_600_000;

/// 2026-01-01T00:00:00Z.
fn t0() -> i64 {
    vike_utility::calendar::days_from_civil(2026, 1, 1) * 86_400_000
}

/// `n` hourly bars from 2026-01-01 whose prices wander (`shift` moves them all), one hour missing, so the
/// curve has something in it a different tape would move.
fn bars(n: i64, shift: f64) -> Vec<Bar> {
    (0..n)
        .filter(|i| *i != 7)
        .map(|i| {
            let close = 100.0 + shift + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.05;
            let open = close - 0.25;
            Bar::ohlcv(t0() + i * STEP_MS, open, close + 1.0, open - 1.0, close, 10.0 + i as f64)
        })
        .collect()
}

/// The profile every run here starts from, over every bar, with `data` added to its `[data]` table.
fn profile(data: &str) -> ProfileBacktest {
    profile_in("0", "9999999999999", data)
}

/// [`profile`] over the window `from..=to`.
fn profile_in(from: &str, to: &str, data: &str) -> ProfileBacktest {
    profile_on(VENUE, from, to, data)
}

/// [`profile_in`] reading `venue`'s series.
fn profile_on(venue: &str, from: &str, to: &str, data: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
name = "bar-source"

[data]
venue = "{venue}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1h"
from = "{from}"
to = "{to}"
{data}

[engine]
cash = 10000.0

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "{SYMBOL}"
commission = 0.1
slippage = 0.05
"#
    ))
    .expect("fixture profile parses")
    .with_stand_in(vike_strategy_lab::stand_in)
}

fn memory_store(bars: &[Bar]) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    store.append_bars(VENUE, SYMBOL, "1h", bars, None).unwrap();
    Arc::new(store)
}

/// `x` as Excel with Russian settings writes it: a space between thousands, a comma decimal mark.
fn ru_number(x: f64) -> String {
    let s = format!("{x}");
    let (int, frac) = s.split_once('.').map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
    let digits: Vec<char> = int.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(*c);
    }
    match frac {
        Some(f) => format!("{grouped},{f}"),
        None => grouped,
    }
}

/// An epoch-ms bar time as Moscow's wall clock (UTC+3), `DD.MM.YYYY HH:mm`.
fn moscow_time(ts: i64) -> String {
    let wall = ts / 1_000 + 3 * 3_600;
    let (y, m, d) = vike_utility::calendar::civil_from_days(wall.div_euclid(86_400));
    let secs = wall.rem_euclid(86_400);
    format!("{d:02}.{m:02}.{y} {:02}:{:02}", secs / 3_600, secs % 3_600 / 60)
}

/// The run without the report line naming the source, which is the one thing that differs.
fn without_source_line(mut r: TradingRecord) -> String {
    r.data_warnings.retain(|l| !l.starts_with("bars read from"));
    format!("{r:?}")
}

#[test]
fn a_backtest_over_a_csv_file_equals_the_same_bars_in_a_store() {
    let held = bars(120, 0.0);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("btc.csv");
    let mut text = String::from("time,open,high,low,close,volume\n");
    for b in &held {
        text.push_str(&format!(
            "{},{},{},{},{},{}\n",
            b.timestamp, b.open, b.high, b.low, b.close, b.volume
        ));
    }
    std::fs::write(&path, text).unwrap();

    let from_file = profile(&format!("file = {:?}", path.display().to_string()));
    let file_run =
        run_backtest(&from_file, Arc::new(HistoryStoreMemory::new())).expect("the file run");
    let store_run = run_backtest(&profile(""), memory_store(&held)).expect("the store run");

    assert!(file_run.equity_curve.len() > 100 && file_run.final_equity != 10_000.0, "{file_run:?}");
    assert_eq!(without_source_line(file_run.clone()), without_source_line(store_run.clone()));
    let line = format!("bars read from the file {} (data.file)", path.display());
    assert!(
        file_run.data_warnings.iter().any(|l| l.starts_with(&line)),
        "{:?}",
        file_run.data_warnings
    );
    assert!(store_run.data_warnings.iter().all(|l| !l.starts_with("bars read from")));

    let stamp = realism_stamp(&from_file);
    assert_eq!(stamp.get("data.source"), Some(format!("file:{}", path.display()).as_str()));
    assert_eq!(
        realism_stamp(&profile("")).get("data.source"),
        None,
        "the default stamps nothing new"
    );
}

/// A synthetic path (`vike_analytics::synthetic`) runs through the harness from `HistoryStoreSynthetic`
/// exactly as the same bars run from a store, and a profile naming another venue is refused rather than
/// handed a generated path as that venue's tape.
#[test]
fn a_backtest_over_a_synthetic_path_equals_the_same_bars_in_a_store() {
    let grid = SyntheticGrid { start_ms: t0(), step_ms: STEP_MS, bars: 120 };
    let model = SyntheticModel::GeometricBrownianMotion { drift: 0.0005, volatility: 0.01 };
    let held = model_bars(&model, &grid, 100.0, 10.0, 16, 7).expect("a synthetic path");
    let synthetic = HistoryStoreSynthetic::default()
        .with_series(SYMBOL, "1h", held.clone())
        .expect("the path is on its grid");
    let memory = HistoryStoreMemory::new();
    memory.append_bars(SYNTHETIC_VENUE, SYMBOL, "1h", &held, None).unwrap();

    let on_synthetic = profile_on(SYNTHETIC_VENUE, "0", "9999999999999", "");
    let synthetic_run =
        run_backtest(&on_synthetic, Arc::new(synthetic.clone())).expect("the synthetic run");
    let store_run = run_backtest(&on_synthetic, Arc::new(memory)).expect("the store run");
    assert!(
        synthetic_run.equity_curve.len() > 100 && synthetic_run.final_equity != 10_000.0,
        "{synthetic_run:?}"
    );
    assert_eq!(format!("{synthetic_run:?}"), format!("{store_run:?}"));

    let why = run_backtest(&profile(""), Arc::new(synthetic)).expect_err("another venue");
    assert!(format!("{why:?}").contains("a synthetic store holds generated data"), "{why:?}");
}

/// A LEARNED path (`vike_analytics::synthetic::learned_bars`) runs through the harness from
/// `HistoryStoreSynthetic` like any generated path. The next-step model is a closure here, where a
/// study passes the network it fitted (`|row| model.predict_one(row)`): an AR(1) of the scaled returns
/// whose shock is the row's draw, its last column.
#[test]
fn a_backtest_runs_over_a_learned_path() {
    let grid = SyntheticGrid { start_ms: t0(), step_ms: STEP_MS, bars: 120 };
    let market = SyntheticModel::GeometricBrownianMotion { drift: 0.0, volatility: 0.01 };
    let source = model_bars(&market, &grid, 100.0, 10.0, 1, 3).expect("a source");
    let rows = next_step_rows(&source, 4).expect("the source's rows");
    let held = learned_bars(|row| Ok(0.3 * row[3] + row[4]), &rows.frame, &grid, 100.0, 10.0, 4, 7)
        .expect("a learned path");
    let synthetic = HistoryStoreSynthetic::default()
        .with_series(SYMBOL, "1h", held)
        .expect("the path is on its grid");
    let on_synthetic = profile_on(SYNTHETIC_VENUE, "0", "9999999999999", "");
    let run = run_backtest(&on_synthetic, Arc::new(synthetic)).expect("the learned run");
    assert!(run.equity_curve.len() > 100 && run.final_equity != 10_000.0, "{run:?}");
}

/// **A file saved by Excel with Russian settings** reads, through `data.file_format`, exactly as the same
/// bars from a store; without the format it is refused, naming the option to set.
#[test]
fn a_regional_csv_reads_through_its_file_format_and_is_refused_without_it() {
    let held = bars(60, 1_000.0);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("btc-ru.csv");
    let mut text = String::from("time;open;high;low;close;volume\r\n");
    for b in &held {
        let cells = [b.open, b.high, b.low, b.close, b.volume].map(ru_number).join(";");
        text.push_str(&format!("{};{cells}\r\n", moscow_time(b.timestamp)));
    }
    assert!(text.contains(' ') && text.contains(','), "the fixture is regional: {text}");
    std::fs::write(&path, &text).unwrap();
    let file = format!("file = {:?}", path.display().to_string());

    let format = "file_format = { delimiter = \";\", decimal = \",\", thousands = \"space\", \
                  time = \"DD.MM.YYYY HH:mm\", tz = \"Europe/Moscow\" }";
    let run =
        run_backtest(&profile(&format!("{file}\n{format}")), Arc::new(HistoryStoreMemory::new()))
            .expect("the regional file run");
    let store_run = run_backtest(&profile(""), memory_store(&held)).expect("the store run");
    assert_eq!(without_source_line(run), without_source_line(store_run));

    let why =
        run_backtest(&profile(&file), Arc::new(HistoryStoreMemory::new())).unwrap_err().to_string();
    assert!(
        why.contains("line 1: format") && why.contains("set the delimiter option to `;`"),
        "{why}"
    );
}

/// A relative `data.file` is the profile's directory's, and a file whose step is not the profile's
/// interval, or that holds no bar in the window, runs nothing.
#[test]
fn a_file_run_resolves_beside_the_profile_and_refuses_what_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::from("time,open,high,low,close,volume\n");
    for b in bars(30, 0.0) {
        text.push_str(&format!(
            "{},{},{},{},{},{}\n",
            b.timestamp, b.open, b.high, b.low, b.close, b.volume
        ));
    }
    std::fs::write(dir.path().join("btc.csv"), &text).unwrap();
    let toml = |data: &str| {
        let p = dir.path().join("profile.toml");
        let mut body = String::new();
        body.push_str(&format!(
            "[data]\nvenue = \"{VENUE}\"\nsymbols = [\"{SYMBOL}\"]\nkind = \"bar\"\n{data}\n\
             [engine]\ncash = 10000.0\n[strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\n\
             symbol = \"{SYMBOL}\"\n"
        ));
        std::fs::write(&p, body).unwrap();
        ProfileBacktest::from_path(&p)
            .expect("profile file parses")
            .with_stand_in(vike_strategy_lab::stand_in)
    };
    let empty = || -> Arc<dyn HistoryStore + Send + Sync> { Arc::new(HistoryStoreMemory::new()) };

    let ok = toml("interval = \"1h\"\nfrom = \"0\"\nto = \"9999999999999\"\nfile = \"btc.csv\"");
    let ran = run_backtest(&ok, empty()).expect("the relative file is the profile's");
    assert!(ran.equity_curve.len() >= 29, "every bar of the file: {}", ran.equity_curve.len());

    let wrong_step =
        toml("interval = \"4h\"\nfrom = \"0\"\nto = \"9999999999999\"\nfile = \"btc.csv\"");
    let why = run_backtest(&wrong_step, empty()).unwrap_err().to_string();
    assert!(why.contains("3600000 ms apart") && why.contains("4h"), "{why}");

    let before = toml("interval = \"1h\"\nfrom = \"0\"\nto = \"1000\"\nfile = \"btc.csv\"");
    let why = run_backtest(&before, empty()).unwrap_err().to_string();
    assert!(why.contains("holds no 1h bars") && why.contains("btc.csv"), "{why}");

    let gone = toml("interval = \"1h\"\nfrom = \"0\"\nto = \"9999999999999\"\nfile = \"nope.csv\"");
    assert!(run_backtest(&gone, empty()).unwrap_err().to_string().contains("nope.csv"));
}

/// `data.source` reads the lane and never the candles beside it; the coverage gate judges the lane; a
/// lane holding nothing in the window runs nothing.
#[test]
fn a_lane_run_reads_the_lane_and_the_gate_judges_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let (candles, theirs) = (bars(80, 0.0), bars(80, 40.0));
    store.append_bars(VENUE, SYMBOL, "1h", &candles, Some("candles")).unwrap();
    store
        .append_bars_sourced(VENUE, "user-my_file", SYMBOL, "1h", &theirs, Some("theirs"))
        .unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let lane = profile("source = \"user-my_file\"");
    let lane_run = run_backtest(&lane, store.clone()).expect("the lane run");
    let same_bars =
        run_backtest(&profile(""), memory_store(&theirs)).expect("the lane's bars in a store");
    let candle_run = run_backtest(&profile(""), store.clone()).expect("the candles' run");
    assert_eq!(without_source_line(lane_run.clone()), without_source_line(same_bars));
    assert_ne!(lane_run.final_equity, candle_run.final_equity, "the candles are another tape");
    assert!(
        lane_run.data_warnings.iter().any(|l| l.contains("the lane `user-my_file` (data.source)")),
        "{:?}",
        lane_run.data_warnings
    );
    assert_eq!(realism_stamp(&lane).get("data.source"), Some("user-my_file"));
    let planned = vike_backtest::run::data_plan::planned_series_ids(&lane, &[]);
    assert_eq!(planned[0].source.as_deref(), Some("user-my_file"), "the run addresses the lane");

    // The gate finds the lane (its coverage is the series', not the candles' inventory row)...
    let (from, to) = (t0().to_string(), (t0() + 79 * STEP_MS).to_string());
    let gated = profile_in(&from, &to, "source = \"user-my_file\"\nrequire_coverage = true");
    run_backtest(&gated, store.clone()).expect("the lane covers its own window");
    // ...and refuses a lane the store does not hold, by its name.
    let absent = profile("source = \"user-other\"\nrequire_coverage = true");
    let why = run_backtest(&absent, store.clone()).unwrap_err().to_string();
    assert!(why.contains("data.require_coverage is armed"), "{why}");
    let ungated = profile("source = \"aggtrades\"");
    let why = run_backtest(&ungated, store).unwrap_err().to_string();
    assert!(why.contains("the lane `aggtrades` holds no 1h bars"), "{why}");
}
