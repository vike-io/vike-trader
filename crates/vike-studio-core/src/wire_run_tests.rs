use super::*;
// One PRODUCTION source of `chosen_params` values: a `[sweep]` grid expanded into points, which
// is where the PROFILE door gets its own. The harness path is kept here because these four axes
// carry four scalar KINDS a hand-built `Float` could not exercise — a DEV edge onto the harness,
// which this crate no longer links normally (2026-11-10).
use vike_analytics::result::WalkforwardWindow;
use vike_backtest::harness::{ProfileBacktest, expand_paramscan};
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
/// (`harness::paramscan::expand_paramscan`, the PROFILE door's source of `chosen_params`) rather than
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

/// A walk's windows cross AS THEMSELVES: a FIXED window with no choice recorded and its two scalars
/// bit-for-bit, and an OPTIMIZED window's choice as the `ParamValue`s it chose, one pair per swept
/// axis, in `expand_paramscan`'s key-sorted order, each keeping its kind: `Bool`, `Str`, `Float`, `Int`.
#[test]
fn a_walks_windows_cross_whole_each_chosen_value_as_its_kind() {
    let profile =
        ProfileBacktest::from_toml_str(CHOSEN_PARAMS_PROFILE).expect("fixture profile parses");
    let points = expand_paramscan(&profile).expect("the one-point grid expands");
    assert_eq!(points.len(), 1, "every axis holds one value, so the grid is one point");

    let report = WalkforwardReport {
        windows: vec![
            WalkforwardWindow {
                test_range: (10, 20),
                out_of_sample_return: 0.25,
                chosen_params: None,
            },
            WalkforwardWindow {
                test_range: (0, 50),
                out_of_sample_return: -0.01,
                chosen_params: Some(points[0].overrides.clone()),
            },
        ],
        out_of_sample_equity_curve: Vec::new(),
        out_of_sample_return: 0.0,
        out_of_sample_sharpe: 0.0,
        walkforward_consistency: 0.0,
        funding_paid: 0.0,
        account_currency: None,
        data_warnings: Vec::new(),
        strategy_file: String::new(),
        strategy_sha256: String::new(),
    };
    let wire = to_wire_walkforward_result(&report);
    assert_eq!(wire.windows, report.windows, "the windows cross unchanged");
    assert_eq!(wire.windows[0].out_of_sample_return.to_bits(), 0.25f64.to_bits());
    assert_eq!(wire.windows[0].chosen_params, None, "a fixed-parameter walk records no choice");
    assert_eq!(
        wire.windows[1].chosen_params,
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
            result: vike_analytics::BacktestResult {
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
    let report = WalkforwardReport {
        windows: vec![
            WalkforwardWindow {
                test_range: (0, 100),
                out_of_sample_return: 0.05,
                chosen_params: None,
            },
            WalkforwardWindow {
                test_range: (100, 200),
                out_of_sample_return: -0.02,
                chosen_params: Some(chosen),
            },
        ],
        out_of_sample_equity_curve: vec![10_000.0, 10_500.0],
        out_of_sample_return: 0.05,
        out_of_sample_sharpe: 1.2,
        walkforward_consistency: 0.5,
        funding_paid: -0.25,
        account_currency: Some("JPY".to_string()),
        data_warnings: vec!["BTCUSDT.P: no funding history".to_string()],
        strategy_file: String::new(),
        strategy_sha256: String::new(),
    };
    let frame = serde_json::to_string(&to_wire_walkforward_result(&report)).expect("serialize");
    let back = from_wire_walkforward_result(serde_json::from_str(&frame).expect("deserialize"));
    assert_eq!(back.windows.len(), 2);
    for (b, r) in back.windows.iter().zip(&report.windows) {
        assert_eq!((b.test_range, b.out_of_sample_return), (r.test_range, r.out_of_sample_return));
        assert_eq!(b.chosen_params, r.chosen_params, "every kind survives the wire: {frame}");
    }
    assert_eq!(back.out_of_sample_equity_curve, report.out_of_sample_equity_curve);
    assert_eq!(
        (
            back.out_of_sample_return,
            back.out_of_sample_sharpe,
            back.walkforward_consistency,
            back.funding_paid
        ),
        (0.05, 1.2, 0.5, -0.25)
    );
    assert_eq!(back.data_warnings, report.data_warnings);
    assert_eq!(back.account_currency.as_deref(), Some("JPY"), "a yen walk is not read as dollars");
}
