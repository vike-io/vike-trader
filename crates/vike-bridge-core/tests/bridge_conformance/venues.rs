//! The ten covered venues' `ConformanceBridge` impls and the captured-template helpers they patch.

use serde_json::{Value, json};

use vike_model::events::{Event, OrderAccepted, OrderRejected};
use vike_model::{OrderRequest, OrderStatusReport, OrderType};

use super::{
    AccountLane, ClaimProof, ConformanceBridge, ExecutionKind, FencedPump, FillFields, FillFrame,
    FillJoin, FillShape, RefusedBeforeTaking, SubmitFenceRow, TriggerListing, VenueRejectRow,
    limit_order, whole_fill,
};

/// The `Unfenced` reason of a bridge whose exec is its own, not `run_loop` (ig, alpaca, fxcm).
const OWN_EXECUTION_UNEXAMINED: &str =
    "its own exec, not `run_loop`: the race is not examined by this row";

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
    let mut current = frame;
    for p in parts {
        current = match p.parse::<usize>() {
            Ok(i) => current.get_mut(i)?,
            Err(_) => current.get_mut(p)?,
        };
    }
    current.as_object_mut().map(|o| (o, leaf))
}

/// Patch a numeric scenario value into a captured template, PRESERVING the captured leaf's wire
/// TYPE: a string-typed leaf (the common venue encoding for qty/px) receives the formatted string,
/// a number-typed (or absent) leaf the JSON number — so the template keeps proving the mapper's
/// string-coercion against the real wire shape.
pub(super) fn patch_f64(frame: &mut Value, ptr: &str, val: f64) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    let patched = match parent.get(&leaf) {
        Some(Value::String(_)) => Value::String(format!("{val}")),
        _ => json!(val),
    };
    parent.insert(leaf, patched);
}

/// Patch a string scenario value (order client id / trade id / status marker) into a captured template.
/// Always writes a string: the scenario's ids are non-numeric, and every mapper reads ids through
/// the shared string-coercing accessors, so a numeric-on-the-wire id leaf (binance's `t`) taking a
/// string here is the one deliberate type departure (documented, assertion-neutral).
fn patch_str(frame: &mut Value, ptr: &str, val: &str) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    parent.insert(leaf, Value::String(val.to_string()));
}

// --- Binance (spot executionReport; ExecutionClientRest) -------------------------------------------------

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
        // Spot (WS-API `executionReport`) and perp (listenKey `ORDER_TRADE_UPDATE`) alike: the stream's `NEW`
        // usually lands first, but one outage can take the POST's answer, the re-query, `NEW` and the sentinel's
        // read, and then the replay's fill needs the accept the fence implies. The pumps need a listenKey REST call
        // or an id-matched ack this harness's fake cannot answer, so their own tests run them:
        // `crates/bridges/binance/src/family/execution/user_data.rs`'s `the_family_pump_and_its_replay_go_through_the_fence`
        // and `crates/bridges/binance/src/execution/user_data/spot.rs`'s `the_spot_pump_and_its_replay_go_through_the_fence`.
        SubmitFenceRow::Fenced { pump: None }
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::RestPoll
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "binance", "BTCUSDT")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // The REAL captured x=NEW frame as the structure template, scenario order_client_id/venue-id patched in.
        if let Some(mut f) = captured_template("binance", "ws_accepted") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), order_client_id);
            patch_str(&mut f, &format!("{p}/i"), venue_order_id);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": order_client_id, "T": 1,
               "i": venue_order_id, "x": "NEW"})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Binance drives terminality off `X` (the order status AFTER this fill), not a cumulative.
        // Captured template: a REAL x=TRADE/X=FILLED frame; the scenario patches the routed order client id,
        // dedup trade id, incremental qty/px (type-preserving — the real wire's STRING quantities stay
        // strings), and the X terminal marker (a PARTIAL is the captured full-fill template with
        // X flipped — market demo orders fill whole, so no real partial frame exists to capture).
        if let Some(mut f) = captured_template("binance", "ws_fill") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), order_client_id);
            patch_str(&mut f, &format!("{p}/t"), trade_id);
            patch_f64(&mut f, &format!("{p}/l"), this_quantity);
            patch_f64(&mut f, &format!("{p}/L"), px);
            patch_str(
                &mut f,
                &format!("{p}/X"),
                if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            );
            // The captured BUY paid its fee in the BASE coin (`n` 0.0000001 `N` BTC), which a spot BUY
            // takes out of the quantity it delivers (`vike_binance::family::execution::spot_fee`; its own tests
            // pin that). These scenarios judge the order's lifecycle against whole quantities, so the
            // fee is stated in the quote at zero, as the hand-authored fallback below states it.
            patch_f64(&mut f, &format!("{p}/n"), 0.0);
            patch_str(&mut f, &format!("{p}/N"), "USDT");
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": order_client_id, "T": 1, "x": "TRADE",
               "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
               "t": trade_id, "S": "BUY", "l": this_quantity, "L": px, "n": 0.0, "N": "USDT", "m": false})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        if let Some(mut f) = captured_template("binance", "ws_canceled") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), order_client_id);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": order_client_id, "T": 1, "x": "CANCELED",
               "r": "NONE"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_binance::execution::event_mapper::spot::map_binance_private(
            frame, "binance", "BTCUSDT", None,
        )
    }
    fn trigger_listing(&self) -> TriggerListing {
        // A claimant (okx's and bybit's rows are the others), on BOTH lanes: the perp lists the Algo
        // Service's stop and stop-limit (`openAlgoOrders`, appended to its report), the spot lists its
        // `STOP_LOSS` and `STOP_LOSS_LIMIT` among its open orders, and the rows below parse a row of every
        // kind caps let it place on each lane.
        TriggerListing::Claims(ClaimProof {
            client_claim: binance_claims,
            open_rows: binance_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // `userTrades` echoes no order client id; the order id joins (the accept frame's `i`). The perp lane
        // spells the fill's symbol as the series label the engine's order carries.
        FillJoin::Row {
            symbol: "BTCUSDT.P",
            body: |_order_client_id, venue_order_id| family_fill_body(venue_order_id),
            parse: |body| {
                vike_binance::reconcile::parse_perp_user_trades(&body.to_string())
                    .expect("a well-formed userTrades body")
            },
            fields: FillFields::at("/0", &["price"], &["qty"], &["id"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        let frame = whole_fill(self);
        let fields = FillFields::at(Self::prefix(&frame), &["L"], &["l"], &["t"]);
        FillFrame { frame, fields }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The captured `NEW` report's envelope as the venue's refusal: `x`/`X` REJECTED and `r` the reason word of the
        // venue's list. An empty `C` names `c` as the refused order (`resting_order_client_id`).
        let mut frame = self.frame_accepted(order_client_id, "v-reject-1");
        let p = Self::prefix(&frame);
        patch_str(&mut frame, &format!("{p}/x"), "REJECTED");
        patch_str(&mut frame, &format!("{p}/X"), "REJECTED");
        patch_str(&mut frame, &format!("{p}/C"), "");
        patch_str(&mut frame, &format!("{p}/r"), "INSUFFICIENT_BALANCE");
        VenueRejectRow::Frame { frame, words: "INSUFFICIENT_BALANCE".into(), unknown: None }
    }
}

/// Binance's recon client of the lane `symbol` names (`.P` the perp), built offline with throwaway
/// credentials on a demo account's addresses (the release's `venue_url` seed), asked the kind claim.
fn binance_lane_claims(symbol: &str) -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: None,
    };
    let hosts = vike_binance::execution::BinanceUrls::resolve(
        &vike_venues::venue_urls::VenueUrls::seed("binance"),
        vike_model::AccountTier::Demo,
    )
    .expect("the seed holds every binance row");
    vike_binance::recon_client(&creds, symbol, hosts)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// The node claims for the VENUE (`RECONCILE_LISTS_TRIGGER_ORDERS` has no lane), so both lanes' clients must.
fn binance_claims() -> bool {
    binance_lane_claims("BTCUSDT.P") && binance_lane_claims("BTCUSDT")
}

/// Whether `rows` hold a LIVE report of `kind` carrying `order_client_id` (what `check_claim` asks of the first
/// report carrying the order client id).
fn proves(rows: &[OrderStatusReport], kind: &OrderType, order_client_id: &str) -> bool {
    rows.iter().any(|r| {
        r.order_client_id.as_deref() == Some(order_client_id)
            && !r.status.is_terminal()
            && &r.order_type == kind
    })
}

/// Both lanes' rows of `kind`. `check_claim` reads the first report carrying the order client id, so it is handed
/// the perp's when the perp's does not prove the kind and the spot's otherwise: a lane that fails fails
/// the claim, with its own report in the message.
fn binance_open_rows(kind: &OrderType, order_client_id: &str) -> Option<Vec<OrderStatusReport>> {
    let perp = binance_algo_open_rows(kind, order_client_id)?;
    let spot = binance_spot_open_rows(kind, order_client_id)?;
    Some(if proves(&perp, kind, order_client_id) { spot } else { perp })
}

/// A venue-native `GET /api/v3/openOrders` row of `kind` carrying `order_client_id`, through binance's own spot
/// parser: the shape the demo host answered for a resting `STOP_LOSS_LIMIT` and `STOP_LOSS` (2026-11-08,
/// `spot_trigger_listing_tests` in `crates/bridges/binance/src/reconcile/mod.rs`). The take-profit
/// (`TAKE_PROFIT`, a SELL triggering above the market) is that shape with the take-profit word, not a
/// capture; it is read once the binance row lists `take_profit` (its demo smoke first). The trailing
/// stop is placed by a PERCENT only (the `BINANCE` row's `percent_trail_kinds`): a `STOP_LOSS` carrying
/// `trailingDelta` in basis points and its activation as `stopPrice`, the shape of the venue's
/// trailing-stop FAQ and not a capture; it is read once the binance row lists the percent trail.
fn binance_spot_open_rows(
    kind: &OrderType,
    order_client_id: &str,
) -> Option<Vec<OrderStatusReport>> {
    let (spot_type, price, stop_price, trailing_delta) = match kind {
        OrderType::Stop => ("STOP_LOSS", "0.00000000", "90.00000000", None),
        OrderType::StopLimit => ("STOP_LOSS_LIMIT", "89.00000000", "90.00000000", None),
        OrderType::TakeProfit => ("TAKE_PROFIT", "0.00000000", "110.00000000", None),
        OrderType::Trailing => ("STOP_LOSS", "0.00000000", "90.00000000", Some(100)),
        _ => return None,
    };
    let mut row = json!({
        "clientOrderId": order_client_id, "cummulativeQuoteQty": "0.00000000", "executedQty": "0.00000000",
        "icebergQty": "0.00000000", "isWorking": false, "orderId": 7002, "orderListId": -1,
        "origQty": "1.00000000", "origQuoteOrderQty": "0.00000000", "price": price,
        "selfTradePreventionMode": "EXPIRE_MAKER", "side": "SELL", "status": "NEW",
        "stopPrice": stop_price, "symbol": "BTCUSDT", "time": 1, "timeInForce": "GTC",
        "type": spot_type, "updateTime": 1, "workingTime": -1
    });
    if let Some(delta) = trailing_delta {
        row["trailingDelta"] = json!(delta);
        row["trailingTime"] = json!(-1);
    }
    let body = json!([row]).to_string();
    Some(
        vike_binance::reconcile::parse_spot_open_orders(&body, None)
            .expect("a well-formed openOrders body"),
    )
}

/// A venue-native `openAlgoOrders` row of `kind` carrying `order_client_id`, through binance's own parser.
/// The stop (`STOP_MARKET`), the stop-limit (`STOP`, priced at its limit) and the take-profit
/// (`TAKE_PROFIT_MARKET`, a SELL triggering above the market) are placed on the Algo Service
/// (`submit_algo`), so each has a fixture; the take-profit's is read once the binance row lists it. The
/// trailing stop by a PERCENT (`TRAILING_STOP_MARKET`, `callbackRate` the percent, `activatePrice` the
/// activation, the request's own field words and not a capture) is read once the binance row lists the
/// percent trail.
fn binance_algo_open_rows(
    kind: &OrderType,
    order_client_id: &str,
) -> Option<Vec<OrderStatusReport>> {
    let (algo_type, price, trigger_price) = match kind {
        OrderType::Stop => ("STOP_MARKET", "0.00", "90.00"),
        OrderType::StopLimit => ("STOP", "89.00", "90.00"),
        OrderType::TakeProfit => ("TAKE_PROFIT_MARKET", "0.00", "110.00"),
        OrderType::Trailing => ("TRAILING_STOP_MARKET", "0.00", "0.00"),
        _ => return None,
    };
    let mut row = json!({
        "algoId": 7001, "clientAlgoId": order_client_id, "algoType": "CONDITIONAL", "orderType": algo_type,
        "symbol": "BTCUSDT", "side": "SELL", "positionSide": "BOTH", "quantity": "1.000",
        "triggerPrice": trigger_price, "price": price, "algoStatus": "NEW", "reduceOnly": true,
        "updateTime": 1
    });
    if *kind == OrderType::Trailing {
        row["activatePrice"] = json!("110.00");
        row["callbackRate"] = json!("1.0");
    }
    let body = json!([row]).to_string();
    Some(
        vike_binance::execution::rest::algo::parse_perp_open_algo_orders(&body, "binance", None)
            .expect("a well-formed openAlgoOrders body"),
    )
}

/// A `userTrades` body of one fill of the order `venue_order_id`: the binance family spells no order client id
/// on a fill row (binance and aster share it).
fn family_fill_body(venue_order_id: &str) -> Value {
    json!([{
        "symbol": "BTCUSDT", "id": 5001, "orderId": venue_order_id, "side": "BUY",
        "price": "100.0", "qty": "1.0", "commission": "0.05", "commissionAsset": "USDT",
        "time": 5, "maker": true
    }])
}

// --- Bybit (V5 linear-perp execution/order topics; ExecutionClientActor) --------------------------------------

pub(super) struct Bybit;

impl Bybit {
    /// A captured V5 private frame batches rows in `data[]`; the scenarios drive ONE logical
    /// event per frame, so a multi-row capture keeps row 0 only (row structure stays verbatim —
    /// stray sibling rows would otherwise fold foreign-order-client-id events into the ContractOrder).
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
                spawn: |websocket_url, events, fence| {
                    let feed =
                        vike_bybit::execution::user_data::spawn_bybit_perp_user_data_with_resync(
                            websocket_url,
                            "key".into(),
                            "secret".into(),
                            "BTCUSDT".into(),
                            events,
                            fence,
                            Vec::new,
                            None,
                            false,
                            vike_bridge_core::signer::VenueClock::default(),
                        );
                    Box::new(move || {
                        let _ = feed.shutdown();
                    })
                },
            }),
        }
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "bybit", "BTCUSDT")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // The REAL captured order-topic New frame as the structure template.
        if let Some(mut f) = captured_template("bybit", "ws_accepted") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", order_client_id);
            patch_str(&mut f, "/data/0/orderId", venue_order_id);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": order_client_id, "orderStatus": "New", "orderId": venue_order_id,
             "updatedTime": 1}]})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        cum_quantity: f64,
        total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // Bybit's terminality is cumExecQty >= orderQty (fallback leavesQty == 0). Captured
        // template: a REAL execution-topic Trade row; the scenario patches order client id, the execId dedup
        // key, and the qty grid (type-preserving — the real wire's STRING numerics stay strings),
        // so a PARTIAL is the same real row with cum < orderQty.
        if let Some(mut f) = captured_template("bybit", "ws_fill") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", order_client_id);
            patch_str(&mut f, "/data/0/execId", trade_id);
            patch_f64(&mut f, "/data/0/execQty", this_quantity);
            patch_f64(&mut f, "/data/0/execPrice", px);
            patch_f64(&mut f, "/data/0/cumExecQty", cum_quantity);
            patch_f64(&mut f, "/data/0/orderQty", total_qty);
            patch_f64(&mut f, "/data/0/leavesQty", total_qty - cum_quantity);
            return f;
        }
        json!({"topic": "execution", "data": [
            {"execType": "Trade", "orderLinkId": order_client_id, "execId": trade_id, "execTime": 1,
             "side": "Buy", "execQty": this_quantity, "execPrice": px, "execFee": 0.0,
             "feeCurrency": "USDT", "isMaker": false,
             "cumExecQty": cum_quantity, "orderQty": total_qty, "leavesQty": total_qty - cum_quantity}]})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        if let Some(mut f) = captured_template("bybit", "ws_canceled") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", order_client_id);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": order_client_id, "orderStatus": "Cancelled", "cancelType": "CancelByUser",
             "updatedTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_bybit::execution::event_mapper::map_bybit_perp(frame, "bybit", "BTCUSDT")
    }
    fn trigger_listing(&self) -> TriggerListing {
        // The second claimant. `order/realtime` is asked with no `orderFilter`, which the demo host
        // answers with the resting conditional beside the active orders (measured 2026-10-24:
        // `orderFilter=Order` lists only the limits, `StopOrder` only the stop, no filter all of them);
        // the client pages and fails rather than truncating; the trigger kinds bybit's `submit` places
        // are the stop, the stop-limit and the take-profit, and the row below parses a conditional of
        // each into a live report of that kind that carries the order client id.
        TriggerListing::Claims(ClaimProof {
            client_claim: bybit_claims,
            open_rows: bybit_conditional_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // `execution/list` echoes `orderLinkId` on every row: the order client id joins, the order id too.
        FillJoin::Row {
            symbol: "BTCUSDT",
            body: |order_client_id, venue_order_id| {
                json!({"list": [{
                    "execType": "Trade", "execId": "e1", "orderId": venue_order_id, "orderLinkId": order_client_id,
                    "symbol": "BTCUSDT", "side": "Buy", "execQty": "1", "execPrice": "100",
                    "execFee": "0.01", "feeCurrency": "USDT", "isMaker": true, "execTime": "5"
                }]})
            },
            parse: |body| {
                vike_bybit::reconcile::parse_fills(&body.to_string())
                    .expect("a well-formed execution list")
            },
            fields: FillFields::at("/list/0", &["execPrice"], &["execQty"], &["execId"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("/data/0", &["execPrice"], &["execQty"], &["execId"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The captured order-topic row turned into the venue's refusal: `orderStatus` `Rejected` and the
        // `rejectReason` of the sample row in `crates/bridges/bybit/src/execution/resync.rs`'s tests.
        let mut frame = self.frame_accepted(order_client_id, "v-reject-1");
        patch_str(&mut frame, "/data/0/orderStatus", "Rejected");
        patch_str(&mut frame, "/data/0/rejectReason", "EC_NoImmediateQtyToFill");
        VenueRejectRow::Frame { frame, words: "EC_NoImmediateQtyToFill".into(), unknown: None }
    }
}

/// Bybit's recon client, built offline with throwaway credentials, asked the kind claim.
fn bybit_claims() -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: None,
    };
    vike_bybit::recon_client(&creds, "BTCUSDT", "https://unused.invalid")
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// A venue-native `order/realtime` row of the conditional `kind` carrying `order_client_id`, through bybit's own
/// parser. The stop, the stop-limit and the take-profit are placed (`build_order_params`: a conditional that
/// waits for `triggerPrice`, a `Market` for the stop and a `Limit` at `price` for the stop-limit), so each
/// has a fixture. All three are the shapes recorded on the demo host for a SELL: an `Untriggered` conditional
/// with `stopOrderType` `Stop`; the stop a reduce-only `Market` priced `0` with `triggerDirection` 2 (a fall)
/// (2026-10-24, `crates/bridges/bybit/tests/offline/reconcile_paging.rs`'s `RECORDED_STOP_ROW`), the stop-limit a
/// `Limit` priced under its trigger, not reduce-only (2026-11-08, `RECORDED_STOP_LIMIT_ROWS` in bybit's
/// `tests/offline/claim_prereqs.rs`), and the take-profit the same conditional firing the other way
/// (`triggerDirection` 1, a rise), a `Market` priced `0`, not reduce-only (2026-11-08,
/// `RECORDED_TAKE_PROFIT_ROWS` in the same file). The venue echoes `stopOrderType` `Stop` for a take-profit
/// too, and `crates/bridges/bybit/src/reconcile/mod.rs`'s `conditional_order_type` types a `Stop` row firing
/// in the profit direction as a `TakeProfit`.
fn bybit_conditional_open_rows(
    kind: &OrderType,
    order_client_id: &str,
) -> Option<Vec<OrderStatusReport>> {
    let (order_type, price, reduce_only, direction, trigger) = match kind {
        OrderType::Stop => ("Market", "0", true, 2, "90.0"),
        OrderType::StopLimit => ("Limit", "89.0", false, 2, "90.0"),
        OrderType::TakeProfit => ("Market", "0", false, 1, "110.0"),
        _ => return None,
    };
    let body = json!({"list": [{
        "symbol": "BTCUSDT", "orderId": "7001", "orderLinkId": order_client_id, "side": "Sell",
        "orderType": order_type, "price": price, "stopOrderType": "Stop",
        "triggerDirection": direction,
        "triggerPrice": trigger, "reduceOnly": reduce_only, "qty": "1", "cumExecQty": "0",
        "avgPrice": "", "orderStatus": "Untriggered", "updatedTime": "1"
    }]})
    .to_string();
    Some(vike_bybit::reconcile::parse_orders(&body).expect("a well-formed order/realtime body"))
}

// --- OKX (V5 SWAP-perp orders channel; ExecutionClientActor) --------------------------------------------------

pub(super) struct Okx;
impl ConformanceBridge for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        // The `orders` channel pushes a `live` row for a market order too (a `live` row, then its `filled`
        // row in the same millisecond), but the fill behind it can still beat the REST answer, and the A3
        // replay carries no `live` row: this scenario feeds the fill with none ahead of it, the worst case.
        SubmitFenceRow::Fenced {
            pump: Some(FencedPump {
                handshake_replies: &[
                    r#"{"event":"login","code":"0"}"#,
                    r#"{"event":"subscribe","code":"0"}"#,
                ],
                spawn: |websocket_url, events, fence| {
                    let feed = vike_okx::execution::user_data::spawn_okx_perp_user_data_with_resync(
                        websocket_url,
                        "key".into(),
                        "secret".into(),
                        "passphrase".into(),
                        "BTC-USDT-SWAP".into(),
                        |_| Some(1.0),
                        events,
                        fence,
                        Vec::new,
                        None,
                        vike_bridge_core::signer::VenueClock::default(),
                    );
                    Box::new(move || {
                        let _ = feed.shutdown();
                    })
                },
            }),
        }
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "okx", "BTC-USDT-SWAP")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": order_client_id, "state": "live", "ordId": venue_order_id, "uTime": 1}]})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        cum_quantity: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // OKX's terminality is state=="filled" (fallback accFillSz >= sz). ct_val is 1.0 here so
        // base qty passes through unchanged.
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": order_client_id, "state": if terminal { "filled" } else { "partially_filled" },
             "tradeId": trade_id, "fillSz": this_quantity, "fillPx": px, "fillFee": "-0.1",
             "fillFeeCcy": "USDT", "side": "buy", "execType": "T",
             "accFillSz": cum_quantity, "sz": total_qty, "fillTime": 1}]})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": order_client_id, "state": "canceled", "cancelSource": "user", "uTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_okx::execution::event_mapper::map_okx_perp(frame, "okx", "BTC-USDT-SWAP", 1.0)
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
        // how the venue spells a hex order client id on its REST fill rows.
        FillJoin::Row {
            symbol: "BTC-USDT-SWAP",
            body: |order_client_id, venue_order_id| {
                json!([{
                    "instId": "BTC-USDT-SWAP", "tradeId": "t1", "ordId": venue_order_id,
                    "clOrdId": order_client_id, "side": "buy", "fillSz": "1", "fillPx": "100", "fee": "-0.01",
                    "feeCcy": "USDT", "execType": "M", "subType": "1", "ts": "5"
                }])
            },
            parse: |body| {
                vike_okx::reconcile::parse_fills(&body.to_string(), 1.0)
                    .expect("a well-formed fills body")
            },
            fields: FillFields::at("/0", &["fillPx"], &["fillSz"], &["tradeId"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("/data/0", &["fillPx"], &["fillSz"], &["tradeId"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, _order_client_id: &str) -> VenueRejectRow {
        VenueRejectRow::Deferred(
            "okx refuses an order in the order POST's reply (`sCode`/`sMsg`, read inside `VenueRestOkx`'s submit, \
             which needs its REST client), and no sample in its tests shows the `orders` channel carrying a refusal \
             (`map_okx_order`'s per-row `code` gate has none)",
        )
    }
}

/// OKX's recon client, built offline with throwaway credentials, asked the kind claim.
fn okx_claims() -> bool {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "conformance-key".to_string().into(),
        api_secret: "conformance-secret".to_string().into(),
        passphrase: Some("conformance-passphrase".to_string().into()),
    };
    let net = vike_okx::execution::rest::OkxNet::resolve(
        &vike_venues::venue_urls::VenueUrls::seed("okx"),
        vike_model::AccountTier::Demo,
    )
    .expect("the seed's okx demo rows");
    vike_okx::recon_client(&creds, "BTC-USDT-SWAP", 1.0, &net)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

/// A venue-native `orders-algo-pending` row of `kind` carrying `order_client_id`, through okx's own parser. The
/// stop, the stop-limit and the take-profit are placed as one `conditional` algo order
/// (`submit_stop_algo`): a stop's `slOrdPx` is `-1` (market), a stop-limit's is its limit, and a
/// take-profit sends only the take-profit side (`tpTriggerPx`, `tpOrdPx` `-1` for a market-if-touched;
/// `crates/bridges/okx/src/reconcile/mod.rs`'s `conditional_kind` reads it back), so each has a fixture.
/// The take-profit and trailing rows are read since the okx row lists both (after their demo smokes).
/// The trailing stop is proven in BOTH its shapes, the trail in price (`callbackSpread`) and by a
/// percent (`callbackRatio`, the row's `percent_trail_kinds`): `check_claim` reads the first report
/// carrying the order client id, so it is handed the price row while that one fails and the percent row otherwise.
fn okx_algo_open_rows(kind: &OrderType, order_client_id: &str) -> Option<Vec<OrderStatusReport>> {
    // the trailing stop is its own algo order (`move_order_stop`)
    if *kind == OrderType::Trailing {
        let trailing = |spread: &str, ratio: &str| {
            let body = json!([{
                "instId": "BTC-USDT-SWAP", "algoId": "7001", "algoClOrdId": order_client_id,
                "ordType": "move_order_stop", "side": "sell", "sz": "1", "state": "live",
                "callbackSpread": spread, "callbackRatio": ratio, "activePx": "",
                "reduceOnly": "true", "uTime": "1"
            }])
            .to_string();
            vike_okx::reconcile::parse_algo_pending(&body, 1.0)
                .expect("a well-formed orders-algo-pending body")
        };
        let price = trailing("50", "");
        let percent = trailing("", "0.01");
        return Some(if proves(&price, kind, order_client_id) { percent } else { price });
    }
    let (sl_trigger_price, sl_ord_price, tp_trigger_price, tp_ord_price) = match kind {
        OrderType::Stop => ("90", "-1", "", ""),
        OrderType::StopLimit => ("90", "89", "", ""),
        OrderType::TakeProfit => ("", "", "110", "-1"),
        _ => return None,
    };
    let body = json!([{
        "instId": "BTC-USDT-SWAP", "algoId": "7001", "algoClOrdId": order_client_id,
        "ordType": "conditional", "side": "sell", "sz": "1", "state": "live",
        "slTriggerPx": sl_trigger_price, "slOrdPx": sl_ord_price,
        "tpTriggerPx": tp_trigger_price, "tpOrdPx": tp_ord_price, "reduceOnly": "true", "uTime": "1"
    }])
    .to_string();
    Some(
        vike_okx::reconcile::parse_algo_pending(&body, 1.0)
            .expect("a well-formed orders-algo-pending body"),
    )
}

// --- Deribit (user.trades fills + JSON-RPC accept + order-history cancel; ExecutionClientRest) ----------
//
// Deribit is the odd crypto venue: its private-WS `event_mapper` (`map_deribit_private`) is
// FILLS-ONLY for an ordinary order — lifecycle accept is the SYNCHRONOUS `private/buy` JSON-RPC reply,
// and a cancel that never traded surfaces via the order-history replay (`map_deribit_history`); only
// a STOP the venue ends or fires on its own rides the `user.orders` channel, and that row shape is not
// part of this table (`crates/bridges/deribit/tests/offline/stop_events.rs` pins it). So `decode`
// dispatches each venue-native frame to the REAL Deribit mapper that owns that lifecycle edge, and
// the fills stay incremental (`amount` per row, `state=="filled"` the SOLE terminal signal).
pub(super) struct Deribit;
impl ConformanceBridge for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        // The stream carries no accept at all (`user.trades` fills, and `user.orders` rows of stops
        // only), so every fill is ahead of the order's own answer. The real pump (a JSON-RPC
        // handshake whose replies carry the request ids) is not driven here; `order_event_order.rs`
        // in the crate runs it, the replay and the re-dial.
        SubmitFenceRow::Fenced { pump: None }
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::RestPoll
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "deribit", "BTC-PERPETUAL")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // A `private/buy` JSON-RPC reply — the venue half of the emitter split. `label` is our order client id
        // (Deribit echoes it on every order/trade row), `order.order_id` the venue id.
        json!({"id": 1, "result": {"order": {
            "order_id": venue_order_id, "label": order_client_id, "order_state": "open"}}})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // A `user.trades.<instrument>.raw` subscription row: `amount` is the INCREMENTAL fill qty,
        // `state=="filled"` is Deribit's SOLE terminal signal (there are no cum/leaves fields).
        json!({"method": "subscription", "params": {
            "channel": "user.trades.BTC-PERPETUAL.raw", "data": [
            {"trade_id": trade_id, "label": order_client_id, "instrument_name": "BTC-PERPETUAL",
             "direction": "buy", "amount": this_quantity, "price": px, "fee": 0.0,
             "fee_currency": "USDT", "liquidity": "T", "timestamp": 1,
             "state": if terminal { "filled" } else { "open" }}]}})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        // A bare `private/get_order_history_by_instrument` array — `order_state=="cancelled"` is the
        // non-fill terminal the history replay recovers (the fills stream carries no lifecycle cancel).
        json!([{"label": order_client_id, "order_id": "o-1", "order_state": "cancelled",
                "last_update_timestamp": 1}])
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // Fills → the fixture-tested `user.trades` mapper.
        if frame.get("method").and_then(|m| m.as_str()) == Some("subscription") {
            return vike_deribit::execution::event_mapper::map_deribit_private(
                frame,
                "deribit",
                "BTC-PERPETUAL",
            );
        }
        // Order-history array → the history replay mapper (cancel/reject terminals).
        if frame.is_array() {
            return vike_deribit::execution::resync::map_deribit_history(
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
        let (_id, result, error) = vike_deribit::transport::rpc::parse_response(frame);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            let reason = err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            return vec![Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: order_field(&result, "label"),
                reason: reason.into(),
                timestamp: 1,
                refusal_id: None,
            })];
        }
        let order_id = order_field(&result, "order_id");
        vec![Event::OrderAccepted(OrderAccepted {
            order_client_id: order_field(&result, "label"),
            venue_order_id: Some(order_id.into()),
            timestamp: 1,
        })]
    }
    fn trigger_listing(&self) -> TriggerListing {
        // A claimant (okx's, bybit's and binance's rows are the others): deribit places stops
        // (`stop_market`, `stop_limit`, `take_*`) and its open-order read lists them (measured on the
        // testnet 2026-10-26: `type: "all"` returns an `untriggered` row, and the fill of a triggered stop
        // carries the stop's `label`); the row below parses a venue-native row of every kind caps let it
        // place. Decision 0192's three edits are this row, the override on `ReconcileClientDeribit` and
        // `deribit` in `RECONCILE_LISTS_TRIGGER_ORDERS`.
        TriggerListing::Claims(ClaimProof {
            client_claim: deribit_claims,
            open_rows: deribit_open_rows,
        })
    }
    fn fill_join(&self) -> FillJoin {
        // `get_user_trades_by_instrument` rows carry our `label` (the order client id) and the `order_id`.
        FillJoin::Row {
            symbol: "BTC-PERPETUAL",
            body: |order_client_id, venue_order_id| {
                json!([{
                    "trade_id": "t1", "order_id": venue_order_id, "label": order_client_id,
                    "instrument_name": "BTC-PERPETUAL", "direction": "buy", "amount": 10,
                    "price": 100, "fee": 0.0, "fee_currency": "BTC", "liquidity": "M",
                    "timestamp": 5
                }])
            },
            parse: |body| {
                vike_deribit::reconcile::parse_user_trades(&body.to_string())
                    .expect("a well-formed user-trades body")
            },
            fields: FillFields::at("/0", &["price"], &["amount"], &["trade_id"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("/params/data/0", &["price"], &["amount"], &["trade_id"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, _order_client_id: &str) -> VenueRejectRow {
        VenueRejectRow::Deferred(
            "deribit refuses an order in the `private/buy` JSON-RPC error, read inside `VenueRestDeribit`'s \
             `dispatch_submit` (a live socket), which also tells a refusal from an OUTCOME UNKNOWN code \
             (`submit_outcome_unknown`): this harness's accept decode mirrors that construction and is not the \
             venue's code, so it would prove nothing; `crates/bridges/deribit/tests/offline/execution_ambiguous_submit.rs` \
             holds the unknown half",
        )
    }
}

/// Deribit's recon client, built offline: `ReconcileClientDeribit::new` takes a transport that has not
/// dialed (the factory `recon_client` connects to the testnet, which a harness must not), and the claim
/// is static -- it fetches nothing.
fn deribit_claims() -> bool {
    use vike_execution::recon::ReconcileClient;
    let transport = vike_deribit::transport::client::DeribitTransport::new(
        "ws://127.0.0.1:1",
        "conformance-key",
        "conformance-secret",
        None,
    );
    let rest = vike_deribit::execution::rest::VenueRestDeribit::new(
        transport,
        "BTC-PERPETUAL",
        vike_model::SymbolProperties::default(),
        "BTC",
    );
    vike_deribit::reconcile::ReconcileClientDeribit::new(std::sync::Arc::new(rest))
        .lists_trigger_orders()
}

/// A venue-native `private/get_open_orders_by_instrument` row of `kind` carrying `order_client_id`, through
/// deribit's own parser. A trigger order rests as `order_state: "untriggered"` with `price:
/// "market_price"` for the market flavours (the testnet's own shape, `native_stops.rs`'s
/// `held_stop_row`); each kind its capability row places has a row. The trailing stop's is the testnet's
/// captured shape (2026-11-08, `native_stops.rs`'s `CAPTURED_TRAILING_ROW`): a `TSTS-` id, the trail as
/// `trigger_offset` beside the `trigger_price` the venue keeps.
fn deribit_open_rows(kind: &OrderType, order_client_id: &str) -> Option<Vec<OrderStatusReport>> {
    let (order_type, order_id) = match kind {
        OrderType::Stop => ("stop_market", "SLTS-7001"),
        OrderType::StopLimit => ("stop_limit", "SLTS-7001"),
        OrderType::TakeProfit => ("take_market", "SLTS-7001"),
        OrderType::Trailing => ("trailing_stop", "TSTS-7001"),
        _ => return None,
    };
    let mut row = json!({
        "instrument_name": "BTC-PERPETUAL", "order_id": order_id, "label": order_client_id,
        "direction": "sell", "order_type": order_type, "order_state": "untriggered",
        "amount": 10.0, "price": "market_price", "reduce_only": true, "trigger": "last_price",
        "trigger_price": 90.0, "triggered": false, "last_update_timestamp": 1
    });
    if *kind == OrderType::Trailing {
        row["trigger_offset"] = json!(10.0);
    }
    let body = json!([row]).to_string();
    Some(
        vike_deribit::reconcile::parse_open_orders(&body)
            .expect("a well-formed get_open_orders_by_instrument body"),
    )
}

/// Read `result.order.<key>` as an owned string (numbers stringified) — the tiny helper Deribit's
/// `decode` uses to lift the order_client_id/venue-id off a JSON-RPC order reply, mirroring `dispatch_submit`.
fn order_field(result: &Option<Value>, key: &str) -> String {
    match result.as_ref().and_then(|r| r.get("order")).and_then(|o| o.get(key)) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// --- Aster (Binance-wire USDⓈ-M perp ORDER_TRADE_UPDATE; ExecutionClientActor) --------------------------------
//
// Aster's perp user-data stream is Binance-verbatim, so its `perp_mapper` re-exports the shared
// Binance-grammar `map_perp` — ONE mapper carries accept (`x=="NEW"`), incremental fills
// (`x=="TRADE"`, terminal on `X=="FILLED"`), and cancel (`x=="CANCELED"`), exactly like bybit/okx.
//
// ⚠ STARTED ON THE PERP, the way production starts it (`crates/vike-venues/src/venue_wiring.rs`'s
// `VENUE_WIRING` aster row is `BTCUSDT.P`). On the spot spelling the mapper's bare `o.s` label equals the
// order's symbol, so the row stays green while every live perp fill is dropped from the position.
// The frames stay bare (`s: "BTCUSDT"` IS the wire); the order and the mapper fallback carry the
// catalog spelling, exactly as `vike_aster::execution::user_data::perp`'s pump passes it.
pub(super) struct Aster;
impl ConformanceBridge for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        // The binance family's mapper and listenKey pump (see `Binance::submit_fence`).
        SubmitFenceRow::Fenced { pump: None }
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "aster", "BTCUSDT.P")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": order_client_id, "x": "NEW", "i": venue_order_id}})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Aster (Binance perp) terminality is `X=="FILLED"`; `l` is the INCREMENTAL last-fill qty.
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": order_client_id, "x": "TRADE",
            "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            "t": trade_id, "S": "BUY", "l": this_quantity, "L": px, "n": 0.0, "N": "USDT",
            "m": false, "ps": "BOTH"}})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": order_client_id, "x": "CANCELED"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_aster::execution::event_mapper::perp::map_aster_perp(frame, "aster", "BTCUSDT.P")
    }
    fn trigger_listing(&self) -> TriggerListing {
        // aster shares binance's family code but not its Algo Service read; whether aster lists its
        // stop on `openOrders` is unmeasured, so no claim.
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        FillJoin::Row {
            symbol: "BTCUSDT.P",
            body: |_order_client_id, venue_order_id| family_fill_body(venue_order_id),
            parse: |body| {
                vike_aster::reconcile::parse_perp_user_trades(&body.to_string())
                    .expect("a well-formed userTrades body")
            },
            fields: FillFields::at("/0", &["price"], &["qty"], &["id"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        FillFrame { frame: whole_fill(self), fields: FillFields::at("/o", &["L"], &["l"], &["t"]) }
    }
    fn venue_reject(&self, _order_client_id: &str) -> VenueRejectRow {
        VenueRejectRow::Deferred(
            "the perp stream carries no refusal (the binance family's `map_perp` has no `REJECTED` arm): aster \
             refuses an order in the order POST's reply, read inside the family's signed REST client; \
             `crates/bridges/aster/tests/offline/submit_status_unknown.rs` holds its OUTCOME UNKNOWN half",
        )
    }
}

// --- Hyperliquid (orderUpdates lifecycle + /exchange partials; bespoke-signer ExecutionClientActor) -----------
//
// HL splits the FSM lane across two surfaces: WS `orderUpdates` carries accept (`open`), the
// terminal fill (`filled`, qty = `origSz`), and cancel (`canceled`) via the REAL `map_order_updates`;
// but `orderUpdates` has NO partial status, so a resting PARTIAL is only ever an
// `OrderPartiallyFilled` through the `/exchange` submit response (`filled.totalSz` < requested) via
// `map_order_response`. That response is POSITIONAL (the exec side pairs each `statuses[i]` with the
// order it sent), so `frame_fill(partial)` carries the same per-slot context in a non-wire `_context`
// sidecar that `decode` rebuilds into a `SubmittedOrder` before folding through the real mapper.
pub(super) struct Hyperliquid;
impl ConformanceBridge for Hyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own command loop (`crates/bridges/hyperliquid/src/execution/actor.rs`'s `run`), not `run_loop`: the race is not examined by this row",
        )
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn account_lane(&self) -> AccountLane {
        // Both surfaces this row decodes carry FSM wraps only: `map_order_updates`'s `filled` wrap
        // and `map_order_response`'s `/exchange` partial (no bare fill — the fee is not in that
        // response). The position moves on `userFills`, a third lane this table does not drive.
        AccountLane::Separate("userFills (orderUpdates and /exchange carry the FSM wraps only)")
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "hyperliquid", "BTC")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // `orderUpdates status=="open"` rests the order. The `cloid` carries our order client id directly (the
        // caller's cloid→order client id remap is shortcut here, as the other venues' frames carry the order client id).
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "1.0",
                       "oid": venue_order_id, "cloid": order_client_id, "timestamp": 1},
             "status": "open", "statusTimestamp": 1}]})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        if terminal {
            // A completing fill rests as `orderUpdates status=="filled"` → terminal OrderFilled;
            // `origSz` is the wrap's qty, `oid` its FSM-dedup trade id.
            json!({"channel": "orderUpdates", "data": [
                {"order": {"coin": "BTC", "side": "B", "limitPx": px, "sz": "0.0",
                           "origSz": this_quantity, "oid": trade_id, "cloid": order_client_id, "timestamp": 1},
                 "status": "filled", "statusTimestamp": 1}]})
        } else {
            // A resting partial: an `/exchange` response whose `filled.totalSz` (< requested `req_sz`)
            // → OrderPartiallyFilled. `_context` carries the positional SubmittedOrder context.
            json!({"status": "ok",
                   "response": {"type": "order", "data": {"statuses": [
                       {"filled": {"totalSz": this_quantity, "avgPx": px, "oid": trade_id}}]}},
                   "_ctx": [{"order_client_id": order_client_id, "coin": "BTC", "side": 1, "req_sz": total_qty, "ts": 1}]})
        }
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "oid": "o-1", "cloid": order_client_id, "timestamp": 1},
             "status": "canceled", "statusTimestamp": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // An `/exchange` response (our `_context` sidecar present) → rebuild the positional
        // SubmittedOrder context and fold through the REAL response mapper; otherwise it is an
        // `orderUpdates` frame.
        if let Some(ctx) = frame.get("_ctx").and_then(|c| c.as_array()) {
            let orders: Vec<vike_hyperliquid::execution::event_mapper::SubmittedOrder> = ctx
                .iter()
                .map(|o| vike_hyperliquid::execution::event_mapper::SubmittedOrder {
                    order_client_id: o
                        .get("order_client_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    coin: o.get("coin").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    side: o.get("side").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
                    req_sz: o.get("req_sz").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    ts: o.get("ts").and_then(|v| v.as_i64()).unwrap_or(0),
                })
                .collect();
            return vike_hyperliquid::execution::event_mapper::map_order_response(
                frame,
                "hyperliquid",
                &orders,
            );
        }
        // A `userFills` frame (row 9's fixture: the lane that carries this venue's bare fills).
        if frame.get("channel").and_then(|c| c.as_str()) == Some("userFills") {
            return vike_hyperliquid::execution::event_mapper::map_user_fills(frame, "hyperliquid")
                .fills
                .into_iter()
                .map(Event::Fill)
                .collect();
        }
        vike_hyperliquid::execution::event_mapper::map_order_updates(frame, "hyperliquid")
    }
    fn trigger_listing(&self) -> TriggerListing {
        // hyperliquid places stop, stop-limit and take-profit, but whether `frontendOpenOrders`
        // lists an untriggered one and echoes its cloid has never been put on a wire: no claim.
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // `userFills` rows carry `oid` (the venue order id the `open` update acknowledged) and a
        // `cloid` that is a hash of our order client id (the core translates it back); the oid joins.
        FillJoin::Row {
            symbol: "BTC",
            body: |_order_client_id, venue_order_id| {
                json!([{
                    "tid": 1, "oid": venue_order_id, "coin": "BTC", "side": "B", "sz": "1.0",
                    "px": "100.0", "fee": "0.01", "feeToken": "USDC", "crossed": true, "time": 5
                }])
            },
            parse: |body| {
                vike_hyperliquid::reconcile::parse_fills(&body.to_string())
                    .expect("a well-formed userFills body")
            },
            fields: FillFields::at("/0", &["px"], &["sz"], &["tid"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        // Not `frame_fill`: this venue's fill frames carry the FSM wraps only (its `AccountLane`), and the bare fill a
        // position moves on rides `userFills`, in the shape `map_user_fills` reads.
        let frame = json!({"channel": "userFills", "data": {"isSnapshot": false, "fills": [{
            "coin": "BTC", "px": "50000.0", "sz": "1.0", "side": "B", "time": 1, "oid": 7,
            "crossed": true, "fee": "0.01", "feeToken": "USDC", "tid": 101}]}});
        FillFrame { frame, fields: FillFields::at("/data/fills/0", &["px"], &["sz"], &["tid"]) }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The `/exchange` answer's per-order error, in the sentence measured on the testnet for a crossing post-only
        // order (`crates/bridges/hyperliquid/src/execution/event_mapper_tests.rs`). Its OUTCOME UNKNOWN twin: an `ok`
        // envelope with no `statuses`, which says neither taken nor refused.
        let words = "Post only order would have immediately matched, bbo was 100.0@100.5. asset=3";
        let context = json!([{"order_client_id": order_client_id, "coin": "BTC", "side": 1, "req_sz": 1.0, "ts": 1}]);
        let frame = json!({"status": "ok",
            "response": {"type": "order", "data": {"statuses": [{"error": words}]}}, "_ctx": context});
        let unknown =
            json!({"status": "ok", "response": {"type": "order", "data": {}}, "_ctx": context});
        VenueRejectRow::Frame { frame, words: words.into(), unknown: Some(unknown) }
    }
}

// --- OANDA (v20 FX; ExecutionClientActor + transactions stream; whole-fill) -----------------------------------
//
// OANDA is the FX-streaming shape. It fills WHOLE ([`FillShape::Whole`]): the
// order-POST reply carries the accept (`orderCreateTransaction`) and, for a MARKET order, an inline
// fill; delayed LIMIT/STOP fills + cancels arrive one-per-line on the transactions stream. So
// `decode` dispatches: a stream transaction (has `"type"`) → the REAL `decode_transaction_events`
// (`ORDER_FILL` → `[Fill, OrderFilled]`, `ORDER_CANCEL` → `[OrderCanceled]`); anything else is a POST
// reply → `map_order_response`, which takes the order client id as a PARAM (the reply body doesn't echo it into
// the fields the mapper reads), so the accept frame carries it in a non-wire `_order_client_id` sidecar — the
// same shortcut hyperliquid uses for its positional `_context`.
pub(super) struct Oanda;
impl ConformanceBridge for Oanda {
    fn venue(&self) -> &'static str {
        "oanda"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(
            "its own exec, not `run_loop`: its `Submit` arm enters the shared fence itself (`crates/bridges/oanda/src/execution/mod.rs`'s `run_on`), which holds the stream's fills behind the submit",
        )
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        // The USER name (decision 0290); the frames below carry the venue's `EUR_USD`.
        limit_order(order_client_id, "oanda", "EURUSD")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // A resting-LIMIT order-POST reply: `orderCreateTransaction` alone (no fill) → OrderAccepted.
        json!({"_order_client_id": order_client_id,
               "orderCreateTransaction": {"id": venue_order_id, "type": "LIMIT_ORDER"},
               "lastTransactionID": venue_order_id})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A transactions-stream ORDER_FILL — OANDA reports the FULL units at once (whole fill).
        // `units`/`price` are STRING-typed on the wire (the mapper `parse()`s them).
        json!({"type": "ORDER_FILL", "id": trade_id, "time": "1", "orderID": "v-1",
               "instrument": "EUR_USD", "units": this_quantity.to_string(), "price": px.to_string(),
               "commission": "0", "clientExtensions": {"id": order_client_id}})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        json!({"type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "v-1",
               "reason": "CLIENT_REQUEST", "clientExtensions": {"id": order_client_id}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // The REAL decode names a fill by its kept row's USER name (the venue's `EUR_USD` is the order's `EURUSD`),
        // over oanda's kept list held as its catalog parses it.
        vike_oanda::test_support::kept_list_for_tests();
        if frame.get("type").is_some() {
            return vike_oanda::decode_transaction_events(frame, &Default::default());
        }
        let order_client_id =
            frame.get("_order_client_id").and_then(|c| c.as_str()).unwrap_or_default();
        vike_oanda::map_order_response(order_client_id, 1, frame, &Default::default())
    }
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // An `ORDER_FILL` transaction echoes `clientExtensions.id` (the order client id) and `orderID`.
        FillJoin::Row {
            symbol: "EURUSD",
            body: |order_client_id, venue_order_id| {
                json!({"transactions": [{
                    "type": "ORDER_FILL", "id": "900", "instrument": "EUR_USD",
                    "orderID": venue_order_id, "clientExtensions": {"id": order_client_id},
                    "units": "1", "price": "1.1", "commission": "0", "time": "1"
                }]})
            },
            parse: |body| {
                vike_oanda::reconcile::parse_fill_reports(body, "EUR_USD", "EURUSD", "USD")
            },
            fields: FillFields::at("/transactions/0", &["price"], &["units"], &["id"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        FillFrame {
            frame: whole_fill(self),
            fields: FillFields::at("", &["price"], &["units"], &["id"]),
        }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The order POST's refusal in the shape of `crates/bridges/oanda/src/execution/execution_tests.rs`'s sample,
        // and its OUTCOME UNKNOWN twin: a reply that names neither a created nor a refused order.
        let frame = json!({"_order_client_id": order_client_id,
            "orderRejectTransaction": {"id": "6372", "type": "LIMIT_ORDER_REJECT", "rejectReason": "INSUFFICIENT_MARGIN"},
            "lastTransactionID": "6372"});
        let unknown = json!({"_order_client_id": order_client_id, "lastTransactionID": "6372"});
        VenueRejectRow::Frame { frame, words: "INSUFFICIENT_MARGIN".into(), unknown: Some(unknown) }
    }
}

// --- IG (FX/CFD; ExecutionClientActor + Lightstreamer; whole-fill) --------------------------------------------
//
// IG splits its lifecycle across TWO real mappers, and fills WHOLE ([`FillShape::Whole`]): the
// synchronous `/confirms/{ref}` reply carries the accept for a resting working order (`map_confirm`
// with `market=false` → OrderAccepted only — its streaming twin `decode_trade_confirm` deliberately
// NEVER re-emits an accept), while the delayed working-order fill/cancel arrives on the Lightstreamer
// trade-update stream (`decode_trade_confirm`: an OPEN execution with size>0 → `[Fill, OrderFilled]`,
// a DELETED status → `[OrderCanceled]`, a REJECTED dealStatus → `[OrderRejected]`). Both mappers take
// the order client id as a PARAM, carried in the `_order_client_id` sidecar. ⚠ This harness's `decode` routes on `status`
// PRESENCE, a convention of ITS OWN synthetic frames, NOT a wire fact: the live sync `/confirms`
// reply carries `status` too (`crates/bridges/ig/tests/ig_smokes/ig_close_position_smoke.rs` asserts
// `Some("CLOSED")` on the SYNC close confirm). The real bridge routes by SOURCE LANE: the confirm
// fetch in `crates/bridges/ig/src/execution/mod.rs` feeds `map_confirm`, the Lightstreamer frames in
// `crates/bridges/ig/src/execution/user_data.rs` feed `decode_trade_confirm`.
pub(super) struct Ig;
impl ConformanceBridge for Ig {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(OWN_EXECUTION_UNEXAMINED)
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        // The USER name (decision 0290); the frames below carry the venue's epic.
        limit_order(order_client_id, "ig", "EURUSD")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // A working-order `/confirms` reply: dealStatus ACCEPTED, no `status`/`level` (still resting).
        json!({"_order_client_id": order_client_id, "dealStatus": "ACCEPTED", "dealId": venue_order_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY", "size": 1.0})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A streamed CONFIRMS execution: an OPEN working order with a level+size → whole fill.
        json!({"_order_client_id": order_client_id, "dealStatus": "ACCEPTED", "status": "OPEN", "dealId": trade_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY",
               "size": this_quantity, "level": px})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        // A streamed CONFIRMS with `status=="DELETED"` — a working order removed without executing.
        json!({"_order_client_id": order_client_id, "dealStatus": "ACCEPTED", "status": "DELETED", "size": 1.0,
               "reason": "CANCELLED"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let order_client_id =
            frame.get("_order_client_id").and_then(|c| c.as_str()).unwrap_or_default();
        // The REAL decode names a fill by the USER name of the kept row whose epic it states (`EURUSD`), over ig's
        // kept list held as the operator's rows make it.
        vike_ig::hold_kept_list_for_tests();
        // Routes on `status` presence: a convention of this harness's frames (see the IG note).
        if frame.get("status").is_some() {
            return vike_ig::decode_trade_confirm(frame, order_client_id, 1);
        }
        vike_ig::map_confirm(order_client_id, 1, false, frame)
    }
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        FillJoin::Deferred(
            "ig's activity feed (`parse_activity_fills`) carries no order client id, and its venue order id is \
             the activity's `dealReference` (a hash of the order client id), not the deal id the confirm \
             acknowledged: the join is unproven until a live capture settles it",
        )
    }
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("", &["level"], &["size"], &["dealId"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The REAL `/confirms` refusal captured on the demo host (its `_provenance` says how), verbatim but for the
        // harness's order-client-id carrier.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bridges/ig/tests/fixtures/confirm_close_limit_rejected.json");
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing ig capture {}: {e}", path.display()));
        let mut frame: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("malformed ig capture {}: {e}", path.display()));
        let words = frame["reason"].as_str().expect("the capture states IG's reason").to_string();
        if let Some(fields) = frame.as_object_mut() {
            fields.remove("_provenance");
        }
        patch_str(&mut frame, "/_order_client_id", order_client_id);
        VenueRejectRow::Frame { frame, words, unknown: None }
    }
}

// --- Alpaca (US equities/crypto; ExecutionClientActor + SSE; whole-fill) --------------------------------------
//
// Alpaca is the SSE shape and the cleanest of the three: ONE real mapper
// (`decode_trade_event`) decodes every `/v2/events/trades` object — `event=="new"` → OrderAccepted,
// `event=="fill"` → `[Fill, OrderFilled]`, `event=="canceled"` → `[OrderCanceled]` — and the order client id
// rides in `order.client_order_id` on the wire (no sidecar; the venue's field name, which the mapper reads, so it is
// not respelled here). It fills WHOLE ([`FillShape::Whole`]):
// the mapper has no partial state — a `"partial_fill"` event folds into `OrderFilled` too, so the
// cumulative-partial Lifecycle would wrongly reach a terminal on the first (0.4) fill.
pub(super) struct Alpaca;
impl ConformanceBridge for Alpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn submit_fence(&self) -> SubmitFenceRow {
        SubmitFenceRow::Unfenced(OWN_EXECUTION_UNEXAMINED)
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        limit_order(order_client_id, "alpaca", "AAPL")
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        json!({"event": "new", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": venue_order_id, "client_order_id": order_client_id, "symbol": "AAPL",
                         "side": "buy"}})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // event="fill" → dual-publish. `qty`/`price` are STRING-typed on the wire (mapper parses).
        json!({"event": "fill", "timestamp": "2026-07-14T05:39:31.4Z", "execution_id": trade_id,
               "qty": this_quantity.to_string(), "price": px.to_string(),
               "order": {"id": "v-1", "client_order_id": order_client_id, "symbol": "AAPL", "side": "buy"}})
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        json!({"event": "canceled", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": "v-1", "client_order_id": order_client_id, "symbol": "AAPL", "side": "buy"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // The harness carrier of the order POST's reply (a status and its body; no wire shape, the POST needs a live
        // host) → the REAL answer mapper, the transport's write finisher included. Every other frame is the stream's.
        if frame.get("_kind").and_then(Value::as_str) == Some("_post_reply") {
            let order_client_id =
                frame.get("_order_client_id").and_then(Value::as_str).unwrap_or_default();
            let status = frame.get("status").and_then(Value::as_u64).unwrap_or_default();
            let body = frame.get("body").and_then(Value::as_str).unwrap_or_default();
            let status = u16::try_from(status).expect("an HTTP status");
            return vike_alpaca::map_order_post_reply(order_client_id, 1, status, body);
        }
        vike_alpaca::decode_trade_event(frame, &Default::default())
    }
    fn trigger_listing(&self) -> TriggerListing {
        TriggerListing::NoClaim
    }
    fn fill_join(&self) -> FillJoin {
        // A FILL activity carries no order client id at all; its `order_id` is the order id the `new` event
        // acknowledged (`order.id`), so the venue id joins.
        FillJoin::Row {
            symbol: "AAPL",
            body: |_order_client_id, venue_order_id| {
                json!([{
                    "activity_type": "FILL", "id": "a1", "order_id": venue_order_id,
                    "symbol": "AAPL", "side": "buy", "qty": "1", "price": "100.0",
                    "transaction_time": "2026-10-20T00:00:00Z"
                }])
            },
            parse: |body| {
                vike_alpaca::reconcile::parse_fills(&body.to_string(), "AAPL", "AAPL")
                    .expect("a well-formed activities body")
            },
            fields: FillFields::at("/0", &["price"], &["qty"], &["id"]),
        }
    }
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("", &["price"], &["qty"], &["execution_id"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The trade-update `rejected` event. Alpaca's trade update states no reason, so the event's own word is all
        // the venue says.
        let frame = json!({"event": "rejected", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": "v-1", "client_order_id": order_client_id, "symbol": "AAPL", "side": "buy"}});
        // Alpaca's refusal BEFORE it takes an order is the POST's 4xx (`code`/`message`, the Trading API's error
        // shape); its OUTCOME UNKNOWN twin a gateway status, which says nothing of what Alpaca did.
        let post_reply = |status: u16, body: &str| {
            json!({"_kind": "_post_reply", "_order_client_id": order_client_id,
                   "status": status, "body": body})
        };
        let before = RefusedBeforeTaking {
            frame: post_reply(403, r#"{"code":40310000,"message":"insufficient buying power"}"#),
            words: "insufficient buying power".into(),
            unknown: Some(post_reply(504, "<html>gateway timeout</html>")),
        };
        VenueRejectRow::RefusedAfterTaking {
            taken_by: None,
            frame,
            words: "rejected".into(),
            why: "a stream `rejected` comes after the order POST's 2xx and an order id, so it ends an order Alpaca \
                  took (the accept, then `OrderCanceled` in Alpaca's words, as IBKR ends one)",
            before,
        }
    }
}

// --- FXCM (ForexConnect C++ SDK; ExecutionClientActor + a polled shim event queue; whole-fill) ---------------
//
// The venue with no wire at all. FXCM speaks a native C++ SDK, so its "frames" are JSON envelopes
// `crates/bridges/fxcm/src/shim/fcshim.cpp` builds with `snprintf` and enqueues for the exec
// thread to drain — and its ACCEPT is not a frame in any sense: it is the synchronous return of a
// placement FFI call. Both halves still reduce to the harness's stateless seam:
//
// * fill / cancel → the REAL `map_fxcm_event`, sourced from `crates/bridges/fxcm/tests/fixtures/
//   shim_events.json`, which is the shim's own emitter grammar transcribed and kept in step with
//   the C++ by `crates/bridges/fxcm/tests/fxcm_shim/fxcm_shim_envelope_grammar.rs`. So these scenarios run
//   against the shape the venue really produces, and a shim rename reddens there rather than here.
// * accept / reject → the REAL `map_placement`, lifted out of the exec loop for exactly this. The
//   `_placement` frame below is a HARNESS CARRIER, not a wire shape (there is no wire): it holds
//   the two values the FFI call returns, and `decode` folds them through the venue's own function.
//   Same shortcut oanda/ig use for their `_order_client_id` param and hyperliquid for its positional `_context`.
//
// [`FillShape::Whole`], on the same evidence oanda/ig/alpaca carry: `map_fxcm_event` has NO
// `OrderPartiallyFilled` path at all — every `kind:"fill"` envelope maps to a terminal `OrderFilled`
// (FXCM's Trades table reports each execution as a whole trade row, not a cumulative on an order).
// [`ExecutionKind::CommandActor`]: fxcm's client is the shared `ExecutionClientActor` itself.
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
        SubmitFenceRow::Unfenced(OWN_EXECUTION_UNEXAMINED)
    }
    fn execution_kind(&self) -> ExecutionKind {
        ExecutionKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, order_client_id: &str) -> OrderRequest {
        // NO `price`, unlike every other venue's `limit_order`: a priced limit is placed through the
        // ABI 2 entry point (`forex_connect_place_entry_at`), which this harness cannot drive, and an unpriced
        // one is the shape every shim — old or new — accepts, so it is the lifecycle this row folds.
        //
        // ⚠ `qty` is BASE UNITS, never a lot count — the fill patched from it (`/amount`) is base
        // units too. `vike_fxcm::execution::event_mapper::lots_for` divides by the live base unit size and
        // refuses an inexact size, so one lot of the ordinary EUR/USD 1000 is what a request is.
        OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "fxcm".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Limit,
            price: None,
            timestamp: 1,
            ..Default::default()
        }
    }
    fn frame_accepted(&self, order_client_id: &str, venue_order_id: &str) -> Value {
        // The harness carrier for a SYNCHRONOUS placement return (see the module note above).
        json!({"_kind": "_placement", "_order_client_id": order_client_id, "order_id": venue_order_id})
    }
    fn frame_fill(
        &self,
        order_client_id: &str,
        trade_id: &'static str,
        this_quantity: f64,
        _cum_quantity: f64,
        _total_quantity: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // The shim's REAL fill envelope, scenario leaves patched in. `amount` is the executed size
        // in BASE UNITS and `trade_id` the reconnect-dedup key; `_terminal` is unread because this
        // venue has no partial state to select — see [`FillShape::Whole`].
        let mut f = Self::envelope("fill");
        patch_str(&mut f, "/_order_client_id", order_client_id);
        patch_str(&mut f, "/trade_id", trade_id);
        patch_f64(&mut f, "/amount", this_quantity);
        patch_f64(&mut f, "/rate", px);
        f
    }
    fn frame_canceled(&self, order_client_id: &str) -> Value {
        let mut f = Self::envelope("canceled");
        patch_str(&mut f, "/_order_client_id", order_client_id);
        f
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let order_client_id =
            frame.get("_order_client_id").and_then(|c| c.as_str()).unwrap_or_default();
        // A placement carrier → the REAL placement mapper (the venue half of the emitter split); a failed one names
        // what it was by the shim's code and FXCM's words, as the exec thread's does (`placement_refusal`).
        if frame.get("_kind").and_then(|k| k.as_str()) == Some("_placement") {
            use vike_fxcm::execution::event_mapper::{map_placement, placement_refusal};
            if let Some(failed) = frame.get("failed") {
                let code = failed.get("code").and_then(Value::as_i64).unwrap_or_default();
                let code = i32::try_from(code).expect("a shim return code");
                let words = failed.get("message").and_then(Value::as_str).unwrap_or_default();
                let refusal = placement_refusal(code, words);
                return map_placement(order_client_id, 1, Err((words, refusal)));
            }
            let oid = frame.get("order_id").and_then(|o| o.as_str()).unwrap_or_default();
            return map_placement(order_client_id, 1, Ok(oid));
        }
        // …otherwise a drained shim envelope → the REAL decode, which names a fill by its kept row's USER name
        // (decision 0290: the venue's `EUR/USD` is the order's `EURUSD`), over fxcm's kept list held as its catalog makes it.
        vike_fxcm::instrument::names::kept_list_for_tests();
        vike_fxcm::execution::event_mapper::map_fxcm_event(frame, order_client_id)
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
    fn malformed_fill(&self) -> FillFrame {
        let fields = FillFields::at("", &["rate"], &["amount"], &["trade_id"]);
        FillFrame { frame: whole_fill(self), fields }
    }
    fn venue_reject(&self, order_client_id: &str) -> VenueRejectRow {
        // The shim's own `rejected` envelope (the committed grammar), carrying the reason the shim writes, after the
        // synchronous placement that took the order (the harness carrier `frame_accepted` builds).
        let mut frame = Self::envelope("rejected");
        let words =
            frame["reason"].as_str().expect("the shim's rejection states a reason").to_string();
        patch_str(&mut frame, "/_order_client_id", order_client_id);
        // fxcm's refusal BEFORE it takes an order is the placement call's own failure: the shim's `-6`
        // (`onRequestFailed`) in FXCM's words, the `ORA-20102` measured on the demo for a permission the account
        // lacks; its OUTCOME UNKNOWN twin the shim's `-11`, sent with no answer.
        let failed = |code: i32, message: &str| {
            json!({"_kind": "_placement", "_order_client_id": order_client_id,
                   "failed": {"code": code, "message": message}})
        };
        let before = RefusedBeforeTaking {
            frame: failed(-6, "ORA-20102: Access Violation"),
            words: "ORA-20102: Access Violation".into(),
            unknown: Some(failed(-11, "no response inside the wait")),
        };
        VenueRejectRow::RefusedAfterTaking {
            taken_by: Some(self.frame_accepted(order_client_id, "v-reject-1")),
            frame,
            words,
            why: "the shim's `rejected` envelope is the Orders table's status R of an order whose synchronous \
                  placement already returned its order id, so it ends an order fxcm took (`OrderCanceled` in the \
                  shim's words, batch 4f's #3457)",
            before,
        }
    }
}
