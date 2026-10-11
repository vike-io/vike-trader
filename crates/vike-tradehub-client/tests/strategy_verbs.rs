//! Feature negotiation for the STRATEGY verbs (split-plane B4) and their later shapes (mount verbs,
//! addressed params, the params read, the bracket): the client REFUSES CLIENT-SIDE, with nothing on
//! the wire after the handshake, against a node whose `Welcome.features` does not advertise the
//! verb's capability, and works normally against one that does.
//!
//! Why a feature string instead of a `NODE_PROTO_VERSION` bump: the version is folded into the
//! signed auth MAC, so a bump breaks the handshake against every running node. These tests drive the
//! REAL client paths ([`strategy_status`], [`RemoteControlHandle`], [`preview_command`]) against
//! `support/strategy_node.rs`'s scripted node, so the refusal assertion is "the node saw NOTHING",
//! not merely "the call errored". The account-scoped shapes live in `account_negotiation.rs`.

#[path = "support/fake_node.rs"]
mod fake_node;
#[path = "support/strategy_node.rs"]
mod strategy_node;

use strategy_node::{
    KEY, canned_status, mount_strategy, old_node_features, scripted_node, scripted_node_answering,
    todays_node_features,
};
use vike_tradehub_client::proto::{
    FEATURE_MOUNT_VERBS, FEATURE_PARAMS_BY_MOUNT, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS,
    Response,
};
use vike_tradehub_client::remote_handle::strategy_params;
use vike_tradehub_client::{
    ControlRejected, RemoteControlHandle, preview_command, strategy_status, wire::WireCommand,
};

/// A `WireCommand::UpdateParams` for the scripted exchanges.
fn update_params() -> WireCommand {
    WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: None,
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

/// The same update NAMING a mount (`maker-b`): the shape that needs [`FEATURE_PARAMS_BY_MOUNT`].
fn addressed_update_params() -> WireCommand {
    let WireCommand::UpdateParams { venue, symbol, interval, params, .. } = update_params() else {
        unreachable!("update_params() builds an UpdateParams");
    };
    WireCommand::UpdateParams { venue, symbol, interval, mount_id: Some("maker-b".into()), params }
}

/// A node that serves the strategy verbs but PREDATES the mount id: it decodes an addressed
/// `UpdateParams`, drops the `mount_id` and retunes the FIRST mount on the series.
fn strategy_verbs_node_without_mount_ids() -> Vec<String> {
    let mut features = old_node_features();
    features.push(FEATURE_STRATEGY_VERBS.into());
    features
}

/// ⚠ THE WRITE-SIDE REFUSAL FOR THE MOUNT ID: an `UpdateParams` that NAMES a mount, against a node
/// advertising `strategy-verbs` but not `strategy-params-mount`, is
/// [`ControlRejected::UnsupportedByNode`] before the queue, and the node receives NOTHING: that
/// node would quietly retune the wrong mount behind a normal Ack.
#[test]
fn an_addressed_update_params_is_refused_client_side_without_the_mount_capability() {
    let (addr, report) = scripted_node(strategy_verbs_node_without_mount_ids());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert_eq!(
        handle.try_command(addressed_update_params()),
        Err(ControlRejected::UnsupportedByNode)
    );
    drop(handle); // hang up so the scripted node reports
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// The per-call preview path enforces the SAME gate for the addressed shape.
#[test]
fn preview_of_an_addressed_update_params_is_refused_client_side_without_the_mount_capability() {
    let (addr, report) = scripted_node(strategy_verbs_node_without_mount_ids());
    let err = preview_command(addr, KEY, &addressed_update_params())
        .expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_PARAMS_BY_MOUNT),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// The gate is SHAPE-specific: on that same node an UNADDRESSED update (the pre-field frame, which
/// carries no `mount_id` key) still sends and acks - `required_feature`'s plain `UpdateParams` arm -
/// and an addressed one flows once the node advertises the capability.
#[test]
fn only_the_addressed_shape_needs_the_mount_capability() {
    let (addr, report) = scripted_node(strategy_verbs_node_without_mount_ids());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let ticket = handle.try_command(update_params()).expect("unaddressed: strategy-verbs suffices");
    assert_eq!(
        handle.await_outcome(ticket, std::time::Duration::from_secs(5)),
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);

    let mut features = strategy_verbs_node_without_mount_ids();
    features.push(FEATURE_PARAMS_BY_MOUNT.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let ticket =
        handle.try_command(addressed_update_params()).expect("advertised: addressed flows");
    assert_eq!(
        handle.await_outcome(ticket, std::time::Duration::from_secs(5)),
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// ⚠ THE B5 GATE IS ITS OWN CAPABILITY: against a B4-era node (`strategy-verbs` but not
/// `strategy-mount-verbs`) the mount verbs are refused client-side, nothing on the wire; riding the
/// B4 string would send a variant that node cannot decode.
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

/// ⚠ THE PARAMS-READ GATE IS ITS OWN CAPABILITY. A node advertising `strategy-verbs` but not
/// `strategy-params` ANSWERS a `StrategyStatus` with rows whose new fields `#[serde(default)]` fills
/// in as empty, indistinguishable from "these mounts publish no typed params": a plausible wrong
/// answer, not an undecodable frame. Same refusal shape: `Unsupported`, naming the capability, and
/// **the node saw NOTHING after the handshake**.
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
// The TP/SL BRACKET — a DECODE capability, the `strategy-mount-verbs` argument
// ---------------------------------------------------------------------------------------------

/// A bracket every test below sends: a limit entry with its stop-loss and take-profit.
fn bracket() -> WireCommand {
    WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    })
}

/// ⚠ **Against today's node minus `bracket`, the verb is refused client-side and NOTHING is sent.**
/// That node cannot decode the variant, so it would only answer "undecodable request"; the
/// refusal here is what names the missing capability instead.
#[test]
fn a_bracket_is_refused_client_side_against_a_node_without_the_bracket_capability() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
    assert_eq!(handle.try_command(bracket()), Err(ControlRejected::UnsupportedByNode));
    drop(handle);
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// The per-call dry-run applies the same gate, and its error names the word.
#[test]
fn a_bracket_preview_is_refused_client_side_against_a_node_without_the_bracket_capability() {
    let (addr, report) = scripted_node(todays_node_features());
    let err = preview_command(addr, KEY, &bracket()).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(vike_tradehub_client::proto::FEATURE_BRACKET),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// ⚠ **The release, without which the tests above are satisfied by a client that refuses every
/// bracket for ever.** A node advertising `bracket` receives the frame.
#[test]
fn a_bracket_flies_to_a_node_advertising_the_bracket_capability() {
    let mut features = todays_node_features();
    features.push(vike_tradehub_client::proto::FEATURE_BRACKET.into());
    let (addr, report) = scripted_node(features);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(bracket()).expect("advertised: the frame flies");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// ⚠ **A CLIENT-side refusal reaches the status strip's latch, and stays there until dismissed.**
/// `try_command`'s refusal latches nothing by itself (the worker never sees the command) and the
/// desktop's status line is rewritten every frame, so `latch_client_refusal` writes the one channel
/// that persists, under a node refusal's rules: an ACCEPTED command does not clear it, a later
/// refusal replaces it, and only `clear_last_error` empties it.
#[test]
fn a_client_side_refusal_latches_until_dismissed_and_a_node_refusal_replaces_it() {
    let (addr, report) = scripted_node_answering(todays_node_features(), |n| match n {
        0 => Response::Ack { coid: String::new() },
        _ => Response::Error("the node refused this one".into()),
    });
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let wait = std::time::Duration::from_secs(5);

    assert_eq!(handle.try_command(bracket()), Err(ControlRejected::UnsupportedByNode));
    assert_eq!(handle.last_error(), None, "the refusal alone latches nothing: the I-1 trap");
    handle.latch_client_refusal("TP/SL bracket NOT sent".into());
    assert_eq!(handle.last_error().as_deref(), Some("TP/SL bracket NOT sent"));

    let accepted = handle.try_command(WireCommand::Cancel("c-1".into())).expect("not gated");
    assert_eq!(
        handle.await_outcome(accepted, wait),
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    assert_eq!(
        handle.last_error().as_deref(),
        Some("TP/SL bracket NOT sent"),
        "an accepted command does not clear the latch: only the operator's dismissal does"
    );

    let refused = handle.try_command(WireCommand::Cancel("c-2".into())).expect("not gated");
    assert_eq!(
        handle.await_outcome(refused, wait),
        Some(vike_tradehub_client::CommandOutcome::Refused("the node refused this one".into()))
    );
    assert_eq!(
        handle.last_error().as_deref(),
        Some("the node refused this one"),
        "a later refusal of either kind replaces the latch"
    );

    handle.clear_last_error();
    assert_eq!(handle.last_error(), None);
    drop(handle);
    assert_eq!(
        report.recv().expect("report"),
        vec!["Command".to_string(), "Command".to_string()],
        "the refused bracket never reached the node"
    );
}
