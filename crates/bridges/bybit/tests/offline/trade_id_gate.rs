//! The `trade_id` gate: every Bybit wire site that reads `execId` REFUSES an absent or empty one,
//! and the fill/report it would have produced does not exist.
//!
//! `vike_model::events::TradeId` makes an empty id unrepresentable, but the type alone does not
//! choose what a mapper DOES with a malformed frame — a permissive site could still synthesize
//! `TradeId::prefixed("MISSING-", …)` from `execTime` or a counter, and compile. Both defeat the
//! dedup the id exists to feed (they differ on the second run), so the chosen behaviour is DROP, and
//! it is asserted here rather than left to a comment.
//!
//! What each drop buys:
//! * On the EVENT lane an id-less fill escapes `vike_exec::ExecutionEngine`'s `seen_trade_ids`
//!   guard, and the audit-A3 resync replays the same execution out of `/v5/execution/list` —
//!   commission and realized PnL get booked twice.
//! * On the REPORT lane an id-less `FillReport` can never match `seen_trade_ids` in
//!   `vike_exec::recon::diff`, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is one
//!   of the two kinds `hybrid` policy AUTO-APPLIES — booking the fill again with no operator in
//!   front of it.
//!
//! Both spellings of the malformed frame are covered per site: `execId` ABSENT and present-but-`""`.
//! A revert to a permissive constructor reddens every test in this file.

use vike_bybit::event_mapper::{map_execution, map_execution_fast};
use vike_bybit::history::map_bybit_history;
use vike_bybit::recon_client::parse_fills;
use vike_model::events::Event;

/// A full `execution` row (`execType: Trade`, fully filled), with `execId` rendered by the caller.
fn execution_row(exec_id: serde_json::Value) -> serde_json::Value {
    let mut row = serde_json::json!({
        "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": "c1", "side": "Buy",
        "execPrice": "50000", "execQty": "1.0", "execFee": "0.1", "feeCurrency": "USDT",
        "isMaker": false, "execTime": "5", "cumExecQty": "1.0", "orderQty": "1.0"
    });
    if !exec_id.is_null() {
        row["execId"] = exec_id;
    }
    row
}

/// A slim `execution.fast` row (the early hint), with `execId` rendered by the caller.
fn fast_row(exec_id: serde_json::Value) -> serde_json::Value {
    let mut row = serde_json::json!({
        "symbol": "BTCUSDT", "orderLinkId": "c1", "side": "Buy", "execPrice": "50000",
        "execQty": "1.0", "isMaker": false, "execTime": "5"
    });
    if !exec_id.is_null() {
        row["execId"] = exec_id;
    }
    row
}

/// The control: with a real `execId` the mapper dual-publishes, so the drops below are attributable
/// to the missing id and not to some other malformation of the frame.
#[test]
fn an_execution_with_a_real_id_still_dual_publishes() {
    let evs = map_execution(&execution_row(serde_json::json!("e99")), "bybit", "BTCUSDT");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f), Event::OrderFilled(_)] if f.trade_id == "e99"),
        "control row must still produce Fill + OrderFilled: {evs:?}"
    );
}

/// An `execution` row with NO `execId` emits NOTHING — the bare `Event::Fill` and its
/// `OrderFilled`/`OrderPartiallyFilled` wrap fall together (a wrap alone would terminalize the FSM
/// for a fill the Account never folded).
#[test]
fn an_execution_without_an_exec_id_emits_no_events() {
    let evs = map_execution(&execution_row(serde_json::Value::Null), "bybit", "BTCUSDT");
    assert!(evs.is_empty(), "an id-less execution must vanish entirely, got {evs:?}");
}

/// ...and the same for an EMPTY `execId`: `""` is exactly the value the old `unwrap_or_default()`
/// path produced, and the one that skipped dedup outright.
#[test]
fn an_execution_with_an_empty_exec_id_emits_no_events() {
    let evs = map_execution(&execution_row(serde_json::json!("")), "bybit", "BTCUSDT");
    assert!(evs.is_empty(), "an empty-id execution must vanish entirely, got {evs:?}");
}

/// The fast hint's whole correctness argument is that its `execId` equals the slow `execution`
/// twin's, so `seen_trade_ids` folds the trade once. With no `execId` it would fold here and AGAIN
/// off the twin.
#[test]
fn the_fast_hint_with_a_real_id_emits_a_bare_fill() {
    let evs = map_execution_fast(&fast_row(serde_json::json!("e99")), "bybit", "BTCUSDT");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f)] if f.trade_id == "e99"),
        "the control must produce exactly one bare Fill: {evs:?}"
    );
}

#[test]
fn the_fast_hint_without_an_exec_id_is_dropped() {
    let absent = map_execution_fast(&fast_row(serde_json::Value::Null), "bybit", "BTCUSDT");
    assert!(absent.is_empty(), "an id-less fast hint must be dropped, got {absent:?}");
    let empty = map_execution_fast(&fast_row(serde_json::json!("")), "bybit", "BTCUSDT");
    assert!(empty.is_empty(), "an empty-id fast hint must be dropped, got {empty:?}");
}

/// REST resync: an execution-history row with no `execId` is SKIPPED, and — deliberately — so is the
/// `OrderFilled` wrap it would have carried, leaving the order non-terminal. A stuck-open order is
/// recoverable (confirm-grace watchdog, recon's `MissingTerminal`); a double-booked fill is not.
/// Note the ORDER's own `Cancelled`/`Rejected` terminal is unaffected — only the fill vanishes.
#[test]
fn a_history_execution_row_without_an_exec_id_is_skipped_wrap_included() {
    let order_history = serde_json::json!([
        {"orderId": "o9", "orderLinkId": "c_fill", "orderStatus": "Filled", "updatedTime": "7"}
    ]);
    for exec_history in [
        // `execId` absent
        serde_json::json!([
            {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execPrice": "50000", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "isMaker": false, "execTime": "5"}
        ]),
        // `execId` present but empty
        serde_json::json!([
            {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execId": "", "execPrice": "50000", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "isMaker": false, "execTime": "5"}
        ]),
    ] {
        let evs = map_bybit_history(&order_history, &exec_history, "bybit", "BTCUSDT");
        assert!(
            evs.is_empty(),
            "an id-less history row must produce neither Fill nor wrap, got {evs:?}"
        );
    }
}

/// The skip is per ROW: a well-formed sibling execution on the same order still replays.
#[test]
fn one_id_less_history_row_does_not_drop_its_siblings() {
    let order_history = serde_json::json!([
        {"orderId": "o9", "orderLinkId": "c_fill", "orderStatus": "Filled", "updatedTime": "7"}
    ]);
    let exec_history = serde_json::json!([
        {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execId": "e1", "execPrice": "50000", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "isMaker": false, "execTime": "5"},
        {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execId": "",   "execPrice": "50001", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "isMaker": false, "execTime": "6"}
    ]);
    let evs = map_bybit_history(&order_history, &exec_history, "bybit", "BTCUSDT");
    let fills: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec!["e1"], "only the id-less row is skipped: {evs:?}");
}

/// The RECON lane. The row is otherwise complete: it is the missing `execId` alone that removes it,
/// and a permissive parser would report a `FillReport` whose empty `trade_id` manufactures the
/// `MissingFill` that `hybrid` auto-applies.
#[test]
fn an_execution_list_row_without_an_exec_id_is_not_reported() {
    // The envelope `parse_fills` sees is the already-unwrapped `result` — a bare `{"list":[…]}`,
    // matching `recon_client_parse`'s own bodies.
    let absent = r#"{"list":[{"symbol":"BTCUSDT","orderId":"o9","orderLinkId":"c1","side":"Buy","execQty":"1.0","execPrice":"50000","execFee":"0.1","feeCurrency":"USDT","isMaker":false,"execTime":"5"}]}"#;
    assert!(parse_fills(absent).unwrap().is_empty(), "an id-less row must be dropped");
    let empty = r#"{"list":[{"execId":"","symbol":"BTCUSDT","orderId":"o9","orderLinkId":"c1","side":"Buy","execQty":"1.0","execPrice":"50000","execFee":"0.1","feeCurrency":"USDT","isMaker":false,"execTime":"5"}]}"#;
    assert!(parse_fills(empty).unwrap().is_empty(), "an empty-id row must be dropped");
    // ...and a well-formed sibling in the SAME body still reports, so this is a row filter and not a
    // whole-response failure (still `Ok`, never an `Err` that loses the good row).
    let mixed = r#"{"list":[{"execId":"","symbol":"BTCUSDT","orderId":"o9","side":"Buy","execQty":"1.0","execPrice":"50000","execTime":"5"},
                            {"execId":"e2","symbol":"BTCUSDT","orderId":"o9","side":"Buy","execQty":"1.0","execPrice":"50000","execTime":"6"}]}"#;
    let rows = parse_fills(mixed).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trade_id, "e2");
}
