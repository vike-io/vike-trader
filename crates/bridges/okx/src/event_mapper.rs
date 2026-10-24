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

use serde_json::Value;
// Loose venue-JSON coercion: the shared keyed accessors, under this module's short local names.
// These wrappers were re-declared per venue; `vike_bridge_core::json` is the one home. `s` keeps
// `json_str`'s Bool arm here — no bool-valued field is read through it.
use vike_bridge_core::json::{get_f64 as f, get_str as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, OrderAccepted, OrderCanceled, OrderRejected,
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
                venue = crate::perp::VENUE,
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

/// One orders row → [FillEvent?, wrap?] or lifecycle-only.
pub fn map_okx_order(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    // per-row error code gate (takes precedence)
    let code = s(item, "code");
    if !code.is_empty() && code != "0" {
        return vec![Event::OrderRejected(OrderRejected {
            client_order_id: s(item, "clOrdId"),
            reason: s(item, "msg").into(),
            ts: ts_of(item),
        })];
    }
    let state = s(item, "state");
    let coid = s(item, "clOrdId");
    let ts = ts_of(item);

    if has_fill(item) {
        // OKX's `orders` channel carries fill details and lifecycle on the SAME row, so
        // [`has_fill`] is the primary gate and it already requires a truthy `tradeId` — this arm is
        // therefore unreachable while the two agree. It exists as the COMPILER-VISIBLE proof of the
        // rule the rest of the family follows, so a future edit to `has_fill` that stops checking
        // `tradeId` cannot silently reintroduce an id-less fill. Verdict if it ever fires: DROP the
        // fill and the wrap `vike_bridge_core::terminal_events` mints together, exactly as the other
        // venues do. An id-less fill escapes the engine's `seen_trade_ids` guard, and the audit-A3
        // resync replays this same fill out of `/api/v5/trade/fills-history` (`crate::history`), so
        // it would book commission and realized PnL TWICE. Nothing is synthesized — a clock or
        // counter id differs on the second run and so defeats the dedup outright.
        let Ok(trade_id) = TradeId::new(s(item, "tradeId")) else {
            tracing::warn!(
                venue,
                symbol = %s(item, "instId"),
                client_order_id = %coid,
                "orders row passed has_fill but carries no `tradeId` — dropping the fill and its \
                 wrap; an un-dedupable fill double-books commission and realized PnL on resync"
            );
            return Vec::new();
        };
        let fill = FillEvent {
            trade_id,
            client_order_id: coid.clone(),
            venue: venue.to_string().into(),
            symbol: match item.get("instId") {
                Some(Value::String(x)) => x.clone().into(),
                _ => symbol.to_string().into(),
            },
            side: if item.get("side").and_then(|x| x.as_str()) == Some("buy") { 1 } else { -1 },
            last_qty: f(item, "fillSz"),
            last_px: f(item, "fillPx"),
            // OKX fillFee < 0 = charge -> positive cost; > 0 = rebate -> negative
            commission: -f(item, "fillFee"),
            commission_asset: s(item, "fillFeeCcy").into(),
            liquidity_side: if item.get("execType").and_then(|x| x.as_str()) == Some("M") {
                LiquiditySide::Maker
            } else {
                LiquiditySide::Taker
            },
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        // terminality: state=='filled' primary; accFillSz >= sz fallback (parse-safe)
        let mut is_filled = state == "filled";
        if !is_filled {
            let acc = match item.get("accFillSz") {
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
            if let (Some(acc), Some(sz)) = (acc, sz)
                && acc >= 0.0
                && sz > 0.0
                && acc >= sz
            {
                is_filled = true;
            }
        }
        return vike_bridge_core::terminal_events(coid, fill, ts, is_filled);
    }

    // no fill — lifecycle-only
    match state.as_str() {
        "live" => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some(s(item, "ordId").into()),
            ts,
        })],
        "canceled" | "mmp_canceled" => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: s(item, "cancelSource").into(),
            ts,
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
            ts: ts_frame,
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

const LIQ_CATEGORIES: [&str; 3] = ["full_liquidation", "partial_liquidation", "adl"];

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
        liq_price: px,
        fee: f(row, "fillFee").abs(),
        ts: ts_of(row),
        trade_id: s(row, "tradeId").into(), // non-empty (has_fill gate)
        // Stamped at the START — see `vike_model::events::PositionLiquidated::route_key`.
        route_key: None,
        fee_ccy: None,
    })
}

/// The engine-side dedup key of a fill of an instrument OTHER than the mounted one: `INSTID:tradeId`.
/// OKX documents `tradeId` per instrument, and the engine's fill ledger (`seen_trade_ids`) keys on
/// the bare id, so once one account trades two instruments a genuine second fill could be dropped as
/// a re-delivery. The mounted instrument keeps the bare id (resync, reconcile and a restored ledger
/// spell it that way), exactly as binance and aster's `fill_trade_id`; only the user-data stream
/// carries a foreign instrument's fills, so nothing ever spells it bare.
fn scoped_trade_id(id: &TradeId, inst: &str) -> TradeId {
    TradeId::new(format!("{inst}:{id}")).unwrap_or_else(|_| id.clone())
}

/// Perp dispatch (`map_okx_perp`): liq-category FILLS → PositionLiquidated ONLY; other
/// rows through map_okx_order with the contracts→base enrichment. ONE `ct_val` for every row — the
/// mounted instrument's; [`map_okx_perp_by_inst`] is the form that reads each row's own.
pub fn map_okx_perp(frame: &Value, venue: &str, symbol: &str, ct_val: f64) -> Vec<Event> {
    map_okx_perp_by_inst(frame, venue, symbol, |_| Some(ct_val))
}

/// [`map_okx_perp`] with the contracts→base factor looked up PER ROW by the row's own `instId`
/// (`symbol` stands in for a row that names none). The private `orders` channel delivers every SWAP
/// of the account, so a fill of an instrument other than the mounted one must be scaled by THAT
/// instrument's `ctVal`: `ct_val_of` answers it, or `None` when it cannot be read — then a row that
/// carries a FILL is dropped with an error log (a fill sized by another contract's constant is wrong
/// money; reconcile surfaces the missing fill) while a row with no fill, which carries no size,
/// passes. `ct_val_of` is asked only for rows that carry a fill.
pub fn map_okx_perp_by_inst(
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
        let inst = match item.get("instId") {
            Some(Value::String(x)) if !x.is_empty() => x.as_str(),
            _ => symbol,
        };
        // `tradeId` is not promised unique across instruments; the engine's fill ledger keys on the
        // bare id, so a fill of any instrument but the mounted one is qualified (see `scoped_trade_id`).
        let foreign = !inst.eq_ignore_ascii_case(symbol);
        let ct_val = if has_fill(item) {
            let Some(ct_val) = ct_val_of(inst) else {
                tracing::error!(
                    venue,
                    inst_id = inst,
                    client_order_id = %s(item, "clOrdId"),
                    "fill of an instrument whose ctVal cannot be read — dropped, not sized with \
                     another instrument's contract size"
                );
                continue;
            };
            ct_val
        } else {
            0.0 // no fill on this row: nothing below reads it
        };
        if has_fill(item) && LIQ_CATEGORIES.contains(&s(item, "category").as_str()) {
            let mut liquidated = liquidation_event(item, venue, symbol, ct_val);
            if foreign && let Event::PositionLiquidated(l) = &mut liquidated {
                l.trade_id = format!("{inst}:{}", l.trade_id).into();
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
        for ev in map_okx_order(item, venue, symbol) {
            events.push(match ev {
                Event::Fill(mut fe) => {
                    fe.last_qty *= ct_val;
                    if foreign {
                        fe.trade_id = scoped_trade_id(&fe.trade_id, inst);
                    }
                    fe.mark_price = mark;
                    fe.position_side = ps;
                    Event::Fill(fe)
                }
                Event::OrderFilled(mut w) => {
                    w.fill.last_qty *= ct_val;
                    if foreign {
                        w.fill.trade_id = scoped_trade_id(&w.fill.trade_id, inst);
                    }
                    w.fill.mark_price = mark;
                    w.fill.position_side = ps;
                    Event::OrderFilled(w)
                }
                Event::OrderPartiallyFilled(mut w) => {
                    w.fill.last_qty *= ct_val;
                    if foreign {
                        w.fill.trade_id = scoped_trade_id(&w.fill.trade_id, inst);
                    }
                    w.fill.mark_price = mark;
                    w.fill.position_side = ps;
                    Event::OrderPartiallyFilled(w)
                }
                // every other event: not this tap's business (audited 2026-10, I-21)
                other => other,
            });
        }
    }
    events
}
