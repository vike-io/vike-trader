//! The COMPUTE half of ruling 7's split: `vike-backend backtest --addr` serves the seven compute
//! verbs and refuses every DATA verb by name, keeping the connection.
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 requires BOTH
//! directions to be tested. The other half — the data daemon refusing a compute verb — is
//! `crates/vike-datahub/tests/plane_split.rs`, and the two are deliberate mirror images: one helper
//! (`vike_datahub_client::proto`'s `wrong_plane_message`) writes both texts, so a change to one
//! refusal cannot leave the other behind.
//!
//! It also carries the round-trips that used to live in `vike-datahub`'s `roundtrip.rs` and moved
//! here with the verbs: `RunBacktest` over the wire, the invalid-profile refusal, the
//! `list_strategies` roster, and the profile-shaped sweep on a store-less build.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `MemHistStore`, no prod store, no network.
#![cfg(feature = "hist-replay")]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_backtest::compute_server::{serve, serve_authed, serve_with_studio};
use vike_backtest::named_run::NamedRunLane;
use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::named_run::{NamedParam, NamedRunOutcome, NamedRunRefusal, NamedRunSpec};
use vike_datahub_client::proto::{
    Plane, Request, Response, WireSearch, WireStudy, plane_of, read_frame, request_kind,
    write_frame,
};
use vike_datahub_client::wire_studio::{WireSlice, WireSliceKind, WireSpec};

/// A minimal, valid bar-mode profile. Over an empty `MemHistStore` it loads no bars, so the run
/// closes no trades — enough to exercise the full request → engine → report → response path.
///
/// ⚠ Moved here from `crates/vike-datahub/tests/roundtrip.rs` with the verbs that consume it.
const MINIMAL_BAR_PROFILE: &str = r#"
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
symbol = "BTCUSDT"
"#;

/// Bind an ephemeral loopback listener and spawn the COMPUTE server over a fresh in-memory store on
/// a detached thread. No Studio table — see [`the_studio_verbs_are_refused_when_no_table_is_mounted`]
/// for what that means and why it is a layer fact rather than an omission.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// The same server WITH a Studio table mounted — built from stub closures rather than the real
/// `vike_studio_core::studio_run_table`, because this crate cannot name that crate (that is the
/// whole reason the seam exists). What it proves is the DISPATCH: a mounted table is reached, and
/// the three verbs are advertised.
fn spawn_server_with_studio() -> SocketAddr {
    use vike_backtest::compute_server::StudioRunTable;
    use vike_datahub_client::wire_studio::{
        WireParamscanResult, WireRunError, WireRunResult, WireWalkforwardResult,
    };

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let table = StudioRunTable::new(
        Box::new(|_, _, _, _| {
            Ok(WireRunResult {
                equity_curve: vec![1.0],
                equity_ts: vec![0],
                final_equity: 1.0,
                n_trades: 0,
                per_symbol_pnl: Vec::new(),
                trades: Vec::new(),
                stale_deferrals: 0,
                session_deferrals: 0,
                cost_model: None,
            })
        }),
        Box::new(|_, _, _, _, _| {
            Ok(WireParamscanResult {
                entries: Vec::new(),
                best_index: 0,
                dsr: None,
                pbo: None,
                cost_model: None,
            })
        }),
        Box::new(|_, _, _, _, _| -> Result<WireWalkforwardResult, WireRunError> {
            Err(WireRunError::new("data", "stub".to_string()))
        }),
    );
    thread::spawn(move || {
        let _ = serve_with_studio(listener, store, Some(table));
    });
    addr
}

/// Every DATA verb, with a payload the data daemon would accept — so what is proved is the refusal,
/// not a decode failure that would look the same from outside.
fn data_requests() -> Vec<Request> {
    use vike_data::SeriesId;
    use vike_datahub_client::proto::SeriesSelector;
    vec![
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), ts: 0 },
        // The SIX tick-level and research reads `docs/decisions/0084` added to the data plane. They
        // belong here for the same reason their neighbours do: this daemon holds the COMPUTE store
        // handle, so it must refuse them BY NAME rather than answer from the wrong store.
        Request::ScanBookUpdates {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        // ⚠ `asset`, not `symbol` — this verb's middle field is the family's odd one out.
        Request::ScanCohort {
            venue: "binance".into(),
            asset: "BTC".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills { venue: "binance".into(), symbol: "BTCUSDT".into() },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesGaps {
            id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
        },
        // 0084's seventh verb: the data plane's, so this daemon refuses it by name like the rest.
        Request::SeriesFacts {
            id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
        },
        Request::Coverage,
        Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
    ]
}

/// The sample set IS the data plane — no more, no less, so a new data verb reddens this file until
/// its author adds a sample.
#[test]
fn every_data_verb_is_covered() {
    assert!(
        data_requests().iter().all(|r| plane_of(r) == Plane::Data),
        "a sample in this file is not a data verb"
    );
}

/// ⚠ THE MIRROR REFUSAL: every DATA verb answers `Response::Error` naming the daemon that serves it,
/// and the CONNECTION SURVIVES. EVERY sample on ONE connection, which is what makes the survival
/// claim mean anything.
///
/// ⚠ This said "all ten" until `docs/decisions/0084` added six verbs at once. A count in prose is
/// the thing this tree has watched rot most often, and the number bought nothing the loop below
/// does not already state — it drives whatever `data_requests` holds, so the claim is exactly as
/// wide as that set and cannot disagree with it.
#[test]
fn a_data_verb_is_refused_by_name_and_the_connection_survives() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for request in data_requests() {
        let kind = request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        let response: Response = read_frame(&mut stream).expect("the connection must survive");
        match response {
            Response::Error(msg) => {
                assert!(msg.contains(kind), "the refusal names the verb: {msg}");
                assert!(
                    msg.contains("vike-backend datahub"),
                    "the refusal names the daemon that DOES serve it: {msg}"
                );
                assert!(
                    msg.contains("config.datahub_addr"),
                    "the refusal names the settings key the client dialled from: {msg}"
                );
            }
            other => panic!("{kind} must be refused, not served: {other:?}"),
        }
    }
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

/// `RunBacktest` round-trips and the accept loop survives it — the test that used to prove this
/// against `vike-datahub`, now against the daemon that actually runs the engine.
#[test]
fn run_backtest_round_trips_and_server_survives() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    client.ping().expect("ping");

    // Over an empty store the run returns either a Report (JSON text) or a clean server-side Error
    // — BOTH well-formed Responses. The point: no hang and no panic that kills the loop.
    match client.run_backtest(MINIMAL_BAR_PROFILE) {
        Ok(json) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).expect("report must be valid JSON");
            assert!(v.is_object(), "report JSON must be an object");
        }
        Err(msg) => assert!(!msg.is_empty(), "a server-side error must carry a message"),
    }

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after a backtest request");
}

/// A malformed profile gets a clean `Response::Error`, and the server keeps serving — one bad
/// request never takes down the accept loop. (`from = "999999"` > `to = "100000"` fails
/// `BacktestProfile::from_toml_str`'s validation SERVER-side, which is where the client ships it.)
#[test]
fn invalid_profile_yields_error_not_a_dropped_connection() {
    let addr = spawn_server();
    let invalid = MINIMAL_BAR_PROFILE.replace("from = \"0\"", "from = \"999999\"");

    let mut client = DatahubClient::connect(addr).expect("connect");
    match client.run_backtest(&invalid) {
        Err(msg) => assert!(!msg.is_empty(), "expected a server-side validation error"),
        Ok(_) => panic!("an invalid (from > to) profile must not produce a report"),
    }

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after an invalid request");
}

/// The `list_strategies` verb round-trips, and the handshake advertises it — the roster is
/// store-independent, which is exactly why ruling 7 moved it here beside this binary's own `--list` flag, the
/// other transport for the same constant.
#[test]
fn list_strategies_round_trips_the_native_roster() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let roster = client.list_strategies().expect("list_strategies must return the roster");
    assert!(!roster.is_empty(), "the native strategy roster must be non-empty");
    assert!(roster.iter().any(|s| s == "buy_hold"), "roster must contain buy_hold: {roster:?}");
    // The rhai SCRIPT arm is deliberately excluded from the native roster.
    assert!(!roster.iter().any(|s| s == "rhai"), "rhai must NOT be in the native roster");

    assert!(
        client.features().iter().any(|f| f == "list_strategies"),
        "server must advertise the list_strategies feature: {:?}",
        client.features()
    );
    // ...and it is the SAME constant the `--list` flag reads, which is the property that made this
    // verb belong on this daemon.
    assert_eq!(roster.len(), vike_backtest::harness::STRATEGIES.len());
}

/// ⚠ The three STUDIO verbs on a daemon with NO table mounted: a NAMED refusal that says it is a
/// build/mount fact, never a silent empty answer — and no advertisement, so a client that reads the
/// handshake does not send one.
#[test]
fn the_studio_verbs_are_refused_when_no_table_is_mounted() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    for gone in ["run_slice", "run_sweep", "run_walkforward"] {
        assert!(
            !client.features().iter().any(|f| f == gone),
            "an unmounted daemon must not advertise {gone}: {:?}",
            client.features()
        );
    }
    let err = client
        .run_slice(
            WireSpec::Rhai("fn on_bar(){}".to_string()),
            WireSlice {
                venue: "binance".into(),
                symbols: vec!["BTCUSDT".into()],
                interval: "1d".into(),
                start: None,
                end: None,
                kind: WireSliceKind::Bars,
            },
            None,
        )
        .expect_err("an unmounted RunSlice must be refused");
    assert!(err.contains("RunSlice"), "the refusal names the verb: {err}");
    assert!(err.contains("vike-backend backtest --addr"), "…and the build that serves it: {err}");
}

/// ...and with a table MOUNTED the same verb is dispatched to it and advertised. Stub runners, so
/// what this proves is the SEAM (mount → advertise → dispatch), not a computation — the computation
/// is `vike-studio-core`'s own parity gate.
#[test]
fn a_mounted_studio_table_is_advertised_and_dispatched_to() {
    let addr = spawn_server_with_studio();
    let mut client = DatahubClient::connect(addr).expect("connect");
    for advertised in ["run_slice", "run_sweep", "run_walkforward"] {
        assert!(
            client.features().iter().any(|f| f == advertised),
            "a mounted daemon must advertise {advertised}: {:?}",
            client.features()
        );
    }
    let result = client
        .run_slice(
            WireSpec::Rhai("fn on_bar(){}".to_string()),
            WireSlice {
                venue: "binance".into(),
                symbols: vec!["BTCUSDT".into()],
                interval: "1d".into(),
                start: None,
                end: None,
                kind: WireSliceKind::Bars,
            },
            None,
        )
        .expect("a mounted RunSlice reaches the table");
    assert_eq!(result.final_equity, 1.0, "the answer came from the mounted runner");
}

// ---------------------------------------------------------------------------------------------
// Stage 7: the SEARCH SELECTOR on the wire, and the research plane's study verb.
// ---------------------------------------------------------------------------------------------

/// [`MINIMAL_BAR_PROFILE`] plus the grid a parameter search needs. The axis is `size`, which the
/// `buy_hold` strategy above genuinely declares — an axis naming a param the strategy does NOT
/// declare is refused by `harness::require_overridable_params`, and every test below would then
/// fail on a message about overridable params rather than on the method.
///
/// The SECTION is `[paramscan]`, which is what the parser calls it since stage 7; `[sweep]` is a
/// permanent serde alias, held open by `crates/vike-backtest/src/harness/profile.rs`'s
/// `both_the_new_section_and_the_legacy_one_load_and_parse_identically`.
fn paramscan_profile() -> String {
    format!("{MINIMAL_BAR_PROFILE}\n[paramscan]\nsize = [1.0, 2.0]\n")
}

/// THE §15.1 ONE-ROSTER GATE'S SERVER LEG: every method the PROTOCOL says it can carry is one this
/// daemon resolves. A fifth entry in `SEARCH_METHODS` with no arm in `search_select` reddens here
/// as well as in the engine's own pin — and this is the leg that proves it over a SOCKET.
///
/// The store is empty, so a run closes no trades and the report is uninteresting. What is asserted
/// is that the method was ACCEPTED: the answer is not an "invalid --optimizer" refusal.
#[test]
fn every_protocol_method_is_accepted_by_the_daemon() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for name in vike_datahub_client::SEARCH_METHODS {
        let search = WireSearch {
            optimizer: Some(name.to_string()),
            // genetic REQUIRES a seed; supplying one for every method would be refused by the
            // owners rule, so it is supplied for exactly the method that owns it.
            seed: (name == "genetic").then(|| "7".to_string()),
            ..WireSearch::default()
        };
        write_frame(
            &mut stream,
            &Request::RunParamscanProfile {
                profile_toml: paramscan_profile(),
                rank_by: None,
                search: Some(search),
            },
        )
        .expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
            Response::ParamscanReport(_) => {}
            Response::Error(msg) => panic!("{name} was refused: {msg}"),
            other => panic!("{name}: expected ParamscanReport, got {other:?}"),
        }
    }
}

/// A knob under a method that does not own it is refused BY NAME, over the wire, with the SAME
/// sentence `--local` produces — the property `search_select` exists for. Before stage 7 this argv
/// could not reach the daemon at all: `vike-cli` refused it at parse.
///
/// ⚠ Two rows, and the SECOND is the one that would rot quietly. `--trials` has ONE owner (tpe) and
/// `--seed` has TWO (tpe AND genetic), and `search_select::refuse_unowned_knob` renders EVERY owner
/// rather than the first — an operator who wrote `--seed 7` under the grid must be told about
/// genetic as well. A single-owner row cannot tell those two renderings apart.
#[test]
fn a_knob_under_the_wrong_method_is_refused_over_the_wire() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for (label, search, owners) in [
        (
            "--trials",
            WireSearch {
                optimizer: Some("grid".to_string()),
                trials: Some("8".to_string()),
                ..WireSearch::default()
            },
            &["tpe"][..],
        ),
        (
            "--seed",
            WireSearch {
                optimizer: Some("grid".to_string()),
                seed: Some("7".to_string()),
                ..WireSearch::default()
            },
            &["tpe", "genetic"][..],
        ),
    ] {
        write_frame(
            &mut stream,
            &Request::RunParamscanProfile {
                profile_toml: paramscan_profile(),
                rank_by: None,
                search: Some(search),
            },
        )
        .expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
            Response::Error(msg) => {
                assert!(msg.contains(label), "names the knob: {msg}");
                assert!(msg.contains("grid"), "names the method that was selected: {msg}");
                for owner in owners {
                    assert!(msg.contains(owner), "names EVERY owner ({owner}): {msg}");
                }
            }
            other => panic!("{label} under grid must be refused, got {other:?}"),
        }
    }
    // …and the connection survives a refused request, like every other refusal on this daemon.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

/// `--rank-by multi` needed no new wire field — the existing `rank_by` string could always carry it
/// — and was refused only because this daemon resolved it through `RankMetric::from_str_ci`, whose
/// four arms have no `multi`. It resolves now, and the report says so in its own `rank_by`.
#[test]
fn the_composite_ranking_is_served() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunParamscanProfile {
            profile_toml: paramscan_profile(),
            rank_by: Some("multi".to_string()),
            search: None,
        },
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::ParamscanReport(json) => {
            assert!(json.contains("\"rank_by\":\"multi\""), "the answer names it: {json}");
        }
        other => panic!("expected ParamscanReport, got {other:?}"),
    }
}

/// A method named on a profile with NO grid table is refused by the table's own message — the same
/// sentence the local engine's own pre-flight produces. This is what makes `vike-cli`'s routing
/// change safe: sending such a run HERE is how it gets an answer, rather than silently becoming a
/// single backtest over `RunBacktest`.
#[test]
fn a_method_without_a_grid_is_refused_by_the_missing_table() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunParamscanProfile {
            profile_toml: MINIMAL_BAR_PROFILE.to_string(),
            rank_by: None,
            search: Some(WireSearch {
                optimizer: Some("tpe".to_string()),
                ..WireSearch::default()
            }),
        },
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("[paramscan]"), "names the missing table: {msg}");
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

/// The HANDSHAKE half: this daemon advertises the capability UNCONDITIONALLY, which is what lets a
/// client refuse locally instead of sending a frame an older peer would silently downgrade.
#[test]
fn the_compute_daemon_advertises_the_search_capability() {
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
        "{:?}",
        client.features()
    );
}

/// **The walk-forward SEARCH capability is MOUNT-conditional, and both directions are asserted** —
/// the opposite rule to its neighbour above, which is why this test exists beside that one rather
/// than inside it.
///
/// It rides `Request::RunWalkforward`, whose runner is injected from a crate ABOVE this one, so an
/// unmounted daemon that advertised it would invite a frame whose only possible answer is the
/// `studio_verb_unmounted` refusal — the exact failure a capability string exists to prevent. And
/// the MOUNTED direction is the half a check gets wrong by writing `false`: without it, a daemon
/// that advertised nothing at all would pass.
#[test]
fn only_a_studio_mounted_daemon_advertises_the_walkforward_search_capability() {
    let unmounted = DatahubClient::connect(spawn_server()).expect("connect");
    assert!(
        !unmounted.features().iter().any(|f| f == vike_datahub_client::FEATURE_WALKFORWARD_SEARCH),
        "an unmounted daemon must not advertise a search it cannot run: {:?}",
        unmounted.features()
    );
    assert!(
        unmounted.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
        "…while the UNCONDITIONAL one IS there, so this cannot pass by an empty list: {:?}",
        unmounted.features()
    );

    let mounted = DatahubClient::connect(spawn_server_with_studio()).expect("connect");
    assert!(
        mounted.features().iter().any(|f| f == vike_datahub_client::FEATURE_WALKFORWARD_SEARCH),
        "a daemon with the Studio runners mounted CAN search, and must say so: {:?}",
        mounted.features()
    );
}

/// A daemon with NO study runner mounted refuses the verb by name and the CONNECTION SURVIVES —
/// the `studio_verb_unmounted` shape, and the state every `cargo run -p vike-backtest --bin
/// backtest` is in, because the runner lives in a crate this one may not name.
#[test]
fn a_study_is_refused_by_a_daemon_that_mounts_no_runner() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunStudy(Box::new(WireStudy {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        })),
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
        Response::Error(msg) => {
            assert!(msg.contains("RunStudy"), "names the verb: {msg}");
            assert!(msg.contains("vike-backend study"), "names the work that DOES run: {msg}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

/// …and a daemon that MOUNTS one serves it, and advertises that it can. The runner here is a
/// closure, not `vike_studio_core`'s — this crate cannot name that crate, which is the whole reason
/// the seam exists — so what is proven is the MOUNT, the arm, the advertisement and the answer
/// shape. `crates/vike-studio-core` builds the real one.
#[test]
fn a_mounted_study_runner_is_advertised_and_reached() {
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            None,
            Some(Box::new(|w: &WireStudy, _store| {
                Ok(format!("{{\"run_id\":\"r-1\",\"study\":{:?}}}", w.study))
            })),
            None,
            // This test is about the STUDY mount; the named-run lane stays at its default, which is
            // also what an operator who set nothing gets.
            NamedRunLane::DISARMED,
        );
    });

    let mut client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_STUDY),
        "a MOUNTED runner must be advertised: {:?}",
        client.features()
    );
    let json = client
        .run_study(&WireStudy {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        })
        .expect("a mounted runner answers");
    assert!(json.contains("r-1"), "the runner's own document comes back verbatim: {json}");
}

/// THE ADVERTISEMENT IS CONDITIONAL, and this is the half a capability check gets wrong. An
/// UNMOUNTED daemon must not advertise it — a client that saw it would send a frame whose only
/// possible answer is the refusal above, which is the whole failure mode `FEATURE_*` exists to
/// prevent. Contrast `FEATURE_SEARCH_METHOD`, which is UNCONDITIONAL because nothing is injected
/// for it.
#[test]
fn an_unmounted_daemon_does_not_advertise_the_study_capability() {
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        !client.features().iter().any(|f| f == vike_datahub_client::FEATURE_STUDY),
        "{:?}",
        client.features()
    );
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
        "…while the unconditional one IS there, so this cannot pass by an empty list: {:?}",
        client.features()
    );
}

// ---- the NAMED RUN (docs/decisions/0064-a-named-run-carries-no-source.md) -----------------------

/// A window this daemon will read: three days of `1d` bars, well inside `NAMED_RUN_MAX_BARS`.
fn named_spec(strategy: &str) -> NamedRunSpec {
    NamedRunSpec {
        strategy: strategy.to_string(),
        params: Vec::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1d".into(),
        start: 0,
        end: 86_400_000 * 3,
    }
}

/// Bind an ephemeral loopback listener and spawn the compute server with the NAMED-RUN lane ARMED.
fn spawn_server_named_run_armed() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            None,
            None,
            None,
            NamedRunLane::from_vars(&std::collections::HashMap::from([(
                vike_backtest::named_run::NAMED_RUN_ENV.to_string(),
                "1".to_string(),
            )])),
        );
    });
    addr
}

/// **The capability is a BUILD fact, so EVERY compute daemon advertises it — armed or not.**
///
/// This is the leg that makes an OLD server and an UNARMED one two different sentences rather than
/// one silence (0064's decision 8). Contrast `FEATURE_STUDY` above, which is a MOUNT fact and is
/// deliberately conditional.
#[test]
fn every_compute_daemon_advertises_the_named_run_capability() {
    for addr in [spawn_server(), spawn_server_named_run_armed()] {
        let client = DatahubClient::connect(addr).expect("connect");
        assert!(
            client.features().iter().any(|f| f == vike_datahub_client::FEATURE_NAMED_RUN),
            "{:?}",
            client.features()
        );
    }
}

/// **An UNARMED daemon ANSWERS BOTH VERBS and runs nothing** — 0064's decision 8, over a real
/// socket. The roster it reports is EMPTY — the arming gates the NAMES as well as the run (decision
/// 8 leg 3), so a picker renders `NamedRoster::unarmed_note` rather than the bare empty list.
#[test]
fn an_unarmed_daemon_answers_the_named_run_and_runs_nothing() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let roster = client.named_strategies().expect("an unarmed daemon still answers the roster");
    assert!(!roster.armed, "this daemon was not armed");
    // ...and it NAMES NOTHING — 0064's decision 8 leg 3, the reason arming gates the roster and not
    // only the run. The client renders `NamedRoster::unarmed_note`, so this is a teaching refusal
    // rather than an empty answer.
    assert!(
        roster.strategies.is_empty(),
        "an UNARMED daemon published its operator's strategy names over the wire: {:?}",
        roster.strategies
    );

    let outcome = client.run_named(&named_spec("buy_hold")).expect("it answers the run too");
    assert_eq!(outcome, NamedRunOutcome::NotArmed);
}

/// **The two rosters are DIFFERENT, and a picker built on the wrong one is wrong in both
/// directions** — 0064's decision 7. `ListStrategies` answers `harness::STRATEGIES`, which carries
/// the simulator-only arms that sit beside the Rhai compiler; the named-run roster cannot resolve
/// those, and it does resolve the operator's own user strategies, which `ListStrategies` has never
/// enumerated at all.
#[test]
fn the_named_roster_is_not_the_list_strategies_roster() {
    // ⚠ ARMED, necessarily: an unarmed daemon names nothing at all (decision 8 leg 3), so this
    // comparison would pass vacuously against `spawn_server()` — the kind of green that measures
    // the wrong thing.
    let addr = spawn_server_named_run_armed();
    let mut client = DatahubClient::connect(addr).expect("connect");
    let native = client.list_strategies().expect("the simulator roster");
    let named = client.named_strategies().expect("the named-run roster").strategies;

    assert!(!named.is_empty(), "the named-run roster must never be empty");
    // Neither roster carries the script arm...
    assert!(!native.iter().any(|s| s == "rhai"));
    assert!(!named.iter().any(|s| s == "rhai"));
    // ...but the DEFERRED simulator arms are on one and not the other, which is the whole point.
    let deferred: Vec<&str> = vike_strategy::SIMULATOR_ONLY.iter().map(|(n, _)| *n).collect();
    assert!(
        deferred.iter().any(|d| native.iter().any(|s| s == d)),
        "the simulator roster must carry the deferred arms, or this test proves nothing: {native:?}"
    );
    assert!(
        !deferred.iter().any(|d| named.iter().any(|s| s == d)),
        "a DEFERRED simulator arm is on the named-run roster. Those arms live in vike-backtest, \
         which CAN name vike-script, so they are outside the compiler-free closure — admitting one \
         is a REOPENER of docs/decisions/0064-a-named-run-carries-no-source.md, not a config \
         change. named={named:?}"
    );
}

/// **An ARMED daemon runs one pass over a real socket** — and the script path is still unreachable
/// by NAME, answered with this server's own roster rather than with a shrug.
#[test]
fn an_armed_daemon_runs_a_named_strategy_and_still_cannot_be_asked_for_a_script() {
    let addr = spawn_server_named_run_armed();
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert!(client.named_strategies().expect("roster").armed);

    // The store is empty, so the run closes no trades — the same thing
    // `run_backtest_round_trips_and_server_survives` relies on. What is proven is the whole path.
    match client.run_named(&named_spec("buy_hold")).expect("an armed daemon runs") {
        NamedRunOutcome::Ran { report_json, .. } => {
            assert!(report_json.contains("buy_hold"), "the report names the run: {report_json}");
        }
        other => panic!("expected a run, got {other:?}"),
    }

    // ...and `rhai` is refused as an UNKNOWN NAME carrying the roster — not as a compile error,
    // because there is no compiler at the end of this path to raise one.
    match client.run_named(&named_spec("rhai")).expect("the script name answers") {
        NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known }) => {
            assert!(!known.iter().any(|k| k == "rhai"), "{known:?}");
        }
        other => panic!("expected an unknown-strategy refusal, got {other:?}"),
    }
}

/// **The SERVER re-checks every bound, and the refusal names the constant it hit.**
///
/// The client refuses the same request locally before writing a frame, so this test goes AROUND the
/// client validator with raw frames — otherwise it would prove the client copy and say nothing
/// about the enforcement, which is the leg that actually matters.
#[test]
fn the_server_refuses_an_over_wide_window_by_name_over_raw_frames() {
    let addr = spawn_server_named_run_armed();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut wide = named_spec("buy_hold");
    wide.interval = "1s".into();
    wide.end = 86_400_000 * 30; // ~2.6M one-second bars
    write_frame(&mut stream, &Request::RunNamed(Box::new(wide))).expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("NAMED_RUN_MAX_BARS"), "the refusal names its bound: {msg}");
        }
        other => panic!("an over-wide window must be refused, got {other:?}"),
    }
    // ...and the connection SURVIVES a refusal, like every other bad request on this daemon.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream).expect("read"), Response::Pong));
}

/// **The reserved SOURCE key is refused by the server too**, and the message says where the script
/// path actually lives. The belt, not the fence — but a belt whose refusal teaches.
#[test]
fn the_server_refuses_a_reserved_source_param_over_raw_frames() {
    let addr = spawn_server_named_run_armed();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut spec = named_spec("buy_hold");
    spec.params.push((vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Int(1)));
    write_frame(&mut stream, &Request::RunNamed(Box::new(spec))).expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("carries no source"), "{msg}");
            assert!(msg.contains("--script"), "and it names the verb that DOES ship one: {msg}");
        }
        other => panic!("a `src` param must be refused, got {other:?}"),
    }
}

/// The named-run pair is COMPUTE-plane — the classification whose opposite would have the data
/// daemon try to answer verbs it holds no roster and no engine for.
#[test]
fn the_named_run_pair_is_compute_plane() {
    for r in [Request::NamedStrategies, Request::RunNamed(Box::new(named_spec("buy_hold")))] {
        assert_eq!(plane_of(&r), Plane::Compute, "{r:?}");
    }
}
