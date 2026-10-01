//! Pure parse tests for the IBKR `ReconClient` report parsers (recon breadth) — NO network: feeds
//! synthetic cpapi JSON bodies straight to `vike_ibkr::recon_client`'s pure `parse_*` functions and
//! asserts the tricky bits the module doc calls out: conId filtering + canonical-symbol stamping,
//! the two side spellings (`BUY`/`SELL` on orders vs `B`/`S` on trades), the fill-progress-derived
//! `PARTIALLY_FILLED`, the signed net-position qty, and the ledger balance currency fallback.
//!
//! The whole file is behind `ibkr-cpapi` (the feature that compiles the recon client + its cpapi
//! transport); the CI lane `cargo test -p vike-ibkr --features ibkr` compiles it (ibkr ⊇ ibkr-cpapi).
#![cfg(feature = "ibkr-cpapi")]

use vike_ibkr::recon_client::{
    normalize_order_status, parse_balance, parse_fill_reports, parse_order_reports,
    parse_position_reports,
};
use vike_model::events::{LiquiditySide, PositionSide};

const CONID: i64 = 265598; // AAPL
const SYMBOL: &str = "AAPL.SMART.USD";

// --- parse_order_reports ------------------------------------------------------------------------

#[test]
fn parses_resting_limit_order_every_field() {
    let body = r#"{"orders":[{
        "conid": 265598, "orderId": 1794719002, "ticker": "AAPL", "secType": "STK",
        "order_ref": "vt-coid-1", "side": "BUY", "orderType": "Limit",
        "totalSize": 100, "filledQuantity": 0, "avgPrice": "0",
        "status": "Submitted", "lastExecutionTime_r": 1647538610000, "price": 150.0
    }]}"#;
    let r = parse_order_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "ibkr");
    assert_eq!(o.symbol, SYMBOL, "stamped canonical symbol, not the raw ticker");
    assert_eq!(o.venue_order_id.as_str(), "1794719002");
    assert_eq!(o.client_order_id.as_deref(), Some("vt-coid-1"), "order_ref → client_order_id");
    assert_eq!(o.side, 1, "BUY → +1");
    assert_eq!(o.order_type, "limit", "lower-cased");
    assert_eq!(o.qty, 100.0);
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.avg_px, 0.0, "string \"0\" avgPrice parses to 0.0, no divide-by-zero");
    assert_eq!(o.status, "ACCEPTED", "Submitted → ACCEPTED");
    assert_eq!(o.ts, 1647538610000);
}

#[test]
fn order_partial_progress_derives_partially_filled() {
    let body = r#"{"orders":[{
        "conid": 265598, "orderId": 1, "side": "SELL", "orderType": "Market",
        "totalSize": 10, "filledQuantity": 4, "avgPrice": 190.5, "status": "Submitted"
    }]}"#;
    let o = &parse_order_reports(body, CONID, SYMBOL).unwrap()[0];
    assert_eq!(o.status, "PARTIALLY_FILLED");
    assert_eq!(o.side, -1, "SELL → -1");
    assert_eq!(o.filled_qty, 4.0);
    assert_eq!(o.avg_px, 190.5);
}

#[test]
fn order_conid_filter_drops_other_symbols() {
    let body = r#"{"orders":[
        {"conid": 8314, "orderId": 1, "side": "BUY", "orderType": "Limit", "totalSize": 5, "status": "Submitted"},
        {"conid": 265598, "orderId": 2, "side": "BUY", "orderType": "Limit", "totalSize": 7, "status": "Submitted"}
    ]}"#;
    let r = parse_order_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "only the AAPL conId row survives");
    assert_eq!(r[0].venue_order_id.as_str(), "2");
}

#[test]
fn order_absent_client_id_normalizes_to_none() {
    let body = r#"{"orders":[{"conid": 265598, "orderId": 9, "side": "BUY", "orderType": "Limit", "totalSize": 1, "status": "Submitted"}]}"#;
    assert_eq!(parse_order_reports(body, CONID, SYMBOL).unwrap()[0].client_order_id, None);
}

#[test]
fn order_string_conid_and_string_orderid_round_trip() {
    // Some gateway builds string-encode ids — both must still filter/round-trip (decode_conid needed
    // exactly this tolerance live).
    let body = r#"{"orders":[{"conid": "265598", "orderId": "42", "side": "BUY", "orderType": "Limit", "totalSize": 1, "status": "Submitted"}]}"#;
    let r = parse_order_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "42");
}

#[test]
fn empty_orders_array_is_ok_empty() {
    assert!(parse_order_reports(r#"{"orders":[]}"#, CONID, SYMBOL).unwrap().is_empty());
}

#[test]
fn order_missing_orders_key_is_err_not_panic() {
    assert!(parse_order_reports(r#"{"snapshot":true}"#, CONID, SYMBOL).is_err());
    assert!(parse_order_reports("not json", CONID, SYMBOL).is_err());
}

// --- parse_fill_reports -------------------------------------------------------------------------

#[test]
fn parses_trade_row_single_letter_side_and_commission_fallback() {
    let body = r#"[{
        "execution_id": "0000e1a7.deadbeef", "conid": 265598, "ticker": "AAPL",
        "side": "S", "size": 50, "price": "191.25", "trade_time_r": 1647538610000,
        "order_ref": "vt-coid-2"
    }]"#;
    let r = parse_fill_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "ibkr");
    assert_eq!(f.symbol, SYMBOL);
    assert_eq!(f.trade_id.as_str(), "0000e1a7.deadbeef");
    assert_eq!(f.side, -1, "\"S\" → -1");
    assert_eq!(f.last_qty, 50.0);
    assert_eq!(f.last_px, 191.25, "string price parses");
    assert_eq!(f.commission, 0.0, "absent commission → 0.0, never a panic");
    assert_eq!(f.commission_asset, "USD", "absent commission_currency → USD fallback");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown, "cpapi /trades has no maker/taker flag");
    assert_eq!(f.client_order_id.as_deref(), Some("vt-coid-2"));
    assert_eq!(f.ts, 1647538610000);
}

#[test]
fn trade_buy_spelling_and_explicit_commission() {
    let body = r#"[{
        "execution_id": "e2", "conid": 265598, "side": "BOT", "size": "10", "price": 150.0,
        "commission": "1.05", "commission_currency": "USD", "trade_time_r": 1
    }]"#;
    let f = &parse_fill_reports(body, CONID, SYMBOL).unwrap()[0];
    assert_eq!(f.side, 1, "\"BOT\" → +1");
    assert_eq!(f.last_qty, 10.0, "string size parses");
    assert_eq!(f.commission, 1.05);
    assert_eq!(f.commission_asset, "USD");
}

#[test]
fn trade_conid_filter_drops_other_symbols() {
    let body = r#"[
        {"execution_id": "e1", "conid": 8314, "side": "B", "size": 1, "price": 1.0},
        {"execution_id": "e2", "conid": 265598, "side": "B", "size": 2, "price": 2.0}
    ]"#;
    let r = parse_fill_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trade_id.as_str(), "e2");
}

/// A trade row with no `execution_id` (absent or `""`) is SKIPPED, not reported with an empty id.
/// This gates the `TradeId::new` handling in `parse_fill_reports`: reverting it to a permissive
/// `.into()` turns the asserted 1 back into 3.
///
/// The hazard is reconcile-specific. `execution_id` is the key `recon::diff` looks up in
/// `seen_trade_ids`; an id-less report can never match, so it FABRICATES a `MissingFill` divergence,
/// and `MissingFill` is one of the two kinds `hybrid` AUTO-APPLIES — the invented divergence books
/// the fill a second time with no operator in front of it.
#[test]
fn trade_rows_without_an_execution_id_are_skipped() {
    let body = r#"[
        {"conid": 265598, "side": "B", "size": 1, "price": 1.0},
        {"execution_id": "", "conid": 265598, "side": "B", "size": 2, "price": 2.0},
        {"execution_id": "e3", "conid": 265598, "side": "B", "size": 3, "price": 3.0}
    ]"#;
    let r = parse_fill_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "the absent-id and empty-id rows are both skipped");
    assert_eq!(r[0].trade_id, "e3", "only the identifiable fill is reported");
}

#[test]
fn trades_non_array_is_err() {
    assert!(parse_fill_reports(r#"{"error":"x"}"#, CONID, SYMBOL).is_err());
}

// --- parse_position_reports ---------------------------------------------------------------------

#[test]
fn parses_long_position_signed_qty() {
    let body = r#"[{"conid": 265598, "ticker": "AAPL", "position": 100, "avgPrice": 148.5, "avgCost": 148.5}]"#;
    let r = parse_position_reports(body, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let p = &r[0];
    assert_eq!(p.venue, "ibkr");
    assert_eq!(p.symbol, SYMBOL);
    assert_eq!(p.position_side, PositionSide::Both, "IBKR is net/one-way → BOTH");
    assert_eq!(p.qty, 100.0);
    assert_eq!(p.avg_px, 148.5);
}

#[test]
fn parses_short_position_as_negative_qty() {
    let body = r#"[{"conid": 265598, "position": -30, "avgCost": 200.0}]"#;
    let p = &parse_position_reports(body, CONID, SYMBOL).unwrap()[0];
    assert_eq!(p.qty, -30.0, "the wire `position` is ALREADY signed");
    assert_eq!(p.avg_px, 200.0, "avgPrice absent → avgCost fallback");
}

#[test]
fn position_conid_filter_yields_empty_for_absent_symbol() {
    // A different symbol's position present → our conId matches nothing → empty (the client then
    // synthesizes the flat row; that synthesis is covered in the client, not this pure parser).
    let body = r#"[{"conid": 8314, "position": 5, "avgCost": 100.0}]"#;
    assert!(parse_position_reports(body, CONID, SYMBOL).unwrap().is_empty());
}

// --- parse_balance ------------------------------------------------------------------------------

#[test]
fn balance_reads_currency_cashbalance() {
    let body = r#"{"BASE":{"cashbalance": 1000000.0},"USD":{"cashbalance": 987654.32}}"#;
    assert_eq!(parse_balance(body, "USD").unwrap(), Some(987654.32));
}

#[test]
fn balance_falls_back_to_base_currency() {
    let body = r#"{"BASE":{"cashbalance": 5000.0}}"#;
    assert_eq!(parse_balance(body, "EUR").unwrap(), Some(5000.0), "no EUR row → BASE fallback");
}

#[test]
fn balance_absent_is_none_and_malformed_is_err() {
    assert_eq!(parse_balance(r#"{"GBP":{"cashbalance": 1.0}}"#, "USD").unwrap(), None);
    assert!(parse_balance("not json", "USD").is_err());
}

// --- normalize_order_status (the full status table) ---------------------------------------------

#[test]
fn status_table_normalizes_every_ib_spelling() {
    for (raw, want) in [
        ("Filled", "FILLED"),
        ("Cancelled", "CANCELED"),
        ("ApiCancelled", "CANCELED"),
        ("PendingCancel", "PENDING_CANCEL"),
        ("PreSubmitted", "ACCEPTED"),
        ("Submitted", "ACCEPTED"),
        ("PendingSubmit", "ACCEPTED"),
        ("Inactive", "REJECTED"),
        ("Rejected", "REJECTED"), // unknown-to-from_ib → Inactive → REJECTED
    ] {
        assert_eq!(normalize_order_status(raw, 0.0, 10.0), want, "{raw}");
    }
    // Every normalized status is a real OrderStatus FSM vocabulary word.
    for s in ["FILLED", "CANCELED", "PENDING_CANCEL", "ACCEPTED", "REJECTED", "PARTIALLY_FILLED"] {
        assert!(vike_exec::order::OrderStatus::parse(s).is_some(), "{s} must be FSM vocabulary");
    }
}
