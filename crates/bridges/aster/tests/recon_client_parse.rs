//! Pure parse tests for the Aster `ReconClient` report parsers (Task 12) — NO network, just
//! representative JSON bodies shaped like Aster's real spot/USDⓈ-M futures REST responses
//! (`/api/v3/openOrders`, `/api/v3/userTrades`, `/fapi/v3/openOrders`, `/fapi/v3/userTrades`,
//! `/fapi/v3/positionRisk` — Aster keeps every fapi path under one `/fapi/v3/*` namespace, unlike
//! Binance's split `/fapi/v1/*` orders/trades vs `/fapi/v2/*` positionRisk). Asserts the tricky
//! mappings the brief calls out explicitly: side sign (BUY/SELL and the `isBuyer` twin), the
//! already-signed perp `positionAmt`, the maker/taker liquidity flag (`maker` vs `isMaker`), and
//! venue-status normalization (`NEW`→`ACCEPTED`, `EXPIRED_IN_MATCH`→`EXPIRED`) to the
//! `OrderStatus::parse` vocabulary `diff::diff` reads. Originally ported verbatim from
//! `vike-binance`'s `tests/offline/recon_client_parse.rs` on the belief that Aster's wire shapes are
//! byte-identical.
//!
//! ⚠ **That belief was false for the SPOT FILL lane**, and it went unnoticed precisely because the
//! port carried Binance's fixture instead of Aster's: aster serves `/api/v3/userTrades` (not
//! `myTrades`) and its rows are futures-shaped (`side`/`maker`, no `isBuyer`/`isMaker`). The two
//! spot-fill tests below now pin BOTH grammars — Aster's real published body, and the Binance
//! spelling the shared parser must keep reading for `vike-binance`'s sake.

use vike_aster::recon_client::{
    parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
    parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
    parse_spot_open_orders,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::FeeSchedule;

/// The exact shape from the task brief.
#[test]
fn parses_perp_user_trade_into_fill_report() {
    let body = r#"[{"symbol":"BTCUSDT","id":28457,"orderId":100,"side":"SELL","price":"30000.0","qty":"0.5","commission":"0.01","commissionAsset":"USDT","maker":true,"time":1700000000000}]"#;
    let reports = parse_perp_user_trades(body).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].side, -1);
    assert_eq!(reports[0].last_px, 30000.0);
    assert_eq!(reports[0].liquidity_side, LiquiditySide::Maker);
}

#[test]
fn perp_user_trade_maps_every_field() {
    let body = r#"[{"symbol":"BTCUSDT","id":28457,"orderId":100,"side":"BUY","price":"30000.0","qty":"0.5","commission":"0.01","commissionAsset":"USDT","maker":false,"time":1700000000000}]"#;
    let reports = parse_perp_user_trades(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "aster");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.trade_id.as_str(), "28457");
    assert_eq!(r.venue_order_id.as_str(), "100");
    assert_eq!(r.side, 1, "BUY -> +1");
    assert_eq!(r.last_qty, 0.5);
    assert_eq!(r.commission, 0.01);
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "maker:false -> Taker");
    assert_eq!(r.ts, 1700000000000);
}

/// **Aster's REAL spot fill wire shape** — the verbatim response example from
/// <https://github.com/asterdex/api-docs> `V3(Recommended)/EN/aster-finance-spot-api-v3.md`,
/// § "Account trade history (USER_DATA)" (`GET /api/v3/userTrades`), read 2026-08-05.
///
/// This is the fixture that would have caught the bug: aster's SPOT rows are FUTURES-shaped
/// (`side` string + `maker` bool + a `buyer` bool that is NOT Binance's `isBuyer`), so the
/// Binance-spot `isBuyer`/`isMaker` reader signs every row `-1`/Taker. `side:"BUY"` here MUST map
/// to `+1` and `maker:false` to `Taker` — if this test ever reads `-1`, the family union grammar
/// (`vike_binance::family::recon::fill_side`) has been "simplified" back to one spelling.
#[test]
fn spot_user_trade_maps_the_real_aster_wire_shape() {
    let body = r#"[
      {
        "symbol": "BNBUSDT",
        "id": 1002,
        "orderId": 266358,
        "side": "BUY",
        "price": "1",
        "qty": "2",
        "quoteQty": "2",
        "commission": "0.00105000",
        "commissionAsset": "BNB",
        "time": 1755656788798,
        "counterpartyId": 19,
        "createUpdateId": null,
        "maker": false,
        "buyer": true
      }
    ]"#;
    let reports = parse_spot_my_trades(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "aster");
    assert_eq!(r.symbol, "BNBUSDT");
    assert_eq!(r.trade_id.as_str(), "1002");
    assert_eq!(r.venue_order_id.as_str(), "266358");
    assert_eq!(r.side, 1, "side:\"BUY\" -> +1 (NOT the absent `isBuyer` -> -1)");
    assert_eq!(r.last_qty, 2.0);
    assert_eq!(r.last_px, 1.0);
    assert_eq!(r.commission, 0.00105);
    assert_eq!(r.commission_asset, "BNB");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "maker:false -> Taker");
    assert_eq!(r.ts, 1755656788798);

    // ...and the SELL direction, so the mapping is proven both ways rather than by one constant.
    let sell = body
        .replace(r#""side": "BUY""#, r#""side": "SELL""#)
        .replace(r#""maker": false"#, r#""maker": true"#);
    let r = &parse_spot_my_trades(&sell).unwrap()[0];
    assert_eq!(r.side, -1, "side:\"SELL\" -> -1");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "maker:true -> Maker");
}

/// The BINANCE-spot spelling (`isBuyer`/`isMaker`), which this shared parser must keep reading —
/// it is the fallback arm of the family union grammar, and `vike-binance` depends on it. Aster's
/// own venue never emits this shape (see the test above); it is pinned here because both venues
/// call the SAME parser, so a change that fixed aster by breaking binance would pass otherwise.
#[test]
fn spot_my_trade_maps_isbuyer_and_ismaker() {
    let body = r#"[{"symbol":"BTCUSDT","id":700,"orderId":123456,"orderListId":-1,"price":"29550.00000000","qty":"0.20000000","quoteQty":"5910.00000000","commission":"0.0002","commissionAsset":"BNB","time":1700000005000,"isBuyer":true,"isMaker":true,"isBestMatch":true}]"#;
    let reports = parse_spot_my_trades(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.side, 1, "isBuyer:true -> +1");
    assert_eq!(r.last_qty, 0.2);
    assert_eq!(r.last_px, 29550.0);
    assert_eq!(r.commission, 0.0002);
    assert_eq!(r.commission_asset, "BNB");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "isMaker:true -> Maker");
    assert_eq!(r.trade_id.as_str(), "700");
    assert_eq!(r.venue_order_id.as_str(), "123456");
    assert_eq!(r.ts, 1700000005000);
}

/// Spot open orders: no venue-side `avgPrice` field — average price is derived from
/// `cummulativeQuoteQty / executedQty`. A `NEW` row (unfilled, empty `clientOrderId`) normalizes
/// status to `ACCEPTED` and treats the empty client id as absent (externally-placed order).
#[test]
fn spot_open_orders_computes_avg_px_and_normalizes_status() {
    let body = r#"[
        {"symbol":"BTCUSDT","orderId":123456,"orderListId":-1,"clientOrderId":"myOrder1","price":"29500.00000000","origQty":"0.50000000","executedQty":"0.20000000","cummulativeQuoteQty":"5910.00000000","status":"PARTIALLY_FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","stopPrice":"0.00000000","icebergQty":"0.00000000","time":1700000000000,"updateTime":1700000005000,"isWorking":true,"origQuoteOrderQty":"0.00000000"},
        {"symbol":"ETHUSDT","orderId":999,"clientOrderId":"","price":"1800.00","origQty":"1.00000000","executedQty":"0.00000000","cummulativeQuoteQty":"0.00000000","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"SELL","time":1700000001000,"updateTime":1700000001000}
    ]"#;
    let reports = parse_spot_open_orders(body).unwrap();
    assert_eq!(reports.len(), 2);

    let filled = &reports[0];
    assert_eq!(filled.symbol, "BTCUSDT");
    assert_eq!(filled.client_order_id.as_deref(), Some("myOrder1"));
    assert_eq!(filled.side, 1);
    assert_eq!(filled.order_type, "limit");
    assert_eq!(filled.qty, 0.5);
    assert_eq!(filled.filled_qty, 0.2);
    assert_eq!(filled.avg_px, 5910.0 / 0.2);
    assert_eq!(filled.status, "PARTIALLY_FILLED");
    assert_eq!(filled.ts, 1700000005000);

    let fresh = &reports[1];
    assert_eq!(fresh.client_order_id, None, "empty clientOrderId normalizes to None");
    assert_eq!(fresh.side, -1);
    assert_eq!(fresh.avg_px, 0.0, "unfilled -> zero avg_px, no divide-by-zero");
    assert_eq!(fresh.status, "ACCEPTED", "NEW -> ACCEPTED");
}

/// Perp open orders carry `avgPrice` directly (no derivation needed); `EXPIRED_IN_MATCH` (a
/// futures-only STP terminal) normalizes to `EXPIRED`.
#[test]
fn perp_open_orders_uses_avg_price_field_and_normalizes_expired_in_match() {
    let body = r#"[{"symbol":"BTCUSDT","orderId":555,"clientOrderId":"perpOrder1","price":"30000.00","avgPrice":"30010.5","origQty":"0.10000000","executedQty":"0.10000000","cumQuote":"3001.05","status":"EXPIRED_IN_MATCH","timeInForce":"GTC","type":"LIMIT","reduceOnly":false,"closePosition":false,"side":"BUY","positionSide":"BOTH","stopPrice":"0","workingType":"CONTRACT_PRICE","priceProtect":false,"origType":"LIMIT","updateTime":1700000002000,"time":1700000002000}]"#;
    let reports = parse_perp_open_orders(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.side, 1);
    assert_eq!(r.avg_px, 30010.5);
    assert_eq!(r.status, "EXPIRED", "EXPIRED_IN_MATCH -> EXPIRED");
    assert_eq!(r.venue_order_id.as_str(), "555");
    assert_eq!(r.client_order_id.as_deref(), Some("perpOrder1"));
}

/// `positionRisk` rows: `positionAmt` is ALREADY SIGNED (long > 0, short < 0) — never re-signed
/// by `position_side`. A flat (qty == 0) row is DELIBERATELY KEPT (not filtered): `diff::diff`
/// needs the report row present to detect "local still shows a position the venue has since
/// closed" (a filtered-out flat row would make that divergence invisible).
#[test]
fn perp_position_risk_keeps_signed_qty_and_flat_rows() {
    let body = r#"[
        {"symbol":"BTCUSDT","positionAmt":"0.500","entryPrice":"29000.0","markPrice":"29500.0","liquidationPrice":"0","leverage":"10","positionSide":"BOTH","updateTime":1700000010000},
        {"symbol":"ETHUSDT","positionAmt":"0.000","entryPrice":"0.0","markPrice":"1800.0","liquidationPrice":"0","leverage":"10","positionSide":"BOTH","updateTime":1700000011000},
        {"symbol":"BTCUSDT","positionAmt":"-0.250","entryPrice":"29200.0","markPrice":"29500.0","liquidationPrice":"0","leverage":"10","marginType":"isolated","isolatedWallet":"812.50","positionSide":"SHORT","updateTime":1700000012000}
    ]"#;
    let reports = parse_perp_position_risk(body).unwrap();
    assert_eq!(reports.len(), 3, "the flat ETHUSDT row must survive parsing");

    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[0].position_side, PositionSide::Both);
    assert_eq!(reports[0].avg_px, 29000.0);
    // No marginType -> the fail-safe Cross default (aster inherits the family-rung parse).
    assert_eq!(reports[0].margin_mode, vike_model::MarginMode::Cross);
    assert_eq!(reports[0].isolated_margin, None);

    assert_eq!(reports[1].qty, 0.0);

    assert_eq!(reports[2].qty, -0.25, "short leg stays negative — already signed by the venue");
    assert_eq!(reports[2].position_side, PositionSide::Short);
    // Margin-mode step-2 rides the SAME family rung binance proves — asserted here for aster too.
    assert_eq!(reports[2].margin_mode, vike_model::MarginMode::Isolated);
    assert_eq!(reports[2].isolated_margin, Some(812.50));
}

#[test]
fn malformed_body_is_an_error_not_a_panic() {
    assert!(parse_spot_open_orders("not json").is_err());
    assert!(parse_perp_open_orders("{}").is_err(), "an object, not an array, is an error");
    assert!(parse_spot_my_trades("null").is_err());
    assert!(parse_perp_user_trades("42").is_err());
    assert!(parse_perp_position_risk("\"oops\"").is_err());
}

/// `GET /fapi/v3/balance` (Aster's endpoint; Binance uses `/fapi/v2/balance`) -> the perp USDT
/// wallet `balance`. The row shape is byte-identical to Binance, so the parser is too.
#[test]
fn parses_perp_balance_usdt() {
    let body = r#"[{"asset":"USDT","balance":"1500.25","availableBalance":"1400.0"}]"#;
    assert_eq!(parse_perp_balance(body).unwrap(), Some(1500.25));
}

#[test]
fn perp_balance_ignores_other_assets_and_missing_usdt_is_none() {
    let body = r#"[{"asset":"BNB","balance":"3.0","availableBalance":"3.0"}]"#;
    assert_eq!(parse_perp_balance(body).unwrap(), None, "no USDT row -> None, not a default 0.0");
}

/// `GET /api/v3/account` -> the spot USDT FREE balance (`balances[].free`, NOT `locked`).
#[test]
fn parses_spot_balance_free_usdt() {
    let body = r#"{"balances":[
        {"asset":"BTC","free":"0.5","locked":"0.0"},
        {"asset":"USDT","free":"2500.75","locked":"100.0"}
    ]}"#;
    assert_eq!(parse_spot_balance(body).unwrap(), Some(2500.75));
}

#[test]
fn spot_balance_missing_usdt_row_is_none() {
    let body = r#"{"balances":[{"asset":"BTC","free":"0.5","locked":"0.0"}]}"#;
    assert_eq!(parse_spot_balance(body).unwrap(), None);
}

#[test]
fn balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_perp_balance("not json").is_err());
    assert!(parse_perp_balance("{}").is_err(), "an object, not an array, is an error");
    assert!(parse_spot_balance("null").is_err());
    assert!(parse_spot_balance("[]").is_err(), "no `balances` key is an error");
}

/// SPOT fee: `commissionRates{maker,taker}` (fractions) in an `/api/v3/account` body -> FeeSchedule.
/// Aster's account body is a Binance fork, so the shared parser applies — this is the cross-check
/// aster reconcile silently lacked before it routed through the family client. The endpoint is the
/// SAME `/api/v3/account` aster already fetches for balance (no new endpoint, fail-soft).
#[test]
fn parses_spot_commission_rates_into_fee_schedule() {
    let body = r#"{"makerCommission":10,"takerCommission":10,"commissionRates":{"maker":"0.00100000","taker":"0.00100000","buyer":"0.00000000","seller":"0.00000000"},"balances":[]}"#;
    let s = parse_spot_fee_rates(body).unwrap().expect("commissionRates present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 });
}

/// A `/api/v3/account` body without `commissionRates` -> None (fail-soft to the static default).
#[test]
fn spot_fee_rates_absent_is_none() {
    let body = r#"{"balances":[]}"#;
    assert_eq!(parse_spot_fee_rates(body).unwrap(), None);
}

/// PERP fee: `makerCommissionRate`/`takerCommissionRate` -> FeeSchedule. The parser is verified and
/// ready even though the perp commissionRate ENDPOINT is not yet wired (see `ASTER_RECON`): the body
/// shape is a Binance fork, so the moment Aster's endpoint is confirmed the lane works.
#[test]
fn parses_perp_commission_rate_into_fee_schedule() {
    let body =
        r#"{"symbol":"BTCUSDT","makerCommissionRate":"0.000200","takerCommissionRate":"0.000500"}"#;
    let s = parse_perp_fee_rates(body).unwrap().expect("rates present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 });
}

/// A perp commissionRate body missing a rate field -> None (fail-soft).
#[test]
fn perp_fee_rates_absent_is_none() {
    let body = r#"{"symbol":"BTCUSDT"}"#;
    assert_eq!(parse_perp_fee_rates(body).unwrap(), None);
}

// --- the `recon_client` factory (ReconFactory seam, wave-2 task 6) — no network, just proves the
// spot/perp `.P`-suffix routing + agent-wallet signer wiring still constructs ---------------------

#[test]
fn recon_client_factory_routes_spot_and_perp_by_suffix() {
    let creds = vike_bridge_core::Credentials {
        api_key: "0x000000000000000000000000000000000000aa".to_string(),
        api_secret: "0x0123456789012345678901234567890123456789012345678901234567890a".to_string(),
        passphrase: None,
    };
    assert!(
        vike_aster::recon_client(vike_bridge_core::Environment::Demo, &creds, "BTCUSDT").is_some(),
        "plain symbol -> spot"
    );
    assert!(
        vike_aster::recon_client(vike_bridge_core::Environment::Demo, &creds, "BTCUSDT.P")
            .is_some(),
        ".P suffix -> perp"
    );
}
