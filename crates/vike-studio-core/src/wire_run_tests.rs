use super::*;
// One PRODUCTION source of `chosen_params` values: a `[sweep]` grid expanded into points, which
// is where the PROFILE door gets its own. The harness path is kept here because these four axes
// carry four scalar KINDS a hand-built `Float` could not exercise — a DEV edge onto the harness,
// which this crate no longer links normally (2026-11-10).
use vike_analytics::results::TradingResults;
use vike_analytics::walkforward::{WalkforwardResultsStep, WalkforwardWindowTimes};
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::paramscan::expand_paramscan;
use vike_model::params::ParamValue;

#[test]
fn to_data_slice_recomposes_the_range_and_kind() {
    let slice = DataSliceWire {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string()],
        interval: String::new(),
        start: Some(10),
        end: None,
        kind: ReplayKindWire::Ticks,
    };
    let data_slice = to_data_slice(&slice);
    assert_eq!(data_slice.venue, "polymarket");
    assert_eq!(data_slice.symbols, vec!["TKN".to_string()]);
    assert_eq!(data_slice.range.start, Some(10));
    assert_eq!(data_slice.range.end, None);
    assert_eq!(data_slice.kind, ReplayKind::Tick);
}

/// The fixture the `chosen_params` window test expands: a ONE-point `[sweep]` grid whose four axes
/// carry the four TOML scalar kinds a real grid produces. Built through the PRODUCTION path
/// (`search::paramscan::expand_paramscan`, the PROFILE door's source of `chosen_params`) rather than
/// by hand, because a hand-built `Float` would exercise ONE of the four kinds the wire has to keep.
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
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0

[paramscan]
flag = [true]
label = ["fast-lane"]
size = [1.5]
slow = [30]
"#;

/// The results of a two-sample curve from 1,000 to `final_equity`, as a step or a walk reports them.
fn results_ending_at(final_equity: f64) -> TradingResults {
    let record = vike_analytics::TradingRecord {
        equity_curve: vec![1000.0, final_equity],
        final_equity,
        ..Default::default()
    };
    TradingResults::from_record(None, &record, 252.0)
}

/// A walk's steps cross AS THEMSELVES: a FIXED step with no choice recorded and its results
/// bit-for-bit, and an OPTIMIZED step's choice as the `ParamValue`s it chose, one pair per swept
/// axis, in `expand_paramscan`'s key-sorted order, each keeping its kind: `Bool`, `Str`, `Float`, `Int`.
#[test]
fn a_walks_steps_cross_whole_each_chosen_value_as_its_kind() {
    let profile =
        ProfileBacktest::from_toml_str(CHOSEN_PARAMS_PROFILE).expect("fixture profile parses");
    let points = expand_paramscan(&profile).expect("the one-point grid expands");
    assert_eq!(points.len(), 1, "every axis holds one value, so the grid is one point");

    let report = WalkforwardResults {
        steps: vec![
            WalkforwardResultsStep {
                test_range: (10, 20),
                window_times: None,
                chosen_params: None,
                results: results_ending_at(1250.0),
            },
            WalkforwardResultsStep {
                test_range: (0, 50),
                window_times: None,
                chosen_params: Some(points[0].overrides.clone()),
                results: results_ending_at(990.0),
            },
        ],
        overall: results_ending_at(1237.5),
        equity_curve: Vec::new(),
        equity_timestamp: Vec::new(),
        profitable_step_share: 0.5,
        account_currency: None,
    };
    let wire = to_wire_walkforward_result(&report);
    assert_eq!(wire.steps, report.steps, "the steps cross unchanged");
    assert_eq!(wire.steps[0].results.total_return.to_bits(), 0.25f64.to_bits());
    assert_eq!(wire.steps[0].chosen_params, None, "a fixed-parameter walk records no choice");
    assert_eq!(
        wire.steps[1].chosen_params,
        Some(vec![
            ("flag".to_string(), ParamValue::Bool(true)),
            ("label".to_string(), ParamValue::Str("fast-lane".to_string())),
            ("size".to_string(), ParamValue::Float(1.5)),
            ("slow".to_string(), ParamValue::Int(30)),
        ])
    );
}

/// The line a failure crosses as is KIND FIRST, the detail whole after it.
#[test]
fn a_run_errors_wire_line_names_its_class_first() {
    assert_eq!(RunError::Compile("x".into()).wire_line(), "compile: x");
    assert_eq!(RunError::Data("no bars".into()).wire_line(), "data: no bars");
    assert_eq!(RunError::Strategy("x".into()).wire_line(), "strategy: x");
}

// --- the CLIENT halves, each proved against the server half beside it ---------------------------

/// Every class the server writes comes back as itself; a line with no known kind (a transport or
/// desync line) is a `Data` error carrying the whole line.
#[test]
fn run_error_from_wire_inverts_the_wire_line() {
    for e in [
        RunError::Compile("bad".into()),
        RunError::Data("no bars".into()),
        RunError::Strategy("nope".into()),
    ] {
        let back = run_error_from_wire(e.wire_line());
        assert_eq!(back.to_string(), e.to_string());
    }
    for line in ["protocol desync: x", "bare"] {
        match run_error_from_wire(line.to_string()) {
            RunError::Data(m) => assert_eq!(m, line),
            other => panic!("expected Data, got {other:?}"),
        }
    }
}

#[test]
fn to_wire_spec_maps_rhai_verbatim() {
    let w = to_wire_spec(&StrategySpec::rhai("fn on_bar() {}")).unwrap();
    assert_eq!(w, StrategySpecWire::Rhai("fn on_bar() {}".to_string()));
}

/// A Rhai buffer's parameters go out as TOML TEXT and come back through the server's [`to_strategy_spec`] as the
/// same table, each value its kind; a table that does not parse is a strategy error, never a panic.
#[test]
fn a_rhai_buffers_parameters_round_trip_through_the_wire() {
    let mut parameters = vike_model::params::Params::new();
    parameters.insert("fast", ParamValue::Int(5));
    parameters.insert("commission", ParamValue::Float(0.001));
    let spec =
        StrategySpec::RhaiWithParameters { source: "fn on_bar() {}".to_string(), parameters };
    let w = to_wire_spec(&spec).expect("the table serializes");
    assert!(
        matches!(&w, StrategySpecWire::RhaiWithParameters { parameters_toml, .. }
            if parameters_toml.contains("fast = 5")),
        "{w:?}"
    );
    assert_eq!(to_strategy_spec(&w).expect("converts back"), spec);
    assert_eq!(spec.label(), "rhai");

    let broken = StrategySpecWire::RhaiWithParameters {
        source: String::new(),
        parameters_toml: "fast = ".to_string(),
    };
    assert!(matches!(to_strategy_spec(&broken), Err(RunError::Strategy(_))));
}

/// An EMPTY table is the bare buffer's run, both ways (the review of 2026-11-20): it goes out as `Rhai`, which any
/// daemon serves and whose saved-answer key is the bare run's, and empty TOML text builds the bare spec.
#[test]
fn a_rhai_buffer_with_an_empty_table_stays_bare_rhai() {
    let source = "fn on_bar() {}";
    let empty = StrategySpec::RhaiWithParameters {
        source: source.to_string(),
        parameters: vike_model::params::Params::new(),
    };
    assert_eq!(to_wire_spec(&empty).unwrap(), StrategySpecWire::Rhai(source.to_string()));
    assert_eq!(
        StrategySpec::rhai_from_toml_str(source, "  \n").unwrap(),
        StrategySpec::rhai(source)
    );
    let wire = StrategySpecWire::RhaiWithParameters {
        source: source.to_string(),
        parameters_toml: String::new(),
    };
    assert_eq!(to_strategy_spec(&wire).unwrap(), StrategySpec::rhai(source));
}

/// A file and a built strategy file spec go out as TOML TEXT and come back through the SERVER's own
/// [`to_strategy_spec`], the sha whole: a client encode and a server decode proven against each
/// other.
#[test]
fn a_file_and_a_strategy_file_spec_round_trip_through_the_wire() {
    let native = StrategySpec::file(
        "trend/buy_hold.rs",
        crate::params_from_rows(&[
            ("size".to_string(), "2".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
        ]),
    );
    assert_eq!(to_strategy_spec(&to_wire_spec(&native).unwrap()).unwrap(), native);
    // The SDK a run names crosses with the file, and only a file carries one.
    let pinned = native.clone().with_sdk(Some(4));
    assert!(matches!(to_wire_spec(&pinned).unwrap(), StrategySpecWire::File { sdk: Some(4), .. }));
    assert_eq!(to_strategy_spec(&to_wire_spec(&pinned).unwrap()).unwrap(), pinned);
    assert_eq!(StrategySpec::rhai("x").with_sdk(Some(4)), StrategySpec::rhai("x"));

    let mut params = vike_model::params::Params::new();
    params.insert("fast", ParamValue::Int(5));
    let strategy_file =
        StrategySpec::StrategyFile { name: "my_strat".to_string(), sha: "a".repeat(64), params };
    let w = to_wire_spec(&strategy_file).expect("strategy file spec converts");
    assert!(matches!(&w, StrategySpecWire::Plugin { sha, .. } if sha.len() == 64), "{w:?}");
    assert_eq!(to_strategy_spec(&w).expect("converts back"), strategy_file);
}

#[test]
fn a_slice_round_trips_through_the_wire() {
    let bars = DataSlice::bars(
        "binance",
        "BTCUSDT",
        "1m",
        TimestampRange { start: Some(1), end: Some(9) },
    );
    assert_eq!(to_data_slice(&to_wire_slice(&bars)), bars);
    let ticks = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TimestampRange::all());
    assert_eq!(to_data_slice(&to_wire_slice(&ticks)), ticks);
}

/// A sweep crosses whole: entries, overrides as their kinds, and a non-finite score back to `NaN`.
#[test]
fn a_sweep_round_trips_through_the_wire() {
    let sweep = StudioParamscan {
        entries: vec![ParamscanEntry {
            overrides: vec![("fast".to_string(), ParamValue::Int(5))],
            result: vike_analytics::TradingRecord {
                equity_curve: vec![100.0, 104.0],
                final_equity: 104.0,
                ..Default::default()
            },
        }],
        dsr: f64::NAN,
        pbo: 0.25,
        best_index: 0,
    };
    let back = from_wire_paramscan_result(to_wire_paramscan_result(&sweep));
    assert_eq!(back.entries.len(), 1);
    assert_eq!(back.entries[0].overrides, sweep.entries[0].overrides);
    assert_eq!(back.entries[0].result.equity_curve, sweep.entries[0].result.equity_curve);
    assert_eq!(back.entries[0].result.final_equity, 104.0);
    assert!(back.dsr.is_nan(), "a non-finite dsr crosses as None and comes back NaN");
    assert_eq!(back.pbo, 0.25);
    assert_eq!(back.best_index, 0);
}

/// A walk-forward crosses whole — through the JSON frame, so every kind a chosen value can be keeps
/// its kind (`2.0` stays a `Float`, `30` an `Int`).
#[test]
fn a_walk_forward_round_trips_through_the_wire() {
    let chosen: Vec<(String, ParamValue)> = vec![
        ("slow".to_string(), ParamValue::Int(30)),
        ("whole".to_string(), ParamValue::Float(2.0)),
        ("symbol".to_string(), ParamValue::Str("BTCUSDT".to_string())),
        ("flag".to_string(), ParamValue::Bool(true)),
        ("levels".to_string(), ParamValue::List(vec![ParamValue::Int(1), ParamValue::Int(2)])),
    ];
    let mut overall = results_ending_at(1029.0);
    overall.funding_paid = -0.25;
    overall.data_warnings = vec!["BTCUSDT.P: no funding history".to_string()];
    let report = WalkforwardResults {
        steps: vec![
            WalkforwardResultsStep {
                test_range: (0, 100),
                window_times: None,
                chosen_params: None,
                results: results_ending_at(1050.0),
            },
            WalkforwardResultsStep {
                test_range: (100, 200),
                window_times: Some(WalkforwardWindowTimes {
                    train_start: Some(0),
                    train_end: Some(99 * 60_000),
                    test_start: 100 * 60_000,
                    test_end: 199 * 60_000,
                }),
                chosen_params: Some(chosen),
                results: results_ending_at(980.0),
            },
        ],
        overall,
        equity_curve: vec![10_000.0, 10_500.0],
        equity_timestamp: vec![100 * 60_000, 199 * 60_000],
        profitable_step_share: 0.5,
        account_currency: Some("JPY".to_string()),
    };
    let frame = serde_json::to_string(&to_wire_walkforward_result(&report)).expect("serialize");
    let back = from_wire_walkforward_result(serde_json::from_str(&frame).expect("deserialize"));
    assert_eq!(back.steps.len(), 2);
    for (b, r) in back.steps.iter().zip(&report.steps) {
        assert_eq!(b.test_range, r.test_range);
        assert_eq!(b.results.total_return.to_bits(), r.results.total_return.to_bits());
        assert_eq!(b.chosen_params, r.chosen_params, "every kind survives the wire: {frame}");
        assert_eq!(b.window_times, r.window_times, "a window's times survive the wire: {frame}");
    }
    assert_eq!(back.equity_curve, report.equity_curve);
    // The stitched times convert in memory and never cross: the whole answer is one frame.
    assert_eq!(to_wire_walkforward_result(&report).equity_timestamp, report.equity_timestamp);
    assert!(back.equity_timestamp.is_empty() && !frame.contains("equity_timestamp"), "{frame}");
    assert_eq!(back.profitable_step_share, 0.5);
    assert_eq!(back.overall.funding_paid, -0.25);
    assert_eq!(back.overall.sharpe.to_bits(), report.overall.sharpe.to_bits());
    assert_eq!(back.overall.data_warnings, report.overall.data_warnings);
    assert_eq!(back.account_currency.as_deref(), Some("JPY"), "a yen walk is not read as dollars");
}
