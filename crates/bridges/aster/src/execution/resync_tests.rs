use super::*;
use vike_bridge_core::execution_drive::event_tags;
use vike_model::events::Event;

#[test]
fn replays_gap_fill_cancel_expire_and_skips_open() {
    let all_orders = serde_json::json!([
        {"orderId": 9,  "clientOrderId": "cfill",   "status": "FILLED",   "origQty": "1.0", "updateTime": 7},
        {"orderId": 10, "clientOrderId": "ccancel", "status": "CANCELED", "origQty": "2.0", "updateTime": 8},
        {"orderId": 11, "clientOrderId": "copen",   "status": "NEW",      "origQty": "1.0", "updateTime": 6},
        {"orderId": 12, "clientOrderId": "cexpire", "status": "EXPIRED",  "origQty": "1.0", "updateTime": 9}
    ]);
    let my_trades = serde_json::json!([
        {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "time": 5, "isBuyer": true, "isMaker": false}
    ]);
    let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
    // order 9 (FILLED, one trade) -> Fill + OrderFilled; order 10 -> OrderCanceled;
    // order 11 (NEW) -> nothing; order 12 -> OrderExpired.
    assert_eq!(
        event_tags(&evs),
        vec![
            "Fill:cfill:100".to_string(),
            "OrderFilled:cfill".to_string(),
            "OrderCanceled:ccancel".to_string(),
            "OrderExpired:cexpire".to_string(),
        ]
    );
    // the fill carries the correct economics
    if let Event::Fill(fill) = &evs[0] {
        assert_eq!(fill.last_qty, 1.0);
        assert_eq!(fill.last_price, 50000.0);
        assert_eq!(fill.side, 1);
        assert_eq!(fill.symbol, "BTCUSDT");
        assert_eq!(fill.venue, "aster");
    } else {
        panic!("first event must be the bare Fill");
    }
}

#[test]
fn history_fill_surfaces_the_commission_asset() {
    // The account-trade REST row carries `commissionAsset` alongside `commission`.
    let all_orders = serde_json::json!([
        {"orderId": 9, "clientOrderId": "cfill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
    ]);
    let my_trades = serde_json::json!([
        {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "isBuyer": true, "isMaker": false}
    ]);
    let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
    if let Event::Fill(fill) = &evs[0] {
        assert_eq!(fill.commission, 0.1);
        assert_eq!(fill.commission_asset, "USDT");
    } else {
        panic!("first event must be the bare Fill");
    }
}

#[test]
fn partial_fills_then_cancel_wraps_as_partially_filled() {
    // an order that took two partial fills then canceled the remainder
    let all_orders = serde_json::json!([
        {"orderId": 20, "clientOrderId": "cpc", "status": "CANCELED", "origQty": "3.0", "updateTime": 9}
    ]);
    let my_trades = serde_json::json!([
        {"id": 201, "orderId": 20, "price": "100", "qty": "1.0", "commission": "0", "time": 5, "isBuyer": true, "isMaker": true},
        {"id": 202, "orderId": 20, "price": "101", "qty": "1.0", "commission": "0", "time": 6, "isBuyer": true, "isMaker": true}
    ]);
    let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
    assert_eq!(
        event_tags(&evs),
        vec![
            "Fill:cpc:201".to_string(),
            "OrderPartiallyFilled:cpc".to_string(),
            "Fill:cpc:202".to_string(),
            "OrderPartiallyFilled:cpc".to_string(),
            "OrderCanceled:cpc".to_string(),
        ]
    );
}

/// The family's one reading of the two words the replay used to read its own way, over this venue's
/// wrappers: `PENDING_CANCEL` ends nothing (a fill may still beat the cancel), `EXPIRED_IN_MATCH` is an end.
#[test]
fn pending_cancel_ends_nothing_and_expired_in_match_ends_the_order() {
    let all_orders = serde_json::json!([
        {"orderId": 1, "clientOrderId": "c_pending", "status": "PENDING_CANCEL", "updateTime": 5},
        {"orderId": 2, "clientOrderId": "c_stp", "status": "EXPIRED_IN_MATCH", "updateTime": 6}
    ]);
    let none = serde_json::json!([]);
    let want = vec!["OrderExpired:c_stp".to_string()];
    assert_eq!(event_tags(&map_aster_history(&all_orders, &none, "aster", "BTCUSDT")), want);
    assert_eq!(event_tags(&map_aster_perp_history(&all_orders, &none, "aster", "BTCUSDT.P")), want);
}

/// ⚠ The A3 SPOT resync replays **Aster's own** `GET /api/v3/userTrades` rows, which are
/// FUTURES-shaped (`side`/`maker`) — not Binance spot's `isBuyer`/`isMaker`, the spelling every
/// fixture above uses because this module was ported from Binance's twin.
///
/// This is the regression that a path-only fix would have introduced. Before 2026-08-05 the
/// path const pointed at Binance's `/api/v3/myTrades`, which 404s on aster; `execution/mod.rs` swallows
/// that into `json!([])`, so the resync replayed nothing and the grammar mismatch stayed
/// invisible. Correcting the path alone would have started feeding REAL rows through an
/// `isBuyer`-only reader — every replayed buy booked as a SELL, at the moment the core is
/// rebuilding state after a disconnect. `side:"BUY"` MUST map to `+1` here.
#[test]
fn spot_resync_replays_asters_futures_shaped_user_trades() {
    let all_orders = serde_json::json!([
        {"orderId": 266358, "clientOrderId": "cbuy", "status": "FILLED", "origQty": "2.0", "updateTime": 9}
    ]);
    // The published /api/v3/userTrades row shape (asterdex/api-docs, read 2026-08-05):
    // `side` + `maker` + `buyer`, and NO `isBuyer`/`isMaker` anywhere.
    let user_trades = serde_json::json!([
        {"symbol": "BNBUSDT", "id": 1002, "orderId": 266358, "side": "BUY", "price": "1",
         "qty": "2", "quoteQty": "2", "commission": "0.00105000", "commissionAsset": "BNB",
         "time": 1755656788798i64, "counterpartyId": 19, "createUpdateId": null,
         "maker": false, "buyer": true}
    ]);
    let evs = map_aster_history(&all_orders, &user_trades, "aster", "BNBUSDT");
    assert_eq!(
        event_tags(&evs),
        vec!["Fill:cbuy:1002".to_string(), "OrderFilled:cbuy".to_string()]
    );
    let Event::Fill(fill) = &evs[0] else { panic!("first event must be the bare Fill") };
    assert_eq!(fill.side, 1, "side:\"BUY\" -> +1 (an isBuyer-only reader gives -1)");
    assert_eq!(fill.last_price, 1.0);
    // BNB is BNBUSDT's own BASE coin, so the family books the fee in the quote at the fill price
    // (`vike_binance::family::execution::spot_fee`): the figure is unchanged at price 1, the asset is USDT, and
    // the BUY delivers its 2 less that fee.
    assert!((fill.last_qty - 1.99895).abs() < 1e-12, "{}", fill.last_qty);
    assert_eq!(fill.commission, 0.00105);
    assert_eq!(fill.commission_asset, "USDT");
    assert_eq!(
        fill.liquidity_side,
        vike_model::events::LiquiditySide::Taker,
        "maker:false -> Taker"
    );
    assert_eq!(fill.timestamp, 1755656788798);
}
