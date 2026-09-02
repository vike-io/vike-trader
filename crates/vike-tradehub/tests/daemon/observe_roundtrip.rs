//! End-to-end observe-server tests (headless two-layer plan, Layer 2, PR-11) — loopback only, no
//! creds, no external network, no `polymarket` feature.
//!
//! A real PAPER node ([`vike_run::build_paper_maker_core`]) + the real [`vike_tradehub::server`] +
//! [`vike_tradehub::publish`] fan-out are bound on an ephemeral `127.0.0.1:0` port, and a real
//! [`vike_tradehub_client::RemoteCoreHandle`] connects over it. The paper mount fills nothing without
//! a feed (a resting limit far from any market simply rests), so the ONLY order in the book is the
//! operator's — which makes the pushed-snapshot assertion deterministic.
//!
//! What is proven:
//! - **Roundtrip:** an operator `Command` driven into the node appears in a PUSHED `WireSnapshot` at
//!   the remote observe handle (S6/S9 — the read-only, push-fed observer).
//! - **Auth refusals:** an unauthenticated `Subscribe` is refused; a `Control`-scope auth is denied;
//!   a `Command` under `Observe` is denied (read-only server, no control path until PR-12).
//! - **Isolation:** a deliberately-stalled subscriber (subscribed, never reading) does NOT stall the
//!   publisher — a second, healthy client keeps receiving updates. This is the per-connection
//!   bounded/drop-oldest mailbox guarantee that keeps the p99 core-hop gate safe.
//!
//! The publisher NEVER touches the vike-core fold (it reads only the arc-swap snapshot cell), so the
//! `cargo test -p vike-core --release --test runtime_latency -- --ignored` p99 gate is the merge
//! condition for PR-11 and is unaffected by anything here — see `publish.rs`'s module doc.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use vike_exec::{Command, OrderIntent};
use vike_model::OrderRequest;
use vike_run::{build_paper_maker_core, MakerMount, MakerMountConfig};
use vike_tradehub::{publish, server};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    read_frame, write_frame, Request, Response, Scope, Topic, NODE_PROTO_VERSION,
};
use vike_tradehub_client::{NodeKeys, RemoteCoreHandle, WireCommand};

const TOKEN: &str = "OBSERVE_ROUNDTRIP_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-run offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;
/// The observe key both sides share in these tests.
const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-tests";

/// Poll `cond` up to `secs`, returning whether it became true — the core folds on its own thread, the
/// publisher fans out on another, and the client receives on a third, so this just waits for the push
/// to propagate.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
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

/// Build a PAPER node and start the observe server + publisher on an ephemeral loopback port. The
/// server is OBSERVE-ONLY (no control key). Returns the live mount (its `CoreHandle` drives commands
/// and keeps the core running) and the assigned address.
fn spawn_node(observe_key: &[u8]) -> (MakerMount, SocketAddr) {
    spawn_node_with(observe_key, None)
}

/// [`spawn_node`] with the REQ-2 datahub advertisement threaded — `Some(addr)` is what a daemon
/// with `config.toml`'s `datahub_advertise_addr` set passes into `server::serve`, so the
/// advertisement tests below run the REAL server path, not a scripted Welcome.
fn spawn_node_with(
    observe_key: &[u8],
    datahub_advertise: Option<&str>,
) -> (MakerMount, SocketAddr) {
    let datahub_advertise = datahub_advertise.map(str::to_string);
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    // A real identity block (split-plane B3): the roundtrip below asserts it survives
    // project → frame → wire → RemoteCoreHandle verbatim.
    let publisher = publish::spawn(
        mount.handle.snapshot_cell(),
        Some(vike_tradehub_client::wire::WireNodeIdentity {
            name: "observe-roundtrip".into(),
            strategy: "spread_maker".into(),
            params: "qty=1".into(),
            live: false,
            build: "test-build".into(),
        }),
    );
    // Observe-only: control key ABSENT, so even a valid Control mac cannot authenticate (also refused
    // outright by the observe server). The server thread OWNS the publisher handle, keeping the poll
    // thread alive for the test's lifetime.
    let keys = NodeKeys::new(observe_key.to_vec(), Vec::new());
    // Observe-only node: NO control sink (the control path is exercised by control_roundtrip.rs).
    thread::spawn(move || {
        // Default edge limits (no size cap, default rate) — the caller-owned config seam (audit
        // F13); an observe-only node never reaches the Command arm anyway. No settings source
        // either — the SettingsShow verb has its own suite (tests/daemon/settings_show.rs).
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            None,
            datahub_advertise,
        );
    });
    (mount, addr)
}

/// Submit a resting limit far from any market — it stays WORKING (never fills without a feed), so the
/// snapshot deterministically carries exactly this one order.
fn drive_resting_order(mount: &MakerMount, coid: &str) {
    let req = OrderRequest {
        client_order_id: coid.to_string(),
        venue: "polymarket".to_string(),
        symbol: TOKEN.to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.40),
        ..Default::default()
    };
    mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
}

/// Complete the Hello -> Welcome -> Auth(Observe) handshake over a raw stream and return it authed
/// (NOT yet subscribed) — the low-level twin of `RemoteCoreHandle::connect` for the refusal/stall
/// tests that need to control the wire directly.
fn authed_observe_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Observe);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Observe, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Observe } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    stream
}

#[test]
fn observe_client_sees_a_driven_order_pushed() {
    vike_log::test_init();
    let (mount, addr) = spawn_node(OBSERVE_KEY);

    // Connect + handshake + auth(Observe) + subscribe, all inside `connect`.
    let remote = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("connect + auth + subscribe");
    assert!(remote.is_connected(), "the receive thread is live right after connect");

    // Drive an operator order into the node through the SAME seam the stdio control uses.
    let coid = "op-1";
    drive_resting_order(&mount, coid);

    // The pushed WireSnapshot must reflect the order (wait for fold -> publish -> push -> store).
    assert!(
        wait_until(5, || remote.snapshot().orders.iter().any(|o| o.client_order_id == coid)),
        "the pushed WireSnapshot never reflected the driven operator order"
    );
    let snap = remote.snapshot();
    let ov = snap.orders.iter().find(|o| o.client_order_id == coid).expect("order present");
    assert_eq!(ov.symbol, TOKEN, "the order routed to the mount symbol");
    assert_eq!(ov.side, 1, "the order side survived the projection + wire round-trip");
    assert_eq!(snap.venue, "polymarket", "the snapshot carries the node's primary venue");
    let id = snap.identity.as_ref().expect("the identity block crossed the wire (B3)");
    assert_eq!(id.name, "observe-roundtrip");
    assert!(!id.live, "a paper mount publishes live: false");
    assert!(remote.is_connected(), "still connected after receiving frames");
}

/// REQ-2 end to end over the REAL server: a node configured with a datahub advertisement carries
/// `datahub=<addr>` in its Welcome, and the client handshake exposes it verbatim as
/// [`RemoteCoreHandle::advertised_datahub`] — the seam vike-app's one resolution point
/// (`vike_app_core::datahub_resolve::resolve_datahub_addr`) fills its `advertised` input from.
#[test]
fn a_configured_node_advertises_its_datahub_to_the_connected_client() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node_with(OBSERVE_KEY, Some("127.0.0.1:7878"));

    let remote = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("connect + auth + subscribe");
    assert_eq!(
        remote.advertised_datahub(),
        Some("127.0.0.1:7878"),
        "the Welcome's datahub advertisement crossed the handshake verbatim"
    );
    assert!(remote.is_connected(), "the value-carrying feature entry broke nothing else");
}

/// …and a node with NO advertisement configured (the default — every other test in this file)
/// exposes `None`: absence of the entry is the answer, never an empty string.
#[test]
fn an_unconfigured_node_advertises_no_datahub() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);

    let remote = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("connect + auth + subscribe");
    assert_eq!(remote.advertised_datahub(), None);
}

#[test]
fn unauthenticated_subscribe_is_refused() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);

    // Send Subscribe with NO prior Hello/Auth — the server must refuse (never register) and close.
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Subscribe { topics: vec![Topic::All] }).expect("subscribe");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { .. } => {}
        other => panic!("an unauthenticated Subscribe must be AuthDenied, got {other:?}"),
    }
}

#[test]
fn control_scope_auth_is_denied() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);

    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    // Even a well-formed Control mac (signed with the shared key) is refused: the observe server has
    // no control path (PR-12).
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Control);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Control, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { .. } => {}
        other => panic!("Control-scope auth must be AuthDenied, got {other:?}"),
    }
}

#[test]
fn a_command_under_observe_is_denied() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);

    // Authenticated as Observe, a Command is still refused — the server is read-only.
    let mut stream = authed_observe_stream(addr, OBSERVE_KEY);
    let cmd = Request::Command { cmd: WireCommand::Cancel("x".into()), reason: None };
    write_frame(&mut stream, &cmd).expect("command");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { .. } => {}
        other => panic!("a Command under Observe must be AuthDenied, got {other:?}"),
    }
}

/// The B4 scope split on ONE connection: an Observe peer CAN read `StrategyStatus` (read-only, so
/// Observe suffices — same as `Snapshot`) and CANNOT write `UpdateParams` (a Command, refused by
/// the same read-only gate as every order verb).
#[test]
fn observe_scope_can_strategy_status_but_not_update_params() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);
    let mut stream = authed_observe_stream(addr, OBSERVE_KEY);

    // The read: answered, and with the MOUNTED truth (the identity block spawn_node published).
    write_frame(&mut stream, &Request::StrategyStatus).expect("status request");
    match read_frame::<_, Response>(&mut stream).expect("status response") {
        Response::StrategyStatus(status) => {
            assert_eq!(status.identity.name, "observe-roundtrip");
            assert_eq!(status.identity.strategy, "spread_maker");
            assert!(!status.identity.live, "a paper mount reports live: false");
            assert_eq!(status.effective_params, "qty=1");
            assert_eq!(status.mounts.len(), 1, "today's daemon mounts exactly one strategy");
            assert_eq!(status.mounts[0].strategy, "spread_maker");
            assert_eq!(status.mounts[0].params, "qty=1");
            assert!(!status.mounts[0].live);
        }
        other => panic!("StrategyStatus under Observe must answer, got {other:?}"),
    }

    // The write: refused read-only, exactly like every other Command under Observe.
    let cmd = Request::Command {
        cmd: WireCommand::UpdateParams {
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            interval: "1m".into(),
            params: serde_json::json!({"SpreadMaker": {"qty": 2.0}}),
        },
        reason: None,
    };
    write_frame(&mut stream, &cmd).expect("update_params");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { .. } => {}
        other => panic!("UpdateParams under Observe must be AuthDenied, got {other:?}"),
    }
}

/// The high-level client fn drives the same read end-to-end — and its success also proves the
/// server ADVERTISES `strategy-verbs` in `Welcome.features`, because `strategy_status` refuses
/// client-side (sending nothing) when the feature is absent.
#[test]
fn strategy_status_client_fn_round_trips_and_proves_the_advertisement() {
    vike_log::test_init();
    let (_mount, addr) = spawn_node(OBSERVE_KEY);
    let status = vike_tradehub_client::strategy_status(addr, OBSERVE_KEY)
        .expect("the real server advertises strategy-verbs, so the client sends");
    assert_eq!(status.identity.name, "observe-roundtrip");
    assert_eq!(status.mounts.len(), 1);
    assert_eq!(status.mounts[0].strategy, "spread_maker");
}

#[test]
fn a_stalled_client_does_not_stall_the_publisher() {
    vike_log::test_init();
    let (mount, addr) = spawn_node(OBSERVE_KEY);

    // Client A: authenticate + subscribe over a RAW stream, then NEVER read it (a deliberate stall).
    // Held in scope through the assertion so it is genuinely subscribed-and-not-draining meanwhile.
    let mut stalled = authed_observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut stalled, &Request::Subscribe { topics: vec![Topic::All] }).expect("subscribe");

    // Client B: a healthy observer that keeps draining.
    let healthy = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("healthy connect");

    // Drive a command; the HEALTHY client must still receive the update while A is stalled — the
    // per-connection bounded/drop-oldest mailboxes isolate A's stall from the publisher and from B.
    let coid = "op-2";
    drive_resting_order(&mount, coid);
    assert!(
        wait_until(5, || healthy.snapshot().orders.iter().any(|o| o.client_order_id == coid)),
        "the healthy client did not receive updates while another client was stalled"
    );

    drop(stalled); // keep A alive until here, then release it
}
