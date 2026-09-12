//! Pure Deribit user.trades → vike event mapper. Exact port of `exec/deribit/mapper.py`.
//!
//! The channel is FILLS-ONLY (lifecycle is published synchronously by the order client),
//! so there is no lifecycle branch. Dual-publish per trade row. Terminality: state ==
//! "filled" is the SOLE signal (no cum/leaves fields). Commission: Deribit `fee` is
//! SIGNED (>0 taker charge, <0 maker rebate) — carried unchanged. Coin units, no
//! rescale. Options are one-way → position_side stays BOTH.
//!
//! COMBO executions (captured live 2026-07-19, testnet — `tests/deribit_combo_fill_probe.rs`;
//! fixture-pinned in `tests/offline/combo_fill_fixtures.rs`): ONE combo fill arrives as N LEG rows (leg
//! `instrument_name`, real leg price/fee, `combo_id` + `combo_trade_id` set) PLUS one aggregate
//! row under the COMBO instrument itself (the NET price; its `trade_id` equals the legs'
//! `combo_trade_id`; NO `combo_id` field) — every row carrying the combo order's `label` and its
//! own `state`. This mapper deliberately stays row-shape-agnostic and dual-publishes each row
//! verbatim: the ENGINE classifies. Leg rows fold into `Account` (per-leg position truth —
//! admitted via `extra_symbols` by the combo lowering, and by coid ownership regardless), while
//! the combo-instrument net print matches neither the order's (empty) `symbol` nor a leg and is
//! skipped by `ExecutionEngine::owns_fill_symbol` — the venue books NO position under the combo
//! id (probe evidence), so folding the net row would mint a phantom position and double-count.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`), byte-identical to the local `fn s` this module used to declare. NOT
// `get_str`, which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{Event, FillEvent, TradeId};

pub fn map_deribit_trade(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let coid = s(item, "label");
    let ts = match item.get("timestamp") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(x)) => x.parse::<i64>().unwrap_or(0),
        _ => 0,
    };
    // `trade_id` is the fill DEDUP key and Deribit documents it on every `user.trades` row —
    // including both COMBO row shapes (the leg rows carry their own, and the aggregate combo row's
    // equals the legs' `combo_trade_id`; see this module's doc). An absent/empty one is therefore a
    // malformed frame, not a shape the venue has. DROP the whole row: with no id the engine's
    // `seen_trade_ids` guard cannot collapse it against the same trade arriving from
    // `private/get_user_trades_by_instrument` on a resync, so commission and realized PnL would be
    // booked TWICE. The bare `Event::Fill` and the wrap `vike_bridge_core::terminal_events` mints
    // from it stand or fall TOGETHER: publishing the wrap alone would terminalize the FSM for a fill
    // the Account never folded. Nothing is synthesized — this channel is fills-only and carries no
    // other unique key, and a clock/counter id differs on the second run, defeating dedup outright.
    let Ok(trade_id) = TradeId::new(s(item, "trade_id")) else {
        tracing::warn!(
            venue,
            symbol = %s(item, "instrument_name"),
            client_order_id = %coid,
            "user.trades row carries no `trade_id` — dropping the fill and its wrap; an \
             un-dedupable fill double-books commission and realized PnL when a resync replays it"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        client_order_id: coid.clone(),
        venue: venue.to_string().into(),
        symbol: match item.get("instrument_name") {
            Some(Value::String(x)) => x.clone().into(),
            _ => symbol.to_string().into(),
        },
        side: if item.get("direction").and_then(|d| d.as_str()) == Some("buy") { 1 } else { -1 },
        last_qty: f(item, "amount"), // COIN units (options) — no rescale
        last_px: f(item, "price"),
        commission: f(item, "fee"), // SIGNED, kept as-is
        commission_asset: s(item, "fee_currency").into(),
        liquidity_side: if item.get("liquidity").and_then(|l| l.as_str()) == Some("M") {
            LiquiditySide::Maker
        } else {
            LiquiditySide::Taker
        },
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    let is_filled = s(item, "state") == "filled";
    vike_bridge_core::terminal_events(coid, fill, ts, is_filled)
}

/// method=='subscription' + channel startswith 'user.trades' + list data → rows; else [].
pub fn map_deribit_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    if !frame.is_object() {
        return Vec::new();
    }
    if frame.get("method").and_then(|m| m.as_str()) != Some("subscription") {
        return Vec::new();
    }
    let Some(params) = frame.get("params").filter(|p| p.is_object()) else {
        return Vec::new();
    };
    let channel = params.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    if !channel.starts_with("user.trades") {
        return Vec::new();
    }
    let Some(data) = params.get("data").and_then(|d| d.as_array()) else {
        return Vec::new(); // other user.* channels send a dict
    };
    let mut events = Vec::new();
    for item in data {
        events.extend(map_deribit_trade(item, venue, symbol));
    }
    events
}
