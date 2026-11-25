use super::*;

#[test]
fn build_limit_buy_matches_literal() {
    let ticket = OrderEntryDispatchOrder::limit("binance", "BTCUSDT", 1, 0.5, 30_000.0);
    let req = build_order_request(&ticket, "dom-7".to_string());
    // Exactly what the inline `OrderRequest { .. }` literal produced at the DOM Place site.
    let expected = OrderRequest {
        order_client_id: "dom-7".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        order_type: OrderType::Limit,
        price: Some(30_000.0),
        trigger_price: None,
        reduce_only: false,
        ..Default::default()
    };
    assert_eq!(req, expected);
}

#[test]
fn build_stop_matches_dom_place_shape() {
    // DOM Place with stop == true: order_type "stop", price None, trigger_price Some(price).
    let ticket = OrderEntryDispatchOrder::stop("okx", "BTC-USDT", -1, 2.0, 25_000.0);
    let req = build_order_request(&ticket, "dom-9".to_string());
    assert_eq!(req.order_type, OrderType::Stop);
    assert_eq!(req.price, None);
    assert_eq!(req.trigger_price, Some(25_000.0));
    assert_eq!(req.side, -1);
    assert_eq!(req.venue, "okx");
}

#[test]
fn build_reduce_only_flag_carried() {
    let ticket = OrderEntryDispatchOrder {
        reduce_only: true,
        ..OrderEntryDispatchOrder::limit("bybit", "ETHUSDT", -1, 1.0, 2000.0)
    };
    let req = build_order_request(&ticket, "dom-1".to_string());
    assert!(req.reduce_only);
}

#[test]
fn build_options_ticket_shape() {
    // Mirrors the options confirm-ticket site: venue "deribit", limit, price Some, no trigger.
    let ticket = OrderEntryDispatchOrder::limit("deribit", "BTC-28MAR25-100000-C", 1, 3.0, 0.045);
    let req = build_order_request(&ticket, "opt-4".to_string());
    let expected = OrderRequest {
        order_client_id: "opt-4".to_string(),
        venue: "deribit".to_string(),
        symbol: "BTC-28MAR25-100000-C".to_string(),
        side: 1,
        qty: 3.0,
        order_type: OrderType::Limit,
        price: Some(0.045),
        ..Default::default()
    };
    assert_eq!(req, expected);
}

/// Every ticket constructor builds one of the kinds the form offers, and the form offers exactly
/// limit, market and stop — the three the deleted ticket-local `OrderKind` spelled.
#[test]
fn every_ticket_constructor_builds_an_offered_order_type() {
    assert_eq!(TICKET_ORDER_TYPES, &[OrderType::Limit, OrderType::Market, OrderType::Stop]);
    for ticket in [
        OrderEntryDispatchOrder::limit("binance", "BTCUSDT", 1, 1.0, 100.0),
        OrderEntryDispatchOrder::market("binance", "BTCUSDT", 1, 1.0),
        OrderEntryDispatchOrder::stop("binance", "BTCUSDT", 1, 1.0, 90.0),
    ] {
        assert!(TICKET_ORDER_TYPES.contains(&ticket.order_type), "{ticket:?}");
        let req = build_order_request(&ticket, "c".into());
        assert_eq!(req.order_type, ticket.order_type, "the request carries the ticket's kind");
    }
}
