//! Pure parse tests for the bybit `ReconClient` report parsers (Task 12) — NO network, just
//! representative JSON bodies shaped like Bybit V5's real `result` objects (the `{retCode,
//! retMsg, result}` envelope already unwrapped by the client — see `recon_client::unwrap_result`)
//! for `/v5/order/realtime`, `/v5/execution/list`, `/v5/position/list`. Asserts the tricky
//! mappings the brief calls out explicitly: Buy/Sell → signed qty (positions are wire-UNSIGNED,
//! `size` + `side`), hedge `positionIdx` → `position_side`, venue order-status normalization to
//! the `OrderStatus::parse` FSM vocabulary, and the maker/taker liquidity flag. Venue ids use
//! Bybit's real wire FORMAT — `orderId`/`execId` are UUIDs (per the V5 docs), `orderLinkId` is our
//! own client id — so a format quirk cannot slip past a toy `"o1"`.

use vike_bybit::recon_client::{
    parse_fee_rate, parse_fills, parse_orders, parse_positions, parse_wallet_balance,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode};

/// `/v5/account/fee-rate` `result.list[0]` -> FeeSchedule from maker/takerFeeRate (fractions).
#[test]
fn parses_fee_rate_into_fee_schedule() {
    let body =
        r#"{"list":[{"symbol":"BTCUSDT","takerFeeRate":"0.00055","makerFeeRate":"0.0002"}]}"#;
    let s = parse_fee_rate(body).unwrap().expect("fee row present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 });
}

/// An empty fee-rate list -> None (fail-soft to the static default).
#[test]
fn empty_fee_rate_list_is_none() {
    assert_eq!(parse_fee_rate(r#"{"list":[]}"#).unwrap(), None);
}

/// The exact shape from the task brief: New/PartiallyFilled/Filled/Cancelled → the FSM vocabulary,
/// an empty `orderLinkId` normalizes to `None` (externally-placed order).
#[test]
fn parses_open_order_new_status_and_empty_client_id() {
    let body = r#"{"category":"linear","list":[
        {"symbol":"BTCUSDT","orderId":"5bd7b284-89fa-4087-8306-9a17381218f3","orderLinkId":"","side":"Buy","orderType":"Limit","qty":"0.010","cumExecQty":"0.000","avgPrice":"0","orderStatus":"New","updatedTime":"1700000000000","positionIdx":0}
    ],"nextPageCursor":""}"#;
    let reports = parse_orders(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.venue_order_id.as_str(), "5bd7b284-89fa-4087-8306-9a17381218f3");
    assert_eq!(r.client_order_id, None, "empty orderLinkId normalizes to None");
    assert_eq!(r.side, 1, "Buy -> +1");
    assert_eq!(r.order_type, "limit");
    assert_eq!(r.qty, 0.01);
    assert_eq!(r.filled_qty, 0.0);
    assert_eq!(r.avg_px, 0.0);
    assert_eq!(r.status, "ACCEPTED", "New -> ACCEPTED");
    assert_eq!(r.ts, 1700000000000);
}

#[test]
fn parses_order_row_every_field_and_status_normalization() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","orderId":"cebf9f04-b25e-4f26-bbdb-55c78a28e1d8","orderLinkId":"c2","side":"Sell","orderType":"Market","qty":"0.500","cumExecQty":"0.500","avgPrice":"30010.5","orderStatus":"Filled","updatedTime":"1700000002000","positionIdx":0},
        {"symbol":"BTCUSDT","orderId":"58a42c41-f5d1-48f0-a086-0dc29ebc7ed6","orderLinkId":"c3","side":"Buy","orderType":"Limit","qty":"0.200","cumExecQty":"0.100","avgPrice":"29500.0","orderStatus":"PartiallyFilled","updatedTime":"1700000003000","positionIdx":0},
        {"symbol":"BTCUSDT","orderId":"8966f129-23bc-453d-8eec-6bbad6ce497f","orderLinkId":"c4","side":"Sell","orderType":"Limit","qty":"0.100","cumExecQty":"0.000","avgPrice":"0","orderStatus":"Cancelled","updatedTime":"1700000004000","positionIdx":0}
    ]}"#;
    let reports = parse_orders(body).unwrap();
    assert_eq!(reports.len(), 3);

    let filled = &reports[0];
    assert_eq!(filled.side, -1, "Sell -> -1");
    assert_eq!(filled.qty, 0.5);
    assert_eq!(filled.filled_qty, 0.5);
    assert_eq!(filled.avg_px, 30010.5);
    assert_eq!(filled.status, "FILLED");
    assert_eq!(filled.client_order_id.as_deref(), Some("c2"));

    let partial = &reports[1];
    assert_eq!(partial.side, 1, "Buy -> +1");
    assert_eq!(partial.status, "PARTIALLY_FILLED");

    let canceled = &reports[2];
    assert_eq!(canceled.status, "CANCELED", "Cancelled -> CANCELED");
}

#[test]
fn parses_execution_list_fill_report_every_field() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","orderId":"e5d2b9af-f7e0-4e8e-8880-679b850f4654","orderLinkId":"c9","execId":"c9f2b7e1-62b4-4164-aaff-aaff82e87d10","side":"Buy","execQty":"0.500","execPrice":"30000.0","execFee":"0.01","feeCurrency":"USDT","isMaker":true,"execTime":"1700000005000"}
    ]}"#;
    let reports = parse_fills(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.trade_id.as_str(), "c9f2b7e1-62b4-4164-aaff-aaff82e87d10");
    assert_eq!(r.venue_order_id.as_str(), "e5d2b9af-f7e0-4e8e-8880-679b850f4654");
    assert_eq!(r.client_order_id.as_deref(), Some("c9"));
    assert_eq!(r.side, 1, "Buy -> +1");
    assert_eq!(r.last_qty, 0.5);
    assert_eq!(r.last_px, 30000.0);
    assert_eq!(r.commission, 0.01);
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "isMaker:true -> Maker");
    assert_eq!(r.ts, 1700000005000);
}

#[test]
fn execution_list_sell_and_taker_flag() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","orderId":"9f8cffee-e6e0-4bac-849e-3f225659a1be","orderLinkId":"c10","execId":"f4ec95f5-c206-40bc-a5c8-7185330ad47f","side":"Sell","execQty":"0.250","execPrice":"29990.0","execFee":"0.005","feeCurrency":"USDT","isMaker":false,"execTime":"1700000006000"}
    ]}"#;
    let reports = parse_fills(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.side, -1, "Sell -> -1");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "isMaker:false -> Taker");
}

/// `/v5/position/list`: `size` is UNSIGNED — the sign comes from `side` Buy/Sell (never re-signed
/// by `positionIdx`). One-way (`positionIdx:0`) -> `PositionSide::Both`.
#[test]
fn position_list_signs_qty_from_buy_sell_side() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","markPrice":"29500.0","positionIdx":0,"tradeMode":0,"updatedTime":"1700000010000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.qty, 0.5, "Buy -> positive signed qty");
    assert_eq!(r.position_side, PositionSide::Both);
    assert_eq!(r.avg_px, 29000.0);
    assert_eq!(r.ts, 1700000010000);
    assert_eq!(r.margin_mode, MarginMode::Cross, "tradeMode 0 -> Cross");
    assert_eq!(r.isolated_margin, None, "bybit has no verified isolated-wallet field");
}

/// Margin-mode step-2: `tradeMode` 1 -> Isolated (number OR string spelling); an absent
/// `tradeMode` is the fail-safe Cross default (pre-field behavior, UTA accounts).
#[test]
fn position_list_trade_mode_maps_to_margin_mode() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","positionIdx":0,"tradeMode":1,"updatedTime":"1700000010000"},
        {"symbol":"ETHUSDT","size":"1.000","side":"Buy","avgPrice":"1800.0","positionIdx":0,"tradeMode":"1","updatedTime":"1700000011000"},
        {"symbol":"XRPUSDT","size":"10.0","side":"Sell","avgPrice":"0.5","positionIdx":0,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].margin_mode, MarginMode::Isolated, "tradeMode 1 (number) -> Isolated");
    assert_eq!(
        reports[1].margin_mode,
        MarginMode::Isolated,
        "tradeMode \"1\" (string) -> Isolated"
    );
    assert_eq!(reports[2].margin_mode, MarginMode::Cross, "absent tradeMode -> fail-safe Cross");
    assert!(reports.iter().all(|r| r.isolated_margin.is_none()));
}

#[test]
fn position_list_sell_side_is_negative_qty() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.250","side":"Sell","avgPrice":"29200.0","markPrice":"29500.0","positionIdx":0,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(
        reports[0].qty, -0.25,
        "Sell -> negative signed qty, size stays unsigned on the wire"
    );
}

/// Hedge mode: `positionIdx` 1 -> LONG, 2 -> SHORT — never the one-way BOTH default. Both hedge
/// legs are signed by their OWN `side`, not inferred from the leg label.
#[test]
fn position_list_hedge_mode_position_idx_maps_to_long_and_short() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","markPrice":"29500.0","positionIdx":1,"updatedTime":"1700000010000"},
        {"symbol":"BTCUSDT","size":"0.250","side":"Sell","avgPrice":"29200.0","markPrice":"29500.0","positionIdx":2,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].position_side, PositionSide::Long, "positionIdx:1 -> LONG");
    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[1].position_side, PositionSide::Short, "positionIdx:2 -> SHORT");
    assert_eq!(
        reports[1].qty, -0.25,
        "the SHORT leg is still signed negative even though it's Sell-labeled Buy/Sell side"
    );
}

/// A flat (`size:"0"`) row is DELIBERATELY KEPT, not filtered: `recon::diff::diff` needs the report
/// row PRESENT to detect "local still shows a position the venue has since closed" — matching the
/// binance `ReconClient`'s documented choice (Task 7).
#[test]
fn position_list_keeps_flat_rows() {
    let body = r#"{"list":[
        {"symbol":"ETHUSDT","size":"0.000","side":"Buy","avgPrice":"0","markPrice":"1800.0","positionIdx":0,"updatedTime":"1700000011000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 1, "the flat row must survive parsing");
    assert_eq!(reports[0].qty, 0.0);
}

#[test]
fn malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders("not json").is_err());
    assert!(parse_orders("{}").is_err(), "no `list` key is an error");
    assert!(parse_fills("null").is_err());
    assert!(parse_fills("42").is_err());
    assert!(parse_positions("\"oops\"").is_err());
    assert!(parse_positions("{}").is_err());
}

/// `GET /v5/account/wallet-balance` {accountType:UNIFIED} -> the UNIFIED account's USDT
/// `walletBalance` (total wallet cash, NOT `availableToWithdraw` — same field
/// `perp::BybitPerpRest::fetch_usdt_balance` already extracts).
#[test]
fn parses_unified_wallet_balance_usdt() {
    let body = r#"{"list":[{"accountType":"UNIFIED","totalEquity":"1520.5","coin":[
        {"coin":"BTC","walletBalance":"0.01","availableToWithdraw":"0.01"},
        {"coin":"USDT","walletBalance":"1500.25","availableToWithdraw":"1400.0"}
    ]}]}"#;
    assert_eq!(parse_wallet_balance(body).unwrap(), Some(1500.25));
}

#[test]
fn wallet_balance_missing_usdt_coin_is_none() {
    let body =
        r#"{"list":[{"accountType":"UNIFIED","coin":[{"coin":"BTC","walletBalance":"0.01"}]}]}"#;
    assert_eq!(parse_wallet_balance(body).unwrap(), None);
}

#[test]
fn wallet_balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_wallet_balance("not json").is_err());
    assert!(parse_wallet_balance("{}").is_err(), "no `list` key is an error");
    assert!(parse_wallet_balance("null").is_err());
}

// --- the `recon_client` factory (ReconFactory seam, wave-2 task 6) — no network, just proves the
// wiring is infallible ------------------------------------------------------------------------

#[test]
fn recon_client_factory_builds_for_any_credentialed_symbol() {
    let creds = vike_bridge_core::Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: None,
    };
    assert!(
        vike_bybit::recon_client(&creds, "BTCUSDT", false).is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}
