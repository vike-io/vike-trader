//! Pure parse tests for the binance `ReconClient` report parsers (Task 7) — NO network, just
//! representative JSON bodies shaped like Binance's real spot/USDS-M futures REST responses
//! (`/api/v3/openOrders`, `/api/v3/myTrades`, `/fapi/v1/openOrders`, `/fapi/v1/userTrades`,
//! `/fapi/v2/positionRisk`). Asserts the tricky mappings the brief calls out explicitly: side
//! sign (BUY/SELL and spot's `isBuyer` twin), the already-signed perp `positionAmt`, the
//! maker/taker liquidity flag (perp's `maker` vs spot's `isMaker`), and venue-status
//! normalization (`NEW`→`ACCEPTED`, `EXPIRED_IN_MATCH`→`EXPIRED`) to the `OrderStatus::parse`
//! vocabulary `diff::diff` reads.

use vike_binance::recon_client::{
    parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
    parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
    parse_spot_open_orders,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode};

/// SPOT: `commissionRates{maker,taker}` (fractions) in an `/api/v3/account` body -> FeeSchedule.
/// This is the field pair already present in the body `connect()` fetches and previously dropped.
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

/// PERP: `/fapi/v1/commissionRate` `makerCommissionRate`/`takerCommissionRate` -> FeeSchedule.
#[test]
fn parses_perp_commission_rate_into_fee_schedule() {
    let body =
        r#"{"symbol":"BTCUSDT","makerCommissionRate":"0.000200","takerCommissionRate":"0.000500"}"#;
    let s = parse_perp_fee_rates(body).unwrap().expect("rates present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 });
}

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
    assert_eq!(r.venue, "binance");
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

/// Binance spot's `myTrades` shape is a DIFFERENT wire format from perp's `userTrades`: side
/// arrives as the boolean `isBuyer` (not a `side` string) and the maker flag is `isMaker` (not
/// `maker`). Both spellings are now read through the family UNION grammar
/// (`vike_binance::family::recon::fill_side`), because Aster's SPOT lane is futures-shaped — see
/// `fill_grammar_is_a_union_not_a_guess` below for the pin that keeps Binance's two lanes exact.
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

/// Unified cross-venue attribution (task 6, fix-round-1): a resting order's `clientOrderId` echoes
/// back whatever `newClientOrderId` was stamped on submit — once a link id is configured that's the
/// broker-prefixed `x-<link_id>-<coid>`. `parse_spot_open_orders`/`parse_perp_open_orders` must
/// decode it back to the BARE local coid (same as the WS mappers), or `vike_exec::recon::diff`
/// would never match a live order to the local registry — every one would diff as
/// `OrphanLocalOrder` and every venue order as `UnknownOrder`, blinding reconcile on both sides.
/// (⚠ was "`hybrid` policy would auto-cancel every one of them"; it would not — see
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.)
#[test]
fn open_orders_strip_broker_prefix_from_client_order_id() {
    let prefixed =
        vike_binance::family::order_map::binance_broker_coid(Some("ABC123"), "deadbeef01");
    assert_eq!(prefixed, "x-ABC123-deadbeef01");

    let spot_body = format!(
        r#"[{{"symbol":"BTCUSDT","orderId":1,"clientOrderId":"{prefixed}","price":"1","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}}]"#
    );
    let spot = parse_spot_open_orders(&spot_body).unwrap();
    assert_eq!(
        spot[0].client_order_id.as_deref(),
        Some("deadbeef01"),
        "spot must strip the broker prefix back to the bare local coid"
    );

    let perp_body = format!(
        r#"[{{"symbol":"BTCUSDT","orderId":1,"clientOrderId":"{prefixed}","price":"1","avgPrice":"0","origQty":"1","executedQty":"0","cumQuote":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}}]"#
    );
    let perp = parse_perp_open_orders(&perp_body).unwrap();
    assert_eq!(
        perp[0].client_order_id.as_deref(),
        Some("deadbeef01"),
        "perp must strip the broker prefix back to the bare local coid"
    );

    // An unconfigured (bare) id — the common case — passes through untouched (no false-positive
    // strip), proven against the SAME parser this test exercises.
    let bare_body = r#"[{"symbol":"BTCUSDT","orderId":2,"clientOrderId":"deadbeef01","price":"1","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}]"#;
    let bare = parse_spot_open_orders(bare_body).unwrap();
    assert_eq!(bare[0].client_order_id.as_deref(), Some("deadbeef01"));
}

/// `positionRisk` rows: `positionAmt` is ALREADY SIGNED (long > 0, short < 0) — never re-signed
/// by `position_side`. A flat (qty == 0) row is DELIBERATELY KEPT (not filtered): `diff::diff`
/// needs the report row present to detect "local still shows a position the venue has since
/// closed" (a filtered-out flat row would make that divergence invisible).
#[test]
fn perp_position_risk_keeps_signed_qty_and_flat_rows() {
    let body = r#"[
        {"symbol":"BTCUSDT","positionAmt":"0.500","entryPrice":"29000.0","markPrice":"29500.0","liquidationPrice":"0","leverage":"10","marginType":"cross","isolatedWallet":"0","isolatedMargin":"0.00000000","positionSide":"BOTH","updateTime":1700000010000},
        {"symbol":"ETHUSDT","positionAmt":"0.000","entryPrice":"0.0","markPrice":"1800.0","liquidationPrice":"0","leverage":"10","positionSide":"BOTH","updateTime":1700000011000},
        {"symbol":"BTCUSDT","positionAmt":"-0.250","entryPrice":"29200.0","markPrice":"29500.0","liquidationPrice":"0","leverage":"10","marginType":"isolated","isolatedWallet":"812.50","isolatedMargin":"820.00","positionSide":"SHORT","updateTime":1700000012000}
    ]"#;
    let reports = parse_perp_position_risk(body).unwrap();
    assert_eq!(reports.len(), 3, "the flat ETHUSDT row must survive parsing");

    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[0].position_side, PositionSide::Both);
    assert_eq!(reports[0].avg_px, 29000.0);
    // marginType "cross": Cross mode, and the venue's zero isolatedWallet is NOT surfaced.
    assert_eq!(reports[0].margin_mode, MarginMode::Cross);
    assert_eq!(reports[0].isolated_margin, None);

    assert_eq!(reports[1].qty, 0.0);
    // No marginType at all -> the fail-safe Cross default (pre-field behavior).
    assert_eq!(reports[1].margin_mode, MarginMode::Cross);
    assert_eq!(reports[1].isolated_margin, None);

    assert_eq!(reports[2].qty, -0.25, "short leg stays negative — already signed by the venue");
    assert_eq!(reports[2].position_side, PositionSide::Short);
    // marginType "isolated": Isolated + the isolatedWallet balance (preferred over isolatedMargin).
    assert_eq!(reports[2].margin_mode, MarginMode::Isolated);
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

/// The exact shape from the task brief: `GET /fapi/v2/balance` -> the perp USDT wallet `balance`
/// (NOT `availableBalance`, which excludes margin locked by open positions/orders).
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

/// `GET /api/v3/account` -> the spot USDT FREE balance (`balances[].free`, NOT `locked` — the
/// portion tied up in resting sell orders is excluded from the account's authoritative cash).
#[test]
fn parses_spot_balance_free_usdt() {
    let body = r#"{"balances":[
        {"asset":"BTC","free":"0.50000000","locked":"0.10000000"},
        {"asset":"USDT","free":"2500.75","locked":"100.00"}
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

// --- the `recon_client` factory (ReconFactory seam, wave-2 task 6) — no network, just proves the
// spot/perp `.P`-suffix routing still wires the right client ------------------------------------

#[test]
fn recon_client_factory_routes_spot_and_perp_by_suffix() {
    let creds = vike_bridge_core::Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: None,
    };
    assert!(vike_binance::recon_client(&creds, "BTCUSDT", false).is_some(), "plain symbol -> spot");
    assert!(vike_binance::recon_client(&creds, "BTCUSDT.P", false).is_some(), ".P suffix -> perp");
}

// --- the family UNION fill grammar (the aster spot-lane fix, 2026-08-05) -----------------------

/// The Binance family spells a fill row's side/liquidity TWO ways, and `fill_side`/`fill_is_maker`
/// read whichever is present. This pins that the union is a UNION, not a guess:
///
///  - a BINANCE SPOT row (`isBuyer`/`isMaker` only) still reads exactly as it did before the union;
///  - a BINANCE PERP row (`side`/`maker` only) likewise;
///  - the explicit `side` string WINS over `isBuyer` when a body somehow carried both, so the
///    precedence is asserted rather than incidental;
///  - a row carrying NEITHER spelling degrades to `-1`/Taker — the pre-union fallback, unchanged.
///
/// The venue that forced this exists in the sibling crate: aster's spot `/api/v3/userTrades` rows
/// are futures-shaped, so a spot parser reading `isBuyer` alone signed every fill `-1`.
#[test]
fn fill_grammar_is_a_union_not_a_guess() {
    let spot = r#"[{"symbol":"BTCUSDT","id":1,"orderId":2,"price":"10","qty":"1","commission":"0","time":5,"isBuyer":true,"isMaker":true}]"#;
    let r = &parse_spot_my_trades(spot).unwrap()[0];
    assert_eq!((r.side, r.liquidity_side), (1, LiquiditySide::Maker), "binance spot spelling");

    let perp = r#"[{"symbol":"BTCUSDT","id":1,"orderId":2,"price":"10","qty":"1","commission":"0","time":5,"side":"SELL","maker":false}]"#;
    let r = &parse_perp_user_trades(perp).unwrap()[0];
    assert_eq!((r.side, r.liquidity_side), (-1, LiquiditySide::Taker), "binance perp spelling");

    // Precedence: the explicit strings win over the boolean twins.
    let both = r#"[{"symbol":"BTCUSDT","id":1,"orderId":2,"price":"10","qty":"1","commission":"0","time":5,"side":"SELL","maker":false,"isBuyer":true,"isMaker":true}]"#;
    let r = &parse_spot_my_trades(both).unwrap()[0];
    assert_eq!(
        (r.side, r.liquidity_side),
        (-1, LiquiditySide::Taker),
        "`side`/`maker` outrank `isBuyer`/`isMaker`"
    );

    // Neither spelling: the unchanged fail-safe (never a panic, never a fabricated BUY).
    let bare = r#"[{"symbol":"BTCUSDT","id":1,"orderId":2,"price":"10","qty":"1","commission":"0","time":5}]"#;
    let r = &parse_spot_my_trades(bare).unwrap()[0];
    assert_eq!((r.side, r.liquidity_side), (-1, LiquiditySide::Taker), "absent -> SELL/Taker");
}
