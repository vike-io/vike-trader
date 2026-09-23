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
//!
//! ⚠ **It has outgrown its name and holds every CLIENT-SIDE capability refusal**, because the
//! scripted-node harness below is the negotiation harness rather than a strategy-verb one: the
//! mount-account gate (B5) and the account-scoped REDUCING verbs both live here for that reason.
//! The last of those is the odd one and is worth knowing before reading it — its capability is
//! advertised by NO node on purpose, so it has a refusal half and a still-flows half but no
//! "works when advertised" half at all.

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
            advertise_addr: String::new(),
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
            asset_class: Some("CryptoPerp".into()),
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
        // ACCOUNT-LESS deliberately: this helper feeds the MOUNT-VERBS gate, and a named account
        // would make those tests pass or fail on the account capability instead — the gate under
        // test would stop being the gate being measured. [`labelled_mount`] is the named twin.
        account: None,
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

/// The same mount, NAMING an account — the twin of [`mount_strategy`], and the only difference.
fn labelled_mount() -> WireCommand {
    match mount_strategy() {
        WireCommand::MountStrategy {
            venue,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
            ..
        } => WireCommand::MountStrategy {
            venue,
            account: Some("ALT".into()),
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        },
        other => panic!("the helper must build a mount, got {other:?}"),
    }
}

/// ⚠ **THE MOUNT'S ACCOUNT IS ITS OWN CAPABILITY AGAIN, and this is the rung the mount-verbs gate
/// above cannot reach.** The node here advertises `strategy-mount-verbs` honestly — it really does
/// serve the verb — and simply predates the field. So it DECODES the frame, `#[serde(default)]`
/// turns the field it never heard of into `account: None`, and it mounts the strategy on the
/// venue's default account while answering a perfectly normal acknowledgement. Nothing errors and
/// nothing is retryable; the operator reads a success and the strategy trades the wrong book for as
/// long as it runs.
///
/// That is why this refusal is CLIENT-SIDE and why the assertion that carries the weight is **that
/// nothing was sent** — the same assertion `FEATURE_ACCOUNT_ROUTING` earns on the order plane.
#[test]
fn a_labelled_mount_is_refused_client_side_against_a_node_without_the_account_capability() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features.push(FEATURE_MOUNT_VERBS.into()); // …but NOT `strategy-mount-account`.
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert_eq!(handle.try_command(labelled_mount()), Err(ControlRejected::UnsupportedByNode));
    drop(handle);
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// ⚠ **The complement, and without it the test above is satisfied by refusing every mount.** On
/// that SAME node — mount verbs, no account capability — an ACCOUNT-LESS mount still flies, because
/// its frame is byte-identical to the one that node has always accepted. This is the compatibility
/// half of the field's design, asserted rather than assumed.
#[test]
fn an_account_less_mount_still_flows_against_a_node_without_the_account_capability() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features.push(FEATURE_MOUNT_VERBS.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(mount_strategy()).expect("account-less ⇒ owes no capability");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// …and against a node that DOES advertise the account capability, the labelled mount sends.
#[test]
fn a_labelled_mount_flows_when_the_account_capability_is_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features.push(FEATURE_MOUNT_VERBS.into());
    features.push(vike_tradehub_client::proto::FEATURE_MOUNT_ACCOUNT.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(labelled_mount()).expect("advertised ⇒ sent");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
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

// ---------------------------------------------------------------------------------------------
// The ACCOUNT-SCOPED REDUCING VERBS — a capability WITHHELD, and the two halves of withholding it
// ---------------------------------------------------------------------------------------------
//
// ⚠ **The node advertises `account-routing` TRUTHFULLY and still drops this field**, which is what
// makes these refusals necessary and what makes them different from every gate above. There the
// missing string means the node cannot decode a frame; here the node decodes it perfectly,
// `crates/vike-tradehub/src/server.rs`'s `lower_command` destructures the variant as
// `{ venue, symbol, .. }`, and `vike_exec::OrderIntent`'s reducing variants have no account field
// for the value to land in. `vike_core`'s `CoreThread::exit_scope_engines` then fans the verb over
// EVERY account of the exchange and the node answers `accepted` — so on a two-account box a
// `market-exit binance ALT` cancels the DEFAULT account's orders and flattens its positions too.
//
// `FEATURE_ACCOUNT_SCOPED_REDUCE` is advertised by NO node, deliberately, so these refuse LOCALLY.
// The assertion that carries the weight is the same one the labelled mount earns: **nothing was
// sent**.

/// A node that advertises everything this branch's own work made true — `account-routing` included
/// — and therefore the sharpest possible witness: the refusals below are NOT "an old node".
fn todays_node_features() -> Vec<String> {
    let mut f = old_node_features();
    f.push(FEATURE_STRATEGY_VERBS.into());
    f.push(FEATURE_MOUNT_VERBS.into());
    f.push(vike_tradehub_client::proto::FEATURE_ACCOUNT_ROUTING.into());
    f.push(vike_tradehub_client::proto::FEATURE_MOUNT_ACCOUNT.into());
    f
}

/// The three risk-REDUCING verbs, each NAMING an account.
fn labelled_reducers() -> Vec<(&'static str, WireCommand)> {
    vec![
        (
            "mass-cancel",
            WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: Some("ALT".into()),
            },
        ),
        (
            "flatten",
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: Some("ALT".into()),
            },
        ),
        (
            "market-exit",
            WireCommand::MarketExit { venue: Some("binance".into()), account: Some("ALT".into()) },
        ),
    ]
}

/// The same three naming NO account — section 4.5's fan-out, which must be byte-identical to
/// before.
fn account_less_reducers() -> Vec<(&'static str, WireCommand)> {
    vec![
        (
            "mass-cancel",
            WireCommand::MassCancel { venue: Some("binance".into()), symbol: None, account: None },
        ),
        (
            "flatten",
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: None,
            },
        ),
        ("market-exit", WireCommand::MarketExit { venue: Some("binance".into()), account: None }),
    ]
}

/// ⚠ **THE WITHDRAWAL: a reducing verb NAMING an account is refused client-side, against a node
/// advertising `account-routing`.** One assertion per verb, because each is a separate arm and
/// "somebody guarded one and not its neighbour" is the failure this shape has had before.
#[test]
fn a_labelled_reducing_verb_is_refused_client_side_and_nothing_is_sent() {
    for (name, cmd) in labelled_reducers() {
        let (addr, report) = scripted_node(todays_node_features());
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert_eq!(
            handle.try_command(cmd),
            Err(ControlRejected::UnsupportedByNode),
            "{name} names an account and this node cannot honour it"
        );
        drop(handle);
        let seen = report.recv().expect("scripted node reports");
        assert!(seen.is_empty(), "{name}: NOTHING may be sent after auth, saw: {seen:?}");
    }
}

/// ⚠ **THE COMPLEMENT, and without it the test above is satisfied by refusing every reduce.** The
/// same three verbs naming NO account still fly to the same node, byte-identically to before the
/// field existed — section 4.5's law that a risk-REDUCING venue verb naming no account FANS OUT to
/// every account of that venue, deliberately: *"a fan-out can never reach an account the sender did
/// not mean, because the sender meant all of them."* Narrowing this would be worse than the defect
/// the test above guards.
#[test]
fn an_account_less_reducing_verb_still_flows_unchanged() {
    for (name, cmd) in account_less_reducers() {
        let (addr, report) = scripted_node(todays_node_features());
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
        let t = handle.try_command(cmd).unwrap_or_else(|e| {
            panic!("{name} names no account, so it owes no capability — got {e:?}")
        });
        let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
        assert_eq!(
            outcome,
            Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() }),
            "{name} must be accepted exactly as before"
        );
        drop(handle);
        assert_eq!(report.recv().expect("report"), vec!["Command".to_string()], "{name}");
    }
}

/// ⚠ **THE UNSCOPED PANIC BUTTON IS UNREFUSABLE, and this is its own test rather than a row above
/// because it is the one thing a gate in this family may never touch.** `MarketExit { venue: None }`
/// names no venue and no account: it means EVERY engine, and an operator reaching for it is already
/// having a bad day. A refusal here would be strictly worse than the misroute the sibling test
/// guards — it would be a kill switch that needed an argument to work.
#[test]
fn the_unscoped_panic_button_is_never_refused_by_the_account_gate() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle
        .try_command(WireCommand::MarketExit { venue: None, account: None })
        .expect("the unscoped panic button owes NO capability and must never refuse");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// …and the ORDER plane is untouched by the withdrawal: a labelled `Submit` still flies at a node
/// advertising `account-routing`, because that string is TRUE for orders. Without this, the arm
/// above could have been written to swallow every account-naming command and nothing would say so.
#[test]
fn a_labelled_submit_still_flows_because_account_routing_is_true_for_orders() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle
        .try_command(WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
            client_order_id: "c-acct".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: Some("ALT".into()),
        }))
        .expect("`account-routing` is advertised and TRUE for orders");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    // ⚠ The coid is the SCRIPTED NODE's canned empty echo, not this order's — the stub answers
    // every command with one fixed `Ack` and never reads the payload. What this test is about is
    // that the frame was SENT and accepted at all; the real node's coid echo is `accept_command`'s
    // business and is proven over a real server in `crates/vike-tradehub/tests/daemon/`.
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}
