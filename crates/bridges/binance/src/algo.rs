//! Binance USDⓈ-M CONDITIONAL orders on the **Algo Service** — binance only.
//!
//! ## Why this module exists
//!
//! Since **2025-12-09** (announced 2025-11-06, USDⓈ-M change log) `POST /fapi/v1/order` and
//! `POST /fapi/v1/batchOrders` REFUSE `STOP`, `STOP_MARKET`, `TAKE_PROFIT`, `TAKE_PROFIT_MARKET` and
//! `TRAILING_STOP_MARKET` with `-4120 STOP_ORDER_SWITCH_ALGO` ("Order type not supported for this
//! endpoint. Please use the Algo Order API endpoints instead."). They are placed on
//! `POST /fapi/v1/algoOrder` with `algoType=CONDITIONAL`. This crate sends exactly one conditional
//! kind, `OrderType::Stop` as `STOP_MARKET`; every other path of the perp lane is unchanged.
//!
//! `family/` is NOT touched: aster shares it, and nothing in aster's docs says its fapi fork moved
//! conditional orders. `family::order_map::build_perp_order_params`'s `Stop` arm (`STOP_MARKET` +
//! `stopPrice`) is therefore aster's alone; binance's stop is [`build_perp_algo_params`].
//!
//! ## The wire (developers.binance.com, re-read 2026-10-19; the names below are the page's own)
//!
//! * **New Algo Order** `POST /fapi/v1/algoOrder`, weight 1 on the 10 s and 1 min ORDER limits, 0 on the
//!   IP weight. Mandatory: `algoType` (only `CONDITIONAL`), `symbol`, `side`, `type`, `timestamp`.
//!   Used here: `quantity`, `triggerPrice` (was `stopPrice`), `clientAlgoId` (was `newClientOrderId`;
//!   "Unique among open orders", `^[\.A-Z\:/a-z0-9_-]{1,36}$`, the same ceiling as the broker coid),
//!   `positionSide`, `reduceOnly` (the strings `"true"`/`"false"`), `workingType` (`MARK_PRICE` |
//!   `CONTRACT_PRICE`), `newOrderRespType` (`ACK`). NOT sent: `closePosition`, `priceProtect`,
//!   `timeInForce` ("Only `STOP` and `TAKE_PROFIT` algo orders can send it"; a `STOP_MARKET` carries
//!   none). The ack has **`algoId`** and no `orderId`: `{algoId, clientAlgoId, algoType, orderType,
//!   symbol, side, positionSide, timeInForce, quantity, algoStatus, triggerPrice, price, ...}`.
//! * **Cancel Algo Order** `DELETE /fapi/v1/algoOrder`: `algoId` OR `clientAlgoId` (one must be sent),
//!   `timestamp`; **no `symbol`**. Answers `{algoId, clientAlgoId, code: "200", msg: "success"}`.
//! * **Query Algo Order** `GET /fapi/v1/algoOrder`: `algoId` OR `clientAlgoId`. The body adds
//!   `actualOrderId`, `actualPrice`, `actualType`, `actualQty` once triggered. A `CANCELED`/`EXPIRED`
//!   order with no fills older than 3 days, and anything older than 90 days, is no longer found.
//! * **Current All Algo Open Orders** `GET /fapi/v1/openAlgoOrders`: `symbol` optional (weight 1 with
//!   it, 40 without), rows shaped like the New Algo Order response. `GET /fapi/v1/openOrders` is NOT
//!   documented to list them, so the reconcile paths read both.
//! * **Query All Algo Orders** `GET /fapi/v1/allAlgoOrders`: weight 5, `symbol` MANDATORY, `algoId` (ids
//!   at or above it), `startTime`/`endTime` (the window must be under 7 days, default the latest 7),
//!   `limit` (default 500, max 1000; the latest orders when `algoId` is omitted). Rows are the Query
//!   Algo Order body (`algoStatus`, `clientAlgoId`, `updateTime`, ...); an unfilled `CANCELED`/`EXPIRED`
//!   row older than 3 days, and anything older than 90 days, is not returned. `allOrders` does not list
//!   an algo order, so this is the only record of a stop that died while the stream was down.
//! * **`ALGO_UPDATE`** (user-data): `{e, T, E, o: {caid, aid, at, o, s, S, ps, f, q, X, ai, ap, aq, act,
//!   tp, p, V, wt, pm, cp, pP, R, tt, gtd, rm, ia}}` — `caid` client algo id, `aid` algo id, `X` one of
//!   `NEW | CANCELED | TRIGGERING | TRIGGERED | FINISHED | REJECTED | EXPIRED`, `ai` the order id in the
//!   matching engine ("populated after the order is placed"), `rm` the failure reason. The portfolio-
//!   margin page spells the same payload `ao` (plus `fs`); [`map_algo_update`] reads `o` then `ao`.
//! * "Modification of untriggered conditional orders is not supported" (change log 2025-11-06).
//!
//! ## What the pages do not say, MEASURED on the DEMO host 2026-10-19 (`tests/perp_algo_smoke.rs`)
//!
//! * **The triggered stop's CHILD order reports `ORDER_TRADE_UPDATE.o.c == clientAlgoId`** (and
//!   `o.i == ALGO_UPDATE.ai`; the child also carries `si` = the algo id and `st` = `ALGO_CONDITION`), so
//!   the family mapper attributes the fill to the stop's own order with no help. The sequence on the
//!   wire is `ALGO_UPDATE TRIGGERING` (`ai` empty) -> the child's `NEW` and `TRADE` frames ->
//!   `ALGO_UPDATE TRIGGERED` (`ai` set, `ap`/`aq` still 0) -> `ALGO_UPDATE FINISHED` (`ap`/`aq` filled):
//!   the fill arrives BEFORE `TRIGGERED`, which is why a re-key keyed on `ai` could never have worked.
//! * `ALGO_UPDATE` is pushed on `DEMO_FAPI_WS` (`wss://fstream.binancefuture.com/ws`), the algo object
//!   under `o` (not the portfolio-margin page's `ao`), `NEW` included.
//! * `GET /fapi/v1/openOrders` does NOT list a resting stop; `GET /fapi/v1/openAlgoOrders` does.
//! * The regular `DELETE /fapi/v1/order` of an algo `clientAlgoId` answers `-2011`; `DELETE` of an unknown
//!   id on the algo endpoint answers `-2011`, `GET` of one `-2013` ([`is_unknown_order`]'s two words);
//!   `GET algoOrder` can read `NEW` for a moment after the `CANCELED` push, like the order query's replica lag.
//! * The old call (`STOP_MARKET` on `/fapi/v1/order`) answers `-4120` on the demo host too.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::{get_i64 as i, get_str_boolless as s, json_num, parse_rows};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_bridge_core::trigger;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected, OrderTriggered,
};
use vike_model::{
    ManagedOrder, OrderRequest, OrderStatus, OrderStatusReport, OrderType, SymbolProperties,
};

use crate::family::order_map::{binance_broker_coid, json_id, strip_broker_coid_prefix};
use crate::family::recon::{FamilyReconClient, ReadFail};
use crate::family::rest_reads::FoundOrder;
use crate::perp::BinancePerpRest;

/// `POST` (new) / `DELETE` (cancel) / `GET` (query) a conditional order.
pub const PATH_ALGO_ORDER: &str = "/fapi/v1/algoOrder";
/// `GET` the open conditional orders (the reconcile read).
pub const PATH_OPEN_ALGO_ORDERS: &str = "/fapi/v1/openAlgoOrders";
/// `GET` the recent conditional orders in any state (the post-reconnect resync read).
pub const PATH_ALL_ALGO_ORDERS: &str = "/fapi/v1/allAlgoOrders";

/// The advisory a stop modify answers (no wire call is made).
const MODIFY_REFUSAL: &str = "binance: a conditional (stop) order cannot be modified in place \
    (Algo Service: modification of untriggered conditional orders is not supported); cancel it and \
    place a new one";

/// Do `code`'s words say "the venue has no such order"? `-2011` (cancel) and `-2013` (query) are the
/// order endpoints' words; the algo endpoints' own are unmeasured (the demo smoke records them), so
/// anything else is an error the caller must not read as "gone".
pub fn is_unknown_order(code: i64) -> bool {
    code == -2011 || code == -2013
}

// ---- the set of algo client ids --------------------------------------------------------------

/// The BARE client order ids this client sent to the algo endpoint (or read off `openAlgoOrders`): a
/// cancel or a status query of a member goes to the algo endpoint first.
///
/// A HINT, not the truth: a restarted process starts empty (the engine's restore hands the adapter a
/// coid and a symbol, not a kind), so every consumer also has the non-member fallback — a regular
/// answer of "unknown" is retried once on the algo endpoint before anyone concludes "gone".
///
/// shortcut: never evicted except by a cancel, one short string per stop for the life of the client;
/// evict on a terminal event if the growth ever matters.
#[derive(Default)]
pub struct AlgoCoids(Mutex<HashSet<String>>);

impl AlgoCoids {
    pub fn insert(&self, coid: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(coid.to_string());
    }

    pub fn remove(&self, coid: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(coid);
    }

    pub fn contains(&self, coid: &str) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).contains(coid)
    }
}

// ---- submit ----------------------------------------------------------------------------------

/// The New Algo Order params of a stop — pure, golden-gated (`algo_tests.rs`). The order is load-
/// bearing and mirrors the old `STOP_MARKET` shape with the renames the Algo Service made:
/// `algoType` is new, `newClientOrderId` is `clientAlgoId`, `stopPrice` is `triggerPrice` (still the
/// tail, `workingType` still after it), and there is no `timeInForce` on a `STOP_MARKET`.
///
/// `venue` selects the trigger-source row (`trigger::venue_trigger_by`; binance's perp lane passes
/// [`crate::perp::TIF_LANE`]): a requested `trigger_by` of Last/Mark becomes `workingType`, a `None`
/// request emits none (the venue default `CONTRACT_PRICE` rules). The submit gates (tif, trigger
/// source, gtd) run BEFORE this in the caller, exactly as for the regular builder.
pub fn build_perp_algo_params(
    request: &OrderRequest,
    symbol: &str,
    properties: &SymbolProperties,
    venue: &str,
    link_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut params: Vec<(&'static str, String)> = vec![
        ("algoType", "CONDITIONAL".to_string()),
        ("symbol", symbol.to_string()),
        ("side", if request.side > 0 { "BUY" } else { "SELL" }.to_string()),
        ("type", "STOP_MARKET".to_string()),
        ("quantity", format_to_step_f(request.qty, properties.step_size)),
        ("clientAlgoId", binance_broker_coid(link_id, &request.client_order_id)),
        ("newOrderRespType", "ACK".to_string()),
        ("positionSide", "BOTH".to_string()), // one-way; hedge LONG/SHORT is a future gap
        ("reduceOnly", if request.reduce_only { "true" } else { "false" }.to_string()),
        (
            "triggerPrice",
            format_to_step_f(request.trigger_price.unwrap_or(0.0), properties.tick_size),
        ),
    ];
    if let Some(wire) =
        request.trigger_by.and_then(|src| trigger::venue_trigger_by(venue, src).wire())
    {
        params.push(("workingType", wire.to_string()));
    }
    params
}

impl<S: Signer, T: RestTransport> BinancePerpRest<S, T> {
    /// [`build_perp_algo_params`] over this client's symbol, grid and link id.
    pub fn build_algo_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        build_perp_algo_params(
            request,
            &self.symbol,
            &self.properties,
            crate::perp::TIF_LANE,
            self.link_id.as_deref(),
        )
    }

    /// The events AFTER `OrderSubmitted` for a stop: `POST /fapi/v1/algoOrder`, accepted with the
    /// ack's **`algoId`** (never `orderId`: the response has none), rejected with the venue's own
    /// words, or — on a timeout that may have landed — resolved by a lookup that asks the algo
    /// endpoint FIRST (the coid is in [`AlgoCoids`] before the POST is sent).
    pub(crate) fn submit_algo(&self, request: &OrderRequest) -> Vec<Event> {
        let params = self.build_algo_params(request);
        self.algo_coids.insert(&request.client_order_id);
        match self.transport.signed(&self.base_url, PATH_ALGO_ORDER, "POST", &params, &self.signer)
        {
            Ok(resp) => vec![Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: resp
                    .get("algoId")
                    .map(json_id)
                    .filter(|id| !id.is_empty())
                    .map(Into::into),
                ts: request.ts,
            })],
            Err(exc) if crate::error_codes::submit_outcome_unknown(exc.code) => {
                self.resolve_ambiguous_submit(request)
            }
            Err(exc) => {
                self.algo_coids.remove(&request.client_order_id);
                tracing::warn!(target: "vike_binance::algo", code = exc.code, msg = %exc.msg, "algo submit rejected");
                vec![Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                })]
            }
        }
    }

    /// The refusal a stop modify answers: the existing non-terminal `modify_failed` advisory, NO wire
    /// call (the venue does not modify an untriggered conditional order).
    pub(crate) fn refuse_stop_modify(&self, order: &OrderRequest) -> Vec<Event> {
        vec![crate::error_codes::modify_failed(
            &order.client_order_id,
            &VenueApiError { code: 0, msg: MODIFY_REFUSAL.to_string() },
        )]
    }

    /// Is `order` a conditional order the Algo Service holds (or would hold)?
    pub(crate) fn is_algo_order(&self, order: &OrderRequest) -> bool {
        order.order_type == OrderType::Stop || self.algo_coids.contains(&order.client_order_id)
    }

    // ---- cancel ------------------------------------------------------------------------------

    /// `DELETE /fapi/v1/algoOrder` by `clientAlgoId` (as the venue stored it: re-prefixed with the
    /// broker link id). The raw verb: the caller judges the venue's answer.
    fn delete_algo(&self, coid: &str) -> Result<(), VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("clientAlgoId", binance_broker_coid(self.link_id.as_deref(), coid))];
        self.transport
            .signed(&self.base_url, PATH_ALGO_ORDER, "DELETE", &params, &self.signer)
            .map(|_| ())
    }

    /// Cancel a conditional order. "Unknown" (-2011/-2013) means it already left the venue by itself
    /// (triggered, expired, cancelled elsewhere) — idempotent, like the regular cancel; any other
    /// answer is an error.
    ///
    /// Called for a member of [`AlgoCoids`] (straight to this endpoint) and as the ONE retry for a
    /// coid the regular endpoint did not know: a resting stop is never reported cancelled because of
    /// an answer that was only about the wrong endpoint, and a FAILED retry is an error, not "gone".
    pub(crate) fn cancel_algo(&self, coid: &str) -> Result<(), VenueApiError> {
        match self.delete_algo(coid) {
            Ok(()) => {
                self.algo_coids.remove(coid);
                Ok(())
            }
            Err(exc) if is_unknown_order(exc.code) => {
                self.algo_coids.remove(coid);
                tracing::debug!(target: "vike_binance::algo", coid, "cancel: algo order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    // ---- lookup ------------------------------------------------------------------------------

    /// `GET /fapi/v1/algoOrder` by `clientAlgoId`: `Ok(Some)` carries the `algoId` and the
    /// `algoStatus` word (`NEW | TRIGGERING | TRIGGERED | FINISHED` are live words for
    /// `family::confirm::found_events`, whose unknown-word arm is "live"; `CANCELED`, `EXPIRED` and
    /// `REJECTED` are its terminals); `Ok(None)` = the venue has no such algo order; `Err` = the query
    /// failed (says nothing about the order).
    pub(crate) fn lookup_algo(&self, coid: &str) -> Result<Option<FoundOrder>, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("clientAlgoId", binance_broker_coid(self.link_id.as_deref(), coid))];
        match self.transport.signed_requery(
            &self.base_url,
            PATH_ALGO_ORDER,
            "GET",
            &params,
            &self.signer,
        ) {
            Ok(resp) => Ok(resp
                .get("algoId")
                .map(|id| FoundOrder { order_id: json_id(id), status: s(&resp, "algoStatus") })),
            Err(exc) if is_unknown_order(exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    // ---- reconcile (the legacy snapshot) -------------------------------------------------------

    /// `GET /fapi/v1/openAlgoOrders?symbol=` -> reconcile-seeded ACCEPTED [`ManagedOrder`]s.
    ///
    /// **STRICT, unlike the regular read beside it:** `apply_snapshot` terminalizes every live local
    /// order the snapshot lacks, so a failed (or malformed) algo read must not become "no stops" —
    /// it fails the whole `reconcile_positions`, and no snapshot is applied. Every coid read is also
    /// remembered in [`AlgoCoids`], so a cancel after this adoption goes straight to the algo endpoint.
    pub(crate) fn fetch_open_algo_orders(&self) -> Result<Vec<ManagedOrder>, VenueApiError> {
        let raw = self.transport.signed(
            &self.base_url,
            PATH_OPEN_ALGO_ORDERS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        )?;
        let rows = raw.as_array().ok_or_else(|| VenueApiError {
            code: 0,
            msg: format!("{PATH_OPEN_ALGO_ORDERS}: expected a JSON array"),
        })?;
        let orders: Vec<ManagedOrder> =
            rows.iter().map(|o| map_perp_open_algo_order(o, crate::VENUE)).collect();
        for mo in &orders {
            self.algo_coids.insert(mo.client_order_id());
        }
        Ok(orders)
    }
}

// ---- open orders: the two reconcile shapes -------------------------------------------------------

/// The order kind of an algo row's `orderType`: the same words `family::recon`'s report parser maps
/// (`STOP_MARKET` -> Stop, `STOP` -> StopLimit, `TAKE_PROFIT[_MARKET]` -> TakeProfit); anything else
/// (`TRAILING_STOP_MARKET`) stays [`OrderType::Unrecognized`] lower-cased.
fn algo_order_type(raw: &str) -> OrderType {
    match raw {
        "STOP_MARKET" => OrderType::Stop,
        "STOP" => OrderType::StopLimit,
        "TAKE_PROFIT_MARKET" | "TAKE_PROFIT" => OrderType::TakeProfit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// A finite, positive number off a string field, or `None` (the venue spells "no price" `"0.000"`).
fn positive(row: &Value, key: &str) -> Option<f64> {
    row.get(key).and_then(json_num).filter(|v| v.is_finite() && *v > 0.0)
}

/// One `openAlgoOrders` row -> a reconcile-seeded ACCEPTED [`ManagedOrder`]: the stop's `triggerPrice`
/// rides `trigger_price`, `reduceOnly` its flag, the `algoId` the venue order id, the coid is the
/// broker-prefix-stripped `clientAlgoId`. Total over any row (a non-finite quantity reads `0.0`, like
/// `family::order_map::map_perp_open_order`).
pub fn map_perp_open_algo_order(o: &Value, venue: &str) -> ManagedOrder {
    let qty = o.get("quantity").and_then(json_num).filter(|q| q.is_finite()).unwrap_or(0.0);
    let request = OrderRequest {
        client_order_id: strip_broker_coid_prefix(&s(o, "clientAlgoId")).to_string(),
        venue: venue.to_string(),
        symbol: s(o, "symbol"),
        side: if s(o, "side") == "BUY" { 1 } else { -1 },
        qty,
        order_type: algo_order_type(&s(o, "orderType")),
        price: positive(o, "price"),
        trigger_price: positive(o, "triggerPrice"),
        reduce_only: o.get("reduceOnly").and_then(Value::as_bool).unwrap_or(false),
        ..Default::default()
    };
    let mut mo = ManagedOrder::new(request);
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = Some(o.get("algoId").map(json_id).unwrap_or_default());
    mo
}

/// An open-list row's `algoStatus` as the report FSM word. The list holds live orders, so the live
/// words (`NEW`, `TRIGGERING`, `TRIGGERED`) and anything unknown read `Accepted` — the conservative
/// direction, as `family::recon`'s status table reads an unknown word.
fn algo_report_status(raw: &str) -> OrderStatus {
    match raw {
        "CANCELED" => OrderStatus::Canceled,
        "EXPIRED" => OrderStatus::Expired,
        "REJECTED" => OrderStatus::Rejected,
        _ => OrderStatus::Accepted,
    }
}

/// `GET /fapi/v1/openAlgoOrders` rows -> [`OrderStatusReport`]s (the reconciliation-engine report
/// path), the perp series label on `symbol`.
pub fn parse_perp_open_algo_orders(
    body: &str,
    venue: &str,
) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| OrderStatusReport {
        venue: venue.to_string(),
        symbol: crate::family::perp_series_symbol(&s(o, "symbol")),
        venue_order_id: o.get("algoId").map(json_id).unwrap_or_default().into(),
        client_order_id: Some(strip_broker_coid_prefix(&s(o, "clientAlgoId")).to_string())
            .filter(|c| !c.is_empty()),
        side: if s(o, "side") == "BUY" { 1 } else { -1 },
        order_type: algo_order_type(&s(o, "orderType")),
        qty: o.get("quantity").and_then(json_num).filter(|q| q.is_finite()).unwrap_or(0.0),
        filled_qty: 0.0,
        avg_px: 0.0,
        status: algo_report_status(&s(o, "algoStatus")),
        ts: o.get("updateTime").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// The algo half of the report path: `openAlgoOrders` for the mounted symbol and every admitted one,
/// as the family's own `openOrders` fetch reads them (`FamilyReconClient::fetch_order_status_reports`),
/// appended to its result by `BinanceReconClient`.
///
/// ⚠ **Any failure fails the whole report.** The restore planner reads an order a successful report
/// lacks as gone, so a stop whose list could not be read must not be reported as absent. The ADMITTED
/// symbols' reads are judged by the family's own rule (`FamilyReconClient::admitted_rows`, the very one
/// its `openOrders` read goes through): a symbol the venue no longer lists (`-1121`) holds nothing, so
/// that is no rows — one delisted symbol must not blind the pass for good — and a RATE LIMIT starts
/// the pass-wide 60 s pause, so the next fetches (orders, fills, positions) send nothing until it
/// lapses instead of retrying 17 reads at full width into a ban.
///
/// ⚠ A failure here still costs the whole account pass its orders, fills, positions and the balance
/// seed (the pass composes them with `?`): an algo-only outage has the reach of an `openOrders` one.
/// Strict is the right side of that trade — a report that silently lacked the stops would make the
/// restore planner declare resting protective orders gone — and the pass reports it loudly.
pub fn fetch_open_algo_reports<S: Signer, T: RestTransport>(
    client: &FamilyReconClient<S, T>,
) -> Result<Vec<OrderStatusReport>, String> {
    let read = |symbol: &str| -> Result<Vec<OrderStatusReport>, ReadFail> {
        let body = client.signed_raw(PATH_OPEN_ALGO_ORDERS, &[("symbol", symbol.to_string())])?;
        parse_perp_open_algo_orders(&body, crate::VENUE)
            .map_err(|msg| ReadFail::Parse(format!("{PATH_OPEN_ALGO_ORDERS}: {msg}")))
    };
    let mut reports = read(client.symbol()).map_err(String::from)?;
    for (label, symbol) in client.admitted_targets() {
        let rows = client.admitted_rows(&label, read(&symbol))?;
        reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&label)));
    }
    Ok(reports)
}

// ---- user data: ALGO_UPDATE ----------------------------------------------------------------------

/// The TERMINAL event a stop's status word stands for (`ALGO_UPDATE.X` and `allAlgoOrders.algoStatus`
/// share the words), or `None` for any other. `REJECTED` is an `OrderCanceled`, NOT an `OrderRejected`:
/// the FSM allows that only from Initialized/Submitted and a stop that reaches `REJECTED` (its trigger
/// refused by the matching engine, e.g. a margin check) is already Accepted.
fn terminal_event(coid: String, status: &str, reason: String, ts: i64) -> Option<Event> {
    match status {
        "CANCELED" | "REJECTED" => Some(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: reason.into(),
            ts,
        })),
        "EXPIRED" => Some(Event::OrderExpired(OrderExpired { client_order_id: coid, ts })),
        _ => None,
    }
}

/// `ALGO_UPDATE` -> the order FSM, by `X`:
///
/// | `X` | event |
/// |---|---|
/// | `NEW` | `OrderAccepted` with the frame's `aid` as the venue order id: the stream's SECOND ack. Every regular order is rescued by its `ORDER_TRADE_UPDATE` `NEW` when the REST ack is lost (5xx, timeout) and a stop must be too, or a stop whose ack AND confirm both fail while the stream is healthy is rejected by the stuck-order watchdog's backstop while it rests at the venue. The FSM drops a repeat silently (`Accepted` allows `OrderAccepted` only from `Submitted`). |
/// | `CANCELED` | `OrderCanceled` |
/// | `EXPIRED` | `OrderExpired` |
/// | `REJECTED` | `OrderCanceled` carrying `rm` (see [`terminal_event`]) |
/// | `TRIGGERING`, `TRIGGERED` | `OrderTriggered` (the second one lands on a stop the child's fill already filled and is dropped by the FSM) |
/// | `FINISHED` | none (the child's own fills / cancel end the order) |
///
/// The coid is `caid` with the broker prefix stripped. The object is read under `o` (measured), then `ao`.
///
/// The child's fill itself needs no help: it arrives as an ordinary `ORDER_TRADE_UPDATE` whose `c` IS
/// the `clientAlgoId` (see the module doc), so the family mapper books it on the stop's own order.
pub fn map_algo_update(frame: &Value) -> Vec<Event> {
    let Some(o) = frame.get("o").or_else(|| frame.get("ao")).filter(|o| o.is_object()) else {
        return Vec::new();
    };
    let coid = strip_broker_coid_prefix(&s(o, "caid")).to_string();
    if coid.is_empty() {
        return Vec::new();
    }
    let ts = i(frame, "T");
    match s(o, "X").as_str() {
        "NEW" => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: o.get("aid").map(json_id).filter(|id| !id.is_empty()).map(Into::into),
            ts,
        })],
        "TRIGGERING" | "TRIGGERED" => {
            vec![Event::OrderTriggered(OrderTriggered { client_order_id: coid, ts })]
        }
        status => {
            let reason = match (status, s(o, "rm")) {
                ("REJECTED", rm) if rm.is_empty() => "algo order REJECTED".to_string(),
                ("REJECTED", rm) => rm,
                _ => String::new(),
            };
            terminal_event(coid, status, reason, ts).into_iter().collect()
        }
    }
}

/// `GET /fapi/v1/allAlgoOrders` rows -> the terminal events of the stops that ended while the stream
/// was down or before their REST ack was folded (the audit-A3 resync, [`crate::exec::perp_history_events`]).
/// `allOrders` does not list an algo order, so this is the only place a cancel / expiry / rejected
/// trigger of an untriggered stop is recorded. Only the TERMINAL rows replay (as
/// `family::history::map_perp_history` does for regular orders): a live or triggered row emits nothing
/// (a triggered stop's fills come from `userTrades`/`allOrders`; whether the child's `allOrders` row
/// carries the `clientAlgoId` as its `clientOrderId`, as its stream frames do, is NOT measured), and a
/// terminal row for a stop the book already closed lands on a terminal order and changes nothing.
/// Total over any body: a non-array or a row without `clientAlgoId` is skipped.
pub fn map_algo_history(rows: &Value) -> Vec<Event> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let coid = strip_broker_coid_prefix(&s(row, "clientAlgoId")).to_string();
            let status = s(row, "algoStatus");
            let reason = match status.as_str() {
                "REJECTED" => "reconcile: algo order rejected during reconnect gap",
                _ => "reconcile: algo order closed during reconnect gap",
            };
            (!coid.is_empty())
                .then(|| terminal_event(coid, &status, reason.to_string(), i(row, "updateTime")))
                .flatten()
        })
        .collect()
}

impl<S: Signer, T: RestTransport> BinancePerpRest<S, T> {
    /// Audit A3 resync: recent algo orders in any state (`GET /fapi/v1/allAlgoOrders`, weight 5, `symbol`
    /// mandatory), on the short-timeout requery transport like `get_all_orders` so the resync cannot
    /// stall. Returns the raw JSON array; [`map_algo_history`] reads it.
    pub fn get_all_algo_orders(&self, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            PATH_ALL_ALGO_ORDERS,
            "GET",
            &params,
            &self.signer,
        )
    }
}

#[path = "algo_tests.rs"]
#[cfg(test)]
mod algo_tests;
