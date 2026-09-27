//! OKX V5 REST orders/fills-history → events, for the audit-A3 post-reconnect resync.
//! Mirrors `binance/history.rs`. Quirks that keep a replayed fill byte-identical to the live WS
//! fold: `fillSz` is in CONTRACTS → `last_qty = fillSz * ct_val` (base units); the REST fill fee
//! field is `fee` (SIGNED, negative = charge) → `commission = -fee`; the timestamp field is `ts`
//! (not the WS `fillTime`). The fill row's `clOrdId` is "0" for our hex coids, so the coid is taken
//! from the ORDER row and fills are joined by `ordId`. OKX order history has no expired/rejected
//! terminal — only `canceled`/`mmp_canceled` → `OrderCanceled`.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`). Deliberately NOT `get_str`, which carries `json_str`'s `Bool` arm — see
// `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderFilled, OrderPartiallyFilled, TradeId,
};

/// Replay recent OKX history (audit A3). `orders_history` = `/api/v5/trade/orders-history` data
/// (open + recently-closed); `fills_history` = `/api/v5/trade/fills-history` data. `ct_val` converts
/// contract fill size → base qty. Emits per order its fills + (canceled → OrderCanceled).
pub fn map_okx_history(
    orders_history: &Value,
    fills_history: &Value,
    venue: &str,
    symbol: &str,
    ct_val: f64,
) -> Vec<Event> {
    let orders = orders_history.as_array().cloned().unwrap_or_default();
    let fills = fills_history.as_array().cloned().unwrap_or_default();

    // Group fills by ordId; sort each group by (ts, tradeId) ascending (fills-history is newest-first).
    let mut by_order: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for fl in &fills {
        by_order.entry(s(fl, "ordId")).or_default().push(fl);
    }
    for v in by_order.values_mut() {
        v.sort_by(|a, b| i(a, "ts").cmp(&i(b, "ts")).then(s(a, "tradeId").cmp(&s(b, "tradeId"))));
    }

    let mut events = Vec::new();
    for o in &orders {
        let ordid = s(o, "ordId");
        let coid = s(o, "clOrdId"); // authoritative (fill rows carry "0" for hex coids)
        let state = o.get("state").and_then(|x| x.as_str()).unwrap_or("");
        let filled = state == "filled";

        if let Some(group) = by_order.get(&ordid) {
            let n = group.len();
            for (idx, fl) in group.iter().enumerate() {
                let mark = match fl.get("fillMarkPx") {
                    Some(Value::Number(n)) => n.as_f64(),
                    Some(Value::String(x)) if !x.is_empty() => x.parse().ok(),
                    _ => None,
                };
                // `tradeId` is the dedup key the whole replay rests on: this mapper re-emits history
                // and relies on the core's `seen_trade_ids`/`seen_fsm_trade_ids` to drop what was
                // already folded, so an id-less row is not replayed-once-more but folded a SECOND
                // time — double-booking commission and realized PnL against the live fill it
                // overlaps. SKIP it. ⚠ That is allowed to cost the wrap: skipping the LAST fill of a
                // `filled` order means no `OrderFilled` here, leaving the order non-terminal until
                // the live stream or a later pass terminalizes it. A stuck-open order is recoverable
                // (confirm-grace watchdog, recon's `MissingTerminal`); a double-booked fill is money
                // that is already wrong. OKX documents `tradeId` on every fills-history row, so this
                // is a malformed-response path, not a shape the venue has.
                let Ok(trade_id) = TradeId::new(s(fl, "tradeId")) else {
                    tracing::warn!(
                        venue,
                        client_order_id = %coid,
                        "fills-history row carries no `tradeId` — skipping the replayed fill; with \
                         no dedup key the core cannot tell it from the live fill it overlaps"
                    );
                    continue;
                };
                let fill = FillEvent {
                    trade_id,
                    client_order_id: coid.clone(),
                    venue: venue.to_string().into(),
                    symbol: match fl.get("instId") {
                        Some(Value::String(x)) if !x.is_empty() => x.clone().into(),
                        _ => symbol.to_string().into(),
                    },
                    side: if fl.get("side").and_then(|x| x.as_str()) == Some("buy") {
                        1
                    } else {
                        -1
                    },
                    last_qty: f(fl, "fillSz") * ct_val, // CONTRACTS → base
                    last_px: f(fl, "fillPx"),
                    commission: -f(fl, "fee"), // SIGNED fee, negated to a positive cost
                    commission_asset: s(fl, "feeCcy").into(),
                    liquidity_side: if fl.get("execType").and_then(|x| x.as_str()) == Some("M") {
                        LiquiditySide::Maker
                    } else {
                        LiquiditySide::Taker
                    },
                    ts: i(fl, "ts"),
                    mark_price: mark,
                    position_side: "BOTH".to_string().into(), // SWAP net mode
                };
                let ts = fill.ts;
                let wrap = if filled && idx + 1 == n {
                    Event::OrderFilled(OrderFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts,
                    })
                } else {
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts,
                    })
                };
                events.push(Event::Fill(fill));
                events.push(wrap);
            }
        }
        // Only canceled is a non-fill terminal on OKX (no expired/rejected in order history).
        if state == "canceled" || state == "mmp_canceled" {
            events.push(Event::OrderCanceled(OrderCanceled {
                client_order_id: coid,
                reason: "reconcile: order closed during reconnect gap".to_string().into(),
                ts: i(o, "uTime"),
            }));
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
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn replays_gap_fill_and_cancel_with_contracts_to_base() {
        // ct_val 0.01: a fillSz of 3 contracts = 0.03 base.
        let orders = serde_json::json!([
            {"ordId": "o9", "clOrdId": "deadbeef1", "state": "filled", "uTime": "7"},
            {"ordId": "o10", "clOrdId": "deadbeef2", "state": "canceled", "uTime": "8"},
            {"ordId": "o11", "clOrdId": "deadbeef3", "state": "live", "uTime": "6"}
        ]);
        // note fills carry clOrdId "0" (our hex coids aren't int64) — coid MUST come from the order row.
        let fills = serde_json::json!([
            {"tradeId": "t1", "ordId": "o9", "clOrdId": "0", "fillPx": "50000", "fillSz": "3", "side": "buy", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "ts": "5"},
            {"tradeId": "t2", "ordId": "o11", "clOrdId": "0", "fillPx": "51000", "fillSz": "1", "side": "sell", "fee": "-0.02", "execType": "M", "ts": "6"}
        ]);
        let evs = map_okx_history(&orders, &fills, "okx", "BTC-USDT-SWAP", 0.01);
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:deadbeef1:t1".to_string(), // coid from ORDER row, not the fill's "0"
                "OrderFilled:deadbeef1".to_string(),
                "OrderCanceled:deadbeef2".to_string(),
                "Fill:deadbeef3:t2".to_string(), // still-open order (state live) → partial only
                "OrderPartiallyFilled:deadbeef3".to_string(),
            ]
        );
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.last_qty, 0.03, "3 contracts * 0.01 ct_val = 0.03 base");
            assert_eq!(fill.commission, 0.1, "SIGNED fee -0.1 negated to +0.1 cost");
            assert_eq!(fill.commission_asset, "USDT", "feeCcy surfaced");
            assert_eq!(fill.side, 1);
        } else {
            panic!("first event must be the bare Fill");
        }
    }
}
