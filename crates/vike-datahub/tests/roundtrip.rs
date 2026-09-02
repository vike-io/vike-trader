//! Hermetic end-to-end tests for vike-datahub — loopback only, no prod store, no external network.
//!
//! The server is bound on an ephemeral `127.0.0.1:0` port and driven over an in-memory
//! `MemHistStore` (vike-data's `test-support` double), so every test is self-contained and
//! deterministic. Plus direct `proto` framing tests over in-memory buffers (write -> read
//! round-trip, and the oversized-length OOM guard).

use std::io::{self, Cursor};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::{
    read_frame, write_frame, DatahubClient, Request, Response, MAX_FRAME_LEN,
};

/// A minimal, valid bar-mode profile. Over an empty `MemHistStore` it loads no bars, so the run
/// closes no trades — enough to exercise the full request -> engine -> report -> response path.
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

/// Bind an ephemeral loopback listener, spawn `serve` over a fresh in-memory store on a detached
/// thread, and return the assigned address for a client to connect to.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        // The serve loop runs for the lifetime of the test process; its Result is only Err on an
        // impossible listener close, which we don't assert on here.
        let _ = serve(listener, store);
    });
    addr
}

#[test]
fn ping_round_trips() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    client.ping().expect("ping must get Pong");
}

#[test]
fn run_backtest_round_trips_and_server_survives() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    client.ping().expect("ping");

    // The client ships the profile's TOML text; over an empty store the run returns either a Report
    // (JSON text) or a clean server-side Error — BOTH well-formed Responses. The point: no hang and
    // no panic that kills the loop.
    match client.run_backtest(MINIMAL_BAR_PROFILE) {
        Ok(json) => {
            // the report round-trips as well-formed JSON — the same shape `backtest --json` emits
            let v: serde_json::Value =
                serde_json::from_str(&json).expect("report must be valid JSON");
            assert!(v.is_object(), "report JSON must be an object");
        }
        Err(msg) => assert!(!msg.is_empty(), "a server-side error must carry a message"),
    }

    // The accept loop survived that request: a brand-new connection still pings. A fresh connection
    // proves the SERVER lived, independent of the first connection's fate.
    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after a backtest request");
}

/// Two malformed requests on separate connections both get a clean `Response::Error`, and the
/// server keeps serving — one bad request never takes down the accept loop.
#[test]
fn invalid_profile_yields_error_not_a_dropped_connection() {
    let addr = spawn_server();

    // `from > to` fails `BacktestProfile::from_toml_str`'s validation server-side. The client ships
    // the raw TOML, so the server is where the invalid profile is caught → a clean Response::Error.
    // (`from = "999999"` > `to = "100000"`.)
    let invalid = MINIMAL_BAR_PROFILE.replace("from = \"0\"", "from = \"999999\"");

    let mut client = DatahubClient::connect(addr).expect("connect");
    match client.run_backtest(&invalid) {
        Err(msg) => assert!(!msg.is_empty(), "expected a server-side validation error"),
        Ok(_) => panic!("an invalid (from > to) profile must not produce a report"),
    }

    // Server survived: a fresh connection still pings.
    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after an invalid request");
}

/// The `list_strategies` verb round-trips over the loopback server (an empty `MemHistStore` — the
/// roster is store-independent), returning the compiled native strategy names non-empty and
/// including `"buy_hold"`. The server keeps serving afterwards.
#[test]
fn list_strategies_round_trips_the_native_roster() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let roster = client.list_strategies().expect("list_strategies must return the roster");
    assert!(!roster.is_empty(), "the native strategy roster must be non-empty");
    assert!(roster.iter().any(|s| s == "buy_hold"), "roster must contain buy_hold: {roster:?}");
    // The rhai SCRIPT arm is deliberately excluded from the native roster.
    assert!(!roster.iter().any(|s| s == "rhai"), "rhai must NOT be in the native roster");

    // The handshake advertises the verb as a served feature.
    assert!(
        client.features().iter().any(|f| f == "list_strategies"),
        "server must advertise the list_strategies feature: {:?}",
        client.features()
    );

    // Server survived: a fresh connection still pings.
    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after list_strategies");
}

/// The v7 PROFILE-shaped sweep verb is served on a LEAN (no-`serve-datafusion`) build — unlike the
/// Studio `RunSweep`, it runs the DataFusion-free `vike_backtest::harness::run_sweep`. Over an empty
/// `MemHistStore` every grid point loads no bars, so this asserts the PLUMBING (request → harness →
/// ranked report → response), not results: the answer is a well-formed `SweepReport` with one row
/// per grid point, or a clean server-side error — never a hang or a dropped connection.
#[test]
fn run_sweep_profile_round_trips_on_a_lean_build() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let profile = format!("{MINIMAL_BAR_PROFILE}\n[sweep]\nsize = [1.0, 2.0]\n");
    match client.run_sweep_profile(&profile, Some("return")) {
        Ok(json) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).expect("sweep report must be valid JSON");
            // `--rank-by return` resolves to `RankMetric::TotalReturn`, whose serde name is
            // `total_return` — the report labels itself with the metric the SERVER applied.
            assert_eq!(v["rank_by"], "total_return", "the requested metric ranked it server-side");
            assert_eq!(v["rows"].as_array().map(Vec::len), Some(2), "one row per grid point");
        }
        Err(msg) => assert!(!msg.is_empty(), "a server-side error must carry a message"),
    }

    // The handshake advertises it as a served feature on THIS (lean) build.
    assert!(
        client.features().iter().any(|f| f == "run_sweep_profile"),
        "a lean server must advertise run_sweep_profile: {:?}",
        client.features()
    );

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after a profile sweep");
}

/// The profile verbs' clean-error paths, all on a lean build: a profile with no `[sweep]` /
/// `[walkforward]` table and an unrecognized `rank_by` are each a `Response::Error` naming the
/// problem — never a silent fallback, and never a dropped connection.
#[test]
fn profile_verbs_reject_a_missing_section_and_an_unknown_rank_by() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let err = client
        .run_sweep_profile(MINIMAL_BAR_PROFILE, None)
        .expect_err("a profile with no [sweep] table must not sweep");
    assert!(err.contains("[sweep]"), "names the missing section: {err}");

    let err = client
        .run_sweep_profile(MINIMAL_BAR_PROFILE, Some("bogus"))
        .expect_err("an unknown rank_by must not silently fall back to the default metric");
    assert!(err.contains("rank_by"), "names the bad argument: {err}");

    let err = client
        .run_walkforward_profile(MINIMAL_BAR_PROFILE)
        .expect_err("a profile with no [walkforward] table must not walk forward");
    assert!(err.contains("[walkforward]"), "names the missing section: {err}");

    assert!(
        client.features().iter().any(|f| f == "run_walkforward_profile"),
        "a lean server must advertise run_walkforward_profile: {:?}",
        client.features()
    );

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after three rejected profile requests");
}

// --- direct proto framing tests (no network) --------------------------------------------------

/// A `Request` and a `Response` survive `write_frame` -> `read_frame` unchanged over an in-memory
/// buffer — the pure framing contract. Uses the payload-free variants so the round-trip is
/// independent of the larger profile/report shapes.
#[test]
fn frame_round_trips_request_and_response() {
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Ping).unwrap();
    write_frame(&mut buf, &Response::Error("boom".to_string())).unwrap();

    let mut cur = Cursor::new(buf);
    let req: Request = read_frame(&mut cur).unwrap();
    assert!(matches!(req, Request::Ping));
    let resp: Response = read_frame(&mut cur).unwrap();
    match resp {
        Response::Error(m) => assert_eq!(m, "boom"),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// An oversized length prefix is rejected BEFORE the body allocation — the OOM guard. A raw prefix
/// of `MAX_FRAME_LEN + 1` must error on the length alone, before any body is read.
#[test]
fn oversized_length_prefix_is_rejected() {
    let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
    framed.push(0); // a trailing byte: the guard must fire on the length, not on a short read
    let mut cur = Cursor::new(framed);
    let err = read_frame::<_, Request>(&mut cur).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}
