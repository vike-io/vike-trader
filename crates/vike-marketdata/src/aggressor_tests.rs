use super::*;
fn t(is_buyer_maker: bool) -> TickTrade {
    TickTrade {
        timestamp: 0,
        local_timestamp: 0,
        price: 100.0,
        size: 2.0,
        is_buyer_maker,
        symbol: Default::default(),
    }
}
#[test]
fn classify_maps_is_buyer_maker() {
    assert_eq!(classify(&t(false)), TickAggressor::Buy); // ask lifted → buy aggressor
    assert_eq!(classify(&t(true)), TickAggressor::Sell); // bid hit → sell aggressor
}
#[test]
fn signed_splits_by_side() {
    assert_eq!(signed(&t(false)), (2.0, 0.0));
    assert_eq!(signed(&t(true)), (0.0, 2.0));
}

#[test]
fn tick_rule_reads_the_move_and_carries_an_unchanged_price() {
    use TickAggressor::{Buy, Sell};
    // The first trade of a tape: no previous price, the book's starting buy.
    assert_eq!(tick_rule(100.0, f64::NAN, Buy), Buy);
    assert_eq!(tick_rule(101.0, 100.0, Sell), Buy); // uptick
    assert_eq!(tick_rule(99.0, 100.0, Buy), Sell); // downtick
    assert_eq!(tick_rule(100.0, 100.0, Sell), Sell); // unchanged: the previous side
    assert_eq!(tick_rule(100.0, 100.0, Buy), Buy);
    assert_eq!(tick_rule(f64::NAN, 100.0, Sell), Sell); // no price: the previous side
}
