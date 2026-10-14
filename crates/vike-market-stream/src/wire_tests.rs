//! Unit tests of the stream wire's tables: the stream-word maps, the sub-op lane words and the
//! start-failure round trip. The wire's BYTES are pinned elsewhere (`tests/stream_tag_fixtures.rs`,
//! over the committed `fixtures/stream_wire/` goldens).

use vike_data::StreamLabel;

use super::*;

#[test]
fn the_wire_stream_word_is_total_and_its_own_inverse() {
    for word in ["1m", "15s", "1d", "", "weird word", "ticks", "quotes", "trades", "book", "depth"]
    {
        assert_eq!(WireStream::from_word(word).word(), word, "the word {word:?} must survive");
    }
    for label in StreamLabel::ALL {
        let wire = WireStream::from_word(label.as_str());
        assert_eq!(wire, WireStream::Lane(WireLane::from_stream_label(*label)));
        assert_eq!(WireLane::from_stream_label(*label).stream_label(), *label);
    }
    assert_eq!(WireStream::from_word("ticks"), WireStream::Link);
    assert_eq!(WireStream::from_word("1m"), WireStream::Bar("1m".into()));
}

#[test]
fn a_sub_op_names_the_stream_word_its_disclosures_carry() {
    let op = SubOp::Bars { symbol: "BTCUSDT".into(), interval: "5m".into() };
    assert_eq!(op.symbol(), "BTCUSDT");
    assert_eq!(op.stream().word(), "5m");
    assert_eq!(SubOp::Quotes { symbol: "X".into() }.stream().word(), StreamLabel::Quotes.as_str());
    assert_eq!(SubOp::Trades { symbol: "X".into() }.stream().word(), StreamLabel::Trades.as_str());
    assert_eq!(SubOp::Book { symbol: "X".into() }.stream().word(), StreamLabel::Book.as_str());
    assert_eq!(SubOp::Depth { symbol: "X".into() }.stream().word(), StreamLabel::Depth.as_str());
}

#[test]
fn a_start_failure_crosses_the_wire_both_ways_with_its_variant_and_its_text() {
    use vike_data::LiveDataError;

    let failures = [
        LiveDataError::Subscribe("the OS refused a feed thread".into()),
        LiveDataError::Unsupported(
            "venue's declared VenueCaps.live_data serves no live trade prints",
        ),
        LiveDataError::Unsupported("a bridge's own reason: no live depth feed"),
    ];
    for original in failures {
        let wire = SubErrKind::from_live_error(&original);
        let json = serde_json::to_string(&wire).expect("encodes");
        let back: SubErrKind = serde_json::from_str(&json).expect("decodes");
        let again = back.into_live_error();
        assert_eq!(again.to_string(), original.to_string(), "the text is unchanged");
        assert_eq!(std::mem::discriminant(&again), std::mem::discriminant(&original));
    }
}

#[test]
fn a_wire_class_is_total_and_its_own_inverse_and_orders_like_the_shared_one() {
    let all = [Delivery::Conflatable, Delivery::Lossless, Delivery::Control];
    for class in all {
        assert_eq!(WireDelivery::from_delivery(class).delivery(), class);
    }
    for a in all {
        for b in all {
            assert_eq!(
                WireDelivery::from_delivery(a).cmp(&WireDelivery::from_delivery(b)),
                a.cmp(&b),
                "the wire order must be the strength order"
            );
        }
    }
}

#[test]
fn a_sub_op_names_its_tick_lane_and_a_bar_lane_has_none() {
    assert_eq!(SubOp::Quotes { symbol: "X".into() }.lane(), Some(WireLane::Quotes));
    assert_eq!(SubOp::Trades { symbol: "X".into() }.lane(), Some(WireLane::Trades));
    assert_eq!(SubOp::Book { symbol: "X".into() }.lane(), Some(WireLane::Book));
    assert_eq!(SubOp::Depth { symbol: "X".into() }.lane(), Some(WireLane::Depth));
    assert_eq!(SubOp::Bars { symbol: "X".into(), interval: "1m".into() }.lane(), None);
}

/// A `Sub` without a class is byte-for-byte the frame a peer without the feature reads and writes: the
/// field is absent, not `null`, and an older frame decodes to `None`.
#[test]
fn a_sub_without_a_class_is_the_frame_every_older_peer_speaks() {
    let op = SubOp::Depth { symbol: "BTCUSDT".into() };
    let plain = StreamUp::Sub { sid: 5, op: op.clone(), class: None };
    let json = serde_json::to_string(&plain).expect("encodes");
    assert!(!json.contains("class"), "{json}");
    assert_eq!(json, r#"{"Sub":{"sid":5,"op":{"Depth":{"symbol":"BTCUSDT"}}}}"#);
    let back: StreamUp = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, plain);

    let raised = StreamUp::Sub { sid: 5, op, class: Some(WireDelivery::Lossless) };
    let json = serde_json::to_string(&raised).expect("encodes");
    assert!(json.ends_with(r#""class":"Lossless"}}"#), "{json}");
    assert_eq!(serde_json::from_str::<StreamUp>(&json).expect("decodes"), raised);
}
