//! Binance-grammar REST order-history → events, for the audit-A3 post-reconnect resync. Shared by
//! vike-binance (`crate::history`) and vike-aster (`vike_aster::history`) — Aster's order/trade JSON
//! is a Binance fork (identical field names/shapes), so both venues call straight into this, each
//! passing its own `venue`.
//!
//! On a WS reconnect a terminal (fill/cancel/expire/reject) that landed during the reconnect
//! window is lost — the private WS does not replay. The resync supervisor fetches
//! `allOrders` (recent order states) + `myTrades`/`userTrades` (recent fills) and REPLAYS them
//! through the normal event lane; the core's `trade_id` dedup (`seen_trade_ids`/`seen_fsm_trade_ids`)
//! drops everything already seen and applies only the gap events. Mirrors the WS mapper's
//! dual-publish (bare `FillEvent` + `OrderPartiallyFilled`/`OrderFilled` wrap) so a replayed fill
//! folds byte-identically to a live one.
//!
//! **Broker-prefix strip (unified cross-venue attribution, task 6, INBOUND half).** `clientOrderId`
//! on an `allOrders` row is the SAME venue-stored id the WS mapper decodes, so it is passed through
//! [`crate::family::order_map::strip_broker_coid_prefix`] here too — see
//! `crate::family::event_mapper`'s module doc for the full rationale.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`; the side/maker booleans are read via `crate::family::recon`'s union readers).
// Deliberately NOT `get_str`, which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderExpired, OrderFilled, OrderPartiallyFilled,
    OrderRejected, TradeId,
};

/// A history trade row's `id` as a [`TradeId`], or `None` for a row that carries none.
///
/// Both replay mappers below skip such a row (see [`map_history`]'s doc for the trade-off). It is a
/// shared helper only so the two loops cannot drift on the verdict.
fn trade_id_of(t: &Value, venue: &str, coid: &str) -> Option<TradeId> {
    match TradeId::new(s(t, "id")) {
        Ok(id) => Some(id),
        Err(_) => {
            tracing::warn!(
                venue,
                client_order_id = %coid,
                "history trade row carries no `id` (tradeId) — skipping the replayed fill; with no \
                 dedup key the core's `seen_trade_ids` cannot tell it from the live fill it \
                 overlaps, so replaying it would double-book commission and realized PnL"
            );
            None
        }
    }
}

/// Replay recent spot order history as events (audit A3). `all_orders` is the
/// `GET /api/v3/allOrders` array, `my_trades` the spot account-trade array — Binance's
/// `GET /api/v3/myTrades` or Aster's `GET /api/v3/userTrades`, whose rows are futures-shaped and
/// are read through the family union grammar (see [`crate::family::recon::fill_side`]). Emits, per order:
/// its fills (bare `FillEvent` + `OrderPartiallyFilled`/`OrderFilled` wrap, mirroring the WS
/// mapper), then a non-fill terminal (`OrderCanceled`/`OrderExpired`/`OrderRejected`) if the order
/// ended that way. Still-open orders (`NEW`/live `PARTIALLY_FILLED`) emit only their fills. The
/// core dedups the overlap; only gap events apply.
///
/// A trade row with no `id` is SKIPPED (see [`trade_id_of`]). ⚠ That is deliberately allowed to cost
/// the wrap: skipping the LAST fill of a `FILLED` order means this replay emits no `OrderFilled`, so
/// the order stays non-terminal in the FSM until the live stream or a later pass terminalizes it.
/// A stuck-open order is recoverable (the confirm-grace watchdog pokes it, and recon reports it as
/// `MissingTerminal`); a double-booked fill is money that is already wrong. Binance/Aster document
/// `id` on every `myTrades`/`userTrades` row, so this is a malformed-response path, not a shape the
/// venue has.
///
/// Re-exported as `map_binance_history` / `map_aster_history` by the two venues.
pub fn map_history(all_orders: &Value, my_trades: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let orders = all_orders.as_array().cloned().unwrap_or_default();
    let trades = my_trades.as_array().cloned().unwrap_or_default();

    // Group trades by venue orderId, sorted by trade id ascending (fill order).
    let mut trades_by_order: std::collections::HashMap<i64, Vec<&Value>> =
        std::collections::HashMap::new();
    for t in &trades {
        trades_by_order.entry(i(t, "orderId")).or_default().push(t);
    }
    for v in trades_by_order.values_mut() {
        v.sort_by_key(|t| i(t, "id"));
    }

    let mut events = Vec::new();
    for o in &orders {
        let oid = i(o, "orderId");
        let coid =
            crate::family::order_map::strip_broker_coid_prefix(&s(o, "clientOrderId")).to_string();
        let status = s(o, "status");
        let update_ts = i(o, "updateTime");

        // Replay this order's fills as bare FillEvent + wrap (mirrors the WS mapper's dual-publish).
        if let Some(order_trades) = trades_by_order.get(&oid) {
            let n = order_trades.len();
            for (idx, t) in order_trades.iter().enumerate() {
                let Some(trade_id) = trade_id_of(t, venue, &coid) else { continue };
                let fill = FillEvent {
                    trade_id,
                    client_order_id: coid.clone(),
                    venue: venue.to_string().into(),
                    symbol: symbol.to_string().into(),
                    // The family UNION grammar, NOT `isBuyer`/`isMaker` alone: Aster's spot
                    // `/api/v3/userTrades` rows are futures-shaped (`side`/`maker`), and this
                    // mapper replays them for the aster spot lane too. See
                    // [`crate::family::recon::fill_side`] for why reading one spelling silently
                    // signed every aster spot fill as a SELL.
                    side: crate::family::recon::fill_side(t),
                    last_qty: f(t, "qty"),
                    last_px: f(t, "price"),
                    commission: f(t, "commission"),
                    commission_asset: s(t, "commissionAsset").into(),
                    liquidity_side: if crate::family::recon::fill_is_maker(t) {
                        LiquiditySide::Maker
                    } else {
                        LiquiditySide::Taker
                    },
                    ts: i(t, "time"),
                    mark_price: None,
                    position_side: "BOTH".to_string().into(),
                };
                // OrderFilled only on the LAST trade of a fully-FILLED order; else PartiallyFilled.
                let wrap = if status == "FILLED" && idx + 1 == n {
                    Event::OrderFilled(OrderFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts: fill.ts,
                    })
                } else {
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts: fill.ts,
                    })
                };
                events.push(Event::Fill(fill));
                events.push(wrap);
            }
        }

        // Non-fill terminals (a FILLED order is already terminalized by its OrderFilled wrap above).
        match status.as_str() {
            "CANCELED" | "PENDING_CANCEL" => events.push(Event::OrderCanceled(OrderCanceled {
                client_order_id: coid,
                reason: "reconcile: order closed during reconnect gap".to_string().into(),
                ts: update_ts,
            })),
            "EXPIRED" => events
                .push(Event::OrderExpired(OrderExpired { client_order_id: coid, ts: update_ts })),
            "REJECTED" => events.push(Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: "reconcile: order rejected during reconnect gap".to_string().into(),
                ts: update_ts,
            })),
            _ => {}
        }
    }
    events
}

/// Replay recent USDⓈ-M futures history (audit A3), the perp twin of [`map_history`].
/// `all_orders` = fapi `allOrders`, `user_trades` = fapi `userTrades`. fapi trade fields differ
/// from spot: `side` (BUY/SELL, not `isBuyer`), `maker` (not `isMaker`), `positionSide`
/// (BOTH/LONG/SHORT). Mirrors the WS [`super::perp_mapper::map_perp`] fill conventions so a
/// replayed fill folds byte-identically.
///
/// Re-exported as `map_binance_perp_history` / `map_aster_perp_history` by the two venues.
pub fn map_perp_history(
    all_orders: &Value,
    user_trades: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
    let orders = all_orders.as_array().cloned().unwrap_or_default();
    let trades = user_trades.as_array().cloned().unwrap_or_default();

    let mut trades_by_order: std::collections::HashMap<i64, Vec<&Value>> =
        std::collections::HashMap::new();
    for t in &trades {
        trades_by_order.entry(i(t, "orderId")).or_default().push(t);
    }
    for v in trades_by_order.values_mut() {
        v.sort_by_key(|t| i(t, "id"));
    }

    let mut events = Vec::new();
    for o in &orders {
        let oid = i(o, "orderId");
        let coid =
            crate::family::order_map::strip_broker_coid_prefix(&s(o, "clientOrderId")).to_string();
        let status = s(o, "status");
        let update_ts = i(o, "updateTime");

        if let Some(order_trades) = trades_by_order.get(&oid) {
            let n = order_trades.len();
            for (idx, t) in order_trades.iter().enumerate() {
                // Same skip-on-missing-`id` verdict as the spot twin — see [`trade_id_of`].
                let Some(trade_id) = trade_id_of(t, venue, &coid) else { continue };
                let fill = FillEvent {
                    trade_id,
                    client_order_id: coid.clone(),
                    venue: venue.to_string().into(),
                    symbol: symbol.to_string().into(),
                    // Same family union as the spot twin above — byte-identical here, since a
                    // fapi row always carries `side`/`maker` and never the `is*` spelling.
                    side: crate::family::recon::fill_side(t),
                    last_qty: f(t, "qty"),
                    last_px: f(t, "price"),
                    commission: f(t, "commission"),
                    commission_asset: s(t, "commissionAsset").into(),
                    liquidity_side: if crate::family::recon::fill_is_maker(t) {
                        LiquiditySide::Maker
                    } else {
                        LiquiditySide::Taker
                    },
                    ts: i(t, "time"),
                    mark_price: None,
                    position_side: match t.get("positionSide") {
                        Some(Value::String(x)) if !x.is_empty() => x.clone().into(),
                        _ => "BOTH".to_string().into(),
                    },
                };
                let wrap = if status == "FILLED" && idx + 1 == n {
                    Event::OrderFilled(OrderFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts: fill.ts,
                    })
                } else {
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: coid.clone(),
                        fill: fill.clone(),
                        ts: fill.ts,
                    })
                };
                events.push(Event::Fill(fill));
                events.push(wrap);
            }
        }
        match status.as_str() {
            "CANCELED" | "PENDING_CANCEL" => events.push(Event::OrderCanceled(OrderCanceled {
                client_order_id: coid,
                reason: "reconcile: order closed during reconnect gap".to_string().into(),
                ts: update_ts,
            })),
            "EXPIRED" => events
                .push(Event::OrderExpired(OrderExpired { client_order_id: coid, ts: update_ts })),
            "REJECTED" => events.push(Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: "reconcile: order rejected during reconnect gap".to_string().into(),
                ts: update_ts,
            })),
            _ => {}
        }
    }
    events
}
