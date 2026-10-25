use super::*;

#[test]
fn a_full_ticker_survives_a_json_round_trip() {
    let mut t = OptionTicker::new(1_700_000_000_000, "BTC-27JUL26-60000-C.deribit");
    t.bid = Some(0.05);
    t.ask = Some(0.055);
    t.bid_size = Some(10.0);
    t.ask_size = Some(12.0);
    t.last = Some(0.052);
    t.mark = Some(0.0525);
    t.mark_iv = Some(0.6633);
    t.bid_iv = Some(0.65);
    t.ask_iv = Some(0.67);
    t.delta = Some(0.45);
    t.gamma = Some(0.0001);
    t.vega = Some(55.0);
    t.theta = Some(-120.0);
    t.rho = Some(8.0);
    t.open_interest = Some(1_000.0);
    t.volume = Some(250.0);
    t.underlying_price = Some(59_100.0);
    t.index_price = Some(59_000.0);
    let text = serde_json::to_string(&t).unwrap();
    assert_eq!(serde_json::from_str::<OptionTicker>(&text).unwrap(), t);
}

#[test]
fn a_ticker_carrying_only_its_identity_still_deserialises() {
    let t: OptionTicker = serde_json::from_str(r#"{"ts":7,"symbol":"X-C.v"}"#).unwrap();
    assert_eq!(t, OptionTicker::new(7, "X-C.v"));
}

#[test]
fn a_ticker_without_its_identity_is_refused() {
    assert!(serde_json::from_str::<OptionTicker>(r#"{"ts":7,"mark":1.0}"#).is_err());
}
