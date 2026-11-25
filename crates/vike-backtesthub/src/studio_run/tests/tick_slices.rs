//! Tick slices through the run pipeline: replayable ticks, an empty tick slice, a tick sweep. The
//! store catalog listings these slices are picked from are tested beside their code,
//! `crates/vike-studio-core/src/run/slice_tests.rs`.

use std::assert_matches;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::{L2BookUpdate, TickQuote, TickTrade};
use vike_model::params::ParamValue;
use vike_store::HistoryStoreDataFusion;
use vike_studio_core::test_support::cross_script::CROSS_SCRIPT;
use vike_studio_core::test_support::sweep_script::SWEEP_SCRIPT;
use vike_studio_core::{DataSlice, RunError, StrategySpec, params_from_rows};

use super::super::{run_paramscan_slice, run_slice, run_walkforward_slice};
use super::support::rhai;

// ---- tick slices -------------------------------------------------------------------------

fn tick_store() -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let quotes: Vec<TickQuote> = (0..200)
        .map(|i| TickQuote {
            timestamp: 1_000 * (i as i64 + 1),
            local_timestamp: 1_000 * (i as i64 + 1),
            bid: 100.0 + (i % 5) as f64,
            ask: 100.5 + (i % 5) as f64,
            bid_size: 5.0,
            ask_size: 5.0,
            symbol: "TKN".into(),
        })
        .collect();
    let trades: Vec<TickTrade> = (0..200)
        .map(|i| TickTrade {
            timestamp: 1_000 * (i as i64 + 1),
            local_timestamp: 1_000 * (i as i64 + 1),
            price: 100.25 + (i % 5) as f64,
            size: 1.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".into(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
    store.append_trades("polymarket", "TKN", &trades, None).unwrap();
    (dir, Arc::new(store) as Arc<dyn HistoryStore + Send + Sync>)
}

/// A `ReplayKind::Tick` slice routes to `replay_ticks` (NOT `load_bars`): the store below has
/// no bar series at all, so a bar-path run could not possibly produce this result.
#[test]
fn tick_slice_replays_recorded_ticks() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TimestampRange::all());
    let spec = StrategySpec::file(
        "trend/buy_hold.rs",
        params_from_rows(&[("symbol".into(), "TKN".into())]),
    );
    let res = run_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        None,
    )
    .expect("tick replay should run");
    assert!(!res.equity_curve.is_empty(), "the tick replay stamps an equity curve");
}

#[test]
fn tick_slice_with_no_recorded_ticks_is_a_data_error() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["MISSING".to_string()], TimestampRange::all());
    let out = run_slice(
        &rhai("fn on_bar() {}"),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        None,
    );
    assert_matches!(out, Err(RunError::Data(_)), "got {out:?}");
}

/// The real tape, except that the FIRST `scan_quotes` of the store's life fails: one grid point of a tick
/// sweep cannot read, whichever point the pool runs first. A sweep reads its tape once and a failed read
/// keeps nothing (`vike_backtest::run::tick_replay`'s prepared tapes), so the next point reads again.
struct OneFailedScan {
    tape: Arc<dyn HistoryStore + Send + Sync>,
    scans: AtomicUsize,
}

impl HistoryStore for OneFailedScan {
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        if self.scans.fetch_add(1, Ordering::Relaxed) == 0 {
            return Err(HistoryStoreError::Io("the datahub went away".to_string()));
        }
        self.tape.scan_quotes(venue, symbol, range)
    }
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.tape.scan_trades(venue, symbol, range)
    }
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.tape.scan_book_updates(venue, symbol, range)
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

/// A point that could not READ fails the sweep. Dropped, it leaves the winner, the DSR and the PBO
/// computed over fewer trials than the grid named, in a result that carries no count.
#[test]
fn a_tick_sweep_fails_when_one_point_cannot_read() {
    let (_dir, tape) = tick_store();
    let failing = Arc::new(OneFailedScan { tape, scans: AtomicUsize::new(0) });
    let store: Arc<dyn HistoryStore + Send + Sync> = failing.clone();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TimestampRange::all());
    let spec = StrategySpec::file(
        "trend/buy_hold.rs",
        params_from_rows(&[("symbol".into(), "TKN".into())]),
    );
    let grid = vec![(
        "size".to_string(),
        vec![ParamValue::Float(1.0), ParamValue::Float(2.0), ParamValue::Float(3.0)],
    )];
    let err = run_paramscan_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        &grid,
    )
    .map(|sw| sw.entries.len());
    assert_matches!(&err, Err(RunError::Data(why)) if why.contains("went away"), "got {err:?}");
    // ONE point failed: its read failed, the next point read the tape again and kept it, and the third
    // borrowed it. Every point failing would read three times.
    assert_eq!(failing.scans.load(Ordering::Relaxed), 2, "one failed read, one good, one borrow");
}

/// A Rhai `param()` takes a number, and a text axis value is not one: the profile's `"rhai"` arm
/// leaves it out of the script's bindings (`Params::numeric`), so that point runs at the script's
/// own default, as the same grid on a `vike-cli backtest run` profile does.
#[test]
fn a_rhai_sweep_leaves_a_text_axis_value_out_as_the_profile_run_does() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TimestampRange::all());
    let grid = vec![(
        "fast".to_string(),
        vec![ParamValue::Float(3.0), ParamValue::Str("not a number".to_string())],
    )];
    let sw = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        &grid,
    )
    .unwrap();
    assert_eq!(sw.entries.len(), 2, "both points ran; the text one at the script's default");
}

#[test]
fn a_bar_only_runner_rejects_a_tick_slice_instead_of_panicking() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TimestampRange::all());
    assert_matches!(
        run_walkforward_slice(
            &rhai(CROSS_SCRIPT),
            &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
            &slice,
            &store,
            4
        ),
        Err(RunError::Data(_))
    );
}
