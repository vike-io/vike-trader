//! The ten covered venues' `ConformanceBridge` impls and the captured-template helpers they patch.

use serde_json::{Value, json};

use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected};

use super::{AccountLane, ConformanceBridge, ExecKind, FillShape, limit_order};

/// Load the FIRST committed captured frame for `(venue, kind)` — `None` when the venue has no
/// committed capture (the hand-authored fallback then applies). Reads the capture fixture format
/// (`{_provenance, frames:[..]}`) with plain fs+serde_json so the default (feature-less) test
/// build sources real frames too; a malformed committed file panics rather than silently falling
/// back. The kind vocabulary is the capture smokes' (`ws_accepted`/`ws_fill`/`ws_canceled`).
pub(super) fn captured_template(venue: &str, kind: &str) -> Option<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bridges")
        .join(venue)
        .join("tests/fixtures/captured")
        .join(format!("{kind}.json"));
    let body = std::fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()));
    root.get("frames").and_then(|f| f.as_array()).and_then(|a| a.first()).cloned()
}

/// Walk `ptr` (JSON-pointer style, object keys + array indices) to the leaf's PARENT object,
/// returning it with the leaf key — the insert-capable twin of `Value::pointer_mut` (which cannot
/// create an absent leaf).
fn leaf_parent<'a>(
    frame: &'a mut Value,
    ptr: &str,
) -> Option<(&'a mut serde_json::Map<String, Value>, String)> {
    let mut parts: Vec<&str> = ptr.trim_start_matches('/').split('/').collect();
    let leaf = parts.pop()?.to_string();
    let mut cur = frame;
    for p in parts {
        cur = match p.parse::<usize>() {
            Ok(i) => cur.get_mut(i)?,
            Err(_) => cur.get_mut(p)?,
        };
    }
    cur.as_object_mut().map(|o| (o, leaf))
}

/// Patch a numeric scenario value into a captured template, PRESERVING the captured leaf's wire
/// TYPE: a string-typed leaf (the common venue encoding for qty/px) receives the formatted string,
/// a number-typed (or absent) leaf the JSON number — so the template keeps proving the mapper's
/// string-coercion against the real wire shape.
fn patch_f64(frame: &mut Value, ptr: &str, val: f64) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    let patched = match parent.get(&leaf) {
        Some(Value::String(_)) => Value::String(format!("{val}")),
        _ => json!(val),
    };
    parent.insert(leaf, patched);
}

/// Patch a string scenario value (coid / trade id / status marker) into a captured template.
/// Always writes a string: the scenario's ids are non-numeric, and every mapper reads ids through
/// the shared string-coercing accessors, so a numeric-on-the-wire id leaf (binance's `t`) taking a
/// string here is the one deliberate type departure (documented, assertion-neutral).
fn patch_str(frame: &mut Value, ptr: &str, val: &str) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    parent.insert(leaf, Value::String(val.to_string()));
}

// --- Binance (spot executionReport; LiveRestClient) -------------------------------------------------

pub(super) struct Binance;

impl Binance {
    /// Captured WS-API user-data frames arrive ENVELOPED (`{"subscriptionId":..,"event":{..}}`)
    /// while the hand-authored fallbacks are bare executionReports — this is the pointer PREFIX to
    /// wherever the report actually lives in this frame (computed before any `&mut` patch borrow).
    fn prefix(frame: &Value) -> &'static str {
        if frame.get("event").is_some() { "/event" } else { "" }
    }
}

impl ConformanceBridge for Binance {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::RestPoll
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "binance", "BTCUSDT")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // The REAL captured x=NEW frame as the structure template, scenario coid/venue-id patched in.
        if let Some(mut f) = captured_template("binance", "ws_accepted") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            patch_str(&mut f, &format!("{p}/i"), venue_order_id);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1,
               "i": venue_order_id, "x": "NEW"})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Binance drives terminality off `X` (the order status AFTER this fill), not a cumulative.
        // Captured template: a REAL x=TRADE/X=FILLED frame; the scenario patches the routed coid,
        // dedup trade id, incremental qty/px (type-preserving — the real wire's STRING qtys stay
        // strings), and the X terminal marker (a PARTIAL is the captured full-fill template with
        // X flipped — market demo orders fill whole, so no real partial frame exists to capture).
        if let Some(mut f) = captured_template("binance", "ws_fill") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            patch_str(&mut f, &format!("{p}/t"), trade_id);
            patch_f64(&mut f, &format!("{p}/l"), this_qty);
            patch_f64(&mut f, &format!("{p}/L"), px);
            patch_str(
                &mut f,
                &format!("{p}/X"),
                if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            );
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1, "x": "TRADE",
               "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
               "t": trade_id, "S": "BUY", "l": this_qty, "L": px, "n": 0.0, "N": "USDT", "m": false})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        if let Some(mut f) = captured_template("binance", "ws_canceled") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1, "x": "CANCELED",
               "r": "NONE"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_binance::event_mapper::map_binance_private(frame, "binance", "BTCUSDT")
    }
}

// --- Bybit (V5 linear-perp execution/order topics; ExecActor) --------------------------------------

pub(super) struct Bybit;

impl Bybit {
    /// A captured V5 private frame batches rows in `data[]`; the scenarios drive ONE logical
    /// event per frame, so a multi-row capture keeps row 0 only (row structure stays verbatim —
    /// stray sibling rows would otherwise fold foreign-coid events into the ContractOrder).
    fn one_row(frame: &mut Value) {
        if let Some(rows) = frame.get_mut("data").and_then(|d| d.as_array_mut()) {
            rows.truncate(1);
        }
    }
}

impl ConformanceBridge for Bybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "bybit", "BTCUSDT")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // The REAL captured order-topic New frame as the structure template.
        if let Some(mut f) = captured_template("bybit", "ws_accepted") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            patch_str(&mut f, "/data/0/orderId", venue_order_id);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": coid, "orderStatus": "New", "orderId": venue_order_id,
             "updatedTime": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        cum_qty: f64,
        total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // Bybit's terminality is cumExecQty >= orderQty (fallback leavesQty == 0). Captured
        // template: a REAL execution-topic Trade row; the scenario patches coid, the execId dedup
        // key, and the qty grid (type-preserving — the real wire's STRING numerics stay strings),
        // so a PARTIAL is the same real row with cum < orderQty.
        if let Some(mut f) = captured_template("bybit", "ws_fill") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            patch_str(&mut f, "/data/0/execId", trade_id);
            patch_f64(&mut f, "/data/0/execQty", this_qty);
            patch_f64(&mut f, "/data/0/execPrice", px);
            patch_f64(&mut f, "/data/0/cumExecQty", cum_qty);
            patch_f64(&mut f, "/data/0/orderQty", total_qty);
            patch_f64(&mut f, "/data/0/leavesQty", total_qty - cum_qty);
            return f;
        }
        json!({"topic": "execution", "data": [
            {"execType": "Trade", "orderLinkId": coid, "execId": trade_id, "execTime": 1,
             "side": "Buy", "execQty": this_qty, "execPrice": px, "execFee": 0.0,
             "feeCurrency": "USDT", "isMaker": false,
             "cumExecQty": cum_qty, "orderQty": total_qty, "leavesQty": total_qty - cum_qty}]})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        if let Some(mut f) = captured_template("bybit", "ws_canceled") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": coid, "orderStatus": "Cancelled", "cancelType": "CancelByUser",
             "updatedTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_bybit::event_mapper::map_bybit_perp(frame, "bybit", "BTCUSDT")
    }
}

// --- OKX (V5 SWAP-perp orders channel; ExecActor) --------------------------------------------------

pub(super) struct Okx;
impl ConformanceBridge for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "okx", "BTC-USDT-SWAP")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": "live", "ordId": venue_order_id, "uTime": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        cum_qty: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // OKX's terminality is state=="filled" (fallback accFillSz >= sz). ct_val is 1.0 here so
        // base qty passes through unchanged.
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": if terminal { "filled" } else { "partially_filled" },
             "tradeId": trade_id, "fillSz": this_qty, "fillPx": px, "fillFee": "-0.1",
             "fillFeeCcy": "USDT", "side": "buy", "execType": "T",
             "accFillSz": cum_qty, "sz": total_qty, "fillTime": 1}]})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": "canceled", "cancelSource": "user", "uTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_okx::event_mapper::map_okx_perp(frame, "okx", "BTC-USDT-SWAP", 1.0)
    }
}

// --- Deribit (user.trades fills + JSON-RPC accept + order-history cancel; LiveRestClient) ----------
//
// Deribit is the odd crypto venue: its private-WS `event_mapper` (`map_deribit_private`) is
// FILLS-ONLY — lifecycle accept is the SYNCHRONOUS `private/buy` JSON-RPC reply, and a cancel that
// never traded surfaces via the order-history replay (`map_deribit_history`). So `decode`
// dispatches each venue-native frame to the REAL Deribit mapper that owns that lifecycle edge, and
// the fills stay incremental (`amount` per row, `state=="filled"` the SOLE terminal signal).
pub(super) struct Deribit;
impl ConformanceBridge for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::RestPoll
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "deribit", "BTC-PERPETUAL")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A `private/buy` JSON-RPC reply — the venue half of the emitter split. `label` is our coid
        // (Deribit echoes it on every order/trade row), `order.order_id` the venue id.
        json!({"id": 1, "result": {"order": {
            "order_id": venue_order_id, "label": coid, "order_state": "open"}}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // A `user.trades.<instrument>.raw` subscription row: `amount` is the INCREMENTAL fill qty,
        // `state=="filled"` is Deribit's SOLE terminal signal (there are no cum/leaves fields).
        json!({"method": "subscription", "params": {
            "channel": "user.trades.BTC-PERPETUAL.raw", "data": [
            {"trade_id": trade_id, "label": coid, "instrument_name": "BTC-PERPETUAL",
             "direction": "buy", "amount": this_qty, "price": px, "fee": 0.0,
             "fee_currency": "USDT", "liquidity": "T", "timestamp": 1,
             "state": if terminal { "filled" } else { "open" }}]}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        // A bare `private/get_order_history_by_instrument` array — `order_state=="cancelled"` is the
        // non-fill terminal the history replay recovers (the fills stream carries no lifecycle cancel).
        json!([{"label": coid, "order_id": "o-1", "order_state": "cancelled",
                "last_update_timestamp": 1}])
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // Fills → the fixture-tested `user.trades` mapper.
        if frame.get("method").and_then(|m| m.as_str()) == Some("subscription") {
            return vike_deribit::event_mapper::map_deribit_private(
                frame,
                "deribit",
                "BTC-PERPETUAL",
            );
        }
        // Order-history array → the history replay mapper (cancel/reject terminals).
        if frame.is_array() {
            return vike_deribit::history::map_deribit_history(
                frame,
                &Value::Array(Vec::new()),
                "deribit",
                "BTC-PERPETUAL",
            );
        }
        // JSON-RPC order reply → the accept. Deribit has NO pure accept mapper (the assembly lives in
        // `client::dispatch_submit`, which needs a live socket); mirror its exact three-field
        // construction here over the REAL `rpc::parse_response` envelope decode. An error envelope
        // resolves to a terminal reject, exactly as `dispatch_submit` does (no silent vanish).
        let (_id, result, error) = vike_deribit::rpc::parse_response(frame);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            let reason = err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            return vec![Event::OrderRejected(OrderRejected {
                client_order_id: order_field(&result, "label"),
                reason: reason.into(),
                ts: 1,
            })];
        }
        let order_id = order_field(&result, "order_id");
        vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: order_field(&result, "label"),
            venue_order_id: Some(order_id.into()),
            ts: 1,
        })]
    }
}

/// Read `result.order.<key>` as an owned string (numbers stringified) — the tiny helper Deribit's
/// `decode` uses to lift the coid/venue-id off a JSON-RPC order reply, mirroring `dispatch_submit`.
fn order_field(result: &Option<Value>, key: &str) -> String {
    match result.as_ref().and_then(|r| r.get("order")).and_then(|o| o.get(key)) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// --- Aster (Binance-wire USDⓈ-M perp ORDER_TRADE_UPDATE; ExecActor) --------------------------------
//
// Aster's perp user-data stream is Binance-verbatim, so its `perp_mapper` re-exports the shared
// Binance-grammar `map_perp` — ONE mapper carries accept (`x=="NEW"`), incremental fills
// (`x=="TRADE"`, terminal on `X=="FILLED"`), and cancel (`x=="CANCELED"`), exactly like bybit/okx.
//
// ⚠ MOUNTED ON THE PERP, the way production mounts it (`crates/vike-tradehub/src/wired_markets.rs`'s
// `ASTER_MARKET` is `BTCUSDT.P`). On the spot spelling the mapper's bare `o.s` label equals the
// order's symbol, so the row stays green while every live perp fill is dropped from the position.
// The frames stay bare (`s: "BTCUSDT"` IS the wire); the order and the mapper fallback carry the
// catalog spelling, exactly as `vike_aster::perp_user_data`'s pump passes it.
pub(super) struct Aster;
impl ConformanceBridge for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "aster", "BTCUSDT.P")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "NEW", "i": venue_order_id}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Aster (Binance perp) terminality is `X=="FILLED"`; `l` is the INCREMENTAL last-fill qty.
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "TRADE",
            "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            "t": trade_id, "S": "BUY", "l": this_qty, "L": px, "n": 0.0, "N": "USDT",
            "m": false, "ps": "BOTH"}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "CANCELED"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_aster::perp_mapper::map_aster_perp(frame, "aster", "BTCUSDT.P")
    }
}

// --- Hyperliquid (orderUpdates lifecycle + /exchange partials; bespoke-signer ExecActor) -----------
//
// HL splits the FSM lane across two surfaces: WS `orderUpdates` carries accept (`open`), the
// terminal fill (`filled`, qty = `origSz`), and cancel (`canceled`) via the REAL `map_order_updates`;
// but `orderUpdates` has NO partial status, so a resting PARTIAL is only ever an
// `OrderPartiallyFilled` through the `/exchange` submit response (`filled.totalSz` < requested) via
// `map_order_response`. That response is POSITIONAL (the exec side pairs each `statuses[i]` with the
// order it sent), so `frame_fill(partial)` carries the same per-slot context in a non-wire `_ctx`
// sidecar that `decode` rebuilds into a `SubmittedOrder` before folding through the real mapper.
pub(super) struct Hyperliquid;
impl ConformanceBridge for Hyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn account_lane(&self) -> AccountLane {
        // Both surfaces this row decodes carry FSM wraps only: `map_order_updates`'s `filled` wrap
        // and `map_order_response`'s `/exchange` partial (no bare fill — the fee is not in that
        // response). The position moves on `userFills`, a third lane this table does not drive.
        AccountLane::Separate("userFills (orderUpdates and /exchange carry the FSM wraps only)")
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "hyperliquid", "BTC")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // `orderUpdates status=="open"` rests the order. The `cloid` carries our coid directly (the
        // caller's cloid→coid remap is shortcut here, as the other venues' frames carry the coid).
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "1.0",
                       "oid": venue_order_id, "cloid": coid, "timestamp": 1},
             "status": "open", "statusTimestamp": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        if terminal {
            // A completing fill rests as `orderUpdates status=="filled"` → terminal OrderFilled;
            // `origSz` is the wrap's qty, `oid` its FSM-dedup trade id.
            json!({"channel": "orderUpdates", "data": [
                {"order": {"coin": "BTC", "side": "B", "limitPx": px, "sz": "0.0",
                           "origSz": this_qty, "oid": trade_id, "cloid": coid, "timestamp": 1},
                 "status": "filled", "statusTimestamp": 1}]})
        } else {
            // A resting partial: an `/exchange` response whose `filled.totalSz` (< requested `req_sz`)
            // → OrderPartiallyFilled. `_ctx` carries the positional SubmittedOrder context.
            json!({"status": "ok",
                   "response": {"type": "order", "data": {"statuses": [
                       {"filled": {"totalSz": this_qty, "avgPx": px, "oid": trade_id}}]}},
                   "_ctx": [{"coid": coid, "coin": "BTC", "side": 1, "req_sz": total_qty, "ts": 1}]})
        }
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "oid": "o-1", "cloid": coid, "timestamp": 1},
             "status": "canceled", "statusTimestamp": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // An `/exchange` response (our `_ctx` sidecar present) → rebuild the positional
        // SubmittedOrder context and fold through the REAL response mapper; otherwise it is an
        // `orderUpdates` frame.
        if let Some(ctx) = frame.get("_ctx").and_then(|c| c.as_array()) {
            let orders: Vec<vike_hyperliquid::event_mapper::SubmittedOrder> = ctx
                .iter()
                .map(|o| vike_hyperliquid::event_mapper::SubmittedOrder {
                    client_order_id: o
                        .get("coid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    coin: o.get("coin").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    side: o.get("side").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
                    req_sz: o.get("req_sz").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    ts: o.get("ts").and_then(|v| v.as_i64()).unwrap_or(0),
                })
                .collect();
            return vike_hyperliquid::event_mapper::map_order_response(
                frame,
                "hyperliquid",
                &orders,
            );
        }
        vike_hyperliquid::event_mapper::map_order_updates(frame, "hyperliquid")
    }
}

// --- OANDA (v20 FX; ExecActor + transactions stream; whole-fill) -----------------------------------
//
// OANDA is the FX-streaming shape. It fills WHOLE ([`FillShape::Whole`]): the
// order-POST reply carries the accept (`orderCreateTransaction`) and, for a MARKET order, an inline
// fill; delayed LIMIT/STOP fills + cancels arrive one-per-line on the transactions stream. So
// `decode` dispatches: a stream transaction (has `"type"`) → the REAL `decode_transaction_events`
// (`ORDER_FILL` → `[Fill, OrderFilled]`, `ORDER_CANCEL` → `[OrderCanceled]`); anything else is a POST
// reply → `map_order_response`, which takes the coid as a PARAM (the reply body doesn't echo it into
// the fields the mapper reads), so the accept frame carries it in a non-wire `_coid` sidecar — the
// same shortcut hyperliquid uses for its positional `_ctx`.
pub(super) struct Oanda;
impl ConformanceBridge for Oanda {
    fn venue(&self) -> &'static str {
        "oanda"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "oanda", "EUR_USD")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A resting-LIMIT order-POST reply: `orderCreateTransaction` alone (no fill) → OrderAccepted.
        json!({"_coid": coid,
               "orderCreateTransaction": {"id": venue_order_id, "type": "LIMIT_ORDER"},
               "lastTransactionID": venue_order_id})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A transactions-stream ORDER_FILL — OANDA reports the FULL units at once (whole fill).
        // `units`/`price` are STRING-typed on the wire (the mapper `parse()`s them).
        json!({"type": "ORDER_FILL", "id": trade_id, "time": "1", "orderID": "v-1",
               "instrument": "EUR_USD", "units": this_qty.to_string(), "price": px.to_string(),
               "commission": "0", "clientExtensions": {"id": coid}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "v-1",
               "reason": "CLIENT_REQUEST", "clientExtensions": {"id": coid}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        if frame.get("type").is_some() {
            return vike_oanda::decode_transaction_events(frame);
        }
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        vike_oanda::map_order_response(coid, 1, frame)
    }
}

// --- IG (FX/CFD; ExecActor + Lightstreamer; whole-fill) --------------------------------------------
//
// IG splits its lifecycle across TWO real mappers, and fills WHOLE ([`FillShape::Whole`]): the
// synchronous `/confirms/{ref}` reply carries the accept for a resting working order (`map_confirm`
// with `market=false` → OrderAccepted only — its streaming twin `decode_trade_confirm` deliberately
// NEVER re-emits an accept), while the delayed working-order fill/cancel arrives on the Lightstreamer
// trade-update stream (`decode_trade_confirm`: an OPEN execution with size>0 → `[Fill, OrderFilled]`,
// a DELETED status → `[OrderCanceled]`, a REJECTED dealStatus → `[OrderRejected]`). Both mappers take
// the coid as a PARAM, carried in the `_coid` sidecar. ⚠ This harness's `decode` routes on `status`
// PRESENCE, a convention of ITS OWN synthetic frames, NOT a wire fact: the live sync `/confirms`
// reply carries `status` too (`crates/bridges/ig/tests/ig_close_position_smoke.rs` asserts
// `Some("CLOSED")` on the SYNC close confirm). The real bridge routes by SOURCE LANE: the confirm
// fetch in `crates/bridges/ig/src/exec.rs` feeds `map_confirm`, the Lightstreamer frames in
// `crates/bridges/ig/src/stream.rs` feed `decode_trade_confirm`.
pub(super) struct Ig;
impl ConformanceBridge for Ig {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "ig", "CS.D.EURUSD.MINI.IP")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A working-order `/confirms` reply: dealStatus ACCEPTED, no `status`/`level` (still resting).
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "dealId": venue_order_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY", "size": 1.0})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A streamed CONFIRMS execution: an OPEN working order with a level+size → whole fill.
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "status": "OPEN", "dealId": trade_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY",
               "size": this_qty, "level": px})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        // A streamed CONFIRMS with `status=="DELETED"` — a working order removed without executing.
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "status": "DELETED", "size": 1.0,
               "reason": "CANCELLED"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        // Routes on `status` presence: a convention of this harness's frames (see the IG note).
        if frame.get("status").is_some() {
            return vike_ig::decode_trade_confirm(frame, coid, 1);
        }
        vike_ig::map_confirm(coid, 1, false, frame)
    }
}

// --- Alpaca (US equities/crypto; ExecActor + SSE; whole-fill) --------------------------------------
//
// Alpaca is the SSE shape and the cleanest of the three: ONE real mapper
// (`decode_trade_event`) decodes every `/v2/events/trades` object — `event=="new"` → OrderAccepted,
// `event=="fill"` → `[Fill, OrderFilled]`, `event=="canceled"` → `[OrderCanceled]` — and the coid
// rides in `order.client_order_id` on the wire (no sidecar). It fills WHOLE ([`FillShape::Whole`]):
// the mapper has no partial state — a `"partial_fill"` event folds into `OrderFilled` too, so the
// cumulative-partial Lifecycle would wrongly reach a terminal on the first (0.4) fill.
pub(super) struct Alpaca;
impl ConformanceBridge for Alpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "alpaca", "AAPL")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"event": "new", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": venue_order_id, "client_order_id": coid, "symbol": "AAPL",
                         "side": "buy"}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // event="fill" → dual-publish. `qty`/`price` are STRING-typed on the wire (mapper parses).
        json!({"event": "fill", "timestamp": "2026-07-14T05:39:31.4Z", "execution_id": trade_id,
               "qty": this_qty.to_string(), "price": px.to_string(),
               "order": {"id": "v-1", "client_order_id": coid, "symbol": "AAPL", "side": "buy"}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"event": "canceled", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": "v-1", "client_order_id": coid, "symbol": "AAPL", "side": "buy"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_alpaca::decode_trade_event(frame)
    }
}

// --- FXCM (ForexConnect C++ SDK; ExecActor + a polled shim event queue; whole-fill) ---------------
//
// The venue with no wire at all. FXCM speaks a native C++ SDK, so its "frames" are JSON envelopes
// `crates/bridges/fxcm/src/shim/fcshim.cpp` builds with `snprintf` and enqueues for the exec
// thread to drain — and its ACCEPT is not a frame in any sense: it is the synchronous return of a
// placement FFI call. Both halves still reduce to the harness's stateless seam:
//
// * fill / cancel → the REAL `map_fxcm_event`, sourced from `crates/bridges/fxcm/tests/fixtures/
//   shim_events.json`, which is the shim's own emitter grammar transcribed and kept in step with
//   the C++ by `crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs`. So these scenarios run
//   against the shape the venue really produces, and a shim rename reddens there rather than here.
// * accept / reject → the REAL `map_placement`, lifted out of the exec loop for exactly this. The
//   `_placement` frame below is a HARNESS CARRIER, not a wire shape (there is no wire): it holds
//   the two values the FFI call returns, and `decode` folds them through the venue's own function.
//   Same shortcut oanda/ig use for their `_coid` param and hyperliquid for its positional `_ctx`.
//
// [`FillShape::Whole`], on the same evidence oanda/ig/alpaca carry: `map_fxcm_event` has NO
// `OrderPartiallyFilled` path at all — every `kind:"fill"` envelope maps to a terminal `OrderFilled`
// (FXCM's Trades table reports each execution as a whole trade row, not a cumulative on an order).
// [`ExecKind::CommandActor`]: `FxcmExecutionClient` is a newtype over the shared `ExecActor`.
//
// ⚠ What this does NOT cover, because a green row here is easy to over-read: the C++ shim itself,
// the FFI, and every ForexConnect behaviour behind it — no CI machine links that SDK. This row
// covers the venue's PURE layer only.
pub(super) struct Fxcm;

impl Fxcm {
    /// One committed shim envelope, by `kind`, as a scenario template.
    ///
    /// Panics rather than falling back to a hand-authored `json!`: the fixture IS this venue's
    /// grammar, and a silent fallback would let the harness keep passing against a shape the shim
    /// no longer emits — the exact failure mode the fixture exists to prevent.
    fn envelope(kind: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bridges/fxcm/tests/fixtures/shim_events.json");
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing fxcm shim fixture {}: {e}", path.display()));
        let root: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("malformed fxcm shim fixture {}: {e}", path.display()));
        root.get("envelopes")
            .and_then(|e| e.get(kind))
            .cloned()
            .unwrap_or_else(|| panic!("fxcm shim fixture has no `{kind}` envelope"))
    }
}

impl ConformanceBridge for Fxcm {
    fn venue(&self) -> &'static str {
        "fxcm"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        // ⚠ NO `price`, unlike every other venue's `limit_order`: FXCM's exec loop REFUSES a limit
        // request carrying one (`preflight_request`) because the shim rests at a fixed pip distance
        // from the live quote instead, so a priced limit would drive a lifecycle that cannot happen.
        //
        // ⚠ `qty` is BASE UNITS, never a lot count — the fill patched from it (`/amount`) is base
        // units too. `vike_fxcm::event_mapper::lots_for` divides by the live base unit size and
        // refuses an inexact size, so one lot of the ordinary EUR/USD 1000 is what a request is.
        OrderRequest {
            client_order_id: coid.into(),
            venue: "fxcm".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: "limit".into(),
            price: None,
            ts: 1,
            ..Default::default()
        }
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // The harness carrier for a SYNCHRONOUS placement return (see the module note above).
        json!({"_kind": "_placement", "_coid": coid, "order_id": venue_order_id})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // The shim's REAL fill envelope, scenario leaves patched in. `amount` is the executed size
        // in BASE UNITS and `trade_id` the reconnect-dedup key; `_terminal` is unread because this
        // venue has no partial state to select — see [`FillShape::Whole`].
        let mut f = Self::envelope("fill");
        patch_str(&mut f, "/_coid", coid);
        patch_str(&mut f, "/trade_id", trade_id);
        patch_f64(&mut f, "/amount", this_qty);
        patch_f64(&mut f, "/rate", px);
        f
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        let mut f = Self::envelope("canceled");
        patch_str(&mut f, "/_coid", coid);
        f
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        // A placement carrier → the REAL placement mapper (the venue half of the emitter split).
        if frame.get("_kind").and_then(|k| k.as_str()) == Some("_placement") {
            let oid = frame.get("order_id").and_then(|o| o.as_str()).unwrap_or_default();
            return vike_fxcm::event_mapper::map_placement(coid, 1, Ok(oid));
        }
        // …otherwise a drained shim envelope → the REAL stateless decode.
        vike_fxcm::event_mapper::map_fxcm_event(frame, coid)
    }
}
