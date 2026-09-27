//! Pure Bybit private-WS → vike event mappers. Exact ports of `exec/bybit/mapper.py` +
//! `exec/bybit/perp_mapper.py`.
//!
//! `execution` rows → dual-publish [FillEvent, wrap]; `order` rows → lifecycle ONLY
//! (Filled/PartiallyFilled → [] — the execution topic owns fills, no double-fold);
//! `wallet` → AccountState from walletBalance totals; op-only ack/pong frames → [].
//! Perp dispatch additionally: BustTrade/AdlTrade execution rows → PositionLiquidated
//! ONLY (their execFee is fee-POSITIVE — never abs(), never negate), and Trade fills are
//! enriched with markPrice + position_side from positionIdx (0/absent→BOTH, 1→LONG,
//! 2→SHORT). Funding/Settle execution rows map to [] — Bybit funding comes from the REST
//! transaction-log (`funding.rs`); the WS Funding row's execFee is the EXACT NEGATIVE of
//! that cashflow (the documented sign trap — routing around it, not flipping it).
//!
//! Fast-fill hint (opt-in `VIKE_BYBIT_FAST_EXEC=1`, wired in [`crate::user_data`]): `execution.fast`
//! rows — Bybit's low-latency fill stream ("significantly reduces data latency compared original
//! execution stream") — map to a single EARLY bare `Event::Fill` via [`map_execution_fast`], with NO
//! dual-publish wrap (the slim frame omits the terminal signal, so a wrap here could strand the FSM).
//! The `execId` is IDENTICAL to the slow `execution` twin, so the engine's `seen_trade_ids` guard
//! books the fill EXACTLY once; the RETAINED `execution` subscription still carries the fee + terminal
//! wrap that drives FSM terminalization. Inert unless an `execution.fast` frame actually arrives (only
//! under the subscribe-list gate), so a default build is byte-identical.

use serde_json::Value;
// Loose venue-JSON coercion: the shared keyed accessors, under this module's short local names.
// These wrappers were re-declared per venue; `vike_bridge_core::json` is the one home. `s` keeps
// `json_str`'s Bool arm here — no bool-valued field is read through it (`isMaker` uses `as_bool`).
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected,
    PositionLiquidated, TradeId,
};

fn sym_or(v: &Value, fallback: &str) -> String {
    match v.get("symbol") {
        Some(Value::String(x)) => x.clone(),
        _ => fallback.to_string(),
    }
}

/// Python `float(item.get(key, default))` — raising (None) on unparseable, used where
/// the oracle wraps in try/except.
fn f_strict(v: &Value, key: &str, default: f64) -> Option<f64> {
    match v.get(key) {
        None => Some(default),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(x)) => x.parse::<f64>().ok(),
        Some(_) => None,
    }
}

/// One execution row → [FillEvent, OrderPartiallyFilled|OrderFilled].
/// execType != 'Trade' → [] (skips BustTrade/Funding/AdlTrade/SettleFundingFee here).
pub fn map_execution(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    if item.get("execType").and_then(|e| e.as_str()) != Some("Trade") {
        return Vec::new();
    }
    let coid = s(item, "orderLinkId");
    let ts = i(item, "execTime");
    // `execId` is the fill DEDUP key and Bybit V5 documents it on every `execution` row — an
    // absent/empty one is a malformed frame, not a shape the venue has. DROP the whole fill: the
    // audit-A3 resync replays this same execution out of `/v5/execution/list` (`crate::history`'s
    // `map_bybit_history`), and with no id the engine's `seen_trade_ids` guard cannot collapse the
    // pair — qty, commission and realized PnL would be booked TWICE. The bare `Event::Fill` and the
    // wrap `vike_bridge_core::terminal_events` mints from it stand or fall TOGETHER: publishing the
    // wrap alone would terminalize the FSM for a fill the Account never folded. Nothing is
    // synthesized — a clock/counter id differs on the second run and so defeats dedup outright.
    let Ok(trade_id) = TradeId::new(s(item, "execId")) else {
        tracing::warn!(
            venue,
            symbol = %sym_or(item, symbol),
            client_order_id = %coid,
            "execution row carries no `execId` — dropping the fill and its wrap; an un-dedupable \
             fill double-books commission and realized PnL when the resync replays it"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        client_order_id: coid.clone(),
        venue: venue.to_string().into(),
        symbol: sym_or(item, symbol).into(),
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
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    // Terminal detection, robust to string-dust: cumExecQty >= orderQty when both
    // present; fallback leavesQty == exactly 0.0; any parse failure -> partial.
    // (Python's try wraps BOTH branches — a bad leavesQty also lands on False.)
    let is_filled = (|| -> Option<bool> {
        let cum = f_strict(item, "cumExecQty", -1.0)?;
        let order_qty = f_strict(item, "orderQty", -1.0)?;
        if cum >= 0.0 && order_qty > 0.0 && cum >= order_qty {
            return Some(true);
        }
        // Python: float(item.get("leavesQty", 1) or 1) — falsy ("", 0, null) -> 1
        let leaves = match item.get("leavesQty") {
            None => 1.0,
            Some(Value::String(x)) if x.is_empty() => 1.0,
            Some(Value::Null) => 1.0,
            Some(Value::Number(n)) => {
                let v = n.as_f64()?;
                if v == 0.0 { 1.0 } else { v }
            }
            Some(Value::String(x)) => {
                let v = x.parse::<f64>().ok()?;
                // Python `or 1` applies to the STRING (non-empty is truthy) — "0" parses
                // to 0.0 and stays 0.0
                v
            }
            Some(_) => return None,
        };
        Some(leaves == 0.0)
    })()
    .unwrap_or(false);
    vike_bridge_core::terminal_events(coid, fill, ts, is_filled)
}

/// True for Bybit's low-latency fill topic — the uncategorised `execution.fast` AND its categorised
/// variants (`execution.fast.linear` / `.spot` / …). Kept distinct from the exact `"execution"` arm:
/// the slow full stream is `"execution"`, the fast slim stream is `"execution.fast"`.
pub(crate) fn is_fast_exec_topic(topic: &str) -> bool {
    topic == "execution.fast" || topic.starts_with("execution.fast.")
}

/// One `execution.fast` row → an EARLY bare `[Event::Fill]` (deliberately NO dual-publish wrap).
///
/// Bybit's fast execution stream is a SLIM frame — category / symbol / orderId / isMaker /
/// orderLinkId / side / execId / execPrice / execQty / execTime — and OMITS execType, execFee +
/// feeCurrency, cumExecQty + orderQty + leavesQty, markPrice, and positionIdx. Consequences, each
/// load-bearing:
///
/// * **Commission UNKNOWN → `0.0`.** The fee is not on the fast wire; the slow `execution` twin
///   carries the real `execFee`. (That twin's bare Fill is dedup-dropped by the engine, so the fee is
///   NOT retro-applied — a documented cost of the early hint, not enrichable without an engine-level
///   fee-delta lane. See the crate report.)
/// * **NO wrap — bare Fill ONLY.** With no cum/order/leaves qty there is no terminal-vs-partial
///   signal, so we cannot emit `OrderFilled`/`OrderPartiallyFilled` correctly. Emitting a partial
///   wrap here would advance the engine's `seen_fsm_trade_ids` for this execId, causing the slow
///   twin's REAL terminal wrap (same execId) to be dedup-dropped — stranding the order at
///   PARTIALLY_FILLED. Emitting only the bare Fill lets the RETAINED `execution` subscription's wrap
///   drive FSM terminalization while the position + the fill-rate breaker move EARLY off the fast row.
/// * **Dedup key `trade_id = execId`** is IDENTICAL across `execution.fast` and `execution`, so the
///   engine's always-on `seen_trade_ids` guard folds the position for this execId EXACTLY once —
///   whichever topic arrives first wins, the other's bare Fill is dropped
///   (`vike_exec::ExecutionEngine::on_event`).
/// * **No execType branch.** The fast stream is fills-only (Bybit never pushes BustTrade/AdlTrade/
///   Funding rows on it), so every row is a Trade fill. `mark_price`/`position_side` are absent →
///   `None` / `BOTH` (correct for one-way; a hedge-mode side is only learned from the slow twin).
pub fn map_execution_fast(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    // The fast hint's ENTIRE correctness argument is the third bullet above — its `execId` equals the
    // slow `execution` twin's, so `seen_trade_ids` folds the trade exactly once. With no `execId`
    // that argument is gone: this row would fold, and then the slow twin would fold AGAIN. Drop it.
    // The slow twin still books the trade (with the real fee and the terminal wrap), so the only
    // thing lost is the few-ms head start.
    let Ok(trade_id) = TradeId::new(s(item, "execId")) else {
        tracing::warn!(
            venue,
            symbol = %sym_or(item, symbol),
            client_order_id = %s(item, "orderLinkId"),
            "execution.fast row carries no `execId` — dropping the early fill hint; without the \
             shared dedup key it would double-fold against its slow `execution` twin"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        client_order_id: s(item, "orderLinkId"),
        venue: venue.to_string().into(),
        symbol: sym_or(item, symbol).into(),
        side: if item.get("side").and_then(|x| x.as_str()) == Some("Buy") { 1 } else { -1 },
        last_qty: f(item, "execQty"),
        last_px: f(item, "execPrice"),
        // Fast frame omits execFee/feeCurrency — fee is unknown at the early hint (the slow
        // `execution` twin carries the real values, but is dedup-dropped by execId).
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: if item.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false) {
            LiquiditySide::Maker
        } else {
            LiquiditySide::Taker
        },
        ts: i(item, "execTime"),
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    // Bare Fill ONLY — see the fn doc: the FSM terminal is owned by the retained `execution` twin.
    vec![Event::Fill(fill)]
}

/// One order row → lifecycle ONLY (never fills).
pub fn map_order(item: &Value, _venue: &str, _symbol: &str) -> Vec<Event> {
    let status = item.get("orderStatus").and_then(|x| x.as_str()).unwrap_or("");
    let coid = s(item, "orderLinkId");
    let ts = i(item, "updatedTime");
    match status {
        // REST already published Accepted; the engine swallows the duplicate transition
        "New" => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some(s(item, "orderId").into()),
            ts,
        })],
        "Cancelled" | "PartiallyFilledCanceled" => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: s(item, "cancelType").into(),
            ts,
        })],
        "Rejected" => vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: s(item, "rejectReason").into(),
            ts,
        })],
        "Deactivated" => vec![Event::OrderExpired(OrderExpired { client_order_id: coid, ts })],
        _ => Vec::new(), // Filled/PartiallyFilled: execution topic owns fills
    }
}

fn wallet_account_state(frame: &Value, venue: &str) -> Option<Event> {
    // data[].coin[] with coin/walletBalance (TOTAL); malformed rows skipped silently
    let ts = i(frame, "creationTime");
    let mut balances: Vec<(String, f64)> = Vec::new();
    for acct in frame.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        for c in acct.get("coin").and_then(|c| c.as_array()).unwrap_or(&vec![]) {
            let asset = c.get("coin").and_then(|a| a.as_str()).unwrap_or("");
            // Python float(c.get("walletBalance", 0) or 0) in try/except -> skip row on bad
            let wb = match c.get("walletBalance") {
                None | Some(Value::Null) => Some(0.0),
                Some(Value::Number(n)) => n.as_f64(),
                Some(Value::String(x)) if x.is_empty() => Some(0.0),
                Some(Value::String(x)) => x.parse::<f64>().ok(),
                Some(_) => None,
            };
            let Some(wb) = wb else { continue };
            if !asset.is_empty() {
                balances.push((asset.to_string(), wb));
            }
        }
    }
    if balances.is_empty() {
        None
    } else {
        Some(Event::AccountState(AccountState {
            venue: venue.to_string().into(),
            balances,
            ts,
            // A bridge holds ONE credential set and knows no account labels: the MOUNT stamps
            // the route key (`vike_mount::account_event_sender`), never a venue adapter.
            route_key: None,
        }))
    }
}

/// Spot dispatch: topic execution/order/wallet; op-only ack frames → [].
pub fn map_bybit_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(topic) = frame.get("topic").and_then(|t| t.as_str()) else {
        return Vec::new(); // op-only ack (pong/auth/subscribe)
    };
    let no_rows = vec![];
    let data = frame.get("data").and_then(|d| d.as_array()).unwrap_or(&no_rows);
    let mut events = Vec::new();
    match topic {
        // Fast-fill hint (opt-in): the low-latency `execution.fast` twin → early bare Fill only.
        t if is_fast_exec_topic(t) => {
            for item in data {
                events.extend(map_execution_fast(item, venue, symbol));
            }
        }
        "execution" => {
            for item in data {
                events.extend(map_execution(item, venue, symbol));
            }
        }
        "order" => {
            for item in data {
                events.extend(map_order(item, venue, symbol));
            }
        }
        "wallet" => events.extend(wallet_account_state(frame, venue)),
        _ => {}
    }
    events
}

/// Bybit V5 linear positionIdx → position_side. 0/absent/bad → BOTH.
pub(crate) fn pside_from_idx(row: &Value) -> String {
    let idx = match row.get("positionIdx") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(x)) => x.parse::<i64>().unwrap_or(0),
        _ => 0,
    };
    match idx {
        1 => "LONG",
        2 => "SHORT",
        _ => "BOTH",
    }
    .to_string()
}

fn liquidation_event(item: &Value, venue: &str, symbol: &str) -> Event {
    Event::PositionLiquidated(PositionLiquidated {
        venue: venue.to_string().into(),
        symbol: sym_or(item, symbol).into(),
        position_side: pside_from_idx(item).into(),
        qty: f(item, "execQty"),
        liq_price: f(item, "execPrice"),
        // execFee on a non-Trade (BustTrade/AdlTrade) row is fee-POSITIVE (a cost);
        // apply_liquidation does balance -= fee. NEVER abs(), NEVER negate.
        fee: f(item, "execFee"),
        ts: i(item, "execTime"),
        trade_id: s(item, "execId").into(),
        // Stamped at the MOUNT, never here — a bridge holds one credential set and knows nothing
        // about accounts. See `vike_model::events::PositionLiquidated::route_key`.
        route_key: None,
    })
}

/// Perp dispatch: liquidation execTypes → PositionLiquidated ONLY; Trade fills enriched
/// with markPrice + position_side (wrap.fill stays identical to the bare fill).
pub fn map_bybit_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(topic) = frame.get("topic").and_then(|t| t.as_str()) else {
        return Vec::new();
    };
    let no_rows = vec![];
    let data = frame.get("data").and_then(|d| d.as_array()).unwrap_or(&no_rows);
    let mut events = Vec::new();
    match topic {
        // Fast-fill hint (opt-in): the low-latency `execution.fast` twin → early bare Fill only.
        // No exec-type/liquidation branch — the fast stream is fills-only (bare Fill, mark/pside
        // absent so None/BOTH); the slow `execution` twin below still carries the enriched terminal.
        t if is_fast_exec_topic(t) => {
            for item in data {
                events.extend(map_execution_fast(item, venue, symbol));
            }
        }
        "execution" => {
            for item in data {
                let et = item.get("execType").and_then(|e| e.as_str());
                if matches!(et, Some("BustTrade") | Some("AdlTrade")) {
                    events.push(liquidation_event(item, venue, symbol));
                    continue; // liquidation -> PositionLiquidated ONLY
                }
                let pside = pside_from_idx(item);
                let mark = match item.get("markPrice") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(x)) if x.is_empty() => None,
                    Some(Value::String(x)) => x.parse::<f64>().ok(),
                    Some(Value::Number(n)) => n.as_f64(),
                    Some(_) => None,
                };
                for ev in map_execution(item, venue, symbol) {
                    events.push(match ev {
                        Event::Fill(mut fe) => {
                            fe.mark_price = mark;
                            fe.position_side = pside.clone().into();
                            Event::Fill(fe)
                        }
                        Event::OrderFilled(mut w) => {
                            w.fill.mark_price = mark;
                            w.fill.position_side = pside.clone().into();
                            Event::OrderFilled(w)
                        }
                        Event::OrderPartiallyFilled(mut w) => {
                            w.fill.mark_price = mark;
                            w.fill.position_side = pside.clone().into();
                            Event::OrderPartiallyFilled(w)
                        }
                        other => other,
                    });
                }
            }
        }
        "order" => {
            for item in data {
                events.extend(map_order(item, venue, symbol));
            }
        }
        "wallet" => events.extend(wallet_account_state(frame, venue)),
        _ => {}
    }
    events
}
