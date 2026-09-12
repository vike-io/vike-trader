//! The node's LINK-LIVENESS policy, against a real paper node on a real loopback socket.
//!
//! Three properties, all invisible from outside this module and each previously untested:
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
//! 3. **A subscriber that stops READING is closed, on a bound.** The push writer had no write
//!    timeout, so a peer that kept its socket open and drained nothing parked the node's
//!    connection thread in `write_all` for as long as it liked — the heartbeat ends a DEAD peer,
//!    never a live one that does not read. `PUSH_WRITE_TIMEOUT` ends it now, and the last test
//!    proves the thread and its slot come back.
//!
//! # Why this is a `#[cfg(test)]` child module and not an integration test
//!
//! All three properties are about NUMBERS — five minutes, fifteen seconds and thirty seconds — and
//! a test that used the production ones would take that long. The seam that scales them
//! ([`LinkPolicy`], and `serve_with_link_policy` which takes it) is crate-private ON PURPOSE: two
//! of its numbers are the server's half of a contract `vike-tradehub-client` owns, and a public
//! knob would let one side of that contract be changed alone. A child module of `server` can
//! reach it; `tests/` cannot. The idiom is `crates/vike-cli/src/cmd/mcp_node_drop_tests.rs`,
//! whose doc argues the same trade.
//!
//! Everything else here is real: a real `vike_run::build_paper_maker_core` node with its
//! `CommandSink` threaded in, the real `serve` accept loop, the real handshake over a real socket.
//! Nothing is scripted, because what is under test is the SERVER's behaviour and a scripted server
//! would be testing the script. The one deviation is the third property's node, which serves the
//! real publisher over a TEST-OWNED snapshot cell instead of a core — its test says why.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use vike_core::snapshot::CoreSnapshot;
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
/// The scaled stand-in for the 30 s push write bound (150x down, the heartbeat's factor, so the
/// two keep their production ratio: two heartbeats to one write bound).
const SCALED_PUSH_WRITE_TIMEOUT: Duration = Duration::from_millis(200);
/// How long the stall test waits for the node to give a non-reading subscriber up before calling
/// the bound absent: 25x the scaled bound, so a loaded box cannot fail it on scheduling alone, and
/// still five seconds rather than forever — which is what the kill proof runs into.
const STALL_DEADLINE: Duration = Duration::from_millis(5_000);
/// The payload one stall frame carries, chosen against what a loopback socket pair BUFFERS: Linux
/// holds a few hundred KiB to a few MiB between the two kernel buffers (`tcp_wmem`/`tcp_rmem`
/// defaults; the receive side autotunes upward only while the application READS, which this peer
/// never does), so a couple of these fill it, and the test keeps publishing them until the writer
/// gives up so the bound is proven against a full buffer rather than a lucky first frame. Far
/// under `MAX_FRAME_LEN`, and small enough that the mailbox's eight of them are a tolerable
/// footprint for one test process.
const STALL_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// The production policy with the three finite bounds scaled down. `authed_idle_timeout` is NOT
/// scaled — it is `None` in production and `None` here, because "no timeout" is the property under
/// test and substituting a small one for it would test the opposite thing.
fn scaled_policy() -> LinkPolicy {
    LinkPolicy {
        handshake_read_timeout: SCALED_HANDSHAKE_TIMEOUT,
        authed_idle_timeout: liveness::AUTHED_IDLE_TIMEOUT,
        observe_heartbeat: SCALED_HEARTBEAT,
        push_write_timeout: Some(SCALED_PUSH_WRITE_TIMEOUT),
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

/// Serve the real server over a TEST-OWNED snapshot cell — no core, no `CommandSink` — on an
/// ephemeral loopback port under `policy`, both scopes keyed exactly as `spawn_node` keys them.
/// For the one property whose proof needs frames ON DEMAND (the push write bound): the test bumps
/// the cell's `seq` and the real publisher polls, frames and fans out exactly as it does under the
/// daemon. Returns the publisher handle too, because its `subscriber_count` is how the writer
/// thread's end is observed from outside it.
fn spawn_publisher_node(
    cell: Arc<ArcSwap<CoreSnapshot>>,
    policy: LinkPolicy,
) -> (PublisherHandle, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(cell, None);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let served = publisher.clone();
    thread::spawn(move || {
        let _ = serve_with_link_policy(
            listener,
            served,
            keys,
            None,
            ControlLimitsConfig::default(),
            None,
            None,
            policy,
        );
    });
    (publisher, addr)
}

/// Poll `cond` every 10 ms until it holds or `within` elapses; the verdict is whether it held.
fn wait_until(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// **THE PUSH WRITE BOUND.** A subscriber that authenticates, subscribes and then NEVER reads must
/// cost the node its writer thread for the write bound and no longer — not for as long as the peer
/// feels like keeping its socket open. This is the LIVE-peer stall the heartbeat cannot end: the
/// client's kernel keeps ACKing until its receive window is shut, so no write ever FAILS; it parks.
///
/// Served over a test-owned snapshot cell rather than a paper node, and the reason is the mechanism
/// under test: to park a write the node has to have written MORE than the two kernel buffers
/// between it and the peer will hold, and a paper node with no feed publishes one small frame and
/// then nothing (measured while writing the heartbeat test above). So this test drives the SAME
/// publisher and the SAME server the daemon runs and forces frames by bumping `seq` on a snapshot
/// whose `recent_events` carries [`STALL_FRAME_BYTES`] of payload — a field the projection passes
/// through verbatim.
///
/// Observed from OUTSIDE the writer thread, two ways. The publisher's subscriber count returns to
/// zero (a `Subscription` deregisters on drop, which only `run_push_writer` returning can cause),
/// and it does so no EARLIER than the bound — which is what says the exit was the timeout and not
/// some other write fault. Then the client's socket reads through to EOF: the node CLOSED the
/// connection rather than merely stopping its feed, which is the property "a timed-out write ends
/// the connection".
///
/// ⚠ KILL PROOF: set `push_write_timeout: None` in `scaled_policy` (the pre-bound behaviour) and
/// this fails on [`STALL_DEADLINE`] with the subscriber still registered — the writer is parked in
/// `write_all` and nothing ends it. The commit message carries both outcomes.
#[test]
fn a_subscriber_that_stops_reading_is_closed_on_the_push_write_bound() {
    vike_log::test_init();
    let cell = Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("polymarket", TOKEN)));
    let (publisher, addr) = spawn_publisher_node(Arc::clone(&cell), scaled_policy());

    let mut stalled = authed_stream(addr, OBSERVE_KEY, Scope::Observe);
    write_frame(&mut stalled, &Request::Subscribe { topics: vec![Topic::All] }).expect("subscribe");
    // From here on this peer reads NOTHING — but it stays connected: `stalled` lives to the end of
    // the test, so nothing on this side ever closes or resets the socket. A dropped stream would
    // fail the node's write outright and prove the OTHER, already-working exit path.

    // The node registers the subscription once it has read the Subscribe.
    assert!(
        wait_until(STALL_DEADLINE, || publisher.subscriber_count() == 1),
        "the node must register the subscriber (count {})",
        publisher.subscriber_count()
    );
    let parked_at = Instant::now();

    // Publish oversized frames until the writer gives the peer up — or the deadline says it never
    // will. Each bump is a distinct `seq`, which is what the publisher frames on.
    let payload: Arc<str> = Arc::from("x".repeat(STALL_FRAME_BYTES));
    let mut seq = 1u64;
    let dropped = loop {
        if publisher.subscriber_count() == 0 {
            break true;
        }
        if parked_at.elapsed() >= STALL_DEADLINE {
            break false;
        }
        let mut snap = CoreSnapshot::empty("polymarket", TOKEN);
        snap.seq = seq;
        snap.recent_events = vec![Arc::clone(&payload)];
        cell.store(Arc::new(snap));
        seq += 1;
        thread::sleep(Duration::from_millis(20));
    };
    let took = parked_at.elapsed();
    assert!(
        dropped,
        "a subscriber that reads nothing must be dropped on the push write bound; still registered \
         after {took:?} and {} oversized frames",
        seq - 1
    );
    assert!(
        took >= SCALED_PUSH_WRITE_TIMEOUT,
        "…and dropped by the TIMEOUT, not by some other write fault: a parked write cannot end \
         before the bound, yet the subscriber was gone after {took:?}"
    );

    // …and the CONNECTION is closed, not just the subscription: the node's side is gone, so
    // draining what it managed to buffer ends in EOF (or a reset) rather than in more frames.
    stalled.set_read_timeout(Some(STALL_DEADLINE)).expect("bounded test read");
    let mut sink = vec![0u8; 64 * 1024];
    let verdict = loop {
        use std::io::Read as _;
        match stalled.read(&mut sink) {
            Ok(0) => break Ok(()),
            Ok(_) => continue,
            Err(e) => break Err(e),
        }
    };
    assert!(
        matches!(verdict, Ok(()))
            || verdict.as_ref().is_err_and(|e| {
                e.kind() != io::ErrorKind::WouldBlock && e.kind() != io::ErrorKind::TimedOut
            }),
        "the node must CLOSE a subscriber it timed out, got {verdict:?}"
    );
}
