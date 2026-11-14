//! The server entries' tests: the cost stamp at the boundary, a Studio slice's fees through
//! `run_slice_local`, and the walk-forward selector as the profile's own table. They were
//! `vike-studio-core`'s `wire_run_tests.rs` until the entries moved here (2026-11-10); the conversion
//! round trips stayed there, beside the conversions.

use super::*;
use vike_backtesthub_client::wire_studio::{ReplayKindWire, WalkforwardWindowSearchWire};
use vike_model::params::ParamValue;
use vike_studio_core::StrategySpec;

/// A bar slice over a venue, for the cost-model tests below.
fn slice_of(venue: &str, symbol: &str) -> DataSliceWire {
    DataSliceWire {
        venue: venue.to_string(),
        symbols: vec![symbol.to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    }
}

/// The profile a `buy_hold` over `slice` runs as, at `params`.
fn profile_of(slice: &DataSliceWire, params: Option<&StrategyEngineConfigWire>) -> ProfileBacktest {
    let spec = StrategySpec::file("trend/buy_hold.rs", vike_studio_core::empty_params());
    slice_profile(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &to_data_slice(slice),
        params,
        &vike_data::HistoryStoreMemory::new(),
    )
    .expect("a profile")
}

/// **The derivation, at the boundary.** With no `fee_rate` on the wire the slice's profile states
/// the venue's own table, and the stamp names the lane that answered — the change that stopped a
/// Studio run being a zero-cost backtest.
#[test]
fn absent_wire_params_cost_the_slice_at_its_own_fee_lane() {
    let profile = profile_of(&slice_of("binance", "BTCUSDT.P"), None);
    assert_eq!(profile.engine.fee.as_ref().map(|f| f.kind.as_str()), Some("venue"));
    let stamp = to_wire_cost_model(&profile, FillMix::default(), None);
    assert_eq!(stamp.source, "derived");
    assert_eq!(stamp.lane.as_deref(), Some("binance-perp"), "the .P symbol selects the perp lane");
}

/// **The PRECEDENCE, at the boundary.** A caller's flat `fee_rate` is the profile's flat rate and
/// states NO table, so nothing displaces it; an explicit zero is `kind = "free"`, an OVERRIDE and
/// not an absence — it names a rate, never a reason.
#[test]
fn an_explicit_fee_rate_overrides_and_a_zero_is_free() {
    let slice = slice_of("binance", "BTCUSDT.P");
    let flat =
        StrategyEngineConfigWire { fee_rate: Some(0.001), ..StrategyEngineConfigWire::default() };
    let profile = profile_of(&slice, Some(&flat));
    assert_eq!(profile.engine.fee_rate, 0.001, "the caller's rate reaches the engine");
    assert!(profile.engine.fee.is_none(), "and no table is stated beside it");
    let stamp = to_wire_cost_model(&profile, FillMix::default(), None);
    assert_eq!((stamp.source.as_str(), stamp.taker_rate), ("override", 0.001));

    let zero =
        StrategyEngineConfigWire { fee_rate: Some(0.0), ..StrategyEngineConfigWire::default() };
    let profile = profile_of(&slice, Some(&zero));
    assert_eq!(profile.engine.fee.as_ref().map(|f| f.kind.as_str()), Some("free"));
    let stamp = to_wire_cost_model(&profile, FillMix::default(), None);
    assert_eq!((stamp.source.as_str(), stamp.taker_rate), ("override", 0.0));
    assert_eq!(stamp.reason, None);
}

/// **The Studio's slippage switch writes the profile's ONE table.** Each model the switch offers
/// lands as `[engine.slippage]` with its value under the key that model owns; a value with no
/// model is the flat fraction (`percent`), the field's old meaning; nothing at all is no table; a
/// value the chosen model has no key for is the table's own refusal, never a silent drop.
#[test]
fn the_slippage_switch_writes_the_one_profile_table() {
    let slice = slice_of("binance", "BTCUSDT");
    let table = |model: Option<&str>, value: Option<f64>, tick: Option<f64>| {
        let wire = StrategyEngineConfigWire {
            slippage: value,
            slippage_model: model.map(str::to_string),
            slippage_tick: tick,
            ..StrategyEngineConfigWire::default()
        };
        let s = profile_of(&slice, Some(&wire)).engine.slippage;
        s.map(|s| (s.model, s.value, s.ticks, s.tick, s.base))
    };
    assert_eq!(table(None, None, None), None, "no slippage, no table");
    let percent = Some(("percent".to_string(), Some(0.0005), None, None, None));
    assert_eq!(table(None, Some(0.0005), None), percent, "a bare value is the flat fraction");
    assert_eq!(table(Some("percent"), Some(0.0005), None), percent);
    assert_eq!(
        table(Some("ticks"), Some(2.0), Some(0.01)),
        Some(("ticks".to_string(), None, Some(2.0), Some(0.01), None))
    );
    assert_eq!(
        table(Some("market_impact"), Some(0.0002), None),
        Some(("market_impact".to_string(), None, None, None, Some(0.0002)))
    );
    assert_eq!(table(Some("none"), None, None), Some(("none".to_string(), None, None, None, None)));
    let wire = StrategyEngineConfigWire {
        slippage: Some(0.001),
        slippage_model: Some("none".to_string()),
        ..StrategyEngineConfigWire::default()
    };
    let spec = StrategySpec::file("trend/buy_hold.rs", vike_studio_core::empty_params());
    let err = slice_profile(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &to_data_slice(&slice),
        Some(&wire),
        &vike_data::HistoryStoreMemory::new(),
    )
    .expect_err("refused");
    assert!(err.to_string().contains("belongs to the percent model"), "{err}");
}

/// The residuals ride the stamp itself rather than living only in the record — a statement that
/// said only what IS modelled would invite a reader to infer that nothing else exists.
#[test]
fn every_stamp_carries_the_declared_residuals() {
    let profile = profile_of(&slice_of("okx", "BTCUSDT"), None);
    let stamp = to_wire_cost_model(&profile, FillMix::default(), Some("sharpe"));
    assert_eq!(stamp.not_modelled.len(), NOT_MODELLED.len());
    assert_eq!(stamp.rank_metric.as_deref(), Some("sharpe"));
    // the variant is carried, so a zero pair can never be misread as free trading
    assert_eq!(stamp.variant, "PercentMakerTaker");
}

/// A store of `venue`'s flat 1m bars, one series per `(symbol, price)`.
fn flat_store(
    venue: &str,
    series: &[(&str, f64)],
) -> (tempfile::TempDir, std::sync::Arc<vike_store::HistoryStoreDataFusion>) {
    use vike_data::HistoryStore;
    use vike_marketdata::test_support::flat_bar_zero_volume;
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(vike_store::HistoryStoreDataFusion::open(dir.path()).unwrap());
    for (symbol, px) in series {
        let bars: Vec<_> = (0..12).map(|i| flat_bar_zero_volume(i * 60_000, *px)).collect();
        store.append_bars(venue, symbol, "1m", &bars, None).unwrap();
    }
    (dir, store)
}

/// The fee a Studio `buy_hold` of `size` `buy` paid over `venue`'s `symbols`.
fn fee_paid(
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    venue: &str,
    symbols: &[&str],
    buy: &str,
    size: f64,
) -> Result<f64, String> {
    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: format!("size = {size:?}\nsymbol = \"{buy}\"\n"),
    };
    let slice = DataSliceWire {
        venue: venue.to_string(),
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    };
    let run = run_slice_local(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        None,
        store.clone(),
    )
    .map_err(|e| e.wire_line())?;
    Ok(run.cost_model.expect("a stamped run").fees_paid)
}

/// **A Studio slice of a deribit option is charged on its underlying, or refused**, the profile
/// harness's rule. The measured demo fill: 0.01 of a USDC option at 1,900 with BTC at 77,266.67 paid
/// 0.2318 USDC. With its perpetual in the slice the option pays that and the perpetual its own row;
/// without the perpetual it is refused by name, where it was charged on the premium.
#[test]
fn a_deribit_option_slice_is_charged_on_its_underlying_or_refused() {
    let (option, perp) = ("BTC_USDC-27NOV26-80000-C", "BTC_USDC-PERPETUAL");
    let (_dir, store) = flat_store("deribit", &[(perp, 77_266.67), (option, 1_900.0)]);
    let fees = fee_paid(&store, "deribit", &[perp, option], option, 0.01).expect("priced and run");
    assert!((fees - 0.2318).abs() < 1e-6, "the venue's 0.2318, not zero: {fees}");
    let perp_fees = fee_paid(&store, "deribit", &[perp, option], perp, 0.01).expect("priced");
    let perp_row = fee_schedule_for_lane("deribit-futures").commission(false, 0.01, 77_266.67);
    assert!((perp_fees - perp_row).abs() < 1e-12, "the perpetual's own row: {perp_fees}");

    let err = fee_paid(&store, "deribit", &[option], option, 0.01).expect_err("refused");
    assert!(err.contains(option) && err.contains(perp), "{err}");
}

/// **A Studio basis slice pays each leg its own row**: binance's spot pair at the spot row and its
/// perpetual at the perp row, and the stamp says no one pair is the run's.
#[test]
fn a_binance_spot_and_perp_slice_charges_each_symbol_its_own_row() {
    let both = ["BTCUSDT", "BTCUSDT.P"];
    let (_dir, store) = flat_store("binance", &[("BTCUSDT", 100.0), ("BTCUSDT.P", 100.0)]);
    for (buy, lane) in [("BTCUSDT", "binance"), ("BTCUSDT.P", "binance-perp")] {
        let fees = fee_paid(&store, "binance", &both, buy, 1.0).expect("priced and run");
        let want = fee_schedule_for_lane(lane).commission(false, 1.0, 100.0);
        assert!(want > 0.0 && (fees - want).abs() < 1e-12, "{buy}: {fees} vs {want}");
    }
    let mut slice = slice_of("binance", "BTCUSDT");
    slice.symbols.push("BTCUSDT.P".to_string());
    let stamp = to_wire_cost_model(&profile_of(&slice, None), FillMix::default(), None);
    assert_eq!(stamp.variant, "PerSymbol");
    assert_eq!(stamp.lane.as_deref(), Some("binance, binance-perp"));
}

/// `vike_venues`' row for `lane`.
fn fee_schedule_for_lane(lane: &str) -> vike_model::FeeSchedule {
    vike_venues::fee_schedule_for(lane)
}

/// The walk-forward selector as the profile's own `[walkforward]` table: an unknown method or
/// metric is the profile's named refusal, never a fixed walk; an absent selector and an explicit
/// `none` (in any case) are the no-search control.
#[test]
fn the_walk_forward_selector_is_the_profiles_walkforward_table() {
    let profile = || profile_of(&slice_of("binance", "BTCUSDT"), None);
    let search = |method: &str, rank_by: Option<&str>| WalkforwardWindowSearchWire {
        method: method.to_string(),
        grid: ParamscanWire { axes: vec![("size".to_string(), vec![ParamValue::Float(1.0)])] },
        rank_by: rank_by.map(str::to_string),
    };
    let refused = |s: WalkforwardWindowSearchWire| match walkforward_profile(profile(), 4, Some(&s))
    {
        Err(RunError::Data(why)) => why,
        other => panic!("refused as data, got {other:?}"),
    };
    let why = refused(search("bayesian", None));
    assert!(why.contains("bayesian") && why.contains("none | paramscan"), "{why}");
    assert!(refused(search("paramscan", Some("calmar"))).contains("calmar"));

    for walk in [None, Some(search("NONE", None))] {
        let p = walkforward_profile(profile(), 4, walk.as_ref()).expect("the control");
        let config = p.walkforward.expect("a [walkforward] table");
        assert!(!config.window_search().expect("parses").searches());
    }
}
