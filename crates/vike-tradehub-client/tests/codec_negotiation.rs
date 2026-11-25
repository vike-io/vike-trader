//! The per-connection CODEC switch (`vike_hub_protocol::frame::BodyCodec`, decision 0107's amendment of 2026-11-19):
//! against a node advertising `FEATURE_WIRE_MSGPACK`, every session request this client sends (a one-shot read, a write's
//! preview, a command on the control handle, the observe handle's subscription) goes in msgpack with field names, and
//! the answers read back value for value; against a node without the word, every node before this build, every request
//! goes in JSON as it always did. The scripted node (`src/test_support/fake_node.rs`) records the codec each request
//! came in and answers in it, as a node of this build does.

use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_tradehub_client::proto::{FEATURE_OBSERVE_COMPRESSED, Request, Response};
use vike_tradehub_client::test_support::fake_node;
use vike_tradehub_client::test_support::observe_frame::full_wire_snapshot;
use vike_tradehub_client::wire::CommandWire;
use vike_tradehub_client::{
    CommandOutcome, TradehubControlHandle, TradehubCoreHandle, preview_command, snapshot_once,
};

const KEY: &[u8] = b"codec-negotiation-test-key";

/// A node advertising `features` that answers each verb these tests send and records `<codec> <verb>` for it.
fn node(features: &[&str]) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let snapshot = || Response::SnapshotFrame(Box::new(full_wire_snapshot()));
    fake_node::scripted_node_in_codecs(KEY, fake_node::features(features), move |request, codec| {
        let preview = Response::Preview {
            accepted: false,
            reason: Some("name one of these books".into()),
            refusal_id: Some("tradehub.door.account_not_named".into()),
            notes: Vec::new(),
        };
        let ack =
            Response::Ack { order_client_id: "c-1".into(), notes: Vec::new(), emulated: None };
        let (verb, answer) = match request {
            Request::Snapshot => ("Snapshot", snapshot()),
            Request::SubscribeWith(_) => ("SubscribeWith", snapshot()),
            Request::Subscribe { .. } => ("Subscribe", snapshot()),
            Request::Preview(_) => ("Preview", preview),
            Request::Command { .. } => ("Command", ack),
            other => panic!("unscripted post-auth request: {other:?}"),
        };
        (format!("{codec:?} {verb}"), answer)
    })
}

/// Every verb kind once against a node advertising `features`, each on its own connection, and what the node heard.
fn every_verb_against(features: &[&str]) -> Vec<String> {
    let mut heard = Vec::new();

    let (addr, report) = node(features);
    let snap = snapshot_once(addr, KEY).expect("a one-shot read");
    assert_eq!(snap, full_wire_snapshot(), "the one-shot answer reads back value for value");
    heard.extend(report.recv().expect("report"));

    let (addr, report) = node(features);
    let (accepted, reason, notes) =
        preview_command(addr, KEY, &CommandWire::Cancel("c-1".into())).expect("preview");
    // The refusal reads back with its id, shown once.
    assert_eq!(
        (accepted, reason.map(|why| why.rendered()), notes),
        (
            false,
            Some("error [tradehub.door.account_not_named]: name one of these books".to_string()),
            Vec::new()
        )
    );
    heard.extend(report.recv().expect("report"));

    let (addr, report) = node(features);
    let control = TradehubControlHandle::connect(addr, KEY).expect("control");
    let ticket = control.try_command(CommandWire::Cancel("c-1".into())).expect("queued");
    assert_eq!(
        control.await_outcome(ticket, Duration::from_secs(10)),
        Some(CommandOutcome::Accepted { order_client_id: "c-1".into(), notes: Vec::new() })
    );
    drop(control);
    heard.extend(report.recv().expect("report"));

    let (addr, report) = node(features);
    let observe = TradehubCoreHandle::connect(addr, KEY).expect("observe");
    let deadline = Instant::now() + Duration::from_secs(10);
    while *observe.snapshot() != full_wire_snapshot() {
        assert!(Instant::now() < deadline, "the observe handle never read the pushed frame");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(observe);
    heard.extend(report.recv().expect("report"));
    heard
}

/// What the node records for [`every_verb_against`]'s four verbs, in `codec`, the observe handle's subscription being
/// `subscribe`.
fn heard_in(codec: &str, subscribe: &str) -> Vec<String> {
    ["Snapshot", "Preview", "Command", subscribe].map(|verb| format!("{codec} {verb}")).to_vec()
}

/// **A node of this build is spoken to in msgpack**, every verb kind, and every answer reads back as the node meant it.
#[test]
fn a_node_advertising_msgpack_is_sent_every_request_in_msgpack() {
    let heard = every_verb_against(&[FEATURE_OBSERVE_COMPRESSED, FEATURE_WIRE_MSGPACK]);
    assert_eq!(heard, heard_in("Msgpack", "SubscribeWith"));
}

/// **A NEW client against an OLD node falls back to JSON**: a node without the word is sent every request in JSON, as
/// before this build, and reads the answers it always read; one older still (no shaped subscription) is subscribed to
/// as before, in JSON.
#[test]
fn a_node_without_msgpack_is_sent_every_request_in_json() {
    let heard = every_verb_against(&[FEATURE_OBSERVE_COMPRESSED]);
    assert_eq!(heard, heard_in("Json", "SubscribeWith"));
    assert_eq!(every_verb_against(&[]), heard_in("Json", "Subscribe"));
}
