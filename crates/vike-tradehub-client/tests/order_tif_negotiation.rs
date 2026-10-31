//! Feature negotiation for the order's LIFETIME: a `Submit` that names `time_in_force` (a `post-only` or
//! `ioc` order) or a `gtd_expiry` owes `order-tif`, because the node that lacks it does not fail to decode
//! the frame: it decodes it, DROPS the two keys and runs the order as a plain GTC limit behind a normal
//! acknowledgement (a post-only order that takes liquidity). The refusal is CLIENT-SIDE and the assertion
//! that carries the weight is that the scripted node recorded NOTHING after the handshake; it has a
//! complement (a submit that names no lifetime still flies) and a release (the advertised word lets the
//! lifetime fly), without which a client refusing every submit would pass.

#[path = "support/fake_node.rs"]
mod fake_node;
// the shared scripted-node kit: this binary builds no mount, so `mount_strategy` is the one item unused here
#[expect(dead_code)]
#[path = "support/strategy_node.rs"]
mod strategy_node;

use strategy_node::{KEY, scripted_node, todays_node_features};
use vike_tradehub_client::proto::FEATURE_ORDER_TIF;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_client::{CommandOutcome, ControlRejected, RemoteControlHandle};

/// A limit `Submit` with the lifetime under test (`account` named or not, the axis the account word guards).
fn submit(
    time_in_force: Option<&str>,
    gtd_expiry: Option<i64>,
    account: Option<&str>,
) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c-tif".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
        time_in_force: time_in_force.map(str::to_string),
        gtd_expiry,
    })
}

/// A node that advertises everything this tree made true EXCEPT `order-tif` (the node of the release before).
fn node_without_the_word() -> Vec<String> {
    let mut f = todays_node_features();
    f.retain(|w| w != FEATURE_ORDER_TIF);
    f
}

/// ⚠ **A lifetime is refused client-side against a node without `order-tif`**, each shape that carries one:
/// a post-only order, a plain `ioc`, a `gtd` with its expiry, and (the arm order) the same naming an account.
/// Nothing is sent.
#[test]
fn a_lifetime_is_refused_client_side_against_a_node_without_the_word() {
    for (what, cmd) in [
        ("post-only", submit(Some("post-only"), None, None)),
        ("ioc", submit(Some("ioc"), None, None)),
        ("gtd", submit(Some("gtd"), Some(4_102_444_800_000), None)),
        ("post-only on a named account", submit(Some("post-only"), None, Some("ALT"))),
    ] {
        let (addr, report) = scripted_node(node_without_the_word());
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake still succeeds");
        assert_eq!(
            handle.try_command(cmd),
            Err(ControlRejected::UnsupportedByHub),
            "{what} must not fly at a node that would drop it"
        );
        drop(handle);
        let seen = report.recv().expect("scripted node reports");
        assert!(seen.is_empty(), "{what}: NOTHING may be sent after auth, saw: {seen:?}");
    }
}

/// The complement: on that SAME node a submit that names no lifetime still flies, byte for byte the frame
/// it has always been (it owes no capability).
#[test]
fn a_submit_naming_no_lifetime_still_flows_to_a_node_without_the_word() {
    let (addr, report) = scripted_node(node_without_the_word());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit(None, None, None)).expect("no lifetime ⇒ owes nothing");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(outcome, Some(CommandOutcome::Accepted { coid: String::new(), notes: Vec::new() }));
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The release: with `order-tif` advertised the lifetime flies.
#[test]
fn a_lifetime_flows_to_a_node_advertising_the_word() {
    let (addr, report) = scripted_node(todays_node_features());
    let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
    let t = handle.try_command(submit(Some("post-only"), None, Some("ALT"))).expect("advertised");
    let outcome = handle.await_outcome(t, std::time::Duration::from_secs(5));
    assert_eq!(outcome, Some(CommandOutcome::Accepted { coid: String::new(), notes: Vec::new() }));
    drop(handle);
    assert_eq!(report.recv().expect("report"), vec!["Command".to_string()]);
}

/// The frame itself: an order that names no lifetime serialises WITHOUT the two keys (the byte-identical
/// frame an older node decodes), and one that names a lifetime carries them verbatim.
#[test]
fn the_lifetime_keys_are_on_the_wire_only_when_named() {
    let bare = serde_json::to_string(&submit(None, None, None)).expect("serialises");
    assert!(!bare.contains("time_in_force") && !bare.contains("gtd_expiry"), "{bare}");
    let named = serde_json::to_string(&submit(Some("gtd"), Some(4_102_444_800_000), None))
        .expect("serialises");
    assert!(
        named.contains("\"time_in_force\":\"gtd\"")
            && named.contains("\"gtd_expiry\":4102444800000"),
        "{named}"
    );
    let old: WireCommand = serde_json::from_str(&bare).expect("a pre-field frame still decodes");
    assert_eq!(old, submit(None, None, None));
}

/// **`takes` answers the gate without sending**: false for exactly the submits `try_command` refuses
/// against a node without the word (a post-only order, on any account), true for them against a node
/// that advertises it, and true for a submit naming no lifetime on both (it owes nothing). This is what
/// the desktop ticket asks before it enables Post only, so a control and a send cannot disagree.
#[test]
fn takes_says_what_the_gate_would_say() {
    let post_only = submit(Some("post-only"), None, None);
    let named = submit(Some("post-only"), None, Some("ALT"));
    let plain = submit(None, None, None);
    for (features, lifetime_ok) in
        [(node_without_the_word(), false), (todays_node_features(), true)]
    {
        let (addr, _report) = scripted_node(features);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("handshake");
        assert_eq!(
            handle.takes(&post_only),
            lifetime_ok,
            "post-only, lifetime word: {lifetime_ok}"
        );
        assert_eq!(handle.takes(&named), lifetime_ok, "post-only on a named account");
        assert!(handle.takes(&plain), "a submit with no lifetime owes nothing");
    }
}
