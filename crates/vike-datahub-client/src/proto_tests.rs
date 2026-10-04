use std::io::{self, Cursor};

use super::*;

/// [`welcome_plane`] names a plane only when the `Welcome` names exactly ONE: a pre-split daemon
/// (both sentinels) and an unknown peer (neither) answer `None`, and so does a feature that
/// merely CONTAINS a sentinel — capability strings are compared whole, never by substring.
#[test]
fn a_welcome_names_a_plane_only_when_it_names_exactly_one() {
    let f = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let compute = f(&[COMPUTE_PLANE_SENTINEL, STUDIO_RUNNER_SENTINEL, FEATURE_AUTH]);
    let data = f(&[DATA_PLANE_SENTINEL, FEATURE_BACKFILL, FEATURE_AUTH]);
    assert_eq!(welcome_plane(&compute), Some(Plane::Compute));
    assert_eq!(welcome_plane(&data), Some(Plane::Data));
    // A pre-split daemon serves BOTH planes, so it is neither — a Write session there carries
    // the store verbs.
    let both = f(&[COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, FEATURE_AUTH]);
    assert_eq!(welcome_plane(&both), None);
    assert_eq!(welcome_plane(&f(&[FEATURE_AUTH])), None);
    assert_eq!(welcome_plane(&[]), None);
    // Whole-string equality: a longer token that happens to start with a sentinel is not it.
    let lookalike = f(&["backtest_v2", "load_bars_v2"]);
    assert_eq!(welcome_plane(&lookalike), None);
    // ...and the three sentinels are three different tokens.
    assert_ne!(COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL);
    assert_ne!(STUDIO_RUNNER_SENTINEL, COMPUTE_PLANE_SENTINEL);
}

/// `Hello` and `Welcome` (the PR-2 handshake pair) survive `write_frame` -> `read_frame`
/// unchanged over an in-memory buffer — the version + feature-list fields round-trip.
#[test]
fn hello_and_welcome_survive_the_frame_codec() {
    let features = vec!["backtest".to_string(), "load_bars".to_string()];
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    write_frame(
        &mut buf,
        &Response::Welcome {
            proto_version: PROTO_VERSION,
            features: features.clone(),
            nonce: None,
        },
    )
    .unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::Hello { proto_version } => assert_eq!(proto_version, PROTO_VERSION),
        other => panic!("expected Hello, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Welcome { proto_version, features: got, nonce } => {
            assert_eq!(proto_version, PROTO_VERSION);
            assert_eq!(got, features);
            assert_eq!(nonce, None);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// The AUTH frames survive the codec, and `Welcome.nonce` round-trips in BOTH shapes.
///
/// ⚠ The load-bearing half is the ABSENT one: a key-less server's `Welcome` must encode with no
/// `nonce` key at all (not `"nonce":null`), which is what makes it byte-identical to the
/// pre-auth protocol's frame and every old client's decode unaffected. Asserted on the JSON
/// text, because that is the only place the difference between "absent" and "present and null"
/// is visible.
#[test]
fn the_auth_frames_survive_the_codec_and_an_absent_nonce_is_absent_on_the_wire() {
    let keyless = serde_json::to_string(&Response::Welcome {
        proto_version: PROTO_VERSION,
        features: vec!["load_bars".to_string()],
        nonce: None,
    })
    .unwrap();
    assert!(!keyless.contains("nonce"), "a key-less Welcome must carry no nonce field: {keyless}");

    let nonce = [7u8; 32];
    let mut buf: Vec<u8> = Vec::new();
    write_frame(
        &mut buf,
        &Response::Welcome {
            proto_version: PROTO_VERSION,
            features: vec![FEATURE_AUTH.to_string()],
            nonce: Some(nonce),
        },
    )
    .unwrap();
    write_frame(&mut buf, &Request::Auth { scope: Scope::Write, mac: vec![9u8; 32] }).unwrap();
    write_frame(&mut buf, &Response::AuthOk { scope: Scope::Write }).unwrap();
    write_frame(&mut buf, &Response::AuthDenied { reason: "bad mac".to_string() }).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Welcome { nonce: got, features, .. } => {
            assert_eq!(got, Some(nonce));
            assert_eq!(features, vec![FEATURE_AUTH.to_string()]);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::Auth { scope, mac } => {
            assert_eq!(scope, Scope::Write);
            assert_eq!(mac, vec![9u8; 32]);
        }
        other => panic!("expected Auth, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::AuthOk { scope } => assert_eq!(scope, Scope::Write),
        other => panic!("expected AuthOk, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::AuthDenied { reason } => assert_eq!(reason, "bad mac"),
        other => panic!("expected AuthDenied, got {other:?}"),
    }
}

/// ⚠ FORWARD compatibility, the other half of the no-version-bump argument: an OLD server's
/// `Welcome` — one written before `nonce` existed, i.e. with no such field — must still decode
/// for a NEW client. That is what `#[serde(default)]` buys, and without it a new binary could
/// not talk to any deployed datahub at all.
#[test]
fn an_old_servers_welcome_still_decodes_for_a_new_client() {
    let old =
        format!(r#"{{"Welcome":{{"proto_version":{PROTO_VERSION},"features":["backtest"]}}}}"#);
    match serde_json::from_str::<Response>(&old).expect("an old Welcome must still decode") {
        Response::Welcome { proto_version, features, nonce } => {
            assert_eq!(proto_version, PROTO_VERSION);
            assert_eq!(features, vec!["backtest".to_string()]);
            assert_eq!(nonce, None, "an absent nonce decodes as None, not an error");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// The PR-3 `RunSlice` request and `RunResult` response survive `write_frame` -> `read_frame`
/// over an in-memory buffer — including the boxed `slice` field (serde-transparent) and the
/// embedded [`WireSpec`]/[`WireEngineParams`]/[`WireRunResult`] DTOs.
#[test]
fn run_slice_and_run_result_survive_the_frame_codec() {
    use crate::wire_studio::WireSliceKind;

    let request = Request::RunSlice {
        spec: WireSpec::Native {
            name: "buy_hold".to_string(),
            params_toml: "size = 1.0\n".to_string(),
        },
        slice: Box::new(WireSlice {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: Some(100_000),
            kind: WireSliceKind::Bars,
        }),
        params: Some(WireEngineParams { cash: Some(5000.0), fee_rate: None, slippage: None }),
    };
    let result = WireRunResult {
        equity_curve: vec![1000.0, 1010.0],
        equity_ts: vec![1, 2],
        final_equity: 1010.0,
        n_trades: 0,
        per_symbol_pnl: vec![],
        trades: vec![],
        stale_deferrals: 0,
        session_deferrals: 0,
        cost_model: None,
    };

    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &request).unwrap();
    write_frame(&mut buf, &Response::RunResult(result.clone())).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunSlice { spec, slice, params } => {
            assert!(matches!(spec, WireSpec::Native { .. }));
            assert_eq!(slice.venue, "binance"); // the box round-trips as a plain WireSlice
            assert_eq!(slice.end, Some(100_000));
            assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
        }
        other => panic!("expected RunSlice, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::RunResult(got) => assert_eq!(got, result),
        other => panic!("expected RunResult, got {other:?}"),
    }
}

/// The v6 `RunSweep` / `RunWalkforward` requests carry the OPTIONAL `params` cost/cash field and
/// survive `write_frame` -> `read_frame` unchanged (including the `Some(WireEngineParams)`).
#[test]
fn run_sweep_and_walkforward_carry_engine_params_over_the_frame_codec() {
    use crate::wire_studio::WireSliceKind;

    let slice = || {
        Box::new(WireSlice {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: WireSliceKind::Bars,
        })
    };
    let sweep = Request::RunParamscan {
        spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
        slice: slice(),
        paramscan: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
        params: Some(WireEngineParams {
            cash: Some(5000.0),
            fee_rate: Some(0.001),
            slippage: None,
        }),
    };
    let wf = Request::RunWalkforward {
        spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
        slice: slice(),
        walkforward: WireWalkforward::fixed(4),
        params: None,
    };

    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &sweep).unwrap();
    write_frame(&mut buf, &wf).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunParamscan { params, .. } => {
            assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
        }
        other => panic!("expected RunSweep, got {other:?}"),
    }
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunWalkforward { params, .. } => assert!(params.is_none()),
        other => panic!("expected RunWalkforward, got {other:?}"),
    }
}

/// A pre-v6 frame that OMITS `params` still decodes — `#[serde(default)]` maps the absent field
/// to `None`, so the field is backward-compatible on the wire.
#[test]
fn run_sweep_without_params_field_decodes_as_none() {
    // A RunSweep body written WITHOUT the `params` key (the pre-v6 shape).
    let body = br#"{"RunSweep":{"spec":{"Rhai":"fn on_bar() {}"},"slice":{"venue":"binance","symbols":["BTCUSDT"],"interval":"1m","start":null,"end":null,"kind":"Bars"},"sweep":{"axes":[]}}}"#;
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
    const PROFILE: &str = "[data]\nvenue = \"binance\"\n\n[sweep]\nfast = [5, 10]\n";

    let mut buf: Vec<u8> = Vec::new();
    write_frame(
        &mut buf,
        &Request::RunParamscanProfile {
            profile_toml: PROFILE.to_string(),
            rank_by: Some("max_dd".to_string()),
            search: None,
        },
    )
    .unwrap();
    write_frame(&mut buf, &Request::RunWalkforwardProfile { profile_toml: PROFILE.to_string() })
        .unwrap();
    write_frame(&mut buf, &Response::ParamscanReport("{\"rows\":[]}".to_string())).unwrap();
    write_frame(&mut buf, &Response::WalkforwardReport("{\"windows\":[]}".to_string())).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunParamscanProfile { profile_toml, rank_by, search } => {
            assert_eq!(profile_toml, PROFILE, "the profile TOML crosses verbatim");
            assert_eq!(rank_by.as_deref(), Some("max_dd"));
            assert!(search.is_none(), "an unselected search is absent, not an empty struct");
        }
        other => panic!("expected RunParamscanProfile, got {other:?}"),
    }
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunWalkforwardProfile { profile_toml } => assert_eq!(profile_toml, PROFILE),
        other => panic!("expected RunWalkforwardProfile, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::ParamscanReport(json) => assert_eq!(json, "{\"rows\":[]}"),
        other => panic!("expected ParamscanReport, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::WalkforwardReport(json) => assert_eq!(json, "{\"windows\":[]}"),
        other => panic!("expected WalkforwardReport, got {other:?}"),
    }
}

/// A v7 frame that OMITS `search` still decodes — the `#[serde(default)]` contract the v6
/// `params` field and the `rank_by` field both already have. This is the OLD-CLIENT /
/// NEW-SERVER direction, and it is the half a version bump would have broken.
#[test]
fn profile_sweep_without_search_decodes_as_none() {
    let body = br#"{"RunSweepProfile":{"profile_toml":"[data]\n"}}"#;
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
    let search = WireSearch {
        optimizer: Some("tpe".to_string()),
        euler_depth: None,
        trials: Some("128".to_string()),
        seed: Some("7".to_string()),
    };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(
        &mut buf,
        &Request::RunParamscanProfile {
            profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
            rank_by: Some("multi".to_string()),
            search: Some(search.clone()),
        },
    )
    .unwrap();
    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
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
    let grid = WireSearch { optimizer: Some("grid".to_string()), ..WireSearch::default() };
    assert!(!grid.needs_capability(), "an explicit grid is what an old daemon would run anyway");
    assert!(!WireSearch::default().needs_capability(), "an empty selector asks for nothing");
    for (label, w) in [
        ("a method", WireSearch { optimizer: Some("tpe".to_string()), ..WireSearch::default() }),
        ("a budget", WireSearch { trials: Some("8".to_string()), ..WireSearch::default() }),
        ("a seed", WireSearch { seed: Some("7".to_string()), ..WireSearch::default() }),
        ("a depth", WireSearch { euler_depth: Some("2".to_string()), ..WireSearch::default() }),
    ] {
        assert!(w.needs_capability(), "{label} would be silently dropped by an old daemon");
    }
    // ⚠ A knob under the GRID needs it too: the refusal it must produce
    // (`--trials is a tpe or genetic flag`) is one an old daemon cannot produce at all — it
    // drops the field and reports success.
    let mixed = WireSearch {
        optimizer: Some("grid".to_string()),
        trials: Some("8".to_string()),
        ..WireSearch::default()
    };
    assert!(mixed.needs_capability(), "a knob under grid must reach a daemon that can refuse it");
}

/// An EMPTY selector is the shape a caller must send as `None` — the property that keeps an
/// ordinary grid frame byte-identical to the one shipped before this field existed.
#[test]
fn an_empty_selector_is_empty_and_a_written_one_is_not() {
    assert!(WireSearch::default().is_empty());
    assert!(!WireSearch { seed: Some("0".to_string()), ..WireSearch::default() }.is_empty());
}

/// The roster is a CONST, not a literal, and `DEFAULT_SEARCH_METHOD` is a member of it — the
/// §15.1 one-roster gate's anchor leg. Every other surface's leg is in its own crate.
#[test]
fn the_default_search_method_is_in_the_roster() {
    assert!(SEARCH_METHODS.contains(&DEFAULT_SEARCH_METHOD), "{SEARCH_METHODS:?}");
    assert_eq!(SEARCH_METHODS[0], DEFAULT_SEARCH_METHOD, "the default is the first row");
}

/// The study request survives `write_frame` -> `read_frame` with its recipe TEXT intact — the
/// property `vike-cli study` was built on: a PATH would be resolved against the BACKEND's
/// filesystem, so the recipe an author is editing would be invisible to the run they started.
#[test]
fn a_study_request_survives_the_frame_codec() {
    let study = WireStudy {
        study: "cohort".to_string(),
        recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
        from: "2026-04-07T05".to_string(),
        to: "1785906000".to_string(),
    };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::RunStudy(Box::new(study.clone()))).unwrap();
    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::RunStudy(got) => assert_eq!(*got, study),
        other => panic!("expected RunStudy, got {other:?}"),
    }
}

/// ⚠ THE THREE CLASSIFIERS, asserted rather than trusted to the compiler. Each of them is
/// exhaustive, so a missing arm is a build failure — but WHICH arm was chosen is a judgement,
/// and these are the three judgements: a study RUNS an engine (compute), it WRITES a run
/// directory on the backend (Control, which `crates/vike-cli/src/cmd/study.rs`'s connect
/// already negotiates under), and it names itself in a refusal.
#[test]
fn the_study_verb_is_a_control_scoped_compute_verb() {
    let r = Request::RunStudy(Box::new(WireStudy {
        study: "cohort".to_string(),
        recipe_toml: String::new(),
        from: "1".to_string(),
        to: "2".to_string(),
    }));
    assert_eq!(plane_of(&r), Plane::Compute);
    assert_eq!(request_kind(&r), "RunStudy");
    assert_eq!(required_scope(&r), VerbScope::Write);
}

/// `RunParamscanProfile` without the optional `rank_by` key still decodes (as `None` ⇒ the server's
/// `sharpe` default) — the same `#[serde(default)]` contract the v6 `params` field has.
#[test]
fn profile_sweep_without_rank_by_decodes_as_none() {
    let body = br#"{"RunSweepProfile":{"profile_toml":"[data]\n"}}"#;
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
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::ListStrategies).unwrap();
    write_frame(&mut buf, &Response::Strategies(roster.clone())).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::ListStrategies => {}
        other => panic!("expected ListStrategies, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Strategies(got) => assert_eq!(got, roster),
        other => panic!("expected Strategies, got {other:?}"),
    }
}

/// The raw read returns a well-framed body WITHOUT decoding, so an undecodable body is
/// recoverable (a server answers it with `Response::Error`); `read_frame` on the SAME bytes still
/// errors on decode. This is the decode-vs-drop split the server relies on.
#[test]
fn read_frame_raw_separates_framing_from_decode() {
    // Valid JSON, but not a known `Request` variant: externally-tagged unit variants serialize
    // as JSON strings, and `"Bogus"` is not one of them, so it frames fine yet fails to decode.
    let body: &[u8] = b"\"Bogus\"";
    let mut framed: Vec<u8> = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    framed.extend_from_slice(body);

    // raw: returns the body bytes verbatim, no decode
    let mut cur = Cursor::new(framed.clone());
    let raw = read_frame_raw(&mut cur).unwrap();
    assert_eq!(raw.as_slice(), body, "raw read returns the framed body untouched");

    // fused: the same bytes fail to decode into a `Request`
    let mut cur2 = Cursor::new(framed);
    let err = read_frame::<_, Request>(&mut cur2).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "an unknown variant fails to decode");
}

/// `read_frame_raw` keeps `read_frame`'s OOM guard: an oversized declared length is rejected
/// before any body allocation.
#[test]
fn read_frame_raw_rejects_an_oversized_length() {
    let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
    framed.push(0); // a trailing byte — the guard must fire on the length, not on a short read
    let mut cur = Cursor::new(framed);
    let err = read_frame_raw(&mut cur).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// The `Backfill` request and `BackfillDone` response (the split-plane backfill-on-demand
/// verb) survive `write_frame` -> `read_frame` over an in-memory buffer. NOTE this verb
/// shipped WITHOUT a `PROTO_VERSION` bump: it is negotiated through the [`FEATURE_BACKFILL`]
/// capability string in `Response::Welcome`, the designed forward-compat hook — see that
/// constant's doc for the whole story.
#[test]
fn backfill_and_backfill_done_survive_the_frame_codec() {
    let mut buf: Vec<u8> = Vec::new();
    write_frame(
        &mut buf,
        &Request::Backfill {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 1_000,
            end: 2_000,
        },
    )
    .unwrap();
    let done = BackfillDone { rows_written: 5, first_ts: Some(1_000), last_ts: Some(1_900) };
    write_frame(&mut buf, &Response::BackfillDone(done.clone())).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::Backfill { venue, symbol, interval, start, end } => {
            assert_eq!(venue, "binance");
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(interval, "1h");
            assert_eq!(start, 1_000);
            assert_eq!(end, 2_000);
        }
        other => panic!("expected Backfill, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::BackfillDone(got) => assert_eq!(got, done),
        other => panic!("expected BackfillDone, got {other:?}"),
    }
}

/// The no-rows outcome (an already-ingested or empty range) round-trips with BOTH ts fields
/// `None` — distinguishable from a failure, which is `Response::Error`, never a zeroed
/// `BackfillDone`.
#[test]
fn backfill_done_no_rows_shape_round_trips() {
    let done = BackfillDone { rows_written: 0, first_ts: None, last_ts: None };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::BackfillDone(done.clone())).unwrap();
    let mut cur = Cursor::new(buf);
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::BackfillDone(got) => assert_eq!(got, done),
        other => panic!("expected BackfillDone, got {other:?}"),
    }
}

/// The `Coverage` request and a NON-EMPTY `Coverage` response survive the frame codec with
/// every field intact — the `BTreeMap<String, KindDays>` and its day vectors included.
///
/// The payload is built through `vike_data::store::coverage::join_coverage` rather than by hand, so
/// this pins the REAL shape the store produces (every `TICK_KINDS` entry present, absent kinds
/// as empty rows) rather than a hand-written approximation of it. Like `Backfill` above, this
/// verb shipped WITHOUT a `PROTO_VERSION` bump — see [`FEATURE_COVERAGE`].
#[test]
fn coverage_request_and_a_non_empty_report_survive_the_frame_codec() {
    let report = vike_data::store::coverage::join_coverage(&[
        (
            vike_data::SeriesId::per_symbol("trade", "binance", "BTCUSDT", None),
            vec![19_000, 19_001, 19_003],
        ),
        (
            vike_data::SeriesId::per_symbol("quote", "binance", "BTCUSDT", None),
            vec![19_000, 19_001],
        ),
    ]);
    assert!(!report.is_empty(), "the fixture must carry a real entry, not an empty vec");

    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Coverage).unwrap();
    write_frame(&mut buf, &Response::Coverage(report.clone())).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::Coverage => {}
        other => panic!("expected Coverage, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Coverage(got) => {
            assert_eq!(got, report, "the report survives the codec whole");
            // ...and the DERIVED answer the Data Manager renders survives with it: the gap in
            // the trade days and the quote kind's shorter span both have to make it across for
            // this to hold.
            assert_eq!(
                got[0].partial_days(),
                report[0].partial_days(),
                "the partial-day fold is identical on both sides of the codec"
            );
        }
        other => panic!("expected Coverage, got {other:?}"),
    }
}

/// The `rec_venue=` pair ROUND-TRIPS, which is the whole reason the builder and the reader ship
/// together: the server writes through one and the client reads through the other, so the
/// spelling exists once.
///
/// Modelled on `crates/vike-datahub-client/tests/market_data_negotiation.rs`'s
/// `the_md_venue_feature_round_trips` — the twin this pair was built in the image of.
#[test]
fn the_rec_venue_feature_round_trips() {
    let features = vec![
        rec_venue_feature("binance"),
        FEATURE_MARKET_DATA.to_string(),
        rec_venue_feature("polymarket"),
    ];
    assert_eq!(
        advertised_rec_venues(&features),
        vec!["binance".to_string(), "polymarket".to_string()],
        "advertisement ORDER is the contract, and a named capability between two entries is \
             not one of them"
    );
}

/// The two per-venue prefixes describe DIFFERENT PLANES and neither reader may see the other's
/// entries — the defect that would make a client report a venue as recordable because it
/// happens to be servable (the shipped image serves six and records two).
///
/// Also the EMPTY-value and whole-string-equality rules, which are
/// [`FEATURE_REC_VENUE_PREFIX`]'s own claims.
#[test]
fn the_two_venue_planes_advertise_separately() {
    let both = vec![md_venue_feature("okx"), rec_venue_feature("binance")];
    assert_eq!(advertised_rec_venues(&both), vec!["binance".to_string()]);
    assert_eq!(advertised_md_venues(&both), vec!["okx".to_string()]);

    // An empty or blank value advertises nothing — "absence is the answer".
    assert!(advertised_rec_venues(&[FEATURE_REC_VENUE_PREFIX.to_string()]).is_empty());
    assert!(advertised_rec_venues(&["rec_venue=   ".to_string()]).is_empty());
    assert_eq!(
        advertised_rec_venues(&["rec_venue= bybit ".to_string()]),
        vec!["bybit".to_string()],
        "the value is trimmed, exactly as the md twin trims"
    );

    // A NAMED capability can neither satisfy nor shadow a per-venue entry.
    let named = vec![FEATURE_BACKFILL.to_string(), FEATURE_COVERAGE.to_string()];
    assert!(advertised_rec_venues(&named).is_empty());
    assert!(advertised_rec_venues(&[]).is_empty(), "no entries is an EMPTY answer, not a panic");
}

/// The `import_format=` pair ROUND-TRIPS in advertisement order, the shape its two venue-prefix
/// precedents established — and it reads ONE prefix: a venue entry of either plane, and the named
/// `archive_import` capability itself, are not formats.
#[test]
fn the_import_format_feature_round_trips_and_reads_one_prefix() {
    let features = vec![
        FEATURE_ARCHIVE_IMPORT.to_string(),
        import_format_feature("dukascopy-bi5"),
        md_venue_feature("okx"),
        rec_venue_feature("binance"),
        import_format_feature("tardis-csv"),
    ];
    assert_eq!(
        advertised_import_formats(&features),
        vec!["dukascopy-bi5".to_string(), "tardis-csv".to_string()],
        "advertisement ORDER is the contract, and nothing but the prefix is read"
    );
    assert_eq!(advertised_md_venues(&features), vec!["okx".to_string()]);
    assert_eq!(advertised_rec_venues(&features), vec!["binance".to_string()]);

    // An empty or blank value advertises nothing — "absence is the answer".
    assert!(advertised_import_formats(&[FEATURE_IMPORT_FORMAT_PREFIX.to_string()]).is_empty());
    assert!(advertised_import_formats(&["import_format=  ".to_string()]).is_empty());
    // Whole-string equality: the named capability is not a format, and a format entry is not the
    // capability.
    assert!(advertised_import_formats(&[FEATURE_ARCHIVE_IMPORT.to_string()]).is_empty());
    assert!(!features[1..].iter().any(|f| f == FEATURE_ARCHIVE_IMPORT));
    assert_ne!(FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_MD_VENUE_PREFIX);
    assert_ne!(FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_REC_VENUE_PREFIX);
}

/// The variant each request maps to, through a match with NO `_` arm — so a new verb fails to
/// COMPILE here until it is given a row, and then fails
/// [`every_verb_is_classified_and_names_itself_by_its_wire_tag`] until it is given a sample.
fn variant_index(r: &Request) -> usize {
    match r {
        Request::Hello { .. } => 0,
        Request::Auth { .. } => 1,
        Request::Ping => 2,
        Request::RunBacktest(_) => 3,
        Request::RunSlice { .. } => 4,
        Request::RunParamscan { .. } => 5,
        Request::RunWalkforward { .. } => 6,
        Request::RunParamscanProfile { .. } => 7,
        Request::RunWalkforwardProfile { .. } => 8,
        Request::RunStudy(_) => 9,
        Request::LoadBars { .. } => 10,
        Request::ScanQuotes { .. } => 11,
        Request::ScanTrades { .. } => 12,
        Request::ScanBookUpdates { .. } => 13,
        Request::ScanDepth { .. } => 14,
        Request::ScanCohort { .. } => 15,
        Request::ScanPerpMetrics { .. } => 16,
        Request::ScanEquity { .. } => 17,
        Request::ScanExecFills { .. } => 18,
        Request::PropertiesAsOf { .. } => 19,
        Request::ListSeries => 20,
        Request::Inventory => 21,
        Request::SeriesFacts { .. } => 22,
        Request::SeriesGaps { .. } => 23,
        Request::Coverage => 24,
        Request::ListStrategies => 25,
        Request::NamedStrategies => 26,
        Request::RunNamed(_) => 27,
        Request::Backfill { .. } => 28,
        Request::ImportArchive(_) => 29,
        Request::SeedSeries { .. } => 30,
        Request::VenueCatalog { .. } => 31,
        Request::DeleteSeries { .. } => 32,
        Request::MdSubscribe { .. } => 33,
        Request::MdUpdate { .. } => 34,
        Request::ListBackfills => 35,
        Request::CancelBackfill { .. } => 36,
        Request::HistoryChannels => 37,
    }
}

/// How many [`Request`] variants [`variant_index`] numbers.
const REQUEST_VARIANTS: usize = 38;

/// One sample of EVERY [`Request`] variant.
fn one_of_every_request() -> Vec<Request> {
    use crate::wire_studio::WireSliceKind;
    let slice = || {
        Box::new(WireSlice {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: WireSliceKind::Bars,
        })
    };
    let spec = || WireSpec::Rhai("fn on_bar() {}".to_string());
    let series = || SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string()));
    let venue = "binance".to_string();
    let symbol = "BTCUSDT".to_string();
    let md = crate::market::MdSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        lane: crate::market::MdLane::Trades,
        depth_levels: None,
    };
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Read, mac: vec![0u8; 32] },
        Request::Ping,
        Request::RunBacktest(String::new()),
        Request::RunSlice { spec: spec(), slice: slice(), params: None },
        Request::RunParamscan {
            spec: spec(),
            slice: slice(),
            paramscan: WireParamscan { axes: vec![] },
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(),
            slice: slice(),
            walkforward: WireWalkforward::fixed(2),
            params: None,
        },
        Request::RunParamscanProfile { profile_toml: String::new(), rank_by: None, search: None },
        Request::RunWalkforwardProfile { profile_toml: String::new() },
        Request::RunStudy(Box::new(WireStudy {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        })),
        Request::LoadBars {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanTrades {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanBookUpdates {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanCohort {
            venue: venue.clone(),
            asset: "BTC".to_string(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills { venue: venue.clone(), symbol: symbol.clone() },
        Request::PropertiesAsOf { venue: venue.clone(), symbol: symbol.clone(), ts: 0 },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesFacts { id: series() },
        Request::SeriesGaps { id: series() },
        Request::Coverage,
        Request::ListStrategies,
        Request::NamedStrategies,
        Request::RunNamed(Box::new(crate::named_run::NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: Vec::new(),
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000,
        })),
        Request::Backfill {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: 0,
            end: 1,
        },
        Request::ImportArchive(crate::archive::ImportSpec {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: None,
            to_day: None,
            bars: vec!["1m".to_string()],
            dry_run: true,
            verify: false,
        }),
        Request::SeedSeries {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            class: None,
        },
        Request::VenueCatalog { venue: venue.clone() },
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        Request::MdSubscribe { specs: vec![md.clone()] },
        Request::MdUpdate { session: crate::market::MdSessionId(7), add: vec![md], remove: vec![] },
        Request::ListBackfills,
        Request::CancelBackfill { venue, symbol, interval: "1h".to_string() },
        Request::HistoryChannels,
    ]
}

/// A request's WIRE TAG, read off its encoding — the externally-tagged key, or the bare string of a
/// unit variant.
fn wire_tag(r: &Request) -> String {
    match serde_json::to_value(r).expect("every request encodes") {
        serde_json::Value::String(s) => s,
        serde_json::Value::Object(m) => {
            assert_eq!(m.len(), 1, "an externally-tagged request has ONE outer key: {m:?}");
            m.keys().next().expect("one key").clone()
        }
        other => panic!("a request encoded as neither a tag nor a tagged object: {other}"),
    }
}

/// ⚠ **A NEW VERB CANNOT BE MISSED.** [`variant_index`] has no `_` arm, so a variant added to
/// [`Request`] fails to compile until it is numbered; this test then fails until the sample set
/// carries it — and for every sample it holds the three exhaustive classifiers to one rule a
/// compiler cannot check: [`request_kind`] names the verb by the tag the WIRE carries, the one an
/// operator can grep a capture for. ([`plane_of`] and [`required_scope`] are exhaustive by
/// construction; WHICH arm they chose is pinned verb by verb in
/// `crates/vike-datahub/tests/auth_roundtrip.rs`'s `the_verb_scope_classification_is_pinned` and,
/// for this crate's newest verb, below.)
#[test]
fn every_verb_is_classified_and_names_itself_by_its_wire_tag() {
    let samples = one_of_every_request();
    let mut seen = [false; REQUEST_VARIANTS];
    for r in &samples {
        let i = variant_index(r);
        assert!(i < REQUEST_VARIANTS, "REQUEST_VARIANTS is stale: {r:?} maps to {i}");
        seen[i] = true;
        assert_eq!(request_kind(r), wire_tag(r), "request_kind must name the WIRE tag: {r:?}");
        // Exhaustive and total: neither may panic on any variant.
        let _ = (plane_of(r), required_scope(r));
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(missing.is_empty(), "variant indices with no sample: {missing:?}");
}

/// ⚠ THE ARCHIVE IMPORT'S THREE CLASSIFIERS, asserted rather than trusted to the compiler: it WRITES
/// the data daemon's store, so it is a DATA-plane verb (a `Plane::Compute` answer would have the
/// data daemon refuse its own verb with a wrong-plane message), it is Control-scoped for the
/// reasons `required_scope`'s arm carries, and it names itself by its wire tag.
#[test]
fn the_import_verb_is_a_control_scoped_data_verb() {
    let r = Request::ImportArchive(crate::archive::ImportSpec {
        format: "dukascopy-bi5".to_string(),
        dataset: "EURUSD".to_string(),
        from_day: None,
        to_day: None,
        bars: vec![],
        dry_run: true,
        verify: false,
    });
    assert_eq!(plane_of(&r), Plane::Data);
    assert_eq!(required_scope(&r), VerbScope::Write);
    assert_eq!(request_kind(&r), "ImportArchive");
    // ...and an Observe credential may not send it, while a Control one may.
    assert!(!scope_admits(Scope::Read, required_scope(&r)));
    assert!(scope_admits(Scope::Write, required_scope(&r)));
}

/// ⚠ THE BACKFILL REGISTRY'S TWO VERBS, asserted rather than trusted to the compiler —
/// `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
/// Both are DATA-plane (the registry is the data daemon's); the list is Observe and the cancel is
/// Control — an Observe key may see what the operator is fetching and may not stop any of it.
#[test]
fn the_backfill_registry_verbs_are_an_observe_list_and_a_control_cancel() {
    let list = Request::ListBackfills;
    let cancel = Request::CancelBackfill {
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
    };
    for r in [&list, &cancel] {
        assert_eq!(plane_of(r), Plane::Data, "{r:?}");
    }
    assert_eq!(required_scope(&list), VerbScope::Read);
    assert_eq!(required_scope(&cancel), VerbScope::Write);
    assert!(scope_admits(Scope::Read, required_scope(&list)));
    assert!(!scope_admits(Scope::Read, required_scope(&cancel)), "Observe must not cancel");
    assert!(scope_admits(Scope::Write, required_scope(&cancel)));
    assert_eq!(request_kind(&list), "ListBackfills");
    assert_eq!(request_kind(&cancel), "CancelBackfill");
}

/// ⚠ THE HISTORY-CHANNELS READ'S THREE CLASSIFIERS, asserted rather than trusted to the compiler —
/// `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`. A DATA-plane verb (its
/// overlay is the data daemon's table, store and credential store), Observe-scoped, a bare unit tag
/// on the wire — and its answer, the client's own compiled table here, survives the frame codec.
#[test]
fn the_history_channels_read_is_an_observe_scoped_data_verb_and_its_answer_round_trips() {
    let r = Request::HistoryChannels;
    assert_eq!(plane_of(&r), Plane::Data);
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(request_kind(&r), "HistoryChannels");
    assert_eq!(
        serde_json::to_value(&r).expect("encode"),
        serde_json::json!("HistoryChannels"),
        "the request names NOTHING — a bare unit tag"
    );

    let report = crate::history::compiled_report(1_790_899_200_000);
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::HistoryChannels(report.clone())).unwrap();
    match read_frame::<_, Response>(&mut Cursor::new(buf)).unwrap() {
        Response::HistoryChannels(got) => assert_eq!(got, report),
        other => panic!("expected HistoryChannels, got {other:?}"),
    }
    assert_eq!(FEATURE_HISTORY_CHANNELS, "history_channels");
}

/// Both answers survive the frame codec, the one-batch reason is the text the design names, and
/// the capability is its own whole string — not `backfill` and not a prefix of it.
#[test]
fn the_backfill_registry_answers_round_trip_and_the_capability_is_its_own_string() {
    let running = RunningBackfill {
        id: 7,
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
        start: 1_104_710_400_000,
        end: 1_104_796_799_999,
        peer: Some("127.0.0.1:50000".to_string()),
        started_ms: 1_700_000_000_000,
        elapsed_ms: 12_345,
        lane: "CredentialedKlines".to_string(),
        stoppable: true,
        cancelled: false,
        boundaries: 3,
    };
    let one_batch = RunningBackfill {
        id: 8,
        lane: "Klines".to_string(),
        stoppable: false,
        boundaries: 0,
        ..running.clone()
    };
    let done =
        BackfillCancelDone { flagged: vec![running.clone()], unstoppable: vec![one_batch.clone()] };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::RunningBackfills(vec![running.clone(), one_batch])).unwrap();
    write_frame(&mut buf, &Response::BackfillsCancelled(done.clone())).unwrap();
    let mut cur = Cursor::new(buf);
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::RunningBackfills(got) => assert_eq!(got.len(), 2),
        other => panic!("expected RunningBackfills, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::BackfillsCancelled(got) => assert_eq!(got, done),
        other => panic!("expected BackfillsCancelled, got {other:?}"),
    }
    assert_eq!(BACKFILL_ONE_BATCH, "cannot stop: one batch per request");
    assert_ne!(FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL);
    assert!(!FEATURE_BACKFILL_CANCEL.is_empty());
    let features = [FEATURE_BACKFILL.to_string()];
    assert!(
        !features.iter().any(|f| f == FEATURE_BACKFILL_CANCEL),
        "a `backfill` advertisement must not read as the cancel capability"
    );
}
