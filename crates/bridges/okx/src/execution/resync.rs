//! OKX V5 REST orders/fills-history → events, for the audit-A3 post-reconnect resync.
//! Mirrors `binance/history.rs`. Quirks that keep a replayed fill byte-identical to the live WS
//! fold: `fillSz` is in CONTRACTS → `last_qty = fillSz * ct_val` (base units); the REST fill fee
//! field is `fee` (SIGNED, negative = charge) → `commission = -fee`; the timestamp field is `ts`
//! (not the WS `fillTime`). The fill row's `clOrdId` is "0" for our hex order client ids, so the order client id is taken
//! from the ORDER row and fills are joined by `ordId`. OKX order history has no expired/rejected
//! terminal — only `canceled`/`mmp_canceled` → `OrderCanceled`. The ORDER row of a fired stop's child
//! carries the stop's client id in `algoClOrdId`, which is the id its fills replay under
//! (`crate::execution::event_mapper::row_order_client_id`); [`map_okx_algo_history`] replays the STOPS that ended in the gap. Only an
//! ordinary trade replays as a fill (`crate::reconcile::is_ordinary_trade`, the reconcile read's rule): a liquidation
//! or ADL row is `PositionLiquidated` on the stream, and one the stream missed in the gap is left to reconcile's position
//! read, never replayed as a fill.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`). Deliberately NOT `get_str`, which carries `json_str`'s `Bool` arm — see
// `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_fee_f64, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{Event, FillEvent, OrderCanceled, TradeId};

/// A fills-history answer for an instrument OTHER than the mounted one, with each row's `tradeId`
/// replaced by the scoped id the user-data stream spells for it (`INSTID:tradeId`,
/// `crate::execution::event_mapper`'s `scoped_trade_id`). OKX's `tradeId` is a per-instrument sequence and the
/// engine's fill ledger keys on the bare string, so the stream scopes a foreign fill; a replay that spelled
/// it bare would fold a fill the stream already folded a second time (two ids for one fill) or drop a
/// genuine one as a repeat of another instrument's. A row with no `tradeId` is left as it is: the mapper
/// skips it. Anything that is not an array comes back as it came.
pub fn scope_fill_ids(fills_history: &Value, instrument: &str) -> Value {
    let mut out = fills_history.clone();
    if let Some(rows) = out.as_array_mut() {
        for row in rows {
            let id = row.get("tradeId").and_then(Value::as_str).filter(|id| !id.is_empty());
            if let Some(id) = id.map(|id| crate::execution::event_mapper::scoped_id(instrument, id))
            {
                row["tradeId"] = Value::String(id);
            }
        }
    }
    out
}

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

    // Group fills by ordId; sort each group by (ts, tradeId) ascending (fills-history is newest-first). Only an ordinary
    // trade is a fill: a liquidation or ADL row is `PositionLiquidated` on the stream, never a fill
    // (`crate::reconcile::is_ordinary_trade`, the rule the reconcile read shares).
    let mut by_order: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for fl in fills.iter().filter(|fl| crate::reconcile::is_ordinary_trade(fl)) {
        by_order.entry(s(fl, "ordId")).or_default().push(fl);
    }
    for v in by_order.values_mut() {
        v.sort_by(|a, b| i(a, "ts").cmp(&i(b, "ts")).then(s(a, "tradeId").cmp(&s(b, "tradeId"))));
    }

    let mut events = Vec::new();
    for o in &orders {
        let ordid = s(o, "ordId");
        // Authoritative (fill rows carry "0" for hex order client ids). A fired stop's child order is filed under
        // the STOP's id (`algoClOrdId`, see `row_order_client_id`), so its fills replay onto the stop.
        let order_client_id = crate::execution::event_mapper::row_order_client_id(o);
        let state = o.get("state").and_then(|x| x.as_str()).unwrap_or("");
        let filled = state == "filled";

        if let Some(group) = by_order.get(&ordid) {
            let n = group.len();
            for (index, fl) in group.iter().enumerate() {
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
                        order_client_id = %order_client_id,
                        "fills-history row carries no `tradeId` — skipping the replayed fill; with \
                         no dedup key the core cannot tell it from the live fill it overlaps"
                    );
                    continue;
                };
                let mut fill = FillEvent {
                    trade_id,
                    order_client_id: order_client_id.clone(),
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
                    last_price: f(fl, "fillPx"),
                    commission: -get_fee_f64(fl, "fee", venue), // SIGNED fee, negated to a positive cost
                    commission_asset: s(fl, "feeCcy").into(),
                    liquidity_side: LiquiditySide::from_maker(
                        fl.get("execType").and_then(|x| x.as_str()) == Some("M"),
                    ),
                    timestamp: i(fl, "ts"),
                    mark_price: mark,
                    position_side: vike_model::events::PositionSide::Both, // SWAP net mode
                };
                // a spot BUY paid in the base coin: the same netted fill the stream folded
                crate::instrument::net_spot_buy_fee(&mut fill);
                let ts = fill.timestamp;
                events.extend(vike_bridge_core::terminal_events(
                    order_client_id.clone(),
                    fill,
                    ts,
                    filled && index + 1 == n,
                ));
            }
        }
        // Only canceled is a non-fill terminal on OKX (no expired/rejected in order history).
        if state == "canceled" || state == "mmp_canceled" {
            events.push(Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: crate::execution::event_mapper::cancel_reason(
                    o,
                    "reconcile: order closed during reconnect gap".to_string(),
                )
                .into(),
                timestamp: i(o, "uTime"),
            }));
        }
    }
    events
}

/// `GET /api/v5/trade/orders-algo-history` rows (the audit-A3 resync of the ALGO socket) → the
/// terminal events of the stops that ended while that socket was down: a stop `canceled` or
/// `order_failed` replays as [`map_okx_algo_order`](crate::execution::event_mapper::map_okx_algo_order)'s
/// `OrderCanceled`. A row in any other state replays
/// nothing: `effective` is the child order's fills' to close (they replay through [`map_okx_history`]
/// under the stop's id), and a terminal row for a stop the book already closed changes nothing.
pub fn map_okx_algo_history(rows: &Value) -> Vec<Event> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            matches!(r.get("state").and_then(Value::as_str), Some("canceled" | "order_failed"))
        })
        .flat_map(crate::execution::event_mapper::map_okx_algo_order)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(fill) => format!("Fill:{}:{}", fill.order_client_id, fill.trade_id),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.order_client_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.order_client_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.order_client_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    /// A post-only order the venue ended for crossing replays with the word the stream gives it, so its
    /// typed reader needs no live frame; every other cancelled row keeps the gap's words.
    #[test]
    fn only_a_post_only_cross_replays_with_the_streams_word() {
        let gap = "reconcile: order closed during reconnect gap";
        let orders = serde_json::json!([
            {"ordId": "o1", "clOrdId": "c1", "state": "canceled", "cancelSource": "31", "uTime": "8"},
            {"ordId": "o2", "clOrdId": "c2", "state": "canceled", "cancelSource": "1", "uTime": "8"},
            {"ordId": "o3", "clOrdId": "c3", "state": "mmp_canceled", "cancelSource": "20", "uTime": "8"},
            {"ordId": "o4", "clOrdId": "c4", "state": "canceled", "uTime": "8"}
        ]);
        let cancels: Vec<(String, bool)> =
            map_okx_history(&orders, &serde_json::json!([]), "okx", "BTC-USDT-SWAP", 0.01)
                .iter()
                .map(|e| match e {
                    Event::OrderCanceled(c) => (c.reason.to_string(), c.is_post_only_cross()),
                    other => panic!("{other:?}"),
                })
                .collect();
        let want = |reason: &str, typed| (reason.to_string(), typed);
        assert_eq!(
            cancels,
            [
                want(vike_model::events::POST_ONLY_CANCEL_REASON, true),
                want(gap, false),
                want(gap, false),
                want(gap, false)
            ]
        );
    }

    #[test]
    fn replays_gap_fill_and_cancel_with_contracts_to_base() {
        // ct_val 0.01: a fillSz of 3 contracts = 0.03 base.
        let orders = serde_json::json!([
            {"ordId": "o9", "clOrdId": "deadbeef1", "state": "filled", "uTime": "7"},
            {"ordId": "o10", "clOrdId": "deadbeef2", "state": "canceled", "uTime": "8"},
            {"ordId": "o11", "clOrdId": "deadbeef3", "state": "live", "uTime": "6"}
        ]);
        // note fills carry clOrdId "0" (our hex order client ids aren't int64) — order client id MUST come from the order row.
        let fills = serde_json::json!([
            {"tradeId": "t1", "ordId": "o9", "clOrdId": "0", "fillPx": "50000", "fillSz": "3", "side": "buy", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "subType": "1", "ts": "5"},
            {"tradeId": "t2", "ordId": "o11", "clOrdId": "0", "fillPx": "51000", "fillSz": "1", "side": "sell", "fee": "-0.02", "execType": "M", "subType": "2", "ts": "6"}
        ]);
        let evs = map_okx_history(&orders, &fills, "okx", "BTC-USDT-SWAP", 0.01);
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:deadbeef1:t1".to_string(), // order client id from ORDER row, not the fill's "0"
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

    /// One order's fills-history rows: an ordinary close (`subType` 6), a partial liquidation (100), a liquidation (104)
    /// and an ADL close (125).
    fn liquidated_order_rows() -> Vec<serde_json::Value> {
        let row = |id: &str, sub: &str, ts: &str| {
            serde_json::json!({"tradeId": id, "ordId": "o9", "clOrdId": "0", "fillPx": "50000", "fillSz": "1",
                "side": "sell", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "subType": sub, "ts": ts})
        };
        vec![
            row("t1", "6", "1"),
            row("-2", "100", "2"),
            row("-3", "104", "3"),
            row("-4", "125", "4"),
        ]
    }

    /// **A liquidation or ADL row in a replay is not a fill**: the stream books it as `PositionLiquidated`, never a fill,
    /// so a replayed fill would close the position a second time. Its ordinary sibling still replays, and its wrap is the
    /// order's terminal (the replay keeps the last ORDINARY fill as the order's last).
    #[test]
    fn a_liquidation_row_in_a_replay_is_not_a_fill() {
        let orders = serde_json::json!([{"ordId": "o9", "clOrdId": "deadbeef1", "state": "filled", "uTime": "7"}]);
        let rows = serde_json::Value::Array(liquidated_order_rows());
        let evs = map_okx_history(&orders, &rows, "okx", "BTC-USDT-SWAP", 0.01);
        assert_eq!(
            kinds(&evs),
            ["Fill:deadbeef1:t1".to_string(), "OrderFilled:deadbeef1".to_string()],
            "only the ordinary close replays: {evs:?}"
        );
    }

    /// **A row with no `subType` is not replayed** (OKX documents it on every row): without it a liquidation cannot be
    /// told from a fill.
    #[test]
    fn a_row_with_no_sub_type_is_not_replayed() {
        let orders = serde_json::json!([{"ordId": "o9", "clOrdId": "deadbeef1", "state": "live", "uTime": "7"}]);
        let untyped = serde_json::json!({"tradeId": "t9", "ordId": "o9", "clOrdId": "0", "fillPx": "50000",
            "fillSz": "1", "side": "sell", "fee": "-0.1", "feeCcy": "USDT", "execType": "T", "ts": "9"});
        let mut rows = liquidated_order_rows();
        rows.push(untyped);
        let evs =
            map_okx_history(&orders, &serde_json::Value::Array(rows), "okx", "BTC-USDT-SWAP", 0.01);
        let ids: Vec<String> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["t1"], "{evs:?}");
    }
}
