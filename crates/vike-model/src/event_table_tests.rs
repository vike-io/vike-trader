//! `Event::TABLE` (expanded by `events!` from the same variant list as the enum) against what
//! serde actually puts on the wire. The table cannot disagree with the enum's renames by
//! construction; these tests hold the WIRE itself still, so a rename edited in the `events!`
//! invocation is a red test and not a silent change of every journal and fixture.

use super::*;

/// The 21 wire tags in declaration order, spelled out. Two variants carry a tag that predates
/// the Rust spelling (`Fill` ships as `FillEvent`, `Funding` as `FundingEvent`): fixtures and the
/// exec journal pin those tags, so only the Rust variant was shortened. The last one,
/// `FeeAdjustmentEvent`, is the newest and follows their rule (the Rust variant drops the suffix, the tag
/// keeps the payload's name): a journal that never carried it has no such tag.
const WIRE_TAGS: [&str; 21] = [
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
    "FeeAdjustmentEvent",
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
    assert_eq!(
        renamed,
        [
            ("Fill", "FillEvent"),
            ("Funding", "FundingEvent"),
            ("FeeAdjustment", "FeeAdjustmentEvent")
        ]
    );
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

/// The newest tag: a fee adjustment is `{"type":"FeeAdjustmentEvent", ...}`, an unset fee currency writes
/// NO key (like a fill's), and it reads back equal. A reader that predates the tag (every build
/// before it) refuses the frame as an unknown variant, which is the whole "older binary" story.
#[test]
fn a_fee_adjustment_travels_under_its_own_tag_and_an_unset_asset_writes_no_key() {
    let adj = FeeAdjustmentEvent {
        trade_id: TradeId::new("0001f4e8.6540b5a1.01.01").expect("non-empty"),
        client_order_id: "c1".into(),
        venue: ustr::ustr("ibkr"),
        symbol: ustr::ustr("AAPL.SMART.USD"),
        last_px: 190.0,
        extra_commission: 1.25,
        commission_asset: ustr::ustr(""),
        ts: 9,
    };
    let json = serde_json::to_string(&Event::FeeAdjustment(adj.clone())).expect("serialize");
    assert!(json.starts_with(r#"{"type":"FeeAdjustmentEvent""#), "{json}");
    assert!(!json.contains("commission_asset"), "an unset asset writes no key: {json}");
    assert_eq!(
        serde_json::from_str::<Event>(&json).expect("round-trips"),
        Event::FeeAdjustment(adj)
    );
    let with_asset = FeeAdjustmentEvent {
        commission_asset: ustr::ustr("USD"),
        ..serde_json::from_str::<FeeAdjustmentEvent>(
            r#"{"trade_id":"t","client_order_id":"c","venue":"ibkr","symbol":"X","last_px":1.0,"extra_commission":0.5}"#,
        )
        .expect("a frame without the optional keys reads")
    };
    let json = serde_json::to_string(&Event::FeeAdjustment(with_asset)).expect("serialize");
    assert!(json.contains(r#""commission_asset":"USD""#), "{json}");
}

/// The adjustment handed to the fee conversions is a fill that moved nothing: zero quantity, the
/// adjustment's own amount as its commission, and the fill's price and symbol.
#[test]
fn the_fee_only_fill_carries_the_amount_and_moves_no_quantity() {
    let adj = FeeAdjustmentEvent {
        trade_id: "t-1".into(),
        client_order_id: "c1".into(),
        venue: ustr::ustr("ibkr"),
        symbol: ustr::ustr("AAPL.SMART.USD"),
        last_px: 190.0,
        extra_commission: 1.25,
        commission_asset: ustr::ustr("USD"),
        ts: 9,
    };
    let fill = adj.as_fee_only_fill();
    assert_eq!((fill.last_qty, fill.commission, fill.last_px), (0.0, 1.25, 190.0));
    assert_eq!((fill.trade_id.as_str(), fill.symbol.as_str()), ("t-1", "AAPL.SMART.USD"));
    assert_eq!(fill.commission_asset.as_str(), "USD");
}
