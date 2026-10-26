//! BybitPerpRest — signed V5 LINEAR perp submit/cancel/reconcile + set-leverage. Exact
//! port of `exec/bybit/perp_client.py` (+ the shared crypto_client flow + spot unwrap).
//!
//! Linear deltas: category=linear, positionIdx=0 (one-way; hedge 1/2 is 5f), reduceOnly
//! as a JSON BOOL, qtyStep filters, set-leverage swallowing 110043 ("leverage not
//! modified"), and /v5/position/list signed reconcile — size is UNSIGNED, the SIGN comes
//! from side Buy/Sell; hedge legs from positionIdx 1/2.

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::confirm::ConfirmByCoid;
use vike_bridge_core::rest::VenueRest;
use vike_exec::ReconcileSnapshot;
use vike_model::events::{
    Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted, PositionSide,
};
use vike_model::{Contract, ManagedOrder, OrderRequest, OrderStatus, OrderType, SymbolProperties};

use crate::catalog::{derivative_contract, derivative_properties};
use crate::error_codes::{modify_failed, submit_outcome_unknown};
use crate::transport::{BybitTransport, unwrap_envelope};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::{json_num, json_text};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;

/// Canonical venue id — the bybit `venue_tif` row key (tif step-2).
pub const VENUE: &str = "bybit";

pub const PATH_ORDER_CREATE: &str = "/v5/order/create";
/// Single-order status query (open + recently-closed) — used by the audit-T1 requery.
pub const PATH_ORDER_REALTIME: &str = "/v5/order/realtime";
/// Recent CLOSED orders + executions — used by the audit-A3 history replay.
pub const PATH_ORDER_HISTORY: &str = "/v5/order/history";
pub const PATH_EXECUTION_LIST: &str = "/v5/execution/list";
pub const PATH_ORDER_CANCEL: &str = "/v5/order/cancel";
pub const PATH_ORDER_AMEND: &str = "/v5/order/amend";
pub const PATH_ORDER_CREATE_BATCH: &str = "/v5/order/create-batch";
pub const PATH_ORDER_CANCEL_BATCH: &str = "/v5/order/cancel-batch";
/// Bybit v5 linear batch cap: 20 orders per create-batch / cancel-batch request.
pub const BATCH_MAX: usize = 20;
pub const PATH_POSITION_LIST: &str = "/v5/position/list";
pub const PATH_SET_LEVERAGE: &str = "/v5/position/set-leverage";
pub const PATH_ACCOUNT: &str = "/v5/account/wallet-balance";
pub const PATH_INSTRUMENTS: &str = "/v5/market/instruments-info";
pub const PATH_TICKERS: &str = "/v5/market/tickers";
/// PUBLIC (keyless) server clock. The startup preflight's clock leg reads it through
/// `crate::adapter`'s `BybitVenueAdapter::server_time_ms` (the venue-adapter contract's clock read,
/// docs/decisions/0096): bybit stamps `X-BAPI-TIMESTAMP` on every private request
/// and rejects one outside [`BybitV5Signer`]'s `recv_window` with `10002`
/// ([`crate::error_codes`]), so this endpoint answers "would our signed orders be rejected now".
///
/// ⚠ The stamp is the v5 envelope's TOP-LEVEL `"time"` — a JSON NUMBER in epoch ms.
/// `result.timeNano` is a nanosecond STRING and `result.timeSecond` a seconds STRING, so a naive
/// `as_i64()` on either reads the wrong unit or nothing. Measured from the CI box on 2026-08-08, both
/// hosts: `{"retCode":0,…,"result":{"timeSecond":"1786218814","timeNano":"1786218814374677738"},
/// "retExtInfo":{},"time":1786218814374}`.
pub const PATH_TIME: &str = "/v5/market/time";

pub const DEMO_REST: &str = "https://api-demo.bybit.com";
pub const MAINNET_REST: &str = "https://api.bybit.com";
pub const DEMO_WS: &str = "wss://stream-demo.bybit.com/v5/private";
pub const MAINNET_WS: &str = "wss://stream.bybit.com/v5/private";

/// The `(rest_base, ws_base)` pair this venue's private exec/user-data path binds to, selected by
/// whether the start resolved the LIVE (mainnet) tier. **Default-safe:** `false` ⇒ the exact demo
/// hosts bybit has always used; `true` ⇒ mainnet (real funds). Bybit has no
/// `x-simulated-trading`-style header — demo is a wholly separate host — so switching the host is
/// the entire change.
///
/// ⚠ **The account's tier IS the network for this venue** (decision 0095, as amended) —
/// `crate::adapter`'s `BybitVenueAdapter::start` takes `mainnet` from the tier
/// (`AdapterInputs::tier`, which `vike-tradehub-core` resolves from the account row's `tier` —
/// never a `BYBIT_MAINNET` flag any more) ONCE
/// per start; the resolved `bool` is then threaded into every adapter site that needs it (the grid
/// pre-fetch, the exec spawn, the funding poller, the recon client) instead of each spawned thread
/// reading anything for itself, and the start's clock read takes the same verdict.
pub fn endpoints(mainnet: bool) -> (&'static str, &'static str) {
    if mainnet { (MAINNET_REST, MAINNET_WS) } else { (DEMO_REST, DEMO_WS) }
}

#[path = "endpoint_switch_tests.rs"]
#[cfg(test)]
mod endpoint_switch_tests;

/// order-not-exists / too-late-to-cancel — swallowed by cancel (unknown ≠ rejection)
const NOT_FOUND: [i64; 2] = [110001, 170213];
const LEVERAGE_NOT_MODIFIED: i64 = 110043; // already at target — benign

/// One row of [`parse_bybit_perp_instruments`]: the grid and the typed contract
/// (`crates/bridges/bybit/src/catalog.rs`'s `derivative_properties`/`derivative_contract`, the same
/// two calls the picker's rows are built from), plus base_asset from baseCoin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BybitInstrument {
    pub properties: SymbolProperties,
    /// **What this row IS**, read off its own fields — margining from `contractType`, the settle
    /// asset from `settleCoin`, a dated future's expiry from `deliveryTime`. `None` when the row
    /// names no contract this crate can type (a spot row, an unknown `contractType`, no
    /// `settleCoin`). Decided by `crates/bridges/bybit/src/catalog.rs`'s `derivative_contract`,
    /// which the catalog's picker rows call too, so the two planes cannot disagree about it.
    pub contract: Option<Contract>,
    pub base_asset: String,
    /// The venue's published account-leverage CEILING for this symbol —
    /// `leverageFilter.maxLeverage`, a decimal STRING (`"100.00"` for BTCUSDT, verified live
    /// 2026-08-05). `None` when the row carries no `leverageFilter` or an unparseable value.
    ///
    /// Deliberately NOT a [`SymbolProperties`] field, following `vike_hyperliquid`'s explicit
    /// precedent ("`max_leverage` is NOT a `SymbolProperties` field — it stays on the
    /// `InstrumentRef`"): `SymbolProperties` is persisted in the `kind=properties` PIT series, so
    /// a field there costs a Parquet codec-column decision, and nothing downstream ROUNDS with a
    /// leverage. Its ONE consumer is `vike_bridge_core::leverage::clamp_to_venue_cap`, at the
    /// exec thread's startup `set_leverage`.
    ///
    /// The sibling `leverageFilter.minLeverage` (`"1"`) and `leverageStep` (`"0.01"`) are
    /// deliberately NOT parsed: nothing clamps upward or snaps a leverage to a step today, and an
    /// unused parsed field reads as a supported capability. Add them with their consumer.
    pub max_leverage: Option<f64>,
    /// **The venue's own `contractType` for this row, verbatim** — `"LinearPerpetual"`,
    /// `"InversePerpetual"`, `"LinearFutures"`, `"InverseFutures"`. `None` when the row carries
    /// none (a spot row, or a shape the venue changed).
    ///
    /// ⚠ **It is parsed because the exec plane needs it to REFUSE, not because anything routes on
    /// it.** MEASURED 2026-09-16: `instruments-info?category=linear&symbol=XRPUSD` ANSWERS, with
    /// `contractType: "InversePerpetual"` and `settleCoin: "XRP"` — so the exec grid fetch's
    /// `category: "linear"` literal does not fail loudly on an inverse symbol, it succeeds and
    /// returns a coin-settled grid (`qtyStep=1`, `minOrderQty=1` — USD contracts) that
    /// `SymbolProperties::multiplier()` then folds at 1.0 because `contract_size` stays unset. Every
    /// notional the RiskGate computes is wrong by roughly the price. `crates/bridges/bybit/src/
    /// exec.rs`'s `non_linear_perpetual_refusal` is the consumer, and it exists so that arm stops
    /// being the one that does not fail loudly.
    pub contract_type: Option<String>,
}

/// Every row of one `instruments-info` `{result:{list}}` page, keyed by its upper-cased symbol —
/// the exec plane's grid map. Unlike the catalog it gates on NOTHING (no `status`, no
/// `contractType`): the exec thread asks for one symbol and must see whatever the venue answered,
/// which is what lets [`crate::exec::non_linear_perpetual_refusal`] read an `InversePerpetual` row
/// the `category: "linear"` request was answered with.
///
/// The grid and the [`BybitInstrument::contract`] are `crates/bridges/bybit/src/catalog.rs`'s
/// `derivative_properties`/`derivative_contract` — the class of the grid is the contract's
/// (`Contract::class`), so the stored class, the typed contract and the picker row are one
/// decision. Its doc records why the margining is the ROW's `contractType`, never the request's
/// `category`.
pub fn parse_bybit_perp_instruments(payload: &Value) -> IndexMap<String, BybitInstrument> {
    let mut out = IndexMap::new();
    let rows = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array());
    for entry in rows.unwrap_or(&vec![]) {
        let symbol = entry.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let contract = derivative_contract(entry);
        out.insert(
            symbol,
            BybitInstrument {
                properties: derivative_properties(entry, contract.as_ref()),
                contract,
                base_asset: entry
                    .get("baseCoin")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
                // The account-leverage ceiling. `Option`, NOT the `num` closure's absent-is-0.0
                // convention: `0.0` here would be indistinguishable from "the venue says you may
                // not use leverage at all", and the ONE consumer
                // (`vike_bridge_core::leverage::clamp_to_venue_cap`) must be able to tell
                // "unknown, change nothing" from a real ceiling.
                max_leverage: entry
                    .get("leverageFilter")
                    .and_then(|d| d.get("maxLeverage"))
                    .and_then(json_num),
                // Carried, never interpreted here. Its one consumer is the exec thread's refusal.
                contract_type: entry
                    .get("contractType")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }
    out
}

#[path = "leverage_cap_tests.rs"]
#[cfg(test)]
mod leverage_cap_tests;

#[path = "asset_class_tests.rs"]
#[cfg(test)]
mod asset_class_tests;

pub struct BybitPerpRest<T: BybitTransport> {
    pub signer: BybitV5Signer,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
}

impl<T: BybitTransport> BybitPerpRest<T> {
    fn call(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let resp = self.transport.signed(&self.base_url, path, method, params, &self.signer)?;
        unwrap_envelope(resp)
    }

    /// Audit A3 resync: recent CLOSED orders (`GET /v5/order/history`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the raw `result.list` array.
    pub fn get_order_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("limit", json!(limit)),
        ];
        let resp = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_HISTORY,
            "GET",
            &params,
            &self.signer,
        )?;
        Ok(unwrap_envelope(resp)?.get("list").cloned().unwrap_or(json!([])))
    }

    /// Audit A3 resync: recent executions (`GET /v5/execution/list`) for the history replay.
    pub fn get_execution_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("limit", json!(limit)),
        ];
        let resp = self.transport.signed_requery(
            &self.base_url,
            PATH_EXECUTION_LIST,
            "GET",
            &params,
            &self.signer,
        )?;
        Ok(unwrap_envelope(resp)?.get("list").cloned().unwrap_or(json!([])))
    }

    /// Audit T1: re-query one order by orderLinkId (= our idempotent client_order_id) on the
    /// short-timeout requery transport, to resolve an ambiguous submit into a NON-phantom outcome.
    /// TWO-STAGE, because `/v5/order/realtime` only retains OPEN + recently-closed orders: a submit
    /// that FILLED-AND-CLOSED just outside that retention window returns EMPTY there, so concluding
    /// "absent" from realtime alone would phantom-reject a REAL live position. We therefore FALL BACK
    /// to `/v5/order/history` (the CLOSED/filled endpoint) on an empty realtime, and only report
    /// absent when BOTH are empty. `Ok(Some(id))` = venue HAS the order (live, or filled/closed) →
    /// managed OrderAccepted (its fill, if any, follows on the user-data / A3 lane); `Ok(None)` =
    /// both endpoints confirm absent → the true terminal reject (the active confirm: only for a dispatched coid);
    /// `Err` = the query itself failed → an optimistic accept on the submit-timeout path, silence on the
    /// active confirm; never a false terminal. The history fallback rides the same
    /// gate-exempt short-timeout requery transport and only ever fires on the rare ambiguous submit.
    fn query_order_orderlinkid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        match self.query_order_status(PATH_ORDER_REALTIME, coid)? {
            Some(id) => Ok(Some(id)),
            // realtime is an OPEN-orders view — empty ≠ absent for a filled-and-closed order that
            // dropped off its retention window. Confirm against history before concluding absent.
            None => self.query_order_status(PATH_ORDER_HISTORY, coid),
        }
    }

    /// Signed by-orderLinkId single-order lookup against a status endpoint (`/v5/order/realtime` OR
    /// `/v5/order/history`), on the short-timeout requery transport. `Ok(Some(id))` = the first
    /// matching row's venue `orderId`; `Ok(None)` = an empty `result.list` (Bybit returns no
    /// not-found error code for a by-id miss); `Err` = the re-query itself failed. Both endpoints
    /// share the `{category, symbol, orderLinkId}` request and the `result.list` reply shape.
    fn query_order_status(&self, path: &str, coid: &str) -> Result<Option<String>, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(coid)),
        ];
        let resp =
            self.transport.signed_requery(&self.base_url, path, "GET", &params, &self.signer)?;
        let result = unwrap_envelope(resp)?;
        match result.get("list").and_then(|l| l.as_array()).and_then(|l| l.first()) {
            Some(o) => Ok(Some(match o.get("orderId") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            })),
            None => Ok(None),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1).
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_order_orderlinkid(&request.client_order_id),
        )
    }

    /// POST set-leverage (one-way: buy == sell). Swallows 110043 (already at target).
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        // Python str(self._leverage) — a float formats as "2.0"
        let lev = vike_bridge_core::format::py_f64_str(self.leverage);
        let params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("buyLeverage", json!(lev)),
            ("sellLeverage", json!(lev)),
        ];
        match self.call(PATH_SET_LEVERAGE, "POST", &params) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == LEVERAGE_NOT_MODIFIED => Ok(()),
            Err(exc) => Err(exc),
        }
    }

    /// Pure, golden-gated: Buy/Sell + Limit/Market casing, orderLinkId as the client id,
    /// positionIdx 0 (int), reduceOnly BOOL.
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, Value)> {
        let is_limit = request.order_type == OrderType::Limit;
        let mut params: Vec<(&'static str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("side", json!(if request.side > 0 { "Buy" } else { "Sell" })),
            ("orderType", json!(if is_limit { "Limit" } else { "Market" })),
            ("qty", json!(format_to_step_f(request.qty, self.properties.step_size))),
            ("orderLinkId", json!(request.client_order_id)),
            ("positionIdx", json!(0)),
            ("reduceOnly", json!(request.reduce_only)),
        ];
        if is_limit {
            // tif step-2 (FLIPPED): the Limit path consumes bybit's `venue_tif` row —
            // GTC/IOC/FOK map 1:1 (a default/GTC request emits the same timeInForce=GTC bytes
            // as the pre-flip hardcode, pinned in `r6_bybit_parity.rs`). An Unsupported TIF
            // (GTD/Day, gated at submit by `deny_unsupported_tif`) that reaches this builder
            // anyway emits NO timeInForce — the venue's server default rules, and the submit
            // gate is the loud guard.
            match vike_venues::venue_tif::venue_tif(VENUE, request.time_in_force) {
                vike_venues::venue_tif::TifOutcome::Mapped(w)
                | vike_venues::venue_tif::TifOutcome::Coerced { wire: w, .. } => {
                    params.push(("timeInForce", json!(w)));
                }
                vike_venues::venue_tif::TifOutcome::Ignored { wire } => {
                    params.push(("timeInForce", json!(wire)));
                }
                vike_venues::venue_tif::TifOutcome::NotEmitted
                | vike_venues::venue_tif::TifOutcome::Unsupported => {}
            }
            params.push((
                "price",
                json!(format_to_step_f(request.price.unwrap_or(0.0), self.properties.tick_size)),
            ));
        }
        // `order_type:"stop"` (bracket SL) → a native conditional (trigger) order on the SAME create
        // endpoint: a Market that fires at triggerPrice. Direction from the exit side — a BUY stop
        // (closing a short) fires as price RISES (1); a SELL stop (closing a long) as it FALLS (2).
        // Additive: the golden limit/market cases never carry "stop", so their bytes are unchanged.
        if request.order_type == OrderType::Stop {
            params.push((
                "triggerPrice",
                json!(format_to_step_f(
                    request.trigger_price.unwrap_or(0.0),
                    self.properties.tick_size
                )),
            ));
            params.push(("triggerDirection", json!(if request.side > 0 { 1 } else { 2 })));
            // Trigger-source law: a requested `trigger_by` consumes bybit's
            // `vike_bridge_core::trigger::venue_trigger_by` row — V5 `triggerBy` expresses all
            // three sources, so nothing is ever denied here. `None` keeps the historical
            // "LastPrice" hardcode byte-identically (the venue's own default is also last, but
            // the explicit param is the pinned pre-field wire shape — never dropped).
            let trigger_by = request
                .trigger_by
                .and_then(|s| vike_bridge_core::trigger::venue_trigger_by(VENUE, s).wire())
                .unwrap_or("LastPrice");
            params.push(("triggerBy", json!(trigger_by)));
        }
        params
    }

    /// Native amend (POST /v5/order/amend): re-price and/or re-size a resting order in place.
    /// `qty`/`price` are step/tick-formatted like submit; the client id is `orderLinkId`. Returns
    /// `[OrderModified]` on the venue's ack; a non-terminal `OrderModifyRejected` (venue reason) on failure (order keeps its terms). RUST-NATIVE,
    /// no Python twin.
    pub fn modify_order(
        &self,
        client_order_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let mut params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(client_order_id)),
        ];
        if let Some(q) = new_qty {
            params.push(("qty", json!(format_to_step_f(q, self.properties.step_size))));
        }
        if let Some(p) = new_price {
            params.push(("price", json!(format_to_step_f(p, self.properties.tick_size))));
        }
        match self.call(PATH_ORDER_AMEND, "POST", &params) {
            Ok(result) => vec![Event::OrderModified(OrderModified {
                client_order_id: client_order_id.to_string(),
                venue_order_id: result
                    .get("orderId")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .map(Into::into),
                new_qty,
                new_price,
                ts: 0,
            })],
            // A refused amend keeps the order's terms, but the refusal must not vanish: a
            // NON-terminal advisory carrying the venue's reason (same shape as binance/aster perp).
            Err(exc) => vec![modify_failed(client_order_id, &exc)],
        }
    }

    /// Native batch-submit (POST /v5/order/create-batch). The body is `{category, request:[…]}` —
    /// an object wrapping the order array, so it rides the flat param map (no raw-body signer
    /// needed). Emits OrderSubmitted per order, then per-order Accepted|Rejected: the top-level
    /// `retCode` gates the whole batch; individual outcomes come from `retExtInfo.list[i].code`
    /// (0 = ok, orderId from `result.list[i]`), matched by request order. Chunks at BATCH_MAX.
    /// RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::with_capacity(requests.len() * 2);
        for r in requests {
            events.push(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: r.client_order_id.clone(),
                ts: r.ts,
            }));
        }
        // tif step-2 gate, per order: a TIF bybit cannot express (GTD/Day) is a LOUD terminal
        // reject and its order never enters a wire chunk; the supported rest batch as before.
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match vike_venues::venue_tif::deny_unsupported_tif(VENUE, r) {
                Some(reject) => events.push(reject),
                None => sendable.push(r),
            }
        }
        for chunk in sendable.chunks(BATCH_MAX) {
            let order_objs: Vec<Value> = chunk
                .iter()
                .map(|r| {
                    Value::Object(
                        self.build_order_params(r)
                            .into_iter()
                            // `category` is the top-level batch field, not a per-order one
                            .filter(|(k, _)| *k != "category")
                            .map(|(k, v)| (k.to_string(), v))
                            .collect(),
                    )
                })
                .collect();
            let params: Vec<(&str, Value)> =
                vec![("category", json!("linear")), ("request", Value::Array(order_objs))];
            // raw envelope (not `call`): per-order status lives in retExtInfo, a sibling of result
            match self.transport.signed(
                &self.base_url,
                PATH_ORDER_CREATE_BATCH,
                "POST",
                &params,
                &self.signer,
            ) {
                Ok(envelope) => {
                    let ret_code = envelope.get("retCode").and_then(|c| c.as_i64()).unwrap_or(0);
                    if ret_code != 0 {
                        // HTTP 200 + a status-unknown retCode: the batch may have landed — re-query each.
                        if submit_outcome_unknown(ret_code) {
                            for r in chunk {
                                events.push(self.resolve_ambiguous_submit(r));
                            }
                            continue;
                        }
                        let msg = envelope
                            .get("retMsg")
                            .and_then(|m| m.as_str())
                            .unwrap_or("batch reject")
                            .to_string();
                        for r in chunk {
                            events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: crate::modes::order_reject_reason(
                                    ret_code,
                                    &msg,
                                    &self.base_url,
                                    &self.symbol,
                                )
                                .into(),
                                ts: r.ts,
                            }));
                        }
                        continue;
                    }
                    let result_list = envelope
                        .get("result")
                        .and_then(|r| r.get("list"))
                        .and_then(|l| l.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ext_list = envelope
                        .get("retExtInfo")
                        .and_then(|r| r.get("list"))
                        .and_then(|l| l.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for (i, r) in chunk.iter().enumerate() {
                        let ext_code =
                            ext_list.get(i).and_then(|e| e.get("code")).and_then(|c| c.as_i64());
                        if ext_code.unwrap_or(0) == 0 {
                            events.push(Event::OrderAccepted(OrderAccepted {
                                client_order_id: r.client_order_id.clone(),
                                venue_order_id: result_list
                                    .get(i)
                                    .and_then(|o| o.get("orderId"))
                                    .and_then(|o| o.as_str())
                                    .map(Into::into),
                                ts: r.ts,
                            }));
                        } else if submit_outcome_unknown(ext_code.unwrap_or(0)) {
                            events.push(self.resolve_ambiguous_submit(r));
                        } else {
                            events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: crate::modes::order_reject_reason(
                                    ext_code.unwrap_or(0),
                                    ext_list
                                        .get(i)
                                        .and_then(|e| e.get("msg"))
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("batch order reject"),
                                    &self.base_url,
                                    &self.symbol,
                                )
                                .into(),
                                ts: r.ts,
                            }));
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if submit_outcome_unknown(exc.code) => {
                    for r in chunk {
                        events.push(self.resolve_ambiguous_submit(r));
                    }
                }
                Err(exc) => {
                    for r in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            client_order_id: r.client_order_id.clone(),
                            reason: crate::modes::order_reject_reason(
                                exc.code,
                                &exc.msg,
                                &self.base_url,
                                &self.symbol,
                            )
                            .into(),
                            ts: r.ts,
                        }));
                    }
                }
            }
        }
        events
    }

    /// Native batch-cancel (POST /v5/order/cancel-batch). Per-order "already gone" is non-fatal
    /// (retCode 0 = the batch was processed; the authoritative OrderCanceled arrives on the WS);
    /// a whole-request/network failure returns the first error. Chunks at BATCH_MAX. RUST-NATIVE.
    pub fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in client_order_ids.chunks(BATCH_MAX) {
            let order_objs: Vec<Value> =
                chunk.iter().map(|c| json!({ "symbol": self.symbol, "orderLinkId": c })).collect();
            let params: Vec<(&str, Value)> =
                vec![("category", json!("linear")), ("request", Value::Array(order_objs))];
            if let Err(e) = self.call(PATH_ORDER_CANCEL_BATCH, "POST", &params) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// UNIFIED walletBalance → USDT total. Default-safe: any failure → 0.0.
    fn fetch_usdt_balance(&self) -> f64 {
        let Ok(result) = self.call(PATH_ACCOUNT, "GET", &[("accountType", json!("UNIFIED"))])
        else {
            return 0.0;
        };
        for acct in result.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]) {
            for coin in acct.get("coin").and_then(|c| c.as_array()).unwrap_or(&vec![]) {
                if coin.get("coin").and_then(|c| c.as_str()) == Some("USDT") {
                    return coin.get("walletBalance").and_then(json_num).unwrap_or(0.0);
                }
            }
        }
        0.0
    }

    /// Signed GET /v5/order/realtime {category:linear, symbol} → the venue's resting orders as
    /// reconcile-seeded [`ManagedOrder`]s. `/v5/order/realtime` is Bybit's OPEN-orders view (the same
    /// endpoint the audit-T1 requery reads). Perps previously returned an EMPTY `open_orders`, so
    /// `apply_snapshot`'s stale-order reap could never fire on a perp; this closes that. Best-effort
    /// by contract: a REST hiccup returns `Vec::new()` (skip the order-fetch, KEEP the position
    /// snapshot) rather than failing the whole reconcile — the "REST hiccup ⇒ skip tick" convention.
    /// Reconcile path only, already off the fold.
    fn fetch_open_orders(&self) -> Vec<ManagedOrder> {
        let Ok(result) = self.call(
            PATH_ORDER_REALTIME,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        ) else {
            return Vec::new();
        };
        result
            .get("list")
            .and_then(|l| l.as_array())
            .map(|rows| rows.iter().map(map_bybit_open_order).collect())
            .unwrap_or_default()
    }

    /// GET /v5/position/list: size is UNSIGNED — sign from side Buy/Sell; hedge legs from
    /// positionIdx 1 (LONG) / 2 (SHORT); one-way idx 0 → BOTH. Flat → one zero BOTH row.
    pub fn reconcile_positions(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let result = self.call(
            PATH_POSITION_LIST,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        let bal = self.fetch_usdt_balance();
        // Best-effort resting-order fetch (empty on a REST hiccup) — feeds apply_snapshot's
        // stale-order reap. Fetched once; moved into whichever return path fires.
        let open_orders = self.fetch_open_orders();
        // (signed, avg, mark, side, reported margin mode)
        let mut legs: Vec<(f64, f64, f64, PositionSide, vike_model::MarginMode)> = Vec::new();
        for p in result.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]) {
            let size = p.get("size").and_then(json_num).unwrap_or(0.0).abs();
            if size == 0.0 {
                continue;
            }
            let side = crate::event_mapper::pside_from_idx(p);
            let sign =
                if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
            legs.push((
                sign * size,
                p.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                p.get("markPrice").and_then(json_num).unwrap_or(0.0),
                side,
                // Step-2 (read-side only): `tradeMode` off the SAME row — the shared read
                // `recon_client::parse_trade_mode` fixture-tests. No isolated-wallet carrier
                // (no verified bybit field — see that fn's doc).
                crate::recon_client::parse_trade_mode(p),
            ));
        }
        if legs.is_empty() {
            // Flat: no margin info reported — empty `position_margin` = carry priors forward.
            return Ok(ReconcileSnapshot {
                positions: vec![(self.symbol.clone(), 0.0)],
                open_orders,
                position_avg_px: vec![(self.symbol.clone(), 0.0)],
                position_mark_px: vec![(self.symbol.clone(), 0.0)],
                position_sides: Vec::new(),
                balance: bal,
                position_margin: Vec::new(),
            });
        }
        let hedge = legs.iter().any(|(_, _, _, sd, _)| *sd != PositionSide::Both);
        Ok(ReconcileSnapshot {
            positions: legs.iter().map(|(q, ..)| (self.symbol.clone(), *q)).collect(),
            open_orders,
            position_avg_px: legs.iter().map(|(_, a, ..)| (self.symbol.clone(), *a)).collect(),
            position_mark_px: legs.iter().map(|(_, _, m, ..)| (self.symbol.clone(), *m)).collect(),
            position_sides: if hedge {
                legs.iter().map(|(_, _, _, sd, _)| (self.symbol.clone(), *sd)).collect()
            } else {
                Vec::new()
            },
            balance: bal,
            position_margin: legs
                .iter()
                .map(|(.., mode)| (self.symbol.clone(), *mode, None))
                .collect(),
        })
    }

    /// Signed GET returning the unwrapped `result` — the seam the funding poller and
    /// smoke probes reuse.
    pub fn call_public(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        self.call(path, "GET", params)
    }

    /// Last price from /v5/market/tickers (signed transport works for public GETs too).
    pub fn last_price(&self) -> Result<f64, VenueApiError> {
        let result = self.call(
            PATH_TICKERS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        let lst = result.get("list").and_then(|l| l.as_array());
        Ok(lst
            .and_then(|l| l.first())
            .and_then(|t| t.get("lastPrice"))
            .and_then(json_num)
            .unwrap_or(0.0))
    }
}

/// Map ONE `/v5/order/realtime` row → a reconcile-seeded [`ManagedOrder`] (status `ACCEPTED` —
/// the venue's resting truth). Pure + fixture-gated, the perp twin of the spot open-order mapping.
/// `orderLinkId` is our idempotent client id; side Buy⇒+1 / Sell⇒−1; `order_type` is the venue
/// `orderType` lowercased; unpriced/market rows carry `price: None` (Bybit sends "0"/"" there).
pub fn map_bybit_open_order(o: &Value) -> ManagedOrder {
    let side = if o.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1 } else { -1 };
    let qty = o.get("qty").and_then(json_num).unwrap_or(0.0);
    // a non-finite wire qty ("NaN", "1e999") would serialize as JSON null and fail the typed decode below
    let qty = if qty.is_finite() { qty } else { 0.0 };
    let price = o.get("price").and_then(|p| p.as_str()).and_then(|p| {
        if matches!(p, "" | "0" | "0.00000000") { None } else { p.parse::<f64>().ok() }
    });
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": o.get("orderLinkId").and_then(|c| c.as_str()).unwrap_or(""),
        "venue": "bybit",
        "symbol": o.get("symbol").and_then(|s| s.as_str()).unwrap_or(""),
        "side": side,
        "qty": qty,
        "order_type": o.get("orderType").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
        "price": price,
    }))
    .expect("static shape");
    let mut mo = ManagedOrder::new(request);
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = o.get("orderId").and_then(|v| v.as_str()).map(str::to_string);
    mo
}

/// The active confirm over the by-orderLinkId re-query ([`BybitPerpRest::query_order_orderlinkid`]:
/// realtime, then history), in the shared form of `vike_bridge_core::confirm`: found -> `OrderAccepted`
/// with the venue id, absent on BOTH endpoints -> the "venue confirms absent" reject only for a coid
/// the exec thread finished dispatching, a FAILED query -> nothing.
impl<T: BybitTransport + Send> ConfirmByCoid for BybitPerpRest<T> {
    // None: bybit documents no age limit on `/v5/order/history` that was measured here.
    const LOOKUP_WINDOW: Option<std::time::Duration> = None;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        vike_bridge_core::confirm::resolve_confirm_by_id(
            coid,
            ts,
            self.query_order_orderlinkid(coid),
            absent_rejects,
        )
    }
}

impl<T: BybitTransport + Send> VenueRest for BybitPerpRest<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF bybit cannot express (GTD/Day) is a LOUD terminal reject —
        // the wire is never touched, never a silent GTC (pinned in `tests/offline/tif_gate.rs`).
        if let Some(reject) = vike_venues::venue_tif::deny_unsupported_tif(VENUE, request) {
            events.push(reject);
            return events;
        }
        let params = self.build_order_params(request);
        match self.call(PATH_ORDER_CREATE, "POST", &params) {
            Ok(result) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some(
                    result.get("orderId").map(json_text).unwrap_or_default().into(),
                ),
                ts: request.ts,
            })),
            // audit T1: an ambiguous timeout may have been accepted — re-query, don't false-reject.
            Err(exc) if submit_outcome_unknown(exc.code) => {
                events.push(self.resolve_ambiguous_submit(request));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_bybit::perp", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: crate::modes::order_reject_reason(
                        exc.code,
                        &exc.msg,
                        &self.base_url,
                        &self.symbol,
                    )
                    .into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!("linear")),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(client_order_id)),
        ];
        match self.call(PATH_ORDER_CANCEL, "POST", &params) {
            Ok(_) => Ok(()),
            Err(exc) if NOT_FOUND.contains(&exc.code) => {
                tracing::debug!(target: "vike_bybit::perp", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// ACTIVELY confirm one order's status, in the shared form of `vike_bridge_core::confirm`: this
    /// trait door assumes the submit was dispatched ([`ConfirmByCoid::confirm_when`] with `true`); the
    /// exec client's confirm closure (`crate::exec`) goes through `confirm_when` with the ledger's
    /// real answer. Runs on the ExecActor's own confirm worker.
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(client_order_id, ts, true)
    }

    /// Native Bybit amend — overrides the no-op default with `/v5/order/amend`. Bybit identifies the
    /// order by `orderLinkId`, so only the coid is needed from the resting `order`.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        BybitPerpRest::modify_order(self, &order.client_order_id, new_qty, new_price)
    }

    /// Native Bybit batch-submit — overrides the fan-out default with `/v5/order/create-batch`.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        BybitPerpRest::submit_batch(self, requests)
    }

    /// Native Bybit batch-cancel — overrides the fan-out default with `/v5/order/cancel-batch`.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        BybitPerpRest::cancel_batch(self, client_order_ids)
    }
}
