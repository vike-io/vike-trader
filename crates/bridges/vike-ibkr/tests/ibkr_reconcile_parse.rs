//! Pure parse tests for the IBKR `ReconClient` report parsers (recon breadth) — NO network: feeds
//! synthetic cpapi JSON bodies straight to `vike_ibkr::recon::cpapi`'s pure `parse_*` functions and
//! asserts the tricky bits the module doc calls out: conId filtering + canonical-symbol stamping,
//! the two side spellings (`BUY`/`SELL` on orders vs `B`/`S` on trades), the fill-progress-derived
//! `PARTIALLY_FILLED`, the signed net-position qty, the ledger balance rule (BASE only when it IS
//! the currency), the
//! account scoping of order and fill rows, the session-account check and the positions paging.
//!
//! The whole file is behind `ibkr-cpapi` (the feature that compiles the recon client + its cpapi
//! transport); the CI lane `cargo test -p vike-ibkr --features ibkr` compiles it (ibkr ⊇ ibkr-cpapi).
#![cfg(feature = "ibkr-cpapi")]

use vike_ibkr::recon::cpapi::{
    collect_position_pages, normalize_order_status, parse_balance, parse_fill_reports,
    parse_order_reports, parse_position_reports, session_holds_account,
};
use vike_model::events::{LiquiditySide, PositionSide};

const CONID: i64 = 265598; // AAPL
const SYMBOL: &str = "AAPL.SMART.USD";
const ACCOUNT: &str = "DU1234567";

// --- parse_order_reports ------------------------------------------------------------------------

#[test]
fn parses_resting_limit_order_every_field() {
    let body = r#"{"orders":[{
        "conid": 265598, "orderId": 1794719002, "ticker": "AAPL", "secType": "STK",
        "order_ref": "vt-coid-1", "side": "BUY", "orderType": "Limit",
        "totalSize": 100, "filledQuantity": 0, "avgPrice": "0",
        "status": "Submitted", "lastExecutionTime_r": 1647538610000, "price": 150.0
    }]}"#;
    let r = parse_order_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "ibkr");
    assert_eq!(o.symbol, SYMBOL, "stamped canonical symbol, not the raw ticker");
    assert_eq!(o.venue_order_id.as_str(), "1794719002");
    assert_eq!(o.client_order_id.as_deref(), Some("vt-coid-1"), "order_ref → client_order_id");
    assert_eq!(o.side, 1, "BUY → +1");
    assert_eq!(o.order_type, vike_model::OrderType::Limit, "lower-cased");
    assert_eq!(o.qty, 100.0);
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.avg_px, 0.0, "string \"0\" avgPrice parses to 0.0, no divide-by-zero");
    assert_eq!(o.status, vike_model::OrderStatus::Accepted, "Submitted → ACCEPTED");
    assert_eq!(o.ts, 1647538610000);
}

/// The reconcile parser binds the coid from the LIVE row's `order_ref` (paper, 2026-10-17; the row
/// has no `cOID`, no `totalSize` and no `status` key, so only the ids are asserted here).
#[test]
fn live_open_order_row_binds_client_order_id_from_order_ref() {
    let body = r#"{"orders":[{
        "acct": "DUQ186573", "conid": 265598, "orderId": 1911375756, "order_ref": "n066ref171246a",
        "side": "BUY", "orderType": "Limit", "sizeAndFills": "0/1", "remainingQuantity": 1.0,
        "filledQuantity": 0.0, "order_ccp_status": "PreSubmitted"
    }]}"#;
    let o = &parse_order_reports(body, "DUQ186573", CONID, SYMBOL).unwrap()[0];
    assert_eq!(o.client_order_id.as_deref(), Some("n066ref171246a"));
    assert_eq!(o.venue_order_id.as_str(), "1911375756");
}

#[test]
fn order_partial_progress_derives_partially_filled() {
    let body = r#"{"orders":[{
        "conid": 265598, "orderId": 1, "side": "SELL", "orderType": "Market",
        "totalSize": 10, "filledQuantity": 4, "avgPrice": 190.5, "status": "Submitted"
    }]}"#;
    let o = &parse_order_reports(body, ACCOUNT, CONID, SYMBOL).unwrap()[0];
    assert_eq!(o.status, vike_model::OrderStatus::PartiallyFilled);
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
    let r = parse_order_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "only the AAPL conId row survives");
    assert_eq!(r[0].venue_order_id.as_str(), "2");
}

#[test]
fn order_absent_client_id_normalizes_to_none() {
    let body = r#"{"orders":[{"conid": 265598, "orderId": 9, "side": "BUY", "orderType": "Limit", "totalSize": 1, "status": "Submitted"}]}"#;
    assert_eq!(parse_order_reports(body, ACCOUNT, CONID, SYMBOL).unwrap()[0].client_order_id, None);
}

#[test]
fn order_string_conid_and_string_orderid_round_trip() {
    // Some gateway builds string-encode ids — both must still filter/round-trip (decode_conid needed
    // exactly this tolerance live).
    let body = r#"{"orders":[{"conid": "265598", "orderId": "42", "side": "BUY", "orderType": "Limit", "totalSize": 1, "status": "Submitted"}]}"#;
    let r = parse_order_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "42");
}

#[test]
fn empty_orders_array_is_ok_empty() {
    assert!(parse_order_reports(r#"{"orders":[]}"#, ACCOUNT, CONID, SYMBOL).unwrap().is_empty());
}

#[test]
fn order_missing_orders_key_is_err_not_panic() {
    assert!(parse_order_reports(r#"{"snapshot":true}"#, ACCOUNT, CONID, SYMBOL).is_err());
    assert!(parse_order_reports("not json", ACCOUNT, CONID, SYMBOL).is_err());
}

// --- parse_fill_reports -------------------------------------------------------------------------

#[test]
fn parses_trade_row_single_letter_side_and_commission_fallback() {
    let body = r#"[{
        "execution_id": "0000e1a7.deadbeef", "conid": 265598, "ticker": "AAPL",
        "side": "S", "size": 50, "price": "191.25", "trade_time_r": 1647538610000,
        "order_ref": "vt-coid-2"
    }]"#;
    let r = parse_fill_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
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
    let f = &parse_fill_reports(body, ACCOUNT, CONID, SYMBOL).unwrap()[0];
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
    let r = parse_fill_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
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
    let r = parse_fill_reports(body, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "the absent-id and empty-id rows are both skipped");
    assert_eq!(r[0].trade_id, "e3", "only the identifiable fill is reported");
}

#[test]
fn trades_non_array_is_err() {
    assert!(parse_fill_reports(r#"{"error":"x"}"#, ACCOUNT, CONID, SYMBOL).is_err());
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

/// The BASE block (a converted total) is read ONLY when it declares the contract's currency as the
/// base; a BASE block for another currency, or one that names no code, is NO seed.
#[test]
fn balance_falls_back_to_base_only_when_base_is_that_currency() {
    let declared_usd = r#"{"BASE":{"cashbalance": 5000.0, "currency": "USD"}}"#;
    assert_eq!(parse_balance(declared_usd, "USD").unwrap(), Some(5000.0), "base IS USD");
    assert_eq!(parse_balance(declared_usd, "EUR").unwrap(), None, "base is not EUR → no seed");
    let undeclared = r#"{"BASE":{"cashbalance": 5000.0, "currency": "BASE"}}"#;
    assert_eq!(parse_balance(undeclared, "USD").unwrap(), None, "a base that names no code");
    let bare = r#"{"BASE":{"cashbalance": 5000.0}}"#;
    assert_eq!(parse_balance(bare, "EUR").unwrap(), None, "the old blind fallback is gone");
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
        ("Filled", vike_model::OrderStatus::Filled),
        ("Cancelled", vike_model::OrderStatus::Canceled),
        ("ApiCancelled", vike_model::OrderStatus::Canceled),
        ("PendingCancel", vike_model::OrderStatus::PendingCancel),
        ("PreSubmitted", vike_model::OrderStatus::Accepted),
        ("Submitted", vike_model::OrderStatus::Accepted),
        ("PendingSubmit", vike_model::OrderStatus::Accepted),
        ("Inactive", vike_model::OrderStatus::Rejected),
        ("Rejected", vike_model::OrderStatus::Rejected),
        // a word IB does not document is Rejected (warned), as the exec path reads it
        ("WarnState", vike_model::OrderStatus::Rejected),
    ] {
        assert_eq!(normalize_order_status(raw, 0.0, 10.0), want, "{raw}");
    }
}

// --- account scoping, the session account, and positions paging ---------------------------------

/// The account-FREE `/orders` and `/trades` paths answer for every account the session holds; a
/// row naming ANOTHER account is not this account's, a row naming none is kept.
#[test]
fn order_and_fill_rows_naming_another_account_are_dropped() {
    let orders = r#"{"orders":[
        {"conid":265598,"orderId":1,"acct":"DU1234567","side":"BUY","totalSize":1,"status":"Submitted"},
        {"conid":265598,"orderId":2,"acct":"DU7654321","side":"BUY","totalSize":1,"status":"Submitted"},
        {"conid":265598,"orderId":3,"side":"BUY","totalSize":1,"status":"Submitted"}
    ]}"#;
    let ids: Vec<String> = parse_order_reports(orders, ACCOUNT, CONID, SYMBOL)
        .unwrap()
        .iter()
        .map(|o| o.venue_order_id.to_string())
        .collect();
    assert_eq!(ids, ["1", "3"]);
    let trades = r#"[
        {"conid":265598,"execution_id":"e1","account":"DU1234567","side":"B","size":1,"price":1},
        {"conid":265598,"execution_id":"e2","account":"DU7654321","side":"B","size":1,"price":1},
        {"conid":265598,"execution_id":"e3","accountCode":"DU7654321","side":"B","size":1,"price":1}
    ]"#;
    let fills = parse_fill_reports(trades, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].trade_id.as_str(), "e1");
}

/// A session that does not hold the configured account is refused; one that does passes; an
/// answer with no `accounts` array is refused too.
#[test]
fn the_session_must_hold_the_configured_account() {
    let v = serde_json::json!({"accounts": ["DU1234567", "DU7654321"], "isPaper": true});
    assert!(session_holds_account(&v, ACCOUNT).is_ok());
    let err = session_holds_account(&v, "DU0000000").unwrap_err();
    assert!(!err.contains("DU1234567"), "the refusal names no account number: {err}");
    assert!(session_holds_account(&serde_json::json!({}), ACCOUNT).is_err());
}

/// 30 rows of OTHER contracts on page 0 (a full page), the symbol on page 1: it is FOUND, never
/// reported flat because page 0 was full.
#[test]
fn a_position_on_page_one_is_found() {
    let other: Vec<String> =
        (0..30).map(|i| format!(r#"{{"conid": {}, "position": 1}}"#, 1000 + i)).collect();
    let page0 = format!("[{}]", other.join(","));
    let page1 = r#"[{"conid": 265598, "position": -7, "avgPrice": 150.0}]"#.to_string();
    let mut asked = Vec::new();
    let r = collect_position_pages(
        |page| {
            asked.push(page);
            Ok(match page {
                0 => page0.clone(),
                1 => page1.clone(),
                _ => panic!("paged past the page that held the symbol"),
            })
        },
        CONID,
        SYMBOL,
    )
    .unwrap();
    assert_eq!(asked, [0, 1]);
    assert_eq!((r.len(), r[0].qty), (1, -7.0));
}

/// A short page ends the read: an absent symbol is then the flat row. A page that fails, after a
/// full one, fails the read (never flat).
#[test]
fn a_short_page_ends_the_read_and_a_failed_page_is_an_error() {
    let flat = collect_position_pages(|_| Ok("[]".to_string()), CONID, SYMBOL).unwrap();
    assert_eq!(flat, vec![vike_model::PositionStatusReport::flat("ibkr", SYMBOL)]);
    let full: Vec<String> =
        (0..30).map(|i| format!(r#"{{"conid": {}, "position": 1}}"#, 1000 + i)).collect();
    let full = format!("[{}]", full.join(","));
    let failed = collect_position_pages(
        |page| if page == 0 { Ok(full.clone()) } else { Err("HTTP 500".to_string()) },
        CONID,
        SYMBOL,
    );
    assert!(failed.unwrap_err().contains("page 1"));
    let endless = collect_position_pages(|_| Ok(full.clone()), CONID, SYMBOL);
    assert!(endless.is_err(), "a gateway that never serves a short page has no answer");
}
