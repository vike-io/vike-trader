use std::assert_matches;
use std::io::Cursor;

use super::*;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_model::params::ParamValue;
use vike_model::run_request::search_methods::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS};

use crate::wire_studio::ReplayKindWire;

/// The `RunSlice` request and `RunResult` response survive `write_frame` -> `read_frame`
/// over an in-memory buffer — including the boxed `slice` field (serde-transparent) and the
/// embedded [`StrategySpecWire`]/[`StrategyEngineConfigWire`]/[`TradingRecordWire`] DTOs.
#[test]
fn run_slice_and_run_result_survive_the_frame_codec() {
    let request = Request::RunSlice {
        spec: StrategySpecWire::File {
            file: "trend/buy_hold.rs".to_string(),
            params_toml: "size = 1.0\n".to_string(),
            sdk: None,
        },
        slice: Box::new(DataSliceWire {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: Some(100_000),
            kind: ReplayKindWire::Bars,
        }),
        params: Some(StrategyEngineConfigWire {
            cash: Some(5000.0),
            ..StrategyEngineConfigWire::default()
        }),
    };
    let result = TradingRecordWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        equity_curve: vec![1000.0, 1010.0],
        equity_timestamp: vec![1, 2],
        final_equity: 1010.0,
        n_trades: 0,
        per_symbol_pnl: vec![],
        trades: vec![],
        stale_deferrals: 0,
        session_deferrals: 0,
        cost_model: None,
        data_warnings: Vec::new(),
        funding_paid: 0.0,
        settle_price: None,
        refused_orders: Vec::new(),
    };

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &request).unwrap();
    write_frame(&mut buffer, &Response::RunResult(result.clone())).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunSlice { spec, slice, params } => {
            assert_matches!(spec, StrategySpecWire::File { .. });
            assert_eq!(slice.venue, "binance"); // the box round-trips as a plain DataSliceWire
            assert_eq!(slice.end, Some(100_000));
            assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
        }
        other => panic!("expected RunSlice, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::RunResult(got) => assert_eq!(got, result),
        other => panic!("expected RunResult, got {other:?}"),
    }
}

/// The v6 `RunSweep` / `RunWalkforward` requests carry the OPTIONAL `params` cost/cash field and
/// survive `write_frame` -> `read_frame` unchanged (including the `Some(StrategyEngineConfigWire)`).
#[test]
fn run_sweep_and_walkforward_carry_engine_params_over_the_frame_codec() {
    let slice = || {
        Box::new(DataSliceWire {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: ReplayKindWire::Bars,
        })
    };
    let sweep = Request::RunParamscan {
        spec: StrategySpecWire::Rhai("fn on_bar() {}".to_string()),
        slice: slice(),
        paramscan: ParamscanWire {
            axes: vec![("fast".to_string(), vec![ParamValue::Float(3.0), ParamValue::Float(5.0)])],
        },
        params: Some(StrategyEngineConfigWire {
            cash: Some(5000.0),
            fee_rate: Some(0.001),
            ..StrategyEngineConfigWire::default()
        }),
    };
    let walkforward_value = Request::RunWalkforward {
        spec: StrategySpecWire::Rhai("fn on_bar() {}".to_string()),
        slice: slice(),
        walkforward: WalkforwardWire::fixed(4),
        params: None,
    };

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &sweep).unwrap();
    write_frame(&mut buffer, &walkforward_value).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunParamscan { params, .. } => {
            assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
        }
        other => panic!("expected RunSweep, got {other:?}"),
    }
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunWalkforward { params, .. } => assert!(params.is_none()),
        other => panic!("expected RunWalkforward, got {other:?}"),
    }
}

/// A pre-v6 frame that OMITS `params` still decodes — `#[serde(default)]` maps the absent field
/// to `None`, so the field is backward-compatible on the wire.
#[test]
fn run_sweep_without_params_field_decodes_as_none() {
    // A RunSweep body written WITHOUT the `params` key (the pre-v6 shape).
    let body = br#"{"RunParamscan":{"spec":{"Rhai":"fn on_bar() {}"},"slice":{"venue":"binance","symbols":["BTCUSDT"],"interval":"1m","start":null,"end":null,"kind":"Bars"},"paramscan":{"axes":[]}}}"#;
    let req: Request = serde_json::from_slice(body).expect("pre-v6 frame must still decode");
    match req {
        Request::RunParamscan { params, .. } => {
            assert!(params.is_none(), "absent params -> None")
        }
        other => panic!("expected RunSweep, got {other:?}"),
    }
}

/// The v7 PROFILE-shaped sweep / walk-forward verbs and their JSON-text answers survive
/// `write_frame` -> `read_frame` — the profile TOML crosses VERBATIM (byte-for-byte the text the
/// client read off disk), which is the whole point of the verb.
#[test]
fn profile_sweep_and_walkforward_survive_the_frame_codec() {
    const PROFILE: &str = "[data]\nvenue = \"binance\"\n\n[paramscan]\nfast = [5, 10]\n";

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(
        &mut buffer,
        &Request::RunParamscanProfile {
            profile_toml: PROFILE.to_string(),
            rank_by: Some("max_dd".to_string()),
            search: None,
        },
    )
    .unwrap();
    write_frame(&mut buffer, &Request::RunWalkforwardProfile { profile_toml: PROFILE.to_string() })
        .unwrap();
    write_frame(&mut buffer, &Response::ParamscanReport("{\"rows\":[]}".to_string())).unwrap();
    write_frame(&mut buffer, &Response::WalkforwardReport("{\"windows\":[]}".to_string())).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunParamscanProfile { profile_toml, rank_by, search } => {
            assert_eq!(profile_toml, PROFILE, "the profile TOML crosses verbatim");
            assert_eq!(rank_by.as_deref(), Some("max_dd"));
            assert!(search.is_none(), "an unselected search is absent, not an empty struct");
        }
        other => panic!("expected RunParamscanProfile, got {other:?}"),
    }
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunWalkforwardProfile { profile_toml } => assert_eq!(profile_toml, PROFILE),
        other => panic!("expected RunWalkforwardProfile, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::ParamscanReport(json) => assert_eq!(json, "{\"rows\":[]}"),
        other => panic!("expected ParamscanReport, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::WalkforwardReport(json) => assert_eq!(json, "{\"windows\":[]}"),
        other => panic!("expected WalkforwardReport, got {other:?}"),
    }
}

/// A v7 frame that OMITS `search` still decodes — the `#[serde(default)]` contract the v6
/// `params` field and the `rank_by` field both already have. This is the OLD-CLIENT /
/// NEW-SERVER direction, and it is the half a version bump would have broken.
#[test]
fn profile_sweep_without_search_decodes_as_none() {
    let body = br#"{"RunParamscanProfile":{"profile_toml":"[data]\n"}}"#;
    match serde_json::from_slice::<Request>(body).expect("must decode without search") {
        Request::RunParamscanProfile { search, rank_by, .. } => {
            assert!(search.is_none(), "absent search -> None");
            assert!(rank_by.is_none(), "absent rank_by is unchanged");
        }
        other => panic!("expected RunParamscanProfile, got {other:?}"),
    }
}

/// The selector survives `write_frame` -> `read_frame` with every field populated, and the
/// values cross as the TOKENS the operator typed — the server's parser is the one authority
/// for what `"128"` means, so nothing is re-typed on the way.
#[test]
fn a_search_selector_survives_the_frame_codec() {
    let search = SearchSelectionWire {
        optimizer: Some("tpe".to_string()),
        euler_depth: None,
        trials: Some("128".to_string()),
        seed: Some("7".to_string()),
    };
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(
        &mut buffer,
        &Request::RunParamscanProfile {
            profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
            rank_by: Some("multi".to_string()),
            search: Some(search.clone()),
        },
    )
    .unwrap();
    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunParamscanProfile { search: got, rank_by, .. } => {
            assert_eq!(got.as_ref(), Some(&search));
            assert_eq!(rank_by.as_deref(), Some("multi"));
        }
        other => panic!("expected RunParamscanProfile, got {other:?}"),
    }
}

/// ⚠ THE PREDICATE THE CLIENT REFUSES ON. An explicit `grid` needs no capability — an old
/// daemon that drops the field runs the grid, which is exactly what was asked, so refusing it
/// would be a false refusal. Everything else would be SILENTLY DOWNGRADED and must not be sent.
#[test]
fn only_a_selector_an_old_daemon_would_change_needs_the_capability() {
    let grid = SearchSelectionWire {
        optimizer: Some("grid".to_string()),
        ..SearchSelectionWire::default()
    };
    assert!(!grid.needs_capability(), "an explicit grid is what an old daemon would run anyway");
    assert!(
        !SearchSelectionWire::default().needs_capability(),
        "an empty selector asks for nothing"
    );
    for (label, w) in [
        (
            "a method",
            SearchSelectionWire {
                optimizer: Some("tpe".to_string()),
                ..SearchSelectionWire::default()
            },
        ),
        (
            "a budget",
            SearchSelectionWire { trials: Some("8".to_string()), ..SearchSelectionWire::default() },
        ),
        (
            "a seed",
            SearchSelectionWire { seed: Some("7".to_string()), ..SearchSelectionWire::default() },
        ),
        (
            "a depth",
            SearchSelectionWire {
                euler_depth: Some("2".to_string()),
                ..SearchSelectionWire::default()
            },
        ),
    ] {
        assert!(w.needs_capability(), "{label} would be silently dropped by an old daemon");
    }
    // ⚠ A knob under the GRID needs it too: the refusal it must produce
    // (`--trials is a tpe or genetic flag`) is one an old daemon cannot produce at all — it
    // drops the field and reports success.
    let mixed = SearchSelectionWire {
        optimizer: Some("grid".to_string()),
        trials: Some("8".to_string()),
        ..SearchSelectionWire::default()
    };
    assert!(mixed.needs_capability(), "a knob under grid must reach a daemon that can refuse it");
}

/// An EMPTY selector is the shape a caller must send as `None` — the property that keeps an
/// ordinary grid frame byte-identical to the one shipped before this field existed.
#[test]
fn an_empty_selector_is_empty_and_a_written_one_is_not() {
    assert!(SearchSelectionWire::default().is_empty());
    assert!(
        !SearchSelectionWire { seed: Some("0".to_string()), ..SearchSelectionWire::default() }
            .is_empty()
    );
}

/// The roster is a CONST, not a literal, and `DEFAULT_SEARCH_METHOD` is a member of it — the
/// §15.1 one-roster gate's anchor leg. Every other surface's leg is in its own crate.
#[test]
fn the_default_search_method_is_in_the_roster() {
    assert!(SEARCH_METHODS.contains(&DEFAULT_SEARCH_METHOD), "{SEARCH_METHODS:?}");
    assert_eq!(SEARCH_METHODS[0], DEFAULT_SEARCH_METHOD, "the default is the first row");
}

/// ⚠ PIN, written BEFORE the roster moved to `vike_model::run_request::search_methods`: the four
/// method names, their ORDER and the default are spelled as LITERALS here, and so is the JSON a
/// `SearchSelectionWire` naming each one puts on the wire. The const moved; none of these bytes may.
#[test]
fn the_search_method_roster_and_the_wire_bytes_it_rides_are_pinned() {
    assert_eq!(SEARCH_METHODS, ["grid", "euler", "tpe", "genetic"], "names AND order");
    assert_eq!(DEFAULT_SEARCH_METHOD, "grid");
    for (i, name) in ["grid", "euler", "tpe", "genetic"].into_iter().enumerate() {
        let search = SearchSelectionWire {
            optimizer: Some(name.to_string()),
            ..SearchSelectionWire::default()
        };
        let json = serde_json::to_string(&search).unwrap();
        assert_eq!(
            json,
            format!(r#"{{"optimizer":"{name}","euler_depth":null,"trials":null,"seed":null}}"#)
        );
        assert_eq!(SEARCH_METHODS[i], name);
        // The capability predicate keys on the default: only an explicit `grid` asks for nothing.
        assert_eq!(search.needs_capability(), name != "grid", "{name}");
    }
}

/// The study request survives `write_frame` -> `read_frame` with its recipe TEXT intact — the
/// property `vike-cli study` was built on: a PATH would be resolved against the BACKEND's
/// filesystem, so the recipe an author is editing would be invisible to the run they started.
#[test]
fn a_study_request_survives_the_frame_codec() {
    let study = StudyRunRequestWire {
        study: "cohort".to_string(),
        recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
        from: "2026-04-07T05".to_string(),
        to: "1785906000".to_string(),
        ..StudyRunRequestWire::default()
    };
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Request::RunStudy(Box::new(study.clone()))).unwrap();
    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::RunStudy(got) => assert_eq!(*got, study),
        other => panic!("expected RunStudy, got {other:?}"),
    }
}

/// `RunParamscanProfile` without the optional `rank_by` key still decodes (as `None` ⇒ the server's
/// `sharpe` default) — the same `#[serde(default)]` contract the v6 `params` field has.
#[test]
fn profile_sweep_without_rank_by_decodes_as_none() {
    let body = br#"{"RunParamscanProfile":{"profile_toml":"[data]\n"}}"#;
    match serde_json::from_slice::<Request>(body).expect("must decode without rank_by") {
        Request::RunParamscanProfile { rank_by, .. } => assert!(rank_by.is_none()),
        other => panic!("expected RunParamscanProfile, got {other:?}"),
    }
}

/// The `ListStrategies` request (a unit verb) and `Strategies` response survive
/// `write_frame` -> `read_frame` over an in-memory buffer — the roster list round-trips.
#[test]
fn list_strategies_and_strategies_survive_the_frame_codec() {
    let roster = vec!["buy_hold".to_string(), "grid".to_string()];
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Request::ListStrategies).unwrap();
    write_frame(&mut buffer, &Response::Strategies(roster.clone())).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::ListStrategies => {}
        other => panic!("expected ListStrategies, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::Strategies(got) => assert_eq!(got, roster),
        other => panic!("expected Strategies, got {other:?}"),
    }
}

/// A daemon that answers the handshake with `server` is refused by this client, naming both numbers,
/// never at a decode later.
fn refused_at_the_handshake(server: u32) -> vike_hub_protocol::call::HubCallError {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let _hello: HandshakeFrame = read_frame(&mut s).expect("Hello");
        let welcome =
            HandshakeReply::Welcome { proto_version: server, features: vec![], nonce: None };
        write_frame(&mut s, &welcome).expect("Welcome");
    });
    let Err(err) = crate::client::BacktesthubClient::connect(addr) else {
        panic!("a v{server} daemon must be refused")
    };
    assert!(matches!(err, vike_hub_protocol::call::HubCallError::Protocol(_)), "{err:?}");
    err
}

/// The P4.5c renames changed this wire's bytes, so the version stepped 7 -> 8: a daemon from before
/// them is refused at the handshake, naming both numbers.
#[test]
fn a_daemon_from_before_the_phase_four_renames_is_refused_at_the_handshake() {
    let err = refused_at_the_handshake(7);
    assert!(err.to_string().contains("client speaks 9, server speaks 7"), "{err}");
}

/// The walk-forward results changed this wire's bytes (package 2 step 4: `steps` and `overall`
/// where `windows` and four numbers stood), so the version stepped 8 -> 9: a daemon from before
/// them, which would answer a walk this client cannot decode, is refused at the handshake.
#[test]
fn a_daemon_from_before_the_walkforward_results_is_refused_at_the_handshake() {
    assert_eq!(PROTO_VERSION, 9);
    let err = refused_at_the_handshake(8);
    assert!(err.to_string().contains("client speaks 9, server speaks 8"), "{err}");
}
