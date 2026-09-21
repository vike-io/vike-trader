//! Pure parse tests for the IG `ReconClient` report parsers (ReconFactory seam) — NO network, just
//! representative JSON bodies shaped like IG's real REST responses (`GET /positions` v2, `GET
//! /workingorders` v2, `GET /history/activity` v3 `detailed=true`, `GET /accounts` v1). Asserts the
//! tricky bits the module doc calls out: per-deal position AGGREGATION into one signed net-qty row,
//! the always-`BOTH` position side (so the report matches IG's `BOTH`-keyed local state), the
//! always-`None` client-order-id, the executed-deal activity filter, and the number-or-string flex
//! parse (positions quote numbers, the activity `details` block quotes strings).

use vike_ig::recon_client::{
    ms_to_ig_datetime, parse_activity_fills, parse_balance, parse_positions, parse_working_orders,
};
use vike_model::events::{LiquiditySide, PositionSide};

// --- parse_positions -----------------------------------------------------------------------

const EPIC: &str = "CS.D.EURUSD.MINI.IP";

#[test]
fn parses_single_long_position_every_field() {
    let body = r#"{"positions":[
        {"position":{"dealId":"DIAAA1","direction":"BUY","size":2.0,"level":1.0930,
                     "createdDateUTC":"2022-01-15T14:00:00","currency":"USD"},
         "market":{"epic":"CS.D.EURUSD.MINI.IP","instrumentName":"EUR/USD"}}
    ]}"#;
    let r = parse_positions(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    let p = &r[0];
    assert_eq!(p.venue, "ig");
    assert_eq!(p.symbol, EPIC);
    assert_eq!(p.position_side, PositionSide::Both, "IG local state keys on BOTH");
    assert_eq!(p.qty, 2.0, "BUY -> positive net qty");
    assert_eq!(p.avg_px, 1.093);
    assert_eq!(p.ts, 1_642_255_200_000);
}

#[test]
fn short_position_is_negative_qty_still_both_side() {
    let body = r#"{"positions":[
        {"position":{"dealId":"D2","direction":"SELL","size":3.0,"level":1.10},
         "market":{"epic":"CS.D.EURUSD.MINI.IP"}}
    ]}"#;
    let r = parse_positions(body, EPIC).unwrap();
    assert_eq!(r[0].qty, -3.0, "SELL -> negative net qty");
    assert_eq!(r[0].position_side, PositionSide::Both);
}

#[test]
fn aggregates_multiple_deals_into_one_net_row_with_weighted_avg() {
    // IG is position-per-deal: two BUY deals on one epic net to a single 3.0 report,
    // avg_px = size-weighted mean = (1*1.10 + 2*1.15) / 3 = 1.1333...
    let body = r#"{"positions":[
        {"position":{"dealId":"D1","direction":"BUY","size":1.0,"level":1.10},
         "market":{"epic":"CS.D.EURUSD.MINI.IP"}},
        {"position":{"dealId":"D2","direction":"BUY","size":2.0,"level":1.15},
         "market":{"epic":"CS.D.EURUSD.MINI.IP"}}
    ]}"#;
    let r = parse_positions(body, EPIC).unwrap();
    assert_eq!(r.len(), 1, "one aggregated row, not two deal rows");
    assert_eq!(r[0].qty, 3.0);
    assert!((r[0].avg_px - (1.0 * 1.10 + 2.0 * 1.15) / 3.0).abs() < 1e-12);
}

#[test]
fn nets_opposing_deals() {
    // A BUY 3 and a SELL 1 on the same epic net to +2.
    let body = r#"{"positions":[
        {"position":{"direction":"BUY","size":3.0,"level":1.10},"market":{"epic":"CS.D.EURUSD.MINI.IP"}},
        {"position":{"direction":"SELL","size":1.0,"level":1.12},"market":{"epic":"CS.D.EURUSD.MINI.IP"}}
    ]}"#;
    let r = parse_positions(body, EPIC).unwrap();
    assert_eq!(r[0].qty, 2.0);
}

#[test]
fn filters_out_other_epics() {
    let body = r#"{"positions":[
        {"position":{"direction":"BUY","size":5.0,"level":1.0},"market":{"epic":"CS.D.GBPUSD.MINI.IP"}}
    ]}"#;
    // no row for the mounted epic -> synthesized flat row (never empty).
    let r = parse_positions(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, EPIC);
    assert_eq!(r[0].qty, 0.0);
    assert_eq!(r[0].position_side, PositionSide::Both);
}

#[test]
fn empty_positions_array_synthesizes_a_flat_row() {
    let r = parse_positions(r#"{"positions":[]}"#, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 0.0);
    assert_eq!(r[0].avg_px, 0.0);
    assert_eq!(r[0].ts, 0);
}

#[test]
fn positions_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_positions("not json", EPIC).is_err());
    assert!(parse_positions("{}", EPIC).is_err(), "missing `positions` array is an error");
    assert!(parse_positions("null", EPIC).is_err());
}

// --- parse_working_orders ------------------------------------------------------------------

#[test]
fn parses_limit_working_order_every_field() {
    let body = r#"{"workingOrders":[
        {"workingOrderData":{"dealId":"DIWO1","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY",
                             "orderType":"LIMIT","orderSize":1.5,"orderLevel":1.05,
                             "timeInForce":"GOOD_TILL_CANCELLED","createdDateUTC":"2022-01-15T14:00:00"},
         "marketData":{"epic":"CS.D.EURUSD.MINI.IP","instrumentName":"EUR/USD"}}
    ]}"#;
    let r = parse_working_orders(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "ig");
    assert_eq!(o.symbol, EPIC);
    assert_eq!(o.venue_order_id.as_str(), "DIWO1");
    assert_eq!(o.client_order_id, None, "IG echoes no client id");
    assert_eq!(o.side, 1, "BUY -> +1");
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.qty, 1.5);
    assert_eq!(o.filled_qty, 0.0, "a working order is unfilled by definition");
    assert_eq!(o.avg_px, 0.0);
    assert_eq!(o.status, "ACCEPTED");
    assert_eq!(o.ts, 1_642_255_200_000);
}

#[test]
fn stop_working_order_sell_side() {
    let body = r#"{"workingOrders":[
        {"workingOrderData":{"dealId":"D2","epic":"CS.D.EURUSD.MINI.IP","direction":"SELL",
                             "orderType":"STOP","orderSize":1.0}}
    ]}"#;
    let r = parse_working_orders(body, EPIC).unwrap();
    assert_eq!(r[0].order_type, "stop");
    assert_eq!(r[0].side, -1, "SELL -> -1");
}

#[test]
fn working_order_epic_falls_back_to_market_data() {
    // workingOrderData without an epic — the marketData epic scopes the row.
    let body = r#"{"workingOrders":[
        {"workingOrderData":{"dealId":"D3","direction":"BUY","orderType":"LIMIT","orderSize":1.0},
         "marketData":{"epic":"CS.D.EURUSD.MINI.IP"}}
    ]}"#;
    let r = parse_working_orders(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "D3");
}

#[test]
fn working_orders_filters_out_other_epics() {
    let body = r#"{"workingOrders":[
        {"workingOrderData":{"dealId":"a","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","orderType":"LIMIT","orderSize":1.0}},
        {"workingOrderData":{"dealId":"b","epic":"CS.D.GBPUSD.MINI.IP","direction":"BUY","orderType":"LIMIT","orderSize":1.0}}
    ]}"#;
    let r = parse_working_orders(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].venue_order_id.as_str(), "a");
}

#[test]
fn working_orders_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_working_orders("not json", EPIC).is_err());
    assert!(parse_working_orders("{}", EPIC).is_err(), "missing `workingOrders` array is an error");
}

// --- parse_activity_fills ------------------------------------------------------------------

#[test]
fn parses_executed_position_activity_as_a_fill() {
    let body = r#"{"activities":[
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"DIAAA9","type":"POSITION","status":"ACCEPTED",
         "details":{"direction":"SELL","size":"-2","level":"1.0955","dealReference":"REF9","currency":"USD"}}
    ]}"#;
    let r = parse_activity_fills(body, EPIC).unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "ig");
    assert_eq!(f.symbol, EPIC);
    assert_eq!(f.trade_id.as_str(), "DIAAA9");
    assert_eq!(f.venue_order_id.as_str(), "REF9", "prefers details.dealReference");
    assert_eq!(f.client_order_id, None);
    assert_eq!(f.side, -1, "SELL -> -1");
    assert_eq!(f.last_qty, 2.0, "abs of the signed size string");
    assert_eq!(f.last_px, 1.0955);
    assert_eq!(f.commission, 0.0);
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.ts, 1_642_255_200_000);
}

#[test]
fn activity_venue_order_id_falls_back_to_deal_id() {
    let body = r#"{"activities":[
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"DEAL7","type":"POSITION","status":"ACCEPTED",
         "details":{"direction":"BUY","size":"1","level":"1.10"}}
    ]}"#;
    let r = parse_activity_fills(body, EPIC).unwrap();
    assert_eq!(r[0].venue_order_id.as_str(), "DEAL7", "no dealReference -> dealId");
    assert_eq!(r[0].side, 1);
    assert_eq!(r[0].last_qty, 1.0);
}

#[test]
fn activity_skips_non_position_and_non_accepted_and_other_epic() {
    let body = r#"{"activities":[
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"w","type":"WORKING_ORDER","status":"ACCEPTED",
         "details":{"direction":"BUY","size":"1","level":"1.1"}},
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"r","type":"POSITION","status":"REJECTED",
         "details":{"direction":"BUY","size":"1","level":"1.1"}},
        {"date":"2022-01-15T14:00:00","epic":"CS.D.GBPUSD.MINI.IP","dealId":"g","type":"POSITION","status":"ACCEPTED",
         "details":{"direction":"BUY","size":"1","level":"1.1"}},
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"keep","type":"POSITION","status":"ACCEPTED",
         "details":{"direction":"BUY","size":"1","level":"1.1"}}
    ]}"#;
    let r = parse_activity_fills(body, EPIC).unwrap();
    assert_eq!(r.len(), 1, "only the executed POSITION on the mounted epic survives");
    assert_eq!(r[0].trade_id.as_str(), "keep");
}

#[test]
fn activity_sign_falls_back_to_size_when_direction_absent() {
    let body = r#"{"activities":[
        {"date":"2022-01-15T14:00:00","epic":"CS.D.EURUSD.MINI.IP","dealId":"d","type":"POSITION","status":"ACCEPTED",
         "details":{"size":"-4","level":"1.2"}}
    ]}"#;
    let r = parse_activity_fills(body, EPIC).unwrap();
    assert_eq!(r[0].side, -1, "no direction -> sign of the size");
    assert_eq!(r[0].last_qty, 4.0);
}

#[test]
fn activity_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_activity_fills("not json", EPIC).is_err());
    assert!(parse_activity_fills("{}", EPIC).is_err(), "missing `activities` array is an error");
}

// --- parse_balance -------------------------------------------------------------------------
// IG `accountId`s are short alphanumeric codes (the `/accounts` v1 shape — e.g. the docs' `PYZFT`),
// NOT tickers; these are IG-shaped synthetic values, never a real demo account id.

#[test]
fn parses_matching_account_balance() {
    let body = r#"{"accounts":[
        {"accountId":"Z8DPF","balance":{"balance":1000.50,"deposit":900.0,"profitLoss":100.5,"available":800.0}},
        {"accountId":"Z9K4T","balance":{"balance":42.0}}
    ]}"#;
    assert_eq!(parse_balance(body, "Z9K4T").unwrap(), Some(42.0), "picks the matching account");
    assert_eq!(parse_balance(body, "Z8DPF").unwrap(), Some(1000.50));
}

#[test]
fn balance_falls_back_to_first_account_when_id_absent() {
    let body = r#"{"accounts":[{"accountId":"Z8DPF","balance":{"balance":7.0}}]}"#;
    assert_eq!(parse_balance(body, "ZQXRW").unwrap(), Some(7.0));
}

#[test]
fn balance_missing_field_is_none() {
    let body = r#"{"accounts":[{"accountId":"Z8DPF","balance":{"deposit":1.0}}]}"#;
    assert_eq!(parse_balance(body, "Z8DPF").unwrap(), None);
    let body2 = r#"{"accounts":[{"accountId":"Z8DPF"}]}"#;
    assert_eq!(parse_balance(body2, "Z8DPF").unwrap(), None);
}

#[test]
fn balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_balance("not json", "Z8DPF").is_err());
    assert!(parse_balance("{}", "Z8DPF").is_err(), "missing `accounts` array is an error");
    assert!(parse_balance("[]", "Z8DPF").is_err());
}

// --- ms_to_ig_datetime ---------------------------------------------------------------------

#[test]
fn ms_to_ig_datetime_is_the_inverse_of_parse_ig_time_utc() {
    // 2022-01-15T14:00:00 UTC == 1_642_255_200_000 ms (the pin data.rs::time_parse_utc asserts).
    assert_eq!(ms_to_ig_datetime(1_642_255_200_000), "2022-01-15T14:00:00");
    assert_eq!(ms_to_ig_datetime(0), "1970-01-01T00:00:00");
    // round-trip through the existing parser
    let s = ms_to_ig_datetime(1_642_255_259_000);
    assert_eq!(vike_ig::parse_ig_time_utc(&s), 1_642_255_259_000);
}
