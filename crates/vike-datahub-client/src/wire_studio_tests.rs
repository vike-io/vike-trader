use super::*;
use serde::de::DeserializeOwned;
use std::fmt::Debug;

/// Serialize `v` to JSON and back, asserting the value survives unchanged — the wire contract
/// every DTO must hold (the fast-lane counterpart of the datahub `frame_round_trips_*` tests).
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: &T) {
    let json = serde_json::to_string(v).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(*v, back, "value must survive a serde round-trip");
}

#[test]
fn wire_spec_rhai_round_trips() {
    round_trip(&WireSpec::Rhai("fn on_bar() {}".to_string()));
}

/// A `WireSpec::Native` carrying its params as TOML TEXT round-trips — the params never become a
/// `toml::Value` on the wire (the `RunBacktest(profile_toml)` idiom).
#[test]
fn wire_spec_native_params_toml_round_trips() {
    let spec = WireSpec::Native {
        name: "buy_hold".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"\n".to_string(),
    };
    round_trip(&spec);
    // and the params text is carried verbatim (not re-encoded)
    let json = serde_json::to_string(&spec).unwrap();
    let back: WireSpec = serde_json::from_str(&json).unwrap();
    match back {
        WireSpec::Native { name, params_toml } => {
            assert_eq!(name, "buy_hold");
            assert!(params_toml.contains("symbol = \"BTCUSDT\""));
        }
        other => panic!("expected Native, got {other:?}"),
    }
}

/// A `WireSpec::Plugin` round-trips, and the sha crosses WHOLE — a truncated one would name a
/// different artifact.
#[test]
fn wire_spec_plugin_round_trips_and_keeps_the_sha_whole() {
    let spec = WireSpec::Plugin {
        name: "my_strat".to_string(),
        sha: "a".repeat(64),
        params_toml: "fast = 5\n".to_string(),
    };
    round_trip(&spec);
    let json = serde_json::to_string(&spec).unwrap();
    let back: WireSpec = serde_json::from_str(&json).unwrap();
    match back {
        WireSpec::Plugin { name, sha, params_toml } => {
            assert_eq!(name, "my_strat");
            assert_eq!(sha.len(), 64);
            assert_eq!(sha, "a".repeat(64));
            assert!(params_toml.contains("fast"));
        }
        other => panic!("expected Plugin, got {other:?}"),
    }
}

#[test]
fn wire_slice_round_trips_both_kinds() {
    round_trip(&WireSlice {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: Some(0),
        end: Some(100_000),
        kind: WireSliceKind::Bars,
    });
    round_trip(&WireSlice {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string(), "TKN2".to_string()],
        interval: String::new(),
        start: None,
        end: None,
        kind: WireSliceKind::Ticks,
    });
}

#[test]
fn wire_engine_params_round_trips_default_and_set() {
    round_trip(&WireEngineParams::default());
    round_trip(&WireEngineParams {
        cash: Some(5000.0),
        fee_rate: Some(0.001),
        slippage: Some(0.0),
    });
}

#[test]
fn wire_run_result_round_trips() {
    round_trip(&WireRunResult {
        equity_curve: vec![1000.0, 1010.5, 995.25],
        equity_ts: vec![1, 2, 3],
        final_equity: 995.25,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
        trades: vec![sample_wire_trade()],
        stale_deferrals: 2,
        session_deferrals: 0,
        cost_model: None,
    });
}

fn sample_wire_trade() -> WireTrade {
    WireTrade {
        entry_price: 100.0,
        exit_price: 95.25,
        size: 1.0,
        pnl: -4.75,
        fees: 0.2,
        entry_ts: 60_000,
        exit_ts: 120_000,
        symbol: "BTCUSDT".to_string(),
        mae: 0.05,
        mfe: 0.02,
        is_long: true,
    }
}

#[test]
fn wire_trade_round_trips_through_serde() {
    round_trip(&sample_wire_trade());
}

/// `Trade -> WireTrade -> Trade` reconstructs the original exactly — the full-mirror losslessness
/// the server relies on when building [`WireRunResult`] from a `BacktestResult`.
#[test]
fn wire_trade_mirrors_vike_model_trade_losslessly() {
    let t = Trade {
        entry_price: 100.0,
        exit_price: 95.25,
        size: 1.5,
        pnl: -7.125,
        fees: 0.3,
        entry_ts: 60_000,
        exit_ts: 120_000,
        symbol: "ETHUSDT".to_string(),
        mae: 0.05,
        mfe: 0.02,
        is_long: false,
    };
    let wire = WireTrade::from_trade(&t);
    assert_eq!(wire.to_trade(), t, "round-trips back to the exact Trade");
}

#[test]
fn wire_run_error_round_trips_and_formats_kind_first() {
    let e = WireRunError::new("compile", "expected `}` at line 1");
    round_trip(&e);
    assert_eq!(e.to_error_string(), "compile: expected `}` at line 1");
}

#[test]
fn wire_sweep_round_trips() {
    round_trip(&WireParamscan::default());
    round_trip(&WireParamscan {
        axes: vec![
            ("fast".to_string(), vec![3.0, 5.0, 8.0]),
            ("slow".to_string(), vec![20.0, 30.0]),
        ],
    });
}

fn sample_wire_run_result() -> WireRunResult {
    WireRunResult {
        equity_curve: vec![1000.0, 1010.5, 995.25],
        equity_ts: vec![1, 2, 3],
        final_equity: 995.25,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
        trades: vec![sample_wire_trade()],
        stale_deferrals: 2,
        session_deferrals: 0,
        cost_model: None,
    }
}

#[test]
fn wire_sweep_entry_round_trips() {
    round_trip(&WireParamscanEntry {
        overrides: vec![("fast".to_string(), 5.0)],
        result: sample_wire_run_result(),
    });
}

/// A sweep result round-trips with `dsr`/`pbo` as BOTH `Some(finite)` and `None` — the `None`
/// case is the load-bearing one ([`WireParamscanResult`]'s doc says why).
#[test]
fn wire_sweep_result_round_trips_including_none_dsr_pbo() {
    round_trip(&WireParamscanResult {
        entries: vec![WireParamscanEntry {
            overrides: vec![("fast".to_string(), 5.0)],
            result: sample_wire_run_result(),
        }],
        dsr: Some(1.25),
        pbo: Some(0.5),
        best_index: 0,
        cost_model: None,
    });
    round_trip(&WireParamscanResult {
        entries: vec![],
        dsr: None,
        pbo: None,
        best_index: 0,
        cost_model: None,
    });
}

#[test]
fn wire_walkforward_round_trips() {
    round_trip(&WireWalkforward::fixed(4));
    round_trip(&WireWalkforward::searching(
        4,
        WireWindowSearch {
            method: "sweep".to_string(),
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
            rank_by: Some("max_dd".to_string()),
        },
    ));
}

/// `needs_capability` is what stands between a client and a daemon that would silently run the
/// FIXED walk. Its two directions are both load-bearing, and the FALSE one is the easier to get
/// wrong: refusing an ordinary fixed walk would break every peer in the field.
#[test]
fn needs_capability_refuses_a_search_and_admits_the_bare_control() {
    let control = WireWindowSearch {
        method: NO_WINDOW_SEARCH.to_string(),
        grid: WireParamscan::default(),
        rank_by: None,
    };
    assert!(!control.needs_capability(), "an explicit control runs correctly on any peer");
    assert!(
        WireWindowSearch { method: "sweep".to_string(), ..control.clone() }.needs_capability(),
        "a sweep an old daemon would DROP must be refused before it is sent"
    );
    assert!(
        WireWindowSearch {
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![1.0])] },
            ..control.clone()
        }
        .needs_capability(),
        "a grid written under the control needs it too — an old daemon drops the whole field \
             and reports success, so the refusal it deserves is one only this side can give"
    );
    assert!(
        WireWindowSearch { rank_by: Some("equity".to_string()), ..control.clone() }
            .needs_capability(),
        "…and so does a rank_by"
    );
    // A method this client has never heard of is the SERVER's to refuse by name — but it is
    // still a search an old daemon would drop, so it does not leave this side.
    assert!(WireWindowSearch { method: "bayesian".to_string(), ..control }.needs_capability());
}

/// The STAMP round-trips, including the two shapes a reader must be able to tell apart: a
/// derived schedule naming its lane, and an overridden flat rate naming none.
#[test]
fn wire_cost_model_round_trips_both_sources() {
    round_trip(&WireCostModel {
        source: "derived".to_string(),
        lane: Some("binance-perp".to_string()),
        variant: "PercentMakerTaker".to_string(),
        maker_rate: 0.0002,
        taker_rate: 0.0005,
        reason: None,
        maker_fills: 0,
        taker_fills: 42,
        fees_paid: 12.5,
        rank_metric: Some("sharpe".to_string()),
        not_modelled: vec!["impact — …".to_string()],
    });
    round_trip(&WireCostModel {
        source: "override".to_string(),
        variant: "none".to_string(),
        maker_rate: 0.001,
        taker_rate: 0.001,
        ..WireCostModel::default()
    });
}

/// The stamp is ADDITIVE (decision 0112, verdict 3): an answer without one does not carry the
/// key, and a frame that omits it — an OLD server answering a NEW client — still decodes.
#[test]
fn an_absent_stamp_is_omitted_from_the_frame_and_an_old_frame_decodes() {
    let unstamped = WireWalkforwardResult {
        windows: vec![],
        oos_equity_curve: vec![],
        oos_return: 0.0,
        oos_sharpe: 0.0,
        wf_consistency: 0.0,
        cost_model: None,
    };
    let json = serde_json::to_string(&unstamped).expect("serialize");
    assert!(!json.contains("cost_model"), "an absent stamp must not reach the frame: {json}");

    let old_frame = r#"{"windows":[],"oos_equity_curve":[],"oos_return":0.0,"oos_sharpe":0.0,"wf_consistency":0.0}"#;
    let decoded: WireWalkforwardResult =
        serde_json::from_str(old_frame).expect("an old frame decodes for a new client");
    assert_eq!(decoded, unstamped);
}

#[test]
fn wire_wf_window_round_trips() {
    // the fixed-parameter walk's shape — every window a walk that did NOT search serves
    let fixed = WireWfWindow { test_range: (100, 200), oos_return: 0.05, chosen_params: None };
    round_trip(&fixed);
    // ...and the optimizing walk's: each chosen value as its rendered TOML text, one pair
    // per swept axis. The quoting on the string is the point — it is what tells the client's
    // parse a `toml::Value::String` from a bare value.
    let chosen = vec![
        ("fast".to_string(), "5".to_string()),
        ("symbol".to_string(), "\"BTCUSDT\"".to_string()),
    ];
    round_trip(&WireWfWindow { chosen_params: Some(chosen), ..fixed });
}

/// The two properties that keep `chosen_params` additive (decision 0112, verdict 3): a `None`
/// choice does not appear in the frame AT ALL (a fixed walk's frame is byte-identical to the
/// pre-field one), and a frame that omits the key — an OLD server answering a NEW client — still
/// decodes, as `None`.
#[test]
fn a_none_choice_is_omitted_from_the_frame_and_an_absent_key_decodes_as_none() {
    let w = WireWfWindow { test_range: (0, 10), oos_return: 0.01, chosen_params: None };
    let json = serde_json::to_string(&w).expect("serialize");
    assert!(
        !json.contains("chosen_params"),
        "a None choice must not reach the frame — that omission is what keeps today's \
             walk-forward frames byte-identical to the pre-field ones: {json}"
    );

    let old_frame = r#"{"test_range":[0,10],"oos_return":0.01}"#;
    let decoded: WireWfWindow = serde_json::from_str(old_frame).expect("deserialize");
    assert_eq!(decoded, w, "an absent key must default to None, not fail the frame");
}

#[test]
fn wire_walkforward_result_round_trips() {
    round_trip(&WireWalkforwardResult {
        windows: vec![
            WireWfWindow { test_range: (0, 100), oos_return: 0.05, chosen_params: None },
            WireWfWindow {
                test_range: (100, 200),
                oos_return: -0.02,
                chosen_params: Some(vec![("fast".to_string(), "8".to_string())]),
            },
        ],
        oos_equity_curve: vec![10_000.0, 10_500.0, 10_290.0],
        oos_return: 0.029,
        oos_sharpe: 1.1,
        wf_consistency: 0.5,
        cost_model: None,
    });
}
