//! `Event::TABLE` (expanded by `events!` from the same variant list as the enum) against what
//! serde actually puts on the wire. The table cannot disagree with the enum's renames by
//! construction; these tests hold the WIRE itself still, so a rename edited in the `events!`
//! invocation is a red test and not a silent change of every journal and fixture.

use super::*;

/// The 20 wire tags in declaration order, spelled out. Two variants carry a tag that predates
/// the Rust spelling (`Fill` ships as `FillEvent`, `Funding` as `FundingEvent`): fixtures and the
/// exec journal pin those tags, so only the Rust variant was shortened.
const WIRE_TAGS: [&str; 20] = [
    "FillEvent",
    "OrderSubmitted",
    "OrderAccepted",
    "OrderRejected",
    "OrderDenied",
    "OrderTriggered",
    "OrderPartiallyFilled",
    "OrderFilled",
    "OrderCanceled",
    "OrderExpired",
    "OrderLiquidated",
    "OrderModified",
    "PositionOpened",
    "PositionChanged",
    "PositionClosed",
    "AccountState",
    "FundingEvent",
    "PositionLiquidated",
    "OrderCancelRejected",
    "OrderModifyRejected",
];

/// The published table's tags are the pinned wire tags, in order.
#[test]
fn the_table_carries_the_pinned_wire_tags() {
    let table: Vec<&str> = Event::TABLE.iter().map(|r| r.wire_tag).collect();
    assert_eq!(table, WIRE_TAGS);
    let renamed: Vec<(&str, &str)> = Event::TABLE
        .iter()
        .filter(|r| r.variant != r.wire_tag)
        .map(|r| (r.variant, r.wire_tag))
        .collect();
    assert_eq!(renamed, [("Fill", "FillEvent"), ("Funding", "FundingEvent")]);
    for r in Event::TABLE.iter().filter(|r| r.variant == r.wire_tag) {
        assert_eq!(r.payload, r.variant, "an un-renamed variant wraps the payload of its name");
    }
}

/// serde's own list of accepted `"type"` tags equals the table's: asking it to deserialize a tag
/// it does not know makes it print the variants it expects, in the order the enum declares them,
/// under their renames. Nothing the table names is missing from the wire, and nothing the wire
/// accepts is missing from the table.
#[test]
fn serde_accepts_exactly_the_tags_the_table_lists() {
    let err = serde_json::from_str::<Event>(r#"{"type":"__no_such_event__"}"#)
        .expect_err("an unknown tag must be refused");
    let message = err.to_string();
    // "unknown variant `__no_such_event__`, expected one of `FillEvent`, `OrderSubmitted`, ..."
    // so the backtick-quoted spans are: the offender, then every accepted tag.
    let accepted: Vec<&str> = message.split('`').skip(1).step_by(2).skip(1).collect();
    let table: Vec<&str> = Event::TABLE.iter().map(|r| r.wire_tag).collect();
    assert_eq!(accepted, table, "serde's expected-variant list: {message}");
}

/// Every table tag is a tag serde recognizes: a body carrying just the tag gets past the variant
/// lookup and fails on a missing payload field, never on an unknown variant.
#[test]
fn every_table_tag_selects_a_variant() {
    for row in Event::TABLE {
        let body = format!(r#"{{"type":"{}"}}"#, row.wire_tag);
        let message = match serde_json::from_str::<Event>(&body) {
            Ok(event) => panic!("a bare tag {} parsed as {event:?}", row.wire_tag),
            Err(e) => e.to_string(),
        };
        assert!(
            !message.contains("unknown variant"),
            "{} is in the table but serde does not know it: {message}",
            row.wire_tag
        );
    }
}

/// The two renamed variants serialize under their wire tag, not their Rust name, and the others
/// under their own. (The remaining payloads' round trips are the route-key and reject tests'.)
#[test]
fn the_renamed_variants_serialize_under_their_wire_tags() {
    let fill = Event::Fill(FillEvent {
        trade_id: TradeId::new("T-1").expect("non-empty"),
        client_order_id: "c1".into(),
        venue: ustr::ustr("binance"),
        symbol: ustr::ustr("BTCUSDT"),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: ustr::ustr("USDT"),
        liquidity_side: LiquiditySide::Taker,
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    });
    let funding = Event::Funding(FundingEvent {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount: -1.25,
        mark_price: None,
        ts: 7,
        route_key: None,
        ccy: None,
    });
    let reject = Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: "c1".into(),
        reason: "r".into(),
        ts: 1,
    });
    for (event, tag) in
        [(fill, "FillEvent"), (funding, "FundingEvent"), (reject, "OrderCancelRejected")]
    {
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.starts_with(&format!(r#"{{"type":"{tag}""#)), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).expect("round-trips"), event);
    }
}
