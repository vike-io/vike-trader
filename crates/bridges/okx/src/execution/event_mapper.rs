//! Pure OKX private-WS → vike event mappers. Exact ports of `exec/okx/mapper.py` +
//! `exec/okx/perp_mapper.py`.
//!
//! OKX collapses Bybit's execution+order split into ONE `orders` channel — each row
//! carries fill details AND lifecycle state. Commission: OKX `fillFee` is NEGATIVE for a
//! charge → `commission = -fillFee` (positive cost / negative rebate). The perp dispatch
//! rescales fills from CONTRACTS to BASE (× ct_val), carries fillMarkPx/markPx, sets
//! position_side from posSide, and gates liquidation categories (full_liquidation /
//! partial_liquidation / adl) on a REAL fill — a non-fill liq-category lifecycle frame
//! falls through to its normal Accepted/Canceled path (else qty=0 would flatten the
//! whole book at price 0 in apply_liquidation).
//!
//! A STOP is an algo order: the `orders` channel carries only the CHILD order a fired stop spawns
//! (filed under the stop's `algoClOrdId` by `row_order_client_id`), and the stop's own state rides the business
//! socket's `orders-algo` channel, mapped by [`map_okx_algo_frame`].

use serde_json::Value;
// Loose venue-JSON coercion: the shared keyed accessors, under this module's short local names.
// These wrappers were re-declared per venue; `vike_bridge_core::json` is the one home. `s` keeps
// `json_str`'s Bool arm here — no bool-valued field is read through it.
use vike_bridge_core::json::{get_f64 as f, get_fee_f64 as fee_f, get_str as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, OrderAccepted, OrderCanceled, OrderRejected, OrderTriggered,
    PositionLiquidated, PositionSide, TradeId,
};

/// Python `int(item.get("fillTime") or item.get("uTime") or 0)` — falsy fallback chain.
fn ts_of(item: &Value) -> i64 {
    for key in ["fillTime", "uTime"] {
        match item.get(key) {
            Some(Value::String(x)) if !x.is_empty() && x != "0" => {
                if let Ok(v) = x.parse::<i64>() {
                    return v;
                }
            }
            Some(Value::Number(n)) => {
                let v = n.as_i64().unwrap_or(0);
                if v != 0 {
                    return v;
                }
            }
            _ => {}
        }
    }
    0
}

/// OKX `posSide` → [`PositionSide`] — the ONE reading of the field (fills, liquidations, the
/// reconcile snapshot and the position reports all call it). OKX spells it LOWERCASE: `long` →
/// `Long`, `short` → `Short`, and `net` (one-way mode) → `Both`; an ABSENT or empty field is
/// one-way too (`Both`). Any other word is WARNED and read as `Both`, the one-way bucket whose
/// signed qty still carries the direction.
pub(crate) fn pside(item: &Value) -> PositionSide {
    match item.get("posSide").and_then(|p| p.as_str()) {
        None | Some("") | Some("net") => PositionSide::Both,
        Some("long") => PositionSide::Long,
        Some("short") => PositionSide::Short,
        Some(other) => {
            tracing::warn!(
                venue = crate::execution::rest::VENUE,
                pos_side = other,
                "unknown posSide word — reading it as the one-way BOTH bucket"
            );
            PositionSide::Both
        }
    }
}

/// `str(item.get("fillSz") or "0") not in ("", "0") and bool(item.get("tradeId"))`
fn has_fill(item: &Value) -> bool {
    let fill_sz = match item.get("fillSz") {
        None | Some(Value::Null) => "0".to_string(),
        Some(Value::String(x)) if x.is_empty() => "0".to_string(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => "0".to_string(),
        Some(Value::String(x)) => x.clone(),
        Some(other) => other.to_string(),
    };
    if fill_sz == "0" || fill_sz.is_empty() {
        return false;
    }
    match item.get("tradeId") {
        Some(Value::String(x)) => !x.is_empty(),
        Some(Value::Number(n)) => n.as_i64() != Some(0),
        _ => false,
    }
}

/// The order client id an `orders` / `orders-history` row's events are filed under.
///
/// A STOP is an algo order, and when it fires OKX places a CHILD order whose own `clOrdId` is the
/// venue's (`O4038740198576748544`, measured on the demo account on 2026-10-24) while its
/// `algoClOrdId` is the stop's client id — ours. The child's rows are the stop's fills, so they are
/// filed under `algoClOrdId`: read `clOrdId` alone and the fill books under an id the core never
/// minted, and the stop stays `ACCEPTED` for good. An ordinary order's `algoClOrdId` is empty, so its
/// id is `clOrdId` as ever.
pub(crate) fn row_order_client_id(item: &Value) -> String {
    let algo = s(item, "algoClOrdId");
    if algo.is_empty() { s(item, "clOrdId") } else { algo }
}

/// The `cancelSource` code OKX gives an order it cancelled because it was post-only and would have
/// taken liquidity (`cancelSourceReason`: "This post-only order will take liquidity from maker orders.").
const POST_ONLY_CANCEL_SOURCE: &str = "31";

/// Why a cancelled row ended: the typed post-only reason for that one `cancelSource` (MEASURED on the
/// demo host, `tests/okx_smokes/okx_post_only_smoke.rs`: the create call succeeded, so the typed terminal
/// is a cancel), `otherwise` the words its reader gives. The stream and the replay both read it.
pub(crate) fn cancel_reason(item: &Value, otherwise: String) -> String {
    if s(item, "cancelSource") == POST_ONLY_CANCEL_SOURCE {
        vike_model::events::POST_ONLY_CANCEL_REASON.to_string()
    } else {
        otherwise
    }
}

/// One orders row → [FillEvent?, wrap?] or lifecycle-only.
pub fn map_okx_order(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    // per-row error code gate (takes precedence)
    let code = s(item, "code");
    if !code.is_empty() && code != "0" {
        return vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: row_order_client_id(item),
            reason: s(item, "msg").into(),
            timestamp: ts_of(item),
        })];
    }
    let state = s(item, "state");
    let order_client_id = row_order_client_id(item);
    let ts = ts_of(item);

    if has_fill(item) {
        // OKX's `orders` channel carries fill details and lifecycle on the SAME row, so
        // [`has_fill`] is the primary gate and it already requires a truthy `tradeId` — this arm is
        // therefore unreachable while the two agree. It exists as the COMPILER-VISIBLE proof of the
        // rule the rest of the family follows, so a future edit to `has_fill` that stops checking
        // `tradeId` cannot silently reintroduce an id-less fill. Verdict if it ever fires: DROP the
        // fill and the wrap `vike_bridge_core::terminal_events` mints together, exactly as the other
        // venues do. An id-less fill escapes the engine's `seen_trade_ids` guard, and the audit-A3
        // resync replays this same fill out of `/api/v5/trade/fills-history` (`crate::execution::resync`), so
        // it would book commission and realized PnL TWICE. Nothing is synthesized — a clock or
        // counter id differs on the second run and so defeats the dedup outright.
        let Ok(trade_id) = TradeId::new(s(item, "tradeId")) else {
            tracing::warn!(
                venue,
                symbol = %s(item, "instId"),
                order_client_id = %order_client_id,
                "orders row passed has_fill but carries no `tradeId` — dropping the fill and its \
                 wrap; an un-dedupable fill double-books commission and realized PnL on resync"
            );
            return Vec::new();
        };
        let fill = FillEvent {
            trade_id,
            order_client_id: order_client_id.clone(),
            venue: venue.to_string().into(),
            symbol: match item.get("instId") {
                Some(Value::String(x)) => x.clone().into(),
                _ => symbol.to_string().into(),
            },
            side: if item.get("side").and_then(|x| x.as_str()) == Some("buy") { 1 } else { -1 },
            last_qty: f(item, "fillSz"),
            last_price: f(item, "fillPx"),
            // OKX fillFee < 0 = charge -> positive cost; > 0 = rebate -> negative
            commission: -fee_f(item, "fillFee", venue),
            commission_asset: s(item, "fillFeeCcy").into(),
            liquidity_side: LiquiditySide::from_maker(
                item.get("execType").and_then(|x| x.as_str()) == Some("M"),
            ),
            timestamp: ts,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        // terminality: state=='filled' primary; accFillSz >= sz fallback (parse-safe)
        let mut is_filled = state == "filled";
        if !is_filled {
            let accumulated = match item.get("accFillSz") {
                None | Some(Value::Null) => Some(-1.0),
                Some(Value::String(x)) if x.is_empty() => Some(-1.0),
                Some(Value::String(x)) => x.parse::<f64>().ok(),
                Some(Value::Number(n)) => n.as_f64(),
                Some(_) => None,
            };
            let sz = match item.get("sz") {
                None | Some(Value::Null) => Some(0.0),
                Some(Value::String(x)) if x.is_empty() => Some(0.0),
                Some(Value::String(x)) => x.parse::<f64>().ok(),
                Some(Value::Number(n)) => n.as_f64(),
                Some(_) => None,
            };
            if let (Some(accumulated), Some(sz)) = (accumulated, sz)
                && accumulated >= 0.0
                && sz > 0.0
                && accumulated >= sz
            {
                is_filled = true;
            }
        }
        return vike_bridge_core::terminal_events(order_client_id, fill, ts, is_filled);
    }

    // no fill — lifecycle-only
    match state.as_str() {
        // A fired stop's CHILD going `live` is not the stop's acceptance (the REST ack was): the
        // child's `ordId` is not the stop's venue id, and the FSM allows `OrderAccepted` from
        // `Submitted` only. The stop's own `effective` word is the algo socket's (`map_okx_algo_order`).
        "live" if !s(item, "algoClOrdId").is_empty() => Vec::new(),
        "live" => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: Some(s(item, "ordId").into()),
            timestamp: ts,
        })],
        "canceled" | "mmp_canceled" => vec![Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: cancel_reason(item, s(item, "cancelSource")).into(),
            timestamp: ts,
        })],
        _ => Vec::new(), // filled/partially_filled snapshot dup — already folded
    }
}

fn account_state(frame: &Value, venue: &str) -> Vec<Event> {
    let mut balances: Vec<(String, f64)> = Vec::new();
    let mut ts_frame = 0i64;
    for entry in frame.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        match entry.get("uTime") {
            Some(Value::String(x)) => {
                if let Ok(v) = x.parse::<i64>() {
                    ts_frame = v;
                }
            }
            Some(Value::Number(n)) => ts_frame = n.as_i64().unwrap_or(ts_frame),
            _ => {}
        }
        for d in entry.get("details").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
            let asset = d.get("ccy").and_then(|c| c.as_str()).unwrap_or("");
            let wb = match d.get("cashBal") {
                None | Some(Value::Null) => Some(0.0),
                Some(Value::String(x)) if x.is_empty() => Some(0.0),
                Some(Value::String(x)) => x.parse::<f64>().ok(),
                Some(Value::Number(n)) => n.as_f64(),
                Some(_) => None, // Python float(dict) raises -> row skipped
            };
            let Some(wb) = wb else { continue };
            if !asset.is_empty() {
                balances.push((asset.to_string(), wb));
            }
        }
    }
    if balances.is_empty() {
        Vec::new()
    } else {
        vec![Event::AccountState(AccountState {
            venue: venue.to_string().into(),
            balances,
            timestamp: ts_frame,
            // A bridge holds ONE credential set and knows no account labels: the START stamps
            // the route key (`vike_tradehub_core::account_event_sender`), never a venue adapter.
            route_key: None,
        })]
    }
}

/// Shared dispatch guard: event acks → []; non-object arg → []; returns the channel.
fn channel_of(frame: &Value) -> Option<String> {
    if !frame.is_object() {
        return None;
    }
    if frame.get("event").is_some() {
        return None; // login/subscribe/error ack
    }
    frame
        .get("arg")
        .filter(|a| a.is_object())
        .and_then(|a| a.get("channel"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// One `orders-algo` row (a STOP's own state, pushed on the business socket; `orders-algo-history`
/// rows read the same way) → the order FSM, under the stop's client id (`algoClOrdId`, ours):
///
/// | `state` | event |
/// |---|---|
/// | `effective` | `OrderTriggered`: the stop fired and spawned its child order (`ordIdList`). The child's fill books on the stop through `row_order_client_id`; this event can land before or after it, and on a stop that fill already closed the FSM drops it |
/// | `canceled` | `OrderCanceled` (ours through `cancel-algos`, or the venue's) |
/// | `order_failed` | `OrderCanceled` carrying `failCode`: the trigger fired and the venue refused the order it placed (`51169`, a reduce-only stop on a flat position; MEASURED), so nothing rests and nothing filled. `OrderCanceled`, not `OrderRejected`: the FSM allows a reject only before the accept, as binance's `REJECTED` stop (`crates/bridges/binance/src/execution/rest/algo.rs`) |
/// | `live`, `pause` | none: the REST ack already accepted it |
/// | `partially_effective`, `partially_failed` | none: part of the stop took effect and neither word is MEASURED, so the stop stays as it is (reconcile and the child's fills decide) |
///
/// A row with no `algoClOrdId` (a stop placed on the exchange by hand) maps to nothing.
pub fn map_okx_algo_order(row: &Value) -> Vec<Event> {
    let order_client_id = s(row, "algoClOrdId");
    if order_client_id.is_empty() {
        return Vec::new();
    }
    let ts = ts_of(row);
    match s(row, "state").as_str() {
        "effective" => {
            vec![Event::OrderTriggered(OrderTriggered { order_client_id, timestamp: ts })]
        }
        "canceled" => vec![Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: "algo order canceled".into(),
            timestamp: ts,
        })],
        "order_failed" => vec![Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: format!("algo order failed at the trigger (failCode {})", s(row, "failCode"))
                .into(),
            timestamp: ts,
        })],
        _ => Vec::new(),
    }
}

/// The `orders-algo` push (business socket) → events: [`map_okx_algo_order`] over every row. The
/// `algo-advance` push (the same socket, subscribed only while okx's row lists `trailing`:
/// `crate::execution::user_data::open_okx_algo_websocket`) maps its TRAILING rows (`ordType=move_order_stop`) the same way,
/// and nothing else it carries (an iceberg or a TWAP is never ours). Any other frame (acks, notices, a
/// `pong`, another channel) maps to nothing.
pub fn map_okx_algo_frame(frame: &Value) -> Vec<Event> {
    let trailing_only = match channel_of(frame).as_deref() {
        Some("orders-algo") => false,
        Some(crate::execution::user_data::ALGO_ADVANCE) => true,
        _ => return Vec::new(),
    };
    frame
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|row| !trailing_only || s(row, "ordType") == "move_order_stop")
        .flat_map(map_okx_algo_order)
        .collect()
}

/// Spot dispatch (`map_okx_private`).
pub fn map_okx_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(channel) = channel_of(frame) else {
        return Vec::new();
    };
    if channel == "account" {
        return account_state(frame, venue);
    }
    if channel != "orders" {
        return Vec::new();
    }
    let mut events = Vec::new();
    for item in frame.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        events.extend(map_okx_order(item, venue, symbol));
    }
    events
}

const LIQUIDATION_CATEGORIES: [&str; 3] = ["full_liquidation", "partial_liquidation", "adl"];

fn liquidation_event(row: &Value, venue: &str, symbol: &str, ct_val: f64) -> Event {
    // fillPx or px falsy-fallback (Python `row.get("fillPx") or row.get("px") or 0`)
    let px = {
        let fp = f(row, "fillPx");
        if fp != 0.0 { fp } else { f(row, "px") }
    };
    Event::PositionLiquidated(PositionLiquidated {
        venue: venue.to_string().into(),
        symbol: match row.get("instId") {
            Some(Value::String(x)) => x.clone().into(),
            _ => symbol.to_string().into(),
        },
        position_side: pside(row),
        qty: f(row, "fillSz") * ct_val, // contracts -> base, same rescale as fills
        liquidation_price: px,
        fee: fee_f(row, "fillFee", venue).abs(),
        timestamp: ts_of(row),
        trade_id: s(row, "tradeId").into(), // non-empty (has_fill gate)
        // Stamped at the START — see `vike_model::events::PositionLiquidated::route_key`.
        route_key: None,
        fee_currency: None,
    })
}

/// The engine-side dedup key of a fill of an instrument OTHER than the mounted one: `INSTID:tradeId`.
/// OKX documents `tradeId` per instrument, and the engine's fill ledger (`seen_trade_ids`) keys on
/// the bare id, so once one account trades two instruments a genuine second fill could be dropped as
/// a re-delivery. The mounted instrument keeps the bare id (resync, reconcile and a restored ledger
/// spell it that way), exactly as binance and aster's `fill_trade_id`; the user-data stream and the history
/// replay of the resync and the gap-sentinel ([`crate::execution::resync::scope_fill_ids`]) carry a foreign
/// instrument's fills, and so does the reconcile client's fills read
/// ([`crate::reconcile::ReconcileClientOkx`]): all three spell it this way, or a pass would read a fill the
/// stream folded as a `MissingFill`.
pub(crate) fn scoped_trade_id(id: &TradeId, instrument: &str) -> TradeId {
    TradeId::new(scoped_id(instrument, id)).unwrap_or_else(|_| id.clone())
}

/// The one spelling of [`scoped_trade_id`]'s `INSTID:tradeId`, for the readers whose id is not a `TradeId`.
pub(crate) fn scoped_id(instrument: &str, id: impl std::fmt::Display) -> String {
    format!("{instrument}:{id}")
}

/// Perp dispatch (`map_okx_perp`): liq-category FILLS → PositionLiquidated ONLY; other
/// rows through map_okx_order with the contracts→base enrichment. ONE `ct_val` for every row — the
/// mounted instrument's; [`map_okx_perp_by_instrument`] is the form that reads each row's own.
pub fn map_okx_perp(frame: &Value, venue: &str, symbol: &str, ct_val: f64) -> Vec<Event> {
    map_okx_perp_by_instrument(frame, venue, symbol, |_| Some(ct_val))
}

/// [`map_okx_perp`] with the contracts→base factor looked up PER ROW by the row's own `instId`
/// (`symbol` stands in for a row that names none). The private `orders` channel delivers every SWAP
/// of the account, so a fill of an instrument other than the mounted one must be scaled by THAT
/// instrument's `ctVal`: `ct_val_of` answers it, or `None` when it cannot be read — then a row that
/// carries a FILL is dropped with an error log (a fill sized by another contract's constant is wrong
/// money; reconcile surfaces the missing fill) while a row with no fill, which carries no size,
/// passes. `ct_val_of` is asked only for rows that carry a fill.
pub fn map_okx_perp_by_instrument(
    frame: &Value,
    venue: &str,
    symbol: &str,
    mut ct_val_of: impl FnMut(&str) -> Option<f64>,
) -> Vec<Event> {
    let Some(channel) = channel_of(frame) else {
        return Vec::new();
    };
    if channel == "account" {
        return account_state(frame, venue);
    }
    if channel != "orders" {
        return Vec::new();
    }
    let mut events = Vec::new();
    for item in frame.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        // The `orders` channel is subscribed for EVERY instrument class (`crate::execution::user_data`'s
        // `ORDERS_INST_TYPE`), and a MARGIN order wears the SPOT pair's instId: this adapter signs neither
        // margin trading nor options, so such a row (an order placed by hand in the OKX app) is not ours.
        if matches!(item.get("instType").and_then(Value::as_str), Some("MARGIN" | "OPTION")) {
            continue;
        }
        let instrument = match item.get("instId") {
            Some(Value::String(x)) if !x.is_empty() => x.as_str(),
            _ => symbol,
        };
        // `tradeId` is not promised unique across instruments; the engine's fill ledger keys on the
        // bare id, so a fill of any instrument but the mounted one is qualified (see `scoped_trade_id`).
        let foreign = !instrument.eq_ignore_ascii_case(symbol);
        let ct_val = if has_fill(item) {
            let Some(ct_val) = ct_val_of(instrument) else {
                tracing::error!(
                    venue,
                    inst_id = instrument,
                    order_client_id = %s(item, "clOrdId"),
                    "fill of an instrument whose ctVal cannot be read — dropped, not sized with \
                     another instrument's contract size"
                );
                continue;
            };
            ct_val
        } else {
            0.0 // no fill on this row: nothing below reads it
        };
        if has_fill(item) && LIQUIDATION_CATEGORIES.contains(&s(item, "category").as_str()) {
            let mut liquidated = liquidation_event(item, venue, symbol, ct_val);
            if foreign && let Event::PositionLiquidated(l) = &mut liquidated {
                l.trade_id = scoped_id(instrument, &l.trade_id).into();
            }
            events.push(liquidated);
            continue; // liquidation FILL -> PositionLiquidated ONLY; never a FillEvent
        }
        // Python: mark_raw = row.get("fillMarkPx") or row.get("markPx") (falsy chains:
        // None/""/numeric-0 fall through; the STRING "0" is truthy and stops the chain);
        // then None if mark_raw in (None, "", "0") else float(mark_raw).
        let truthy = |v: Option<&Value>| -> Option<Value> {
            match v {
                Some(Value::String(x)) if !x.is_empty() => Some(Value::String(x.clone())),
                Some(Value::Number(n)) if n.as_f64() != Some(0.0) => Some(Value::Number(n.clone())),
                _ => None,
            }
        };
        let mark_raw = truthy(item.get("fillMarkPx")).or_else(|| truthy(item.get("markPx")));
        let mark = match mark_raw {
            None => None,
            Some(Value::String(x)) if x == "0" => None,
            Some(Value::String(x)) => x.parse::<f64>().ok(),
            Some(Value::Number(n)) => n.as_f64(),
            Some(_) => None,
        };
        let ps = pside(item);
        for mut event in map_okx_order(item, venue, symbol) {
            // the fill a bare Fill or a wrap carries; every other event is not this tap's business
            if let Some(fe) = event.fill_mut() {
                fe.last_qty *= ct_val;
                crate::instrument::net_spot_buy_fee(fe);
                if foreign {
                    fe.trade_id = scoped_trade_id(&fe.trade_id, instrument);
                }
                fe.mark_price = mark;
                fe.position_side = ps;
            }
            events.push(event);
        }
    }
    events
}
