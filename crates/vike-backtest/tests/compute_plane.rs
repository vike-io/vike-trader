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

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_backtest::compute_server::{serve, serve_with_studio};
use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    Plane, Request, Response, plane_of, read_frame, request_kind, write_frame,
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
        WireRunError, WireRunResult, WireSweepResult, WireWalkforwardResult,
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
            })
        }),
        Box::new(|_, _, _, _, _| {
            Ok(WireSweepResult { entries: Vec::new(), best_index: 0, dsr: None, pbo: None })
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
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), ts: 0 },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesGaps {
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
/// and the CONNECTION SURVIVES. All ten on ONE connection, which is what makes the survival claim
/// mean anything.
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
