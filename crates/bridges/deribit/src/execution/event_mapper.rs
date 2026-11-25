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
//! admitted via `extra_symbols` by the combo lowering, and by order client id ownership regardless), while
//! the combo-instrument net print matches neither the order's (empty) `symbol` nor a leg and is
//! skipped by `Engine::owns_fill_symbol` — the venue books NO position under the combo
//! id (probe evidence), so folding the net row would mint a phantom position and double-count.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`), byte-identical to the local `fn s` this module used to declare. NOT
// `get_str`, which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_f64 as f, get_fee_f64, get_i64 as i, get_str_boolless as s};
use vike_bridge_core::side::sign_default_sell;
use vike_model::events::LiquiditySide;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderTriggered, TradeId};

pub fn map_deribit_trade(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let order_client_id = s(item, "label");
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
            order_client_id = %order_client_id,
            "user.trades row carries no `trade_id` — dropping the fill and its wrap; an \
             un-dedupable fill double-books commission and realized PnL when a resync replays it"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        order_client_id: order_client_id.clone(),
        venue: venue.to_string().into(),
        symbol: match item.get("instrument_name") {
            Some(Value::String(x)) => x.clone().into(),
            _ => symbol.to_string().into(),
        },
        side: sign_default_sell(item.get("direction").and_then(|d| d.as_str()).unwrap_or("")),
        last_qty: f(item, "amount"), // COIN units (options) — no rescale
        last_price: f(item, "price"),
        commission: get_fee_f64(item, "fee", venue), // SIGNED, kept as-is
        commission_asset: s(item, "fee_currency").into(),
        liquidity_side: LiquiditySide::from_maker(
            item.get("liquidity").and_then(|l| l.as_str()) == Some("M"),
        ),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    let is_filled = s(item, "state") == "filled";
    vike_bridge_core::terminal_events(order_client_id, fill, ts, is_filled)
}

/// The `order_type`s the bridge places as a venue-held TRIGGER order (`execution::rest::order_params_on`):
/// the stop's own row carries one of these, the order it spawns when it fires does not. A
/// resting `trailing_stop`'s row was captured on the testnet (2026-11-08); a fired one's never was.
const TRIGGER_ORDER_TYPES: [&str; 5] =
    ["stop_market", "stop_limit", "take_market", "take_limit", "trailing_stop"];

/// The venue's two words for an order that ended without a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndWord {
    Cancelled,
    Rejected,
}

/// What one order object of the venue says, whichever read brought it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderRow {
    /// A stop, the order a fired stop placed, or a limit a refused edit ended: each was accepted long
    /// ago, so its end is an `OrderCanceled` whichever word the venue used, because the order FSM allows
    /// a reject only before the accept. `whose` names it in a reason.
    EndedAfterAccept { word: EndWord, whose: &'static str },
    /// An ordinary order that ended. The live stream leaves it to the order client's synchronous
    /// answers; the replay exists to say it.
    OrdinaryEnd(EndWord),
    /// A stop fired.
    Fired,
    /// Nothing to say: it rests or waits, it filled (a fill is `user.trades`'s), or it is a stop's own
    /// row closing as it fires.
    Silent,
}

/// The ONE reading of an order row: [`map_deribit_order_row`] (the live `user.orders` stream) and
/// `crate::execution::resync::map_deribit_history` (the A3 replay) both match on it and keep only their own words.
///
/// ⚠ A TRIGGER row that ends while `triggered` is true and names no `trigger_order_id` is
/// [`OrderRow::Silent`], on purpose: if the venue closes a stop's own row as it fires, a terminal here
/// would cancel an order about to fill and the fill's `OrderFilled` would then be refused by the FSM.
/// MEASURED on the testnet 2026-11-08 (`deribit_native_stops_on_the_testnet`): once a stop fires, its
/// own row names its own id as `trigger_order_id`; a stop that fires and fills leaves that row
/// `triggered` (its market child says `filled`), and a reduce-only stop that fires on a flat position
/// ends its own row in ONE state word, `rejected:invalid_reduce_only_order`, with no `reject_reason`.
pub(crate) fn read_order_row(row: &Value) -> OrderRow {
    let trigger_row = TRIGGER_ORDER_TYPES.contains(&s(row, "order_type").as_str());
    let fired = row.get("triggered").and_then(Value::as_bool) == Some(true);
    let word = match s(row, "order_state").as_str() {
        "cancelled" => EndWord::Cancelled,
        w if is_rejected(w) => EndWord::Rejected,
        "triggered" if trigger_row => return OrderRow::Fired,
        "open" | "untriggered" if trigger_row && fired => return OrderRow::Fired,
        _ => return OrderRow::Silent,
    };
    if !s(row, "trigger_order_id").is_empty() {
        return OrderRow::EndedAfterAccept { word, whose: "the order a stop fired" };
    }
    match (trigger_row, fired) {
        (true, true) => OrderRow::Silent,
        (true, false) => OrderRow::EndedAfterAccept { word, whose: "the stop" },
        (false, false) if is_post_only_cross(row) => {
            OrderRow::EndedAfterAccept { word, whose: "the order" }
        }
        (false, _) => OrderRow::OrdinaryEnd(word),
    }
}

/// Did the venue end this order because an EDIT of it, post-only, would have taken liquidity? Its
/// `cancel_reason` `edit_post_only_reject` (MEASURED on the testnet 2026-11-06,
/// `tests/deribit_post_only_smoke.rs`), whatever `order_state` word it came under: the
/// `OrderCanceled::post_only_would_cross` every reader of an order object sets.
pub(crate) fn is_post_only_cross(order: &Value) -> bool {
    s(order, "cancel_reason") == "edit_post_only_reject"
}

/// The venue's refusal in `order_state`: `rejected`, or `rejected:<reason>` (one word, measured on a
/// fired trigger's own row: `read_order_row`'s doc). Every reader of an order object asks this.
pub(crate) fn is_rejected(order_state: &str) -> bool {
    order_state == "rejected" || order_state.starts_with("rejected:")
}

/// Why the venue refused the order: `reject_reason`, else the word after `rejected:` in `order_state`
/// (a fired trigger the venue refuses states it only there, as `read_order_row`'s doc measured).
pub(crate) fn reject_reason(row: &Value) -> String {
    let why = s(row, "reject_reason");
    if !why.is_empty() {
        return why;
    }
    s(row, "order_state").strip_prefix("rejected:").unwrap_or_default().to_string()
}

/// One `user.orders` row → the end (or the firing) of a STOP, under the stop's order client id.
///
/// The `user.trades` channel above is fills-only, so a stop the venue ends WITHOUT a fill (cancelled
/// by someone else, or fired into an order the venue then refused) was never announced and the node
/// kept it `Accepted` for good. This is the other half. It looks at two kinds of row only, because
/// everything else already has its events (an ordinary order's accept and cancel are the order
/// client's synchronous answers, its fills are the channel above, and the A3 replay recovers a
/// gap):
///
/// * a TRIGGER row (`order_type` in `TRIGGER_ORDER_TYPES`: the five kinds the bridge places);
/// * the CHILD a fired stop spawns (`trigger_order_id` names the stop), which carries the stop's
///   `label` (MEASURED on the testnet 2026-10-26: its fill and `get_order_state_by_label` show it
///   under the order client id);
/// * a limit the venue cancelled for a refused edit (`cancel_reason` `edit_post_only_reject`, the
///   documented word, never captured): the edit's answer says so when it can
///   (`crate::execution::rest`'s `edit_order`), and this row is what a lost reply leaves.
///
/// | row | event |
/// |---|---|
/// | trigger row, `order_state` `cancelled`, `triggered` not true | `OrderCanceled` (the stop was cancelled while it waited) |
/// | child row, `order_state` `cancelled` | `OrderCanceled` (a stop-limit's resting limit cancelled, or a market child the venue ended) |
/// | child row (or a fired stop's own row: it names itself in `trigger_order_id`), or a trigger row whose `triggered` is not true, `order_state` `rejected` or `rejected:<reason>` | `OrderCanceled` carrying `reject_reason` (else the `<reason>`): the trigger fired and the venue refused the order it placed (a reduce-only stop on a flat position), so nothing rests and nothing filled. `OrderCanceled`, not `OrderRejected`: the order FSM allows a reject only before the accept, and a stop is always accepted first |
/// | trigger row, `order_state` `triggered`, or `triggered` true on a row that is not ended | `OrderTriggered` (the stop fired; its fill, if any, books on the stop by `label`) |
/// | anything else (`untriggered`, `open`, `filled`, a row with no `label`, a stop's own row ended while `triggered` is true: `read_order_row`) | nothing: the submit's reply already accepted it, and a fill is the `user.trades` channel's |
///
/// A row the node's own cancel already ended repeats a terminal the FSM and the gatewayhub's
/// `OnceMemory` drop; so does the A3 replay's (`crate::execution::resync::map_deribit_history`) for a child that
/// shows in the order history: one fact, one record, downstream of this mapper.
pub fn map_deribit_order_row(row: &Value) -> Vec<Event> {
    let order_client_id = s(row, "label");
    if order_client_id.is_empty() {
        return Vec::new();
    }
    let ts = i(row, "last_update_timestamp");
    match read_order_row(row) {
        OrderRow::EndedAfterAccept { word, whose } => {
            let crossed = is_post_only_cross(row);
            // The venue's refusal of an order it had taken: a `rejected` state, or a post-only cross.
            let refused = word == EndWord::Rejected || crossed;
            let (word, why) = match word {
                EndWord::Cancelled => ("cancelled", s(row, "cancel_reason")),
                EndWord::Rejected => ("rejected", reject_reason(row)),
            };
            let why = if why.is_empty() { "no reason given".to_string() } else { why };
            let reason = format!("deribit {word} {whose}: {why}").into();
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason,
                timestamp: ts,
                post_only_would_cross: crossed,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                )
                .filter(|_| refused),
            })]
        }
        OrderRow::Fired => {
            vec![Event::OrderTriggered(OrderTriggered { order_client_id, timestamp: ts })]
        }
        OrderRow::OrdinaryEnd(_) | OrderRow::Silent => Vec::new(),
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
