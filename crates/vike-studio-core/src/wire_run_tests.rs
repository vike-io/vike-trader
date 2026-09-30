use super::*;
// One PRODUCTION source of `chosen_params` values: a `[sweep]` grid expanded into points, which
// is where the PROFILE door gets its own. (⚠ The comment here used to say this crate "cannot
// name `toml::Value` at all — no `toml` dependency". It can and it does: the manifest carries
// `toml`, and since the search arm landed `crate::run` builds a `toml::Value::Float` per chosen
// axis. The no-`toml` fact belongs to `vike-datahub-client`, which is why the RENDERING is on
// the wire.) The harness path is kept here because these four axes carry four TOML scalar
// SHAPES a hand-built `Float` could not exercise.
use vike_backtest::harness::{BacktestProfile, expand_paramscan};

#[test]
fn to_engine_params_none_is_all_default() {
    let ep = to_engine_params(None);
    let def = EngineParams::default();
    assert_eq!(ep.cash.to_bits(), def.cash.to_bits());
    assert_eq!(ep.fee_rate.to_bits(), def.fee_rate.to_bits());
    assert_eq!(ep.slippage.to_bits(), def.slippage.to_bits());
}

#[test]
fn to_engine_params_applies_only_set_fields() {
    let def = EngineParams::default();
    let wp = WireEngineParams { cash: Some(5000.0), fee_rate: None, slippage: Some(0.25) };
    let ep = to_engine_params(Some(&wp));
    assert_eq!(ep.cash, 5000.0);
    assert_eq!(ep.slippage, 0.25);
    // an omitted field keeps the engine default (bit-for-bit)
    assert_eq!(ep.fee_rate.to_bits(), def.fee_rate.to_bits());
}

#[test]
fn to_data_slice_recomposes_the_range_and_kind() {
    let slice = WireSlice {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string()],
        interval: String::new(),
        start: Some(10),
        end: None,
        kind: WireSliceKind::Ticks,
    };
    let ds = to_data_slice(&slice);
    assert_eq!(ds.venue, "polymarket");
    assert_eq!(ds.symbols, vec!["TKN".to_string()]);
    assert_eq!(ds.range.start, Some(10));
    assert_eq!(ds.range.end, None);
    assert_eq!(ds.kind, SliceKind::Ticks);
}

/// The fixture the `chosen_params` rendering test expands: a ONE-point `[sweep]` grid whose
/// four axes carry the four TOML scalar shapes a real grid produces. Built through the
/// PRODUCTION path (`harness::sweep::expand_paramscan`, the PROFILE door's source of
/// `chosen_params`) rather than by hand — not because this crate cannot name `toml::Value` (it
/// can, and `crate::run`'s searching driver builds one per chosen axis), but because a
/// hand-built `Float` would exercise ONE of the four TOML scalar shapes the rendering has to
/// survive.
const CHOSEN_PARAMS_PROFILE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0

[sweep]
flag = [true]
label = ["fast-lane"]
size = [1.5]
slow = [30]
"#;

/// A FIXED-parameter window — every window this verb serves today — mirrors with no choice
/// recorded, and the two scalar fields still copy bit-for-bit.
#[test]
fn to_wire_wf_window_carries_a_fixed_window_with_no_choice() {
    let w = WfWindow { test_range: (10, 20), oos_return: 0.25, chosen_params: None };
    let wire = to_wire_wf_window(&w);
    assert_eq!(wire.test_range, (10, 20));
    assert_eq!(wire.oos_return.to_bits(), 0.25f64.to_bits());
    assert_eq!(wire.chosen_params, None, "a fixed-parameter walk records no choice");
}

/// An OPTIMIZED window's choice crosses as rendered TOML text, one pair per swept axis, in
/// `expand_paramscan`'s key-sorted order.
///
/// The renderings are PINNED verbatim (`true` / `"fast-lane"` / `1.5` / `30`) because they ARE
/// the wire bytes: a change to any of them changes what `vike_studio::remote`'s `to_wf_window`
/// parses back. The string case is the one worth watching — its TOML quoting is the whole
/// difference between a rendered `String` and a rendered bare value on the way back.
#[test]
fn to_wire_wf_window_renders_each_chosen_value_as_toml_text() {
    let profile =
        BacktestProfile::from_toml_str(CHOSEN_PARAMS_PROFILE).expect("fixture profile parses");
    let points = expand_paramscan(&profile).expect("the one-point grid expands");
    assert_eq!(points.len(), 1, "every axis holds one value, so the grid is one point");

    let w = WfWindow {
        test_range: (0, 50),
        oos_return: -0.01,
        chosen_params: Some(points[0].overrides.clone()),
    };
    assert_eq!(
        to_wire_wf_window(&w).chosen_params,
        Some(vec![
            ("flag".to_string(), "true".to_string()),
            ("label".to_string(), "\"fast-lane\"".to_string()),
            ("size".to_string(), "1.5".to_string()),
            ("slow".to_string(), "30".to_string()),
        ])
    );
}

/// A bar slice over a venue, for the cost-model tests below.
fn slice_of(venue: &str, symbol: &str) -> WireSlice {
    WireSlice {
        venue: venue.to_string(),
        symbols: vec![symbol.to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: WireSliceKind::Bars,
    }
}

/// **The derivation, at the boundary.** With no `fee_rate` on the wire the slice's own lane
/// resolves a real schedule INTO the engine params — the change that stops a Studio run being a
/// zero-cost backtest.
#[test]
fn absent_wire_params_derive_the_slices_own_fee_schedule() {
    let (ep, model) = engine_params_for(&slice_of("binance", "BTCUSDT.P"), None);
    assert!(
        ep.fee_schedule.is_some(),
        "a run with no fee_rate on the wire must be priced by its lane, not by \
             EngineParams::default's ZERO"
    );
    assert_eq!(model.source_token(), "derived");
    assert_eq!(model.lane(), Some("binance-perp"), "the .P symbol selects the perp lane");
}

/// **The PRECEDENCE, at the boundary.** A caller's flat `fee_rate` reaches the engine AND
/// suppresses the derivation. Both halves are load-bearing: `StrategyEngine::new` prefers a
/// schedule over the flat rate, so a schedule installed here would discard the override in
/// silence while the stamp still said `override`.
#[test]
fn an_explicit_fee_rate_overrides_and_no_schedule_is_installed() {
    let params = WireEngineParams { cash: None, fee_rate: Some(0.001), slippage: None };
    let (ep, model) = engine_params_for(&slice_of("binance", "BTCUSDT.P"), Some(&params));
    assert_eq!(ep.fee_rate, 0.001, "the caller's rate reaches the engine");
    assert_eq!(
        ep.fee_schedule, None,
        "an override must install NO schedule — the engine prefers a schedule over fee_rate, \
             so one here would silently discard what the caller asked for"
    );
    assert_eq!(model.source_token(), "override");
}

/// The three sources are DISTINGUISHABLE on the wire, which is the whole point of carrying the
/// source at all — including the case that reads most like an absence and is not.
#[test]
fn the_stamp_separates_derived_from_override_from_none() {
    let derived = engine_params_for(&slice_of("bybit", "BTCUSDT"), None).1;
    let zero_override = engine_params_for(
        &slice_of("bybit", "BTCUSDT"),
        Some(&WireEngineParams { cash: None, fee_rate: Some(0.0), slippage: None }),
    )
    .1;
    let mut mixed = slice_of("binance", "BTCUSDT");
    mixed.symbols.push("ETHUSDT.P".to_string());
    let none = engine_params_for(&mixed, None).1;

    let stamp = |m: &CostModel| to_wire_cost_model(m, FillMix::default(), None);
    assert_eq!(stamp(&derived).source, "derived");
    assert_eq!(stamp(&zero_override).source, "override");
    assert_eq!(stamp(&none).source, "none");
    // …and the zero override is NOT read as "nothing applied": it names a rate, not a reason.
    assert_eq!(stamp(&zero_override).taker_rate, 0.0);
    assert_eq!(stamp(&zero_override).reason, None);
    assert!(stamp(&none).reason.is_some(), "a none stamp always says why");
}

/// The residuals ride the stamp itself rather than living only in the record — a statement that
/// said only what IS modelled would invite a reader to infer that nothing else exists.
#[test]
fn every_stamp_carries_the_declared_residuals() {
    let model = engine_params_for(&slice_of("okx", "BTCUSDT"), None).1;
    let stamp = to_wire_cost_model(&model, FillMix::default(), Some("sharpe"));
    assert_eq!(stamp.not_modelled.len(), NOT_MODELLED.len());
    assert_eq!(stamp.rank_metric.as_deref(), Some("sharpe"));
    // the variant is carried, so a zero pair can never be misread as free trading
    assert_eq!(stamp.variant, "PercentMakerTaker");
}

/// **The SERVER-SIDE re-check.** An unknown method is REFUSED BY NAME, never demoted to the
/// fixed walk — which would answer a different question while looking like success.
#[test]
fn an_unknown_search_method_is_refused_rather_than_run_as_the_fixed_walk() {
    let wf = WireWalkforward::searching(
        4,
        vike_datahub_client::wire_studio::WireWindowSearch {
            method: "bayesian".to_string(),
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0])] },
            rank_by: None,
        },
    );
    let err = to_window_search_plan(&wf).expect_err("an unknown method must be refused");
    assert_eq!(err.kind, "data");
    assert!(err.message.contains("bayesian"), "the refusal quotes what was sent: {err:?}");
    assert!(err.message.contains("none | sweep"), "it names the accepted set: {err:?}");
}

/// The ranking spelling goes through the ENGINE's parser, so this door and the profile door
/// cannot drift; an unknown value is refused here rather than silently defaulting to sharpe.
#[test]
fn an_unknown_rank_by_is_refused() {
    let wf = WireWalkforward::searching(
        4,
        vike_datahub_client::wire_studio::WireWindowSearch {
            method: "sweep".to_string(),
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0])] },
            rank_by: Some("calmar".to_string()),
        },
    );
    let err = to_window_search_plan(&wf).expect_err("an unknown rank_by must be refused");
    assert!(err.message.contains("calmar"), "{err:?}");
}

/// An ABSENT selector is the fixed walk — the control, reached by the same driver, and
/// byte-identical to every frame this verb carried before the field existed.
#[test]
fn an_absent_selector_is_the_no_search_control() {
    let wf = WireWalkforward::fixed(4);
    let plan = to_window_search_plan(&wf).expect("the fixed walk");
    assert!(!plan.search.searches());
    assert!(plan.grid.is_empty());
}

/// …and an explicit `none` resolves to the SAME control, because it is a first-class value
/// rather than an omitted field.
#[test]
fn an_explicit_none_resolves_to_the_same_control() {
    let wf = WireWalkforward::searching(
        4,
        vike_datahub_client::wire_studio::WireWindowSearch {
            method: "NONE".to_string(), // the parse is case-insensitive, like the profile door
            grid: WireParamscan::default(),
            rank_by: None,
        },
    );
    let plan = to_window_search_plan(&wf).expect("the control parses");
    assert!(!plan.search.searches());
}

#[test]
fn run_error_to_wire_classifies_each_variant() {
    assert_eq!(run_error_to_wire(&RunError::Compile("x".into())).kind, "compile");
    assert_eq!(run_error_to_wire(&RunError::Data("x".into())).kind, "data");
    assert_eq!(run_error_to_wire(&RunError::Strategy("x".into())).kind, "strategy");
    // the detail survives
    assert_eq!(run_error_to_wire(&RunError::Data("no bars".into())).message, "no bars");
}
