//! Store-backed `run_backtest` tests over a concrete `HistoryStoreDataFusion` fixture.

use super::*;

// The concrete store the tests build; the non-test code drives `run_backtest` through the
// `Arc<dyn HistoryStore>` seam, so this import belongs to the tests alone.
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_model::SymbolProperties;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// `strategy.params.symbol` is set explicitly (rather than relying on `BuyHold`'s `bar.symbol`
/// fallback): bar-mode always sets `StrategyEngineConfig.default_venue`, which re-tags each bar's
/// `symbol` field as `"SYMBOL.VENUE"` (`format_instrument`) for filter-grid lookups —a
/// different string from the bare symbol key `OrderBrokerSimulator` indexes positions by (see
/// `tests/filters_fills.rs`'s `OpenClose` doc comment). An explicit `symbol` param sidesteps
/// that mismatch identically to how a real strategy would.
fn bar_profile(kind: &str, extra_engine: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0
{extra_engine}

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "{SYMBOL}"
"#
    )
}

#[test]
fn bar_mode_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let profile = ProfileBacktest::from_toml_str(&bar_profile("bar", ""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&profile, store).unwrap();

    // BuyHold opens a 1.0-unit position on the first bar and holds — no CLOSED trade, but
    // final_equity tracks the mark against the last bar's close (100.0 cash spent -> equity
    // re-marked at 102.0), so equity has moved off the starting cash.
    assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
}

#[test]
fn bar_mode_no_symbol_param_runs_without_panic() {
    // The footgun fix: a plain bar-mode profile (no `snap_to_properties`) must NOT set
    // `default_venue`, so bars keep their BARE symbol and `buy_hold` routes on `bar.symbol`
    // with no magic `strategy.params.symbol`. Before the fix this panicked ("unknown symbol").
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let toml = format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
"#
    );
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&profile, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "buy_hold ran and opened a position (no symbol param)");
}

#[test]
fn tick_mode_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
    store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

    let profile = ProfileBacktest::from_toml_str(&bar_profile("tick", ""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&profile, store).unwrap();

    assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
}

#[test]
fn tick_snap_to_properties_gates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
    store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
    store
        .append_symbol_properties(
            VENUE,
            SYMBOL,
            &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
            None,
        )
        .unwrap();

    // snap ON: the opening buy (size 1.0) is below the recorded min_qty -> gated -> no fill.
    let profile_on =
        ProfileBacktest::from_toml_str(&bar_profile("tick", "snap_to_properties = true"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let result_on = run_backtest(&profile_on, store.clone()).unwrap();
    assert_eq!(result_on.final_equity, 1000.0, "snap-on must gate the sub-min_qty opening buy");

    // snap OFF (default): raw replay, the buy fills.
    let profile_off =
        ProfileBacktest::from_toml_str(&bar_profile("tick", "snap_to_properties = false"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let result_off = run_backtest(&profile_off, store).unwrap();
    assert_ne!(result_off.final_equity, 1000.0, "snap-off must fill the opening buy");
}

#[test]
fn bar_snap_to_properties_gates() {
    // Bar-mode snapping is a DISTINCT path from tick: `run_backtest` sets `params.properties`
    // directly (bars bypass replay_ticks) AND sets `default_venue` (needed as the properties
    // venue key), so `bar_profile` supplies the explicit `strategy.params.symbol`.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();
    store
        .append_symbol_properties(
            VENUE,
            SYMBOL,
            &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
            None,
        )
        .unwrap();

    // snap ON: the opening buy (size 1.0) is below the recorded min_qty → gated → no fill.
    let profile_on =
        ProfileBacktest::from_toml_str(&bar_profile("bar", "snap_to_properties = true"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let result_on = run_backtest(&profile_on, store.clone()).unwrap();
    assert_eq!(result_on.final_equity, 1000.0, "bar snap-on must gate the sub-min_qty open");

    // snap OFF: raw fill.
    let profile_off =
        ProfileBacktest::from_toml_str(&bar_profile("bar", "snap_to_properties = false"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let result_off = run_backtest(&profile_off, store).unwrap();
    assert_ne!(result_off.final_equity, 1000.0, "bar snap-off must fill the open");
}

/// Bars and quotes for any range, where ONE `properties_as_of` of the store's life fails and every
/// other truly finds no record. One failed lookup among many is the hard case: the run goes on, every
/// other fill looks normal, and only this store knows one was never checked.
struct OnePropertiesReadFails {
    reads: std::sync::atomic::AtomicUsize,
    failing: usize,
}

impl OnePropertiesReadFails {
    /// The first read of the store's life fails: a route's pre-flight, where there is one.
    fn new() -> Arc<Self> {
        Arc::new(Self { reads: std::sync::atomic::AtomicUsize::new(0), failing: 0 })
    }

    /// The read after the pre-flight's fails, so the failure lands where a fill or a window reads the
    /// grid. The pre-flight (`missing_grid_lines`) asks once for a series with no record, and a failure
    /// there would refuse the run before anything these tests are about had a chance to read.
    fn past_the_preflight() -> Arc<Self> {
        Arc::new(Self { reads: std::sync::atomic::AtomicUsize::new(0), failing: 1 })
    }
}

impl HistoryStore for OnePropertiesReadFails {
    fn load_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: vike_data::TimestampRange,
    ) -> Result<Vec<Bar>, vike_data::HistoryStoreError> {
        Ok((0..20).map(|i| flat_bar_zero_volume(i * 1000, 100.0 + i as f64)).collect())
    }
    fn scan_quotes(
        &self,
        _: &str,
        _: &str,
        _: vike_data::TimestampRange,
    ) -> Result<Vec<vike_marketdata::TickQuote>, vike_data::HistoryStoreError> {
        Ok((0..20)
            .map(|i| vike_marketdata::TickQuote {
                symbol: SYMBOL.into(),
                ..quote(i * 1000, 10.0, 11.0)
            })
            .collect())
    }
    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: vike_data::TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, vike_data::HistoryStoreError> {
        if self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == self.failing {
            return Err(vike_data::HistoryStoreError::Io("the datahub went away".to_string()));
        }
        Ok(Vec::new())
    }
    vike_data::hist_store_stubs!(inert: writes, scan_trades, scan_book_updates, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

fn assert_refused_for_the_failed_read(outcome: Result<TradingRecord, HarnessError>) {
    match outcome {
        Err(HarnessError::Data(why)) => {
            assert!(why.contains("went away"), "the cause is named: {why}")
        }
        Err(other) => panic!("a Data error, got {other}"),
        Ok(r) => panic!("a clean result with an unchecked fill: final equity {}", r.final_equity),
    }
}

/// A failed `properties_as_of` under `snap_to_properties` read as "no record": that fill skipped the
/// grid and the minimum size checks, and the run reported it with exit 0. Both lanes, both doors.
#[test]
fn a_failed_properties_read_fails_the_run() {
    for kind in ["bar", "tick"] {
        let toml = bar_profile(kind, "snap_to_properties = true");
        let profile = ProfileBacktest::from_toml_str(&toml)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
        assert_refused_for_the_failed_read(run_backtest(
            &profile,
            OnePropertiesReadFails::past_the_preflight(),
        ));
    }

    let toml = bar_profile("bar", "snap_to_properties = true");
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let store: Arc<dyn HistoryStore + Send + Sync> = OnePropertiesReadFails::new();
    let (bars, funding) = load_profile_bars(&profile, &store, None, &mut Vec::new()).unwrap();
    let outcome = run_backtest_over_bars(&profile, &store, bars, &funding, &[], &Arc::default());
    assert_refused_for_the_failed_read(outcome);
}

/// The lookup at RUN START is the one that arms the venue hold for the whole run
/// (`OrderBrokerSimulator::taker_hold_table_ns`), so failing there zeroed every order's hold.
#[test]
fn a_failed_properties_read_at_run_start_fails_the_run() {
    let toml = bar_profile("tick", "snap_to_properties = true\norder_latency_ms = 1");
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert_refused_for_the_failed_read(run_backtest(
        &profile,
        OnePropertiesReadFails::past_the_preflight(),
    ));
}

/// The pre-flight's own read: a store that will not say whether it holds a grid refuses the run before
/// anything ran, on the words that say so.
#[test]
fn a_failed_properties_read_in_the_preflight_refuses_the_run() {
    let toml = bar_profile("bar", "snap_to_properties = true");
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    match run_backtest(&profile, OnePropertiesReadFails::new()) {
        Err(HarnessError::Data(why)) => {
            assert!(why.contains("went away") && why.contains("Nothing ran"), "{why}");
        }
        Err(other) => panic!("a Data error, got {other}"),
        Ok(r) => panic!("a clean result off an unread grid: final equity {}", r.final_equity),
    }
}

#[test]
fn a_failed_properties_read_fails_the_walk_forward() {
    let toml = format!(
        "{}\n[walkforward]\nn_splits = 2\n",
        bar_profile("bar", "snap_to_properties = true")
    );
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let outcome = crate::walkforward::runner::run_walkforward(
        &profile,
        OnePropertiesReadFails::past_the_preflight(),
    );
    match outcome {
        Err(HarnessError::Data(why)) => assert!(why.contains("went away"), "{why}"),
        Err(other) => panic!("a Data error, got {other}"),
        Ok(_) => panic!("a clean walk with an unchecked fill"),
    }
}

/// The refusal names the cause itself, not only the warn log, on the door that has no search: the
/// control arm of the optimizing walk runs each validation window straight.
#[test]
fn the_optimizing_walks_validation_refusal_carries_the_cause() {
    let toml = format!(
        "{}\n[walkforward]\nn_splits = 2\nsearch = \"none\"\n",
        bar_profile("bar", "snap_to_properties = true")
    );
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let outcome = crate::walkforward::runner::run_walkforward_optimized(
        &profile,
        OnePropertiesReadFails::past_the_preflight(),
    );
    match outcome {
        Err(HarnessError::Data(why)) => {
            assert!(why.contains("its own validation window"), "{why}");
            assert!(why.contains("went away"), "{why}");
        }
        Err(other) => panic!("a Data error, got {other}"),
        Ok(_) => panic!("a clean walk with an unchecked fill"),
    }
}

/// One candidate of a window's search that could not READ is not a candidate that lost: the winner
/// would come from fewer points than the grid named, in a walk that reports clean. A value the
/// strategy rejects stays the skip it always was
/// (`crates/vike-backtest/tests/walkforward_optimize.rs`).
#[test]
fn a_failed_properties_read_in_a_windows_search_fails_the_walk() {
    let toml = format!(
        "{}\n[paramscan]\nsize = [1.0, 2.0]\n\n[walkforward]\nn_splits = 2\nsearch = \"paramscan\"\n",
        bar_profile("bar", "snap_to_properties = true")
    );
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let outcome = crate::walkforward::runner::run_walkforward_optimized(
        &profile,
        OnePropertiesReadFails::past_the_preflight(),
    );
    match outcome {
        Err(HarnessError::Data(why)) => assert!(why.contains("went away"), "{why}"),
        Err(other) => panic!("a Data error, got {other}"),
        Ok(_) => panic!("a clean walk whose winner came from fewer points than the grid"),
    }
}

/// The run is refused from the first failed lookup on, so a dead datahub is not dialled again for
/// every later one. The first error stays the one kept.
#[test]
fn a_failed_properties_read_stops_the_lookups() {
    let store = OnePropertiesReadFails::new();
    let first_error: Arc<OnceLock<String>> = Arc::default();
    let lookup = properties_source(store.clone(), Arc::clone(&first_error));

    assert!(lookup(VENUE, SYMBOL, 1).is_none(), "the failed lookup reads as no record");
    assert!(lookup(VENUE, SYMBOL, 2).is_none(), "and so does every lookup after it");
    assert!(lookup(VENUE, SYMBOL, 3).is_none());

    let reads = store.reads.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(reads, 1, "the store was asked once, not once per lookup");
    let kept = first_error.get().expect("the failure is kept");
    assert!(kept.contains("went away") && kept.contains(" at 1 "), "the FIRST error: {kept}");
}

/// "No record" is a true answer and stays one: a store with no properties for the symbol runs, with
/// the fill unconstrained, exactly as before.
#[test]
fn no_recorded_properties_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let toml = bar_profile("bar", "snap_to_properties = true");
    let profile =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&profile, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "no record: the open fills unconstrained");
}

/// One symbol's prices on both lanes under `kind`'s own profile, plus `grid` as its properties rows.
fn snap_run(kind: &str, snap: &str, grid: &[(i64, SymbolProperties)]) -> TradingRecord {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();
    store
        .append_quotes(VENUE, SYMBOL, &[quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)], None)
        .unwrap();
    if !grid.is_empty() {
        store.append_symbol_properties(VENUE, SYMBOL, grid, None).unwrap();
    }
    let profile = ProfileBacktest::from_toml_str(&bar_profile(kind, snap))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    run_backtest(&profile, store).unwrap()
}

/// A run's findings but the one every BTCUSDT run on a store with no USDT-USD series carries, its USDT booked one
/// to one (decision 0243, amended 2026-11-16).
fn findings(result: &vike_analytics::TradingRecord) -> Vec<&String> {
    let usdt = crate::run::data_plan::USDT_USD_NOT_FOUND;
    result.data_warnings.iter().filter(|l| l.as_str() != usdt).collect()
}

/// **`engine.snap_to_properties` over a store with no grid for the symbol says so in the result.**
/// "No record" is a legal answer to a fill's lookup, so the fills skip the rounding and the minimums
/// and the report used to read as a clean run. It still runs: the finding is on the record.
#[test]
fn a_snap_run_says_when_the_store_holds_no_grid_for_its_symbol() {
    for kind in ["bar", "tick"] {
        let result = snap_run(kind, "snap_to_properties = true", &[]);
        assert_ne!(result.final_equity, 1000.0, "{kind}: the run still runs");
        let found = findings(&result);
        assert_eq!(found.len(), 1, "{kind}: {:?}", result.data_warnings);
        let line = found[0];
        assert!(line.starts_with(SYMBOL), "{kind}: names the symbol: {line}");
        assert!(line.contains("holds no instrument properties"), "{kind}: {line}");
        assert!(line.contains("min_qty"), "{kind}: says what the fills skipped: {line}");

        let off = snap_run(kind, "snap_to_properties = false", &[]);
        assert!(findings(&off).is_empty(), "{kind}: {:?}", off.data_warnings);
    }
}

/// A tape that begins inside the window is the shape a box has the day after the flag goes on: the
/// lookup at the window's end finds a row, and every fill before the first row skipped the same checks.
#[test]
fn a_snap_run_says_when_the_grid_begins_inside_its_window() {
    let grid =
        [(1000, SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() })];
    for kind in ["bar", "tick"] {
        let result = snap_run(kind, "snap_to_properties = true", &grid);
        let found = findings(&result);
        assert_eq!(found.len(), 1, "{kind}: {:?}", result.data_warnings);
        assert!(found[0].contains("begin after this window opens"), "{kind}");
    }
}

/// A grid that covers the window adds nothing, so a clean report stays byte-identical.
#[test]
fn a_snap_run_whose_grid_covers_its_window_adds_no_warning() {
    let grid = [(0, SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() })];
    for kind in ["bar", "tick"] {
        let result = snap_run(kind, "snap_to_properties = true", &grid);
        assert!(findings(&result).is_empty(), "{kind}: {:?}", result.data_warnings);
    }
}

/// `ProfileEngine::emulator_release_stops` reaches the BAR-mode `StrategyEngineConfig` construction site
/// (`bar_engine_params`): absent, a profile carries the harness default `false` (matching
/// `StrategyEngineConfig::default()`); an explicit `true` survives into the built params. Before this
/// fix, `bar_engine_params`'s literal assigned neither and `..Default::default()` silently
/// discarded the profile's value on this lane (the tick lane's own inline literal had the same
/// hole — see `ProfileEngine::emulator_release_stops`'s doc).
#[test]
fn emulator_release_stops_survives_into_engine_params() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());

    let profile_absent = ProfileBacktest::from_toml_str(&bar_profile("bar", ""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let params = bar_engine_params(
        &profile_absent,
        &store,
        &Arc::default(),
        &crate::run::costs::run_costs(&profile_absent).unwrap(),
    )
    .unwrap();
    assert!(!params.emulator_release_stops, "absent -> the harness default, false");

    let profile_true =
        ProfileBacktest::from_toml_str(&bar_profile("bar", "emulator_release_stops = true"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let params = bar_engine_params(
        &profile_true,
        &store,
        &Arc::default(),
        &crate::run::costs::run_costs(&profile_true).unwrap(),
    )
    .unwrap();
    assert!(params.emulator_release_stops, "explicit true must survive into StrategyEngineConfig");

    let profile_false =
        ProfileBacktest::from_toml_str(&bar_profile("bar", "emulator_release_stops = false"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let params = bar_engine_params(
        &profile_false,
        &store,
        &Arc::default(),
        &crate::run::costs::run_costs(&profile_false).unwrap(),
    )
    .unwrap();
    assert!(
        !params.emulator_release_stops,
        "explicit false must survive into StrategyEngineConfig"
    );
}

/// A profile that names no commission, over a strategy that states none, is charged none: no venue
/// table (the owner's ruling of 2026-11-18). A `commission` param reaches the engine as the very schedule a
/// rule stating it builds (`vike_model::Commission::schedule`), never as the flat rate.
#[test]
fn the_commission_param_reaches_the_engine_as_its_schedule() {
    use vike_model::FeeSchedule::{PerShareWithFloor, PercentMakerTaker};
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let fees_of = |params: &str| {
        let toml = bar_profile("bar", "").replace("size = 1.0", &format!("size = 1.0\n{params}"));
        let profile = ProfileBacktest::from_toml_str(&toml)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
        let p = bar_engine_params(
            &profile,
            &store,
            &Arc::default(),
            &crate::run::costs::run_costs(&profile).unwrap(),
        )
        .unwrap();
        (p.fee_rate, p.fee_schedule)
    };
    assert_eq!(fees_of(""), (0.0, None), "no venue table");
    assert_eq!(
        fees_of("commission = 0.05"),
        (0.0, Some(PercentMakerTaker { maker_bps: 5.0, taker_bps: 5.0 }))
    );
    assert_eq!(
        fees_of("commission = 1.0\ncommission_unit = \"per_order\""),
        (0.0, Some(PerShareWithFloor { per_share: 0.0, min: 1.0, max_percent: 0.0 }))
    );
}

/// A bar profile with extra `[data]` lines — the twin of [`bar_profile`], which splices into
/// `[engine]` instead. `data.warmup` and `data.detail_interval` are `[data]` keys, so neither
/// can be reached through that helper. The range is wide enough for a multi-DAY fixture (the
/// detail-tape tests need one coarse bar per day and 24 finer bars inside each).
fn data_profile(extra_data: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1d"
from = "0"
to = "300000000"
{extra_data}

[engine]
cash = 1000.0

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
"#
    )
}

/// Parse a profile WITHOUT `ProfileBacktest::validate` — serde only.
///
/// ⚠ Load-bearing for the refusal tests below, not a shortcut. `from_toml_str` validates, and
/// the cross-table half of these keys' rules (`detail_interval` against `[walkforward]` and
/// `engine.cash_gate`) belongs in `ProfileBacktest::refusals`; the moment the resolvers are
/// ALSO called from there, a `from_toml_str` on a deliberately-bad profile would fail at the
/// parse instead of reaching the resolver under test, and the assertion would silently stop
/// testing the thing it names. Going through serde alone keeps each test pinned to the
/// resolver that owns the sentence.
fn parse_unvalidated(toml_source: &str) -> ProfileBacktest {
    toml::from_str::<ProfileBacktest>(toml_source)
        .expect("the fixture is well-formed TOML")
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// `data.warmup` reaches `StrategyEngineConfig::warmup` through `bar_engine_params`, in both span
/// shapes — and absent leaves it `None`, which is the byte-identical no-op every profile
/// written before the key existed relies on.
///
/// The duration arm is the one worth a case each way: `"3d"` over `1d` bars is exactly 3, and
/// `"25h"` over `1d` bars is 1.04 bars of history — which must round UP to 2, because the key
/// states a MINIMUM and 1 bar is not 25 hours.
#[test]
fn warmup_reaches_engine_params_in_both_span_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());

    let absent = ProfileBacktest::from_toml_str(&data_profile(""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(
        bar_engine_params(
            &absent,
            &store,
            &Arc::default(),
            &crate::run::costs::run_costs(&absent).unwrap()
        )
        .unwrap()
        .warmup,
        None,
        "an absent data.warmup must leave the gate at exactly Strategy::warmup()"
    );

    let bars = ProfileBacktest::from_toml_str(&data_profile("warmup = \"200bars\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(
        bar_engine_params(
            &bars,
            &store,
            &Arc::default(),
            &crate::run::costs::run_costs(&bars).unwrap()
        )
        .unwrap()
        .warmup,
        Some(200)
    );

    let exact = ProfileBacktest::from_toml_str(&data_profile("warmup = \"3d\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(
        bar_engine_params(
            &exact,
            &store,
            &Arc::default(),
            &crate::run::costs::run_costs(&exact).unwrap()
        )
        .unwrap()
        .warmup,
        Some(3)
    );

    let rounds_up = ProfileBacktest::from_toml_str(&data_profile("warmup = \"25h\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(
        bar_engine_params(
            &rounds_up,
            &store,
            &Arc::default(),
            &crate::run::costs::run_costs(&rounds_up).unwrap()
        )
        .unwrap()
        .warmup,
        Some(2),
        "a duration that is not a whole number of bars rounds UP — 1 bar is not 25 hours"
    );
}

/// The `data.warmup` spellings that are refused rather than coerced, each by name.
///
/// A calendar span has no fixed step count, a duration has no meaning on a tick tape, and a
/// duration over an unparseable base interval has no divisor — in every one of those cases the
/// alternative to refusing is silently picking a number, which is the irreproducible answer
/// the key exists to remove.
#[test]
fn warmup_refuses_the_spellings_it_cannot_resolve() {
    let months = parse_unvalidated(&data_profile("warmup = \"3mo\""));
    let err = months.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("CALENDAR span"), "got {err:?}");

    // Tick lane: the count spelling is fine (the gate is an EVENT index), the duration is not.
    let tick = parse_unvalidated(
        &data_profile("warmup = \"7d\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    let err = tick.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("bar-mode only"), "got {err:?}");
    let tick_count = parse_unvalidated(
        &data_profile("warmup = \"200bars\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    assert_eq!(tick_count.data.warmup_steps().unwrap(), Some(200));

    // A duration needs the base interval as its divisor, so an unparseable one is refused
    // HERE rather than guessed at.
    let bad_base = parse_unvalidated(
        &data_profile("warmup = \"7d\"").replace("interval = \"1d\"", "interval = \"1fortnight\""),
    );
    let err = bad_base.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("data.interval"), "got {err:?}");

    // The grammar itself refuses a zero count, so there is no spelling meaning "no warm-up".
    let zero = parse_unvalidated(&data_profile("warmup = \"0bars\""));
    assert!(zero.data.warmup_steps().is_err());
}

/// The gate the key opens is REAL, and the number the report carries is the number the run
/// gated on — the pair that makes a declared warm-up auditable rather than merely accepted.
///
/// Three bars and `warmup = "3bars"` means `index >= 3` is never true, so `buy_hold` never
/// submits and equity never leaves the starting cash; the control (no key) opens a position on
/// the first bar. `TradingRecordSimulator::warmup` reading 3 is what lets
/// `vike_analytics::zero_trade` diagnose the flat run as `warmup-shortfall` instead of
/// reporting a strategy that "never signalled".
#[test]
fn a_declared_warmup_gates_the_run_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let gated = ProfileBacktest::from_toml_str(&data_profile("warmup = \"3bars\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&gated, store.clone()).unwrap();
    assert_eq!(
        result.final_equity, 1000.0,
        "a 3-bar warm-up over 3 bars must never open the dispatch gate"
    );
    assert_eq!(
        result.simulator().unwrap().warmup,
        3,
        "the report must carry the number the run actually gated on"
    );

    let control = ProfileBacktest::from_toml_str(&data_profile(""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let result = run_backtest(&control, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "the control must trade from the first bar");
    assert_eq!(result.simulator().unwrap().warmup, 0);
}

/// A strategy declaring its own warm-up, so [`StrategyEngineConfig::warmup`]'s `max` semantics can be
/// proven in BOTH directions. Counts the `on_bar` calls that got past the gate.
struct DeclaresWarmup {
    declared: usize,
    fired: std::rc::Rc<std::cell::Cell<usize>>,
}

impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for DeclaresWarmup {
    fn warmup(&self) -> usize {
        self.declared
    }
    fn on_bar(&mut self, _context: &mut vike_simulator::OrderBrokerSimulator, _bar: &Bar) {
        self.fired.set(self.fired.get() + 1);
    }
}

/// `StrategyEngineConfig::warmup` is a FLOOR on `Strategy::warmup()`, never a replacement — the
/// property `data.warmup`'s doc promises an operator, and the one that makes the key safe to
/// expose at all.
///
/// Driven at the ENGINE rather than through a profile because no shipped strategy file declares a
/// non-zero warm-up, so a profile-level test could only ever exercise the `0` side of the
/// `max` and would pass just as well against a plain assignment. Five bars, and the gate is
/// asserted by COUNTING dispatches (`5 - effective`) as well as by reading the reported
/// number, so a resolution that agreed with the report but not with the fold loop would still
/// fail.
#[test]
fn the_configured_warmup_is_a_floor_not_a_replacement() {
    let series = vec![(
        SYMBOL.to_string(),
        vec![
            flat_bar_zero_volume(0, 100.0),
            flat_bar_zero_volume(1, 100.0),
            flat_bar_zero_volume(2, 100.0),
            flat_bar_zero_volume(3, 100.0),
            flat_bar_zero_volume(4, 100.0),
        ],
    )];
    let run = |declared: usize, configured: Option<usize>| -> (usize, usize) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let strategy = DeclaresWarmup { declared, fired: std::rc::Rc::clone(&fired) };
        let params = StrategyEngineConfig { warmup: configured, ..Default::default() };
        let result = StrategyEngine::new(series.clone(), strategy, params).run();
        (fired.get(), result.simulator().unwrap().warmup)
    };

    // The configured floor is HIGHER: it wins.
    assert_eq!(run(1, Some(4)), (1, 4), "a higher configured floor must raise the gate");
    // The strategy's own requirement is HIGHER: it wins, and the configured number is inert.
    assert_eq!(
        run(4, Some(1)),
        (1, 4),
        "a lower configured floor must NOT lower a strategy's own warm-up — a run on \
         unconverged indicators is the failure this gate exists to prevent"
    );
    // Neither side asks for anything: the frozen behaviour.
    assert_eq!(run(0, None), (5, 0));
    // Only the strategy asks: unchanged from before the field existed.
    assert_eq!(run(2, None), (3, 2));
}

/// `data.detail_interval` loads one finer series per resolved symbol into the shape
/// `StrategyEngineConfig::granular_by_symbol` takes — the door onto the granular sub-bar lane, which
/// no profile could reach before.
#[test]
fn detail_interval_loads_one_finer_series_per_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let coarse = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(86_400_000, 101.0),
        flat_bar_zero_volume(172_800_000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &coarse, None).unwrap();
    let fine: Vec<Bar> =
        (0..72).map(|h| flat_bar_zero_volume(h * 3_600_000, 100.0 + h as f64 * 0.01)).collect();
    store.append_bars(VENUE, SYMBOL, "1h", &fine, None).unwrap();

    let absent = ProfileBacktest::from_toml_str(&data_profile(""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(
        load_profile_detail_bars(&absent, &store).unwrap().is_empty(),
        "an absent key must load nothing at all — the byte-identical no-op"
    );

    let on = ProfileBacktest::from_toml_str(&data_profile("detail_interval = \"1h\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let loaded = load_profile_detail_bars(&on, &store).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, SYMBOL, "the key must be a symbol of the run, by construction");
    assert_eq!(loaded[0].1.len(), 72);

    // ...and it reaches the engine: the whole run must still complete with the tape mounted.
    let result = run_backtest(&on, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "the run must still trade with a detail tape on");
}

/// The four `data.detail_interval` refusals, each replacing a silence that would read as a
/// working realism knob.
#[test]
fn detail_interval_refuses_every_silent_no_op() {
    // Not an interval the store could hold.
    let bad = parse_unvalidated(&data_profile("detail_interval = \"3mo\""));
    let err = bad.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("not a valid interval"), "got {err:?}");

    // Equal to the base: at most one sub-bar per step, so no ambiguity is resolved.
    let equal = parse_unvalidated(&data_profile("detail_interval = \"1d\""));
    let err = equal.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("strictly FINER"), "got {err:?}");

    // Coarser than the base: the same, and the opposite of what the operator asked for.
    let coarser = parse_unvalidated(
        &data_profile("detail_interval = \"4h\"").replace("interval = \"1d\"", "interval = \"1h\""),
    );
    let err = coarser.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("strictly FINER"), "got {err:?}");

    // Tick mode: `run_ticks` never reads the per-symbol sub-bar buckets.
    let tick = parse_unvalidated(
        &data_profile("detail_interval = \"1h\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    let err = tick.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("bar-mode only"), "got {err:?}");
}

/// A detail interval the store holds NOTHING for is refused, not silently downgraded.
///
/// `StrategyEngine::new` skips an empty sub-bar vector, so without this the run would fall
/// back to the coarse adverse-first guess for that symbol alone while the operator believed
/// the tape was mounted — a per-symbol silence, which is the hardest kind to notice.
#[test]
fn a_detail_interval_the_store_cannot_serve_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    store.append_bars(VENUE, SYMBOL, "1d", &[flat_bar_zero_volume(0, 100.0)], None).unwrap();

    let on = ProfileBacktest::from_toml_str(&data_profile("detail_interval = \"1h\""))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let err = load_profile_detail_bars(&on, &store).unwrap_err().to_string();
    assert!(err.contains("holds no 1h bars"), "got {err:?}");
}

/// The deribit USDC option of the measured demo fill, and the perpetual of its book, by their USER names (decision
/// 0290: an option keeps the venue's own; the venue's `BTC_USDC-PERPETUAL` is `BTCUSDC.P`).
const OPTION: &str = "BTC_USDC-27NOV26-80000-C";
const OPTION_BOOK_PERP: &str = "BTCUSDC.P";

/// Deribit's options schedule, as a strategy file states it: no commission unit names it, so only a
/// file's own `vike_model::Strategy::stated_fee` reaches a run (the owner's ruling of 2026-11-18).
const OPTIONS_FEE: vike_model::FeeSchedule =
    vike_model::FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 };

/// Buys `size` of `symbol` once, at its first bar or quote, stating [`OPTIONS_FEE`].
struct BuyOnceStatingOptionsFee {
    symbol: String,
    size: f64,
    bought: bool,
}

impl BuyOnceStatingOptionsFee {
    fn buy(&mut self, broker: &mut vike_simulator::OrderBrokerSimulator) {
        use vike_model::OrderBroker;
        if !self.bought {
            self.bought = true;
            broker.place(&self.symbol, "", 1, self.size, vike_model::OrderPlacement::Market, false);
        }
    }
}

impl Strategy<vike_simulator::OrderBrokerSimulator> for BuyOnceStatingOptionsFee {
    fn stated_fee(&self) -> Option<vike_model::FeeSchedule> {
        Some(OPTIONS_FEE)
    }
    fn on_bar(&mut self, broker: &mut vike_simulator::OrderBrokerSimulator, _: &Bar) {
        self.buy(broker);
    }
    fn on_quote_tick(
        &mut self,
        broker: &mut vike_simulator::OrderBrokerSimulator,
        _: &vike_marketdata::TickQuote,
    ) {
        self.buy(broker);
    }
}

/// A buy of 0.01 `buy` over deribit's `symbols`, by a strategy stating deribit's options schedule.
fn deribit_options_profile(kind: &str, symbols: &[&str], buy: &str) -> ProfileBacktest {
    let symbols = symbols.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ");
    let mut profile = ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "deribit"
symbols = [{symbols}]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000000.0

[strategy]
file = "trend/buy_hold.rs"
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in);
    let symbol = buy.to_string();
    profile.strategy.as_mut().unwrap().builder =
        Some(crate::run::strategy_builder::StrategyBuilder::new(move |_| {
            Ok(Box::new(BuyOnceStatingOptionsFee {
                symbol: symbol.clone(),
                size: 0.01,
                bought: false,
            }))
        }));
    profile
}

/// Twelve flat rows of deribit's `symbol` at `px`, as bars and as quotes.
fn append_flat_deribit(store: &HistoryStoreDataFusion, symbol: &str, px: f64) {
    let bars: Vec<_> = (0..12).map(|i| flat_bar_zero_volume(i * 1000, px)).collect();
    store.append_bars("deribit", symbol, "1d", &bars, None).unwrap();
    let quotes: Vec<_> = (0..12).map(|i| quote(i * 1000, px, px)).collect();
    store.append_quotes("deribit", symbol, &quotes, None).unwrap();
}

/// A deribit option is charged on its UNDERLYING's series, on both lanes. The measured demo fill: 0.01
/// at a premium of 1,900 with BTC at 77,266.67 paid 0.2318 USDC, 3 bps of the underlying (12.5 % of the
/// premium is 2.375). The run charged 0.0057, 3 bps of the PREMIUM, before.
#[test]
fn a_deribit_option_is_charged_on_its_underlying_series() {
    for kind in ["bar", "tick"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
        append_flat_deribit(&store, OPTION_BOOK_PERP, 77_266.67);
        append_flat_deribit(&store, OPTION, 1_900.0);
        let profile = deribit_options_profile(kind, &[OPTION_BOOK_PERP, OPTION], OPTION);
        let result = run_backtest(&profile, store).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        assert!((result.fees_paid - 0.2318).abs() < 1e-6, "{kind}: {}", result.fees_paid);
    }
}

/// Without its underlying's series an option's fee could only be charged on the premium: refused, naming
/// the option and the series that would price it, before a bar is read.
#[test]
fn a_deribit_option_without_its_underlying_series_is_refused() {
    for kind in ["bar", "tick"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
        append_flat_deribit(&store, OPTION, 1_900.0);
        let profile = deribit_options_profile(kind, &[OPTION], OPTION);
        let Err(HarnessError::Data(m)) = run_backtest(&profile, store) else {
            panic!("{kind}: an option ran with its fee on the premium");
        };
        assert!(m.contains(OPTION) && m.contains(OPTION_BOOK_PERP), "{kind}: {m}");
    }
}

/// A deribit symbol that is no option keeps the schedule's plain reading: 3 bps of its own notional.
#[test]
fn a_deribit_symbol_that_is_no_option_is_charged_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    append_flat_deribit(&store, OPTION_BOOK_PERP, 77_266.67);
    let profile = deribit_options_profile("bar", &[OPTION_BOOK_PERP], OPTION_BOOK_PERP);
    let result = run_backtest(&profile, store).unwrap();
    let want = 0.01 * 77_266.67 * 0.0003;
    assert!((result.fees_paid - want).abs() < 1e-12, "{} vs {want}", result.fees_paid);
}

/// The bar lane's engine params of `bar_profile("bar", engine)` over `store`, a failed read kept in `error`.
fn engine_params_over(
    engine: &str,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    error: &Arc<OnceLock<String>>,
) -> StrategyEngineConfig {
    let profile = ProfileBacktest::from_toml_str(&bar_profile("bar", engine))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    bar_engine_params(&profile, store, error, &crate::run::costs::run_costs(&profile).unwrap())
        .unwrap()
}

/// A store that holds no properties kind, as a CSV or synthetic store answers.
struct HoldsNoProperties;

fn holds_none(verb: &'static str) -> vike_data::HistoryStoreError {
    vike_data::HistoryStoreError::Unsupported(format!("{verb}: this store holds no properties"))
}

impl HistoryStore for HoldsNoProperties {
    vike_data::hist_store_stubs!(refuse(holds_none): all);
}

/// The strategy's dated tick (`dated_ticks`, the owner's precedence of 2026-11-19) is wired in a run that
/// does not snap and in no snapping run, whose gate reads the record already. It answers the record's tick
/// under the run's slot; a store that holds no properties kind answers no record; any other failed read
/// answers none and refuses the run, as a snapping run's does.
#[test]
fn a_run_that_does_not_snap_reads_the_dated_tick_and_a_failed_read_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let datafusion = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let record = SymbolProperties { tick_size: 0.05, ..Default::default() };
    datafusion.append_symbol_properties(VENUE, SYMBOL, &[(1_000, record)], None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(datafusion);
    let snapping = engine_params_over("snap_to_properties = true", &store, &Arc::default());
    assert!(snapping.dated_ticks.is_none(), "a snapping run reads the record through its gate");

    let error = Arc::default();
    let dated =
        engine_params_over("", &store, &error).dated_ticks.expect("a run that does not snap");
    assert_eq!(dated(SYMBOL, 0), None, "before the record");
    assert_eq!(dated(SYMBOL, 2_000), Some(0.05));
    assert_eq!(dated("OTHER", 2_000), None, "a slot the run does not hold");
    assert!(error.get().is_none());

    let none: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HoldsNoProperties);
    let error = Arc::default();
    let dated = engine_params_over("", &none, &error).dated_ticks.unwrap();
    assert_eq!(dated(SYMBOL, 2_000), None);
    assert!(error.get().is_none(), "holding no properties is no failure: {error:?}");

    let failing: Arc<dyn HistoryStore + Send + Sync> = OnePropertiesReadFails::new();
    let error = Arc::default();
    let dated = engine_params_over("", &failing, &error).dated_ticks.unwrap();
    assert_eq!(dated(SYMBOL, 2_000), None);
    let refused = refuse_after_failed_properties(&error).unwrap_err().to_string();
    assert!(
        refused.contains("went away") && refused.contains("tick the strategy reads"),
        "{refused}"
    );
}
