use super::*;

/// THE PINS: the exact JSON each payload produced BEFORE `route_key` existed, spelled as
/// literals for the reason the `AccountState` twin gives — a fixture derived from the type
/// under test moves both sides together and proves nothing.
const PRE_FIELD_FUNDING: &str = concat!(
    r#"{"type":"FundingEvent","venue":"binance","symbol":"BTCUSDT","#,
    r#""position_side":"BOTH","funding_rate":0.0001,"amount":-1.25,"#,
    r#""mark_price":null,"ts":7}"#
);

const PRE_FIELD_LIQ: &str = concat!(
    r#"{"type":"PositionLiquidated","venue":"binance","symbol":"BTCUSDT","#,
    r#""position_side":"BOTH","qty":1.0,"liq_price":90.0,"fee":0.2,"ts":7,"#,
    r#""trade_id":"l1"}"#
);

fn unstamped_funding() -> Event {
    Event::Funding(FundingEvent {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount: -1.25,
        mark_price: None,
        ts: 7,
        route_key: None,
        ccy: None,
    })
}

fn unstamped_liquidation() -> Event {
    Event::PositionLiquidated(PositionLiquidated {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        qty: 1.0,
        liq_price: 90.0,
        fee: 0.2,
        ts: 7,
        trade_id: "l1".into(),
        route_key: None,
        fee_ccy: None,
    })
}

#[test]
fn an_unstamped_payload_is_byte_identical_to_the_pre_field_wire() {
    assert_eq!(
        serde_json::to_string(&unstamped_funding()).expect("serialize"),
        PRE_FIELD_FUNDING,
        "a default-account box must emit no `route_key` key at all"
    );
    assert_eq!(
        serde_json::to_string(&unstamped_liquidation()).expect("serialize"),
        PRE_FIELD_LIQ,
        "a default-account box must emit no `route_key` key at all"
    );
}

/// The READ direction: bytes written by a build that had no such field — every journal segment
/// and every captured fixture on disk today — parse, and mean "the venue's sole account".
#[test]
fn pre_field_bytes_read_back_as_the_sole_account() {
    assert_eq!(
        serde_json::from_str::<Event>(PRE_FIELD_FUNDING).expect("deserialize"),
        unstamped_funding()
    );
    assert_eq!(
        serde_json::from_str::<Event>(PRE_FIELD_LIQ).expect("deserialize"),
        unstamped_liquidation()
    );
}

/// …and a STAMPED one carries the key, round-trips, and is NOT equal to its unstamped twin —
/// the property `vike_core`'s `route_event` and `ExecutionEngine::on_event` both read.
#[test]
fn a_stamped_payload_carries_its_route_key_and_round_trips() {
    for (unstamped, stamped) in [
        (unstamped_funding(), {
            let mut e = unstamped_funding();
            if let Event::Funding(f) = &mut e {
                f.route_key = Some("binance#alt".into());
            }
            e
        }),
        (unstamped_liquidation(), {
            let mut e = unstamped_liquidation();
            if let Event::PositionLiquidated(p) = &mut e {
                p.route_key = Some("binance#alt".into());
            }
            e
        }),
    ] {
        let json = serde_json::to_string(&stamped).expect("serialize");
        assert!(json.contains(r#""route_key":"binance#alt""#), "got {json}");
        assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), stamped);
        assert_ne!(stamped, unstamped, "the stamp must be observable, not cosmetic");
    }
}

/// The currency a venue states for the amount (decision 0183, item 9) rides the wire under its own
/// key and round-trips; unstated, it is absent (the pins above) — so a build that predates the field
/// reads the unstated bytes unchanged, and an unstated currency is never read as any particular one.
#[test]
fn a_stated_currency_carries_its_key_and_round_trips() {
    let (funding, liquidation) = (
        Event::Funding(FundingEvent {
            ccy: Some("EUR".into()),
            ..match unstamped_funding() {
                Event::Funding(f) => f,
                _ => unreachable!("a funding event"),
            }
        }),
        Event::PositionLiquidated(PositionLiquidated {
            fee_ccy: Some("EUR".into()),
            ..match unstamped_liquidation() {
                Event::PositionLiquidated(p) => p,
                _ => unreachable!("a liquidation"),
            }
        }),
    );
    for (stated, key, unstated) in [
        (funding, r#""ccy":"EUR""#, unstamped_funding()),
        (liquidation, r#""fee_ccy":"EUR""#, unstamped_liquidation()),
    ] {
        let json = serde_json::to_string(&stated).expect("serialize");
        assert!(json.contains(key), "got {json}");
        assert_eq!(serde_json::from_str::<Event>(&json).expect("deserialize"), stated);
        assert_ne!(stated, unstated, "the currency must be observable, not cosmetic");
    }
}
