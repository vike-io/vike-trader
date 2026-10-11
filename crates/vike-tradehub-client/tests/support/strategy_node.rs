//! The STRATEGY-plane script `strategy_verbs.rs` and `account_negotiation.rs` share: the key, the
//! canned status, the node that answers `Command`/`Preview`/`StrategyStatus`, and the feature sets
//! of the nodes those suites refuse against.

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_tradehub_client::proto::{FEATURE_MOUNT_VERBS, FEATURE_STRATEGY_VERBS, Request, Response};
use vike_tradehub_client::wire::{WireCommand, WireMountRow, WireNodeIdentity, WireStrategyStatus};

use crate::fake_node;

/// The one key the scripted node accepts for EITHER scope.
pub const KEY: &[u8] = b"strategy-verbs-negotiation-test-key";

/// The canned status a feature-advertising scripted node answers.
pub fn canned_status() -> WireStrategyStatus {
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
            mount_id: String::new(),
            typed_params: Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})),
            asset_class: Some("CryptoPerp".into()),
        }],
    }
}

/// A scripted node (`fake_node`'s) advertising exactly `features` that answers `Command` → `Ack`,
/// `Preview` → accepted and `StrategyStatus` → [`canned_status`], recording each variant name.
pub fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    scripted_node_answering(features, |_| Response::Ack { coid: String::new() })
}

/// [`scripted_node`], with each post-auth `Command` answered by `command_reply(n)` for the `n`th
/// command (0-based) instead of always `Ack` — how a test makes the node REFUSE one.
pub fn scripted_node_answering(
    features: Vec<String>,
    mut command_reply: impl FnMut(usize) -> Response + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let mut commands = 0;
    fake_node::scripted_node(KEY, features, move |req| {
        let (name, reply) = match req {
            Request::Command { .. } => {
                commands += 1;
                ("Command", command_reply(commands - 1))
            }
            Request::Preview(_) => ("Preview", Response::Preview { accepted: true, reason: None }),
            Request::StrategyStatus => {
                ("StrategyStatus", Response::StrategyStatus(Box::new(canned_status())))
            }
            other => panic!("unscripted post-auth request: {other:?}"),
        };
        (name.to_string(), reply)
    })
}

/// The pre-B4 feature set an OLD node advertises — everything EXCEPT `strategy-verbs`.
pub fn old_node_features() -> Vec<String> {
    fake_node::features(&["observe", "subscribe", "snapshot", "preview"])
}

/// A `WireCommand::MountStrategy` for the scripted exchanges (split-plane B5).
pub fn mount_strategy() -> WireCommand {
    WireCommand::MountStrategy {
        venue: "binance".into(),
        // ACCOUNT-LESS deliberately: this helper feeds the MOUNT-VERBS gate, and a named account
        // would make those tests measure the account capability instead. `labelled_mount` in
        // `account_negotiation.rs` is the named twin.
        account: None,
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: Some("grid-a".into()),
        name: Some("grid".into()),
        rhai: None,
        params: serde_json::json!({"qty": 1.0}),
    }
}

/// What a RELEASED node that DROPS the account of a `Submit` and of a reduce advertises: MEASURED
/// against the tags, every `vike-tradehub` from `v0.1.27` through `v0.1.32` pushes exactly these
/// account strings (`account-routing`, `strategy-mount-account`; neither `account-scoped-submit`
/// nor `account-scoped-reduce`). The sharpest witness for either local refusal: a node that
/// decodes the frame and would widen it. `FEATURE_ACCOUNT_SCOPED_SUBMIT`'s doc carries the
/// measurement.
pub fn released_account_dropping_node_features() -> Vec<String> {
    let mut f = old_node_features();
    f.push(FEATURE_STRATEGY_VERBS.into());
    f.push(FEATURE_MOUNT_VERBS.into());
    f.push(vike_tradehub_client::proto::FEATURE_ACCOUNT_ROUTING.into());
    f.push(vike_tradehub_client::proto::FEATURE_MOUNT_ACCOUNT.into());
    f
}

/// A node advertising everything this tree made true, `account-routing` AND
/// `account-scoped-submit` included: the refusals against it are NOT "an old node".
///
/// ⚠ Before `account-scoped-submit` existed this list equalled the released account-dropping
/// node's, so "a labelled `Submit` flows here" was equally true of a node that discards the account.
pub fn todays_node_features() -> Vec<String> {
    let mut f = released_account_dropping_node_features();
    f.push(vike_tradehub_client::proto::FEATURE_ACCOUNT_SCOPED_SUBMIT.into());
    f
}
