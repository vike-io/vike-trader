use super::*;

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[test]
fn every_feed_value_survives_a_json_round_trip() {
    let mark = MarkPrice {
        ts: 1,
        symbol: "BTC-PERP.deribit".into(),
        mark: 60_000.5,
        index: Some(60_001.0),
    };
    assert_eq!(round_trip(&mark), mark);

    let index = IndexPrice { ts: 2, name: "btc_usd".into(), price: 60_000.0 };
    assert_eq!(round_trip(&index), index);

    let funding = FundingRate {
        ts: 3,
        symbol: "BTCUSDT.binance".into(),
        rate: 0.0001,
        interval_ms: Some(28_800_000),
        next_ts: Some(1_700_000_000_000),
        predicted: Some(-0.00005),
    };
    assert_eq!(round_trip(&funding), funding);

    let oi = OpenInterest {
        ts: 4,
        symbol: "ETHUSDT.bybit".into(),
        contracts: 12.5,
        notional: Some(40_000.0),
    };
    assert_eq!(round_trip(&oi), oi);

    let liq = Liquidation {
        ts: 5,
        symbol: "BTCUSDT.binance".into(),
        side: -1,
        price: 59_000.0,
        qty: 0.25,
    };
    assert_eq!(round_trip(&liq), liq);

    let vol = VolIndexTick { ts: 6, name: "btc_usd".into(), value: 52.3 };
    assert_eq!(round_trip(&vol), vol);
}

#[test]
fn a_payload_without_the_optional_fields_still_deserialises() {
    let mark: MarkPrice = serde_json::from_str(r#"{"ts":1,"symbol":"X.v","mark":2.0}"#).unwrap();
    assert_eq!(mark.index, None);

    let funding: FundingRate =
        serde_json::from_str(r#"{"ts":1,"symbol":"X.v","rate":0.0002}"#).unwrap();
    assert_eq!((funding.interval_ms, funding.next_ts, funding.predicted), (None, None, None));
    assert_eq!(funding.rate, 0.0002);

    let oi: OpenInterest =
        serde_json::from_str(r#"{"ts":1,"symbol":"X.v","contracts":3.0}"#).unwrap();
    assert_eq!(oi.notional, None);
}

#[test]
fn a_required_field_is_still_required() {
    // `#[serde(default)]` is on the OPTIONAL fields only: a mark with no mark is refused, not 0.0.
    assert!(serde_json::from_str::<MarkPrice>(r#"{"ts":1,"symbol":"X.v"}"#).is_err());
    assert!(
        serde_json::from_str::<Liquidation>(r#"{"ts":1,"symbol":"X.v","price":1.0,"qty":1.0}"#)
            .is_err()
    );
}
