//! Bybit V5 REST order/execution-history → events, for the audit-A3 post-reconnect resync.
//! Mirrors `binance/history.rs`: replays recent history through the normal event lane, where the
//! core's `execId`/`trade_id` dedup + FSM absorb the overlap and apply only the gap events. The
//! `FillEvent` is built from the exact fields `map_execution` reads (byte-identical fold), plus the
//! perp enrichment (markPrice + positionIdx→position_side) `map_bybit_perp` applies.
//!
//! Bybit quirk vs Binance: `/v5/order/history` returns only CLOSED orders, so a fill on a
//! still-resting order has no order-history row — those are recovered in a second pass as
//! `OrderPartiallyFilled` (never a terminal). `orderLinkId` (our order client id) is carried on BOTH row
//! types, so no id-join is needed to recover the order client id.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`; `isMaker` is read via `as_bool` directly). Deliberately NOT `get_str`,
// which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{
    get_f64 as f, get_fee_f64 as fee_f, get_i64 as i, get_str_boolless as s,
};
use vike_model::events::LiquiditySide;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderRejected, TradeId};

use crate::execution::event_mapper::pside_from_index;

/// Build the bare FillEvent from one `Trade` execution row — the exact fields `map_execution`
/// reads, plus the perp mark/pside enrichment `map_bybit_perp` applies (byte-identical to a live fill).
///
/// `None` for a row carrying no `execId`, which [`emit_fills`] then SKIPS. `execId` is the dedup key
/// the whole replay rests on: this mapper's job is to re-emit history and let the core's
/// `seen_trade_ids`/`seen_fsm_trade_ids` drop what was already folded, so an id-less row is not
/// "replayed once more" but folded a SECOND time — double-booking commission and realized PnL
/// against the live fill it overlaps. Bybit V5 documents `execId` on every `/v5/execution/list`
/// row, so this is a malformed-response path, not a shape the venue has.
fn execution_fill(item: &Value, venue: &str, symbol: &str, spot: bool) -> Option<FillEvent> {
    let mark = match item.get("markPrice") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(x)) => x.parse().ok(),
        _ => None,
    };
    let trade_id = match TradeId::new(s(item, "execId")) {
        Ok(id) => id,
        Err(_) => {
            tracing::warn!(
                venue,
                order_client_id = %s(item, "orderLinkId"),
                "execution-history row carries no `execId` — skipping the replayed fill; with no \
                 dedup key the core cannot tell it from the live fill it overlaps, so replaying it \
                 would double-book commission and realized PnL"
            );
            return None;
        }
    };
    // A history row carries no `category`, and the request named ONE instrument in ONE book: every row is the
    // instrument the REPLAY was asked for, booked under its USER name (decision 0290: a linear `BTCUSDT` row of a
    // `BTCUSDT.P` replay is `BTCUSDT.P`, never the spot pair's `BTCUSDT`).
    let mut fill = FillEvent {
        trade_id,
        order_client_id: s(item, "orderLinkId"),
        venue: venue.to_string().into(),
        symbol: symbol.into(),
        side: if item.get("side").and_then(|x| x.as_str()) == Some("Buy") { 1 } else { -1 },
        last_qty: f(item, "execQty"),
        last_price: f(item, "execPrice"),
        commission: fee_f(item, "execFee", venue),
        commission_asset: s(item, "feeCurrency").into(),
        liquidity_side: LiquiditySide::from_maker(
            item.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false),
        ),
        timestamp: i(item, "execTime"),
        mark_price: mark,
        position_side: pside_from_index(item),
    };
    crate::execution::event_mapper::net_spot_base_fee(&mut fill, spot);
    Some(fill)
}

/// ⚠ Skipping an id-less row (see [`execution_fill`]) is deliberately allowed to cost the WRAP: if
/// the skipped row was the LAST fill of a `Filled` order, this replay emits no `OrderFilled` and the
/// order stays non-terminal in the FSM until the live stream or a later pass terminalizes it. A
/// stuck-open order is recoverable (the confirm-grace watchdog pokes it; recon reports it as
/// `MissingTerminal`); a double-booked fill is money that is already wrong.
fn emit_fills(
    events: &mut Vec<Event>,
    group: &[&Value],
    (venue, symbol, spot): (&str, &str, bool),
    filled: bool,
) {
    let n = group.len();
    for (index, e) in group.iter().enumerate() {
        let Some(fill) = execution_fill(e, venue, symbol, spot) else { continue };
        let order_client_id = fill.order_client_id.clone();
        let ts = fill.timestamp;
        events.extend(vike_bridge_core::terminal_events(
            order_client_id,
            fill,
            ts,
            filled && index + 1 == n,
        ));
    }
}

/// Replay recent Bybit history (audit A3). `order_history` = `/v5/order/history` result.list;
/// `execution_history` = `/v5/execution/list` result.list. Emits per closed order its fills +
/// (OrderCanceled/OrderRejected) terminal, a `Deactivated` conditional's included (the stream's word,
/// [`crate::execution::event_mapper::map_order`]); then, for fills on still-open orders (no order-history row),
/// OrderPartiallyFilled with no terminal. `symbol` is the USER name the history was asked for, and every fill is
/// booked under it; a SPOT pair's (its kept row's book, `crate::instrument::class::venue_symbol`) buy has its
/// base-coin fee netted, as on the stream.
pub fn map_bybit_history(
    order_history: &Value,
    execution_history: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
    let spot = crate::instrument::class::venue_symbol(symbol)
        .is_ok_and(|(_, book)| book == crate::history::Category::Spot);
    let named = (venue, symbol, spot);
    let orders = order_history.as_array().cloned().unwrap_or_default();
    let execs = execution_history.as_array().cloned().unwrap_or_default();

    // Group `Trade` execs by orderId, each sorted by (execTime, execId) ascending (fill order).
    let mut by_order: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for e in &execs {
        if e.get("execType").and_then(|x| x.as_str()) != Some("Trade") {
            continue;
        }
        by_order.entry(s(e, "orderId")).or_default().push(e);
    }
    for v in by_order.values_mut() {
        v.sort_by(|a, b| {
            i(a, "execTime").cmp(&i(b, "execTime")).then(s(a, "execId").cmp(&s(b, "execId")))
        });
    }

    let mut events = Vec::new();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: closed orders (order/history) — fills then non-fill terminal.
    for o in &orders {
        let oid = s(o, "orderId");
        emitted.insert(oid.clone());
        let order_client_id = s(o, "orderLinkId");
        let status = o.get("orderStatus").and_then(|x| x.as_str()).unwrap_or("");
        let ts = i(o, "updatedTime");
        if let Some(group) = by_order.get(&oid) {
            emit_fills(&mut events, group, named, status == "Filled");
        }
        // a post-only order the venue ended for crossing: the typed cancel, as the live stream says it
        if matches!(status, "Cancelled" | "Rejected")
            && crate::execution::event_mapper::is_post_only_cross(o)
        {
            events.push(Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: vike_model::events::POST_ONLY_CANCEL_REASON.into(),
                timestamp: ts,
                post_only_would_cross: true,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            }));
            continue;
        }
        match status {
            "Cancelled" | "PartiallyFilledCanceled" | "Deactivated" => {
                events.push(Event::OrderCanceled(OrderCanceled {
                    order_client_id,
                    reason: s(o, "cancelType").into(),
                    timestamp: ts,
                    post_only_would_cross: false,
                    refusal_id: None,
                }))
            }
            "Rejected" => events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id,
                reason: s(o, "rejectReason").into(),
                timestamp: ts,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            })),
            _ => {} // Filled = terminalized by its OrderFilled wrap; open states = fills only
        }
    }

    // Pass 2: fills on still-open orders (no order/history row) — replay in exec input order for
    // determinism, first-seen orderId only.
    let mut pass2_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &execs {
        if e.get("execType").and_then(|x| x.as_str()) != Some("Trade") {
            continue;
        }
        let oid = s(e, "orderId");
        if emitted.contains(&oid) || pass2_seen.contains(&oid) {
            continue;
        }
        pass2_seen.insert(oid.clone());
        if let Some(group) = by_order.get(&oid) {
            emit_fills(&mut events, group, named, false);
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::execution_drive::event_tags;

    #[test]
    fn replays_gap_fill_cancel_and_open_partial() {
        let order_history = serde_json::json!([
            {"orderId": "o9", "orderLinkId": "c_fill", "orderStatus": "Filled", "updatedTime": "7"},
            {"orderId": "o10", "orderLinkId": "c_cancel", "orderStatus": "Cancelled", "cancelType": "CancelByUser", "updatedTime": "8"},
            {"orderId": "o11", "orderLinkId": "c_rej", "orderStatus": "Rejected", "rejectReason": "EC_NoImmediateQtyToFill", "updatedTime": "9"}
        ]);
        // exec on o9 (filled), plus a fill on o99 (a still-open order, NOT in order_history)
        let execution_history = serde_json::json!([
            {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execId": "e1", "execPrice": "50000", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "feeCurrency": "USDT", "isMaker": false, "execTime": "5"},
            {"execType": "Trade", "orderId": "o99", "orderLinkId": "c_open", "execId": "e2", "execPrice": "51000", "execQty": "0.5", "side": "Sell", "execFee": "0.05", "isMaker": true, "execTime": "6"}
        ]);
        let evs = map_bybit_history(&order_history, &execution_history, "bybit", "BTCUSDT");
        assert_eq!(
            event_tags(&evs),
            vec![
                "Fill:c_fill:e1".to_string(),
                "OrderFilled:c_fill".to_string(),
                "OrderCanceled:c_cancel".to_string(),
                "OrderRejected:c_rej".to_string(),
                "Fill:c_open:e2".to_string(),
                "OrderPartiallyFilled:c_open".to_string(),
            ]
        );
        // fill economics + fee (raw, not negated on bybit)
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.last_qty, 1.0);
            assert_eq!(fill.last_price, 50000.0);
            assert_eq!(fill.side, 1);
            assert_eq!(fill.commission, 0.1);
            assert_eq!(fill.commission_asset, "USDT");
        } else {
            panic!("first event must be the bare Fill");
        }
    }

    /// A post-only order the venue ended for crossing replays as the typed cancel under either status
    /// word, as the stream maps it; a plain cancel does not.
    #[test]
    fn a_post_only_cross_replays_as_the_typed_cancel() {
        let order_history = serde_json::json!([
            {"orderId": "o1", "orderLinkId": "c1", "orderStatus": "Cancelled", "rejectReason": "EC_PostOnlyWillTakeLiquidity", "updatedTime": "8"},
            {"orderId": "o2", "orderLinkId": "c2", "orderStatus": "Rejected", "rejectReason": "EC_PostOnlyWillTakeLiquidity", "updatedTime": "8"},
            {"orderId": "o3", "orderLinkId": "c3", "orderStatus": "Cancelled", "cancelType": "CancelByUser", "updatedTime": "8"}
        ]);
        let typed: Vec<bool> =
            map_bybit_history(&order_history, &serde_json::json!([]), "bybit", "BTCUSDT")
                .iter()
                .map(|e| match e {
                    Event::OrderCanceled(c) => c.post_only_would_cross,
                    other => panic!("{other:?}"),
                })
                .collect();
        assert_eq!(typed, [true, true, false]);
    }

    #[test]
    fn skips_non_trade_execution_rows() {
        let order_history = serde_json::json!([]);
        let execution_history = serde_json::json!([
            {"execType": "Funding", "orderId": "x", "orderLinkId": "c", "execId": "f1", "execTime": "1"},
            {"execType": "BustTrade", "orderId": "y", "orderLinkId": "c", "execId": "b1", "execTime": "2"}
        ]);
        assert!(
            map_bybit_history(&order_history, &execution_history, "bybit", "BTCUSDT").is_empty()
        );
    }
}
