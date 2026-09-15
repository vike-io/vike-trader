//! The `trade_id` gate: every Deribit wire site that reads `trade_id` REFUSES an absent or empty one,
//! and the fill/report it would have produced does not exist.
//!
//! `vike_model::events::TradeId` makes an empty id unrepresentable, but the type alone does not
//! choose what a mapper DOES with a malformed frame — a permissive site could still synthesize
//! `TradeId::prefixed("MISSING-", …)` from `timestamp` or a counter, and compile. Both defeat the
//! dedup the id exists to feed (they differ on the second run), so the chosen behaviour is DROP, and
//! it is asserted here rather than left to a comment.
//!
//! What each drop buys:
//! * On the EVENT lane an id-less fill escapes `vike_exec::ExecutionEngine`'s `seen_trade_ids` guard,
//!   and the same trade is reachable again from `private/get_user_trades_by_instrument` — commission
//!   and realized PnL get booked twice.
//! * On the REPORT lane an id-less `FillReport` can never match `seen_trade_ids` in
//!   `vike_exec::recon::diff`, so it FABRICATES a `MissingFill` divergence, and `MissingFill` is one
//!   of the two kinds `hybrid` policy AUTO-APPLIES — booking the fill again with no operator in front
//!   of it.
//!
//! Deribit's `user.trades` channel is FILLS-ONLY, so a dropped row leaves nothing behind at all. Note
//! the COMBO shapes are covered by the same gate rather than exempted: every leg row carries its own
//! `trade_id`, and the aggregate combo row's equals the legs' `combo_trade_id` (see
//! `vike_deribit::event_mapper`'s module doc), so an id-less row is malformed in both shapes.
//!
//! Both spellings are covered per site: `trade_id` ABSENT and present-but-`""`.

use vike_deribit::event_mapper::{map_deribit_private, map_deribit_trade};
use vike_deribit::recon_client::parse_user_trades;
use vike_model::events::Event;

/// One `user.trades` row (`state: filled`), with `trade_id` rendered by the caller.
fn trade_row(trade_id: serde_json::Value) -> serde_json::Value {
    let mut row = serde_json::json!({
        "instrument_name": "BTC-PERPETUAL", "label": "c1", "direction": "buy",
        "amount": 10.0, "price": 50000.0, "fee": 0.1, "fee_currency": "BTC",
        "liquidity": "T", "state": "filled", "timestamp": 5
    });
    if !trade_id.is_null() {
        row["trade_id"] = trade_id;
    }
    row
}

/// A `user.trades` subscription frame wrapping the rows the caller supplies.
fn subscription(rows: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "method": "subscription",
        "params": { "channel": "user.trades.BTC-PERPETUAL.raw", "data": rows }
    })
}

/// The control: with a real `trade_id` the mapper dual-publishes, so the drops below are attributable
/// to the missing id and nothing else about the row.
#[test]
fn a_trade_with_a_real_id_still_dual_publishes() {
    let evs = map_deribit_trade(&trade_row(serde_json::json!("t99")), "deribit", "BTC-PERPETUAL");
    assert!(
        matches!(evs.as_slice(), [Event::Fill(f), Event::OrderFilled(_)] if f.trade_id == "t99"),
        "control row must still produce Fill + OrderFilled: {evs:?}"
    );
}

/// A `user.trades` row with NO `trade_id` emits NOTHING — the bare `Event::Fill` and its
/// `OrderFilled`/`OrderPartiallyFilled` wrap fall together (a wrap alone would terminalize the FSM
/// for a fill the Account never folded).
#[test]
fn a_trade_without_a_trade_id_emits_no_events() {
    let evs = map_deribit_trade(&trade_row(serde_json::Value::Null), "deribit", "BTC-PERPETUAL");
    assert!(evs.is_empty(), "an id-less trade must vanish entirely, got {evs:?}");
}

/// ...and the same for an EMPTY `trade_id`: `""` is exactly the value the old `unwrap_or_default()`
/// path produced, and the one that skipped dedup outright.
#[test]
fn a_trade_with_an_empty_trade_id_emits_no_events() {
    let evs = map_deribit_trade(&trade_row(serde_json::json!("")), "deribit", "BTC-PERPETUAL");
    assert!(evs.is_empty(), "an empty-id trade must vanish entirely, got {evs:?}");
}

/// Through the real dispatch, and per ROW: one malformed row in a multi-row subscription frame does
/// not take its well-formed siblings with it.
#[test]
fn one_id_less_row_in_a_subscription_frame_does_not_drop_its_siblings() {
    let frame = subscription(vec![
        trade_row(serde_json::json!("t1")),
        trade_row(serde_json::json!("")),
        trade_row(serde_json::json!("t3")),
    ]);
    let evs = map_deribit_private(&frame, "deribit", "BTC-PERPETUAL");
    let fills: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fills, vec!["t1", "t3"], "only the id-less row is dropped: {evs:?}");
    // ...and each surviving fill still carries its wrap: 2 fills + 2 wraps, nothing orphaned.
    assert_eq!(evs.len(), 4, "each surviving fill keeps exactly one wrap: {evs:?}");
}

/// The RECON lane. The row is otherwise complete: it is the missing `trade_id` alone that removes it,
/// and a permissive parser would report a `FillReport` whose empty `trade_id` manufactures the
/// `MissingFill` that `hybrid` auto-applies.
#[test]
fn a_user_trades_row_without_a_trade_id_is_not_reported() {
    let absent = r#"[{"instrument_name":"BTC-PERPETUAL","order_id":"o9","label":"c1","direction":"buy","amount":10.0,"price":50000.0,"fee":0.1,"fee_currency":"BTC","liquidity":"T","timestamp":5}]"#;
    assert!(parse_user_trades(absent).unwrap().is_empty(), "an id-less row must be dropped");
    let empty = r#"[{"trade_id":"","instrument_name":"BTC-PERPETUAL","order_id":"o9","label":"c1","direction":"buy","amount":10.0,"price":50000.0,"fee":0.1,"fee_currency":"BTC","liquidity":"T","timestamp":5}]"#;
    assert!(parse_user_trades(empty).unwrap().is_empty(), "an empty-id row must be dropped");
    // ...and a well-formed sibling in the SAME body still reports, so this is a row filter and not a
    // whole-response failure (still `Ok`, never an `Err` that loses the good row).
    let mixed = r#"[{"trade_id":"","instrument_name":"BTC-PERPETUAL","order_id":"o9","direction":"buy","amount":10.0,"price":50000.0,"timestamp":5},
                    {"trade_id":"t2","instrument_name":"BTC-PERPETUAL","order_id":"o9","direction":"buy","amount":10.0,"price":50000.0,"timestamp":6}]"#;
    let rows = parse_user_trades(mixed).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trade_id, "t2");
}
