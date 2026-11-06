use super::*;

#[test]
fn order_cancel_rejected_round_trips_with_wire_tag() {
    let ev = Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: "c1".into(),
        reason: "network error: timed out".into(),
        ts: 42,
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(
        json.contains("\"type\":\"OrderCancelRejected\""),
        "wire tag must be OrderCancelRejected, got {json}"
    );
    let back: Event = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(ev, back);
}

#[test]
fn order_modify_rejected_round_trips_with_wire_tag() {
    let ev = Event::OrderModifyRejected(OrderModifyRejected {
        client_order_id: "c2".into(),
        reason: "modify rejected".into(),
        ts: 7,
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(
        json.contains("\"type\":\"OrderModifyRejected\""),
        "wire tag must be OrderModifyRejected, got {json}"
    );
    let back: Event = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(ev, back);
}

/// An `OrderRejected` that carries no cause is the event it always was: not one byte moves in a journal
/// segment, a gateway ring frame or a fixture, an old record reads back with `cause: None`, and an
/// older reader that meets the key of a caused one ignores it.
#[test]
fn an_uncaused_rejection_is_byte_identical() {
    let ev = Event::OrderRejected(OrderRejected {
        client_order_id: "c".into(),
        reason: "r".into(),
        ts: 3,
        cause: None,
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert_eq!(json, r#"{"type":"OrderRejected","client_order_id":"c","reason":"r","ts":3}"#);
    let old = r#"{"type":"OrderRejected","client_order_id":"c","reason":"r","ts":3}"#;
    assert_eq!(serde_json::from_str::<Event>(old).expect("an old record reads"), ev);
}

/// The typed cause rides the same event and spells itself in snake case; it survives a round trip.
#[test]
fn a_nothing_to_reduce_cause_round_trips() {
    let ev = Event::OrderRejected(OrderRejected {
        client_order_id: "c".into(),
        reason: "reduce-only: no open long order to reduce".into(),
        ts: 3,
        cause: Some(RejectCause::NothingToReduce),
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.ends_with(r#","cause":"nothing_to_reduce"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), ev);
}

/// The post-only refusal is a typed cause too: snake case on the wire, and it round-trips.
#[test]
fn a_post_only_would_cross_cause_round_trips() {
    let ev = Event::OrderRejected(OrderRejected {
        client_order_id: "c".into(),
        reason: "post-only order would take liquidity".into(),
        ts: 3,
        cause: Some(RejectCause::PostOnlyWouldCross),
    });
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.ends_with(r#","cause":"post_only_would_cross"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), ev);
}

/// LiquiditySide must be byte-identical to the old `String` field: lowercase maker/taker,
/// empty string for the unsurfaced case, and any unknown/legacy label folds to Unknown.
#[test]
fn liquidity_side_wire_is_byte_identical() {
    for (variant, wire) in [
        (LiquiditySide::Maker, "\"maker\""),
        (LiquiditySide::Taker, "\"taker\""),
        (LiquiditySide::Unknown, "\"\""),
    ] {
        assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
        assert_eq!(serde_json::from_str::<LiquiditySide>(wire).unwrap(), variant);
    }
    // legacy/unknown labels and a missing field both fold to Unknown (the old "" default)
    assert_eq!(serde_json::from_str::<LiquiditySide>("\"foo\"").unwrap(), LiquiditySide::Unknown);
    assert_eq!(LiquiditySide::default(), LiquiditySide::Unknown);
}
