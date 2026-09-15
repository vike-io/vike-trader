//! Feature negotiation for the STRATEGY verbs (split-plane B4): the client REFUSES CLIENT-SIDE —
//! nothing on the wire after the handshake — against a node whose `Welcome.features` does not
//! advertise [`FEATURE_STRATEGY_VERBS`], and works normally against one that does.
//!
//! Why this gate exists instead of a `NODE_PROTO_VERSION` bump: the version is folded into the
//! signed auth MAC, so a bump breaks the handshake against every running node. `Welcome.features`
//! is the designed forward-compat hook — an old node still authenticates this client, it just
//! cannot DECODE the new verbs, so the client must not send them. These tests drive the REAL
//! client paths ([`strategy_status`], [`RemoteControlHandle`], [`preview_command`]) against a
//! scripted loopback node that RECORDS every frame it receives after auth — the refusal assertion
//! is "the server saw NOTHING", not merely "the call errored".

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_tradehub_client::proto::{
    FEATURE_MOUNT_VERBS, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS, NODE_PROTO_VERSION,
    Request, Response, read_frame, write_frame,
};
use vike_tradehub_client::remote_handle::strategy_params;
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity, WireStrategyStatus};
use vike_tradehub_client::{
    ControlRejected, RemoteControlHandle, WireCommand, auth, preview_command, strategy_status,
};

/// The one key the scripted node accepts for EITHER scope (scope separation is the real server's
/// concern, proven elsewhere; these tests are about feature negotiation).
const KEY: &[u8] = b"strategy-verbs-negotiation-test-key";

/// The canned status a feature-advertising scripted node answers.
fn canned_status() -> WireStrategyStatus {
    WireStrategyStatus {
        identity: WireNodeIdentity {
            name: "scripted".into(),
            strategy: "spread_maker".into(),
            params: "qty=1".into(),
            live: false,
            build: "test-build".into(),
        },
        effective_params: "qty=1".into(),
        mounts: vec![WireMountRow {
            strategy: "spread_maker".into(),
            params: "qty=1".into(),
            live: false,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            typed_params: Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})),
        }],
    }
}

/// Spawn a one-connection scripted node on loopback advertising exactly `features`. It runs the
/// real handshake (verifying the client's mac with [`KEY`] for whatever scope it claims), then
/// answers each post-auth request (`Command` → `Ack`, `Preview` → accepted, `StrategyStatus` → the
/// canned status) while RECORDING the request variant names, and finally — when the client hangs up
/// — reports that list through the returned channel. An EMPTY list is the client-side-refusal
/// proof: the client authenticated and then sent NOTHING.
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (report_tx, report_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let nonce = [7u8; 32];
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
        )
        .expect("write Welcome");
        let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Auth after Welcome");
        };
        assert!(
            auth::verify(KEY, &nonce, NODE_PROTO_VERSION, scope, &mac),
            "the scripted node's one key must verify"
        );
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("write AuthOk");

        // Post-auth: serve + record until the client hangs up (a read error is EOF / client
        // shutdown — the connection is over).
        let mut seen: Vec<String> = Vec::new();
        while let Ok(req) = read_frame::<_, Request>(&mut stream) {
            let (name, reply) = match req {
                Request::Command { .. } => ("Command", Response::Ack { coid: String::new() }),
                Request::Preview(_) => {
                    ("Preview", Response::Preview { accepted: true, reason: None })
                }
                Request::StrategyStatus => {
                    ("StrategyStatus", Response::StrategyStatus(Box::new(canned_status())))
                }
                other => panic!("unscripted post-auth request: {other:?}"),
            };
            seen.push(name.to_string());
            write_frame(&mut stream, &reply).expect("write scripted reply");
        }
        report_tx.send(seen).expect("report the post-auth frame list");
    });
    (addr, report_rx)
}

/// The pre-B4 feature set an OLD node advertises — everything EXCEPT `strategy-verbs`.
fn old_node_features() -> Vec<String> {
    vec!["observe".into(), "subscribe".into(), "snapshot".into(), "preview".into()]
}

/// A `WireCommand::UpdateParams` for the scripted exchanges.
fn update_params() -> WireCommand {
    WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params: serde_json::json!({"SpreadMaker": {"qty": 2.0}}),
    }
}

/// ⚠ THE REFUSAL: `strategy_status` against a node that does not advertise the feature errors
/// [`std::io::ErrorKind::Unsupported`] naming the capability — and the node received NOTHING after
/// the handshake (no frame went on the wire).
#[test]
fn strategy_status_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let err = strategy_status(addr, KEY).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_STRATEGY_VERBS),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node that DOES advertise it, the same call round-trips the canned status.
#[test]
fn strategy_status_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    let (addr, report) = scripted_node(features);
    let status = strategy_status(addr, KEY).expect("advertised ⇒ served");
    assert_eq!(status, canned_status());
    assert_eq!(report.recv().expect("report"), vec!["StrategyStatus".to_string()]);
}

/// ⚠ THE WRITE-SIDE REFUSAL: `UpdateParams` through a [`RemoteControlHandle`] connected to an old
/// node is [`ControlRejected::UnsupportedByNode`] — refused before the queue, so the node receives
/// NOTHING after the handshake.
#[test]
fn update_params_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    let refused = handle.try_command(update_params());
    assert_eq!(refused, Err(ControlRejected::UnsupportedByNode));
    drop(handle); // hang up so the scripted node reports
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// The gate is VERB-specific, not connection-wide: on that same old node a pre-B4 command (Cancel)
/// still sends and acks — `required_feature`'s `None` arm.
#[test]
fn pre_b4_verbs_still_flow_to_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let ticket = handle.try_command(WireCommand::Cancel("c-1".into())).expect("not feature-gated");
    let outcome = handle.await_outcome(ticket, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The per-call preview path enforces the SAME gate: previewing an `UpdateParams` against an old
/// node is refused client-side (the node could only answer "undecodable request", not a verdict).
#[test]
fn preview_of_update_params_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let err = preview_command(addr, KEY, &update_params()).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// A `WireCommand::MountStrategy` for the scripted exchanges (split-plane B5).
fn mount_strategy() -> WireCommand {
    WireCommand::MountStrategy {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: Some("grid-a".into()),
        name: Some("grid".into()),
        rhai: None,
        params: serde_json::json!({"qty": 1.0}),
    }
}

/// ⚠ THE B5 GATE IS ITS OWN CAPABILITY: a B4-era node — one that DOES advertise `strategy-verbs`
/// but not `strategy-mount-verbs` — refuses the mount verbs client-side, with nothing on the wire.
/// Riding the B4 string would send a variant that node cannot decode, which is exactly what the
/// feature list exists to prevent.
#[test]
fn mount_verbs_are_refused_client_side_against_a_b4_node() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert_eq!(handle.try_command(mount_strategy()), Err(ControlRejected::UnsupportedByNode));
    assert_eq!(
        handle.try_command(WireCommand::UnmountStrategy { controller_id: "grid-a".into() }),
        Err(ControlRejected::UnsupportedByNode)
    );
    drop(handle);
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node advertising [`FEATURE_MOUNT_VERBS`], both mount verbs send and ack.
#[test]
fn mount_verbs_flow_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features.push(FEATURE_MOUNT_VERBS.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t1 = handle.try_command(mount_strategy()).expect("advertised ⇒ sent");
    let outcome = handle.await_outcome(t1, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    let t2 = handle
        .try_command(WireCommand::UnmountStrategy { controller_id: "grid-a".into() })
        .expect("advertised ⇒ sent");
    let outcome = handle.await_outcome(t2, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string(), "Command".to_string()]);
}

/// ⚠ THE PARAMS-READ GATE IS ITS OWN CAPABILITY, and the reason is one rung sharper than the
/// mount verbs' above. A node advertising `strategy-verbs` but not `strategy-params` DECODES a
/// `StrategyStatus` perfectly well and ANSWERS it — with rows whose new fields `#[serde(default)]`
/// fills in as empty, which a client cannot tell from an honest "these mounts publish no typed
/// params". So the failure this refusal prevents is not an undecodable frame: it is a client
/// silently doing the wrong thing with a plausible answer.
///
/// The refusal is therefore the same shape as every other one here — `Unsupported`, naming the
/// capability, and **the node saw NOTHING after the handshake**.
#[test]
fn the_params_read_is_refused_client_side_against_a_node_without_the_capability() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into()); // the node CAN answer StrategyStatus...
    let (addr, report) = scripted_node(features); // ...but does not advertise strategy-params
    let err = strategy_params(addr, KEY).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_STRATEGY_PARAMS),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node that DOES advertise it, the same call round-trips the canned status,
/// carrying both new halves: the addressing key a `WireCommand::UpdateParams` targets, and the
/// typed bag it takes back.
#[test]
fn the_params_read_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features.push(FEATURE_STRATEGY_PARAMS.into());
    let (addr, report) = scripted_node(features);
    let status = strategy_params(addr, KEY).expect("advertised ⇒ served");
    assert_eq!(status, canned_status());
    let row = status.mounts.first().expect("one mounted strategy");
    assert_eq!(
        (row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()),
        ("binance", "BTCUSDT", "1m")
    );
    assert_eq!(row.typed_params, Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})));
    // The verb rides the EXISTING request — no new `Request` variant, so an old node's decoder is
    // never handed a frame it cannot parse; the negotiation alone is what stops the send.
    assert_eq!(report.recv().expect("report"), vec!["StrategyStatus".to_string()]);
}
