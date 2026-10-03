use super::*;

/// THE PIN: the exact JSON an unstamped `AccountState` produced BEFORE `route_key` existed,
/// spelled as a literal rather than derived from the type, so a change to the type cannot move
/// both sides together. (That failure mode is not hypothetical in this workspace — a fixture
/// seeded through the very function under test makes an equality between two identical wrong
/// answers pass.)
const PRE_FIELD_WIRE: &str = concat!(
    r#"{"type":"AccountState","venue":"binance","#,
    r#""balances":[["USDT",1234.5]],"ts":7}"#
);

fn unstamped() -> Event {
    Event::AccountState(AccountState {
        venue: "binance".into(),
        balances: vec![("USDT".to_string(), 1234.5)],
        ts: 7,
        route_key: None,
    })
}

#[test]
fn an_unstamped_snapshot_is_byte_identical_to_the_pre_field_wire() {
    assert_eq!(
        serde_json::to_string(&unstamped()).expect("serialize"),
        PRE_FIELD_WIRE,
        "a default-account box must emit no `route_key` key at all — journal bytes, fixtures \
             and captures all depend on it"
    );
}

/// The READ direction of the same property: bytes written by a build that had no such field
/// (every journal segment and fixture on disk today) parse, and mean "the venue's sole
/// account" rather than erroring or inventing a key.
#[test]
fn pre_field_bytes_read_back_as_the_sole_account() {
    let back: Event = serde_json::from_str(PRE_FIELD_WIRE).expect("deserialize");
    assert_eq!(back, unstamped());
    match back {
        Event::AccountState(a) => assert_eq!(a.route_key, None),
        other => panic!("expected AccountState, got {other:?}"),
    }
}

/// …and a STAMPED one carries the key, round-trips, and is NOT equal to the unstamped twin —
/// the property `vike_core`'s router reads.
#[test]
fn a_stamped_snapshot_carries_its_route_key_and_round_trips() {
    let ev = Event::AccountState(AccountState {
        venue: "binance".into(),
        balances: vec![("USDT".to_string(), 1234.5)],
        ts: 7,
        route_key: Some("binance#alt".into()),
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.contains(r#""route_key":"binance#alt""#), "got {json}");
    assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), ev);
    assert_ne!(ev, unstamped(), "the stamp must be observable, not cosmetic");
}
