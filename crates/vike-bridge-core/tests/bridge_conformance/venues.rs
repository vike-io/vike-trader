//! The ten covered venues' `ConformanceBridge` impls and the captured-template helpers they patch.

use serde_json::{Value, json};

use vike_model::events::{Event, OrderAccepted, OrderRejected};
use vike_model::{OrderRequest, OrderStatusReport, OrderType};

use super::{
    AccountLane, ClaimProof, ConformanceBridge, ExecKind, FencedPump, FillJoin, FillShape,
    SubmitFenceRow, TriggerListing, limit_order,
};

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
    fn submit_fence(&self) -> SubmitFenceRow {
        // Spot (WS-API `executionReport`) and perp (listenKey `ORDER_TRADE_UPDATE`) alike: the stream pushes
        // an order's `NEW` report BEFORE its first `TRADE`, on one ordered socket, and the mapper turns
        // `NEW` into a bare `OrderAccepted` that names the venue order id. That accept reaches the engine
        // ahead of any fill (`ManagedOrder::apply_over_lost_prefix` takes it on an `Initialized` order),
        // so a fill pushed before the REST answer lands on an accepted order. What stays open is only a
        // reconnect replay (it carries no accept) landing inside a slow submit.
        SubmitFenceRow::Unfenced(
            "the stream pushes `NEW` (a bare OrderAccepted naming the venue order id) before an order's first `TRADE` on one ordered socket, so the pump's own accept lands first",
        )
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
    fn trigger_listing(&self) -> TriggerListing {
        // A claimant (okx's and bybit's rows are the others): the PERP lane lists the Algo Service's stop
        // (`openAlgoOrders`, appended to its report), and the row below parses an algo row of every
        // kind caps let it place.
        TriggerListing::Claims(ClaimProof {
            client_claim: binance_perp_claims,
            open_rows: binance_algo_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // `userTrades` echoes no coid; the order id joins (the accept frame's `i`). The perp lane
        // spells the fill's symbol as the series label the engine's order carries.
        FillJoin::Row {
            symbol: "BTCUSDT.P",
            parse: |_coid, venue_order_id| {
                vike_binance::recon_client::parse_perp_user_trades(&family_fill_body(
                    venue_order_id,
                ))
                .expect("a well-formed userTrades body")
            },
        }
    }
}

/// Binance's PERP recon client, built offline with throwaway credentials, asked the kind claim.
fn binance_perp_claims() -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: None,
    };
    vike_binance::recon_client(&creds, "BTCUSDT.P", false)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// A venue-native `openAlgoOrders` row of `kind` carrying `coid`, through binance's own parser.
/// Only the stop is placed on the Algo Service (`submit_algo`), so only it has a fixture.
fn binance_algo_open_rows(kind: &OrderType, coid: &str) -> Option<Vec<OrderStatusReport>> {
    let algo_type = match kind {
        OrderType::Stop => "STOP_MARKET",
        _ => return None,
    };
    let body = json!([{
        "algoId": 7001, "clientAlgoId": coid, "algoType": "CONDITIONAL", "orderType": algo_type,
        "symbol": "BTCUSDT", "side": "SELL", "positionSide": "BOTH", "quantity": "1.000",
        "triggerPrice": "90.00", "price": "0.00", "algoStatus": "NEW", "reduceOnly": true,
        "updateTime": 1
    }])
    .to_string();
    Some(
        vike_binance::algo::parse_perp_open_algo_orders(&body, "binance")
            .expect("a well-formed openAlgoOrders body"),
    )
}

/// A `userTrades` body of one fill of the order `venue_order_id`: the binance family spells no coid
/// on a fill row (binance and aster share it).
fn family_fill_body(venue_order_id: &str) -> String {
    json!([{
        "symbol": "BTCUSDT", "id": 5001, "orderId": venue_order_id, "side": "BUY",
        "price": "100.0", "qty": "1.0", "commission": "0.05", "commissionAsset": "USDT",
        "time": 5, "maker": true
    }])
    .to_string()
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
    fn submit_fence(&self) -> SubmitFenceRow {
        // The `execution` topic (fills) and the `order` topic (`New`) are separate topics of one socket
        // with no promised order between them, and a market order may get no `New` row at all, so a
        // fill can reach the engine with no accept of its own ahead of it.
        SubmitFenceRow::Fenced {
            pump: Some(FencedPump {
                handshake_replies: &[
                    r#"{"op":"auth","success":true}"#,
                    r#"{"op":"subscribe","success":true}"#,
                ],
                spawn: |ws_url, events, fence| {
                    let feed = vike_bybit::user_data::spawn_bybit_perp_user_data_with_resync(
                        ws_url,
                        "key".into(),
                        "secret".into(),
                        "BTCUSDT".into(),
                        events,
                        fence,
                        Vec::new,
                        None,
                        false,
                    );
                    Box::new(move || {
                        let _ = feed.shutdown();
                    })
                },
            }),
        }
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
    fn trigger_listing(&self) -> TriggerListing {
        // The second claimant. `order/realtime` is asked with no `orderFilter`, which the demo host
        // answers with the resting conditional beside the active orders (measured 2026-10-24:
        // `orderFilter=Order` lists only the limits, `StopOrder` only the stop, no filter all of them);
        // the client pages and fails rather than truncating; the one trigger kind bybit's `submit`
        // places is the stop, and the row below parses a conditional of it into a live `Stop` that
        // carries the coid.
        TriggerListing::Claims(ClaimProof {
            client_claim: bybit_claims,
            open_rows: bybit_conditional_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // `execution/list` echoes `orderLinkId` on every row: the coid joins, the order id too.
        FillJoin::Row {
            symbol: "BTCUSDT",
            parse: |coid, venue_order_id| {
                let body = json!({"list": [{
                    "execId": "e1", "orderId": venue_order_id, "orderLinkId": coid,
                    "symbol": "BTCUSDT", "side": "Buy", "execQty": "1", "execPrice": "100",
                    "execFee": "0.01", "feeCurrency": "USDT", "isMaker": true, "execTime": "5"
                }]})
                .to_string();
                vike_bybit::recon_client::parse_fills(&body).expect("a well-formed execution list")
            },
        }
    }
}

/// Bybit's recon client, built offline with throwaway credentials, asked the kind claim.
fn bybit_claims() -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: None,
    };
    vike_bybit::recon_client(&creds, "BTCUSDT", false)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// A venue-native `order/realtime` row of the conditional `kind` carrying `coid`, through bybit's own
/// parser. Only the stop is placed (`build_order_params`: a reduce-only `Market` that waits for
/// `triggerPrice`), so only it has a fixture. The row is the shape recorded on the demo host
/// 2026-10-24 (`crates/bridges/bybit/tests/offline/recon_paging.rs`'s `RECORDED_STOP_ROW` is the whole
/// answer): an `Untriggered` `Market` with `stopOrderType` `Stop`, `triggerDirection` 2 (a fall) for a SELL.
fn bybit_conditional_open_rows(kind: &OrderType, coid: &str) -> Option<Vec<OrderStatusReport>> {
    let stop_order_type = match kind {
        OrderType::Stop => "Stop",
        _ => return None,
    };
    let body = json!({"list": [{
        "symbol": "BTCUSDT", "orderId": "7001", "orderLinkId": coid, "side": "Sell",
        "orderType": "Market", "stopOrderType": stop_order_type, "triggerDirection": 2,
        "triggerPrice": "90.0", "reduceOnly": true, "qty": "1", "cumExecQty": "0",
        "avgPrice": "", "orderStatus": "Untriggered", "updatedTime": "1"
    }]})
    .to_string();
    Some(vike_bybit::recon_client::parse_orders(&body).expect("a well-formed order/realtime body"))
}

// --- OKX (V5 SWAP-perp orders channel; ExecActor) --------------------------------------------------

pub(super) struct Okx;
impl ConformanceBridge for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        // The `orders` channel pushes a `live` row only for an order that rests; an order that fills at
        // once (a market order) can have a `filled` row as the first thing the stream carries for it.
        SubmitFenceRow::Fenced {
            pump: Some(FencedPump {
                handshake_replies: &[
                    r#"{"event":"login","code":"0"}"#,
                    r#"{"event":"subscribe","code":"0"}"#,
                ],
                spawn: |ws_url, events, fence| {
                    let feed = vike_okx::user_data::spawn_okx_perp_user_data_with_resync(
                        ws_url,
                        "key".into(),
                        "secret".into(),
                        "passphrase".into(),
                        "BTC-USDT-SWAP".into(),
                        |_| Some(1.0),
                        events,
                        fence,
                        Vec::new,
                        None,
                    );
                    Box::new(move || {
                        let _ = feed.shutdown();
                    })
                },
            }),
        }
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
    fn trigger_listing(&self) -> TriggerListing {
        // okx places its stop as an ALGO order (`order-algo`) and its recon client lists it on
        // `orders-algo-pending`, beside `orders-pending` (strict: a failed read fails the report). The row
        // below parses an `orders-algo-pending` row of every kind caps let it place; a fired stop's fill
        // is named by `orders-algo-history` (`crates/bridges/okx/tests/offline/claim_restart.rs`).
        TriggerListing::Claims(ClaimProof {
            client_claim: okx_claims,
            open_rows: okx_algo_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // The row echoes `clOrdId` and `ordId`; the order id is the join that does not depend on
        // how the venue spells a hex coid on its REST fill rows.
        FillJoin::Row {
            symbol: "BTC-USDT-SWAP",
            parse: |coid, venue_order_id| {
                let body = json!([{
                    "instId": "BTC-USDT-SWAP", "tradeId": "t1", "ordId": venue_order_id,
                    "clOrdId": coid, "side": "buy", "fillSz": "1", "fillPx": "100", "fee": "-0.01",
                    "feeCcy": "USDT", "execType": "M", "ts": "5"
                }])
                .to_string();
                vike_okx::recon_client::parse_fills(&body, 1.0).expect("a well-formed fills body")
            },
        }
    }
}

/// OKX's recon client, built offline with throwaway credentials, asked the kind claim.
fn okx_claims() -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: Some("conformance-passphrase".to_string().into()),
    };
    vike_okx::recon_client(&creds, "BTC-USDT-SWAP", 1.0, false)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// A venue-native `orders-algo-pending` row of `kind` carrying `coid`, through okx's own parser. Only
/// the stop is placed as an algo order (`submit_stop_algo`, `ordType=conditional`), so only it has a
/// fixture.
fn okx_algo_open_rows(kind: &OrderType, coid: &str) -> Option<Vec<OrderStatusReport>> {
    match kind {
        OrderType::Stop => {}
        _ => return None,
    }
    let body = json!([{
        "instId": "BTC-USDT-SWAP", "algoId": "7001", "algoClOrdId": coid,
        "ordType": "conditional", "side": "sell", "sz": "1", "state": "live",
        "slTriggerPx": "90", "reduceOnly": "true", "uTime": "1"
    }])
    .to_string();
    Some(
        vike_okx::recon_client::parse_algo_pending(&body, 1.0)
            .expect("a well-formed orders-algo-pending body"),
    )
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
    fn submit_fence(&self) -> SubmitFenceRow {
        // The stream is `user.trades` only: it carries no accept at all, so every fill is ahead of the
        // order's own answer. The real pump (a JSON-RPC handshake whose replies carry the request ids)
        // is not driven here; `order_event_order.rs` in the crate runs it, the replay and the re-dial.
        SubmitFenceRow::Fenced { pump: None }
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
    fn trigger_listing(&self) -> TriggerListing {
        // deribit places no stop kind (its capability row declares market and limit only).
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // `get_user_trades_by_instrument` rows carry our `label` (the coid) and the `order_id`.
        FillJoin::Row {
            symbol: "BTC-PERPETUAL",
            parse: |coid, venue_order_id| {
                let body = json!([{
                    "trade_id": "t1", "order_id": venue_order_id, "label": coid,
                    "instrument_name": "BTC-PERPETUAL", "direction": "buy", "amount": 10,
                    "price": 100, "fee": 0.0, "fee_currency": "BTC", "liquidity": "M",
                    "timestamp": 5
                }])
                .to_string();
                vike_deribit::recon_client::parse_user_trades(&body)
                    .expect("a well-formed user-trades body")
            },
        }
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
// ⚠ STARTED ON THE PERP, the way production starts it (`crates/vike-venues/src/venue_wiring.rs`'s
// `VENUE_WIRING` aster row is `BTCUSDT.P`). On the spot spelling the mapper's bare `o.s` label equals the
// order's symbol, so the row stays green while every live perp fill is dropped from the position.
// The frames stay bare (`s: "BTCUSDT"` IS the wire); the order and the mapper fallback carry the
// catalog spelling, exactly as `vike_aster::perp_user_data`'s pump passes it.
pub(super) struct Aster;
impl ConformanceBridge for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        // The binance family's mapper and listenKey pump (see `Binance::submit_fence`).
        SubmitFenceRow::Unfenced(
            "the binance family's stream: `NEW` (a bare OrderAccepted naming the venue order id) precedes an order's first `TRADE` on one ordered socket, so the pump's own accept lands first",
        )
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
    fn trigger_listing(&self) -> TriggerListing {
        // aster shares binance's family code but not its Algo Service read; whether aster lists its
        // stop on `openOrders` is unmeasured, so no claim.
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        FillJoin::Row {
            symbol: "BTCUSDT.P",
            parse: |_coid, venue_order_id| {
                vike_aster::recon_client::parse_perp_user_trades(&family_fill_body(venue_order_id))
                    .expect("a well-formed userTrades body")
            },
        }
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
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec client (`crates/bridges/hyperliquid/src/exec/client.rs`), not `run_loop`: the race is not examined by this row",
        )
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
            let orders: Vec<vike_hyperliquid::exec::event_mapper::SubmittedOrder> = ctx
                .iter()
                .map(|o| vike_hyperliquid::exec::event_mapper::SubmittedOrder {
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
            return vike_hyperliquid::exec::event_mapper::map_order_response(
                frame,
                "hyperliquid",
                &orders,
            );
        }
        vike_hyperliquid::exec::event_mapper::map_order_updates(frame, "hyperliquid")
    }
    fn trigger_listing(&self) -> TriggerListing {
        // hyperliquid places stop, stop-limit and take-profit, but whether `frontendOpenOrders`
        // lists an untriggered one and echoes its cloid has never been put on a wire: no claim.
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // `userFills` rows carry `oid` (the venue order id the `open` update acknowledged) and a
        // `cloid` that is a hash of our coid (the core translates it back); the oid joins.
        FillJoin::Row {
            symbol: "BTC",
            parse: |_coid, venue_order_id| {
                let body = json!([{
                    "tid": 1, "oid": venue_order_id, "coin": "BTC", "side": "B", "sz": "1.0",
                    "px": "100.0", "fee": "0.01", "feeToken": "USDC", "crossed": true, "time": 5
                }])
                .to_string();
                vike_hyperliquid::exec::recon_client::parse_fills(&body)
                    .expect("a well-formed userFills body")
            },
        }
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
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec, not `run_loop`: `SubmittedSymbols` (`crates/bridges/oanda/src/symbols.rs`) already holds the stream's fills behind the submit",
        )
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
            return vike_oanda::decode_transaction_events(frame, &Default::default());
        }
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        vike_oanda::map_order_response(coid, 1, frame, &Default::default())
    }
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // An `ORDER_FILL` transaction echoes `clientExtensions.id` (the coid) and `orderID`.
        FillJoin::Row {
            symbol: "EURUSD",
            parse: |coid, venue_order_id| {
                let body = json!({"transactions": [{
                    "type": "ORDER_FILL", "id": "900", "instrument": "EUR_USD",
                    "orderID": venue_order_id, "clientExtensions": {"id": coid},
                    "units": "1", "price": "1.1", "commission": "0", "time": "1"
                }]});
                vike_oanda::recon_client::parse_fill_reports(&body, "EUR_USD", "EURUSD", "USD")
            },
        }
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
// `crates/bridges/ig/src/exec/stream.rs` feed `decode_trade_confirm`.
pub(super) struct Ig;
impl ConformanceBridge for Ig {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec, not `run_loop`: the race is not examined by this row",
        )
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
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        FillJoin::Deferred(
            "ig's activity feed (`parse_activity_fills`) carries no coid, and its venue order id is \
             the activity's `dealReference` (a hash of the coid), not the deal id the confirm \
             acknowledged: the join is unproven until a live capture settles it",
        )
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
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec, not `run_loop`: the race is not examined by this row",
        )
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
        vike_alpaca::decode_trade_event(frame, &Default::default())
    }
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // A FILL activity carries no coid at all; its `order_id` is the order id the `new` event
        // acknowledged (`order.id`), so the venue id joins.
        FillJoin::Row {
            symbol: "AAPL",
            parse: |_coid, venue_order_id| {
                let body = json!([{
                    "activity_type": "FILL", "id": "a1", "order_id": venue_order_id,
                    "symbol": "AAPL", "side": "buy", "qty": "1", "price": "100.0",
                    "transaction_time": "2026-10-20T00:00:00Z"
                }])
                .to_string();
                vike_alpaca::recon_client::parse_fills(&body, "AAPL", "AAPL")
                    .expect("a well-formed activities body")
            },
        }
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
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec, not `run_loop`: the race is not examined by this row",
        )
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        // NO `price`, unlike every other venue's `limit_order`: a priced limit is placed through the
        // ABI 2 entry point (`fc_place_entry_at`), which this harness cannot drive, and an unpriced
        // one is the shape every shim — old or new — accepts, so it is the lifecycle this row folds.
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
            order_type: vike_model::OrderType::Limit,
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
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        FillJoin::Deferred(
            "fxcm reads one row per OPEN trade (`parse_fills`), not a fill history, so a trade \
             closed while the node was down has no row to join",
        )
    }
}
