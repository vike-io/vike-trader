//! Pure Deribit user.trades → vike event mapper. Exact port of `exec/deribit/mapper.py`.
//!
//! The `user.trades` channel is FILLS-ONLY (lifecycle is published synchronously by the order
//! client), so there is no lifecycle branch in [`map_deribit_trade`]; the one lifecycle the client
//! cannot publish, a STOP the venue ends or fires on its own, rides `user.orders` and is
//! [`map_deribit_order_row`]'s. Dual-publish per trade row. Terminality: state ==
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
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str_boolless as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderTriggered, TradeId};

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
        position_side: vike_model::events::PositionSide::Both,
    };
    let is_filled = s(item, "state") == "filled";
    vike_bridge_core::terminal_events(coid, fill, ts, is_filled)
}

/// The `order_type`s the bridge places as a venue-held TRIGGER order (`client::order_params_on`):
/// the stop's own row carries one of these, the order it spawns when it fires does not.
const TRIGGER_ORDER_TYPES: [&str; 4] = ["stop_market", "stop_limit", "take_market", "take_limit"];

/// One `user.orders` row → the end (or the firing) of a STOP, under the stop's client order id.
///
/// The `user.trades` channel above is fills-only, so a stop the venue ends WITHOUT a fill (cancelled
/// by someone else, or fired into an order the venue then refused) was never announced and the node
/// kept it `Accepted` for good. This is the other half. It looks at two kinds of row only, because
/// everything else already has its events (an ordinary order's accept and cancel are the order
/// client's synchronous answers, its fills are the channel above, and the A3 replay recovers a
/// gap):
///
/// * a TRIGGER row (`order_type` in `TRIGGER_ORDER_TYPES`: the four kinds the bridge places);
/// * the CHILD a fired stop spawns (`trigger_order_id` names the stop), which carries the stop's
///   `label` (MEASURED on the testnet 2026-10-26: its fill and `get_order_state_by_label` show it
///   under the coid).
///
/// | row | event |
/// |---|---|
/// | trigger row, `order_state` `cancelled`, `triggered` not true | `OrderCanceled` (the stop was cancelled while it waited) |
/// | child row, `order_state` `cancelled` | `OrderCanceled` (a stop-limit's resting limit cancelled, or a market child the venue ended) |
/// | trigger or child row, `order_state` `rejected` | `OrderCanceled` carrying `reject_reason`: the trigger fired and the venue refused the order it placed (a reduce-only stop on a flat position), so nothing rests and nothing filled. `OrderCanceled`, not `OrderRejected`: the order FSM allows a reject only before the accept, and a stop is always accepted first |
/// | trigger row, `order_state` `triggered`, or `triggered` true on a row that is not ended | `OrderTriggered` (the stop fired; its fill, if any, books on the stop by `label`) |
/// | anything else (`untriggered`, `open`, `filled`, a row with no `label`) | nothing: the submit's reply already accepted it, and a fill is the `user.trades` channel's |
///
/// ⚠ A TRIGGER row that is `cancelled` while `triggered` is true maps to nothing, on purpose: if the
/// venue closes a stop's own row as it fires, a terminal here would cancel an order about to fill and
/// the fill's `OrderFilled` would then be refused by the FSM. Whether it does is UNMEASURED (no
/// captured `user.orders` frame is in this tree: the field names are the documented ones);
/// the 0.1.97 kit's Deribit stop drill (B10) records the raw frames.
///
/// A row the node's own cancel already ended repeats a terminal the FSM and the gatewayhub's
/// `OnceMemory` drop; so does the A3 replay's (`crate::history::map_deribit_history`) for a child that
/// shows in the order history: one fact, one record, downstream of this mapper.
pub fn map_deribit_order_row(row: &Value) -> Vec<Event> {
    let coid = s(row, "label");
    if coid.is_empty() {
        return Vec::new();
    }
    let trigger_row = TRIGGER_ORDER_TYPES.contains(&s(row, "order_type").as_str());
    let child = !s(row, "trigger_order_id").is_empty();
    if !trigger_row && !child {
        return Vec::new();
    }
    let fired = row.get("triggered").and_then(Value::as_bool) == Some(true);
    let ts = i(row, "last_update_timestamp");
    let triggered = |coid: &str, ts| {
        Event::OrderTriggered(OrderTriggered { client_order_id: coid.to_string(), ts })
    };
    let whose = if child { "the order a stop fired" } else { "the stop" };
    let ended = |word: &str, why: String| {
        let why = if why.is_empty() { "no reason given".to_string() } else { why };
        let reason = format!("deribit {word} {whose}: {why}").into();
        vec![Event::OrderCanceled(OrderCanceled { client_order_id: coid.clone(), reason, ts })]
    };
    match s(row, "order_state").as_str() {
        "cancelled" if child || !fired => ended("cancelled", s(row, "cancel_reason")),
        "rejected" if child || !fired => ended("rejected", s(row, "reject_reason")),
        "triggered" if trigger_row => vec![triggered(&coid, ts)],
        "open" | "untriggered" if trigger_row && fired => vec![triggered(&coid, ts)],
        _ => Vec::new(),
    }
}

/// method=='subscription' + channel startswith 'user.trades' + list data → rows; channel startswith
/// 'user.orders' → [`map_deribit_order_row`] per row (a list or one object); else [].
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
    if channel.starts_with("user.orders") {
        return match params.get("data") {
            Some(Value::Array(rows)) => rows.iter().flat_map(map_deribit_order_row).collect(),
            Some(row @ Value::Object(_)) => map_deribit_order_row(row),
            _ => Vec::new(),
        };
    }
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
