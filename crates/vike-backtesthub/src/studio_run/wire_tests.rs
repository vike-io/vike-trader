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
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &to_data_slice(slice),
        params,
        &vike_data::HistoryStoreMemory::new(),
        SliceRun::Single,
    )
    .expect("a profile")
}

/// A stand-in file stating 0.05 a contract, as `vike_model::Commission::per_contract(0.05)` does.
fn stating_a_fee(
    _file: &str,
    _params: &vike_model::params::Params,
) -> Result<Box<dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator> + Send>, String> {
    struct Stating;
    impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for Stating {
        fn stated_fee(&self) -> Option<FeeSchedule> {
            Some(FeeSchedule::PerShareWithFloor { per_share: 0.05, min: 0.0, max_percent: 0.0 })
        }
    }
    Ok(Box::new(Stating))
}

/// **A strategy's stated fee, at the boundary** (decision 0269): a Studio slice naming no commission
/// over a file stating 0.05 a contract is charged that, and the stamp says so.
#[test]
fn a_slice_over_a_rule_stating_a_fee_is_stamped_with_the_strategys() {
    let spec = StrategySpec::file("other/stating_a_fee.rs", vike_studio_core::empty_params());
    let profile = slice_profile(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(stating_a_fee),
        &to_data_slice(&slice_of("binance", "BTCUSDT")),
        None,
        &vike_data::HistoryStoreMemory::new(),
        SliceRun::Single,
    )
    .expect("a profile");
    let stamp = to_wire_cost_model(&profile, [], FillMix::default(), None);
    assert_eq!((stamp.source.as_str(), stamp.lane), ("strategy", None));
    assert_eq!(stamp.variant, "PerShareWithFloor");
}

/// **No venue table, at the boundary** (the owner's ruling of 2026-11-18): a slice naming no commission
/// over a strategy stating none pays none, and the stamp says so in words rather than leaving a reader
/// to conclude that trading is free.
#[test]
fn a_slice_naming_no_commission_is_stamped_none_in_words() {
    let stamp = to_wire_cost_model(
        &profile_of(&slice_of("binance", "BTCUSDT.P"), None),
        [],
        FillMix::default(),
        None,
    );
    assert_eq!((stamp.source.as_str(), stamp.variant.as_str()), ("none", "none"));
    assert_eq!((stamp.maker_rate, stamp.taker_rate, stamp.lane.as_deref()), (0.0, 0.0, None));
    assert!(stamp.reason.as_deref().is_some_and(|r| r.contains("no venue table")), "{stamp:?}");
}

/// **The params panel's `commission` row is the run's commission**: it rides in the strategy's params, the
/// stamp calls it an override, and its rate is the one the engine charged.
#[test]
fn a_commission_row_is_the_override() {
    let mut params = vike_studio_core::empty_params();
    params.insert("commission", ParamValue::Float(0.1));
    let spec = StrategySpec::file("trend/buy_hold.rs", params);
    let profile = slice_profile(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &to_data_slice(&slice_of("binance", "BTCUSDT")),
        None,
        &vike_data::HistoryStoreMemory::new(),
        SliceRun::Single,
    )
    .expect("a profile");
    let stamp = to_wire_cost_model(&profile, [], FillMix::default(), None);
    assert_eq!((stamp.source.as_str(), stamp.variant.as_str()), ("override", "PercentMakerTaker"));
    assert_eq!((stamp.maker_rate, stamp.taker_rate, stamp.reason), (0.001, 0.001, None));
}

/// **A sweep over `commission` is no one cost**: its points each paid their own, so the stamp calls the
/// commission an override that varies by point, names the axis and carries the fees the points paid,
/// and never reads as free. A grid over another param keeps the base's word.
#[test]
fn a_sweep_over_commission_is_stamped_as_varying_never_free() {
    let profile = profile_of(&slice_of("binance", "BTCUSDT"), None);
    let mix = FillMix { fees_paid: 1.5, ..FillMix::default() };
    let stamp = to_wire_cost_model(&profile, ["commission", "size"], mix, Some("sharpe"));
    assert_eq!((stamp.source.as_str(), stamp.variant.as_str()), ("override", "varies by point"));
    assert!(stamp.reason.as_deref().is_some_and(|r| r.contains("(axis: commission)")), "{stamp:?}");
    assert_eq!((stamp.fees_paid, stamp.maker_rate, stamp.taker_rate), (1.5, 0.0, 0.0));
    let unswept = to_wire_cost_model(&profile, ["size"], FillMix::default(), None);
    assert_eq!(unswept.source, "none", "{unswept:?}");
}

/// **An older Studio's cost fields are refused in words, never read**: a run's commission and slippage
/// are strategy params, so a `fee_rate`, a slippage value, model or tick on the wire names them.
#[test]
fn the_old_wire_cost_fields_are_refused_naming_the_params() {
    let refused = |wire: StrategyEngineConfigWire, words: &str| {
        let spec = StrategySpec::file("trend/buy_hold.rs", vike_studio_core::empty_params());
        let err = slice_profile(
            &spec,
            &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
            &to_data_slice(&slice_of("binance", "BTCUSDT")),
            Some(&wire),
            &vike_data::HistoryStoreMemory::new(),
            SliceRun::Single,
        )
        .expect_err("refused");
        assert!(err.to_string().contains(words), "{err}");
    };
    let none = StrategyEngineConfigWire::default;
    refused(StrategyEngineConfigWire { fee_rate: Some(0.0), ..none() }, "commission is a strategy");
    refused(StrategyEngineConfigWire { slippage: Some(0.001), ..none() }, "slippage is a strategy");
    refused(
        StrategyEngineConfigWire { slippage_model: Some("ticks".to_string()), ..none() },
        "slippage is a strategy",
    );
    refused(
        StrategyEngineConfigWire { slippage_tick: Some(0.01), ..none() },
        "slippage is a strategy",
    );
    let cash = StrategyEngineConfigWire { cash: Some(5_000.0), ..none() };
    assert_eq!(profile_of(&slice_of("binance", "BTCUSDT"), Some(&cash)).engine.cash, 5_000.0);
}

/// The residuals ride the stamp itself rather than living only in the record — a statement that
/// said only what IS modelled would invite a reader to infer that nothing else exists.
#[test]
fn every_stamp_carries_the_declared_residuals() {
    let profile = profile_of(&slice_of("okx", "BTCUSDT"), None);
    let stamp = to_wire_cost_model(&profile, [], FillMix::default(), Some("sharpe"));
    assert_eq!(stamp.not_modelled.len(), NOT_MODELLED.len());
    assert_eq!(stamp.rank_metric.as_deref(), Some("sharpe"));
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

/// A store keeping a binance and an okx row, recording each kept ask: the venues asked, or `None` for every venue's.
#[derive(Default)]
struct AskedKept(std::sync::Mutex<Vec<Option<Vec<String>>>>);

impl AskedKept {
    fn rows() -> Vec<vike_model::Instrument> {
        let spot = || vike_model::Contract::CryptoSpot;
        vike_catalog::spelled(vec![
            vike_model::Instrument::new("binance", "BTCUSDT", "BTC", "USDT", spot()),
            vike_model::Instrument::new("okx", "BTC-USDT", "BTC", "USDT", spot()),
        ])
    }
}

impl vike_data::HistoryStore for AskedKept {
    fn kept_instruments_and_splits(
        &self,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, vike_data::HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, vike_data::HistoryStoreError>,
    ) {
        self.0.lock().expect("asks").push(None);
        (Ok(Some(Self::rows())), Ok(None))
    }
    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, vike_data::HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, vike_data::HistoryStoreError>,
    ) {
        self.0.lock().expect("asks").push(Some(venues.to_vec()));
        let rows = Self::rows().into_iter().filter(|r| venues.contains(&r.venue)).collect();
        (Ok(Some(rows)), Ok(None))
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// **A Studio single run asks its own venues, a scan every venue's** (`SliceRun`): a single run of binance BTCUSDT asks
/// binance alone, as a CLI single run does, a scan's slice asks every venue's, and both hold the same row and warnings.
#[test]
fn a_studio_single_run_asks_its_venues_and_a_scan_every_venues() {
    let spec = StrategySpec::file("trend/buy_hold.rs", vike_studio_core::empty_params());
    let files = crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in);
    let slice = to_data_slice(&slice_of("binance", "BTCUSDT"));
    let asked = |run| {
        let store = AskedKept::default();
        let profile = slice_profile(&spec, &files, &slice, None, &store, run).expect("a profile");
        (store.0.into_inner().expect("asks"), profile.kept)
    };
    let (single, single_kept) = asked(SliceRun::Single);
    let (points, points_kept) = asked(SliceRun::Points);
    assert_eq!(single, [Some(vec!["binance".to_string()])]);
    assert_eq!(points, [None]);
    assert_eq!(single_kept.warnings, points_kept.warnings);
    let row = |kept: &vike_backtest::run::KeptRows| kept.row("binance", "BTCUSDT").cloned();
    assert_eq!(row(&single_kept), row(&points_kept));
    assert!(row(&single_kept).is_some(), "the slice's row is held");
}
