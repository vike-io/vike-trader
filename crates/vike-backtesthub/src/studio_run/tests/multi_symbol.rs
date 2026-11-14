//! Multi-symbol bar slices: every symbol registers, a misaligned pick is a data error.

use std::assert_matches;
use std::sync::Arc;
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

use super::support::bar_at;
use crate::studio_run::run_slice;
use vike_studio_core::{DataSlice, RunError, StrategySpec};

/// The two-symbol rotation every test below runs.
fn rotation() -> StrategySpec {
    StrategySpec::file("portfolio/rotation_top_k.rs", vike_studio_core::empty_params())
}

// ---- multi-symbol ------------------------------------------------------------------------

#[test]
fn multi_symbol_bar_slice_registers_every_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let mk = |symbol_name: &str, base: f64| -> Vec<Bar> {
        (0..50)
            .map(|i| {
                let c = base + (i % 7) as f64;
                Bar {
                    symbol: Some(symbol_name.to_string()),
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    ..bar_at(60_000 * (i as i64 + 1))
                }
            })
            .collect()
    };
    store.append_bars("binance", "BTCUSDT", "1m", &mk("BTCUSDT", 100.0), None).unwrap();
    store.append_bars("binance", "ETHUSDT", "1m", &mk("ETHUSDT", 50.0), None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        "1m",
        TimestampRange::all(),
    );
    let res = run_slice(
        &rotation(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &store,
        None,
    )
    .expect("a two-symbol run backtests");
    assert!(!res.equity_curve.is_empty());
    let mut symbols: Vec<&str> = res.per_symbol_pnl.iter().map(|(s, _)| s.as_str()).collect();
    symbols.sort_unstable();
    assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"], "both symbols registered");
}

/// Misaligned per-symbol bar counts are `StrategyEngine::new`'s own assert — surfaced as a
/// `RunError` here rather than panicking the worker thread.
#[test]
fn misaligned_multi_symbol_slice_is_a_data_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars_a: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    let bars_b: Vec<Bar> = (0..5).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    store.append_bars("binance", "AAA", "1m", &bars_a, None).unwrap();
    store.append_bars("binance", "BBB", "1m", &bars_b, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["AAA".to_string(), "BBB".to_string()],
        "1m",
        TimestampRange::all(),
    );
    assert_matches!(
        run_slice(
            &rotation(),
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &slice,
            &store,
            None
        ),
        Err(RunError::Data(_))
    );
}

/// The simulator books one dollar ledger and holds no rate series (decision 0183, item 13): a pick
/// whose symbol name says another fiat quote is a red banner naming it, never a run that books yen
/// as dollars. A dollar-quoted pick loads as before.
#[test]
fn a_foreign_quoted_pick_is_a_data_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    store.append_bars("oanda", "USDJPY", "1m", &bars, None).unwrap();
    store.append_bars("oanda", "EURUSD", "1m", &bars, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let slice = DataSlice::bars("oanda", "USDJPY", "1m", TimestampRange::all());
    let err = run_slice(
        &rotation(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &store,
        None,
    )
    .expect_err("a yen pick is refused");
    assert_matches!(&err, RunError::Data(m) if m.contains("USDJPY (JPY)"));
    // ⚠ A picker has no ledger field, so the sentence must not send the user to one it cannot set:
    // it says so and points at a profile run instead.
    assert_matches!(
        &err,
        RunError::Data(m) if m.contains("no ledger field")
            && m.contains("--profile")
            && !m.contains("leave engine.account_currency out")
    );
    // The tick door says the same words (it used to print the profile door's).
    let ticks = DataSlice::ticks("oanda", vec!["USDJPY".to_string()], TimestampRange::all());
    let tick_run = run_slice(
        &rotation(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &ticks,
        &store,
        None,
    );
    assert_matches!(&tick_run, Err(RunError::Data(m)) if m.contains("no ledger field"));

    let fine = DataSlice::bars("oanda", "EURUSD", "1m", TimestampRange::all());
    assert!(
        run_slice(
            &rotation(),
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &fine,
            &store,
            None
        )
        .is_ok(),
        "a dollar-quoted pick runs"
    );
}

/// The simulator cannot book a coin-margined contract: a pick of one is a red banner naming it on both
/// the bar door and the tick door, never a run that books it as linear. The linear book of the same
/// coin loads as before.
#[test]
fn an_inverse_pick_is_a_data_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    store.append_bars("deribit", "BTC-PERPETUAL", "1m", &bars, None).unwrap();
    store.append_bars("deribit", "BTC_USDC-PERPETUAL", "1m", &bars, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let slice = DataSlice::bars("deribit", "BTC-PERPETUAL", "1m", TimestampRange::all());
    let err = run_slice(
        &rotation(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &store,
        None,
    )
    .expect_err("an inverse pick is refused");
    assert_matches!(&err, RunError::Data(m) if m.contains("BTC-PERPETUAL on deribit"));
    let ticks =
        DataSlice::ticks("deribit", vec!["BTC-PERPETUAL".to_string()], TimestampRange::all());
    let tick_run = run_slice(
        &rotation(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &ticks,
        &store,
        None,
    );
    assert_matches!(&tick_run, Err(RunError::Data(m)) if m.contains("BTC-PERPETUAL on deribit"));

    let fine = DataSlice::bars("deribit", "BTC_USDC-PERPETUAL", "1m", TimestampRange::all());
    assert!(
        run_slice(
            &rotation(),
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &fine,
            &store,
            None
        )
        .is_ok(),
        "a linear pick runs"
    );
}

#[test]
fn slice_label_and_symbol_accessor() {
    let s = DataSlice::bars("binance", "BTCUSDT", "1m", TimestampRange::all());
    assert_eq!(s.symbol(), "BTCUSDT");
    assert_eq!(s.label(), "binance · BTCUSDT · 1m");
    let t = DataSlice::ticks("polymarket", vec!["A".into(), "B".into()], TimestampRange::all());
    assert_eq!(t.label(), "polymarket · A+1 · ticks");
}
