//! Section 7: the two connection timings an operator may set (`config.node_idle_read_timeout_ms`,
//! `config.md_heartbeat_ms`) reach the wire, and a box without the rows runs what it always ran.
//!
//! Every wait here is a fraction of a second: the server's own seam is `serve_with_lanes`, whose last
//! parameter is the [`ServerTimings`] the boot builds from the rows, so a test hands it a short idle
//! timeout or heartbeat directly — the `serve_authed_with_handshake_deadline` idiom of
//! `preauth_bounds.rs`, for the same reason (the production values are minutes).

use super::*;
use std::assert_matches;

use vike_config::config::{
    MD_HEARTBEAT_MAX_MS, MD_HEARTBEAT_MIN_MS, NODE_IDLE_READ_TIMEOUT_MAX_MS,
    NODE_IDLE_READ_TIMEOUT_MIN_MS,
};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::market::{MD_HEARTBEAT, MD_HEARTBEAT_MAX, MD_HEARTBEAT_MIN, MdFrame};
use vike_node_proto::limits::{
    SERVER_IDLE_READ_TIMEOUT, SERVER_IDLE_READ_TIMEOUT_MAX, SERVER_IDLE_READ_TIMEOUT_MIN,
};

use super::md_mode_switch::mounted_hub;

/// The idle read timeout these tests run under: short enough that waiting it out costs nothing, long
/// enough that a loaded box still delivers a loopback `Ping` inside it.
const TEST_IDLE: Duration = Duration::from_millis(400);

// The point of injecting the value is that these tests are CHEAP; a test timeout that crept up to
// the production one would make them as slow as the real wait, silently.
const _: () = assert!(
    TEST_IDLE.as_millis() * 100 <= SERVER_IDLE_READ_TIMEOUT.as_millis(),
    "TEST_IDLE must stay two orders of magnitude under production's SERVER_IDLE_READ_TIMEOUT"
);

/// [`spawn`](super::spawn)'s server with `timings` in place of [`ServerTimings::PRODUCTION`], through
/// the entry the boot calls.
fn spawn_with_timings(
    keys: Option<NodeKeys>,
    md: Option<Arc<MdHub>>,
    timings: ServerTimings,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            keys,
            md,
            None,
            ServerLanes::default(),
            None,
            false,
            timings,
        );
    });
    addr
}

// ---- the defaults: a box without the rows --------------------------------------------------------

/// **A box with no row runs today's values, to the millisecond.** The two production timings are the
/// constants the server read before the rows existed, `from_rows(None, None)` is exactly them, and
/// the literals are spelled so that a changed constant is a visible edit to this file.
#[test]
fn a_box_without_the_rows_runs_the_constants_it_always_ran() {
    assert_eq!(ServerTimings::PRODUCTION.idle_read_timeout, SERVER_IDLE_READ_TIMEOUT);
    assert_eq!(ServerTimings::PRODUCTION.md_heartbeat, MD_HEARTBEAT);
    assert_eq!(ServerTimings::PRODUCTION.idle_read_timeout, Duration::from_millis(300_000));
    assert_eq!(ServerTimings::PRODUCTION.md_heartbeat, Duration::from_millis(15_000));
    assert_eq!(ServerTimings::from_rows(None, None), ServerTimings::PRODUCTION);
}

/// A row is its milliseconds, and ONE row leaves the other timing at production.
#[test]
fn a_row_is_its_milliseconds_and_one_row_leaves_the_other_alone() {
    let both = ServerTimings::from_rows(Some(120_000), Some(1_000));
    assert_eq!(both.idle_read_timeout, Duration::from_secs(120));
    assert_eq!(both.md_heartbeat, Duration::from_secs(1));

    let idle_only = ServerTimings::from_rows(Some(3_600_000), None);
    assert_eq!(idle_only.idle_read_timeout, Duration::from_secs(3600));
    assert_eq!(idle_only.md_heartbeat, ServerTimings::PRODUCTION.md_heartbeat);

    let heartbeat_only = ServerTimings::from_rows(None, Some(60_000));
    assert_eq!(heartbeat_only.idle_read_timeout, ServerTimings::PRODUCTION.idle_read_timeout);
    assert_eq!(heartbeat_only.md_heartbeat, Duration::from_secs(60));
}

// ---- the ranges `vike-config` restates ----------------------------------------------------------

/// **`vike-config` restates the idle row's range and this holds the restatement equal** — the
/// `RECONCILE_POLICIES` precedent: that crate cannot name `vike-node-proto`, so a number it refuses at
/// the write and the load is data there and a pin here. The default must lie inside it, or a box
/// without the row would run a value no box may write.
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

/// The same pin for the heartbeat row: `vike-config`'s ends are the client crate's
/// `MD_HEARTBEAT_MIN`/`MD_HEARTBEAT_MAX` (the range its compile-time assert holds `MD_HEARTBEAT`
/// to), and the default lies inside.
#[test]
fn the_heartbeat_range_is_the_one_vike_config_restates() {
    assert_eq!(u128::from(MD_HEARTBEAT_MIN_MS), MD_HEARTBEAT_MIN.as_millis());
    assert_eq!(u128::from(MD_HEARTBEAT_MAX_MS), MD_HEARTBEAT_MAX.as_millis());
    let default_ms = MD_HEARTBEAT.as_millis();
    assert!(
        u128::from(MD_HEARTBEAT_MIN_MS) <= default_ms
            && default_ms <= u128::from(MD_HEARTBEAT_MAX_MS),
        "the default {default_ms} ms lies outside the range a row is accepted in"
    );
}

// ---- the idle read timeout on the wire ----------------------------------------------------------

/// **A silent connection is reaped AT the configured idle timeout**, on a key-less server. Both
/// bounds: not held to the 300 s production default (the read below would otherwise run into this
/// test's own timeout, twenty idles), and not dropped at once.
#[test]
fn a_silent_connection_is_reaped_at_the_configured_idle_timeout() {
    let addr = spawn_with_timings(
        None,
        None,
        ServerTimings { idle_read_timeout: TEST_IDLE, ..ServerTimings::PRODUCTION },
    );
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

/// **On a KEYED server the configured idle timeout REPLACES the handshake deadline once `AuthOk` is
/// written**, and replaces it with the configured value, not the 300 s default: the handshake
/// deadline stays production's ten seconds here, so a server that left it armed would hold this peer
/// ten seconds (and one that applied the default, five minutes), both past this test's bound.
#[test]
fn an_authenticated_connection_is_reaped_at_the_configured_idle_timeout() {
    let addr = spawn_with_timings(
        Some(keys()),
        None,
        ServerTimings { idle_read_timeout: TEST_IDLE, ..ServerTimings::PRODUCTION },
    );
    let mut s = authed_stream(addr, &keys(), Scope::Read);
    s.set_read_timeout(Some(TEST_IDLE * 20)).expect("read timeout");
    let started = Instant::now();
    let answer = read_frame::<_, Response>(&mut s);
    let waited = started.elapsed();
    assert!(answer.is_err(), "an idle authenticated peer must be dropped: {answer:?}");
    assert!(
        waited >= TEST_IDLE / 2 && waited < TEST_IDLE * 10,
        "the idle authenticated peer was held {waited:?}, which is not the {TEST_IDLE:?} idle timeout"
    );
}

/// **The timeout is per GAP, not per connection lifetime**: a client that sends a request every
/// quarter of the timeout keeps its connection for three times the timeout, and every `Ping` is
/// answered. (The regression a lifetime cap would be.)
#[test]
fn a_connection_that_keeps_asking_outlives_the_idle_timeout() {
    let addr = spawn_with_timings(
        None,
        None,
        ServerTimings { idle_read_timeout: TEST_IDLE, ..ServerTimings::PRODUCTION },
    );
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(TEST_IDLE * 20)).expect("read timeout");
    let started = Instant::now();
    while started.elapsed() < TEST_IDLE * 3 {
        assert_matches!(
            exchange(&mut s, &Request::Ping),
            Response::Pong,
            "a connection with a request every {:?} was reaped after {:?}",
            TEST_IDLE / 4,
            started.elapsed()
        );
        thread::sleep(TEST_IDLE / 4);
    }
}

// ---- the heartbeat on the wire ------------------------------------------------------------------

/// **The configured heartbeat is the period the server SENDS and the period it KEEPS.** `MdSubscribed`
/// carries it (`heartbeat_ms`, the field the client derives its read deadline from), and a quiet
/// stream's first pushed frame is a `Heartbeat` after about that long — not after the production
/// fifteen seconds.
#[test]
fn the_configured_heartbeat_is_sent_in_the_reply_and_kept_by_the_writer() {
    let beat = Duration::from_millis(200);
    let addr = spawn_with_timings(
        None,
        Some(mounted_hub()),
        ServerTimings { md_heartbeat: beat, ..ServerTimings::PRODUCTION },
    );
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
    write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).expect("subscribe");
    match read_frame::<_, Response>(&mut s).expect("the positional reply") {
        Response::MdSubscribed { heartbeat_ms, .. } => assert_eq!(
            heartbeat_ms, 200,
            "the reply names the period this server runs, not the production default"
        ),
        other => panic!("expected MdSubscribed, got {other:?}"),
    }

    let started = Instant::now();
    match read_frame::<_, Response>(&mut s).expect("a quiet stream still writes") {
        Response::Md(frame) => assert_eq!(*frame, MdFrame::Heartbeat),
        other => panic!("a subscribed socket carries only Response::Md, got {other:?}"),
    }
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(5),
        "the first heartbeat took {waited:?}: the writer kept the production period, not {beat:?}"
    );
}

/// A server with NO heartbeat row tells the client the production period — the reply a client of any
/// version has always been given.
#[test]
fn a_server_without_the_row_replies_with_the_production_heartbeat() {
    let addr = spawn_with_timings(None, Some(mounted_hub()), ServerTimings::PRODUCTION);
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
    write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).expect("subscribe");
    match read_frame::<_, Response>(&mut s).expect("the positional reply") {
        Response::MdSubscribed { heartbeat_ms, .. } => {
            assert_eq!(u128::from(heartbeat_ms), MD_HEARTBEAT.as_millis());
        }
        other => panic!("expected MdSubscribed, got {other:?}"),
    }
}
