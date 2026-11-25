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
        order_client_id: "c1".into(),
        venue: vike_marketdata::Symbol::new("binance"),
        symbol: vike_marketdata::Symbol::new("BTCUSDT"),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new("USDT"),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
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
        timestamp: 7,
        route_key: None,
        currency: None,
    });
    let reject = Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: "c1".into(),
        reason: "r".into(),
        timestamp: 1,
        refusal_id: None,
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
        order_client_id: "c1".into(),
        venue: vike_marketdata::Symbol::new("ibkr"),
        symbol: vike_marketdata::Symbol::new("AAPL.SMART.USD"),
        last_price: 190.0,
        extra_commission: 1.25,
        commission_asset: vike_marketdata::Symbol::new(""),
        timestamp: 9,
    };
    let json = serde_json::to_string(&Event::FeeAdjustment(adj.clone())).expect("serialize");
    assert!(json.starts_with(r#"{"type":"FeeAdjustmentEvent""#), "{json}");
    assert!(!json.contains("commission_asset"), "an unset asset writes no key: {json}");
    assert_eq!(
        serde_json::from_str::<Event>(&json).expect("round-trips"),
        Event::FeeAdjustment(adj)
    );
    let with_asset = FeeAdjustmentEvent {
        commission_asset: vike_marketdata::Symbol::new("USD"),
        ..serde_json::from_str::<FeeAdjustmentEvent>(
            r#"{"trade_id":"t","order_client_id":"c","venue":"ibkr","symbol":"X","last_price":1.0,"extra_commission":0.5}"#,
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
        order_client_id: "c1".into(),
        venue: vike_marketdata::Symbol::new("ibkr"),
        symbol: vike_marketdata::Symbol::new("AAPL.SMART.USD"),
        last_price: 190.0,
        extra_commission: 1.25,
        commission_asset: vike_marketdata::Symbol::new("USD"),
        timestamp: 9,
    };
    let fill = adj.as_fee_only_fill();
    assert_eq!((fill.last_qty, fill.commission, fill.last_price), (0.0, 1.25, 190.0));
    assert_eq!((fill.trade_id.as_str(), fill.symbol.as_str()), ("t-1", "AAPL.SMART.USD"));
    assert_eq!(fill.commission_asset.as_str(), "USD");
}

/// `Event::wire_tag` — the name the order FSM's refusals (`OrderTransitionError`) and the r5
/// parity suite's delivered sequence give an event — is the very `type` serde writes, for a plain
/// variant and for the renamed ones alike. A mutant that hands a variant its Rust name instead of
/// its tag goes red here.
#[test]
fn an_events_wire_tag_is_the_type_serde_writes() {
    let adj = FeeAdjustmentEvent {
        trade_id: "t-1".into(),
        order_client_id: "c1".into(),
        venue: vike_marketdata::Symbol::new("ibkr"),
        symbol: vike_marketdata::Symbol::new("AAPL.SMART.USD"),
        last_price: 190.0,
        extra_commission: 1.25,
        commission_asset: vike_marketdata::Symbol::new("USD"),
        timestamp: 9,
    };
    let events = [
        (
            Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 1 }),
            "OrderSubmitted",
        ),
        (Event::Fill(adj.as_fee_only_fill()), "FillEvent"),
        (Event::FeeAdjustment(adj), "FeeAdjustmentEvent"),
    ];
    for (event, tag) in events {
        assert_eq!(event.wire_tag(), tag);
        assert_eq!(serde_json::to_value(&event).unwrap()["type"], tag);
    }
}

/// One event of a wire tag, naming order `c1`: each payload reads what it needs of this one body and
/// ignores the rest.
fn event_of(tag: &str) -> Event {
    serde_json::from_value(serde_json::json!({
        "type": tag, "order_client_id": "c1", "trade_id": "t1", "venue": "v", "symbol": "s",
        "position_side": "BOTH", "qty": 1.0, "average_price": 1.0, "funding_rate": 0.0, "amount": 0.0,
        "liquidation_price": 1.0, "side": 1, "last_qty": 1.0, "last_price": 1.0, "extra_commission": 0.0,
        "fill": { "trade_id": "t1", "order_client_id": "c1", "venue": "v", "symbol": "s",
                  "side": 1, "last_qty": 1.0, "last_price": 1.0 },
    }))
    .unwrap_or_else(|e| panic!("{tag}: {e}"))
}

/// Which order each variant ends, the whole table spelled out in its order: a new variant is a red
/// row here as well as a compile error in `Event::ended_order`.
const ENDS_AN_ORDER: [(&str, bool); 21] = [
    ("FillEvent", false),
    ("OrderSubmitted", false),
    ("OrderAccepted", false),
    ("OrderRejected", true),
    ("OrderDenied", true),
    ("OrderTriggered", false),
    ("OrderPartiallyFilled", false),
    ("OrderFilled", true),
    ("OrderCanceled", true),
    ("OrderExpired", true),
    ("OrderLiquidated", false),
    ("OrderModified", false),
    ("PositionOpened", false),
    ("PositionChanged", false),
    ("PositionClosed", false),
    ("AccountState", false),
    ("FundingEvent", false),
    ("PositionLiquidated", false),
    ("OrderCancelRejected", false),
    ("OrderModifyRejected", false),
    ("FeeAdjustmentEvent", false),
];

#[test]
fn every_variant_of_the_table_says_which_order_it_ends() {
    let tags: Vec<&str> = Event::TABLE.iter().map(|r| r.wire_tag).collect();
    assert_eq!(tags, ENDS_AN_ORDER.map(|(tag, _)| tag));
    for (tag, ends) in ENDS_AN_ORDER {
        assert_eq!(event_of(tag).ended_order(), ends.then_some("c1"), "{tag}");
    }
}

/// The event's answer and the status's: from wherever the FSM takes it, an event that ends an order lands
/// on a status `OrderStatus::is_terminal` calls terminal, and no other event does.
#[test]
fn an_event_ends_an_order_exactly_when_the_state_machine_lands_it_on_a_terminal_status() {
    use crate::{OrderRecord, OrderRequest, OrderStatus};
    for (tag, ends) in ENDS_AN_ORDER {
        let event = event_of(tag);
        let landed: Vec<OrderStatus> =
            [OrderStatus::Initialized, OrderStatus::Submitted, OrderStatus::Accepted]
                .into_iter()
                .filter_map(|from| {
                    let mut order = OrderRecord::new(OrderRequest::default());
                    order.status = from;
                    order.apply(&event).ok().map(|()| order.status)
                })
                .collect();
        assert!(landed.iter().all(|to| to.is_terminal() == ends), "{tag} lands on {landed:?}");
        assert!(!ends || !landed.is_empty(), "{tag} ends an order the FSM never takes it to");
    }
}

/// Exactly three variants carry a fill (the bare `Fill` and the two fill-carrying lifecycle events),
/// over the whole table; `fill_mut` reaches the same fill `fill` reads.
#[test]
fn exactly_the_three_fill_carrying_variants_hand_out_their_fill() {
    const CARRIES_A_FILL: [&str; 3] = ["FillEvent", "OrderPartiallyFilled", "OrderFilled"];
    for (tag, _) in ENDS_AN_ORDER {
        let mut event = event_of(tag);
        let carries = CARRIES_A_FILL.contains(&tag);
        assert_eq!(event.fill().map(|f| f.trade_id.as_str()), carries.then_some("t1"), "{tag}");
        if let Some(f) = event.fill_mut() {
            f.commission = 0.25;
        }
        assert_eq!(event.fill().map(|f| f.commission), carries.then_some(0.25), "{tag}");
    }
}

#[test]
fn a_maker_flag_is_a_maker_or_a_taker_never_unknown() {
    assert_eq!(LiquiditySide::from_maker(true), LiquiditySide::Maker);
    assert_eq!(LiquiditySide::from_maker(false), LiquiditySide::Taker);
}
