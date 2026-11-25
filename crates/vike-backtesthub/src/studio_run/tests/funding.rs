//! A perpetual's funding through the Studio's slice runners and through the profile run (the CLI/MCP
//! door, `vike_backtest::run::run_backtest`) over ONE store: a Studio slice IS a profile run, so
//! the same slice reports the same `funding_paid`.

use std::sync::Arc;
use vike_backtest::run::{profile::ProfileBacktest, run_backtest};
use vike_backtesthub_client::wire_studio::{
    DataSliceWire, ReplayKindWire, StrategyEngineConfigWire, StrategySpecWire, WalkforwardWire,
};
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::Bar;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_model::params::ParamValue;
use vike_store::HistoryStoreDataFusion;

use crate::studio_run::{
    run_paramscan_slice, run_paramscan_slice_with_params, run_slice, run_walkforward_local,
    run_walkforward_slice_with_params,
};
use vike_studio_core::{DataSlice, StrategySpec};

const HOUR: i64 = 3_600_000;
const SYMBOL: &str = "BTCUSDT.P";
const PARAMS: &str = "size = 1.0\nsymbol = \"BTCUSDT.P\"";

/// A row of the stored funding series: the rate at its instant and the venue's mark on `close`
/// (`0.0` = none), the shape the funding backfill writes.
fn funding(ts: i64, rate: f64, mark: f64) -> Bar {
    Bar { funding: Some(rate), close: mark, ..flat_bar_zero_volume(ts, 0.0) }
}

/// The profile run of the same slice: `buy_hold` long 1.0 from the second bar's open, funding ON
/// (`attach_funding = true`: absent is off since 2026-11-21).
fn profile(kind: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        "[data]\nvenue = \"bybit\"\nsymbols = [\"{SYMBOL}\"]\nkind = \"{kind}\"\ninterval = \"1h\"\n\
         from = \"0\"\nto = \"1000000000\"\n\n[engine]\ncash = 100000.0\nattach_funding = true\n\n\
         [strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\n{PARAMS}\n"
    ))
    .unwrap().with_stand_in(vike_strategy_lab::stand_in)
}

fn spec() -> StrategySpec {
    StrategySpec::file_from_toml_str("trend/buy_hold.rs", PARAMS).unwrap()
}

/// The profile's cash at the venue's own fees, funding switched ON as [`profile`] states it.
const COSTS: StrategyEngineConfigWire = StrategyEngineConfigWire {
    cash: Some(100_000.0),
    fee_rate: None,
    slippage: None,
    slippage_model: None,
    slippage_tick: None,
    attach_funding: Some(true),
};

/// 48 hourly bars, open 100+i and close 101+i.
fn ramp() -> Vec<Bar> {
    (0..48)
        .map(|i| {
            let (open, close) = (100.0 + i as f64, 101.0 + i as f64);
            Bar { open, high: close, low: open, close, ..flat_bar_zero_volume(i * HOUR, open) }
        })
        .collect()
}

/// bybit's BTCUSDT perpetual on 1h bars (open 100+i, close 101+i) with 8h settlements, two carrying
/// the venue's mark: each charged once on the 1.0 held, at the mark else the bar's open:
/// 108·1e-4 + 150·2e-4 + 124·3e-4 + 150·4e-4 + 140·5e-4 = 0.208.
#[test]
fn a_studio_slice_and_a_profile_run_charge_the_same_funding() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("bybit", SYMBOL, "1h", &ramp(), None).unwrap();
    let rows: Vec<Bar> = (1..=5)
        .map(|k| funding(k * 8 * HOUR, 1e-4 * k as f64, if k % 2 == 0 { 150.0 } else { 0.0 }))
        .collect();
    store.append_bars("bybit", SYMBOL, "funding", &rows, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let cli = run_backtest(&profile("bar"), store.clone()).unwrap().funding_paid;
    assert!((cli - (-0.208)).abs() < 1e-9, "the profile run charges every settlement: {cli}");

    let slice = DataSlice::bars("bybit", SYMBOL, "1h", TimestampRange::all());
    let studio = run_slice(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        Some(&COSTS),
    )
    .unwrap()
    .funding_paid;
    assert_eq!(studio, cli, "a Studio run of the same slice pays the same funding");

    let grid = [("size".to_string(), vec![ParamValue::Float(1.0)])];
    let sweep = run_paramscan_slice_with_params(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        &grid,
        Some(&COSTS),
    )
    .unwrap();
    assert_eq!(sweep.entries[0].result.funding_paid, cli, "a sweep point pays it too");
    let unswitched = run_paramscan_slice(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        &grid,
    )
    .unwrap();
    assert_eq!(unswitched.entries[0].result.funding_paid, 0.0, "a sweep with no switch pays none");
}

/// The Studio's funding switch (2026-11-20): `attach_funding = false` runs the same perpetual slice with no funding
/// paid, as the absent key does since 2026-11-21, `true` charges it, and the slice's profile text states the key only
/// when it is set.
#[test]
fn a_studio_run_with_its_funding_switch_off_pays_no_funding() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("bybit", SYMBOL, "1h", &ramp(), None).unwrap();
    let rows: Vec<Bar> = (1..=5).map(|k| funding(k * 8 * HOUR, 1e-4 * k as f64, 0.0)).collect();
    store.append_bars("bybit", SYMBOL, "funding", &rows, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let files = crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in);
    let slice = DataSlice::bars("bybit", SYMBOL, "1h", TimestampRange::all());
    let run = |attach_funding: Option<bool>| {
        let costs = StrategyEngineConfigWire { attach_funding, ..COSTS };
        run_slice(&spec(), &files, &slice, &store, Some(&costs)).unwrap()
    };

    let on = run(Some(true)).funding_paid;
    assert!(on < 0.0, "a perpetual pays its funding when the switch is on: {on}");
    for attach_funding in [None, Some(false)] {
        let off = run(attach_funding);
        assert_eq!(off.funding_paid, 0.0, "{attach_funding:?} pays none");
        let warned = off.data_warnings.iter().any(|w| w.contains("NO FUNDING"));
        assert!(!warned, "{attach_funding:?}: {:?}", off.data_warnings);
    }

    let text = |attach_funding: Option<bool>| {
        let costs = StrategyEngineConfigWire { attach_funding, ..COSTS };
        let strategy = crate::studio_run::strategy_config(&spec(), &files).unwrap();
        ProfileBacktest::slice_text(
            "bybit",
            &[SYMBOL.to_string()],
            "1h",
            TimestampRange::all(),
            vike_model::run_request::replay::ReplayKind::Bar,
            Some(&costs),
            &strategy,
        )
        .unwrap()
    };
    assert!(text(Some(false)).contains("attach_funding = false"), "{}", text(Some(false)));
    assert!(!text(None).contains("attach_funding"), "{}", text(None));
}

/// A perpetual that asked for funding with NO stored history runs uncharged on both doors, with the same
/// report line.
#[test]
fn a_perpetual_without_history_names_what_each_door_can_do() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let price: Vec<Bar> = (0..10).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
    store.append_bars("bybit", SYMBOL, "1h", &price, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let line = |warnings: &[String]| {
        warnings.iter().find(|w| w.contains("NO FUNDING")).cloned().unwrap_or_default()
    };

    let cli = run_backtest(&profile("bar"), store.clone()).unwrap();
    let cli_line = line(&cli.data_warnings);
    assert_eq!(cli.funding_paid, 0.0);
    assert!(
        cli_line.contains("bybit:BTCUSDT.P:funding")
            && cli_line
                .ends_with("or drop `engine.attach_funding = true` to run without it on purpose"),
        "{:?}",
        cli.data_warnings
    );

    let slice = DataSlice::bars("bybit", SYMBOL, "1h", TimestampRange::all());
    let studio = run_slice(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        Some(&COSTS),
    )
    .unwrap();
    assert_eq!(studio.funding_paid, 0.0);
    assert_eq!(line(&studio.data_warnings), cli_line, "{:?}", studio.data_warnings);
}

/// The tick lane: one settlement at 5s, charged on the first tick at or after it at the mid (100).
#[test]
fn a_studio_tick_slice_and_a_profile_tick_run_charge_the_same_funding() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let quotes: Vec<_> = (0..11).map(|i| quote(i * 1_000, 99.0, 101.0)).collect();
    store.append_quotes("bybit", SYMBOL, &quotes, None).unwrap();
    store.append_bars("bybit", SYMBOL, "funding", &[funding(5_000, 0.01, 0.0)], None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let cli = run_backtest(&profile("tick"), store.clone()).unwrap().funding_paid;
    assert!((cli - (-1.0)).abs() < 1e-9, "1 x 100 x 0.01: {cli}");

    let slice = DataSlice::ticks("bybit", vec![SYMBOL.to_string()], TimestampRange::all());
    let studio = run_slice(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &store,
        Some(&COSTS),
    )
    .unwrap()
    .funding_paid;
    assert_eq!(studio, cli, "a Studio tick run of the same slice pays the same funding");
}

/// A 2-split walk over the 48 bars tests bars [16, 32) and [32, 48), each window buying 1.0 on its
/// second bar. Settlements a few bars inside each window are held through in the windows and in one
/// run over [16h, 47h] alike: 120·1e-4 + 150·2e-4 + 136·3e-4 + 150·4e-4 = 0.1428.
#[test]
fn a_walk_forward_pays_the_funding_a_single_run_over_its_windows_pays() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("bybit", SYMBOL, "1h", &ramp(), None).unwrap();
    let rows = [
        funding(20 * HOUR, 1e-4, 0.0),
        funding(28 * HOUR, 2e-4, 150.0),
        funding(36 * HOUR, 3e-4, 0.0),
        funding(44 * HOUR, 4e-4, 150.0),
    ];
    store.append_bars("bybit", SYMBOL, "funding", &rows, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let covered = DataSlice::bars("bybit", SYMBOL, "1h", TimestampRange::of(16 * HOUR, 47 * HOUR));
    let single = run_slice(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &covered,
        &store,
        Some(&COSTS),
    )
    .unwrap()
    .funding_paid;
    assert!((single - (-0.1428)).abs() < 1e-9, "the single run charges every settlement: {single}");

    let all = DataSlice::bars("bybit", SYMBOL, "1h", TimestampRange::all());
    let walk = run_walkforward_slice_with_params(
        &spec(),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &all,
        &store,
        2,
        Some(&COSTS),
    )
    .unwrap();
    assert_eq!(walk.steps.len(), 2);
    assert!(
        (walk.overall.funding_paid - single).abs() < 1e-9,
        "the walk's windows pay what one run over them pays: {} vs {single}",
        walk.overall.funding_paid
    );
}

/// A walk-forward over a perpetual that asked for funding with NO stored history: the no-funding line
/// reaches the `RunWalkforward` caller on the wire answer, as it reaches a `RunSlice` caller on the run's.
#[test]
fn a_walk_forwards_no_funding_line_crosses_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("bybit", SYMBOL, "1h", &ramp(), None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);

    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: PARAMS.to_string(),
        sdk: None,
    };
    let slice = DataSliceWire {
        venue: "bybit".to_string(),
        symbols: vec![SYMBOL.to_string()],
        interval: "1h".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    };
    let wire = run_walkforward_local(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice,
        &WalkforwardWire::fixed(2),
        Some(&COSTS),
        store,
    )
    .unwrap();
    assert!(
        wire.overall
            .data_warnings
            .iter()
            .any(|w| w.contains("NO FUNDING") && w.contains("bybit:BTCUSDT.P:funding")),
        "{:?}",
        wire.overall.data_warnings
    );
}
