//! The node's LINK-LIVENESS policy, against a real paper node on a real loopback socket.
//!
//! Two properties, both invisible from outside this module and both previously untested:
//!
//! 1. **An AUTHENTICATED connection outlives the unauthenticated handshake bound.** The 300 s
//!    handshake timeout was set on every accepted socket and never replaced, so a control peer —
//!    which never subscribes and therefore stays in the server's request/response read loop — was
//!    closed after five quiet minutes. That is the dominant trigger of the drop `vike-cli mcp`
//!    reports as "outcome UNKNOWN — may have executed": the node stopped reading before the
//!    command existed. It is also, deliberately, only HALF the property — the unauthenticated
//!    bound must still bite, and the second test holds it.
//! 2. **A subscribed stream that has nothing to say still says something.** One `Response::Pong`
//!    per idle heartbeat, so a client can deadline its read and stop mistaking a silent death for
//!    a quiet node.
//!
//! # Why this is a `#[cfg(test)]` child module and not an integration test
//!
//! Both properties are about NUMBERS — five minutes and fifteen seconds — and a test that used the
//! production ones would take five minutes and fifteen seconds. The seam that scales them
//! ([`LinkPolicy`], and `serve_with_link_policy` which takes it) is crate-private ON PURPOSE: two
//! of its three numbers are the server's half of a contract `vike-tradehub-client` owns, and a
//! public knob would let one side of that contract be changed alone. A child module of `server`
//! can reach it; `tests/` cannot. The idiom is `crates/vike-cli/src/cmd/mcp_node_drop_tests.rs`,
//! whose doc argues the same trade.
//!
//! Everything else here is real: a real `vike_run::build_paper_maker_core` node with its
//! `CommandSink` threaded in, the real `serve` accept loop, the real handshake over a real socket.
//! Nothing is scripted, because what is under test is the SERVER's behaviour and a scripted server
//! would be testing the script.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use vike_run::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, Topic, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

use super::*;
use crate::publish;

const TOKEN: &str = "LINK_LIVENESS_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-run offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"observe-key-for-the-link-liveness-tests";
const CONTROL_KEY: &[u8] = b"control-key-for-the-link-liveness-tests";

/// The scaled stand-in for the 300 s handshake bound. Everything the first test asserts is a
/// MULTIPLE of this, so the property is the same at either scale: "an authed connection is still
/// open long after the unauthenticated bound would have closed it".
const SCALED_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(300);
/// The scaled stand-in for the 15 s observe heartbeat (~150x down).
const SCALED_HEARTBEAT: Duration = Duration::from_millis(100);

/// The production policy with the two bounds scaled down. `authed_idle_timeout` is NOT scaled —
/// it is `None` in production and `None` here, because "no timeout" is the property under test and
/// substituting a small one for it would test the opposite thing.
fn scaled_policy() -> LinkPolicy {
    LinkPolicy {
        handshake_read_timeout: SCALED_HANDSHAKE_TIMEOUT,
        authed_idle_timeout: liveness::AUTHED_IDLE_TIMEOUT,
        observe_heartbeat: SCALED_HEARTBEAT,
    }
}

/// Build a PAPER node and serve it on an ephemeral loopback port under `policy`, with both scopes
/// keyed and the core's `CommandSink` threaded in (control ENABLED, so an accepted command really
/// reaches the core and answers with an `Ack` rather than a "control not enabled" refusal — the
/// difference between proving the connection is OPEN and proving it still WORKS).
fn spawn_node(policy: LinkPolicy) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    thread::spawn(move || {
        let _ = serve_with_link_policy(
            listener,
            publisher,
            keys,
            commands,
            ControlLimitsConfig::default(),
            None,
            None,
            policy,
        );
    });
    (mount, addr)
}

/// Complete the real handshake under `scope` and return the authed stream (NOT subscribed).
fn authed_stream(addr: SocketAddr, key: &[u8], scope: Scope) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, scope);
    write_frame(&mut stream, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: granted } if granted == scope => {}
        other => panic!("expected AuthOk({scope:?}), got {other:?}"),
    }
    stream
}

/// A resting limit far from any market — with no feed it never fills, so the node's book carries
/// exactly what was sent.
fn resting_submit(coid: &str) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.to_string(),
        venue: "polymarket".to_string(),
        symbol: TOKEN.to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.40),
        trigger_price: None,
        reduce_only: false,
    })
}

/// **THE DEFECT.** An authed control connection goes quiet for far longer than the unauthenticated
/// handshake bound, and must still be OPEN and still ACCEPT a command — because the bound that
/// closed it was written for strangers, and a control client's ordinary state is silence between
/// an operator's commands.
///
/// The idle stretch here is 4x the scaled bound; in production the ratio that matters is any pause
/// over five minutes, which is one long think by an agent or one coffee by a human.
///
/// ⚠ KILL PROOF: delete the `set_read_timeout(policy.authed_idle_timeout)` replacement in
/// `handle_connection` (leaving the handshake bound in force, which is what shipped) and this
/// fails — the node closes the link during the sleep, and the `Ack` read hits EOF. The commit
/// message carries both outcomes.
#[test]
fn an_authed_control_connection_survives_the_old_idle_timeout() {
    vike_log::test_init();
    let (mount, addr) = spawn_node(scaled_policy());
    let mut ctl = authed_stream(addr, CONTROL_KEY, Scope::Control);

    // Say NOTHING for four times the bound that used to close this connection.
    thread::sleep(SCALED_HANDSHAKE_TIMEOUT * 4);

    // Still open, and still a working control connection — not merely a socket that has not been
    // reaped yet. The node reads the frame, lowers it into the real core and acks the coid.
    let coid = "link-liveness-after-idle";
    write_frame(&mut ctl, &Request::Command { cmd: resting_submit(coid), reason: None })
        .expect("the node must still be reading this connection");
    match read_frame::<_, Response>(&mut ctl).expect("the node must still answer") {
        Response::Ack { coid: acked } => assert_eq!(acked, coid),
        other => panic!("an idle-then-used control connection must ack, got {other:?}"),
    }

    // …and the order really reached the node's book, off the core's own snapshot cell.
    let deadline = Instant::now() + Duration::from_secs(5);
    let booked = loop {
        let ids: Vec<String> = mount
            .handle
            .snapshot_cell()
            .load()
            .orders
            .iter()
            .map(|o| o.client_order_id.clone())
            .collect();
        if ids.iter().any(|id| id == coid) || Instant::now() >= deadline {
            break ids;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(booked.iter().any(|id| id == coid), "the command reached the core's book: {booked:?}");
}

/// The OTHER half, and the reason the fix is a REPLACEMENT rather than a deletion: an
/// UNAUTHENTICATED peer that opens a socket and says nothing is still closed on the handshake
/// bound. That bound is what stops a stranger pinning connection threads (`MAX_CONNECTIONS`'s doc
/// leans on it), and removing it while removing the authed one would have traded a self-inflicted
/// outage for a denial-of-service surface.
#[test]
fn an_unauthenticated_peer_is_still_closed_on_the_handshake_bound() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(scaled_policy());

    let mut silent = TcpStream::connect(addr).expect("connect");
    // Never send Hello. The node must give up on its own and close.
    silent.set_read_timeout(Some(SCALED_HANDSHAKE_TIMEOUT * 10)).expect("bounded test read");
    let started = Instant::now();
    let mut byte = [0u8; 1];
    let verdict = {
        use std::io::Read as _;
        silent.read(&mut byte)
    };
    assert!(
        matches!(verdict, Ok(0))
            || verdict.is_err()
                && verdict.as_ref().unwrap_err().kind() != io::ErrorKind::WouldBlock,
        "the node must CLOSE an unauthenticated peer that says nothing, got {verdict:?}"
    );
    assert!(
        started.elapsed() < SCALED_HANDSHAKE_TIMEOUT * 8,
        "…on the handshake bound, not on the test's own patience: {:?}",
        started.elapsed()
    );
}

/// **THE HEARTBEAT.** A subscribed stream with nothing to publish must still put something on the
/// wire, or a client has no way to tell it from a link that died in silence. A paper node with no
/// feed and no commands never publishes after its first frame (measured while writing the MCP drop
/// harness), so this is exactly the idle case: whatever arrives after the initial snapshot is a
/// heartbeat and nothing else.
///
/// Asserts more than one, because a single frame proves only that the writer wrote once — the
/// property is a CADENCE. And asserts the heartbeat is a `Pong` rather than a re-sent snapshot:
/// a repeated `seq` is a repeated FACT, and every consumer of this stream would then have to be
/// trusted forever to read it as liveness rather than as change (`run_push_writer`'s doc argues
/// the choice).
#[test]
fn an_idle_observe_stream_carries_a_heartbeat() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(scaled_policy());
    let mut obs = authed_stream(addr, OBSERVE_KEY, Scope::Observe);
    write_frame(&mut obs, &Request::Subscribe { topics: vec![Topic::All] }).expect("subscribe");
    // Generous per-read bound: it must not be what ends the loop on a healthy stream, and it is
    // 20x the scaled cadence so a loaded box does not fail this on scheduling alone.
    obs.set_read_timeout(Some(SCALED_HEARTBEAT * 20)).expect("bounded test read");

    let mut pongs = 0usize;
    let mut frames = 0usize;
    let deadline = Instant::now() + SCALED_HEARTBEAT * 30;
    while Instant::now() < deadline && pongs < 3 {
        match read_frame::<_, Response>(&mut obs) {
            Ok(Response::Pong) => pongs += 1,
            Ok(Response::SnapshotFrame(_)) => frames += 1,
            Ok(other) => panic!(
                "an idle subscribed stream carries frames and heartbeats only, got {other:?}"
            ),
            Err(e) => panic!("the node must keep the idle stream alive, got {e}"),
        }
    }
    assert!(
        pongs >= 3,
        "an idle subscribed stream must carry a heartbeat at a CADENCE (got {pongs} in \
         {:?}; {frames} real frames)",
        SCALED_HEARTBEAT * 30
    );
}

/// …and the capability is ADVERTISED, because the client arms its read deadline on the
/// advertisement alone. A node that heartbeated without saying so would be doing the work and
/// getting none of the benefit; one that advertised without heartbeating would make every client
/// tear down a healthy link. The two must move together, so the same test asserts both.
#[test]
fn a_heartbeating_node_advertises_the_capability() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(scaled_policy());
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { features, .. } => assert!(
            features.iter().any(|f| f == FEATURE_OBSERVE_HEARTBEAT),
            "the Welcome must advertise the heartbeat this node performs: {features:?}"
        ),
        other => panic!("expected Welcome, got {other:?}"),
    }
}
