//! The node's door listens BEFORE its engines are up, says `starting`, and never anything else
//! (2026-10-17, the v0.1.64 deploy: a hung venue kept 7879 closed past the deploy helper's health
//! window and the deploy rolled back).
//!
//! What is proven, over real loopback sockets and the real handshake:
//! - while the start never finishes (the door is never handed over — the "blocks forever" half), an
//!   authenticated peer's every verb is answered `Response::Error("starting: ...")` carrying the
//!   per-account progress: a snapshot read, a subscribe, and — the fail-closed half — an ORDER
//!   command under the control scope, which is refused and reaches nothing;
//! - an unauthenticated peer (a wrong key) learns nothing: the same denial the real server gives;
//! - the thin client the GUI uses connects, then reads the node as not connected (its existing
//!   reconnect state) and never holds a snapshot;
//! - after the hand-over the SAME listener is served by the real server: the state flips, a snapshot
//!   is pushed and the same control command is now ACKed;
//! - the stdout `ready` line is printed only after the server took the listener over: pinned by the
//!   order of the calls in `tradehub_cli.rs` (the live start itself needs a gatewayhub and venues).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_log::poll::wait_until;
use vike_tradehub::server::starting::{AccountStart, StartProgress, StartingDoor};
use vike_tradehub::{publish, server};
use vike_tradehub_client::TradehubCoreHandle;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response, is_starting_error};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};

const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-the-starting-door";
const CONTROL_KEY: &[u8] = b"control-secret-key-for-the-starting-door";
const TOKEN: &str = "STARTING_DOOR_TOKEN";
const RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;

fn keys() -> HubKeys {
    HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// A listener on an ephemeral loopback port, and a door answering on it with two accounts in
/// different states.
fn open_door() -> (TcpListener, SocketAddr, StartProgress, StartingDoor) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("assigned port");
    let progress = StartProgress::new();
    progress.phase("starting the accounts");
    progress.account("bybit", AccountStart::Up);
    progress.account("ibkr#A", AccountStart::Starting);
    let door = StartingDoor::open(&listener, keys(), None, true, false, progress.clone())
        .expect("the door opens on a fresh listener");
    (listener, addr, progress, door)
}

/// `Hello`, then `Auth` under `scope` signed with `key`; the authed stream, or the reply that was
/// not `AuthOk`.
fn handshake(addr: SocketAddr, key: &[u8], scope: Scope) -> Result<TcpStream, HandshakeReply> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
        .expect("hello");
    let HandshakeReply::Welcome { nonce: Some(nonce), .. } =
        read_frame::<_, HandshakeReply>(&mut stream).expect("welcome")
    else {
        panic!("expected Welcome");
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, scope);
    write_frame(&mut stream, &HandshakeFrame::Auth { scope, mac }).expect("auth");
    match read_frame::<_, HandshakeReply>(&mut stream).expect("auth reply") {
        HandshakeReply::AuthOk { .. } => Ok(stream),
        other => Err(other),
    }
}

fn resting_submit(order_client_id: &str) -> Request {
    Request::Command {
        command: CommandWire::Submit(OrderRequestWire {
            order_client_id: order_client_id.to_string(),
            venue: "polymarket".to_string(),
            symbol: TOKEN.to_string(),
            side: 1,
            qty: 20.0,
            order_type: "limit".to_string(),
            price: Some(0.40),
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail: None,
            limit_offset: None,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        }),
        reason: None,
    }
}

/// The text of the `Error` the next frame must be, with the `starting:` marker checked.
fn starting_text(stream: &mut TcpStream) -> String {
    match read_frame::<_, Response>(stream).expect("the door answers") {
        Response::Error(text) if is_starting_error(&text) => text,
        other => panic!("a node that is still starting answers `starting:`, got {other:?}"),
    }
}

/// **The door's answer reaches each client generation in the shape it reads** (decision 0112, owner's Q8): a client
/// that never says it reads the refusal frame gets the `Error` it always read, and one that does (its `Reads` after
/// the handshake, answered with nothing) gets the frame, whose ID says "starting", with the very same text.
#[test]
fn the_door_answers_the_starting_frame_only_to_a_client_that_reads_it() {
    use vike_hub_protocol::features::FEATURE_REFUSAL_FRAME;
    use vike_tradehub_client::refusals::{TradehubRefusalId, frame_outcome};
    use vike_utility::errors::FailureOutcome;

    let (_listener, addr, _progress, _door) = open_door();
    let mut older = handshake(addr, OBSERVE_KEY, Scope::Read).expect("observe auth");
    write_frame(&mut older, &Request::Snapshot).expect("snapshot request");
    let older_text = starting_text(&mut older);

    let mut reader = handshake(addr, OBSERVE_KEY, Scope::Read).expect("observe auth");
    write_frame(
        &mut reader,
        &HandshakeFrame::Reads { features: vec![FEATURE_REFUSAL_FRAME.into()] },
    )
    .expect("reads");
    write_frame(&mut reader, &Request::Snapshot).expect("snapshot request");
    match read_frame::<_, Response>(&mut reader).expect("the door answers") {
        Response::Refused(refusal) => {
            assert_eq!(
                TradehubRefusalId::from_word(&refusal.id),
                Some(TradehubRefusalId::Starting)
            );
            assert_eq!(frame_outcome(&refusal), FailureOutcome::NotNow);
            assert_eq!(refusal.text, older_text, "the same words as the older client's Error");
        }
        other => panic!("the starting frame, got {other:?}"),
    }
}

#[test]
fn a_start_that_never_finishes_keeps_the_door_answering_starting_and_nothing_else() {
    let (_listener, addr, _progress, _door) = open_door();

    // A read: the state, with the per-account progress.
    let mut observe = handshake(addr, OBSERVE_KEY, Scope::Read).expect("observe auth");
    write_frame(&mut observe, &Request::Snapshot).expect("snapshot request");
    let text = starting_text(&mut observe);
    assert!(text.contains("starting the accounts"), "the phase is said: {text}");
    assert!(text.contains("bybit up"), "an account that is up is said: {text}");
    assert!(text.contains("ibkr#A starting"), "an account still starting is said: {text}");

    // A subscribe is answered the same way and the connection closes: no frame stream starts.
    let mut subscriber = handshake(addr, OBSERVE_KEY, Scope::Read).expect("observe auth");
    write_frame(&mut subscriber, &Request::Subscribe { topics: vec![] }).expect("subscribe");
    starting_text(&mut subscriber);
    assert!(
        read_frame::<_, Response>(&mut subscriber).is_err(),
        "nothing follows a `starting` answer: the connection is closed, never a snapshot stream"
    );

    // THE FAIL-CLOSED HALF: an order under the control scope is refused with `starting`.
    let mut control = handshake(addr, CONTROL_KEY, Scope::Write).expect("control auth");
    write_frame(&mut control, &resting_submit("never-placed")).expect("command");
    let text = starting_text(&mut control);
    assert!(text.contains("bybit up"), "the refusal says where the start is: {text}");

    // A wrong key is denied exactly as the real server denies it, and learns nothing.
    match handshake(addr, b"not-the-observe-key", Scope::Read) {
        Err(HandshakeReply::AuthDenied { .. }) => {}
        other => panic!("a wrong key must be AuthDenied, got {other:?}"),
    }
    // A control scope on a node holding no control key is denied too (the same admission check).
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let no_control = listener.local_addr().expect("addr");
    let _door = StartingDoor::open(
        &listener,
        HubKeys::new(OBSERVE_KEY.to_vec(), Vec::new()),
        None,
        true,
        false,
        StartProgress::new(),
    )
    .expect("open");
    match handshake(no_control, CONTROL_KEY, Scope::Write) {
        Err(HandshakeReply::AuthDenied { reason }) => {
            assert!(reason.contains("control disabled"), "reason: {reason}");
        }
        other => panic!("control without a control key must be AuthDenied, got {other:?}"),
    }
}

/// **The control link reads a starting node as NOT NOW, never as a refusal**: the client of this build says it
/// reads the frame, the door answers the order with its `starting` id, and the worker decides once.
#[test]
fn the_control_link_reads_a_starting_node_as_not_now() {
    use vike_tradehub_client::refusals::TradehubRefusal;
    use vike_tradehub_client::{CommandOutcome, CommandRefusal, TradehubControlHandle};

    let (_listener, addr, progress, _door) = open_door();
    let control =
        TradehubControlHandle::connect(addr, CONTROL_KEY).expect("the handshake succeeds");
    let Request::Command { command, .. } = resting_submit("never-placed") else {
        panic!("resting_submit builds a command")
    };
    let ticket = control.try_command(command).expect("enqueued");
    let starting = TradehubRefusal::Starting { progress: progress.describe() };
    assert_eq!(
        control.await_outcome(ticket, Duration::from_secs(10)),
        Some(CommandOutcome::NotNow(CommandRefusal::Frame(starting.into())))
    );
}

#[test]
fn the_thin_client_reads_a_starting_node_as_not_connected_and_holds_no_snapshot() {
    let (_listener, addr, _progress, _door) = open_door();
    let observer = TradehubCoreHandle::connect(addr, OBSERVE_KEY).expect("the handshake succeeds");
    assert!(
        wait_until(Duration::from_secs(5), || !observer.is_connected()),
        "a starting node ends the stream, which the client reads as disconnected (its reconnect state)"
    );
    assert_eq!(observer.snapshot().sequence, 0, "no snapshot was ever pushed");
    assert!(observer.snapshot().orders.is_empty());
}

#[test]
fn after_the_hand_over_the_same_listener_is_served_and_commands_work() {
    let (listener, addr, _progress, door) = open_door();

    // Before: starting, and the order is refused.
    let mut control = handshake(addr, CONTROL_KEY, Scope::Write).expect("control auth");
    write_frame(&mut control, &resting_submit("c-before")).expect("command");
    starting_text(&mut control);

    // The "start" finishes: a paper core, the door handed over, the real server on the listener.
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TIMESTAMP),
    );
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let sink = mount.handle.command_sink();
    door.hand_over(&listener).expect("the listener is blocking again");
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys(),
            Some(sink),
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });

    // After: the state flipped. A snapshot is pushed and the same command is ACKed.
    let observer = TradehubCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    thread::sleep(Duration::from_millis(300));
    assert!(observer.is_connected(), "the real server keeps the stream open");
    let mut control = handshake(addr, CONTROL_KEY, Scope::Write).expect("control auth");
    write_frame(&mut control, &resting_submit("c-after")).expect("command");
    match read_frame::<_, Response>(&mut control).expect("ack") {
        Response::Ack { order_client_id, .. } => assert_eq!(order_client_id, "c-after"),
        other => panic!("the server must ACK once it has taken over, got {other:?}"),
    }
    assert!(
        wait_until(Duration::from_secs(5), || observer
            .snapshot()
            .orders
            .iter()
            .any(|o| o.order_client_id == "c-after")),
        "the order reached the pushed snapshot"
    );
}

#[test]
fn the_progress_record_renders_each_account_and_reports_every_change() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let progress = StartProgress::new()
        .with_reporter(move |text| sink.lock().expect("reporter lock").push(text.to_string()));
    assert_eq!(progress.describe(), "the node is starting", "an empty record still says so");

    progress.phase("starting the accounts");
    progress.account("okx", AccountStart::Starting);
    progress.account("okx", AccountStart::Up);
    progress.account("ibkr", AccountStart::NotRunning("did not start, see the journal".into()));

    assert_eq!(
        progress.describe(),
        "starting the accounts; accounts: okx up, ibkr NOT RUNNING (did not start, see the journal)"
    );
    let seen = seen.lock().expect("reporter lock");
    assert_eq!(seen.len(), 4, "every change is reported once: {seen:?}");
    assert_eq!(seen[1], "starting the accounts; accounts: okx starting");
}

/// The `ready` line is the stdout banner `spawn_stdin_control` prints, and the deploy helper greps
/// the journal for it: it must come AFTER the server took the listener over, and the door must be
/// open BEFORE the mount that starts the engines. The live start needs a gatewayhub and venues, so
/// the order is pinned in the source, comment lines excluded.
#[test]
fn the_door_opens_before_the_engines_and_ready_is_printed_after_the_server_took_over() {
    let code: String = include_str!("../../src/tradehub_cli.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let at = |needle: &str| {
        code.find(needle)
            .unwrap_or_else(|| panic!("`tradehub_cli.rs` no longer contains `{needle}`"))
    };
    let order = [
        at("hub_server::open_observe_door("),
        at(".answer_starting("),
        at("match live_runtime("),
        at(".stop_answering_starting("),
        at("hub_server::start_observe_server("),
        at("spawn_stdin_control(&handle"),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "the order of open door, answer starting, the live mount, hand-over, server, `ready` banner \
         changed: {order:?}"
    );
}
