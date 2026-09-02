//! Hyperliquid exec JSON -> the canonical [`vike_model::events::Event`]. Pure, fixture-tested.
//!
//! Ports the wire semantics in `docs/research/2026-07-16-hyperliquid-adapters/README.md` sec 6
//! (order lifecycle) + sec 10 (the 29-value order-status enumeration). No I/O, no `unsafe`; every
//! function is a pure map over borrowed `serde_json` (deps: `serde_json`, `tiny-keccak`, `hex`,
//! `vike-model` — plus `tracing` for the one diagnostic below, which is a `warn!` on a MALFORMED
//! frame, i.e. per-fault and never per-message).
//!
//! ## A fill with no venue id is REFUSED, not folded
//!
//! Both fold lanes below dedup on [`vike_model::events::TradeId`], so a fill that carries no venue
//! id cannot be deduplicated at all — it re-books position and realized PnL on every reconnect
//! replay. Every `trade_id` here therefore goes through the fallible `TradeId::new`, and an
//! id-less fill yields NO event (`None` / a skipped row) with a `warn!` naming the missing field.
//! HL's schema makes both ids (`userFills.tid`, `orderUpdates.oid`) mandatory, so these paths are
//! expected to stay at zero; they exist because a malformed frame must cost a dropped fill, never a
//! double-booked one. Nothing here synthesizes a replacement id: the only other per-fill fields
//! (size / price / timestamp) REPEAT verbatim on a replay, so a synthetic id built from them would
//! collide across distinct fills while still failing to collapse the replay.
//!
//! ## Three sources, mapped onto the engine's TWO fold lanes
//!
//! The [`vike_exec`] engine folds venue events on two independent lanes, each with its own reconnect
//! dedup set: a bare [`Event::Fill`] drives `Account` (positions + PnL, keyed on `trade_id`), while
//! the `Order*` lifecycle events drive the `ManagedOrder` FSM (the `OrderFilled`/`OrderPartiallyFilled`
//! wraps carry a fill for `filled_qty`/`avg_fill_px` ONLY, keyed on a SEPARATE set). Hyperliquid's
//! surfaces line up cleanly onto those lanes:
//!
//! - `/exchange` order response (synchronous submit ack, positional per submitted order) ->
//!   FSM lane. `resting` -> `OrderAccepted` (captures the venue `oid`); `filled` -> `OrderFilled` /
//!   `OrderPartiallyFilled`; `error` -> `OrderRejected`; `waitingForTrigger`/`waitingForFill` ->
//!   `OrderAccepted` (resting).
//! - WS `orderUpdates` -> FSM lane. Status mapped by SUFFIX over the 29-value enum; an unknown
//!   status is a soft no-op ([`None`]), NEVER a panic (HL grows the enum over time).
//! - WS `userFills` -> the Account lane: bare [`FillEvent`]s (the authoritative economics: real
//!   exec price, `fee`, `tid`). The `isSnapshot` flag is surfaced so the pump can drop the initial
//!   replay it has already processed.
//!
//! Account is folded ONLY from `userFills` (real `tid` keys `seen_trade_ids`); the FSM `filled`
//! wraps key their own set on the `oid`, so the two lanes never double-count each other. The FSM
//! wrap fills built from the response / `orderUpdates` (which carry no exec fee) are lifecycle
//! markers whose price is the request/limit price; the money-true fill always arrives via
//! `userFills`.
//!
//! ## coin / cloid carry-through
//!
//! Symbols are NOT resolved here: the venue `coin` string is carried verbatim into
//! `FillEvent.symbol`, and the caller (user_data/exec) remaps coin -> unified symbol via
//! [`crate::symbology`]. Likewise, WS lifecycle events carry the venue `cloid` (`0x...`) in
//! `client_order_id`; the caller remaps cloid -> its framework client-order-id (it holds both,
//! having derived the cloid via [`cloid_from_client_order_id`] at submit). The `/exchange` response
//! is positional and the caller already knows each slot's real coid, so it is passed in via
//! [`SubmittedOrder`].

use serde_json::Value;
use tiny_keccak::{Hasher, Keccak};
use vike_model::events::{
    Event, FillEvent, LiquiditySide, OrderAccepted, OrderCanceled, OrderFilled,
    OrderPartiallyFilled, OrderRejected, OrderTriggered, PositionSide, TradeId,
};

/// Immediate-fill full-vs-partial classification slack (HL sizes are exact decimals; this only
/// guards last-ulp wire re-encode of `totalSz` against the requested size).
const FILL_EPS: f64 = 1e-9;

// --- loose venue-JSON coercion (HL puts px/sz/fee on the wire as decimal STRINGS, ids/times as
// numbers) -------------------------------------------------------------------------------------

/// String-or-number field -> f64 (0.0 if absent/unparseable).
fn num(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Owned string field ("" if absent/non-string).
fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string).unwrap_or_default()
}

/// Integer-ish field -> i64 (0 if absent). Accepts a number or a numeric string.
fn int(v: &Value, key: &str) -> i64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => {
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64)).unwrap_or(0)
        }
        _ => 0,
    }
}

/// An id field (`oid`/`tid`) -> its canonical decimal string (HL sends them as JSON integers);
/// "" if absent.
fn id_str(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// HL side field: `"A"` (ask) = sell = -1; `"B"` (bid) and anything else = buy = +1.
fn side_sign(v: &Value) -> i32 {
    match v.get("side").and_then(|s| s.as_str()) {
        Some("A") => -1,
        _ => 1,
    }
}

/// The client-order key to carry on a WS event: the venue `cloid` (`0x...`, what exec set
/// deterministically at submit -> the caller remaps cloid to its coid) when present, else the `oid`
/// as a string so the key is never empty (an oid-keyed event the engine drops as an unknown coid).
fn order_coid(order: &Value) -> String {
    let cloid = text(order, "cloid");
    if cloid.is_empty() {
        id_str(order, "oid")
    } else {
        cloid
    }
}

// --- 1. `/exchange` order response (synchronous submit ack) ------------------------------------

/// Per-submitted-order context the caller carries positionally: the `/exchange` response echoes
/// none of it back, so slot `i`'s identity/side/size come from the order we sent.
#[derive(Debug, Clone)]
pub struct SubmittedOrder {
    /// Our framework client-order-id (goes straight onto the resulting events).
    pub client_order_id: String,
    /// The venue `coin` for this order -> carried into the fill's `symbol` (caller remaps).
    pub coin: String,
    /// +1 buy / -1 sell, from the submitted order.
    pub side: i32,
    /// The requested size -> classifies an immediate `filled` as full vs partial.
    pub req_sz: f64,
    /// Submit-time ts (ms) stamped onto the resulting events.
    pub ts: i64,
}

/// Locate the positional `statuses` array across the documented response envelopes:
/// `{status:"ok", response:{data:{statuses:[..]}}}`, or a bare `{data:{statuses:[..]}}`, or
/// `{statuses:[..]}`.
fn find_statuses(resp: &Value) -> Option<&Vec<Value>> {
    resp.get("response")
        .and_then(|r| r.get("data"))
        .and_then(|d| d.get("statuses"))
        .and_then(|s| s.as_array())
        .or_else(|| resp.get("data").and_then(|d| d.get("statuses")).and_then(|s| s.as_array()))
        .or_else(|| resp.get("statuses").and_then(|s| s.as_array()))
}

/// Map one `/exchange` `statuses[i]` -> an FSM lifecycle event, using the positional
/// [`SubmittedOrder`] for the identity/side/size the response omits. [`None`] only for an
/// unrecognized status shape (never panics).
pub fn map_order_status(status: &Value, venue: &str, order: &SubmittedOrder) -> Option<Event> {
    // String statuses: a trigger/limit order accepted and resting (waiting to trigger / fill).
    if let Some(s) = status.as_str() {
        return match s {
            "waitingForTrigger" | "waitingForFill" => Some(Event::OrderAccepted(OrderAccepted {
                client_order_id: order.client_order_id.clone(),
                venue_order_id: None,
                ts: order.ts,
            })),
            _ => None, // unrecognized string status -> soft no-op
        };
    }
    // Object statuses.
    if let Some(resting) = status.get("resting") {
        let oid = id_str(resting, "oid");
        return Some(Event::OrderAccepted(OrderAccepted {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: (!oid.is_empty()).then(|| oid.into()),
            ts: order.ts,
        }));
    }
    if let Some(filled) = status.get("filled") {
        let total_sz = num(filled, "totalSz");
        // The FSM wrap's dedup key is the venue `oid` (module doc: it collapses against the
        // `orderUpdates` `filled` wrap, which keys on the SAME oid). Without one the wrap cannot
        // dedup, so a reconnect-replayed `filled` would re-add `totalSz` to `filled_qty` and move
        // `avg_fill_px` again. Refuse the whole status — this function's documented `None` is
        // exactly "unrecognized status shape", and a `filled` with no order id is one.
        let trade_id = match TradeId::new(id_str(filled, "oid")) {
            Ok(t) => t,
            Err(_) => {
                tracing::warn!(
                    venue,
                    coid = %order.client_order_id,
                    "/exchange `filled` status carries no `oid` — dropping the fill wrap; an \
                     un-dedupable wrap would re-add filled_qty on every replay"
                );
                return None;
            }
        };
        let fill = FillEvent {
            trade_id,
            client_order_id: order.client_order_id.clone(),
            venue: venue.into(),
            symbol: order.coin.as_str().into(),
            side: order.side,
            last_qty: total_sz,
            last_px: num(filled, "avgPx"),
            commission: 0.0, // fee not in the /exchange response (userFills carries it)
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Unknown,
            ts: order.ts,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        // Full (or unknown requested size) -> terminal Filled; else a resting PartiallyFilled.
        let full = order.req_sz <= 0.0 || total_sz + FILL_EPS >= order.req_sz;
        return Some(if full {
            Event::OrderFilled(OrderFilled {
                client_order_id: order.client_order_id.clone(),
                fill,
                ts: order.ts,
            })
        } else {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: order.client_order_id.clone(),
                fill,
                ts: order.ts,
            })
        });
    }
    if let Some(err) = status.get("error") {
        return Some(Event::OrderRejected(OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason: err.as_str().unwrap_or("order rejected").into(),
            ts: order.ts,
        }));
    }
    None // unrecognized object shape -> soft no-op
}

/// Map a full `/exchange` order response -> FSM lifecycle events, positionally zipped with the
/// orders we submitted. A top-level `{status:"err", response:"<str>"}` batch failure rejects EVERY
/// submitted order with the shared reason (no order silently vanishes). An unrecognized ok-envelope
/// (no `statuses`) yields no events — the exec-side watchdog owns that fault.
pub fn map_order_response(resp: &Value, venue: &str, orders: &[SubmittedOrder]) -> Vec<Event> {
    if resp.get("status").and_then(|s| s.as_str()) == Some("err") {
        let reason =
            resp.get("response").and_then(|r| r.as_str()).unwrap_or("order request failed");
        return orders
            .iter()
            .map(|o| {
                Event::OrderRejected(OrderRejected {
                    client_order_id: o.client_order_id.clone(),
                    reason: reason.into(),
                    ts: o.ts,
                })
            })
            .collect();
    }
    match find_statuses(resp) {
        Some(statuses) => statuses
            .iter()
            .zip(orders.iter())
            .filter_map(|(st, o)| map_order_status(st, venue, o))
            .collect(),
        None => Vec::new(),
    }
}

// --- 2. WS `orderUpdates` (FSM lifecycle lane) -------------------------------------------------

/// Status -> canceled, by the `*Canceled` suffix (base `"canceled"` + every prefixed variant:
/// margin/selfTrade/reduceOnly/siblingFilled/delisted/liquidated/...Canceled) PLUS the one cancel
/// status HL spells without the `-ed`: `"scheduledCancel"` (the dead-man's-switch trip).
fn is_cancel(status: &str) -> bool {
    status == "canceled" || status.ends_with("Canceled") || status == "scheduledCancel"
}

/// Status -> rejected, by the `*Rejected` suffix (base `"rejected"` + tick/minTradeNtl/perpMargin/
/// reduceOnly/badAloPx/... Rejected).
fn is_reject(status: &str) -> bool {
    status == "rejected" || status.ends_with("Rejected")
}

/// Build the fill an `orderUpdates` `filled` -> `OrderFilled` wrap carries. This drives the FSM
/// terminal + its `filled_qty`/`avg_fill_px` ONLY — it never folds into `Account` (the engine
/// routes bare `Event::Fill`, sourced from `userFills`, there). `orderUpdates` carries no exec
/// price/fee/trade-id, so qty = `origSz` (the full filled size; fallback `sz`), px = the order
/// `limitPx` (a lifecycle approximation), trade_id = `oid` (dedups against the `/exchange`
/// immediate-fill wrap, which uses the same oid). Authoritative economics arrive via `userFills`.
///
/// [`None`] when the row carries no `oid`: that id IS the wrap's dedup key, so folding one without
/// it would re-add `filled_qty` on every reconnect replay of the same `filled` row. `oid` is
/// mandatory in HL's `orderUpdates` schema, so this is a malformed-frame path.
fn order_fill(order: &Value, venue: &str, coid: &str, ts: i64) -> Option<FillEvent> {
    let orig = num(order, "origSz");
    let sz = if orig != 0.0 { orig } else { num(order, "sz") };
    let trade_id = match TradeId::new(id_str(order, "oid")) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                venue,
                %coid,
                "`orderUpdates` filled row carries no `oid` — dropping the fill wrap; an \
                 un-dedupable wrap would re-add filled_qty on every replay"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: text(order, "coin").as_str().into(),
        side: side_sign(order),
        last_qty: sz,
        last_px: num(order, "limitPx"),
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Unknown,
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// Map one `orderUpdates` row (`{order:{coin,side,limitPx,sz,oid,cloid?,timestamp}, status,
/// statusTimestamp}`) -> an FSM lifecycle event, keyed by the status SUFFIX over the 29-value enum.
/// [`None`] for an unknown status — a SOFT no-op, NEVER a hard error (HL adds statuses over time).
/// The row's `coin` rides on any fill's `symbol` (caller remaps); the `cloid` (else `oid`) rides on
/// `client_order_id` (caller remaps cloid to its coid).
pub fn map_order_update(row: &Value, venue: &str) -> Option<Event> {
    let status = row.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let null = Value::Null;
    let order = row.get("order").unwrap_or(&null);
    let coid = order_coid(order);
    // Prefer the status-change ts; fall back to the order's own timestamp.
    let ts = {
        let t = int(row, "statusTimestamp");
        if t != 0 {
            t
        } else {
            int(order, "timestamp")
        }
    };

    if status == "open" || status == "resting" {
        let oid = id_str(order, "oid");
        return Some(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: (!oid.is_empty()).then(|| oid.into()),
            ts,
        }));
    }
    if status == "triggered" {
        return Some(Event::OrderTriggered(OrderTriggered { client_order_id: coid, ts }));
    }
    if status == "filled" {
        // No `oid` ⇒ no dedup key ⇒ no event at all (see [`order_fill`]). The FSM keeps the order
        // live rather than terminalizing it off an un-dedupable wrap; the money-true fill still
        // arrives on the `userFills` lane, and the confirm/recon watchdogs own the stuck order.
        let fill = order_fill(order, venue, &coid, ts)?;
        return Some(Event::OrderFilled(OrderFilled { client_order_id: coid, fill, ts }));
    }
    if is_cancel(status) {
        return Some(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: status.into(),
            ts,
        }));
    }
    if is_reject(status) {
        return Some(Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: status.into(),
            ts,
        }));
    }
    None // unknown status -> soft no-op (never panic)
}

/// The row array of a WS frame: `data` when the frame is the `{channel, data:[..]}` envelope, else
/// the value itself when it is already an array; empty otherwise.
fn rows_of(frame: &Value) -> &[Value] {
    frame
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| frame.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Map a full `orderUpdates` frame (`{channel, data:[rows]}` or a bare `[rows]`) -> FSM lifecycle
/// events. Unknown-status rows drop out (soft no-op), so the result is only the recognized lifecycle.
pub fn map_order_updates(frame: &Value, venue: &str) -> Vec<Event> {
    rows_of(frame).iter().filter_map(|row| map_order_update(row, venue)).collect()
}

// --- 3. WS `userFills` (Account fold lane) -----------------------------------------------------

/// A mapped `userFills` batch: the bare [`FillEvent`]s (for the `Event::Fill` / `Account` lane)
/// plus the `is_snapshot` flag the WS pump needs.
#[derive(Debug, Clone, PartialEq)]
pub struct UserFills {
    /// True for the initial `userFills` snapshot frame (HL replays recent fills on subscribe). The
    /// caller drops it once it has processed the snapshot, so fills never double-fold.
    pub is_snapshot: bool,
    /// The fills, in frame order. The caller wraps each as `Event::Fill` for the account fold.
    pub fills: Vec<FillEvent>,
}

/// One `userFills` fill row -> a bare [`FillEvent`] (Account lane). `side` "A" = sell / "B" = buy;
/// `crossed` true -> taker, false -> maker; `fee` is SIGNED into `commission` (HL: positive = cost,
/// negative = rebate — the vike convention); `feeToken` -> `commission_asset`; `tid` -> `trade_id`;
/// `coin` -> `symbol` (caller remaps); `cloid` (else `oid`) -> `client_order_id` (caller remaps).
///
/// [`None`] when `tid` is absent/null/empty. This is the ACCOUNT lane — `tid` is the key
/// `seen_trade_ids` holds, and an id-less fill is the one shape that guard cannot collapse, so a
/// reconnect snapshot would re-fold it and double-count position + realized PnL without bound. The
/// row is refused here rather than downstream, because the type makes an id-less `FillEvent`
/// unconstructible; `oid` is deliberately NOT used as a substitute (one order yields many fills, so
/// oid-keyed fills would collapse into each other and LOSE real economics).
fn map_one_fill(f: &Value, venue: &str) -> Option<FillEvent> {
    let crossed = f.get("crossed").and_then(|c| c.as_bool()).unwrap_or(false);
    let trade_id = TradeId::new(id_str(f, "tid")).ok()?;
    Some(FillEvent {
        trade_id,
        client_order_id: order_coid(f),
        venue: venue.into(),
        symbol: text(f, "coin").as_str().into(),
        side: side_sign(f),
        last_qty: num(f, "sz"),
        last_px: num(f, "px"),
        commission: num(f, "fee"),
        commission_asset: text(f, "feeToken").as_str().into(),
        liquidity_side: if crossed { LiquiditySide::Taker } else { LiquiditySide::Maker },
        ts: int(f, "time"),
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// Map a `userFills` frame (`{isSnapshot?, fills:[..]}`, optionally wrapped in `{channel, data:{..}}`)
/// -> bare [`FillEvent`]s plus the surfaced `isSnapshot` flag. See [`map_one_fill`] for the per-fill
/// field mapping.
/// A row with no `tid` is SKIPPED and counted into one `warn!` per frame — see [`map_one_fill`] for
/// why an id-less fill must not reach the Account fold. The drop is unconditional (not
/// replay-only): the engine's `seen_trade_ids` cannot hold an id that does not exist, so admitting
/// one on first connect only defers the double-count to the first reconnect.
pub fn map_user_fills(frame: &Value, venue: &str) -> UserFills {
    // Unwrap the {channel, data:{..}} envelope if present; else the frame is the body itself.
    let body = frame.get("data").unwrap_or(frame);
    let is_snapshot = body.get("isSnapshot").and_then(|b| b.as_bool()).unwrap_or(false);
    let rows = body.get("fills").and_then(|f| f.as_array()).map(|a| a.as_slice()).unwrap_or(&[]);
    let fills: Vec<FillEvent> = rows.iter().filter_map(|f| map_one_fill(f, venue)).collect();
    let untagged = rows.len() - fills.len();
    if untagged > 0 {
        tracing::warn!(
            venue,
            dropped = untagged,
            is_snapshot,
            "`userFills` rows carry no `tid` — dropped; the engine cannot dedup an untagged fill, \
             so folding one would double-count position and realized PnL on every reconnect. HL's \
             schema makes `tid` mandatory, so this is a malformed frame; VIKE_RECONCILE recovers \
             the economics if these were real fills."
        );
    }
    UserFills { is_snapshot, fills }
}

// --- 4. deterministic cloid ---------------------------------------------------------------------

/// Framework client-order-id -> the HL `cloid`: `"0x"` + the first 16 bytes of
/// `keccak256(coid.as_bytes())` as 32 lowercase hex chars (a 128-bit id). Deterministic and
/// table-free: exec sets the cloid this way at submit, and the WS mappers read it back on
/// `orderUpdates`/`userFills`. See research sec 6.
pub fn cloid_from_client_order_id(coid: &str) -> String {
    let mut k = Keccak::v256();
    k.update(coid.as_bytes());
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    format!("0x{}", hex::encode(&out[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V: &str = "hyperliquid";

    fn order(coid: &str, coin: &str, side: i32, req_sz: f64) -> SubmittedOrder {
        SubmittedOrder {
            client_order_id: coid.to_string(),
            coin: coin.to_string(),
            side,
            req_sz,
            ts: 1000,
        }
    }

    // --- 1. /exchange response ---

    #[test]
    fn response_resting_filled_error_positional() {
        let resp = json!({
            "status": "ok",
            "response": {"type": "order", "data": {"statuses": [
                {"resting": {"oid": 77738308}},
                {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 77738309}},
                {"error": "Order must have minimum value of $10."}
            ]}}
        });
        let orders = [
            order("c-rest", "ETH", 1, 0.5),
            order("c-fill", "ETH", 1, 0.02), // fully filled
            order("c-err", "PURR/USDC", -1, 3.0),
        ];
        let evs = map_order_response(&resp, V, &orders);
        assert_eq!(evs.len(), 3);

        match &evs[0] {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "c-rest");
                assert_eq!(a.venue_order_id.as_deref(), Some("77738308"));
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
        match &evs[1] {
            Event::OrderFilled(w) => {
                assert_eq!(w.client_order_id, "c-fill");
                assert_eq!(w.fill.trade_id, "77738309"); // oid keys the FSM wrap dedup
                assert_eq!(w.fill.venue, V);
                assert_eq!(w.fill.symbol, "ETH"); // coin carried through verbatim
                assert_eq!(w.fill.side, 1);
                assert_eq!(w.fill.last_qty, 0.02);
                assert_eq!(w.fill.last_px, 1891.4);
            }
            other => panic!("expected OrderFilled, got {other:?}"),
        }
        match &evs[2] {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, "c-err");
                assert!(r.reason.contains("minimum value"));
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
    }

    #[test]
    fn response_partial_fill_when_totalsz_below_requested() {
        let resp = json!({"status": "ok", "response": {"data": {"statuses": [
            {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 5}}
        ]}}});
        let orders = [order("c1", "ETH", 1, 0.05)]; // requested 0.05, only 0.02 filled
        let evs = map_order_response(&resp, V, &orders);
        match &evs[0] {
            Event::OrderPartiallyFilled(w) => {
                assert_eq!(w.fill.last_qty, 0.02);
                assert_eq!(w.client_order_id, "c1");
            }
            other => panic!("expected OrderPartiallyFilled, got {other:?}"),
        }
    }

    #[test]
    fn response_top_level_err_rejects_every_order() {
        let resp = json!({"status": "err", "response": "Insufficient margin to place order."});
        let orders = [order("a", "BTC", 1, 1.0), order("b", "ETH", -1, 2.0)];
        let evs = map_order_response(&resp, V, &orders);
        assert_eq!(evs.len(), 2);
        for (ev, coid) in evs.iter().zip(["a", "b"]) {
            match ev {
                Event::OrderRejected(r) => {
                    assert_eq!(r.client_order_id, coid);
                    assert!(r.reason.contains("Insufficient margin"));
                }
                other => panic!("expected OrderRejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn response_waiting_for_trigger_is_accepted() {
        let resp =
            json!({"status": "ok", "response": {"data": {"statuses": ["waitingForTrigger"]}}});
        let orders = [order("c1", "BTC", 1, 1.0)];
        let evs = map_order_response(&resp, V, &orders);
        match &evs[0] {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "c1");
                assert_eq!(a.venue_order_id, None); // no oid until it rests/triggers
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
    }

    // --- 2. orderUpdates ---

    fn update(status: &str, oid: u64, cloid: &str) -> Value {
        json!({
            "order": {
                "coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "0.0",
                "origSz": "0.1", "oid": oid, "cloid": cloid, "timestamp": 1234
            },
            "status": status,
            "statusTimestamp": 5678
        })
    }

    #[test]
    fn order_update_open_is_accepted_with_oid() {
        let ev = map_order_update(&update("open", 42, "0xabc"), V).expect("event");
        match ev {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "0xabc"); // cloid carried (caller remaps to coid)
                assert_eq!(a.venue_order_id.as_deref(), Some("42"));
                assert_eq!(a.ts, 5678); // statusTimestamp preferred
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
    }

    #[test]
    fn order_update_filled_is_orderfilled_from_origsz() {
        let ev = map_order_update(&update("filled", 7, "0xdef"), V).expect("event");
        match ev {
            Event::OrderFilled(w) => {
                assert_eq!(w.client_order_id, "0xdef");
                assert_eq!(w.fill.trade_id, "7"); // oid keys the FSM-wrap dedup
                assert_eq!(w.fill.last_qty, 0.1); // origSz, not the (drained) sz=0.0
                assert_eq!(w.fill.last_px, 50000.0); // limitPx (lifecycle approximation)
                assert_eq!(w.fill.side, 1); // "B" = buy
                assert_eq!(w.fill.symbol, "BTC");
            }
            other => panic!("expected OrderFilled, got {other:?}"),
        }
    }

    #[test]
    fn order_update_prefixed_canceled_by_suffix() {
        // a *Canceled variant not spelled plain "canceled"
        let ev = map_order_update(&update("marginCanceled", 9, "0x1"), V).expect("event");
        match ev {
            Event::OrderCanceled(c) => {
                assert_eq!(c.client_order_id, "0x1");
                assert_eq!(c.reason, "marginCanceled");
            }
            other => panic!("expected OrderCanceled, got {other:?}"),
        }
        // the dead-man's-switch cancel that lacks the -ed suffix
        assert!(matches!(
            map_order_update(&update("scheduledCancel", 9, "0x1"), V),
            Some(Event::OrderCanceled(_))
        ));
    }

    #[test]
    fn order_update_prefixed_rejected_by_suffix() {
        let ev = map_order_update(&update("tickRejected", 3, "0x2"), V).expect("event");
        match ev {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, "0x2");
                assert_eq!(r.reason, "tickRejected");
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
    }

    #[test]
    fn order_update_triggered() {
        assert!(matches!(
            map_order_update(&update("triggered", 1, "0x3"), V),
            Some(Event::OrderTriggered(_))
        ));
    }

    #[test]
    fn order_update_unknown_status_is_soft_none() {
        // an unmapped status (the Hummingbot #7689 crash class) must be a soft no-op, not a panic
        assert!(map_order_update(&update("someBrandNewStatus", 1, "0x4"), V).is_none());
    }

    #[test]
    fn order_update_falls_back_to_oid_when_no_cloid() {
        let row = json!({
            "order": {"coin": "BTC", "side": "A", "sz": "1.0", "oid": 555, "timestamp": 1},
            "status": "open"
        });
        match map_order_update(&row, V).expect("event") {
            Event::OrderAccepted(a) => assert_eq!(a.client_order_id, "555"),
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
    }

    #[test]
    fn order_updates_frame_maps_recognized_rows_only() {
        let frame = json!({"channel": "orderUpdates", "data": [
            update("open", 1, "0xa"),
            update("weirdUnknown", 2, "0xb"), // dropped (soft no-op)
            update("filled", 3, "0xc")
        ]});
        let evs = map_order_updates(&frame, V);
        assert_eq!(evs.len(), 2, "the unknown-status row drops out");
        assert!(matches!(evs[0], Event::OrderAccepted(_)));
        assert!(matches!(evs[1], Event::OrderFilled(_)));
    }

    // --- 3. userFills ---

    fn fills_frame(is_snapshot: bool) -> Value {
        json!({"channel": "userFills", "data": {
            "isSnapshot": is_snapshot,
            "user": "0xmaster",
            "fills": [
                { // taker BUY
                    "coin": "BTC", "px": "50000.0", "sz": "0.1", "side": "B",
                    "oid": 100, "cloid": "0xc1", "tid": 900, "fee": "2.5",
                    "feeToken": "USDC", "crossed": true, "dir": "Open Long",
                    "startPosition": "0.0", "closedPnl": "0.0", "time": 111
                },
                { // maker SELL with a rebate (negative fee)
                    "coin": "ETH", "px": "3000.0", "sz": "1.0", "side": "A",
                    "oid": 101, "tid": 901, "fee": "-0.3",
                    "feeToken": "USDC", "crossed": false, "dir": "Close Long",
                    "startPosition": "1.0", "closedPnl": "5.0", "time": 222
                }
            ]
        }})
    }

    #[test]
    fn user_fills_stream_taker_and_maker() {
        let out = map_user_fills(&fills_frame(false), V);
        assert!(!out.is_snapshot);
        assert_eq!(out.fills.len(), 2);

        let taker = &out.fills[0];
        assert_eq!(taker.trade_id, "900"); // tid keys the Account-lane dedup
        assert_eq!(taker.client_order_id, "0xc1"); // cloid carried
        assert_eq!(taker.venue, V);
        assert_eq!(taker.symbol, "BTC");
        assert_eq!(taker.side, 1); // "B" = buy
        assert_eq!(taker.last_qty, 0.1);
        assert_eq!(taker.last_px, 50000.0);
        assert_eq!(taker.commission, 2.5); // positive = cost
        assert_eq!(taker.commission_asset, "USDC");
        assert_eq!(taker.liquidity_side, LiquiditySide::Taker); // crossed = true
        assert_eq!(taker.ts, 111);
        assert_eq!(taker.position_side, PositionSide::Both);

        let maker = &out.fills[1];
        assert_eq!(maker.trade_id, "901");
        assert_eq!(maker.client_order_id, "101"); // no cloid -> oid fallback
        assert_eq!(maker.symbol, "ETH");
        assert_eq!(maker.side, -1); // "A" = sell
        assert_eq!(maker.commission, -0.3); // negative = maker rebate (SIGNED, kept as-is)
        assert_eq!(maker.liquidity_side, LiquiditySide::Maker); // crossed = false
    }

    #[test]
    fn user_fills_exposes_snapshot_flag() {
        let out = map_user_fills(&fills_frame(true), V);
        assert!(out.is_snapshot, "the initial snapshot flag must be surfaced, not dropped");
        assert_eq!(out.fills.len(), 2);
    }

    #[test]
    fn user_fills_accepts_bare_body_without_envelope() {
        // the pump may hand us the inner {isSnapshot, fills} body directly
        let body = json!({
            "isSnapshot": false,
            "fills": [{"coin": "SOL", "px": "150.0", "sz": "2.0", "side": "B",
                       "oid": 1, "tid": 2, "fee": "0.1", "feeToken": "USDC",
                       "crossed": true, "time": 5}]
        });
        let out = map_user_fills(&body, V);
        assert_eq!(out.fills.len(), 1);
        assert_eq!(out.fills[0].symbol, "SOL");
    }

    // --- 3b. the id-less fill is REFUSED on both lanes ---
    //
    // These gate the `TradeId::new` handling, not merely the type. Reverting any of the three sites
    // to a permissive id (`unwrap_or_default()`, a `""` fallback, or an id synthesized from
    // qty/px/ts) reddens them: each asserts the fill/wrap is ABSENT, so a permissive site turns the
    // count from 0 back to 1. The hazard being gated is double-booking — `seen_trade_ids` cannot
    // hold an id that does not exist, so an admitted id-less fill re-folds on every replay.

    /// `userFills` (ACCOUNT lane): a row with `tid` absent, null or `""` yields no fill at all, on
    /// an incremental frame AND on a snapshot. A tagged sibling in the same frame is unaffected.
    #[test]
    fn user_fill_without_a_tid_is_not_emitted() {
        let row = |tid: Value| {
            json!({"coin": "BTC", "px": "50000.0", "sz": "0.1", "side": "B", "oid": 100,
                   "cloid": "0xc1", "tid": tid, "fee": "1.0", "feeToken": "USDC",
                   "crossed": true, "time": 111})
        };
        let mut absent = row(Value::Null);
        absent.as_object_mut().expect("object").remove("tid");

        for (label, bad) in
            [("absent", absent), ("null", row(Value::Null)), ("empty string", row(json!("")))]
        {
            for is_snapshot in [false, true] {
                let frame = json!({"isSnapshot": is_snapshot, "fills": [bad.clone()]});
                let out = map_user_fills(&frame, V);
                assert!(
                    out.fills.is_empty(),
                    "a `tid`-{label} fill must NOT be emitted (is_snapshot={is_snapshot}): an \
                     un-dedupable fill double-books position and PnL on replay"
                );
            }
        }

        // ...and the drop is per-row, not per-frame.
        let frame = json!({"isSnapshot": false, "fills": [row(json!("")), row(json!(900))]});
        let out = map_user_fills(&frame, V);
        assert_eq!(out.fills.len(), 1, "only the untagged row drops");
        assert_eq!(out.fills[0].trade_id, "900");
    }

    /// `orderUpdates` (FSM lane): a `filled` row with no `oid` emits NOTHING — no `OrderFilled`
    /// wrap, because `oid` is that wrap's dedup key and an un-dedupable wrap re-adds `filled_qty`
    /// on every replay. Sibling rows in the same frame still map.
    #[test]
    fn order_update_filled_without_an_oid_is_not_emitted() {
        let bad = json!({
            "order": {"coin": "BTC", "side": "B", "sz": "1.0", "origSz": "1.0",
                      "limitPx": "50000.0", "cloid": "0xc9", "timestamp": 7},
            "status": "filled", "statusTimestamp": 8
        });
        assert!(
            map_order_update(&bad, V).is_none(),
            "a `filled` row with no `oid` must yield no FSM wrap"
        );

        let frame = json!({"channel": "orderUpdates", "data": [bad, update("filled", 3, "0xc")]});
        let evs = map_order_updates(&frame, V);
        assert_eq!(evs.len(), 1, "only the oid-less row drops out");
        assert!(matches!(evs[0], Event::OrderFilled(_)));
    }

    /// `/exchange` (FSM lane): a `filled` status with no `oid` yields no event for THAT slot, and
    /// the positional zip keeps every other slot's event — the emitter-split rule (no order
    /// silently vanishes) is still served by the other slots plus the exec-side watchdog.
    #[test]
    fn response_filled_without_an_oid_is_not_emitted() {
        let resp = json!({"status": "ok", "response": {"type": "order", "data": {"statuses": [
            {"filled": {"totalSz": "0.02", "avgPx": "1891.4"}},          // no oid
            {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 5}} // fine
        ]}}});
        let orders = [order("c-noid", "ETH", 1, 0.02), order("c-ok", "ETH", 1, 0.02)];
        let evs = map_order_response(&resp, V, &orders);
        assert_eq!(evs.len(), 1, "the oid-less slot emits nothing");
        match &evs[0] {
            Event::OrderFilled(w) => {
                assert_eq!(w.client_order_id, "c-ok");
                assert_eq!(w.fill.trade_id, "5");
            }
            other => panic!("expected the tagged OrderFilled, got {other:?}"),
        }
    }

    // --- 4. cloid determinism ---

    #[test]
    fn cloid_is_deterministic_and_well_formed() {
        let a = cloid_from_client_order_id("vike-000123");
        let b = cloid_from_client_order_id("vike-000123");
        assert_eq!(a, b, "same coid -> same cloid (idempotent, no mapping table)");
        assert_ne!(
            a,
            cloid_from_client_order_id("vike-000124"),
            "distinct coids -> distinct cloids"
        );
        // "0x" + 16 bytes as 32 lowercase hex chars = 34 chars total
        assert_eq!(a.len(), 34);
        assert!(a.starts_with("0x"));
        assert!(
            a[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex only, got {a}"
        );
    }
}
