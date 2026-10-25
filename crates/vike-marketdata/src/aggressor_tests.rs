use super::*;
fn t(is_buyer_maker: bool) -> TradeTick {
    TradeTick { ts: 0, local_ts: 0, price: 100.0, size: 2.0, is_buyer_maker, symbol: String::new() }
}
#[test]
fn classify_maps_is_buyer_maker() {
    assert_eq!(classify(&t(false)), Side::Buy); // ask lifted → buy aggressor
    assert_eq!(classify(&t(true)), Side::Sell); // bid hit → sell aggressor
}
#[test]
fn signed_splits_by_side() {
    assert_eq!(signed(&t(false)), (2.0, 0.0));
    assert_eq!(signed(&t(true)), (0.0, 2.0));
}
