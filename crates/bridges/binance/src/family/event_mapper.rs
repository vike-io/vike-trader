//! Pure Binance-grammar executionReport → vike event mapper (no socket; golden-gated with
//! Python-scripted frames). Exact port of `exec/binance/mapper.py`. Shared by vike-binance
//! (`crate::event_mapper`) and vike-aster (`vike_aster::event_mapper`) — Aster's spot user-data
//! stream is Binance-verbatim (same field letters, listenKey transport), so both venues call
//! straight into this, each passing its own `venue`.
//!
//! The WS executionReport is the SOLE source of truth for fills. On x=TRADE we emit BOTH
//! a bare FillEvent (the Account folds it) AND the wrapping OrderPartiallyFilled/
//! OrderFilled (the FSM registry applies it) — the dual-publish contract. Commission is
//! carried on FillEvent.commission, NEVER netted into last_px. trade_id = venue `t`
//! (the reconnect dedup key); client_order_id = `c` on NEW/TRADE.
//!
//! Coid source is STATUS-DEPENDENT (live-capture finding, PR #604). On a report that
//! TERMINATES a resting order via cancel — CANCELED, and the cancel-triggered
//! EXPIRED/REJECTED — Binance puts the RESTING order's coid in `C` (origClientOrderId) and
//! the cancel *request's* own auto-generated id in `c`. Reading `c` there would decode the
//! event under the throwaway cancel-request id, which the OMS can't correlate back to the
//! resting order. So the cancel-family arms resolve the coid from `C`, falling back to `c`
//! only when `C` is empty (a natural GTD/IOC expiry or a placement-time reject never rested,
//! so `C` is empty and `c` IS its coid). See `resting_coid`.
//!
//! **Broker-prefix strip (unified cross-venue attribution, task 6, INBOUND half).** Binance
//! echoes back whatever `newClientOrderId` it stored, so once a link id is configured
//! `crate::family::order_map::binance_broker_coid` has prefixed the outbound id
//! (`x-<link_id>-<coid>`) — every `c`/`C` read here is passed through the paired
//! [`crate::family::order_map::strip_broker_coid_prefix`] before it becomes an event's
//! `client_order_id`, or the local registry (keyed by the BARE coid) could never match the
//! report again. The strip is unconditional and safe even when unconfigured (Aster included —
//! its callers never prefix): a bare local coid can never itself start with `x-...-`.

use serde_json::Value;
// Loose venue-JSON coercion: the shared keyed accessors, under this module's short local names.
// These wrappers were re-declared per venue; `vike_bridge_core::json` is the one home. `s` keeps
// `json_str`'s Bool arm here — this mapper's spot executionReport has no bool-valued field read
// through it, unlike perp_mapper's deliberately Bool-less `s`.
use vike_bridge_core::json::{get_f64 as f, get_i64 as i, get_str as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected,
    TradeId,
};

use crate::family::order_map::strip_broker_coid_prefix;

/// The RESTING order's coid for a cancel-family executionReport. Binance carries it in `C`
/// (origClientOrderId) on CANCELED / cancel-triggered EXPIRED / REJECTED — with `c` holding the
/// cancel *request's* own auto-generated id — but leaves `C` empty on NEW/TRADE and on a natural
/// (non-cancel) expiry or a placement-time reject, where `c` IS the resting coid. So prefer `C`,
/// fall back to `c`. This is the id the OMS correlates a cancel back to the resting order by;
/// reading `c` on a cancel would surface an id nothing local can match (live-capture bug, #604).
/// Broker-prefix stripped (task 6) — see this module's doc.
fn resting_coid(frame: &Value) -> String {
    let orig = s(frame, "C");
    let raw = if orig.is_empty() { s(frame, "c") } else { orig };
    strip_broker_coid_prefix(&raw).to_string()
}

/// Map one executionReport to events (empty for an unknown exec type). The WS stream is
/// account-wide; the frame's `s` field carries the true order symbol (fall back to the
/// passed `symbol` only when absent).
pub fn map_execution_report(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let symbol = match frame.get("s") {
        Some(Value::String(v)) => v.clone(),
        _ => symbol.to_string(),
    };
    // Broker-prefix stripped (task 6) — see this module's doc.
    let coid = strip_broker_coid_prefix(&s(frame, "c")).to_string();
    let ts = i(frame, "T");
    match frame.get("x").and_then(|x| x.as_str()) {
        Some("NEW") => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some(s(frame, "i").into()),
            ts,
        })],
        Some("CANCELED") => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: resting_coid(frame),
            reason: s(frame, "r").into(),
            ts,
        })],
        Some("REJECTED") => vec![Event::OrderRejected(OrderRejected {
            client_order_id: resting_coid(frame),
            reason: s(frame, "r").into(),
            ts,
        })],
        Some("EXPIRED") => {
            vec![Event::OrderExpired(OrderExpired { client_order_id: resting_coid(frame), ts })]
        }
        Some("TRADE") => {
            // `t` (tradeId) is the fill DEDUP key, and Binance documents it on every
            // executionReport whose `x` is TRADE — so an absent/empty `t` here is a malformed
            // frame, not a shape the venue has. DROP the whole fill rather than fold an
            // un-dedupable one: the audit-A3 resync replays this same fill out of `myTrades`
            // (`crate::family::history`'s `map_history`), and with no id the engine's
            // `seen_trade_ids` guard cannot collapse the pair — qty, commission and realized PnL
            // would all be booked TWICE. The bare `Event::Fill` and the `OrderFilled`/
            // `OrderPartiallyFilled` wrap `vike_bridge_core::terminal_events` mints from it stand
            // or fall TOGETHER: publishing the wrap alone would terminalize the FSM for a fill the
            // Account never folded. No id is synthesized — nothing else on this frame is a unique
            // key, and a clock/counter id differs on the second run, which defeats dedup outright.
            let Ok(trade_id) = TradeId::new(s(frame, "t")) else {
                tracing::warn!(
                    venue,
                    symbol = %symbol,
                    client_order_id = %coid,
                    "executionReport x=TRADE carries no `t` (tradeId) — dropping the fill and its \
                     wrap; an un-dedupable fill double-books commission and realized PnL when the \
                     reconnect resync replays it"
                );
                return Vec::new();
            };
            let fill = FillEvent {
                trade_id,
                client_order_id: coid.clone(),
                venue: venue.to_string().into(),
                symbol: symbol.into(),
                side: if frame.get("S").and_then(|v| v.as_str()) == Some("BUY") { 1 } else { -1 },
                last_qty: f(frame, "l"),
                last_px: f(frame, "L"),
                commission: f(frame, "n"),
                commission_asset: s(frame, "N").into(),
                liquidity_side: if frame.get("m").and_then(|m| m.as_bool()).unwrap_or(false) {
                    LiquiditySide::Maker
                } else {
                    LiquiditySide::Taker
                },
                ts,
                mark_price: None,
                position_side: "BOTH".to_string().into(),
            };
            let is_filled = frame.get("X").and_then(|x| x.as_str()) == Some("FILLED");
            vike_bridge_core::terminal_events(coid, fill, ts, is_filled)
        }
        _ => Vec::new(),
    }
}

/// Dispatch a Binance-grammar WS user-data frame → events:
/// non-object / ACK echo (top-level status|result|error) → [];
/// unwrap the `{"subscriptionId":0,"event":{...}}` envelope (tolerate raw listenKey frames,
/// which carry no envelope — `.get("event")` falls back to the frame itself);
/// `e == "executionReport"` → map_execution_report;
/// `e == "outboundAccountPosition"` → AccountState (full snapshot; balanceUpdate is
/// deliberately NOT mapped — a single-asset delta, not a snapshot); anything else → [].
///
/// Re-exported as `map_binance_private` / `map_aster_private` by the two venues.
pub fn map_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(obj) = frame.as_object() else {
        return Vec::new();
    };
    if obj.contains_key("status") || obj.contains_key("result") || obj.contains_key("error") {
        return Vec::new();
    }
    let inner = frame.get("event").unwrap_or(frame);
    if !inner.is_object() {
        return Vec::new();
    }
    match inner.get("e").and_then(|e| e.as_str()) {
        Some("executionReport") => map_execution_report(inner, venue, symbol),
        Some("outboundAccountPosition") => {
            // Spot balance snapshot: B[].{a=asset, f=free, l=locked}; total = free+locked.
            // Python wraps the row in try/except (TypeError, ValueError) — an unparseable
            // f/l SKIPS THE WHOLE ROW, it does not default to 0.
            let ts = i(inner, "E");
            let strict = |row: &Value, key: &str| -> Option<f64> {
                match row.get(key) {
                    None => Some(0.0), // Python .get(key, 0) default
                    Some(Value::Number(n)) => n.as_f64(),
                    Some(Value::String(v)) => v.parse::<f64>().ok(), // float("bad") raises
                    Some(Value::Null) => Some(0.0),                  // float(None or 0) = 0.0
                    Some(_) => None,                                 // float(dict/list) raises
                }
            };
            let mut balances: Vec<(String, f64)> = Vec::new();
            for row in inner.get("B").and_then(|b| b.as_array()).unwrap_or(&vec![]) {
                let asset = row.get("a").and_then(|a| a.as_str()).unwrap_or("");
                let (Some(free), Some(locked)) = (strict(row, "f"), strict(row, "l")) else {
                    continue; // row skipped, Python-style
                };
                if !asset.is_empty() {
                    balances.push((asset.to_string(), free + locked));
                }
            }
            if balances.is_empty() {
                Vec::new()
            } else {
                vec![Event::AccountState(AccountState {
                    venue: venue.to_string().into(),
                    balances,
                    ts,
                    // A bridge holds ONE credential set and knows no account labels: the MOUNT stamps
                    // the route key (`vike_mount::account_event_sender`), never a venue adapter.
                    route_key: None,
                })]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    //! Pins the STATUS-DEPENDENT coid source (live-capture bug #604): cancel-family reports read
    //! `C` (origClientOrderId, the resting order's coid), NEW/TRADE read `c`.
    use super::*;
    use serde_json::json;

    fn coid_of(ev: &Event) -> &str {
        match ev {
            Event::OrderAccepted(a) => &a.client_order_id,
            Event::OrderCanceled(c) => &c.client_order_id,
            Event::OrderRejected(r) => &r.client_order_id,
            Event::OrderExpired(e) => &e.client_order_id,
            Event::Fill(f) => &f.client_order_id,
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// A CANCELED report carries the resting coid in `C` and the throwaway cancel-request id in
    /// `c` — the mapper must decode `OrderCanceled` under `C`, never `c`.
    #[test]
    fn canceled_takes_resting_coid_from_capital_c() {
        let frame = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "CANCELED", "X": "CANCELED",
            "C": "RESTING-123", "c": "cancel-req-xyz", "r": "NONE", "T": 7, "i": 9
        });
        let events = map_execution_report(&frame, "binance", "BTCUSDT");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::OrderCanceled(_)));
        assert_eq!(coid_of(&events[0]), "RESTING-123", "cancel coid must come from `C`");
    }

    /// A cancel-triggered REJECTED / EXPIRED likewise carries the resting coid in `C`.
    #[test]
    fn cancel_triggered_reject_and_expire_take_capital_c() {
        for x in ["REJECTED", "EXPIRED"] {
            let frame = json!({
                "e": "executionReport", "s": "BTCUSDT", "x": x, "X": x,
                "C": "RESTING-9", "c": "req-throwaway", "r": "NONE", "T": 1, "i": 2
            });
            let events = map_execution_report(&frame, "binance", "BTCUSDT");
            assert_eq!(coid_of(&events[0]), "RESTING-9", "{x}: coid must come from `C`");
        }
    }

    /// A natural (non-cancel) expiry / placement-reject leaves `C` empty, so the coid falls back
    /// to `c` — the resting order's own id.
    #[test]
    fn expire_and_reject_fall_back_to_c_when_capital_c_empty() {
        for x in ["EXPIRED", "REJECTED"] {
            // `C` absent
            let f1 = json!({"e":"executionReport","s":"BTCUSDT","x":x,"X":x,"c":"own-coid","r":"NONE","T":1,"i":2});
            assert_eq!(coid_of(&map_execution_report(&f1, "binance", "BTCUSDT")[0]), "own-coid");
            // `C` present but empty
            let f2 = json!({"e":"executionReport","s":"BTCUSDT","x":x,"X":x,"C":"","c":"own-coid","r":"NONE","T":1,"i":2});
            assert_eq!(coid_of(&map_execution_report(&f2, "binance", "BTCUSDT")[0]), "own-coid");
        }
    }

    /// Regression guard: NEW and TRADE ALWAYS read `c` — an incidental `C` on those reports must
    /// NOT hijack the coid.
    #[test]
    fn new_and_trade_always_read_c() {
        let new = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "NEW", "X": "NEW",
            "C": "should-be-ignored", "c": "the-coid", "T": 1, "i": 9
        });
        assert_eq!(coid_of(&map_execution_report(&new, "binance", "BTCUSDT")[0]), "the-coid");

        let trade = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "TRADE", "X": "FILLED",
            "C": "should-be-ignored", "c": "the-coid", "S": "BUY",
            "l": "1.0", "L": "100.0", "n": "0.1", "N": "USDT", "t": "42", "m": false, "T": 1
        });
        let events = map_execution_report(&trade, "binance", "BTCUSDT");
        // [Fill, OrderFilled] — both under `c`.
        assert_eq!(coid_of(&events[0]), "the-coid", "fill coid reads `c`");
        let Event::OrderFilled(w) = &events[1] else { panic!("expected OrderFilled: {events:?}") };
        assert_eq!(w.client_order_id, "the-coid", "wrap coid reads `c`");
    }

    /// Unified cross-venue attribution (task 6) round-trip: a venue that echoes back the
    /// broker-prefixed `newClientOrderId` (`x-<link_id>-<coid>`, what
    /// `crate::family::order_map::binance_broker_coid` stamped on submit) must decode to the BARE
    /// local coid the registry keys orders by — on NEW (reads `c`) and on CANCELED (reads `C`,
    /// with an unrelated throwaway `c`).
    #[test]
    fn broker_prefixed_coid_round_trips_to_bare_on_new_and_canceled() {
        let prefixed = crate::family::order_map::binance_broker_coid(Some("ABC123"), "deadbeef01");
        assert_eq!(prefixed, "x-ABC123-deadbeef01");

        let new = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "NEW", "X": "NEW",
            "c": prefixed, "T": 1, "i": 9
        });
        assert_eq!(
            coid_of(&map_execution_report(&new, "binance", "BTCUSDT")[0]),
            "deadbeef01",
            "NEW must decode the prefixed `c` back to the bare local coid"
        );

        let canceled = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "CANCELED", "X": "CANCELED",
            "C": prefixed, "c": "cancel-req-xyz", "r": "NONE", "T": 7, "i": 9
        });
        assert_eq!(
            coid_of(&map_execution_report(&canceled, "binance", "BTCUSDT")[0]),
            "deadbeef01",
            "CANCELED must decode the prefixed `C` back to the bare local coid"
        );

        // A bare (unconfigured / Aster) coid passes through untouched.
        let unconfigured = json!({
            "e": "executionReport", "s": "BTCUSDT", "x": "NEW", "X": "NEW",
            "c": "deadbeef01", "T": 1, "i": 9
        });
        assert_eq!(
            coid_of(&map_execution_report(&unconfigured, "binance", "BTCUSDT")[0]),
            "deadbeef01"
        );
    }
}
