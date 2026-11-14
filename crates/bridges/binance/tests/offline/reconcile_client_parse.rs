//! Pure parse tests for the binance `ReconcileClient` report parsers (Task 7) — NO network, just
//! representative JSON bodies shaped like Binance's real spot/USDS-M futures REST responses
//! (`/api/v3/openOrders`, `/api/v3/myTrades`, `/fapi/v1/openOrders`, `/fapi/v1/userTrades`,
//! `/fapi/v2/positionRisk`). Asserts the tricky mappings the brief calls out explicitly: side
//! sign (BUY/SELL and spot's `isBuyer` twin), the already-signed perp `positionAmt`, the
//! maker/taker liquidity flag (perp's `maker` vs spot's `isMaker`), and venue-status
//! normalization (`NEW`→`ACCEPTED`, `EXPIRED_IN_MATCH`→`EXPIRED`) to the `OrderStatus::parse`
//! vocabulary `diff::diff` reads.

use vike_binance::reconcile::{
    parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
    parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
    parse_spot_open_orders,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode};

/// SPOT: `commissionRates{maker,taker}` (fractions) in an `/api/v3/account` body -> FeeSchedule.
/// This is the field pair already present in the account body the balance read fetches.
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
    assert_eq!(reports[0].last_price, 30000.0);
    assert_eq!(reports[0].liquidity_side, LiquiditySide::Maker);
}

#[test]
fn perp_user_trade_maps_every_field() {
    let body = r#"[{"symbol":"BTCUSDT","id":28457,"orderId":100,"side":"BUY","price":"30000.0","qty":"0.5","commission":"0.01","commissionAsset":"USDT","maker":false,"time":1700000000000}]"#;
    let reports = parse_perp_user_trades(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "binance");
    // The perp SERIES label, not the row's bare `symbol`: the local book keys a perp on
    // `BTCUSDT.P`, and a bare report never matched it (fixed 2026-10-03).
    assert_eq!(r.symbol, "BTCUSDT.P");
    assert_eq!(r.trade_id.as_str(), "28457");
    assert_eq!(r.venue_order_id.as_str(), "100");
    assert_eq!(r.side, 1, "BUY -> +1");
    assert_eq!(r.last_qty, 0.5);
    assert_eq!(r.commission, 0.01);
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "maker:false -> Taker");
    assert_eq!(r.timestamp, 1700000000000);
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
    assert_eq!(r.last_price, 29550.0);
    assert_eq!(r.commission, 0.0002);
    assert_eq!(r.commission_asset, "BNB");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "isMaker:true -> Maker");
    assert_eq!(r.trade_id.as_str(), "700");
    assert_eq!(r.venue_order_id.as_str(), "123456");
    assert_eq!(r.timestamp, 1700000005000);
}

/// Spot open orders: no venue-side `avgPrice` field — average price is derived from
/// `cummulativeQuoteQty / executedQty`. A `NEW` row (unfilled, empty `clientOrderId`) normalizes
/// status to `ACCEPTED` and treats the empty client id as absent (externally-placed order).
#[test]
fn spot_open_orders_computes_average_price_and_normalizes_status() {
    let body = r#"[
        {"symbol":"BTCUSDT","orderId":123456,"orderListId":-1,"clientOrderId":"myOrder1","price":"29500.00000000","origQty":"0.50000000","executedQty":"0.20000000","cummulativeQuoteQty":"5910.00000000","status":"PARTIALLY_FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","stopPrice":"0.00000000","icebergQty":"0.00000000","time":1700000000000,"updateTime":1700000005000,"isWorking":true,"origQuoteOrderQty":"0.00000000"},
        {"symbol":"ETHUSDT","orderId":999,"clientOrderId":"","price":"1800.00","origQty":"1.00000000","executedQty":"0.00000000","cummulativeQuoteQty":"0.00000000","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"SELL","time":1700000001000,"updateTime":1700000001000}
    ]"#;
    let reports = parse_spot_open_orders(body, None).unwrap();
    assert_eq!(reports.len(), 2);

    let filled = &reports[0];
    assert_eq!(filled.symbol, "BTCUSDT");
    assert_eq!(filled.order_client_id.as_deref(), Some("myOrder1"));
    assert_eq!(filled.side, 1);
    assert_eq!(filled.order_type, vike_model::OrderType::Limit);
    assert_eq!(filled.qty, 0.5);
    assert_eq!(filled.filled_qty, 0.2);
    assert_eq!(filled.average_price, 5910.0 / 0.2);
    assert_eq!(filled.status, vike_model::OrderStatus::PartiallyFilled);
    assert_eq!(filled.timestamp, 1700000005000);

    let fresh = &reports[1];
    assert_eq!(fresh.order_client_id, None, "empty clientOrderId normalizes to None");
    assert_eq!(fresh.side, -1);
    assert_eq!(fresh.average_price, 0.0, "unfilled -> zero avg_px, no divide-by-zero");
    assert_eq!(fresh.status, vike_model::OrderStatus::Accepted, "NEW -> ACCEPTED");
}

/// A spot TRAILING stop reads back as a `trailing` (`family::recon`'s `normalize_order_type`): a
/// `STOP_LOSS` with a `trailingDelta` (no activation) and a `TAKE_PROFIT` with one (the activated form the
/// bridge sends). Without a delta, or a delta of 0, they stay a stop and a take-profit; a trailing
/// `STOP_LOSS_LIMIT` keeps its kind (a FIXED limit). ⚠ Not a capture: the FAQ's fields on the documented
/// `openOrders` row; the spot smoke `trailing_percent_smoke` prints the venue's own row.
#[test]
fn spot_open_orders_read_a_trailing_delta_as_a_trailing_stop() {
    let row = |order_client_id: &str, ty: &str, extra: &str| {
        format!(
            r#"{{"symbol":"ETHUSDT","orderId":7,"clientOrderId":"{order_client_id}","price":"0.00","origQty":"0.0042","executedQty":"0","cummulativeQuoteQty":"0","status":"NEW","timeInForce":"GTC","type":"{ty}","side":"BUY","time":1,"updateTime":2{extra}}}"#
        )
    };
    let body = format!(
        "[{},{},{},{},{}]",
        row("tN", "STOP_LOSS", r#","trailingDelta":1000"#),
        row("tA", "TAKE_PROFIT", r#","stopPrice":"2900.00","trailingDelta":500"#),
        row("pS", "STOP_LOSS", r#","stopPrice":"3100.00""#),
        row("z0", "TAKE_PROFIT", r#","stopPrice":"2900.00","trailingDelta":0"#),
        row("tL", "STOP_LOSS_LIMIT", r#","stopPrice":"3100.00","trailingDelta":"300""#),
    );
    let reports = parse_spot_open_orders(&body, None).unwrap();
    let kinds: Vec<_> = reports
        .iter()
        .map(|r| (r.order_client_id.clone().unwrap(), r.order_type.clone()))
        .collect();
    use vike_model::OrderType::{Stop, StopLimit, TakeProfit, Trailing};
    let want =
        [("tN", Trailing), ("tA", Trailing), ("pS", Stop), ("z0", TakeProfit), ("tL", StopLimit)];
    assert_eq!(kinds, want.map(|(c, k)| (c.to_string(), k)));
    // The terms: a trailing stop's percent is its delta in bips / 100 and its activation the
    // `TAKE_PROFIT` form's `stopPrice`; the others' `stopPrice` is their trigger, "0.00" no price.
    let terms: Vec<_> =
        reports.iter().map(|r| (r.price, r.trigger_price, r.trail, r.trail_percent)).collect();
    assert_eq!(
        terms,
        [
            (None, None, None, Some(10.0)),
            (None, Some(2900.0), None, Some(5.0)),
            (None, Some(3100.0), None, None),
            (None, Some(2900.0), None, None),
            (None, Some(3100.0), None, None),
        ]
    );
}

/// Perp open orders carry `avgPrice` directly (no derivation needed); `EXPIRED_IN_MATCH` (a
/// futures-only STP terminal) normalizes to `EXPIRED`.
#[test]
fn perp_open_orders_uses_avg_price_field_and_normalizes_expired_in_match() {
    let body = r#"[{"symbol":"BTCUSDT","orderId":555,"clientOrderId":"perpOrder1","price":"30000.00","avgPrice":"30010.5","origQty":"0.10000000","executedQty":"0.10000000","cumQuote":"3001.05","status":"EXPIRED_IN_MATCH","timeInForce":"GTC","type":"LIMIT","reduceOnly":false,"closePosition":false,"side":"BUY","positionSide":"BOTH","stopPrice":"0","workingType":"CONTRACT_PRICE","priceProtect":false,"origType":"LIMIT","updateTime":1700000002000,"time":1700000002000}]"#;
    let reports = parse_perp_open_orders(body, None).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.side, 1);
    assert_eq!(r.average_price, 30010.5);
    assert_eq!(r.status, vike_model::OrderStatus::Expired, "EXPIRED_IN_MATCH -> EXPIRED");
    assert_eq!(r.venue_order_id.as_str(), "555");
    assert_eq!(r.order_client_id.as_deref(), Some("perpOrder1"));
    assert_eq!(r.symbol, "BTCUSDT.P", "a perp order report carries the series label");
}

/// Unified cross-venue attribution (task 6, fix-round-1): a resting order's `clientOrderId` echoes
/// back whatever `newClientOrderId` was stamped on submit — once a link id is configured that's the
/// broker-prefixed `x-<link_id>-<order client id>`. `parse_spot_open_orders`/`parse_perp_open_orders` must
/// decode it back to the BARE local order client id (same as the WS mappers), or `vike_execution::recon::diff`
/// would never match a live order to the local registry — every one would diff as
/// `OrphanLocalOrder` and every venue order as `UnknownOrder`, blinding reconcile on both sides.
/// (⚠ was "`hybrid` policy would auto-cancel every one of them"; it would not — see
/// `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`.)
#[test]
fn open_orders_strip_broker_prefix_from_order_client_id() {
    let prefixed = vike_binance::family::execution::order_map::binance_broker_order_client_id(
        Some("ABC123"),
        "deadbeef01",
    );
    assert_eq!(prefixed, "x-ABC123-deadbeef01");

    let spot_body = format!(
        r#"[{{"symbol":"BTCUSDT","orderId":1,"clientOrderId":"{prefixed}","price":"1","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}}]"#
    );
    let spot = parse_spot_open_orders(&spot_body, Some("ABC123")).unwrap();
    assert_eq!(
        spot[0].order_client_id.as_deref(),
        Some("deadbeef01"),
        "spot must strip the broker prefix back to the bare local order client id"
    );

    let perp_body = format!(
        r#"[{{"symbol":"BTCUSDT","orderId":1,"clientOrderId":"{prefixed}","price":"1","avgPrice":"0","origQty":"1","executedQty":"0","cumQuote":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}}]"#
    );
    let perp = parse_perp_open_orders(&perp_body, Some("ABC123")).unwrap();
    assert_eq!(
        perp[0].order_client_id.as_deref(),
        Some("deadbeef01"),
        "perp must strip the broker prefix back to the bare local order client id"
    );

    // An unconfigured (bare) id — the common case — passes through untouched (no false-positive
    // strip), proven against the SAME parser this test exercises.
    let bare_body = r#"[{"symbol":"BTCUSDT","orderId":2,"clientOrderId":"deadbeef01","price":"1","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","time":1,"updateTime":1}]"#;
    let bare = parse_spot_open_orders(bare_body, None).unwrap();
    assert_eq!(bare[0].order_client_id.as_deref(), Some("deadbeef01"));
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
    // Each row is labelled from its OWN symbol — the series label the local book keys on.
    let labels: Vec<&str> = reports.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(labels, ["BTCUSDT.P", "ETHUSDT.P", "BTCUSDT.P"]);

    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[0].position_side, PositionSide::Both);
    assert_eq!(reports[0].average_price, 29000.0);
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
    assert!(parse_spot_open_orders("not json", None).is_err());
    assert!(parse_perp_open_orders("{}", None).is_err(), "an object, not an array, is an error");
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

/// `GET /api/v3/account` -> the spot USDT TOTAL, `free + locked`: the quote a resting BUY locks is
/// still the account's cash (the user-data stream's `outboundAccountPosition` states the same sum).
#[test]
fn parses_spot_balance_total_usdt() {
    let body = r#"{"balances":[
        {"asset":"BTC","free":"0.50000000","locked":"0.10000000"},
        {"asset":"USDT","free":"2500.75","locked":"100.00"}
    ]}"#;
    assert_eq!(parse_spot_balance(body).unwrap(), Some(2600.75));
}

/// A row that states no `locked` (or a null one) holds nothing locked, as the stream mapper reads
/// it; a `locked` that is present and not a number is "not reported", never a free-only guess.
#[test]
fn spot_balance_without_a_locked_figure_is_the_free_figure_and_an_unreadable_one_is_none() {
    let absent = r#"{"balances":[{"asset":"USDT","free":"2500.75"}]}"#;
    assert_eq!(parse_spot_balance(absent).unwrap(), Some(2500.75));
    let null = r#"{"balances":[{"asset":"USDT","free":"2500.75","locked":null}]}"#;
    assert_eq!(parse_spot_balance(null).unwrap(), Some(2500.75));
    let word = r#"{"balances":[{"asset":"USDT","free":"2500.75","locked":"oops"}]}"#;
    assert_eq!(parse_spot_balance(word).unwrap(), None);
    let no_free = r#"{"balances":[{"asset":"USDT","locked":"100.0"}]}"#;
    assert_eq!(parse_spot_balance(no_free).unwrap(), None);
}

// --- a resting spot BUY must not read as unexplained cash (the pass the reconcile really runs) ---
//
// The venue moves price x qty of USDT from `free` to `locked` while a BUY rests; the engine's wallet
// is set from the user-data stream's `outboundAccountPosition`, which states `free + locked` (the
// captured frame `tests/fixtures/captured/ws_account_state.json` shows it: free + locked of the
// frame that opened a resting BUY is the free of the frame that followed its cancel, to the last
// digit). The body below is the venue's own: the `GET /api/v3/account` the Binance spot DEMO
// returned at 2026-10-30 19:57 UTC while `binance_reconcile_order_lifecycle_smoke`'s far BUY
// rested (account id left out, and all but three of its all-zero asset rows). Before the
// order the cash was 4983.54405618 free, and after its cancel it was again. The stream frame is the
// same state in the stream's words (`f` and `l` are the row's `free` and `locked`).

const RESTING_FRAME: &str = r#"{"subscriptionId":0,"event":{"B":[
    {"a":"USDT","f":"4975.84706218","l":"7.69699400"}],
    "E":1793390247182,"e":"outboundAccountPosition","u":1793390247182}}"#;

const RESTING_ACCOUNT_BODY: &str = r#"{"accountType":"SPOT","balances":[
    {"asset":"BTC","free":"0.00024112","locked":"0.00000000"},
    {"asset":"USDT","free":"4975.84706218","locked":"7.69699400"},
    {"asset":"USDC","free":"5000.00000000","locked":"0.00000000"},
    {"asset":"IDR","free":"0.00","locked":"0.00"},{"asset":"BTTC","free":"0.0","locked":"0.0"},
    {"asset":"1000SATS","free":"0.00","locked":"0.00"}],
    "brokered":false,"buyerCommission":0,"canDeposit":true,"canTrade":true,"canWithdraw":true,
    "commissionRates":{"buyer":"0.00000000","maker":"0.00100000","seller":"0.00000000",
    "taker":"0.00100000"},"makerCommission":10,"permissions":["SPOT"],"preventSor":false,
    "requireSelfTradePrevention":false,"sellerCommission":0,"takerCommission":10,
    "updateTime":1793390247182}"#;

/// The engine's cash figure after the stream frame, in the shape the reconcile diff reads it.
fn engine_cash_after(frame: &str) -> vike_execution::recon::ReconcileLocalCash {
    let frame: serde_json::Value = serde_json::from_str(frame).unwrap();
    let events = vike_binance::execution::event_mapper::spot::map_binance_private(
        &frame, "binance", "BTCUSDT", None,
    );
    let Some(vike_model::events::Event::AccountState(state)) = events.into_iter().next() else {
        panic!("the frame must map to an AccountState");
    };
    let mut account =
        vike_execution::Account::new(1.0, "binance", None, vike_execution::BalanceMode::Delta);
    account.apply_account_state(&state, "USDT");
    vike_execution::recon::ReconcileLocalCash {
        balance: account.balance,
        realized_pnl: account.realized_pnl,
        realized_at_sync: account.realized_pnl_at_balance_sync,
        mode: account.balance_mode,
    }
}

fn judge(
    cash: vike_execution::recon::ReconcileLocalCash,
    venue: Option<f64>,
) -> vike_execution::recon::ReconcileBalanceCheck {
    let (orders, seen, positions) =
        (indexmap::IndexMap::new(), std::collections::HashSet::new(), indexmap::IndexMap::new());
    let local = vike_execution::recon::ReconcileLocalView {
        venue: "binance",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &positions,
        quantity_tolerance: 0.0,
        cash,
    };
    vike_execution::recon::diff_balance(
        &local,
        venue,
        "USDT",
        vike_execution::recon::ReconcileBalanceTolerance::default(),
        1,
    )
}

#[test]
fn a_pass_with_a_resting_buy_reports_no_balance_drift() {
    let cash = engine_cash_after(RESTING_FRAME);
    assert!(
        (cash.balance - 4983.54405618).abs() < 1e-6,
        "the stream states free + locked: {}",
        cash.balance
    );
    let venue = parse_spot_balance(RESTING_ACCOUNT_BODY).unwrap();
    match judge(cash, venue) {
        vike_execution::recon::ReconcileBalanceCheck::Anchored { drift: None, .. } => {}
        other => panic!("a resting BUY's lock is cash, not drift: venue {venue:?}, got {other:?}"),
    }
}

#[test]
fn a_pass_still_reports_cash_that_really_left_the_account() {
    // The same resting state, but the venue holds 50 USDT less in total (a withdrawal): reported.
    let cash = engine_cash_after(RESTING_FRAME);
    let venue = parse_spot_balance(RESTING_ACCOUNT_BODY).unwrap().map(|total| total - 50.0);
    match judge(cash, venue) {
        vike_execution::recon::ReconcileBalanceCheck::Anchored {
            drift:
                Some(vike_execution::recon::ReconcileDivergence::BalanceDrift {
                    venue_balance,
                    local,
                    ..
                }),
            ..
        } => assert!(
            (local - venue_balance - 50.0).abs() < 1e-6,
            "local {local}, venue {venue_balance}"
        ),
        other => panic!("a 50 USDT withdrawal must be reported: {other:?}"),
    }
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

/// A demo account's addresses, from the release's `venue_url` seed.
fn demo_hosts() -> vike_binance::execution::BinanceUrls {
    vike_binance::execution::BinanceUrls::resolve(
        &vike_venues::venue_urls::VenueUrls::seed("binance"),
        vike_model::AccountTier::Demo,
    )
    .expect("the seed holds every binance row")
}

#[test]
fn reconcile_client_factory_routes_spot_and_perp_by_suffix() {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "test-key".to_string().into(),
        api_secret: "test-secret".to_string().into(),
        passphrase: None,
    };
    let hosts = demo_hosts();
    assert!(vike_binance::recon_client(&creds, "BTCUSDT", hosts).is_some(), "plain symbol -> spot");
    assert!(vike_binance::recon_client(&creds, "BTCUSDT.P", hosts).is_some(), ".P suffix -> perp");
}

/// The kind claim (`ReconcileClient::lists_trigger_orders`) is made on BOTH lanes, each for its own read:
/// the perp client appends the Algo Service's `openAlgoOrders` to its report, and the spot client's
/// `openOrders` lists the `STOP_LOSS`/`STOP_LOSS_LIMIT` it now places as ordinary rows
/// (`crates/bridges/binance/src/reconcile/mod.rs`'s `spot_trigger_listing_tests` parses the measured ones).
#[test]
fn both_lanes_claim_to_list_trigger_orders() {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "test-key".to_string().into(),
        api_secret: "test-secret".to_string().into(),
        passphrase: None,
    };
    let claim = |symbol: &str| {
        vike_binance::recon_client(&creds, symbol, demo_hosts())
            .expect("offline")
            .lists_trigger_orders()
    };
    assert!(claim("BTCUSDT.P"), "perp: openAlgoOrders is appended to the report");
    assert!(claim("BTCUSDT"), "spot: openOrders lists the spot stop and stop-limit");
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
