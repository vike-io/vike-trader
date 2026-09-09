//! Bybit V5 REST order/execution-history → events, for the audit-A3 post-reconnect resync.
//! Mirrors `binance/history.rs`: replays recent history through the normal event lane, where the
//! core's `execId`/`trade_id` dedup + FSM absorb the overlap and apply only the gap events. The
//! `FillEvent` is built from the exact fields `map_execution` reads (byte-identical fold), plus the
//! perp enrichment (markPrice + positionIdx→position_side) `map_bybit_perp` applies.
//!
//! Bybit quirk vs Binance: `/v5/order/history` returns only CLOSED orders, so a fill on a
//! still-resting order has no order-history row — those are recovered in a second pass as
//! `OrderPartiallyFilled` (never a terminal). `orderLinkId` (our coid) is carried on BOTH row
//! types, so no id-join is needed to recover the coid.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`; `isMaker` is read via `as_bool` directly). Deliberately NOT `get_str`,
// which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderExpired, OrderFilled, OrderPartiallyFilled,
    OrderRejected, TradeId,
};

use crate::event_mapper::pside_from_idx;

/// Build the bare FillEvent from one `Trade` execution row — the exact fields `map_execution`
/// reads, plus the perp mark/pside enrichment `map_bybit_perp` applies (byte-identical to a live fill).
///
/// `None` for a row carrying no `execId`, which [`emit_fills`] then SKIPS. `execId` is the dedup key
/// the whole replay rests on: this mapper's job is to re-emit history and let the core's
/// `seen_trade_ids`/`seen_fsm_trade_ids` drop what was already folded, so an id-less row is not
/// "replayed once more" but folded a SECOND time — double-booking commission and realized PnL
/// against the live fill it overlaps. Bybit V5 documents `execId` on every `/v5/execution/list`
/// row, so this is a malformed-response path, not a shape the venue has.
fn execution_fill(item: &Value, venue: &str, symbol: &str) -> Option<FillEvent> {
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
                client_order_id = %s(item, "orderLinkId"),
                "execution-history row carries no `execId` — skipping the replayed fill; with no \
                 dedup key the core cannot tell it from the live fill it overlaps, so replaying it \
                 would double-book commission and realized PnL"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: s(item, "orderLinkId"),
        venue: venue.to_string().into(),
        symbol: match item.get("symbol") {
            Some(Value::String(x)) => x.clone().into(),
            _ => symbol.to_string().into(),
        },
        side: if item.get("side").and_then(|x| x.as_str()) == Some("Buy") { 1 } else { -1 },
        last_qty: f(item, "execQty"),
        last_px: f(item, "execPrice"),
        commission: f(item, "execFee"),
        commission_asset: s(item, "feeCurrency").into(),
        liquidity_side: if item.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false) {
            LiquiditySide::Maker
        } else {
            LiquiditySide::Taker
        },
        ts: i(item, "execTime"),
        mark_price: mark,
        position_side: pside_from_idx(item).into(),
    })
}

/// ⚠ Skipping an id-less row (see [`execution_fill`]) is deliberately allowed to cost the WRAP: if
/// the skipped row was the LAST fill of a `Filled` order, this replay emits no `OrderFilled` and the
/// order stays non-terminal in the FSM until the live stream or a later pass terminalizes it. A
/// stuck-open order is recoverable (the confirm-grace watchdog pokes it; recon reports it as
/// `MissingTerminal`); a double-booked fill is money that is already wrong.
fn emit_fills(events: &mut Vec<Event>, group: &[&Value], venue: &str, symbol: &str, filled: bool) {
    let n = group.len();
    for (idx, e) in group.iter().enumerate() {
        let Some(fill) = execution_fill(e, venue, symbol) else { continue };
        let coid = fill.client_order_id.clone();
        let ts = fill.ts;
        let wrap = if filled && idx + 1 == n {
            Event::OrderFilled(OrderFilled { client_order_id: coid, fill: fill.clone(), ts })
        } else {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: coid,
                fill: fill.clone(),
                ts,
            })
        };
        events.push(Event::Fill(fill));
        events.push(wrap);
    }
}

/// Replay recent Bybit history (audit A3). `order_history` = `/v5/order/history` result.list;
/// `execution_history` = `/v5/execution/list` result.list. Emits per closed order its fills +
/// (OrderCanceled/OrderExpired/OrderRejected) terminal; then, for fills on still-open orders (no
/// order-history row), OrderPartiallyFilled with no terminal.
pub fn map_bybit_history(
    order_history: &Value,
    execution_history: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
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
        let coid = s(o, "orderLinkId");
        let status = o.get("orderStatus").and_then(|x| x.as_str()).unwrap_or("");
        let ts = i(o, "updatedTime");
        if let Some(group) = by_order.get(&oid) {
            emit_fills(&mut events, group, venue, symbol, status == "Filled");
        }
        match status {
            "Cancelled" | "PartiallyFilledCanceled" => {
                events.push(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid,
                    reason: s(o, "cancelType").into(),
                    ts,
                }))
            }
            "Rejected" => events.push(Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: s(o, "rejectReason").into(),
                ts,
            })),
            "Deactivated" => {
                events.push(Event::OrderExpired(OrderExpired { client_order_id: coid, ts }))
            }
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
            emit_fills(&mut events, group, venue, symbol, false);
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(fill) => format!("Fill:{}:{}", fill.client_order_id, fill.trade_id),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.client_order_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
                Event::OrderExpired(w) => format!("OrderExpired:{}", w.client_order_id),
                Event::OrderRejected(w) => format!("OrderRejected:{}", w.client_order_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn replays_gap_fill_cancel_and_open_partial() {
        let order_history = serde_json::json!([
            {"orderId": "o9", "orderLinkId": "c_fill", "orderStatus": "Filled", "updatedTime": "7"},
            {"orderId": "o10", "orderLinkId": "c_cancel", "orderStatus": "Cancelled", "cancelType": "CancelByUser", "updatedTime": "8"},
            {"orderId": "o11", "orderLinkId": "c_rej", "orderStatus": "Rejected", "rejectReason": "EC_NoImmediateQtyToFill", "updatedTime": "9"}
        ]);
        // exec on o9 (filled), plus a fill on o99 (a still-open order, NOT in order_history)
        let exec_history = serde_json::json!([
            {"execType": "Trade", "orderId": "o9", "orderLinkId": "c_fill", "execId": "e1", "execPrice": "50000", "execQty": "1.0", "side": "Buy", "execFee": "0.1", "feeCurrency": "USDT", "isMaker": false, "execTime": "5"},
            {"execType": "Trade", "orderId": "o99", "orderLinkId": "c_open", "execId": "e2", "execPrice": "51000", "execQty": "0.5", "side": "Sell", "execFee": "0.05", "isMaker": true, "execTime": "6"}
        ]);
        let evs = map_bybit_history(&order_history, &exec_history, "bybit", "BTCUSDT");
        assert_eq!(
            kinds(&evs),
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
            assert_eq!(fill.last_px, 50000.0);
            assert_eq!(fill.side, 1);
            assert_eq!(fill.commission, 0.1);
            assert_eq!(fill.commission_asset, "USDT");
        } else {
            panic!("first event must be the bare Fill");
        }
    }

    #[test]
    fn skips_non_trade_exec_rows() {
        let order_history = serde_json::json!([]);
        let exec_history = serde_json::json!([
            {"execType": "Funding", "orderId": "x", "orderLinkId": "c", "execId": "f1", "execTime": "1"},
            {"execType": "BustTrade", "orderId": "y", "orderLinkId": "c", "execId": "b1", "execTime": "2"}
        ]);
        assert!(map_bybit_history(&order_history, &exec_history, "bybit", "BTCUSDT").is_empty());
    }
}
