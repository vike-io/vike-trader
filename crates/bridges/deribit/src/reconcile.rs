//! Pure Deribit private-getter results → ReconcileSnapshot. Exact port of
//! `exec/deribit/reconcile.py`.
//!
//! `size` is ALREADY SIGNED (negative=short); `direction` is read ONLY for the flat
//! guard — re-signing by it would invert every short. Coin units (no ct rescale).
//! Options are one-way → position_sides stays empty. Empty-label orders are externally
//! placed and SKIPPED; `price` may be the literal string "market_price" (→ None);
//! filled_amount>0 seeds PARTIALLY_FILLED so a later WS fill transitions legally.

use serde_json::Value;
use vike_bridge_core::json::{get_f64 as num, json_num};
use vike_exec::{ManagedOrder, OrderStatus, ReconcileSnapshot};
use vike_model::OrderRequest;

const VENUE: &str = "deribit";

/// Deribit `price` is numeric for limits but "market_price" for open trigger markets.
fn numeric_or_none(v: Option<&Value>) -> Option<f64> {
    v.and_then(json_num)
}

fn build_positions(positions: &Value, symbol: &str) -> Option<(f64, f64, f64)> {
    for row in positions.as_array().unwrap_or(&vec![]) {
        if row.get("instrument_name").and_then(|n| n.as_str()) != Some(symbol) {
            continue;
        }
        let direction = row.get("direction").and_then(|d| d.as_str()).unwrap_or("");
        let size = num(row, "size"); // ALREADY signed — never re-sign by direction
        if direction == "zero" || size == 0.0 {
            continue;
        }
        return Some((size, num(row, "average_price"), num(row, "mark_price")));
    }
    None
}

fn build_orders(orders: &Value, symbol: &str) -> Vec<ManagedOrder> {
    let mut out = Vec::new();
    for row in orders.as_array().unwrap_or(&vec![]) {
        if row.get("order_state").and_then(|s| s.as_str()) != Some("open") {
            continue;
        }
        let label = row.get("label").and_then(|l| l.as_str()).unwrap_or("");
        if label.is_empty() {
            continue; // externally-placed (no vike coid) — not ours to manage
        }
        let side =
            if row.get("direction").and_then(|d| d.as_str()) == Some("buy") { 1 } else { -1 };
        let filled = num(row, "filled_amount");
        let request: OrderRequest = serde_json::from_value(serde_json::json!({
            "client_order_id": label, "venue": VENUE, "symbol": symbol, "side": side,
            "qty": num(row, "amount"),
            "order_type": row.get("order_type").and_then(|t| t.as_str()).unwrap_or("limit"),
            "price": numeric_or_none(row.get("price")),
        }))
        .expect("static shape");
        let mut mo = ManagedOrder::new(request);
        mo.status = if filled > 0.0 { OrderStatus::PartiallyFilled } else { OrderStatus::Accepted };
        mo.venue_order_id = Some(match row.get("order_id") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        });
        mo.filled_qty = filled;
        mo.avg_fill_px = num(row, "average_price");
        out.push(mo);
    }
    out
}

pub fn build_reconcile_snapshot(
    positions_result: &Value,
    orders_result: &Value,
    symbol: &str,
) -> ReconcileSnapshot {
    let open_orders = build_orders(orders_result, symbol);
    match build_positions(positions_result, symbol) {
        // `position_margin` empty on both arms: deribit is cross-only (its portfolio margin is a
        // maintenance-rate variation WITHIN cross — see `vike_model::venue_margin_support`), so there is
        // no mode to report; apply_snapshot carries priors forward (default Cross).
        None => ReconcileSnapshot {
            positions: vec![(symbol.to_string(), 0.0)],
            open_orders,
            position_avg_px: vec![(symbol.to_string(), 0.0)],
            position_mark_px: vec![(symbol.to_string(), 0.0)],
            position_sides: Vec::new(),
            balance: 0.0,
            position_margin: Vec::new(),
        },
        Some((signed, avg, mark)) => ReconcileSnapshot {
            positions: vec![(symbol.to_string(), signed)],
            open_orders,
            position_avg_px: vec![(symbol.to_string(), avg)],
            position_mark_px: vec![(symbol.to_string(), mark)],
            position_sides: Vec::new(),
            balance: 0.0,
            position_margin: Vec::new(),
        },
    }
}
