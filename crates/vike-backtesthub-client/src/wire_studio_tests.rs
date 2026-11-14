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
    round_trip(&StrategySpecWire::Rhai("fn on_bar() {}".to_string()));
}

/// A `StrategySpecWire::File` carrying its params as TOML TEXT round-trips — the params never become a
/// `toml::Value` on the wire (the `RunBacktest(profile_toml)` idiom). The retired `Native` still
/// decodes, so its sender can be told the fix.
#[test]
fn wire_spec_file_params_toml_round_trips() {
    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"\n".to_string(),
    };
    round_trip(&spec);
    // and the params text is carried verbatim (not re-encoded)
    let json = serde_json::to_string(&spec).unwrap();
    let back: StrategySpecWire = serde_json::from_str(&json).unwrap();
    match back {
        StrategySpecWire::File { file, params_toml } => {
            assert_eq!(file, "trend/buy_hold.rs");
            assert!(params_toml.contains("symbol = \"BTCUSDT\""));
        }
        other => panic!("expected File, got {other:?}"),
    }
    round_trip(&StrategySpecWire::Native {
        name: "buy_hold".to_string(),
        params_toml: String::new(),
    });
}

/// A `StrategySpecWire::Plugin` round-trips, and the sha crosses WHOLE — a truncated one would name a
/// different artifact.
#[test]
fn wire_spec_strategy_file_round_trips_and_keeps_the_sha_whole() {
    let spec = StrategySpecWire::Plugin {
        name: "my_strat".to_string(),
        sha: "a".repeat(64),
        params_toml: "fast = 5\n".to_string(),
    };
    round_trip(&spec);
    let json = serde_json::to_string(&spec).unwrap();
    let back: StrategySpecWire = serde_json::from_str(&json).unwrap();
    match back {
        StrategySpecWire::Plugin { name, sha, params_toml } => {
            assert_eq!(name, "my_strat");
            assert_eq!(sha.len(), 64);
            assert_eq!(sha, "a".repeat(64));
            assert!(params_toml.contains("fast"));
        }
        other => panic!("expected Strategy file, got {other:?}"),
    }
}

#[test]
fn wire_slice_round_trips_both_kinds() {
    round_trip(&DataSliceWire {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: Some(0),
        end: Some(100_000),
        kind: ReplayKindWire::Bars,
    });
    round_trip(&DataSliceWire {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string(), "TKN2".to_string()],
        interval: String::new(),
        start: None,
        end: None,
        kind: ReplayKindWire::Ticks,
    });
}

#[test]
fn wire_engine_params_round_trips_default_and_set() {
    round_trip(&StrategyEngineConfigWire::default());
    round_trip(&StrategyEngineConfigWire {
        cash: Some(5000.0),
        fee_rate: Some(0.001),
        slippage: Some(0.0),
        ..StrategyEngineConfigWire::default()
    });
    round_trip(&StrategyEngineConfigWire {
        slippage: Some(2.0),
        slippage_model: Some("ticks".to_string()),
        slippage_tick: Some(0.01),
        ..StrategyEngineConfigWire::default()
    });
}

#[test]
fn wire_run_result_round_trips() {
    round_trip(&BacktestResultWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        equity_curve: vec![1000.0, 1010.5, 995.25],
        equity_timestamp: vec![1, 2, 3],
        final_equity: 995.25,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
        trades: vec![sample_trade()],
        stale_deferrals: 2,
        session_deferrals: 0,
        cost_model: None,
        data_warnings: vec!["ETHUSDT: did not run".to_string()],
        funding_paid: -0.5,
        settle_price: Some(66_000.0),
        refused_orders: vec![(
            OrderDrop {
                symbol: "BTCUSDT".to_string(),
                reason: "insufficient-margin".to_string(),
                size: 2.0,
                weight: 1.0,
            },
            3,
        )],
    });
}

/// The four fields are additive both ways: a clean run's frame is the frame an older peer already
/// reads, and an older server's frame decodes to no warnings, no funding, no settle price and no
/// refused order.
#[test]
fn data_warnings_are_absent_from_a_clean_frame_and_an_old_frame_decodes() {
    let clean = serde_json::to_string(&BacktestResultWire::default()).expect("serialize");
    assert!(!clean.contains("data_warnings") && !clean.contains("funding_paid"), "{clean}");
    assert!(!clean.contains("settle_price") && !clean.contains("refused_orders"), "{clean}");

    let decoded: BacktestResultWire = serde_json::from_str(&clean).expect("an old frame decodes");
    assert!(decoded.data_warnings.is_empty());
    assert_eq!(decoded.funding_paid, 0.0);
    assert_eq!(decoded.settle_price, None);
    assert!(decoded.refused_orders.is_empty(), "an old frame refused nothing");
}

fn sample_trade() -> Trade {
    Trade {
        entry_price: 100.0,
        exit_price: 95.25,
        size: 1.0,
        pnl: -4.75,
        fees: 0.2,
        entry_timestamp: 60_000,
        exit_timestamp: 120_000,
        symbol: "BTCUSDT".to_string(),
        mae: 0.05,
        mfe: 0.02,
        is_long: true,
    }
}

/// GOLDEN: a closed trade crosses as the model's own `Trade`, and its serde writes the eleven keys the
/// wire mirror it replaced wrote, in that mirror's order, every one present.
#[test]
fn a_trade_crosses_as_the_eleven_keys_the_mirror_wrote() {
    let json = serde_json::to_string(&sample_trade()).expect("serialize");
    assert_eq!(
        json,
        r#"{"entry_price":100.0,"exit_price":95.25,"size":1.0,"pnl":-4.75,"fees":0.2,"entry_timestamp":60000,"exit_timestamp":120000,"symbol":"BTCUSDT","mae":0.05,"mfe":0.02,"is_long":true}"#
    );
    round_trip(&sample_trade());
}

#[test]
fn wire_sweep_round_trips() {
    round_trip(&ParamscanWire::default());
    round_trip(&ParamscanWire {
        axes: vec![
            (
                "fast".to_string(),
                vec![ParamValue::Float(3.0), ParamValue::Float(5.0), ParamValue::Float(8.0)],
            ),
            ("slow".to_string(), vec![ParamValue::Float(20.0), ParamValue::Float(30.0)]),
        ],
    });
}

fn sample_wire_run_result() -> BacktestResultWire {
    BacktestResultWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        equity_curve: vec![1000.0, 1010.5, 995.25],
        equity_timestamp: vec![1, 2, 3],
        final_equity: 995.25,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
        trades: vec![sample_trade()],
        stale_deferrals: 2,
        session_deferrals: 0,
        cost_model: None,
        data_warnings: Vec::new(),
        funding_paid: 0.0,
        settle_price: None,
        refused_orders: Vec::new(),
    }
}

#[test]
fn wire_sweep_entry_round_trips() {
    round_trip(&ParamscanEntryWire {
        overrides: vec![("fast".to_string(), ParamValue::Float(5.0))],
        result: sample_wire_run_result(),
    });
}

/// A sweep result round-trips with `dsr`/`pbo` as BOTH `Some(finite)` and `None` — the `None`
/// case is the load-bearing one ([`StudioParamscanWire`]'s doc says why).
#[test]
fn wire_sweep_result_round_trips_including_none_dsr_pbo() {
    round_trip(&StudioParamscanWire {
        entries: vec![ParamscanEntryWire {
            overrides: vec![("fast".to_string(), ParamValue::Float(5.0))],
            result: sample_wire_run_result(),
        }],
        dsr: Some(1.25),
        pbo: Some(0.5),
        best_index: 0,
        cost_model: None,
    });
    round_trip(&StudioParamscanWire {
        entries: vec![],
        dsr: None,
        pbo: None,
        best_index: 0,
        cost_model: None,
    });
}

#[test]
fn wire_walkforward_round_trips() {
    round_trip(&WalkforwardWire::fixed(4));
    round_trip(&WalkforwardWire::searching(
        4,
        WalkforwardWindowSearchWire {
            method: "paramscan".to_string(),
            grid: ParamscanWire {
                axes: vec![(
                    "fast".to_string(),
                    vec![ParamValue::Float(3.0), ParamValue::Float(5.0)],
                )],
            },
            rank_by: Some("max_dd".to_string()),
        },
    ));
}

/// `needs_capability` is what stands between a client and a daemon that would silently run the
/// FIXED walk. Its two directions are both load-bearing, and the FALSE one is the easier to get
/// wrong: refusing an ordinary fixed walk would break every peer in the field.
#[test]
fn needs_capability_refuses_a_search_and_admits_the_bare_control() {
    let control = WalkforwardWindowSearchWire {
        method: NO_WINDOW_SEARCH.to_string(),
        grid: ParamscanWire::default(),
        rank_by: None,
    };
    assert!(!control.needs_capability(), "an explicit control runs correctly on any peer");
    assert!(
        WalkforwardWindowSearchWire { method: "paramscan".to_string(), ..control.clone() }
            .needs_capability(),
        "a sweep an old daemon would DROP must be refused before it is sent"
    );
    assert!(
        WalkforwardWindowSearchWire {
            grid: ParamscanWire { axes: vec![("fast".to_string(), vec![ParamValue::Float(1.0)])] },
            ..control.clone()
        }
        .needs_capability(),
        "a grid written under the control needs it too — an old daemon drops the whole field \
             and reports success, so the refusal it deserves is one only this side can give"
    );
    assert!(
        WalkforwardWindowSearchWire { rank_by: Some("equity".to_string()), ..control.clone() }
            .needs_capability(),
        "…and so does a rank_by"
    );
    // A method this client has never heard of is the SERVER's to refuse by name — but it is
    // still a search an old daemon would drop, so it does not leave this side.
    assert!(
        WalkforwardWindowSearchWire { method: "bayesian".to_string(), ..control }
            .needs_capability()
    );
}

/// The STAMP round-trips, including the two shapes a reader must be able to tell apart: a
/// derived schedule naming its lane, and an overridden flat rate naming none.
#[test]
fn wire_cost_model_round_trips_both_sources() {
    round_trip(&CostModelStampWire {
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
    round_trip(&CostModelStampWire {
        source: "override".to_string(),
        variant: "none".to_string(),
        maker_rate: 0.001,
        taker_rate: 0.001,
        ..CostModelStampWire::default()
    });
}

/// The stamp is ADDITIVE (decision 0112, verdict 3): an answer without one does not carry the
/// key, and a frame that omits it — an OLD server answering a NEW client — still decodes.
#[test]
fn an_absent_stamp_is_omitted_from_the_frame_and_an_old_frame_decodes() {
    let unstamped = WalkforwardReportWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        windows: vec![],
        out_of_sample_equity_curve: vec![],
        out_of_sample_return: 0.0,
        out_of_sample_sharpe: 0.0,
        walkforward_consistency: 0.0,
        cost_model: None,
        data_warnings: Vec::new(),
        funding_paid: 0.0,
        account_currency: None,
    };
    let json = serde_json::to_string(&unstamped).expect("serialize");
    assert!(!json.contains("cost_model"), "an absent stamp must not reach the frame: {json}");
    assert!(!json.contains("account_currency"), "a dollar walk's frame keeps its bytes: {json}");

    let old_frame = r#"{"windows":[],"out_of_sample_equity_curve":[],"out_of_sample_return":0.0,"out_of_sample_sharpe":0.0,"walkforward_consistency":0.0}"#;
    let decoded: WalkforwardReportWire =
        serde_json::from_str(old_frame).expect("an old frame decodes for a new client");
    assert_eq!(decoded, unstamped);
}

#[test]
fn wire_walkforward_window_round_trips() {
    // the fixed-parameter walk's shape — every window a walk that did NOT search serves
    let fixed = WalkforwardWindow {
        test_range: (100, 200),
        out_of_sample_return: 0.05,
        chosen_params: None,
    };
    round_trip(&fixed);
    // ...and the optimizing walk's: each chosen value as the `ParamValue` the window chose, one
    // pair per swept axis, every kind keeping its kind.
    let chosen = vec![
        ("fast".to_string(), ParamValue::Int(5)),
        ("rate".to_string(), ParamValue::Float(5.0)),
        ("symbol".to_string(), ParamValue::Str("BTCUSDT".to_string())),
    ];
    round_trip(&WalkforwardWindow { chosen_params: Some(chosen), ..fixed });
}

/// GOLDEN: a window's chosen values are written as the JSON of their kind — `5` for an `Int`, `5.0`
/// for a `Float`, a quoted string for a `Str` — and read back as that kind.
#[test]
fn a_chosen_value_crosses_as_its_kind() {
    let w = WalkforwardWindow {
        test_range: (0, 10),
        out_of_sample_return: 0.5,
        chosen_params: Some(vec![
            ("fast".to_string(), ParamValue::Int(5)),
            ("rate".to_string(), ParamValue::Float(5.0)),
            ("symbol".to_string(), ParamValue::Str("BTCUSDT".to_string())),
        ]),
    };
    let json = serde_json::to_string(&w).expect("serialize");
    assert_eq!(
        json,
        r#"{"test_range":[0,10],"out_of_sample_return":0.5,"chosen_params":[["fast",5],["rate",5.0],["symbol","BTCUSDT"]]}"#
    );
    let back: WalkforwardWindow = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, w);
}

/// GOLDEN, the wire-compatibility claim of this module's doc: a `Float` axis is written exactly as
/// the `f64` axis was (`5.0`), an `Int` axis as an integer (`5`); the frame an `f64` client wrote
/// reads back as `Float`, an integer frame as `Int`; and a reader that still holds the OLD `f64`
/// shape accepts both new frames.
#[test]
fn a_sweep_axis_keeps_its_kind_and_the_f64_bytes() {
    let float_axis = ParamscanWire {
        axes: vec![("size".to_string(), vec![ParamValue::Float(5.0), ParamValue::Float(10.0)])],
    };
    let int_axis = ParamscanWire {
        axes: vec![("rungs".to_string(), vec![ParamValue::Int(5), ParamValue::Int(10)])],
    };
    let float_json = serde_json::to_string(&float_axis).expect("serialize");
    let int_json = serde_json::to_string(&int_axis).expect("serialize");
    assert_eq!(float_json, r#"{"axes":[["size",[5.0,10.0]]]}"#);
    assert_eq!(int_json, r#"{"axes":[["rungs",[5,10]]]}"#);

    // The bytes the OLD `Vec<f64>` axis wrote are the `Float` axis's bytes...
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct OldParamscan {
        axes: Vec<(String, Vec<f64>)>,
    }
    let old = OldParamscan { axes: vec![("size".to_string(), vec![5.0, 10.0])] };
    assert_eq!(serde_json::to_string(&old).expect("serialize"), float_json);
    // ...and they read back as `Float`, an integer frame as `Int`.
    let read: ParamscanWire = serde_json::from_str(r#"{"axes":[["size",[5.0,10.0]]]}"#).unwrap();
    assert_eq!(read, float_axis);
    let read: ParamscanWire = serde_json::from_str(r#"{"axes":[["rungs",[5,10]]]}"#).unwrap();
    assert_eq!(read, int_axis);
    // An `f64` reader (a peer that predates this type) accepts BOTH new frames.
    let old_reads_int: OldParamscan = serde_json::from_str(&int_json).expect("f64 reads 5");
    assert_eq!(old_reads_int.axes, vec![("rungs".to_string(), vec![5.0, 10.0])]);
    let old_reads_float: OldParamscan = serde_json::from_str(&float_json).expect("f64 reads 5.0");
    assert_eq!(old_reads_float, old);
}

/// GOLDEN: a sweep point's overrides are written as the JSON of their kind, and an `f64` reader
/// accepts them.
#[test]
fn a_sweep_entry_override_keeps_its_kind() {
    let entry = ParamscanEntryWire {
        overrides: vec![
            ("rungs".to_string(), ParamValue::Int(6)),
            ("size".to_string(), ParamValue::Float(2.0)),
        ],
        result: BacktestResultWire::default(),
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.starts_with(r#"{"overrides":[["rungs",6],["size",2.0]],"#), "{json}");
    let back: ParamscanEntryWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, entry);

    #[derive(Deserialize)]
    struct OldEntry {
        overrides: Vec<(String, f64)>,
    }
    let old: OldEntry = serde_json::from_str(&json).expect("an f64 reader accepts the new frame");
    assert_eq!(old.overrides, vec![("rungs".to_string(), 6.0), ("size".to_string(), 2.0)]);
}

/// The two properties that keep `chosen_params` additive (decision 0112, verdict 3): a `None`
/// choice does not appear in the frame AT ALL (a fixed walk's frame is byte-identical to the
/// pre-field one), and a frame that omits the key — an OLD server answering a NEW client — still
/// decodes, as `None`.
#[test]
fn a_none_choice_is_omitted_from_the_frame_and_an_absent_key_decodes_as_none() {
    let w =
        WalkforwardWindow { test_range: (0, 10), out_of_sample_return: 0.01, chosen_params: None };
    let json = serde_json::to_string(&w).expect("serialize");
    assert!(
        !json.contains("chosen_params"),
        "a None choice must not reach the frame — that omission is what keeps today's \
             walk-forward frames byte-identical to the pre-field ones: {json}"
    );

    let old_frame = r#"{"test_range":[0,10],"out_of_sample_return":0.01}"#;
    let decoded: WalkforwardWindow = serde_json::from_str(old_frame).expect("deserialize");
    assert_eq!(decoded, w, "an absent key must default to None, not fail the frame");
}

#[test]
fn wire_walkforward_result_round_trips() {
    round_trip(&WalkforwardReportWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        windows: vec![
            WalkforwardWindow {
                test_range: (0, 100),
                out_of_sample_return: 0.05,
                chosen_params: None,
            },
            WalkforwardWindow {
                test_range: (100, 200),
                out_of_sample_return: -0.02,
                chosen_params: Some(vec![("fast".to_string(), ParamValue::Int(8))]),
            },
        ],
        out_of_sample_equity_curve: vec![10_000.0, 10_500.0, 10_290.0],
        out_of_sample_return: 0.029,
        out_of_sample_sharpe: 1.1,
        walkforward_consistency: 0.5,
        cost_model: None,
        data_warnings: vec!["BTCUSDT.P: no funding history".to_string()],
        funding_paid: -0.25,
        account_currency: Some("JPY".to_string()),
    });
}
