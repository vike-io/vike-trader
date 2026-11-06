//! [`super::account_event_sender`]: puts an account's identity on the ONE venue-tagged payload
//! that carries no symbol.
//!
//! Gates the pairing: `account_route_key` renders the bare venue id for the DEFAULT account and
//! `vike_exec::EventSender::routed` stamps nothing when the key equals the venue, so a
//! single-account box emits `route_key: None` with unchanged journal bytes — which lets
//! `make_engine_for_account` call it unconditionally. Driven through the REAL `EventSender` and
//! ingest channel: "does the payload change" is the question, not "is a field set".

use super::{account_event_sender, account_route_key};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{AccountState, Event};

const VENUE: &str = "binance";

/// What the lane actually delivered, given the account label the mount would have resolved.
fn delivered(label: &AccountLabel) -> AccountState {
    let (plain, mut rx) = vike_exec::event_channel(4);
    let scoped = account_event_sender(&plain, &account_route_key(VENUE, label));
    scoped
        .blocking_send(Event::AccountState(AccountState {
            venue: VENUE.into(),
            balances: vec![("USDT".to_string(), 100.0)],
            ts: 1,
            route_key: None,
        }))
        .expect("receiver alive");
    match rx.blocking_recv() {
        Some(vike_exec::Ingest::Event(Event::AccountState(a))) => a,
        other => panic!("expected an AccountState ingest, got {other:?}"),
    }
}

/// THE INERTNESS HALF (every existing deployment).
#[test]
fn the_default_accounts_lane_leaves_the_payload_byte_identical() {
    let got = delivered(&AccountLabel::Default);
    assert_eq!(
        got.route_key, None,
        "a box with no labelled account must emit the payload it always emitted"
    );
    assert_eq!(
        serde_json::to_string(&got).expect("serialize"),
        r#"{"venue":"binance","balances":[["USDT",100.0]],"ts":1}"#,
        "…and that means no `route_key` key on the wire at all"
    );
}

/// THE WORKING HALF: a labelled account's lane stamps the key `vike_core`'s router folds on,
/// the SAME string as the engine's own `route_key`.
#[test]
fn a_labelled_accounts_lane_stamps_the_engines_own_route_key() {
    let label = AccountLabel::parse("ALT").expect("a valid label");
    let expected = account_route_key(VENUE, &label);
    assert_ne!(expected, VENUE, "precondition: a labelled account decorates its key");
    assert_eq!(delivered(&label).route_key, Some(expected.as_str().into()));
}
