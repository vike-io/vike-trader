//! The `trade_id` gate: every OKX wire site that reads `tradeId` REFUSES an absent or empty one, and
//! the fill/report it would have produced does not exist.
//!
//! `vike_model::events::TradeId` makes an empty id unrepresentable, but the type alone does not
//! choose what a mapper DOES with a malformed frame — a permissive site could still synthesize
//! `TradeId::prefixed("MISSING-", …)` from `fillTime` or a counter, and compile. Both defeat the
//! dedup the id exists to feed (they differ on the second run), so the chosen behaviour is DROP, and
//! it is asserted here rather than left to a comment.
//!
//! ⚠ OKX is the one venue whose live mapper already had a gate: `event_mapper`'s `has_fill` requires
//! a truthy `tradeId` before the fill branch is taken at all, so an id-less `orders` row falls
//! through to the LIFECYCLE path rather than being dropped outright. That is the same outcome for
//! the money lane (no fill is folded) and the tests below pin it — including the lifecycle event the
//! row does still produce, so a future edit to `has_fill` that stops checking `tradeId` cannot pass
//! this file by accident.
//!
//! The REST sites have no such gate and are drops:
//! * `history` (audit-A3 resync) — an id-less fill would escape `seen_trade_ids` and double-book
//!   commission and realized PnL against the live fill it overlaps.
//! * `recon_client::parse_fills` — an id-less `FillReport` can never match `seen_trade_ids` in
//!   `vike_exec::recon::diff`, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is one
//!   of the two kinds `hybrid` policy AUTO-APPLIES: it books the fill again with no operator in
//!   front of it.
//!
//! Both spellings of the malformed frame are covered per site: `tradeId` ABSENT and present-but-`""`.

use vike_model::events::Event;
use vike_okx::event_mapper::map_okx_order;
use vike_okx::history::map_okx_history;
use vike_okx::recon_client::parse_fills;

/// An `orders` row carrying a real fill (`fillSz` non-zero, state `filled`), with `tradeId` rendered
/// by the caller.
fn orders_row(trade_id: serde_json::Value) -> serde_json::Value {
    let mut row = serde_json::json!({
        "instId": "BTC-USDT", "clOrdId": "c1", "state": "filled", "side": "buy",
        "fillSz": "1.0", "fillPx": "50000", "fillFee": "-0.1", "fillFeeCcy": "USDT",
        "execType": "T", "accFillSz": "1.0", "sz": "1.0", "fillTime": "5", "ordId": "o9"
    });
    if !trade_id.is_null() {
        row["tradeId"] = trade_id;
    }
    row
}

/// The control: with a real `tradeId` the mapper dual-publishes, so the assertions below are
/// measuring the missing id and nothing else about the frame.
#[test]
fn an_orders_row_with_a_real_trade_id_still_dual_publishes() {
    let evs = map_okx_order(&orders_row(serde_json::json!("t99")), "okx", "BTC-USDT");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f), Event::OrderFilled(_)] if f.trade_id == "t99"),
        "control row must still produce Fill + OrderFilled: {evs:?}"
    );
}

/// The money-lane property, stated as the thing that actually matters: an `orders` row with no
/// usable `tradeId` produces NO fill and NO fill wrap, whichever of the two gates catches it.
#[test]
fn an_orders_row_without_a_trade_id_folds_no_fill() {
    for row in [
        orders_row(serde_json::Value::Null), // `tradeId` absent
        orders_row(serde_json::json!("")),   // `tradeId` present but empty
        orders_row(serde_json::json!(0)),    // the numeric falsy `has_fill` also rejects
    ] {
        let evs = map_okx_order(&row, "okx", "BTC-USDT");
        assert!(
            !evs.iter().any(|e| matches!(
                e,
                Event::Fill(_) | Event::OrderFilled(_) | Event::OrderPartiallyFilled(_)
            )),
            "an id-less orders row must fold no fill and no fill wrap, got {evs:?}"
        );
    }
}

/// ...and the SHAPE of what it does instead, pinned so the two gates stay distinguishable. `has_fill`
/// rejects first, so the row is treated as lifecycle-only: `state: filled` has no lifecycle arm, so
/// nothing is emitted; a `state: live` row still yields its `OrderAccepted`. If a future edit removes
/// `tradeId` from `has_fill`, the fill branch is entered and `event_mapper`'s own `TradeId::new` arm
/// drops the row instead — the test above stays green either way, which is the point of splitting it
/// from this one.
#[test]
fn an_id_less_orders_row_falls_through_to_the_lifecycle_path() {
    let mut live = orders_row(serde_json::json!(""));
    live["state"] = serde_json::json!("live");
    let evs = map_okx_order(&live, "okx", "BTC-USDT");
    assert!(
        matches!(evs.as_slice(), [Event::OrderAccepted(a)] if a.client_order_id == "c1"),
        "has_fill rejects the empty tradeId, so this is a lifecycle row: {evs:?}"
    );
}

/// REST resync: a `fills-history` row with no `tradeId` is SKIPPED, and — deliberately — so is the
/// `OrderFilled` wrap it would have carried, leaving the order non-terminal. A stuck-open order is
/// recoverable (confirm-grace watchdog, recon's `MissingTerminal`); a double-booked fill is not.
#[test]
fn a_history_fill_row_without_a_trade_id_is_skipped_wrap_included() {
    let orders_history = serde_json::json!([
        {"ordId": "o9", "clOrdId": "c_fill", "state": "filled", "uTime": "7"}
    ]);
    for fills_history in [
        // `tradeId` absent
        serde_json::json!([
            {"ordId": "o9", "instId": "BTC-USDT", "side": "buy", "fillSz": "1", "fillPx": "50000", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "ts": "5"}
        ]),
        // `tradeId` present but empty
        serde_json::json!([
            {"ordId": "o9", "tradeId": "", "instId": "BTC-USDT", "side": "buy", "fillSz": "1", "fillPx": "50000", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "ts": "5"}
        ]),
    ] {
        let evs = map_okx_history(&orders_history, &fills_history, "okx", "BTC-USDT", 1.0);
        assert!(
            evs.is_empty(),
            "an id-less history row must produce neither Fill nor wrap, got {evs:?}"
        );
    }
}

/// The skip is per ROW: a well-formed sibling fill on the same order still replays.
#[test]
fn one_id_less_history_row_does_not_drop_its_siblings() {
    let orders_history = serde_json::json!([
        {"ordId": "o9", "clOrdId": "c_fill", "state": "filled", "uTime": "7"}
    ]);
    let fills_history = serde_json::json!([
        {"ordId": "o9", "tradeId": "t1", "instId": "BTC-USDT", "side": "buy", "fillSz": "1", "fillPx": "50000", "fee": "-0.1", "execType": "T", "ts": "5"},
        {"ordId": "o9", "tradeId": "",   "instId": "BTC-USDT", "side": "buy", "fillSz": "1", "fillPx": "50001", "fee": "-0.1", "execType": "T", "ts": "6"}
    ]);
    let evs = map_okx_history(&orders_history, &fills_history, "okx", "BTC-USDT", 1.0);
    let fills: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec!["t1"], "only the id-less row is skipped: {evs:?}");
}

/// The RECON lane. The row is otherwise complete: it is the missing `tradeId` alone that removes it,
/// and a permissive parser would report a `FillReport` whose empty `trade_id` manufactures the
/// `MissingFill` that `hybrid` auto-applies.
#[test]
fn a_fills_row_without_a_trade_id_is_not_reported() {
    let absent = r#"[{"instId":"BTC-USDT","ordId":"o9","clOrdId":"c1","side":"buy","fillSz":"1","fillPx":"50000","fee":"-0.1","feeCcy":"USDT","execType":"T","ts":"5"}]"#;
    assert!(parse_fills(absent, 1.0).unwrap().is_empty(), "an id-less row must be dropped");
    let empty = r#"[{"tradeId":"","instId":"BTC-USDT","ordId":"o9","clOrdId":"c1","side":"buy","fillSz":"1","fillPx":"50000","fee":"-0.1","feeCcy":"USDT","execType":"T","ts":"5"}]"#;
    assert!(parse_fills(empty, 1.0).unwrap().is_empty(), "an empty-id row must be dropped");
    // ...and a well-formed sibling in the SAME body still reports, so this is a row filter and not a
    // whole-response failure (still `Ok`, never an `Err` that loses the good row).
    let mixed = r#"[{"tradeId":"","instId":"BTC-USDT","ordId":"o9","side":"buy","fillSz":"1","fillPx":"50000","ts":"5"},
                    {"tradeId":"t2","instId":"BTC-USDT","ordId":"o9","side":"buy","fillSz":"1","fillPx":"50000","ts":"6"}]"#;
    let rows = parse_fills(mixed, 1.0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trade_id, "t2");
}
