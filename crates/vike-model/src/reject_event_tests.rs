use super::*;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[test]
fn order_cancel_rejected_round_trips_with_wire_tag() {
    let event = Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: "c1".into(),
        reason: "network error: timed out".into(),
        timestamp: 42,
        refusal_id: None,
    });
    let json = serde_json::to_string(&event).expect("serialize");
    assert!(
        json.contains("\"type\":\"OrderCancelRejected\""),
        "wire tag must be OrderCancelRejected, got {json}"
    );
    let back: Event = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(event, back);
}

#[test]
fn order_modify_rejected_round_trips_with_wire_tag() {
    let event = Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: "c2".into(),
        reason: "modify rejected".into(),
        timestamp: 7,
        refusal_id: None,
    });
    let json = serde_json::to_string(&event).expect("serialize");
    assert!(
        json.contains("\"type\":\"OrderModifyRejected\""),
        "wire tag must be OrderModifyRejected, got {json}"
    );
    let back: Event = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(event, back);
}

/// An `OrderRejected` that carries no cause is the event it always was: not one byte moves in a journal
/// segment, a gateway ring frame or a fixture, an old record reads back with `cause: None`, and an
/// older reader that meets the key of a caused one ignores it.
#[test]
fn an_uncaused_rejection_is_byte_identical() {
    let event = Event::OrderRejected(OrderRejected {
        order_client_id: "c".into(),
        reason: "r".into(),
        timestamp: 3,
        cause: None,
        refusal_id: None,
    });
    let json = serde_json::to_string(&event).expect("serialize");
    assert_eq!(
        json,
        r#"{"type":"OrderRejected","order_client_id":"c","reason":"r","timestamp":3}"#
    );
    let old = r#"{"type":"OrderRejected","order_client_id":"c","reason":"r","timestamp":3}"#;
    assert_eq!(serde_json::from_str::<Event>(old).expect("an old record reads"), event);
}

/// The typed cause rides the same event and spells itself in snake case; it survives a round trip.
#[test]
fn a_nothing_to_reduce_cause_round_trips() {
    let event = Event::OrderRejected(OrderRejected {
        order_client_id: "c".into(),
        reason: "reduce-only: no open long order to reduce".into(),
        timestamp: 3,
        cause: Some(RejectCause::NothingToReduce),
        refusal_id: None,
    });
    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.ends_with(r#","cause":"nothing_to_reduce"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), event);
}

/// The post-only refusal is a typed cause too: snake case on the wire, and it round-trips.
#[test]
fn a_post_only_would_cross_cause_round_trips() {
    let event = Event::OrderRejected(OrderRejected {
        order_client_id: "c".into(),
        reason: "post-only order would take liquidity".into(),
        timestamp: 3,
        cause: Some(RejectCause::PostOnlyWouldCross),
        refusal_id: None,
    });
    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.ends_with(r#","cause":"post_only_would_cross"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), event);
}

/// The journal's msgpack: every struct a map of its field names, serde's human-readable mode
/// (`crates/vike-journal/src/frame.rs`'s `encode_into`).
fn journal_bytes(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut serializer =
        rmp_serde::Serializer::new(&mut bytes).with_struct_map().with_human_readable();
    value.serialize(&mut serializer).expect("serializes");
    bytes
}

/// The four refusal events as a build before `refusal_id` declared them (and before a cause word it did
/// not know read as `None`): the OLDER READER of the tests below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OlderRejectCause {
    NothingToReduce,
    PostOnlyWouldCross,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OlderOrderRejected {
    order_client_id: String,
    #[serde(default)]
    reason: CompactString,
    #[serde(default)]
    timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cause: Option<OlderRejectCause>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OlderRefusal {
    order_client_id: String,
    #[serde(default)]
    reason: CompactString,
    #[serde(default)]
    timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[expect(clippy::enum_variant_names, reason = "the variants are the four events' wire tags")]
enum OlderEvent {
    OrderRejected(OlderOrderRejected),
    OrderDenied(OlderRefusal),
    OrderCancelRejected(OlderRefusal),
    OrderModifyRejected(OlderRefusal),
}

/// The four refusal events, with or without an id.
fn the_four(refusal_id: Option<CompactString>) -> [Event; 4] {
    let (order_client_id, reason, timestamp) = (String::from("c"), CompactString::from("r"), 3);
    [
        Event::OrderRejected(OrderRejected {
            order_client_id: order_client_id.clone(),
            reason: reason.clone(),
            timestamp,
            cause: None,
            refusal_id: refusal_id.clone(),
        }),
        Event::OrderDenied(OrderDenied {
            order_client_id: order_client_id.clone(),
            reason: reason.clone(),
            timestamp,
            refusal_id: refusal_id.clone(),
        }),
        Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: order_client_id.clone(),
            reason: reason.clone(),
            timestamp,
            refusal_id: refusal_id.clone(),
        }),
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id,
            reason,
            timestamp,
            refusal_id,
        }),
    ]
}

/// The same four as the older build wrote them.
fn the_four_as_the_older_build_wrote_them() -> [OlderEvent; 4] {
    let refusal = || OlderRefusal { order_client_id: "c".into(), reason: "r".into(), timestamp: 3 };
    [
        OlderEvent::OrderRejected(OlderOrderRejected {
            order_client_id: "c".into(),
            reason: "r".into(),
            timestamp: 3,
            cause: None,
        }),
        OlderEvent::OrderDenied(refusal()),
        OlderEvent::OrderCancelRejected(refusal()),
        OlderEvent::OrderModifyRejected(refusal()),
    ]
}

/// An event with no refusal id is the event it always was: the same JSON and the same journal msgpack,
/// byte for byte, as the build before the field wrote, and those bytes read back as `refusal_id: None`.
#[test]
fn an_event_without_a_refusal_id_is_byte_identical() {
    for (event, old) in the_four(None).iter().zip(the_four_as_the_older_build_wrote_them()) {
        let json = serde_json::to_string(event).expect("serialize");
        assert_eq!(json, serde_json::to_string(&old).expect("serialize"), "{event:?}");
        assert!(!json.contains("refusal_id"), "{json}");
        assert_eq!(journal_bytes(event), journal_bytes(&old), "{event:?}");
        assert_eq!(&serde_json::from_str::<Event>(&json).expect("an old record reads"), event);
        let back: Event = rmp_serde::from_slice(&journal_bytes(&old)).expect("an old frame reads");
        assert_eq!(&back, event);
    }
    assert_eq!(
        serde_json::to_string(&the_four(None)[1]).expect("serialize"),
        r#"{"type":"OrderDenied","order_client_id":"c","reason":"r","timestamp":3}"#
    );
}

/// An OLDER reader (a desktop, a CLI, a journal replay of the build before the field) decodes a newer
/// node's event that carries an id, in JSON and in the journal's msgpack: it ignores the key and keeps the
/// reason, and the id rides the newer build's round trip.
#[test]
fn an_older_reader_decodes_an_event_that_carries_a_refusal_id() {
    let with_id = the_four(crate::events::refusal_id_of(crate::OrderDenialId::Halted.word()));
    for (event, old) in with_id.iter().zip(the_four_as_the_older_build_wrote_them()) {
        let json = serde_json::to_string(event).expect("serialize");
        assert!(json.ends_with(r#","refusal_id":"order.risk.halted"}"#), "{json}");
        assert_eq!(serde_json::from_str::<OlderEvent>(&json).expect("an older reader"), old);
        let frame = journal_bytes(event);
        let replayed: OlderEvent = rmp_serde::from_slice(&frame).expect("an older replay");
        assert_eq!(replayed, old);
        assert_eq!(&serde_json::from_str::<Event>(&json).expect("round trip"), event);
        assert_eq!(&rmp_serde::from_slice::<Event>(&frame).expect("round trip"), event);
    }
}

/// A cause word this build does not know (a newer node's typed cause) reads as no cause, in JSON and in
/// the journal's msgpack, whatever its shape: the rejection decodes and keeps its reason and its id.
#[test]
fn a_cause_this_build_does_not_know_reads_as_none() {
    let expected = Event::OrderRejected(OrderRejected {
        order_client_id: "c".into(),
        reason: "r".into(),
        timestamp: 3,
        cause: None,
        refusal_id: crate::events::refusal_id_of("order.venue_refused"),
    });
    for cause in [r#""a_later_cause""#, r#"{"a_later_cause":{"code":-2010}}"#, "7"] {
        let json = format!(
            r#"{{"type":"OrderRejected","order_client_id":"c","reason":"r","timestamp":3,"cause":{cause},"refusal_id":"order.venue_refused"}}"#
        );
        assert_eq!(serde_json::from_str::<Event>(&json).expect("decodes"), expected, "{json}");
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        let frame = journal_bytes(&value);
        assert_eq!(rmp_serde::from_slice::<Event>(&frame).expect("decodes"), expected, "{json}");
    }
    let known = r#"{"type":"OrderRejected","order_client_id":"c","reason":"r","timestamp":3,"cause":"post_only_would_cross"}"#;
    let Event::OrderRejected(read) = serde_json::from_str::<Event>(known).expect("known") else {
        panic!("an OrderRejected");
    };
    assert_eq!(read.cause, Some(RejectCause::PostOnlyWouldCross));
}

/// [`OrderCanceled`] as a build before `post_only_would_cross` declared it (the same three fields as
/// [`OlderRefusal`]), inside the one-variant shape of its `Event`: what an older reader decodes.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
enum EventBeforeTheField {
    OrderCanceled(OlderRefusal),
}

fn json_bytes(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("serializes")
}

fn json_read<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
    serde_json::from_slice(bytes).expect("reads")
}

/// The hub frame's msgpack (`vike_hub_protocol::frame::BodyCodec`): field names, no human-readable mode.
fn hub_bytes(value: &impl Serialize) -> Vec<u8> {
    rmp_serde::to_vec_named(value).expect("serializes")
}

fn hub_read<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
    rmp_serde::from_slice(bytes).expect("reads")
}

fn journal_read<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
    let mut deserializer = rmp_serde::Deserializer::from_read_ref(bytes).with_human_readable();
    T::deserialize(&mut deserializer).expect("reads")
}

type Codec<Before> = (
    &'static str,
    fn(&Event) -> Vec<u8>,
    fn(&Before) -> Vec<u8>,
    fn(&[u8]) -> Event,
    fn(&[u8]) -> Before,
);

/// Every encoding an event rides, each with its writer and reader for this build's [`Event`] and for `Before`, the
/// event as an older build declared it: JSON (fixtures, the journal before format 34, a hub's JSON leg), a hub's
/// msgpack and the journal's.
fn codecs<Before: Serialize + serde::de::DeserializeOwned>() -> [Codec<Before>; 3] {
    [
        ("json", json_bytes, json_bytes, json_read, json_read),
        ("hub msgpack", hub_bytes, hub_bytes, hub_read, hub_read),
        ("journal msgpack", journal_bytes, journal_bytes, journal_read, journal_read),
    ]
}

/// The post-only cross on a cancel is a typed field, and it crosses every encoding an event rides, both
/// ways: JSON (fixtures, the journal before format 34, a hub's JSON leg), a hub's msgpack and the
/// journal's. A plain cancel keeps its bytes, an older record reads back `false`, and an older reader
/// decodes a flagged cancel with its words, the ones it decided on.
#[test]
fn a_post_only_cross_cancel_reads_both_ways() {
    let cancel = |post_only_would_cross| {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "c".into(),
            reason: POST_ONLY_CANCEL_REASON.into(),
            timestamp: 3,
            post_only_would_cross,
            refusal_id: None,
        })
    };
    let before = EventBeforeTheField::OrderCanceled(OlderRefusal {
        order_client_id: "c".into(),
        reason: POST_ONLY_CANCEL_REASON.into(),
        timestamp: 3,
    });
    for (codec, write, write_before, read, read_before) in codecs::<EventBeforeTheField>() {
        assert_eq!(write(&cancel(false)), write_before(&before), "{codec}: a plain cancel's bytes");
        assert_eq!(read(&write_before(&before)), cancel(false), "{codec}: an older record");
        assert_eq!(read(&write(&cancel(true))), cancel(true), "{codec}: the round trip");
        assert_eq!(read_before(&write(&cancel(true))), before, "{codec}: an older reader");
    }
    assert_eq!(
        String::from_utf8(json_bytes(&cancel(true))).expect("text"),
        r#"{"type":"OrderCanceled","order_client_id":"c","reason":"post-only: would have taken liquidity","timestamp":3,"post_only_would_cross":true}"#
    );
}

/// [`OrderCanceled`] as a build before `refusal_id` declared it (`post_only_would_cross` its last field), inside
/// the one-variant shape of its `Event`: what an older reader decodes.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
enum EventBeforeTheCancelId {
    OrderCanceled(CanceledBeforeTheId),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct CanceledBeforeTheId {
    order_client_id: String,
    #[serde(default)]
    reason: CompactString,
    #[serde(default)]
    timestamp: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    post_only_would_cross: bool,
}

/// A cancel's refusal id (the venue's refusal of an order it had taken) crosses every encoding an event rides, both
/// ways, as the post-only flag does: a cancel without one keeps its bytes, post-only flagged or not, an older record
/// reads back `None`, and an older reader decodes a cancel that carries one with its words and its flag.
#[test]
fn a_cancel_refusal_id_reads_both_ways() {
    let venue_refused = || crate::events::refusal_id_of(crate::OrderRefusalId::VenueRefused.word());
    let cancel = |post_only_would_cross, refusal_id| {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "c".into(),
            reason: "rejected".into(),
            timestamp: 3,
            post_only_would_cross,
            refusal_id,
        })
    };
    let before = |post_only_would_cross| {
        EventBeforeTheCancelId::OrderCanceled(CanceledBeforeTheId {
            order_client_id: "c".into(),
            reason: "rejected".into(),
            timestamp: 3,
            post_only_would_cross,
        })
    };
    for (codec, write, write_before, read, read_before) in codecs::<EventBeforeTheCancelId>() {
        for crossed in [false, true] {
            let (plain, identified) = (cancel(crossed, None), cancel(crossed, venue_refused()));
            assert_eq!(write(&plain), write_before(&before(crossed)), "{codec}: a cancel's bytes");
            assert_eq!(read(&write_before(&before(crossed))), plain, "{codec}: an older record");
            assert_eq!(read(&write(&identified)), identified, "{codec}: the round trip");
            assert_eq!(
                read_before(&write(&identified)),
                before(crossed),
                "{codec}: an older reader"
            );
        }
    }
    assert_eq!(
        String::from_utf8(json_bytes(&cancel(false, venue_refused()))).expect("text"),
        r#"{"type":"OrderCanceled","order_client_id":"c","reason":"rejected","timestamp":3,"refusal_id":"order.venue_refused"}"#
    );
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
