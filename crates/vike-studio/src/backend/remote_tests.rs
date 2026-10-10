use super::*;
use std::assert_matches;
use vike_data::TsRange;
use vike_model::Trade;

#[test]
fn backend_defaults_to_local() {
    // The default is the DAEMON now; `Local` is gone. See `impl Default for Backend`.
    assert_eq!(Backend::default(), Backend::Remote { addr: DEFAULT_COMPUTE_ADDR.to_string() });
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// ⚠ **Studio's compute key is WRITE-ONLY, trimmed, and read from its own variable alone.** The
/// Read slot is empty by construction, so the key cannot be presented as an observe key even
/// inside this module; the value is trimmed the way the daemon trims its own copy, or a
/// trailing newline from the launcher's ssh read would earn `bad mac`.
#[test]
fn the_compute_key_is_write_only_and_trimmed() {
    let key = compute_key_from_vars(&vars(&[(COMPUTE_KEY_ENV, "  placeholder-value \n")]))
        .expect("a set variable resolves");
    assert!(
        key.write_only.key_for(Scope::Read).is_empty(),
        "the Read half of Studio's compute key must be EMPTY — it is a Control key for one dial"
    );
    assert_eq!(key.write_only.key_for(Scope::Write), b"placeholder-value");
}

/// A blank value is no key, and an absent one is none — the ordinary unconfigured state.
#[test]
fn a_blank_or_absent_compute_key_is_none() {
    assert!(compute_key_from_vars(&vars(&[])).is_none());
    for blank in ["", "   ", "\t\n"] {
        assert!(
            compute_key_from_vars(&vars(&[(COMPUTE_KEY_ENV, blank)])).is_none(),
            "{blank:?} was treated as a key"
        );
    }
}

/// ⚠ **The PLATFORM name is ignored, whatever it holds.** The whole reach limit rests on this
/// function reading one name that nothing else reads; a fallback to
/// `VIKE_DATAHUB_CONTROL_KEY` would make every environment that carries the platform pair —
/// a box running `vike-cli` against its own datahub — a Studio holding a Write key it was
/// never handed.
#[test]
fn the_platform_control_key_name_is_never_read_for_studio() {
    let only_platform =
        vars(&[(vike_node_proto::auth::DATAHUB_CONTROL_KEY_ENV, "placeholder-value")]);
    assert!(
        compute_key_from_vars(&only_platform).is_none(),
        "Studio's compute key resolved from the PLATFORM control key name"
    );
    assert_ne!(COMPUTE_KEY_ENV, vike_node_proto::auth::DATAHUB_CONTROL_KEY_ENV);
    assert_ne!(COMPUTE_KEY_ENV, vike_node_proto::auth::DATAHUB_OBSERVE_KEY_ENV);
}

/// Its `Debug` carries presence only — so a `{:?}` of the state that holds it, or a panic
/// message formatting it, cannot print the key.
#[test]
fn the_compute_key_debug_is_redacted() {
    let key =
        compute_key_from_vars(&vars(&[(COMPUTE_KEY_ENV, "placeholder-value")])).expect("resolves");
    let shown = format!("{key:?} {:?}", Some(key.clone()));
    assert!(!shown.contains("placeholder-value"), "the key reached a Debug rendering: {shown}");
    assert!(shown.contains("redacted"), "{shown}");
}

/// The refusal a keyless Studio shows against a KEYED compute daemon names the variable and the
/// launcher, keeps the phrase every earlier banner used, and does NOT send the operator to a
/// credential store Studio's Run never reads.
#[test]
fn a_keyless_studio_is_told_where_its_compute_key_comes_from() {
    let msg = no_compute_key_message("127.0.0.1:7880");
    assert!(msg.contains("REQUIRES authentication"), "{msg}");
    assert!(msg.contains(COMPUTE_KEY_ENV), "names the variable: {msg}");
    assert!(msg.contains("just studio"), "names the launcher: {msg}");
    assert!(
        !msg.contains(vike_node_proto::auth::DATAHUB_CONTROL_KEY_ENV),
        "must not send the operator to the platform key name: {msg}"
    );
}

/// ⚠ **STUDIO'S OWN RUN PATH, holding a compute key, pointed at a DATA-plane server, sends
/// nothing after its `Hello`.** This is the production call `StudioState::start_run` makes, not
/// the client crate's constructor on its own — so it also fails if [`connect`] ever signs the
/// key through the UNGUARDED `connect_authed`, which the client crate's own tests cannot see.
#[test]
fn a_run_holding_the_compute_key_sends_nothing_to_a_data_plane_server() {
    use std::io::Read;
    use vike_datahub_client::{
        DATA_PLANE_SENTINEL, FEATURE_AUTH, PROTO_VERSION,
        proto::{Request, Response, read_frame, write_frame},
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr").to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        assert_matches!(read_frame::<_, Request>(&mut stream), Ok(Request::Hello { .. }));
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![DATA_PLANE_SENTINEL.to_string(), FEATURE_AUTH.to_string()],
                nonce: Some([9u8; 32]),
            },
        )
        .expect("welcome");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("timeout");
        let mut after_hello = Vec::new();
        let _ = stream.read_to_end(&mut after_hello);
        after_hello
    });

    let key =
        compute_key_from_vars(&vars(&[(COMPUTE_KEY_ENV, "placeholder-value")])).expect("resolves");
    let slice =
        DataSlice::bars("binance", "BTCUSDT", "1h", TsRange { start: Some(0), end: Some(1) });
    let err = run_slice_remote(&addr, Some(&key), &StrategySpec::rhai("fn on_bar(){}"), &slice)
        .expect_err("a DATA-plane server must be refused for Studio's compute key");
    assert!(err.to_string().contains("DATA daemon"), "{err}");
    let after_hello = server.join().expect("server thread");
    assert!(
        after_hello.is_empty(),
        "Studio's Run wrote {} byte(s) after its Hello to the DATA daemon while holding a \
             Control key",
        after_hello.len()
    );
}

#[test]
fn to_wire_spec_maps_rhai_verbatim() {
    let w = to_wire_spec(&StrategySpec::rhai("fn on_bar() {}")).unwrap();
    assert_eq!(w, WireSpec::Rhai("fn on_bar() {}".to_string()));
}

/// A native spec's params go out as TOML TEXT and round-trip through the SAME parser the server
/// uses (`native_from_toml_str`), so a remote native run resolves the identical params table.
#[test]
fn to_wire_spec_serializes_native_params_to_toml_text() {
    let spec = StrategySpec::native(
        "buy_hold",
        vike_studio_core::params_from_rows(&[
            ("size".to_string(), "2".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
        ]),
    );
    match to_wire_spec(&spec).unwrap() {
        WireSpec::Native { name, params_toml } => {
            assert_eq!(name, "buy_hold");
            let back = StrategySpec::native_from_toml_str("buy_hold", &params_toml).unwrap();
            assert_eq!(back, spec, "params must survive Value -> TOML text -> Value");
        }
        other => panic!("expected Native, got {other:?}"),
    }
}

/// The full round trip through the wire: `StrategySpec::Plugin` -> `WireSpec::Plugin` (params
/// serialized to TOML text, sha carried whole) and back through the SERVER's own converter
/// (`vike_studio_core::to_strategy_spec`), so a client encode and a server decode are proven
/// against each other rather than each half proving only itself.
#[test]
fn a_plugin_spec_round_trips_through_the_wire() {
    let mut params = toml::map::Map::new();
    params.insert("fast".to_string(), toml::Value::Integer(5));
    let spec = StrategySpec::Plugin {
        name: "my_strat".to_string(),
        sha: "a".repeat(64),
        params: toml::Value::Table(params),
    };
    let w = to_wire_spec(&spec).expect("plugin spec converts");
    match &w {
        WireSpec::Plugin { name, sha, params_toml } => {
            assert_eq!(name, "my_strat");
            assert_eq!(
                sha.len(),
                64,
                "the sha must survive whole — a truncated one names another artifact"
            );
            assert!(params_toml.contains("fast"), "params ride as TOML text");
        }
        other => panic!("wrong variant: {other:?}"),
    }
    assert_eq!(vike_studio_core::to_strategy_spec(&w).expect("converts back"), spec);
}

#[test]
fn to_wire_slice_maps_fields_and_kind() {
    let bars =
        DataSlice::bars("binance", "BTCUSDT", "1m", TsRange { start: Some(1), end: Some(9) });
    let w = to_wire_slice(&bars);
    assert_eq!(w.venue, "binance");
    assert_eq!(w.symbols, vec!["BTCUSDT".to_string()]);
    assert_eq!(w.interval, "1m");
    assert_eq!(w.start, Some(1));
    assert_eq!(w.end, Some(9));
    assert_eq!(w.kind, WireSliceKind::Bars);

    let ticks = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
    assert_eq!(to_wire_slice(&ticks).kind, WireSliceKind::Ticks);
}

fn sample_trade() -> Trade {
    Trade {
        entry_price: 100.0,
        exit_price: 110.0,
        size: 1.0,
        pnl: 10.0,
        fees: 0.1,
        entry_ts: 1,
        exit_ts: 2,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    }
}

#[test]
fn to_backtest_result_reconstructs_rendered_fields() {
    let wire = WireRunResult {
        equity_curve: vec![1000.0, 1010.0],
        equity_ts: vec![1, 2],
        final_equity: 1010.0,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
        trades: vec![WireTrade::from_trade(&sample_trade())],
        stale_deferrals: 3,
        session_deferrals: 4,
        cost_model: None,
    };
    let r = to_backtest_result(wire);
    assert_eq!(r.equity_curve, vec![1000.0, 1010.0]);
    assert_eq!(r.equity_ts, vec![1, 2]);
    assert_eq!(r.final_equity, 1010.0);
    assert_eq!(r.n_trades, 1);
    assert_eq!(r.per_symbol_pnl, vec![("BTCUSDT".to_string(), 10.0)]);
    assert_eq!(r.trades, vec![sample_trade()]);
    assert_eq!(r.stale_deferrals, 3);
    assert_eq!(r.session_deferrals, 4);
    // fields the wire never carries fall back to Default
    assert_eq!(r.warmup, 0);
    assert!(r.dropped.is_empty());
}

#[test]
fn to_studio_sweep_maps_entries_and_none_scores_to_nan() {
    let wire = WireParamscanResult {
        entries: vec![WireParamscanEntry {
            overrides: vec![("fast".to_string(), 5.0)],
            result: WireRunResult { final_equity: 42.0, ..Default::default() },
        }],
        dsr: None,
        pbo: Some(0.25),
        best_index: 0,
        cost_model: None,
    };
    let s = to_studio_sweep(wire);
    assert_eq!(s.entries.len(), 1);
    assert_eq!(s.entries[0].overrides, vec![("fast".to_string(), 5.0)]);
    assert_eq!(s.entries[0].result.final_equity, 42.0);
    assert!(s.dsr.is_nan(), "a None dsr becomes NaN (not assessable)");
    assert_eq!(s.pbo, 0.25);
    assert_eq!(s.best_index, 0);
}

#[test]
fn to_walkforward_report_is_a_full_mirror() {
    let wire = WireWalkforwardResult {
        windows: vec![
            WireWfWindow { test_range: (0, 100), oos_return: 0.05, chosen_params: None },
            WireWfWindow {
                test_range: (100, 200),
                oos_return: -0.02,
                chosen_params: Some(vec![("fast".to_string(), "8".to_string())]),
            },
        ],
        oos_equity_curve: vec![10_000.0, 10_500.0],
        oos_return: 0.05,
        oos_sharpe: 1.2,
        wf_consistency: 1.0,
        cost_model: None,
    };
    let r = to_walkforward_report(wire);
    assert_eq!(r.windows.len(), 2);
    assert_eq!(r.windows[0].test_range, (0, 100));
    assert_eq!(r.windows[0].oos_return, 0.05);
    // the fixed-parameter window records no choice; the searched one carries its winner back
    // as a real `toml::Value`, not as the text it crossed the wire in
    assert_eq!(r.windows[0].chosen_params, None);
    assert_eq!(
        r.windows[1].chosen_params,
        Some(vec![("fast".to_string(), toml::Value::Integer(8))])
    );
    assert_eq!(r.oos_equity_curve, vec![10_000.0, 10_500.0]);
    assert_eq!(r.oos_return, 0.05);
    assert_eq!(r.oos_sharpe, 1.2);
    assert_eq!(r.wf_consistency, 1.0);
}

/// The render→parse PAIR, end to end. `vike_studio_core::wire_run`'s `to_wire_wf_window` writes
/// each chosen value with `toml::Value`'s `Display` — the `.to_string()` below IS that call —
/// and [`to_wf_window`] parses it back. So this asserts the exact property
/// `WireWfWindow::chosen_params`' doc claims and nothing weaker: across the value shapes a
/// `[sweep]` axis produces, the text trip is lossless.
#[test]
fn a_rendered_toml_value_parses_back_to_itself() {
    let levels = toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]);
    let originals: Vec<(String, toml::Value)> = vec![
        ("slow".to_string(), toml::Value::Integer(30)),
        ("size".to_string(), toml::Value::Float(1.5)),
        ("symbol".to_string(), toml::Value::String("BTCUSDT".to_string())),
        ("flag".to_string(), toml::Value::Boolean(true)),
        ("levels".to_string(), levels),
    ];
    // exactly what the server puts on the wire
    let rendered: Vec<(String, String)> =
        originals.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();

    let back = to_wf_window(WireWfWindow {
        test_range: (0, 10),
        oos_return: 0.0,
        chosen_params: Some(rendered),
    });
    assert_eq!(
        back.chosen_params,
        Some(originals),
        "every shape a [sweep] axis produces must survive Value -> TOML text -> Value"
    );
}

/// A rendering that does NOT parse back is preserved as a `toml::Value::String` of the raw
/// text — never dropped, and never a panic. This is the residual the wire type's doc names:
/// the pair survives, the TYPE does not, and a report is read rather than re-executed.
#[test]
fn an_unparseable_rendering_is_kept_as_its_own_text() {
    let back = to_wf_window(WireWfWindow {
        test_range: (0, 10),
        oos_return: 0.0,
        chosen_params: Some(vec![("mystery".to_string(), "not a toml value".to_string())]),
    });
    let kept = toml::Value::String("not a toml value".to_string());
    assert_eq!(back.chosen_params, Some(vec![("mystery".to_string(), kept)]));
}

#[test]
fn parse_wire_run_error_classifies_the_kind_prefix() {
    match parse_wire_run_error("compile: bad".to_string()) {
        RunError::Compile(m) => assert_eq!(m, "bad"),
        other => panic!("expected Compile, got {other:?}"),
    }
    match parse_wire_run_error("data: no bars".to_string()) {
        RunError::Data(m) => assert_eq!(m, "no bars"),
        other => panic!("expected Data, got {other:?}"),
    }
    match parse_wire_run_error("strategy: nope".to_string()) {
        RunError::Strategy(m) => assert_eq!(m, "nope"),
        other => panic!("expected Strategy, got {other:?}"),
    }
    // an unrecognized prefix (a desync / transport line) is surfaced verbatim as Data
    match parse_wire_run_error("protocol desync: x".to_string()) {
        RunError::Data(m) => assert_eq!(m, "protocol desync: x"),
        other => panic!("expected Data, got {other:?}"),
    }
    match parse_wire_run_error("bare".to_string()) {
        RunError::Data(m) => assert_eq!(m, "bare"),
        other => panic!("expected Data, got {other:?}"),
    }
}

// ⚠ `remote_store_tick_refusal` STOOD HERE AND IS DELETED, not ported.
//
// It walked the split-plane tick rule exhaustively, and the ONE refused cell it asserted
// was (remote store, tick slice, LOCAL backend) - a refusal that exists only because a
// local path was the thing being refused IN FAVOUR OF. `Backend::Local` is gone
// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`), so the
// cell is unreachable and a test asserting it would pass for a reason that no longer
// exists - which this tree treats as worse than no test at all.
