//! A strategy's failures reach the run's record and its report (`vike_model::Strategy::failures`, read by
//! the engine after `on_stop`): a `.rhai` script whose `on_bar` errors on every bar is disabled after 10 in
//! a row, and its report says so in a typed block; a clean run's report carries no block at all.

use std::sync::Arc;

use vike_analytics::results::TradingResults;
use vike_data::HistoryStoreMemory;
use vike_marketdata::Bar;
use vike_rhai::RhaiStrategy;
use vike_simulator::OrderBrokerSimulator;

use super::*;
use crate::run::strategy_builder::StrategyBuilder;

/// A UTC midnight.
const S: i64 = 19_675 * 86_400_000;
const MINUTE: i64 = 60_000;

/// Twelve flat one-minute bars of BTCUSDT.
fn store() -> Arc<HistoryStoreMemory> {
    let s = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..12)
        .map(|i| Bar {
            timestamp: S + i * MINUTE,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect();
    s.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    Arc::new(s)
}

/// The twelve-minute run of the Rhai script `src`, its record and its report.
fn run(src: &'static str) -> (TradingRecord, TradingResults) {
    let toml = format!(
        "[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n[engine]\ncash = 10000.0\n\n[strategy]\nfile = \"other/x.rhai\"\n",
        S + 12 * MINUTE - 1
    );
    let mut p = ProfileBacktest::from_toml_str(&toml).unwrap_or_else(|e| panic!("{e}\n{toml}"));
    p.strategy.as_mut().unwrap().builder = Some(StrategyBuilder::new(move |_| {
        Ok(Box::new(RhaiStrategy::<OrderBrokerSimulator>::compile(src).expect("compiles")))
    }));
    let record = run_backtest(&p, store()).expect("the run runs");
    let report = TradingResults::from_record(None, &record, 252.0);
    (record, report)
}

#[test]
fn a_script_disabled_after_ten_errors_says_so_in_its_report() {
    let (record, report) = run("fn on_bar() { throw \"bad bar\"; }");
    let failures = &record.strategy_failures;
    assert_eq!(
        (failures.count, failures.first_at, failures.disabled, failures.messages.len()),
        (10, Some(S), true, 1),
        "{failures:?}"
    );
    assert_eq!(&report.strategy_failures, failures, "the report copies the record's");
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    assert_eq!(json["strategy_failures"]["disabled"], serde_json::Value::Bool(true));
    assert_eq!(json["strategy_failures"]["count"], serde_json::json!(10));
}

#[test]
fn a_clean_script_reports_no_failures() {
    let (record, report) = run("fn on_bar() { let x = close(); }");
    assert!(record.strategy_failures.is_empty(), "{:?}", record.strategy_failures);
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    assert!(json.get("strategy_failures").is_none(), "a clean report keeps its bytes: {json}");
}
