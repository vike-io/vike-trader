//! A CONFIRMED write with a `notifications/cancelled` behind it, over [`serve`] and a real paper node,
//! and one the node has not answered within the ack wait.
//!
//! The danger: the SDK's serve loop drops the answer of any request a cancel names, while the tool
//! keeps running. On a confirmed `submit_order` the order then goes out and the agent is told nothing,
//! so all it can do is send again. `crates/vike-mcp/src/rmcp_wire.rs` is what closes it: on the
//! unwrapped SDK the first test here FAILS (`THE AGENT WAS TOLD NOTHING`, measured on both of the
//! SDK's entries on 2026-11-06).
//!
//! [`GatedRelay`] is the only harness here: a frame-level relay that holds each `Request::Command`
//! until the test opens the gate, so "in flight" is a state the test can stand in rather than a race.
//! Everything else is the node of this suite ([`spawn_hub`]): the order really lands in its book.

use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, mpsc};

use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_log::poll::wait_until;

use super::*;
use crate::mcp_tests::cancellation::{Session, WAIT, cancelled, line};

/// A relay in front of the node that holds every `Request::Command` until a send on `gate`.
struct GatedRelay {
    addr: SocketAddr,
    /// Each command frame the server wrote, in order, reported before it is forwarded.
    commands: mpsc::Receiver<Value>,
    gate: mpsc::Sender<()>,
}

fn gated_relay(node: SocketAddr) -> GatedRelay {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (seen, commands) = mpsc::channel();
    let (gate, gate_rx) = mpsc::channel::<()>();
    let gate_rx = Arc::new(Mutex::new(gate_rx));
    thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { return };
            let mut upstream = TcpStream::connect(node).expect("the node listens");
            let mut down_from = upstream.try_clone().expect("clone upstream");
            let mut down_to = client.try_clone().expect("clone client");
            thread::spawn(move || {
                let _ = io::copy(&mut down_from, &mut down_to);
                let _ = down_to.shutdown(Shutdown::Write);
            });
            let (seen, gate_rx) = (seen.clone(), Arc::clone(&gate_rx));
            thread::spawn(move || {
                while let Ok(frame) = read_frame::<_, Value>(&mut client) {
                    if frame.get("Command").is_some() {
                        let _ = seen.send(frame.clone());
                        let _ = gate_rx.lock().expect("gate").recv_timeout(WAIT);
                    }
                    if write_frame(&mut upstream, &frame).is_err() {
                        break;
                    }
                }
                let _ = upstream.shutdown(Shutdown::Write);
            });
        }
    });
    GatedRelay { addr, commands, gate }
}

/// A session past a real client's opening, its server dialling the node through `relay`.
fn session_through(relay: SocketAddr, node: SocketAddr) -> Session {
    let session = Session::start_with(move || Server {
        node_addr: Some(relay.to_string()),
        ..server_at(node)
    });
    session.send(line(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "cancel-write", "version": "0" }
        }),
    ));
    assert_eq!(session.next_response()["id"], json!(1));
    session.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    session
}

fn submit(id: i64, args: Value) -> Value {
    line(id, "tools/call", json!({ "name": "submit_order", "arguments": args }))
}

fn confirming(mut args: Value, token: &str) -> Value {
    args["confirm"] = json!(true);
    args["preview_token"] = json!(token);
    args
}

/// A node past its first fold, so the venue gate has a frame to read: the seed is the first order.
fn folded_node() -> (StrategyCorePaper, SocketAddr) {
    let (mount, node) = spawn_hub();
    let seeded = seed_order(node);
    assert!(
        wait_until(WAIT, || book(&mount).contains(&seeded)),
        "the seed order reaches the node's book"
    );
    (mount, node)
}

/// Preview `args` as `id`: the token, and the order client id the preview shows.
fn preview_over_the_wire(session: &Session, id: i64, args: &Value) -> (String, String) {
    session.send(submit(id, args.clone()));
    let answer = session.next_response();
    assert_eq!(answer["id"], json!(id), "{answer}");
    let body = &answer["result"]["structuredContent"];
    let token = body["preview_token"].as_str().expect("a preview mints a token").to_string();
    let order_client_id =
        body["wire_command"]["Submit"]["order_client_id"].as_str().expect("an order client id");
    (token, order_client_id.to_string())
}

/// **THE PROPERTY.** The cancel lands while the command is on the wire and the node has not answered:
/// the order goes out, the agent still gets its receipt, one command crossed, and the order client id is the one
/// the preview showed.
#[test]
fn a_confirmed_write_cancelled_in_flight_keeps_its_receipt_and_goes_out_once() {
    let (mount, node) = folded_node();
    let relay = gated_relay(node);
    let session = session_through(relay.addr, node);

    let (token, order_client_id) = preview_over_the_wire(&session, 2, &submit_args());
    session.send(submit(3, confirming(submit_args(), &token)));
    let held = relay.commands.recv_timeout(WAIT).expect("the confirm reaches the wire");
    assert_eq!(
        held["Command"]["command"]["Submit"]["order_client_id"],
        json!(order_client_id),
        "{held}"
    );

    session.send(cancelled(3));
    session.send(line(4, "ping", json!({})));
    // Long enough for whichever layer reads the cancel to have read it before the node answers.
    thread::sleep(Duration::from_millis(300));
    relay.gate.send(()).expect("the relay holds the gate");
    assert!(wait_until(WAIT, || book(&mount).contains(&order_client_id)), "the order went out");

    let answers = session.finish();
    let Some(receipt) = answers.iter().find(|a| a["id"] == json!(3)) else {
        panic!(
            "THE AGENT WAS TOLD NOTHING: order {order_client_id} is in the node's book, its confirm (id 3) got \
             no answer, and a resend is all that is left to it: {answers:?}"
        );
    };
    let body = &receipt["result"]["structuredContent"];
    assert_eq!(body["outcome"], json!("accepted"), "{receipt}");
    assert_eq!(
        body["order_client_id"],
        json!(order_client_id),
        "the receipt names the previewed order client id"
    );
    assert!(answers.iter().any(|a| a["id"] == json!(4)), "the ping is answered: {answers:?}");

    assert!(
        relay.commands.recv_timeout(Duration::from_millis(300)).is_err(),
        "exactly one command crossed"
    );
    let ids = book(&mount);
    assert_eq!(
        ids.iter().filter(|id| **id == order_client_id).count(),
        1,
        "the order client id does not repeat: {ids:?}"
    );
    assert_eq!(ids.len(), 2, "the seed and this one order, nothing else: {ids:?}");
}

/// A confirmed write the node has not answered within the ack wait is OUTCOME UNKNOWN in the `isError`
/// shape a dropped link has, never a success-shaped `"outcome": "unknown"` an agent reading `isError` takes
/// for a success. The link stays held, and the command still goes out once the relay lets it.
#[test]
fn a_write_unanswered_within_the_ack_wait_is_outcome_unknown_as_an_error() {
    let (mount, node) = folded_node();
    let relay = gated_relay(node);
    let ack_wait = Duration::from_millis(200);
    let mut s = Server { node_addr: Some(relay.addr.to_string()), ack_wait, ..server_at(node) };

    let (pv, token) = preview(&mut s);
    let order_client_id =
        pv["wire_command"]["Submit"]["order_client_id"].as_str().unwrap().to_string();
    let unknown = confirm(&mut s, &token).expect_err("an unanswered command is no success");
    assert_eq!(
        unknown,
        format!(
            "node {}: {} Call node_snapshot to check BEFORE retrying.",
            relay.addr,
            vike_tradehub_client::UnansweredCommand::NoAnswerWithin(ack_wait)
        )
    );
    assert!(relay.commands.recv_timeout(WAIT).is_ok(), "it was written: not a never-sent");
    assert!(s.control.is_some(), "a wait that ran out leaves the link held");

    relay.gate.send(()).expect("the relay holds the gate");
    assert!(
        wait_until(WAIT, || book(&mount).contains(&order_client_id)),
        "an unknown outcome may still execute"
    );
}

/// What an agent left without an answer would do, and what the node can make of it. A resend under a
/// new preview token is a NEW order: the token never leaves this process and the order client id is minted anew,
/// so the two commands the node reads differ in the order client id alone. Only an order client id the agent pinned itself
/// makes the second one refusable.
#[test]
fn a_resend_under_a_new_preview_token_is_a_new_order_unless_the_agent_pinned_its_order_client_id() {
    let (mount, node) = folded_node();
    let relay = gated_relay(node);
    let session = session_through(relay.addr, node);

    let place = |id: i64, args: Value| -> (String, Value, Value) {
        let (token, order_client_id) = preview_over_the_wire(&session, id, &args);
        relay.gate.send(()).expect("the relay holds the gate");
        session.send(submit(id + 1, confirming(args, &token)));
        let answer = session.next_response();
        assert_eq!(answer["id"], json!(id + 1), "{answer}");
        (order_client_id, answer, relay.commands.recv_timeout(WAIT).expect("a command crossed"))
    };

    let (first, first_answer, first_frame) = place(10, submit_args());
    let (second, second_answer, second_frame) = place(20, submit_args());
    assert_eq!(first_answer["result"]["structuredContent"]["outcome"], json!("accepted"));
    assert_eq!(second_answer["result"]["structuredContent"]["outcome"], json!("accepted"));
    assert_ne!(first, second, "a new preview mints a new order client id");
    let without_order_client_id = |mut frame: Value| {
        frame["Command"]["command"]["Submit"]["order_client_id"] = json!("");
        frame
    };
    assert_eq!(
        without_order_client_id(first_frame),
        without_order_client_id(second_frame),
        "the node reads no token: the two commands differ in the order client id alone"
    );
    assert!(
        wait_until(WAIT, || {
            let ids = book(&mount);
            ids.contains(&first) && ids.contains(&second)
        }),
        "the node took both as orders"
    );

    // The door judges an order client id against the PUBLISHED orders, so give the publisher its tick.
    thread::sleep(Duration::from_millis(500));
    let mut pinned = submit_args();
    pinned["order_client_id"] = json!(first);
    // The refusal comes at the preview now: no token, so no second call can send it.
    session.send(submit(30, pinned));
    let refused = session.next_response();
    let body = &refused["result"]["structuredContent"];
    assert_eq!(body["preview_token"], Value::Null, "{refused}");
    assert_eq!(body["node_verdict"]["accepted"], json!(false), "{refused}");
    let why = body["node_verdict"]["reason"].as_str().unwrap_or_default();
    assert!(
        why.contains("already used"),
        "the node refuses a repeated order client id, in words: {why}"
    );
    let ids = book(&mount);
    assert_eq!(ids.len(), 3, "the seed and the two orders; the pinned resend added none: {ids:?}");
    assert!(session.finish().is_empty());
}
