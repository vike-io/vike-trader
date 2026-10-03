//! Pure parse tests for the OANDA `ReconClient` report parsers (recon-breadth → OANDA) — NO
//! network: feeds synthetic v20 JSON bodies (built with `serde_json::json!`, the SAME UNIX-format
//! string-scalar shape a live account returns) straight to `vike_oanda::recon_client`'s pure
//! `parse_*` functions. Covers the tricky bits the module doc calls out: signed-`units` → side/qty,
//! the instrument client-side filter + canonical-symbol stamping, net long/short position folding,
//! `ORDER_FILL`-only fill filtering with the `orderID`/`clientExtensions.id` lift, order-state
//! normalization, and every empty/absent/partial edge.

use serde_json::json;
use vike_model::events::{LiquiditySide, PositionSide};
use vike_oanda::recon_client::{
    normalize_order_state, parse_balance, parse_fill_reports, parse_order_reports,
    parse_position_reports,
};

const INSTR: &str = "EUR_USD";
const SYMBOL: &str = "EURUSD";

// --- parse_order_reports -------------------------------------------------------------------------

#[test]
fn parses_resting_limit_order_every_field() {
    let body = json!({
        "orders": [{
            "id": "6375",
            "createTime": "1478024434.937173861",
            "type": "LIMIT",
            "instrument": "EUR_USD",
            "units": "100",
            "price": "1.07000",
            "timeInForce": "GTC",
            "state": "PENDING",
            "clientExtensions": { "id": "my_order_100" }
        }],
        "lastTransactionID": "6375"
    });
    let r = parse_order_reports(&body, INSTR, SYMBOL);
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "oanda");
    assert_eq!(o.symbol, SYMBOL, "the CANONICAL vike symbol is stamped, not the OANDA form");
    assert_eq!(o.venue_order_id.as_str(), "6375");
    assert_eq!(o.client_order_id.as_deref(), Some("my_order_100"));
    assert_eq!(o.side, 1, "positive units → buy");
    assert_eq!(o.order_type, "limit", "OANDA `type` lower-cased");
    assert_eq!(o.qty, 100.0);
    assert_eq!(o.filled_qty, 0.0, "a resting order carries no fill progress");
    assert_eq!(o.avg_px, 0.0);
    assert_eq!(o.status, "ACCEPTED", "PENDING → ACCEPTED");
    assert_eq!(o.ts, 1_478_024_434_937);
}

#[test]
fn sell_order_reads_negative_units_and_absent_client_id_is_none() {
    let body = json!({
        "orders": [{
            "id": "7", "createTime": "1.0", "type": "STOP", "instrument": "EUR_USD",
            "units": "-2500", "state": "PENDING"
        }]
    });
    let r = parse_order_reports(&body, INSTR, SYMBOL);
    assert_eq!(r[0].side, -1, "negative units → sell");
    assert_eq!(r[0].qty, 2500.0, "qty is the magnitude");
    assert_eq!(r[0].order_type, "stop");
    assert_eq!(r[0].client_order_id, None, "no clientExtensions → None (externally placed)");
}

#[test]
fn order_filter_drops_other_instruments_and_instrumentless_exit_orders() {
    let body = json!({
        "orders": [
            { "id": "1", "type": "LIMIT", "instrument": "GBP_USD", "units": "1", "state": "PENDING" },
            // an exit order (TAKE_PROFIT) references a tradeID, carries NO instrument → dropped
            { "id": "2", "type": "TAKE_PROFIT", "tradeID": "55", "price": "1.2", "state": "PENDING" },
            { "id": "3", "type": "LIMIT", "instrument": "EUR_USD", "units": "1", "state": "PENDING" }
        ]
    });
    let r = parse_order_reports(&body, INSTR, SYMBOL);
    assert_eq!(r.len(), 1, "only the EUR_USD entry order survives");
    assert_eq!(r[0].venue_order_id.as_str(), "3");
}

#[test]
fn empty_and_absent_order_bodies_yield_no_rows() {
    assert!(parse_order_reports(&json!({ "orders": [] }), INSTR, SYMBOL).is_empty());
    assert!(parse_order_reports(&json!({}), INSTR, SYMBOL).is_empty(), "absent `orders` key");
    assert!(parse_order_reports(&json!({ "orders": "nope" }), INSTR, SYMBOL).is_empty());
}

#[test]
fn every_order_state_normalizes() {
    for (state, want) in [
        ("PENDING", "ACCEPTED"),
        ("TRIGGERED", "ACCEPTED"),
        ("FILLED", "FILLED"),
        ("CANCELLED", "CANCELED"),
    ] {
        assert_eq!(normalize_order_state(state), want, "{state}");
        let body = json!({ "orders": [{ "id": "1", "instrument": "EUR_USD", "units": "1", "type": "LIMIT", "state": state }] });
        assert_eq!(parse_order_reports(&body, INSTR, SYMBOL)[0].status, want);
    }
}

// --- parse_position_reports ----------------------------------------------------------------------

#[test]
fn parses_net_long_position() {
    let body = json!({
        "positions": [{
            "instrument": "EUR_USD",
            "long":  { "units": "150", "averagePrice": "1.10000" },
            "short": { "units": "0",   "averagePrice": "0" }
        }]
    });
    let r = parse_position_reports(&body, INSTR, SYMBOL);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue, "oanda");
    assert_eq!(r[0].symbol, SYMBOL);
    assert_eq!(r[0].position_side, PositionSide::Both, "OANDA is a net/one-way account");
    assert_eq!(r[0].qty, 150.0);
    assert_eq!(r[0].avg_px, 1.10000, "net long → long leg's averagePrice");
    assert_eq!(r[0].ts, 0);
}

#[test]
fn parses_net_short_position_as_negative_qty() {
    let body = json!({
        "positions": [{
            "instrument": "EUR_USD",
            "long":  { "units": "0", "averagePrice": "0" },
            "short": { "units": "-300", "averagePrice": "1.09500" }
        }]
    });
    let r = parse_position_reports(&body, INSTR, SYMBOL);
    assert_eq!(r[0].qty, -300.0, "short.units is already signed negative");
    assert_eq!(r[0].avg_px, 1.09500, "net short → short leg's averagePrice");
}

#[test]
fn a_flat_listed_position_folds_to_net_zero() {
    // OANDA `/positions` lists a closed instrument with both legs at 0 — it must fold to a flat row
    // (present, qty 0), which the driver treats identically to the synthesized-flat case.
    let body = json!({
        "positions": [{
            "instrument": "EUR_USD",
            "long":  { "units": "0", "averagePrice": "0" },
            "short": { "units": "0", "averagePrice": "0" }
        }]
    });
    let r = parse_position_reports(&body, INSTR, SYMBOL);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 0.0);
    assert_eq!(r[0].avg_px, 0.0);
}

#[test]
fn position_filter_drops_other_instruments_and_empty_bodies_yield_nothing() {
    let body = json!({
        "positions": [{ "instrument": "USD_JPY", "long": { "units": "1" }, "short": { "units": "0" } }]
    });
    assert!(
        parse_position_reports(&body, INSTR, SYMBOL).is_empty(),
        "a different instrument is filtered out (the client then synthesizes the flat row)"
    );
    assert!(parse_position_reports(&json!({ "positions": [] }), INSTR, SYMBOL).is_empty());
    assert!(parse_position_reports(&json!({}), INSTR, SYMBOL).is_empty());
}

// --- parse_fill_reports --------------------------------------------------------------------------

#[test]
fn parses_order_fill_transaction_every_field() {
    let body = json!({
        "transactions": [{
            "type": "ORDER_FILL",
            "id": "6373",
            "time": "1478012400.000000000",
            "orderID": "6372",
            "instrument": "EUR_USD",
            "units": "1000",
            "price": "1.09000",
            "commission": "0.04",
            "clientExtensions": { "id": "coid-9" }
        }],
        "lastTransactionID": "6373"
    });
    let r = parse_fill_reports(&body, INSTR, SYMBOL, "USD");
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "oanda");
    assert_eq!(f.symbol, SYMBOL);
    assert_eq!(f.trade_id.as_str(), "6373", "the fill transaction id is the trade id");
    assert_eq!(f.venue_order_id.as_str(), "6372", "orderID → venue_order_id");
    assert_eq!(f.client_order_id.as_deref(), Some("coid-9"));
    assert_eq!(f.side, 1);
    assert_eq!(f.last_qty, 1000.0);
    assert_eq!(f.last_px, 1.09);
    assert_eq!(f.commission, 0.04);
    assert_eq!(f.commission_asset, "USD", "stamped with the account home currency");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.ts, 1_478_012_400_000);
}

#[test]
fn fill_reads_negative_units_as_sell_and_missing_commission_as_zero() {
    let body = json!({
        "transactions": [{
            "type": "ORDER_FILL", "id": "9", "time": "2.0", "orderID": "8",
            "instrument": "EUR_USD", "units": "-500", "price": "1.10"
        }]
    });
    let r = parse_fill_reports(&body, INSTR, SYMBOL, "EUR");
    assert_eq!(r[0].side, -1);
    assert_eq!(r[0].last_qty, 500.0);
    assert_eq!(r[0].commission, 0.0, "absent commission → 0.0");
    assert_eq!(r[0].client_order_id, None);
    assert_eq!(r[0].commission_asset, "EUR");
}

#[test]
fn fill_filter_keeps_only_order_fills_for_this_instrument() {
    let body = json!({
        "transactions": [
            { "type": "ORDER_FILL", "id": "1", "orderID": "a", "instrument": "GBP_USD", "units": "1", "price": "1" },
            { "type": "MARKET_ORDER", "id": "2", "instrument": "EUR_USD" },       // not a fill
            { "type": "ORDER_CANCEL", "id": "3", "instrument": "EUR_USD" },       // not a fill
            { "type": "ORDER_FILL", "id": "4", "orderID": "b", "instrument": "EUR_USD", "units": "2", "price": "1.1" }
        ],
        "lastTransactionID": "4"
    });
    let r = parse_fill_reports(&body, INSTR, SYMBOL, "USD");
    assert_eq!(r.len(), 1, "only the EUR_USD ORDER_FILL survives");
    assert_eq!(r[0].trade_id.as_str(), "4");
    assert_eq!(r[0].venue_order_id.as_str(), "b");
}

#[test]
fn empty_and_absent_transaction_bodies_yield_no_fills() {
    assert!(parse_fill_reports(&json!({ "transactions": [] }), INSTR, SYMBOL, "USD").is_empty());
    assert!(parse_fill_reports(&json!({}), INSTR, SYMBOL, "USD").is_empty());
}

// --- parse_balance -------------------------------------------------------------------------------

#[test]
fn parses_summary_balance_and_handles_absence() {
    let body = json!({
        "account": { "id": "101-004-1-001", "balance": "99873.4200", "currency": "USD", "lastTransactionID": "6373" },
        "lastTransactionID": "6373"
    });
    assert_eq!(parse_balance(&body), Some(99873.42));
    assert_eq!(parse_balance(&json!({ "account": {} })), None, "no balance field → None");
    assert_eq!(parse_balance(&json!({})), None, "no account object → None");
}
