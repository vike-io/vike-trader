//! The backtesthub's idle read timeout is the CONFIGURED one: `config.node_idle_read_timeout_ms`
//! reaches `serve_authed`'s connection loop through `vike_node_proto::limits::server_idle_read_timeout`,
//! and a box without the row runs `SERVER_IDLE_READ_TIMEOUT`, byte for byte.
//!
//! The data daemon reads the same row; its twin of this file is
//! `crates/vike-datahub/tests/auth_roundtrip/connection_timings.rs`. Every wait here is a fraction of
//! a second: `serve_authed` takes the timeout as a parameter, so a test hands it a short one (the
//! production value is five minutes) — the `serve_authed_with_handshake_deadline` idiom of the data
//! daemon.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `MemHistStore`, no prod store, no network.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_backtest::named_run::NamedRunLane;
use vike_backtesthub::backtesthub_server::{StudyDirs, serve_authed};
use vike_config::config::{NODE_IDLE_READ_TIMEOUT_MAX_MS, NODE_IDLE_READ_TIMEOUT_MIN_MS};
use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::proto::{Request, Response, write_frame};
use vike_node_proto::frame::read_frame;
use vike_node_proto::limits::{
    SERVER_IDLE_READ_TIMEOUT, SERVER_IDLE_READ_TIMEOUT_MAX, SERVER_IDLE_READ_TIMEOUT_MIN,
    server_idle_read_timeout,
};

/// The timeout these tests run under: short enough that waiting it out costs nothing, long enough
/// that a loaded box still delivers a loopback `Ping` inside it.
const TEST_IDLE: Duration = Duration::from_millis(400);

// The point of injecting the value is that these tests are CHEAP; a test timeout that crept up to
// the production one would make them as slow as the real wait, silently.
const _: () = assert!(
    TEST_IDLE.as_millis() * 100 <= SERVER_IDLE_READ_TIMEOUT.as_millis(),
    "TEST_IDLE must stay two orders of magnitude under production's SERVER_IDLE_READ_TIMEOUT"
);

/// Spawn the backtesthub, key-less, with `idle_read_timeout`.
fn spawn_with_idle(idle_read_timeout: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            StudyDirs::working_directory(),
            None,
            NamedRunLane::DISARMED,
            idle_read_timeout,
        );
    });
    addr
}

/// **`vike-config` restates the row's range and this holds the restatement equal** — the
/// `RECONCILE_POLICIES` precedent (that crate cannot name `vike-node-proto`). The data daemon's pin
/// holds the same two numbers; the default must lie inside them, or a box without the row would run a
/// value no box may write.
#[test]
fn the_idle_read_timeout_range_is_the_one_vike_config_restates() {
    assert_eq!(u128::from(NODE_IDLE_READ_TIMEOUT_MIN_MS), SERVER_IDLE_READ_TIMEOUT_MIN.as_millis());
    assert_eq!(u128::from(NODE_IDLE_READ_TIMEOUT_MAX_MS), SERVER_IDLE_READ_TIMEOUT_MAX.as_millis());
    let default_ms = SERVER_IDLE_READ_TIMEOUT.as_millis();
    assert!(
        u128::from(NODE_IDLE_READ_TIMEOUT_MIN_MS) <= default_ms
            && default_ms <= u128::from(NODE_IDLE_READ_TIMEOUT_MAX_MS),
        "the default {default_ms} ms lies outside the range a row is accepted in"
    );
}

/// **A box with no row runs today's value**: the composition root hands `None` and gets the constant
/// (300 s, spelled out so a changed constant is a visible edit here); a row is its milliseconds.
#[test]
fn a_box_without_the_row_runs_the_constant_it_always_ran() {
    assert_eq!(server_idle_read_timeout(None), SERVER_IDLE_READ_TIMEOUT);
    assert_eq!(server_idle_read_timeout(None), Duration::from_millis(300_000));
    assert_eq!(server_idle_read_timeout(Some(600_000)), Duration::from_secs(600));
}

/// **A silent connection is reaped AT the configured idle timeout.** Both bounds: not held to the
/// 300 s production default (the read would otherwise run into this test's own timeout, twenty
/// idles), and not dropped at once.
#[test]
fn a_silent_connection_is_reaped_at_the_configured_idle_timeout() {
    let addr = spawn_with_idle(TEST_IDLE);
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(TEST_IDLE * 20)).expect("read timeout");
    let started = Instant::now();
    let answer = read_frame::<_, Response>(&mut s);
    let waited = started.elapsed();
    assert!(answer.is_err(), "a silent peer must be dropped, not served: {answer:?}");
    assert!(
        waited >= TEST_IDLE / 2 && waited < TEST_IDLE * 10,
        "the silent peer was held {waited:?}, which is not the {TEST_IDLE:?} idle timeout"
    );
}

/// **The timeout is per GAP, not per connection lifetime**: a client that sends a request every
/// quarter of the timeout keeps its connection for three times the timeout, and every `Ping` is
/// answered. (The regression a lifetime cap would be.)
#[test]
fn a_connection_that_keeps_asking_outlives_the_idle_timeout() {
    let addr = spawn_with_idle(TEST_IDLE);
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(TEST_IDLE * 20)).expect("read timeout");
    let started = Instant::now();
    while started.elapsed() < TEST_IDLE * 3 {
        write_frame(&mut s, &Request::Ping).expect("ping");
        let answer = read_frame::<_, Response>(&mut s);
        assert!(
            matches!(answer, Ok(Response::Pong)),
            "a connection with a request every {:?} was reaped after {:?}: {answer:?}",
            TEST_IDLE / 4,
            started.elapsed()
        );
        thread::sleep(TEST_IDLE / 4);
    }
}
