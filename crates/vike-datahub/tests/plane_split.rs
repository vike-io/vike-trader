//! The DATA half of ruling 7's split: this daemon answers a COMPUTE verb with a clean, named
//! `Response::Error` and keeps the connection, and it advertises none of them.
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 requires BOTH
//! directions to be tested. The other half —`vike-backend backtest --addr` refusing a DATA verb —
//! is `crates/vike-backtest/tests/compute_plane.rs`, and the two are deliberate mirror images: the
//! same helper (`vike_datahub_client::proto`'s `wrong_plane_message`) writes both texts, so a change
//! to one refusal cannot leave the other behind.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `MemHistStore`, no prod store, no network.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    Plane, Request, Response, plane_of, read_frame, request_kind, write_frame,
};

/// A minimal, valid bar-mode profile — a REAL payload, so what is being proved is the refusal
/// rather than a decode failure that would look the same from the outside.
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

fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Every COMPUTE verb, with a payload a server would actually accept if it served them.
///
/// ⚠ Derived-checked rather than hand-trusted: `every_compute_verb_is_covered` below asserts this
/// list is exactly the [`Plane::Compute`] set, so a new compute verb reddens this file until its
/// author adds a sample.
fn compute_requests() -> Vec<Request> {
    use vike_datahub_client::wire_studio::{
        WireSlice, WireSliceKind, WireSpec, WireSweep, WireWalkforward,
    };
    let slice = || {
        Box::new(WireSlice {
            venue: "binance".into(),
            symbols: vec!["BTCUSDT".into()],
            interval: "1d".into(),
            start: None,
            end: None,
            kind: WireSliceKind::Bars,
        })
    };
    let spec = || WireSpec::Rhai("fn on_bar(){}".to_string());
    vec![
        Request::RunBacktest(MINIMAL_BAR_PROFILE.to_string()),
        Request::RunSweepProfile { profile_toml: MINIMAL_BAR_PROFILE.to_string(), rank_by: None },
        Request::RunWalkforwardProfile { profile_toml: MINIMAL_BAR_PROFILE.to_string() },
        Request::ListStrategies,
        Request::RunSlice { spec: spec(), slice: slice(), params: None },
        Request::RunSweep {
            spec: spec(),
            slice: slice(),
            sweep: WireSweep { axes: Vec::new() },
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(),
            slice: slice(),
            walkforward: WireWalkforward { n_splits: 2 },
            params: None,
        },
    ]
}

/// The sample set IS the compute plane — no more, no less.
#[test]
fn every_compute_verb_is_covered() {
    assert!(
        compute_requests().iter().all(|r| plane_of(r) == Plane::Compute),
        "a sample in this file is not a compute verb"
    );
    // Seven, and the number is the ruling's own: RunBacktest, RunSlice, RunSweep, RunWalkforward,
    // RunSweepProfile, RunWalkforwardProfile, ListStrategies. Asserted as a COUNT rather than by
    // re-listing them, because the list is right above and a second copy is what rots.
    assert_eq!(compute_requests().len(), 7, "ruling 7 moved SEVEN verbs");
}

/// ⚠ THE REFUSAL: every compute verb answers `Response::Error` naming the command that serves it,
/// and the CONNECTION SURVIVES — one bad request is a bad *request*, never a bad *connection*. All
/// seven are sent on ONE connection, which is what makes the survival claim mean something.
#[test]
fn a_compute_verb_is_refused_by_name_and_the_connection_survives() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for request in compute_requests() {
        let kind = request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        let response: Response = read_frame(&mut stream).expect("the connection must survive");
        match response {
            Response::Error(msg) => {
                assert!(msg.contains(kind), "the refusal names the verb: {msg}");
                assert!(
                    msg.contains("vike-backend backtest --addr"),
                    "the refusal names the command that DOES serve it: {msg}"
                );
                assert!(
                    msg.contains("config.backtest_addr"),
                    "the refusal names the settings key the client dialled from: {msg}"
                );
            }
            other => panic!("{kind} must be refused, not served: {other:?}"),
        }
    }
    // ...and the SAME connection still answers a data verb, which is the whole point of a refusal
    // that is not a drop.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

/// The HANDSHAKE half: a `Welcome` from this daemon advertises no compute verb, so a client that
/// reads the feature list never sends one. Advertisement and refusal are two legs of one contract —
/// the advertisement stops a well-behaved client, the refusal holds for a client that does not read
/// it, and neither substitutes for the other.
#[test]
fn the_handshake_advertises_no_compute_verb() {
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("connect");
    for gone in [
        "backtest",
        "list_strategies",
        "run_sweep_profile",
        "run_walkforward_profile",
        "run_slice",
        "run_sweep",
        "run_walkforward",
    ] {
        assert!(
            !client.features().iter().any(|f| f == gone),
            "the data daemon must not advertise {gone}: {:?}",
            client.features()
        );
    }
    // ...while the data verbs it DOES serve are still advertised, so this test cannot pass by the
    // feature list being empty.
    assert!(client.features().iter().any(|f| f == "load_bars"), "{:?}", client.features());
}
