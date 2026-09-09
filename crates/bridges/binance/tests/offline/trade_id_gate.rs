//! The `trade_id` gate: every Binance wire site that reads a venue exec id REFUSES an absent or
//! empty one, and the fill/report it would have produced does not exist.
//!
//! `vike_model::events::TradeId` makes an empty id unrepresentable, but the type alone does not
//! choose what a mapper DOES with a malformed frame — a permissive site could still synthesize
//! `TradeId::prefixed("MISSING-", …)` from a counter, or fall back to a timestamp, and compile.
//! Both of those defeat the dedup the id exists to feed (they differ on the second run), so the
//! chosen behaviour is DROP, and it is asserted here rather than left to a comment.
//!
//! What each drop buys, and why it is the safe direction rather than merely the tidy one:
//! * On the EVENT lane an id-less fill escapes `vike_exec::ExecutionEngine`'s `seen_trade_ids`
//!   guard, and the audit-A3 resync replays the same fill out of REST — commission and realized
//!   PnL get booked twice.
//! * On the REPORT lane an id-less `FillReport` can never match `seen_trade_ids` in
//!   `vike_exec::recon::diff`, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is
//!   one of the two kinds `hybrid` policy AUTO-APPLIES — it books the fill a second time with no
//!   operator in front of it.
//!
//! Both spellings of the malformed frame are covered per site: the field ABSENT and the field
//! present-but-`""`. A revert to a permissive constructor reddens every test in this file.

use vike_binance::event_mapper::map_execution_report;
use vike_binance::history::{map_binance_history, map_binance_perp_history};
use vike_binance::perp_mapper::{map_binance_perp, map_binance_perp_opts};
use vike_binance::recon_client::{parse_perp_user_trades, parse_spot_my_trades};
use vike_model::events::Event;

/// A spot `executionReport` on `x=TRADE`, with `t` (tradeId) rendered by the caller.
fn spot_trade(trade_id: serde_json::Value) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "e": "executionReport", "s": "BTCUSDT", "c": "c1", "x": "TRADE", "X": "FILLED",
        "S": "BUY", "l": "1.0", "L": "50000", "n": "0.1", "N": "USDT", "m": false, "T": 5
    });
    if !trade_id.is_null() {
        frame["t"] = trade_id;
    }
    frame
}

/// A perp `ORDER_TRADE_UPDATE` on `o.x=TRADE`, with `o.t` rendered by the caller.
fn perp_trade(trade_id: serde_json::Value) -> serde_json::Value {
    let mut o = serde_json::json!({
        "s": "BTCUSDT", "c": "c1", "x": "TRADE", "X": "FILLED", "S": "BUY",
        "l": "1.0", "L": "50000", "n": "0.1", "N": "USDT", "m": false, "ps": "BOTH"
    });
    if !trade_id.is_null() {
        o["t"] = trade_id;
    }
    serde_json::json!({ "e": "ORDER_TRADE_UPDATE", "T": 5, "o": o })
}

/// A `TRADE_LITE` early fast-fill hint (flat fields), with `t` rendered by the caller.
fn trade_lite(trade_id: serde_json::Value) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "e": "TRADE_LITE", "s": "BTCUSDT", "c": "c1", "S": "BUY",
        "l": "1.0", "L": "50000", "m": false, "T": 5
    });
    if !trade_id.is_null() {
        frame["t"] = trade_id;
    }
    frame
}

/// The control: with a real `t` the spot mapper dual-publishes, so the assertions below are
/// measuring the missing id and not a frame that was malformed for some other reason.
#[test]
fn a_spot_trade_with_a_real_id_still_dual_publishes() {
    let evs = map_execution_report(&spot_trade(serde_json::json!("t99")), "binance", "BTCUSDT");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f), Event::OrderFilled(_)] if f.trade_id == "t99"),
        "control frame must still produce Fill + OrderFilled: {evs:?}"
    );
}

/// An `executionReport` TRADE with NO `t` emits NOTHING — the bare `Event::Fill` and its
/// `OrderFilled` wrap fall together (a wrap alone would terminalize the FSM for a fill the Account
/// never folded).
#[test]
fn a_spot_trade_without_a_trade_id_emits_no_events() {
    let evs = map_execution_report(&spot_trade(serde_json::Value::Null), "binance", "BTCUSDT");
    assert!(evs.is_empty(), "an id-less spot fill must vanish entirely, got {evs:?}");
}

/// ...and the same for an EMPTY `t`: `""` is exactly the value the old `unwrap_or_default()` path
/// produced, and the one that skipped dedup outright.
#[test]
fn a_spot_trade_with_an_empty_trade_id_emits_no_events() {
    let evs = map_execution_report(&spot_trade(serde_json::json!("")), "binance", "BTCUSDT");
    assert!(evs.is_empty(), "an empty-id spot fill must vanish entirely, got {evs:?}");
}

/// The perp control, so the two drops below are attributable to the id alone.
#[test]
fn a_perp_trade_with_a_real_id_still_dual_publishes() {
    let evs = map_binance_perp(&perp_trade(serde_json::json!("t99")), "binance", "BTCUSDT");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f), Event::OrderFilled(_)] if f.trade_id == "t99"),
        "control frame must still produce Fill + OrderFilled: {evs:?}"
    );
}

#[test]
fn a_perp_trade_without_a_trade_id_emits_no_events() {
    let evs = map_binance_perp(&perp_trade(serde_json::Value::Null), "binance", "BTCUSDT");
    assert!(evs.is_empty(), "an id-less perp fill must vanish entirely, got {evs:?}");
}

#[test]
fn a_perp_trade_with_an_empty_trade_id_emits_no_events() {
    let evs = map_binance_perp(&perp_trade(serde_json::json!("")), "binance", "BTCUSDT");
    assert!(evs.is_empty(), "an empty-id perp fill must vanish entirely, got {evs:?}");
}

/// TRADE_LITE's whole correctness argument is that its `t` equals the authoritative
/// `ORDER_TRADE_UPDATE`'s, so `seen_trade_ids` collapses the pair. With no `t` the hint would fold
/// once here and AGAIN off the slow twin, so the hint is dropped — proven with the opt-in flag ON
/// (`map_binance_perp_opts`'s `early_trade_lite_fill`), because a default build drops TRADE_LITE
/// regardless and would pass this test for the wrong reason.
#[test]
fn an_early_trade_lite_hint_with_a_real_id_emits_a_bare_fill() {
    let evs =
        map_binance_perp_opts(&trade_lite(serde_json::json!("t99")), "binance", "BTCUSDT", true);
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f)] if f.trade_id == "t99"),
        "the flag-ON control must produce exactly one bare Fill: {evs:?}"
    );
}

#[test]
fn an_early_trade_lite_hint_without_a_trade_id_is_dropped() {
    let absent =
        map_binance_perp_opts(&trade_lite(serde_json::Value::Null), "binance", "BTCUSDT", true);
    assert!(absent.is_empty(), "an id-less TRADE_LITE hint must be dropped, got {absent:?}");
    let empty =
        map_binance_perp_opts(&trade_lite(serde_json::json!("")), "binance", "BTCUSDT", true);
    assert!(empty.is_empty(), "an empty-id TRADE_LITE hint must be dropped, got {empty:?}");
}

/// REST resync, spot: a `myTrades` row with no `id` is SKIPPED, and — deliberately — so is the
/// `OrderFilled` wrap it would have carried, leaving the order non-terminal. A stuck-open order is
/// recoverable (confirm-grace watchdog, recon's `MissingTerminal`); a double-booked fill is not.
#[test]
fn a_history_trade_row_without_an_id_is_skipped_wrap_included() {
    let all_orders = serde_json::json!([
        {"orderId": 9, "clientOrderId": "c_fill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
    ]);
    for my_trades in [
        // `id` absent
        serde_json::json!([
            {"orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "time": 5, "isBuyer": true, "isMaker": false}
        ]),
        // `id` present but empty
        serde_json::json!([
            {"id": "", "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "time": 5, "isBuyer": true, "isMaker": false}
        ]),
    ] {
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT");
        assert!(
            evs.is_empty(),
            "an id-less history row must produce neither Fill nor wrap, got {evs:?}"
        );
    }
}

/// The sibling order's fills still replay — the skip is per ROW, not per response.
#[test]
fn one_id_less_history_row_does_not_drop_its_siblings() {
    let all_orders = serde_json::json!([
        {"orderId": 20, "clientOrderId": "c_pc", "status": "CANCELED", "origQty": "3.0", "updateTime": 9}
    ]);
    let my_trades = serde_json::json!([
        {"id": 201, "orderId": 20, "price": "100", "qty": "1.0", "commission": "0", "time": 5, "isBuyer": true, "isMaker": true},
        {"id": "",  "orderId": 20, "price": "101", "qty": "1.0", "commission": "0", "time": 6, "isBuyer": true, "isMaker": true}
    ]);
    let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT");
    let fills: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec!["201"], "only the id-less row is skipped: {evs:?}");
}

/// REST resync, perp: same verdict through the fapi twin.
#[test]
fn a_perp_history_trade_row_without_an_id_is_skipped() {
    let all_orders = serde_json::json!([
        {"orderId": 9, "clientOrderId": "c_fill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
    ]);
    for user_trades in [
        serde_json::json!([
            {"orderId": 9, "price": "60000", "qty": "1.0", "commission": "0.2", "maker": true, "time": 5, "side": "SELL", "positionSide": "SHORT"}
        ]),
        serde_json::json!([
            {"id": "", "orderId": 9, "price": "60000", "qty": "1.0", "commission": "0.2", "maker": true, "time": 5, "side": "SELL", "positionSide": "SHORT"}
        ]),
    ] {
        let evs = map_binance_perp_history(&all_orders, &user_trades, "binance", "BTCUSDT");
        assert!(evs.is_empty(), "an id-less perp history row must be skipped, got {evs:?}");
    }
}

/// The RECON lane, spot. Note the row is otherwise complete: it is the missing `id` alone that
/// removes it, and a permissive parser would report a `FillReport` whose empty `trade_id` manufactures
/// the `MissingFill` that `hybrid` auto-applies.
#[test]
fn a_spot_my_trades_row_without_an_id_is_not_reported() {
    // absent `id`
    let absent = r#"[{"symbol":"BTCUSDT","orderId":9,"price":"50000","qty":"1.0","commission":"0.1","commissionAsset":"USDT","time":5,"isBuyer":true,"isMaker":false}]"#;
    assert!(parse_spot_my_trades(absent).unwrap().is_empty(), "an id-less row must be dropped");
    // empty `id`
    let empty = r#"[{"id":"","symbol":"BTCUSDT","orderId":9,"price":"50000","qty":"1.0","commission":"0.1","commissionAsset":"USDT","time":5,"isBuyer":true,"isMaker":false}]"#;
    assert!(parse_spot_my_trades(empty).unwrap().is_empty(), "an empty-id row must be dropped");
    // ...and a well-formed sibling in the SAME body still reports, so this is a row filter and not
    // a whole-response failure (the response is still `Ok`, never an `Err` that loses the good row).
    let mixed = r#"[{"id":"","symbol":"BTCUSDT","orderId":9,"price":"50000","qty":"1.0","time":5,"isBuyer":true,"isMaker":false},
                    {"id":100,"symbol":"BTCUSDT","orderId":9,"price":"50000","qty":"1.0","time":6,"isBuyer":true,"isMaker":false}]"#;
    let rows = parse_spot_my_trades(mixed).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trade_id, "100");
}

/// The RECON lane, perp.
#[test]
fn a_perp_user_trades_row_without_an_id_is_not_reported() {
    let absent = r#"[{"symbol":"BTCUSDT","orderId":9,"price":"60000","qty":"1.0","commission":"0.2","commissionAsset":"USDT","time":5,"side":"SELL","maker":true}]"#;
    assert!(parse_perp_user_trades(absent).unwrap().is_empty(), "an id-less row must be dropped");
    let empty = r#"[{"id":"","symbol":"BTCUSDT","orderId":9,"price":"60000","qty":"1.0","commission":"0.2","commissionAsset":"USDT","time":5,"side":"SELL","maker":true}]"#;
    assert!(parse_perp_user_trades(empty).unwrap().is_empty(), "an empty-id row must be dropped");
}
