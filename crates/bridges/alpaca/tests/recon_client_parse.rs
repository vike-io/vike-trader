//! Pure parse tests for the Alpaca `ReconClient` report parsers (ReconFactory seam, wave-2 task 6)
//! — NO network, just representative JSON bodies shaped like the Broker API's real responses (per
//! `alpacahq/alpaca-docs`): `GET /v1/trading/accounts/{id}/orders`, `GET
//! /v1/accounts/activities/FILL`, `GET /v1/trading/accounts/{id}/positions`, `GET
//! /v1/trading/accounts/{id}/account`. Asserts the tricky bits the module doc calls out: the
//! wire-vs-vike symbol relabeling for crypto pairs, unsigned `qty` + `side` -> signed position
//! qty, and the flat-position synthesis when Alpaca omits a symbol entirely.

use vike_alpaca::recon_client::{parse_balance, parse_fills, parse_orders, parse_positions};
use vike_model::events::{LiquiditySide, PositionSide};

// --- parse_orders --------------------------------------------------------------------------

#[test]
fn parses_order_row_every_field_and_normalizes_status() {
    let body = r#"[
        {
            "id": "61e69015-8549-4bfd-b9c3-01e75843f47d",
            "client_order_id": "eb9e2aaf-806f-4de3-a19d-c7a3fca6862d",
            "updated_at": "2026-03-16T18:38:02.000Z",
            "symbol": "AAPL",
            "qty": "5",
            "filled_qty": "2",
            "filled_avg_price": "150.25",
            "order_type": "limit",
            "type": "limit",
            "side": "buy",
            "status": "partially_filled"
        }
    ]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "alpaca");
    assert_eq!(o.symbol, "AAPL");
    assert_eq!(o.venue_order_id.as_str(), "61e69015-8549-4bfd-b9c3-01e75843f47d");
    assert_eq!(o.client_order_id.as_deref(), Some("eb9e2aaf-806f-4de3-a19d-c7a3fca6862d"));
    assert_eq!(o.side, 1, "buy -> +1");
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.qty, 5.0);
    assert_eq!(o.filled_qty, 2.0);
    assert_eq!(o.avg_px, 150.25);
    assert_eq!(o.status, "PARTIALLY_FILLED");
    assert_eq!(o.ts, 1773686282000);
}

#[test]
fn order_row_empty_client_order_id_normalizes_to_none() {
    let body = r#"[{"id":"x","client_order_id":"","symbol":"AAPL","side":"sell","status":"new"}]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r[0].client_order_id, None);
    assert_eq!(r[0].side, -1, "sell -> -1");
    assert_eq!(r[0].status, "ACCEPTED", "new -> ACCEPTED");
}

#[test]
fn parse_orders_relabels_crypto_wire_symbol_to_the_vike_symbol() {
    // Alpaca's wire form slashes bare crypto pairs; the report must carry the UNIFIED vike symbol
    // (what local state is keyed by), not the wire spelling — see the module doc.
    let body = r#"[{"id":"x","symbol":"BTC/USD","side":"buy","qty":"0.5","status":"filled"}]"#;
    let r = parse_orders(body, "BTC/USD", "BTCUSD").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "BTCUSD", "output carries the vike spelling, not the wire slash");
}

#[test]
fn parse_orders_filters_out_other_symbols() {
    let body = r#"[
        {"id":"a","symbol":"AAPL","side":"buy","status":"new"},
        {"id":"b","symbol":"MSFT","side":"buy","status":"new"}
    ]"#;
    let r = parse_orders(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "a");
}

#[test]
fn every_documented_status_normalizes() {
    for (raw, want) in [
        ("new", "ACCEPTED"),
        ("accepted", "ACCEPTED"),
        ("pending_new", "ACCEPTED"),
        ("accepted_for_bidding", "ACCEPTED"),
        ("pending_replace", "ACCEPTED"),
        ("replaced", "ACCEPTED"),
        ("stopped", "ACCEPTED"),
        ("suspended", "ACCEPTED"),
        ("calculated", "ACCEPTED"),
        ("partially_filled", "PARTIALLY_FILLED"),
        ("filled", "FILLED"),
        ("canceled", "CANCELED"),
        ("pending_cancel", "CANCELED"),
        ("done_for_day", "CANCELED"),
        ("expired", "EXPIRED"),
        ("rejected", "REJECTED"),
    ] {
        let body = format!(r#"[{{"id":"x","symbol":"AAPL","side":"buy","status":"{raw}"}}]"#);
        let r = parse_orders(&body, "AAPL", "AAPL").unwrap();
        assert_eq!(r[0].status, want, "status {raw}");
    }
}

#[test]
fn orders_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders("not json", "AAPL", "AAPL").is_err());
    assert!(parse_orders("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
    assert!(parse_orders("null", "AAPL", "AAPL").is_err());
}

// --- parse_fills ---------------------------------------------------------------------------

#[test]
fn parses_fill_activity_every_field() {
    let body = r#"[
        {
            "id": "20210510100104650::88b5f678-fef5-447b-af15-f21e367e6d8c",
            "account_id": "c8f1ef5d-edc0-4f23-9ee4-378f19cb92a4",
            "activity_type": "FILL",
            "transaction_time": "2021-05-10T14:01:04.650Z",
            "type": "fill",
            "price": "128.33",
            "qty": "1",
            "side": "sell",
            "symbol": "AAPL",
            "order_id": "fe060a1b-5b45-4eba-ba46-c3a3345d8255",
            "order_status": "filled"
        }
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "alpaca");
    assert_eq!(f.symbol, "AAPL");
    assert_eq!(f.trade_id.as_str(), "20210510100104650::88b5f678-fef5-447b-af15-f21e367e6d8c");
    assert_eq!(f.venue_order_id.as_str(), "fe060a1b-5b45-4eba-ba46-c3a3345d8255");
    assert_eq!(f.client_order_id, None, "activities carry no client-order-id");
    assert_eq!(f.side, -1, "sell -> -1");
    assert_eq!(f.last_qty, 1.0);
    assert_eq!(f.last_px, 128.33);
    assert_eq!(f.commission, 0.0, "no fee field on this activity shape");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.ts, 1620655264650);
}

#[test]
fn parse_fills_ignores_non_fill_activity_types() {
    let body = r#"[
        {"id":"a","activity_type":"CSD","symbol":"AAPL"},
        {"id":"b","activity_type":"FILL","symbol":"AAPL","side":"buy","qty":"1","price":"1.0","order_id":"o1"}
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "b");
}

#[test]
fn parse_fills_filters_out_other_symbols() {
    let body = r#"[
        {"id":"a","activity_type":"FILL","symbol":"AAPL","side":"buy"},
        {"id":"b","activity_type":"FILL","symbol":"MSFT","side":"buy"}
    ]"#;
    let r = parse_fills(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "a");
}

#[test]
fn fills_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_fills("not json", "AAPL", "AAPL").is_err());
    assert!(parse_fills("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
}

// --- parse_positions -----------------------------------------------------------------------

#[test]
fn parses_long_position_row() {
    let body = r#"[
        {
            "asset_id": "904837e3-3b76-47ec-b432-046db621571b",
            "symbol": "AAPL",
            "avg_entry_price": "100.0",
            "qty": "5",
            "side": "long"
        }
    ]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    let p = &r[0];
    assert_eq!(p.symbol, "AAPL");
    assert_eq!(p.position_side, PositionSide::Long);
    assert_eq!(p.qty, 5.0);
    assert_eq!(p.avg_px, 100.0);
}

#[test]
fn parses_short_position_row_as_negative_qty() {
    let body = r#"[{"symbol":"AAPL","avg_entry_price":"50.0","qty":"3","side":"short"}]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r[0].position_side, PositionSide::Short);
    assert_eq!(r[0].qty, -3.0, "unsigned wire qty + side=short -> negative");
}

#[test]
fn absent_symbol_synthesizes_a_flat_row() {
    // Alpaca omits a symbol entirely once flat — no row at all, unlike a `qty: "0"` row.
    let body = r#"[{"symbol":"MSFT","avg_entry_price":"1.0","qty":"1","side":"long"}]"#;
    let r = parse_positions(body, "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "AAPL");
    assert_eq!(r[0].position_side, PositionSide::Both);
    assert_eq!(r[0].qty, 0.0);
}

#[test]
fn empty_positions_array_synthesizes_a_flat_row() {
    let r = parse_positions("[]", "AAPL", "AAPL").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 0.0);
}

#[test]
fn parse_positions_relabels_crypto_wire_symbol_to_the_vike_symbol() {
    let body = r#"[{"symbol":"BTC/USD","avg_entry_price":"20000.0","qty":"0.1","side":"long"}]"#;
    let r = parse_positions(body, "BTC/USD", "BTCUSD").unwrap();
    assert_eq!(r[0].symbol, "BTCUSD");
}

#[test]
fn positions_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_positions("not json", "AAPL", "AAPL").is_err());
    assert!(parse_positions("{}", "AAPL", "AAPL").is_err(), "an object, not an array, is an error");
}

// --- parse_balance ---------------------------------------------------------------------------

#[test]
fn parses_account_cash() {
    let body = r#"{"cash":"-23140.2","equity":"103820.56","buying_power":"262113.632"}"#;
    assert_eq!(parse_balance(body).unwrap(), Some(-23140.2));
}

#[test]
fn balance_missing_cash_field_is_none() {
    assert_eq!(parse_balance(r#"{"equity":"100.0"}"#).unwrap(), None);
}

#[test]
fn balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_balance("not json").is_err());
    assert!(parse_balance("[]").is_err(), "an array, not an object, is an error");
    assert!(parse_balance("null").is_err());
}

// --- the `recon_client` factory — no network, just proves the wiring is infallible ------------

#[test]
fn recon_client_factory_builds_for_any_configured_symbol() {
    let config = vike_alpaca::AlpacaConfig {
        client_id: "cid".to_string(),
        client_secret: "csecret".to_string(),
        account_id: "acct-1".to_string(),
        env: vike_bridge_core::Environment::Demo,
        hosts: vike_alpaca::hosts_for(vike_bridge_core::Environment::Demo),
    };
    assert!(
        vike_alpaca::recon_client(&config, "AAPL").is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}
