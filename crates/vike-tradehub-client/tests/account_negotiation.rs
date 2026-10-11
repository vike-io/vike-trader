//! Feature negotiation for the ACCOUNT-scoped shapes of the write plane: a labelled mount, a
//! labelled reducing verb and a labelled `Submit` each owe their OWN capability, because the node
//! that lacks it does not fail to decode the frame: it decodes it, drops the account and acts on a
//! book nobody named behind a normal acknowledgement. Each refusal is CLIENT-SIDE and the assertion
//! that carries the weight is that the scripted node recorded NOTHING after the handshake; each has
//! a complement (the account-less shape still flows) and a release (the advertised capability
//! lets it fly), without which a client refusing everything would pass.

#[path = "support/fake_node.rs"]
mod fake_node;
#[path = "support/strategy_node.rs"]
mod strategy_node;

use strategy_node::{
    KEY, mount_strategy, old_node_features, released_account_dropping_node_features, scripted_node,
    todays_node_features,
};
use vike_tradehub_client::proto::{FEATURE_MOUNT_VERBS, FEATURE_STRATEGY_VERBS};
use vike_tradehub_client::{ControlRejected, RemoteControlHandle, wire::WireCommand};

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

/// ⚠ **THE MOUNT'S ACCOUNT IS ITS OWN CAPABILITY, the rung the mount-verbs gate cannot reach.** A
/// node advertising `strategy-mount-verbs` honestly but predating the field DECODES the frame,
/// `#[serde(default)]` makes the account `None`, and it mounts on the venue's default account behind
/// a normal acknowledgement: the strategy trades the wrong book for as long as it runs. Hence a
/// CLIENT-SIDE refusal, and the assertion that carries the weight is **that nothing was sent**.
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

/// ⚠ **The complement, without which the test above is satisfied by refusing every mount.** On that
/// SAME node an ACCOUNT-LESS mount still flies: its frame is byte-identical to the one it has always
/// accepted.
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

// ---------------------------------------------------------------------------------------------
// The ACCOUNT-SCOPED REDUCING VERBS: the capability separates the nodes that WIDEN a labelled
// reduce from the nodes that NARROW it
// ---------------------------------------------------------------------------------------------
//
// ⚠ **A node built before the reducing verbs learned their account advertises `account-routing`
// and still drops this field**: it decodes the frame, its `lower_command` destructures
// `{ venue, symbol, .. }`, its core fans the verb over EVERY account of the exchange and it answers
// `accepted`, so `market-exit binance ALT` also flattened the DEFAULT account. Hence the LOCAL
// refusal, and again the assertion that carries the weight is **nothing was sent**.
//
// ⚠ From `v0.1.27` through `v0.1.32` `account-routing` is advertised while a `Submit`'s account is
// dropped too: the three `Submit` tests at the bottom are that measurement's order-plane half.

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

/// ⚠ **THE LOCAL REFUSAL: a reducing verb NAMING an account is refused client-side by a node
/// advertising `account-routing` and NOT `account-scoped-reduce`** (the node that would widen it).
/// One assertion per verb: each is a separate arm, and guarding one but not its neighbour has
/// happened before.
#[test]
fn a_labelled_reducing_verb_is_refused_client_side_and_nothing_is_sent() {
    for (name, cmd) in labelled_reducers() {
        let (addr, report) = scripted_node(released_account_dropping_node_features());
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

/// ⚠ **THE RELEASE, without which the test above is satisfied by refusing every labelled reduce.**
/// A node that ADVERTISES `account-scoped-reduce` narrows the verb to the named account, so each of
/// the three flies: the client honours the string in BOTH directions.
#[test]
fn a_labelled_reducing_verb_flies_to_a_node_that_narrows_it() {
    for (name, cmd) in labelled_reducers() {
        let mut features = released_account_dropping_node_features();
        features.push(vike_tradehub_client::proto::FEATURE_ACCOUNT_SCOPED_REDUCE.into());
        let (addr, report) = scripted_node(features);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
        let t = handle.try_command(cmd).unwrap_or_else(|e| {
            panic!("{name}: the node advertises the narrowing, so the frame must fly — got {e:?}")
        });
        let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
        assert_eq!(
            outcome,
            Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() }),
            "{name}"
        );
        drop(handle);
        assert_eq!(report.recv().expect("report"), vec!["Command".to_string()], "{name}: sent");
    }
}

/// ⚠ **THE COMPLEMENT, without which the test above is satisfied by refusing every reduce.** The
/// same three verbs naming NO account still fly, byte-identically: section 4.5's law that such a
/// verb FANS OUT to every account of the venue, deliberately: *"a fan-out can never reach an account
/// the sender did not mean, because the sender meant all of them."*
#[test]
fn an_account_less_reducing_verb_still_flows_unchanged() {
    let narrowing = {
        let mut f = released_account_dropping_node_features();
        f.push(vike_tradehub_client::proto::FEATURE_ACCOUNT_SCOPED_REDUCE.into());
        f
    };
    // To BOTH nodes: the one that would widen a labelled reduce and the one that narrows it. An
    // account-less reduce owes neither string, so neither may refuse it.
    for ((name, cmd), features) in account_less_reducers().into_iter().flat_map(|r| {
        [(r.clone(), released_account_dropping_node_features()), (r, narrowing.clone())]
    }) {
        let (addr, report) = scripted_node(features);
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

/// ⚠ **THE UNSCOPED PANIC BUTTON IS UNREFUSABLE.** `MarketExit { venue: None }` means EVERY engine;
/// a refusal here would be a kill switch that needed an argument to work.
#[test]
fn the_unscoped_panic_button_is_never_refused_by_the_account_gate() {
    let (addr, report) = scripted_node(released_account_dropping_node_features());
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

/// A `Submit` naming `account` (or none) — the order-plane frame the tests below send.
fn submit_naming(account: Option<&str>) -> WireCommand {
    WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
        client_order_id: "c-acct".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// ⚠ **A labelled `Submit` is refused client-side against a RELEASED node whose `account-routing`
/// is FALSE** (`v0.1.27` through `v0.1.32` advertise it and route by venue alone). A shipped node
/// cannot un-advertise it, so the client demands `account-scoped-submit` instead.
///
/// Both spellings are asserted: `required_feature` must not special-case `DEFAULT`, which that node
/// drops exactly like a label. The weight is again **nothing was sent**.
#[test]
fn a_named_account_submit_is_refused_client_side_against_a_released_node_that_drops_it() {
    for account in ["ALT", "DEFAULT"] {
        let (addr, report) = scripted_node(released_account_dropping_node_features());
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert_eq!(
            handle.try_command(submit_naming(Some(account))),
            Err(ControlRejected::UnsupportedByNode),
            "a submit naming {account} must not fly at a node whose account-routing is false"
        );
        drop(handle);
        let seen = report.recv().expect("scripted node reports");
        assert!(seen.is_empty(), "{account}: NOTHING may be sent after auth, saw: {seen:?}");
    }
}

/// ⚠ **The complement, without which the test above is satisfied by refusing every submit.** On that
/// SAME released node an ACCOUNT-LESS submit still flies: it owes no capability at all.
#[test]
fn an_account_less_submit_still_flows_to_a_released_node_that_drops_the_field() {
    let (addr, report) = scripted_node(released_account_dropping_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit_naming(None)).expect("account-less ⇒ owes no capability");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// …and the release: a labelled `Submit` flies at a node advertising `account-scoped-submit`.
/// Without this, the arm above could swallow every account-naming command and nothing would say so.
///
/// ⚠ Its premise used to be `account-routing` alone, which was false for six releases (the refusal
/// test above), so the node it flies at now carries the string that is true.
#[test]
fn a_labelled_submit_still_flows_to_a_node_advertising_account_scoped_submit() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit_naming(Some("ALT"))).expect(
        "`account-scoped-submit` is advertised, and it is the string that is TRUE for orders",
    );
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    // ⚠ The coid is the SCRIPTED NODE's canned empty echo, not this order's: the stub never reads
    // the payload. The real echo is proven over a real server in `crates/vike-tradehub/tests/daemon/`.
    assert_eq!(
        outcome,
        Some(vike_tradehub_client::CommandOutcome::Accepted { coid: String::new() })
    );
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}
