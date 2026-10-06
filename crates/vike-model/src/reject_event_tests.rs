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
