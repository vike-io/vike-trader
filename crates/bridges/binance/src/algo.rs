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
//!
//! ## MEASURED on the DEMO host 2026-10-24 (read-only; `tests/offline/algo_old_stop.rs` replays it)
//!
//! * `allAlgoOrders` filters on a stop's **`updateTime`**, not its `createTime`, answers newest `algoId`
//!   first, and refuses a window over seven days: `{"code":-4165,"msg":"Maximum time interval is 7
//!   days."}`. A triggered stop's `updateTime` is its `triggerTime` plus 17 ms.
//! * The triggered stop's child, read back over REST (`GET /fapi/v1/order?orderId=`), carries
//!   `clientOrderId` equal to the stop's `clientAlgoId`; `GET /fapi/v1/algoOrder?clientAlgoId=` answers
//!   the stop with `actualOrderId` equal to that order. Neither read takes a window.
//!
//! ## The same by-id read names a PLAIN order's fill (spot and perpetual)
//!
//! `userTrades` and spot `myTrades` carry `orderId` and no client order id, so a plain order that filled
//! while the node was down came back as a fill with `coid=None` (MEASURED 2026-10-24, the Binance demo
//! perpetual: four held `MissingFill` rows, told apart only by price). [`name_algo_children`] reads each
//! such fill's order by id (the read a stop's child gets) and, when the stored `clientOrderId` is one of
//! OURS ([`own_coid`]), names the fill with it: on the perpetual when the algo endpoint knows no stop of
//! that id (`-2013`, measured 2026-10-19), on spot always (no Algo Service). A hand-placed order, whose
//! id is not ours, and an order the venue no longer finds, stay unnamed.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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
    FillReport, ManagedOrder, OrderRequest, OrderStatus, OrderStatusReport, OrderType,
    SymbolProperties,
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
                    reason: crate::modes::order_reject_reason(exc.code, &exc.msg, &self.base_url)
                        .into(),
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

// ---- fills of a triggered stop: the child order's id -> the stop's coid -----------------------------

/// `GET /fapi/v1/allAlgoOrders` rows -> `(child order id, bare coid)` of every stop that TRIGGERED:
/// `actualOrderId` (the matching engine's order the trigger placed) and the broker-prefix-stripped
/// `clientAlgoId`. A stop that has not triggered carries no `actualOrderId` (an absent, empty or `0`
/// field) and maps to nothing. The second element is how many rows the body held, so the caller can
/// tell a full page (older stops may exist) from a short one.
pub fn parse_algo_children(body: &str) -> Result<(Vec<(String, String)>, usize), String> {
    let rows = parse_rows(body, |o| {
        let child = o.get("actualOrderId").map(json_id).unwrap_or_default();
        let coid = strip_broker_coid_prefix(&s(o, "clientAlgoId")).to_string();
        (child, coid)
    })?;
    let held = rows.len();
    let named = rows
        .into_iter()
        .filter(|(child, coid)| !child.is_empty() && child != "0" && !coid.is_empty())
        .collect();
    Ok((named, held))
}

/// Give the coid-less fills of `label` the coid of the stop whose child order they belong to: the
/// pure half of [`name_algo_children`]. A fill that already names a coid, a fill of another symbol
/// and a fill of an order that is no stop's child are left as they are.
fn join_algo_children(fills: &mut [FillReport], label: &str, children: &[(String, String)]) {
    for fill in fills
        .iter_mut()
        .filter(|f| f.client_order_id.is_none() && f.symbol.eq_ignore_ascii_case(label))
    {
        if let Some((_, coid)) = children.iter().find(|(child, _)| *child == *fill.venue_order_id) {
            fill.client_order_id = Some(coid.clone());
        }
    }
}

/// Rows one history read asks for: `limit` at the documented maximum, the latest orders (the
/// endpoint returns the latest ones when no `algoId` is sent). There is NO `startTime`/`endTime`: the
/// venue filters on a stop's UPDATE time, answers an unwindowed read with the latest seven days of
/// it, and refuses a window over seven days with `-4165` (all measured on the demo host,
/// 2026-10-24). A stop's update time is its trigger, so one that rested for longer than that and
/// then fired is in the answer for the seven days after the fire; one the answer misses is named by
/// [`read_child`].
const ALGO_HISTORY_LIMIT: usize = 1000;

/// The most symbols one pass reads the history of: the engine's own ceiling (the mounted symbol and
/// every admitted one), so the cost is bounded by construction at one read of weight five per symbol
/// per pass, on the order-gate-EXEMPT requery path (which is why it is bounded here and not by the
/// gate). A symbol past it stays unnamed this pass, with the hourly notice.
const ALGO_HISTORY_MAX_READS: usize = vike_exec::MAX_EXTRA_SYMBOLS + 1;

/// How many passes in a row may fail the fill fetch over an unreadable history: strict, as the
/// `openAlgoOrders` read beside it (a fill list that silently lacked the names would let the restore
/// planner read a filled stop as one the venue dropped). From the next failure on the pass goes ahead
/// with those fills unnamed, so an endpoint that stays broken cannot blind the whole account for good.
const ALGO_NAMING_STRICT_FAILURES: u32 = 3;

/// How often the degraded and capped cases repeat their one plain WARN.
const ALGO_NAMING_NOTICE_EVERY: Duration = Duration::from_secs(3600);

/// The most distinct fill orders one pass asks the venue about BY ID ([`read_child`]: two weight-one
/// reads each on the perpetual, so at most fifty weight a pass; one weight-four read on spot, so at
/// most a hundred; both on the order-gate-EXEMPT requery path, which is why the bound is here and
/// not at the gate). Each answer is remembered, so this only limits the first pass
/// after a start, which meets every coid-less fill of the lookback at once; what is left is asked on
/// the next pass, and a failure counts only while something is left that the history did not name.
pub const ALGO_CHILD_LOOKUPS_PER_PASS: usize = 25;

/// How many by-id answers are remembered before half of them are dropped (one short entry per fill
/// order; a restart empties it anyway).
const ALGO_CHILD_MEMORY: usize = 2_000;

/// The kinds of hourly notice `AlgoNaming` rate-limits, each on its own clock so one cannot hide the
/// other: fills left unnamed (a degraded or capped pass), and a failed history forgiven because every
/// fill order was resolved by id.
const NOTICE_UNNAMED: usize = 0;
const NOTICE_FORGIVEN: usize = 1;

/// How the naming reads have been going across passes (the client lives for the process): consecutive
/// failing passes since the last good read, when the last notice was logged, and the by-id answers.
#[derive(Debug, Default)]
pub struct AlgoNaming(Mutex<NamingState>);

#[derive(Debug, Default)]
struct NamingState {
    failures: u32,
    /// When each kind of hourly notice was last logged ([`NOTICE_UNNAMED`], [`NOTICE_FORGIVEN`]).
    noticed: [Option<Instant>; 2],
    /// `(symbol label, fill order id)` -> the bare coid the order carries (its stop's, or its own),
    /// or `None` when it was asked about and is nobody's we can name.
    children: std::collections::HashMap<(String, String), Option<String>>,
}

impl AlgoNaming {
    fn state(&self) -> std::sync::MutexGuard<'_, NamingState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A pass named what it needed: strictness comes back.
    fn succeeded(&self) {
        self.state().failures = 0;
    }

    /// What an earlier pass learned about fill order `id` of `label` (`None`: never asked).
    fn child(&self, label: &str, id: &str) -> Option<Option<String>> {
        self.state().children.get(&(label.to_string(), id.to_string())).cloned()
    }

    fn remember_child(&self, label: &str, id: &str, coid: Option<String>) {
        let mut st = self.state();
        if st.children.len() >= ALGO_CHILD_MEMORY {
            // evict half (an arbitrary half), not all: the answers just learned must not be asked again
            let mut keep = false;
            st.children.retain(|_, _| {
                keep = !keep;
                keep
            });
        }
        st.children.insert((label.to_string(), id.to_string()), coid);
    }

    /// Is the hourly notice of kind `slot` ([`NOTICE_UNNAMED`], [`NOTICE_FORGIVEN`]) due at `now` (none
    /// of that kind within the last hour)? Taking it marks it logged.
    fn notice_due(&self, slot: usize, now: Instant) -> bool {
        let mut st = self.state();
        let due =
            st.noticed[slot].is_none_or(|at| now.duration_since(at) >= ALGO_NAMING_NOTICE_EVERY);
        if due {
            st.noticed[slot] = Some(now);
        }
        due
    }

    /// A pass's history read failed with `why`. The first [`ALGO_NAMING_STRICT_FAILURES`] consecutive
    /// failures are `Err` (the fill fetch fails). After that the pass goes ahead unnamed: `Ok(true)`
    /// when its WARN is due, `Ok(false)` inside the hour of the last one.
    fn failed(&self, now: Instant, why: String) -> Result<bool, String> {
        {
            let mut st = self.state();
            st.failures = st.failures.saturating_add(1);
            if st.failures <= ALGO_NAMING_STRICT_FAILURES {
                return Err(why);
            }
        }
        Ok(self.notice_due(NOTICE_UNNAMED, now))
    }
}

/// The distinct symbols of the coid-less fills, at most [`ALGO_HISTORY_MAX_READS`] of them, and
/// whether some were left out.
fn history_labels(fills: &[FillReport]) -> (Vec<String>, bool) {
    let mut labels: Vec<String> =
        fills.iter().filter(|f| f.client_order_id.is_none()).map(|f| f.symbol.clone()).collect();
    labels.sort_unstable();
    labels.dedup();
    let capped = labels.len() > ALGO_HISTORY_MAX_READS;
    labels.truncate(ALGO_HISTORY_MAX_READS);
    (labels, capped)
}

/// `GET /fapi/v1/allAlgoOrders` for one symbol: the stops that triggered (child order id -> bare
/// coid) and how many rows the page held.
fn read_history<S: Signer, T: RestTransport>(
    client: &FamilyReconClient<S, T>,
    label: &str,
) -> Result<(Vec<(String, String)>, usize), String> {
    let symbol = vike_catalog::split_perp(label).0.to_string();
    let params = [("symbol", symbol), ("limit", ALGO_HISTORY_LIMIT.to_string())];
    let mut rows = 0usize;
    let mut read = || -> Result<Vec<(String, String)>, ReadFail> {
        let body = client.signed_requery_raw(PATH_ALL_ALGO_ORDERS, &params)?;
        let (children, held) = parse_algo_children(&body)
            .map_err(|msg| ReadFail::Parse(format!("{PATH_ALL_ALGO_ORDERS}: {msg}")))?;
        rows = held;
        Ok(children)
    };
    let children = client.admitted_rows(label, read())?;
    Ok((children, rows))
}

/// Whose order is fill order `id` of `symbol`, by ITS OWN stored id? Two weight-one reads on the
/// perpetual (spot: one, of weight four), with no window to age out of: `GET /fapi/v1/order` (spot
/// `GET /api/v3/order`) by `orderId` gives the order's stored `clientOrderId` (MEASURED on the demo
/// host, over the user stream and over this very read on 2026-10-24: a triggered stop's child
/// carries the stop's `clientAlgoId`; a spelling that differed would make every stop answer here
/// "plain"), and on the perpetual `GET /fapi/v1/algoOrder` by that id says whether such a stop exists
/// and which order it placed (`actualOrderId`). Only an id of OURS ([`own_coid`]) is looked up, and a
/// stop must be on this symbol. The answer is the bare coid (the stop's, or, when the venue knows no
/// stop of that id, or on spot, the PLAIN order's own: the venue stores the id we sent and
/// `userTrades`/`myTrades` carry none), [`Child::No`] when this is nobody's we can name (the order is
/// not found, its id is not ours, or a stop of that id placed another order), or [`Child::Later`].
/// "Unknown order" (`-2011`/`-2013`) from the order read is "no" and from the algo read is "no stop";
/// any other failure is an error, never an answer.
fn read_child<S: Signer, T: RestTransport>(
    client: &FamilyReconClient<S, T>,
    symbol: &str,
    id: &str,
    link_id: Option<&str>,
) -> Result<Vec<Child>, ReadFail> {
    let one = |c: Child| -> Result<Vec<Child>, ReadFail> { Ok(vec![c]) };
    let perp = matches!(client, FamilyReconClient::Perp { .. });
    let path = if perp { crate::perp::PATH_ORDER } else { crate::spot::PATH_ORDER };
    let order = match client
        .signed_requery_raw(path, &[("symbol", symbol.to_string()), ("orderId", id.to_string())])
    {
        Ok(body) => body,
        Err(exc) if is_unknown_order(exc.code) => return one(Child::No),
        Err(exc) => return Err(exc.into()),
    };
    let order: Value =
        serde_json::from_str(&order).map_err(|e| ReadFail::Parse(format!("{path}: {e}")))?;
    let stored = s(&order, "clientOrderId");
    let Some(coid) = own_coid(&stored, link_id) else { return one(Child::No) };
    if !perp {
        // no Algo Service on spot: the order is a plain one of ours
        return one(Child::Of(coid));
    }
    let stop = match client.signed_requery_raw(PATH_ALGO_ORDER, &[("clientAlgoId", stored)]) {
        Ok(body) => body,
        // no stop of that id: a plain order of ours, whose own id names its fill
        Err(exc) if is_unknown_order(exc.code) => return one(Child::Of(coid)),
        Err(exc) => return Err(exc.into()),
    };
    let stop: Value = serde_json::from_str(&stop)
        .map_err(|e| ReadFail::Parse(format!("{PATH_ALGO_ORDER}: {e}")))?;
    if !s(&stop, "symbol").eq_ignore_ascii_case(symbol) {
        return one(Child::No);
    }
    match stop.get("actualOrderId").map(json_id).unwrap_or_default().as_str() {
        placed if placed == id => one(Child::Of(coid)),
        "" | "0" => one(Child::Later),
        _ => one(Child::No),
    }
}

/// The bare coid of `stored`, a client id as the venue keeps it, when it is one of OURS; `None` for
/// anyone else's. Ours is what this client mints: the bare coid (`<session><seq>`, alphanumeric,
/// `is_valid_crypto_coid`) or, when a broker link id is configured, `x-<link id>-<coid>` with THAT
/// link id. An order or stop placed by hand (the web UI, another tool) carries its own id, and
/// naming a fill with a coid this node never minted would hand the reconcile diff a coid-linked fill
/// of an order it does not know (under `hybrid`, worse than an unnamed one). With no link id
/// configured an `x-` id is never ours; an order placed under a link id that is no longer
/// configured stays unnamed (the safe side).
fn own_coid(stored: &str, link_id: Option<&str>) -> Option<String> {
    let bare = match stored.strip_prefix("x-") {
        Some(rest) => rest.strip_prefix(link_id?)?.strip_prefix('-')?,
        None => stored,
    };
    vike_model::is_valid_crypto_coid(bare).then(|| bare.to_string())
}

/// What the venue said about one fill order.
#[derive(Debug, PartialEq)]
enum Child {
    /// The order carries this bare coid of ours: the stop it is the child of, or itself (a plain
    /// order, or any spot order).
    Of(String),
    /// Nobody's we can name: the order is not found, its id is not ours, or it shares that id with a
    /// stop on another symbol or one that placed another order.
    No,
    /// A stop of that id exists on this symbol but does not say yet which order it placed (the algo
    /// record lags the child by a moment): not an answer, so not remembered, asked again next pass.
    Later,
}

/// What one symbol's by-id naming came to.
#[derive(Default)]
struct ByIdOutcome {
    /// The first read that failed (the rest of this symbol was not asked).
    error: Option<String>,
    /// Distinct fill orders left unasked because the pass's budget ran out.
    left: usize,
}

/// Name the coid-less fills of `label` (a stop's child, or a plain order's), by asking the venue about
/// each distinct fill order ONCE ([`read_child`]); the answer, "no" included, is remembered in
/// `naming` ([`Child::Later`] is not an answer and is asked again), so a fill order costs its reads
/// the first time it is seen and none after. `budget` is the pass's remaining reads of this kind,
/// shared by all symbols; `link_id` is the broker link id this client's orders carry ([`own_coid`]).
fn name_by_order_id<S: Signer, T: RestTransport>(
    client: &FamilyReconClient<S, T>,
    naming: &AlgoNaming,
    fills: &mut [FillReport],
    label: &str,
    link_id: Option<&str>,
    budget: &mut usize,
) -> ByIdOutcome {
    let mut ids: Vec<String> = fills
        .iter()
        .filter(|f| {
            f.client_order_id.is_none()
                && f.symbol.eq_ignore_ascii_case(label)
                && !f.venue_order_id.is_empty()
        })
        .map(|f| f.venue_order_id.to_string())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let symbol = vike_catalog::split_perp(label).0.to_string();
    let mut out = ByIdOutcome::default();
    for id in ids {
        let coid = match naming.child(label, &id) {
            Some(known) => known,
            None if *budget == 0 => {
                out.left += 1;
                continue;
            }
            None => {
                *budget -= 1;
                match client.admitted_rows(label, read_child(client, &symbol, &id, link_id)) {
                    Ok(found) => match found.into_iter().next() {
                        Some(Child::Later) => {
                            out.left += 1;
                            continue;
                        }
                        Some(Child::Of(coid)) => {
                            naming.remember_child(label, &id, Some(coid.clone()));
                            Some(coid)
                        }
                        Some(Child::No) | None => {
                            naming.remember_child(label, &id, None);
                            None
                        }
                    },
                    Err(why) => {
                        out.error = Some(why);
                        return out;
                    }
                }
            }
        };
        if let Some(coid) = coid {
            join_algo_children(fills, label, &[(id, coid)]);
        }
    }
    out
}

/// A failed pass, under the policy of [`AlgoNaming::failed`]: `Err` while strict, `Ok` (the fills go
/// on unnamed, one WARN an hour) once the endpoint has stayed broken for a few passes.
fn give_up_or_fail(naming: &AlgoNaming, why: String) -> Result<(), String> {
    match naming.failed(Instant::now(), why.clone()) {
        Err(strict) => Err(strict),
        Ok(due) => {
            if due {
                tracing::warn!(
                    venue = crate::VENUE,
                    "reconcile: the fills of triggered stops and plain orders could not be named \
                     after repeated passes ({why}); they stay unnamed, so a stop that filled while \
                     the node was down may be read as cancelled and a plain order's fill cannot be \
                     joined to its order. This note repeats once an hour"
                );
            }
            Ok(())
        }
    }
}

/// Name the coid-less fills: those of triggered stops, and those of plain orders. `userTrades` and
/// spot `myTrades` report a fill by the order that traded and carry no coid. A triggered stop traded as
/// its CHILD order (`ALGO_UPDATE.ai`): an id that is neither the `algoId` the venue acknowledged nor
/// the coid we sent, so after a restart such a fill would match nothing and the stop it closed would
/// look like one the venue merely stopped listing. A plain order's fill names the order we sent, but
/// nothing joined the two: it came back `coid=None` and the book stayed wrong until an operator
/// confirmed it (the 2026-10-24 measurement). Two sources, the second for what the first cannot see
/// (the perpetual only has the first: spot has no Algo Service):
///
/// 1. `allAlgoOrders` (weight five, `symbol` mandatory): which child belongs to which stop, for the
///    stops its window returns. The window is cut on the stop's UPDATE time (measured), so a stop
///    that fired within the last seven days is returned however long it rested before; a stop whose
///    last update is older is not.
/// 2. the fill's order itself, by id ([`read_child`]): no window at all, so a protective stop that is
///    older than the history's reach and triggered while the node was down is still named, and so is
///    a plain order of ours, by its own stored `clientOrderId`.
///
/// ONE history read per symbol that has a coid-less fill (at most [`ALGO_HISTORY_MAX_READS`] a pass)
/// plus at most [`ALGO_CHILD_LOOKUPS_PER_PASS`] fill orders asked by id, each answer remembered. ⚠ **A
/// failure that leaves a fill possibly unnamed fails the whole fill fetch**, as the `openAlgoOrders`
/// read beside it fails the order report, for the first [`ALGO_NAMING_STRICT_FAILURES`] failing
/// passes since the last good read; after that the fills stay unnamed and one plain WARN an hour says
/// so (`naming` remembers across passes). A failed history read is forgiven when every fill order was
/// resolved by id, with ONE warning an hour that says so: it rests on the child's REST
/// `clientOrderId` being the stop's `clientAlgoId` (measured on the demo host). A symbol the venue no longer
/// lists, and a rate limit, are judged by the family's own rule (`FamilyReconClient::admitted_rows`).
/// A symbol whose by-id read failed does not stop the other symbols' (the first error is kept and the
/// pass fails at the end under the policy above), unless a rate limit paused the pass.
pub fn name_algo_children<S: Signer, T: RestTransport>(
    client: &FamilyReconClient<S, T>,
    naming: &AlgoNaming,
    link_id: Option<&str>,
    fills: &mut [FillReport],
) -> Result<(), String> {
    let (labels, capped) = history_labels(fills);
    if labels.is_empty() {
        // a pass with nothing to name is a good read: "in a row" counts failing passes since the last
        naming.succeeded();
        return Ok(());
    }
    let mut full_pages: Vec<&str> = Vec::new();
    let mut history_error: Option<String> = None;
    let mut by_id_error: Option<String> = None;
    let mut left = 0usize;
    let mut budget = ALGO_CHILD_LOOKUPS_PER_PASS;
    // the Algo Service is the perpetual's: spot has no history to read, only the orders themselves
    let perp = matches!(client, FamilyReconClient::Perp { .. });
    for label in &labels {
        if perp {
            match read_history(client, label) {
                Ok((children, rows)) => {
                    if rows >= ALGO_HISTORY_LIMIT {
                        full_pages.push(label);
                    }
                    join_algo_children(fills, label, &children);
                }
                Err(why) => history_error = history_error.or(Some(why)),
            }
        }
        let by_id = name_by_order_id(client, naming, fills, label, link_id, &mut budget);
        left += by_id.left;
        if let Some(why) = by_id.error {
            by_id_error = by_id_error.or(Some(why));
            // a persistent error must not blind the later symbols, but a rate limit that paused the
            // pass must not be hammered
            if client.not_backing_off().is_err() {
                break;
            }
        }
    }
    let unresolved = left > 0 || capped;
    if let Some(why) = by_id_error.or_else(|| history_error.clone().filter(|_| unresolved)) {
        return give_up_or_fail(naming, why);
    }
    if let Some(why) = history_error
        && naming.notice_due(NOTICE_FORGIVEN, Instant::now())
    {
        tracing::warn!(
            venue = crate::VENUE,
            "reconcile: the algo history could not be read ({why}); every fill order was resolved by \
             id; this relies on the child order's clientOrderId being the stop's clientAlgoId \
             (measured on the demo host). This note repeats once an hour"
        );
    }
    naming.succeeded();
    if (unresolved || !full_pages.is_empty()) && naming.notice_due(NOTICE_UNNAMED, Instant::now()) {
        tracing::warn!(
            venue = crate::VENUE,
            "reconcile: the fills may not all carry their order's client id yet (symbols left \
             out: {capped}; fill orders left for the next pass: {left}; full history pages for \
             {full_pages:?}): an older triggered stop can stay unnamed. This note repeats once an hour"
        );
    }
    Ok(())
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
