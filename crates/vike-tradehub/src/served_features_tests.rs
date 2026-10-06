use super::control::lower_command;
use super::handshake::served_features;
use super::refusal::account_refusal;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_ROUTING, FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT,
    FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_STRATEGY_VERBS, FEATURE_TEARSHEET,
    advertised_datahub, datahub_feature,
};
use vike_tradehub_client::wire::WireCommand;

/// Unconfigured — the default, and every pre-REQ-2 caller — advertises NO datahub entry, and
/// the list is exactly the named capabilities it always was.
#[test]
fn an_unconfigured_daemon_advertises_no_datahub() {
    let features = served_features(None, false);
    assert!(advertised_datahub(&features).is_none());
    assert!(features.iter().all(|f| !f.starts_with("datahub=")));
}

/// Configured — the entry rides BESIDE the named capabilities (nothing is displaced; the
/// exact-match feature guards still find their strings) and round-trips through the client
/// parser verbatim.
#[test]
fn a_configured_daemon_advertises_its_datahub_beside_the_named_capabilities() {
    let features = served_features(Some("127.0.0.1:7878"), false);
    assert_eq!(advertised_datahub(&features), Some("127.0.0.1:7878".to_string()));
    for named in [
        "observe",
        "preview",
        FEATURE_STRATEGY_VERBS,
        FEATURE_SETTINGS_SHOW,
        FEATURE_OBSERVE_HEARTBEAT,
    ] {
        assert!(features.iter().any(|f| f == named), "{named} displaced");
    }
    assert_eq!(features, {
        let mut unconfigured = served_features(None, false);
        unconfigured.push(datahub_feature("127.0.0.1:7878"));
        unconfigured
    });
}

/// The tearsheet capability and its renderer ship TOGETHER — this is the half a wire client
/// negotiates on, and it was deliberately absent while the arm was an IOU. Pinned so that
/// removing [`super::tearsheet::tearsheet_reply`] without removing the string (or the reverse) is caught here
/// rather than by an operator meeting an opaque server error.
#[test]
fn the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it() {
    assert!(served_features(None, false).iter().any(|f| f == FEATURE_TEARSHEET));
}

/// The claim is true again: `crates/vike-core/src/runtime/apply/routed.rs`'s `apply_intent_routed` now
/// reads the account `lower_command` copies onto `vike_model::OrderRequest`, so a node running
/// this build really does route a labelled order rather than dropping it onto the venue's
/// default engine. This test was
/// `the_node_does_not_advertise_account_routing_while_the_field_is_unread` — its assertion is
/// inverted here rather than deleted, per
/// `docs/superpowers/plans/2026-09-22-the-order-payload-names-its-account.md`'s Task 5, so the
/// history of the withholding stays attached to the pin that replaced it.
#[test]
fn the_node_advertises_account_routing_now_that_routing_reads_the_field() {
    let features = served_features(None, false);
    assert!(
        features.iter().any(|f| f == FEATURE_ACCOUNT_ROUTING),
        "the node does not advertise account routing even though `apply_intent_routed` reads \
             the field: {features:?}"
    );
}

/// `account-scoped-submit` and the edge refusal it promises ship TOGETHER — the string a client
/// now demands before it names an account on a `Submit`, because `account-routing` was
/// advertised by six released nodes that discarded the field
/// (`vike_tradehub_client::proto::FEATURE_ACCOUNT_SCOPED_SUBMIT`'s doc carries the measurement).
/// Both halves are asserted, so removing [`super::refusal::account_refusal`]'s `Submit` arm without withdrawing
/// the string — or the reverse — is caught here rather than by an order on a book nobody named.
#[test]
fn the_node_advertises_account_scoped_submit_beside_the_edge_refusal_it_promises() {
    use vike_tradehub_client::wire::WireOrderRequest;

    let features = served_features(None, false);
    assert!(
        features.iter().any(|f| f == FEATURE_ACCOUNT_SCOPED_SUBMIT),
        "a conforming client refuses every account-naming submit to this node: {features:?}"
    );
    let unheld = WireCommand::Submit(WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        account: Some("NOSUCH".into()),
    });
    assert!(
        account_refusal(&unheld, &["binance".to_string()]).is_some(),
        "the string promises an edge refusal of an account this node runs no engine of"
    );
}

/// ⚠ **The reducing verbs' claim, made in the same build that makes it true.** This string was
/// advertised by NO node while `lower_command` dropped the account on `MassCancel`/`Flatten`/
/// `MarketExit` and the core fanned them over every account of the venue — a client obeying
/// the capability rule refused a labelled reduce locally rather than flatten a book nobody
/// named. The core now narrows to the named account and this edge refuses an unheld one before
/// the Ack, so the claim is true and a client may send the field.
#[test]
fn the_node_advertises_account_scoped_reduce_now_that_the_core_honours_it() {
    let features = served_features(None, false);
    assert!(
        features.iter().any(|f| f == FEATURE_ACCOUNT_SCOPED_REDUCE),
        "the node honours a labelled reducing verb and must say so: {features:?}"
    );
}

/// The `bracket` word and the arm that serves it ship TOGETHER. `lower_command` lowers the variant,
/// `account_refusal` answers which book it reaches, and this word tells a client it may send one.
/// Advertising the word before the arm would turn a clean client-side refusal into an opaque server
/// error (`FEATURE_TEARSHEET`'s doc). Both halves are asserted, so withdrawing either alone is caught
/// here; `lower_command_tests`' `a_bracket_lowers_field_for_field_and_echoes_no_coid` pins the arm in
/// full.
#[test]
fn the_node_advertises_the_bracket_verb_beside_the_arm_that_serves_it() {
    // The full path, so this file compiles BEFORE `server.rs` imports the constant, and the red run
    // is a failed assertion rather than a compile error.
    let word = vike_tradehub_client::proto::FEATURE_BRACKET;
    assert!(served_features(None, false).iter().any(|f| f == word), "the word is advertised");
    let bracket = WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
    });
    assert_eq!(
        account_refusal(&bracket, &["binance".to_string()]),
        None,
        "...the gate admits a bracket on a venue's one default account"
    );
    // ...and it is a REAL gate: the arm that admits a default-account bracket refuses one it cannot
    // place (an unpublished roster), which a placeholder answering `None` for every bracket could
    // not do.
    assert!(
        account_refusal(&bracket, &[]).is_some(),
        "...and refuses a bracket while the roster is empty"
    );
    assert!(lower_command(bracket).is_ok(), "...and the arm lowers it");
}
