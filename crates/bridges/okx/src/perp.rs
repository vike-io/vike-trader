//! OkxPerpRest — signed V5 SWAP submit/cancel/reconcile + set-leverage. Exact port of
//! `exec/okx/perp_client.py` (+ the shared flow and the spot client's unwrap/NOT_FOUND).
//!
//! SWAP deltas: instId `BTC-USDT-SWAP`, tdMode honors `OrderRequest.margin_mode` via
//! [`swap_td_mode`] (unset = the historical `"cross"` byte-for-byte; `Isolated` sends
//! `"isolated"`; `Cash` is DENIED — spot-only on OKX), `ordType` carries the requested TIF
//! (tif step-2 FLIPPED: unset/GTC → `"limit"` byte-identical, Ioc/Fok → the `ioc`/`fok`
//! ordTypes, GTD/Day loud-denied at submit via `deny_unsupported_tif`), `sz` in CONTRACTS
//! (`to_contracts` — one of the pinned Decimal wire sites, see
//! `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES`: base/ctVal floored to lotSz,
//! FRACTIONAL contracts allowed), posSide=net, reduceOnly bool, NO tgtCcy; set-leverage
//! re-raises on ANY non-'0' code (OKX is idempotent — no benign code confirmed);
//! /api/v5/account/positions reconcile where `pos` is ALREADY SIGNED contracts.

use indexmap::IndexMap;
use rust_decimal::prelude::*;
use serde_json::{Value, json};

use vike_bridge_core::confirm::ConfirmByCoid;
use vike_bridge_core::rest::VenueRest;
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{
    ManagedOrder, MarginMode, OrderRequest, OrderStatus, OrderType, SymbolProperties,
};

use crate::error_codes::{modify_failed, submit_outcome_unknown};
use crate::transport::{OkxTransport, unwrap_okx};
use vike_bridge_core::format::{format_to_step_f, py_f64_str};
use vike_bridge_core::json::{json_num, json_text};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;

mod batch;
mod reconcile;
mod stop_algo;

/// Canonical venue id — the okx `venue_tif` row key (tif step-2).
pub const VENUE: &str = "okx";

pub const PATH_ORDER_CREATE: &str = "/api/v5/trade/order";
pub const PATH_ORDER_CANCEL: &str = "/api/v5/trade/cancel-order";
pub const PATH_ORDER_AMEND: &str = "/api/v5/trade/amend-order";
pub const PATH_BATCH_ORDERS: &str = "/api/v5/trade/batch-orders";
pub const PATH_CANCEL_BATCH: &str = "/api/v5/trade/cancel-batch-orders";
pub const PATH_ORDER_ALGO: &str = "/api/v5/trade/order-algo";
/// Cancel untriggered algo orders — the ONLY cancel of a stop; its body is a JSON ARRAY (≤ 10).
pub const PATH_CANCEL_ALGOS: &str = "/api/v5/trade/cancel-algos";
/// Untriggered algo orders — `orders-pending` is not documented to list them; `ordType` is required.
pub const PATH_ORDERS_ALGO_PENDING: &str = "/api/v5/trade/orders-algo-pending";
/// Recent orders + fills — used by the audit-A3 history replay.
pub const PATH_ORDERS_HISTORY: &str = "/api/v5/trade/orders-history";
pub const PATH_FILLS_HISTORY: &str = "/api/v5/trade/fills-history";
/// OKX caps batch-orders / cancel-batch-orders at 20 items per request.
pub const BATCH_MAX: usize = 20;
pub const PATH_POSITIONS: &str = "/api/v5/account/positions";
/// Live (resting) orders — the reconcile open-order fetch.
pub const PATH_ORDERS_PENDING: &str = "/api/v5/trade/orders-pending";
pub const PATH_SET_LEVERAGE: &str = "/api/v5/account/set-leverage";
pub const PATH_ACCOUNT: &str = "/api/v5/account/balance";
pub const PATH_TICKER: &str = "/api/v5/market/ticker";
pub const PATH_INSTRUMENTS: &str = "/api/v5/public/instruments";
pub const PATH_BILLS: &str = "/api/v5/account/bills";
/// PUBLIC (keyless) server clock. The startup preflight's clock leg reads it through
/// `crate::adapter`'s `OkxVenueAdapter::server_time_ms`: okx stamps `OK-ACCESS-TIMESTAMP` on every
/// private request and rejects one outside its window with `50102` ([`crate::error_codes`]).
///
/// ⚠ The stamp is `data[0].ts` — epoch ms as a **STRING inside an ARRAY** — so `as_i64()` reads
/// `None` and a correct parse needs `as_str()` + `parse::<i64>()` plus an empty-`data` guard.
/// Measured from the CI box on 2026-08-08: `{"code":"0","data":[{"ts":"1786218814806"}],"msg":""}`,
/// byte-identical in shape with and without the `x-simulated-trading: 1` header (demo and mainnet
/// share [`REST`], so both tiers read one host).
pub const PATH_TIME: &str = "/api/v5/public/time";

pub const REST: &str = "https://www.okx.com"; // demo + mainnet share the host
pub const DEMO_WS: &str = "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999";
pub const MAINNET_WS: &str = "wss://ws.okx.com:8443/ws/v5/private";

/// Whether the REST transport must send the `x-simulated-trading: 1` header — `true` for demo,
/// `false` for mainnet. It is the exact inverse of whether the start resolved the LIVE (mainnet)
/// tier: `UreqOkxTransport::new(true)` (its historical, hardcoded value) is precisely
/// `simulated(false)`, so a demo-ceiling start is byte-identical to before this existed.
///
/// ⚠ **The account's tier IS the network for this venue** (decision 0095, as amended) —
/// `crate::adapter`'s `OkxVenueAdapter::start` resolves `mainnet` ONCE per start from the tier
/// (`AdapterInputs::tier`, the account row's `tier` as `vike-tradehub-core` folds it — never an
/// `OKX_MAINNET` flag any more), and the resolved `bool` is then threaded into every adapter site
/// that calls this (and [`ws_url`]) — the grid pre-fetch, the exec spawn, the funding poller, the
/// recon client — instead of each spawned thread reading anything for itself. OKX is unusual —
/// demo and mainnet SHARE the REST host ([`REST`]); demo is selected by this header, so switching
/// to mainnet means DROPPING it AND using the mainnet private WS ([`MAINNET_WS`] instead of
/// [`DEMO_WS`]).
pub fn simulated(mainnet: bool) -> bool {
    !mainnet
}

/// The PRIVATE user-data WS URL, selected by whether the start resolved the LIVE tier.
/// **Default-safe:** `false` ⇒ the exact demo WS OKX has always used; `true` ⇒ the mainnet private
/// WS.
pub fn ws_url(mainnet: bool) -> &'static str {
    if mainnet { MAINNET_WS } else { DEMO_WS }
}

#[path = "endpoint_switch_tests.rs"]
#[cfg(test)]
mod endpoint_switch_tests;

/// cancel: filled / already-canceled / does-not-exist family — the REGULAR book's "unknown" answer.
/// A resting STOP (`order-algo`) is not in that book, so this answer is about the wrong endpoint and
/// proves nothing about a stop: `cancel_order` retries it on the algo endpoint
/// (`stop_algo::cancel_algo`) before it reads the order as gone, and the cancel batch does the same
/// per row.
///
/// **The set is exactly the words `cancel_order` swallowed before stops were cancelled on
/// `cancel-algos`, so every OTHER regular answer (`51603`, `51413`, `51403`, ...) stays the `Err` it
/// always was.** Measured 2026-10-23 on the demo account (`tests/okx_algo_stop_smoke.rs`'s
/// `MEASURED regular cancel-order` line): `cancel-order` of a STOP's coid answers `51400`, and the
/// stop survives it. To add a code, add exactly the one a printed line shows, with a row in
/// `tests/offline/algo_stop.rs`'s pin tests.
const NOT_FOUND: [i64; 3] = [51400, 51401, 51402];

/// Map the request's margin mode → the OKX SWAP `tdMode` wire value — the margin-mode STEP-2
/// un-hardcode for the `SwitchMechanism::PerOrderField` okx row of
/// `vike_venues::venue_margin_support` (the capability-map playbook; TIF-playbook shape).
///
/// - `None` (unset) → `"cross"` — EXACTLY the pre-flip hardcode, so the default path stays
///   byte-identical (pinned in `r6_okx_parity.rs`).
/// - `Some(Cross)` → `"cross"`; `Some(Isolated)` → `"isolated"` (per-order, no out-of-band
///   switch — OKX's `tdMode` IS the switch mechanism).
/// - `Some(Cash)` → `Err`: `tdMode:"cash"` is spot/non-margin only and this adapter trades
///   SWAP, so the caller synthesizes a terminal `OrderRejected` from the reason (deny loudly,
///   never silently coerce — the roster-gate philosophy).
pub fn swap_td_mode(mode: Option<MarginMode>) -> Result<&'static str, String> {
    match mode {
        None | Some(MarginMode::Cross) => Ok("cross"),
        Some(MarginMode::Isolated) => Ok("isolated"),
        Some(MarginMode::Cash) => Err(
            "okx: margin_mode=Cash denied — tdMode \"cash\" is spot-only, this adapter trades SWAP (cross/isolated)"
                .to_string(),
        ),
    }
}

/// Twin of `parse_okx_perp_instruments`: lotSz/tickSz/minSz/maxMktSz in CONTRACTS,
/// ctVal (base per contract) + ctMult, base asset from ctValCcy. No SWAP min_notional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OkxInstrument {
    pub properties: SymbolProperties,
    pub base_asset: String,
    pub ct_val: f64,
    pub ct_mult: f64,
    /// The venue's published account-leverage CEILING for this instrument — the top-level `lever`
    /// field, a STRING (`"100"` for BTC-USDT-SWAP, verified live 2026-08-05). OKX documents it as
    /// "Max Leverage", and it is absent on SPOT/OPTION — hence `Option`, not this struct's
    /// absent-is-`0.0` sibling convention: `0.0` would read as "no leverage permitted" where the
    /// truth is "the venue said nothing".
    ///
    /// Deliberately NOT a [`SymbolProperties`] field, following `vike_hyperliquid`'s explicit
    /// precedent ("`max_leverage` is NOT a `SymbolProperties` field — it stays on the
    /// `InstrumentRef`"): that struct is persisted in the `kind=properties` PIT series, so a field
    /// there costs a Parquet codec-column decision, and no rounding site wants a leverage. Its ONE
    /// consumer is `vike_bridge_core::leverage::clamp_to_venue_cap`, at the exec thread's startup
    /// `set_leverage`.
    pub max_leverage: Option<f64>,
    /// The row's `ctType` word (`"linear"` or `"inverse"`), verbatim; `None` when the row states
    /// none. Read by the exec thread's INVERSE refusal ([`crate::exec::inverse_swap_refusal`]): an
    /// inverse swap counts a contract in USD (`ctVal` 100 USD on `BTC-USD-SWAP`), and
    /// [`OkxPerpRest::to_contracts`] divides a BASE quantity by it.
    pub contract_type: Option<String>,
}

/// The exec plane's per-instId map over a `public/instruments` payload. The grid (and its class,
/// from the row's own `instType`) is `crate::catalog::row_grid` — the SAME row read the catalog's
/// `parse_insts` uses, so identity is decided once (decision 0126).
///
/// ⚠ **This parser does not only ever see SWAP rows, which is why the class is read per ROW.**
/// [`crate::exec::fetch_okx_instrument_row`] asks `instType=SWAP`, but the function is a pure
/// parse over whatever `data[]` it is handed, and OKX's four instTypes share one row shape. Unlike
/// the catalog it keeps a row whose `Contract` cannot be built (no `ctType`, no `instType`): the
/// exec plane needs only the grid and `ct_val`, and its offline pins feed such rows.
pub fn parse_okx_perp_instruments(payload: &Value) -> IndexMap<String, OkxInstrument> {
    let mut out = IndexMap::new();
    for entry in payload.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        let inst_id = entry.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if inst_id.is_empty() {
            continue;
        }
        let f = |key: &str| -> f64 { entry.get(key).and_then(json_num).unwrap_or(0.0) };
        out.insert(
            inst_id,
            OkxInstrument {
                // `contract_size` stays absent here: OKX sizes in CONTRACTS and this map carries
                // its own contracts→base factor (`ct_val`, applied at `to_contracts`), deliberately
                // NOT duplicated (absent = multiplier 1.0) — see `row_grid`'s doc.
                properties: crate::catalog::row_grid(entry),
                base_asset: entry
                    .get("ctValCcy")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
                ct_val: f("ctVal"),
                ct_mult: f("ctMult"),
                // The account-leverage ceiling. NOT routed through `f` (the absent-is-0.0 closure
                // every grid field above uses): the ONE consumer
                // (`vike_bridge_core::leverage::clamp_to_venue_cap`) must distinguish "unknown,
                // change nothing" from a real ceiling, and `0.0` cannot express the former.
                max_leverage: entry.get("lever").and_then(json_num),
                contract_type: entry.get("ctType").and_then(|v| v.as_str()).map(str::to_string),
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

pub struct OkxPerpRest<T: OkxTransport> {
    pub signer: OkxV5Signer,
    pub transport: T,
    pub base_url: String,
    /// instId form: BTC-USDT-SWAP
    pub symbol: String,
    pub properties: SymbolProperties,
    pub ct_val: f64,
    pub leverage: f64,
    /// OKX Fully-Disclosed-Broker attribution code, resolved ONCE at start from
    /// `OKX_BROKER_CODE`/`OKX_BUILDER_CODE` via `attribution_code_from` (see
    /// `vike_secrets::credentials`). `None` (unset, or fails the venue's
    /// `AttributionMechanic` validation) ⇒ `build_order_params` emits no `tag` key —
    /// byte-identical to before this field existed.
    pub broker_code: Option<String>,
}

/// ⚠ **The same grid, restated in BASE units** — for any consumer that judges a `qty` in base
/// rather than one that is about to hit the wire.
///
/// `parse_okx_perp_instruments` fills `step_size`/`min_qty`/`max_qty` from OKX's `lotSz`/
/// `minSz`/`maxMktSz`, which are counts of CONTRACTS. [`OkxPerpRest::to_contracts`] is the reason that
/// is right: it takes a BASE qty, divides by `ct_val`, and floors on that contracts grid. Every
/// wire-side consumer wants exactly those raw numbers.
///
/// But `OrderRequest.qty` is BASE, and so is everything the pre-trade gate reasons about. A
/// consumer that hands the raw grid to `vike_model::RiskLimits::from_properties` is asking the
/// gate to floor a base quantity on a contracts step — for BTC-USDT-SWAP (`ct_val` 0.01) that
/// grid is 100x too coarse, so a legitimate 0.015 BTC order floors to 0.01 and a size the venue
/// would accept is refused outright. That is the bug this exists to stop, and it is spelled
/// HERE, once, next to `to_contracts`, rather than re-derived at each start.
///
/// ⚠ Only the QUANTITY fields scale. `tick_size` is quote-per-base and `min_notional` is quote
/// — both are already in the units their consumers expect, and multiplying either by `ct_val`
/// would be a second bug of the same kind in the opposite direction.
///
/// A non-finite or non-positive `ct_val` returns the properties unchanged: the caller then has
/// today's behaviour rather than a grid silently zeroed or made infinite.
pub fn properties_in_base(properties: &SymbolProperties, ct_val: f64) -> SymbolProperties {
    if !ct_val.is_finite() || ct_val <= 0.0 {
        return *properties;
    }
    SymbolProperties {
        step_size: properties.step_size * ct_val,
        min_qty: properties.min_qty * ct_val,
        max_qty: properties.max_qty * ct_val,
        ..*properties
    }
}

impl<T: OkxTransport> OkxPerpRest<T> {
    fn call(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let resp = self.transport.signed(&self.base_url, path, method, params, &self.signer)?;
        unwrap_okx(resp)
    }

    /// Signed GET returning the unwrapped `data` list — the funding poller's seam.
    pub fn call_get(&self, path: &str, params: &[(&str, Value)]) -> Result<Value, VenueApiError> {
        self.call(path, "GET", params)
    }

    /// Audit A3 resync: recent orders (`GET /api/v5/trade/orders-history`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the unwrapped `data` list.
    pub fn get_orders_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("instType", json!("SWAP")),
            ("instId", json!(self.symbol)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_ORDERS_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// Audit A3 resync: recent fills (`GET /api/v5/trade/fills-history`) for the history replay.
    pub fn get_fills_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("instType", json!("SWAP")),
            ("instId", json!(self.symbol)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_FILLS_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// Audit T1: re-query one order by clOrdId (= our idempotent client_order_id) on the
    /// short-timeout requery transport. `Ok(Some(id))` = live, `Ok(None)` = venue confirms absent
    /// (top-level code 51603 / NOT_FOUND family, or empty data), `Err` = query failed (an optimistic
    /// accept on the submit-timeout path, silence on the active confirm). The two-endpoint form below
    /// is the confirm's; the submit-timeout path uses the regular-book half alone.
    ///
    /// **"Absent" needs BOTH endpoints:** the regular book first (the ordinary order's one call), then
    /// the algo endpoint — a resting stop is not in the regular book, so the regular `51603` alone would
    /// reject a live stop. A FAILED algo query is `Err` (says nothing), never "absent".
    ///
    /// ⚠ This is the ACTIVE CONFIRM's lookup (`ConfirmByCoid::confirm_when`) ONLY. The submit-timeout
    /// path ([`Self::resolve_ambiguous_submit`]) asks [`Self::query_regular_ordid`] alone: an ordinary
    /// submit can never be a stop (`submit_order` hands stops to `submit_stop_algo`, which never
    /// resolves), and a failed algo query there would turn the venue's clean "absent" into an id-less
    /// accept of an order that never reached OKX.
    fn query_order_ordid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        match self.query_regular_ordid(coid)? {
            Some(id) => Ok(Some(id)),
            None => self.lookup_algo(coid),
        }
    }

    /// The regular-book half of [`Self::query_order_ordid`] (`GET /trade/order` by `clOrdId`).
    fn query_regular_ordid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        let params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(coid))];
        let resp = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_CREATE,
            "GET",
            &params,
            &self.signer,
        );
        match resp.and_then(unwrap_okx) {
            Ok(data) => Ok(data
                .as_array()
                .and_then(|d| d.first())
                .and_then(|d0| d0.get("ordId"))
                .map(json_text)),
            Err(exc) if exc.code == 51603 || NOT_FOUND.contains(&exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1). Regular book only,
    /// as it always was: see [`Self::query_order_ordid`] for why the algo endpoint is not asked here.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_regular_ordid(&request.client_order_id),
        )
    }

    /// base qty → CONTRACTS, floored to the lotSz step. One of the pinned Decimal wire sites
    /// (plan §f64-policy; `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES`
    /// is the list). FRACTIONAL contracts are legal (lotSz e.g. 0.01) — never
    /// round to a whole contract or sub-0.5-contract orders floor to 0.
    pub fn to_contracts(&self, base_qty: f64) -> f64 {
        let (Some(raw), Some(ct), Some(step)) = (
            Decimal::from_str(&py_f64_str(base_qty)).ok(),
            Decimal::from_str(&py_f64_str(self.ct_val)).ok(),
            Decimal::from_str(&py_f64_str(self.properties.step_size)).ok(),
        ) else {
            return 0.0;
        };
        if ct.is_zero() || step.is_zero() {
            return 0.0;
        }
        // Python: (raw / ct) // step * step — Decimal // truncates toward zero
        let contracts = (raw / ct / step).trunc() * step;
        contracts.to_f64().unwrap_or(0.0)
    }

    /// signed contracts → base (plain float multiply, matching Python `_to_base`).
    pub fn to_base(&self, contracts: f64) -> f64 {
        contracts * self.ct_val
    }

    /// POST set-leverage (mgnMode=cross). Re-raise on ANY non-'0' code — OKX repeat
    /// calls return '0' (idempotent); no benign code is swallowed without demo proof.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        let lev = if self.leverage == (self.leverage as i64) as f64 {
            format!("{}", self.leverage as i64)
        } else {
            py_f64_str(self.leverage)
        };
        let params: Vec<(&str, Value)> = vec![
            ("instId", json!(self.symbol)),
            ("lever", json!(lev)),
            ("mgnMode", json!("cross")),
        ];
        self.call(PATH_SET_LEVERAGE, "POST", &params).map(|_| ())
    }

    /// SWAP order: sz in CONTRACTS, tdMode from [`swap_td_mode`] (unset = the historical
    /// `"cross"`), posSide=net, reduceOnly, NO tgtCcy. `Err` = the requested margin mode cannot
    /// ride the wire (Cash on SWAP) — the caller MUST surface it as a terminal `OrderRejected`.
    ///
    /// tif step-2 (FLIPPED): OKX has no TIF field — the TIF rides `ordType`, and the limit path
    /// consumes okx's `vike_venues::venue_tif::venue_tif` row: Gtc stays NotEmitted
    /// (`ordType:"limit"`, the venue default good-till-cancel rules — byte-identical to
    /// pre-flip, pinned in `r6_okx_parity.rs`), Ioc/Fok map to the `ioc`/`fok` ordTypes (still
    /// limit orders: px required and sent). Gtd/Day are Unsupported and gated at submit
    /// (`deny_unsupported_tif`) — if one reaches this builder anyway, `ordType:"limit"` is
    /// emitted (the venue default), the gate being the loud guard. Market orders keep today's
    /// `ordType:"market"` (the table's resting-path scope; stops route to the algo endpoint).
    pub fn build_order_params(
        &self,
        request: &OrderRequest,
        broker_code: Option<&str>,
    ) -> Result<Vec<(&'static str, Value)>, String> {
        let td_mode = swap_td_mode(request.margin_mode)?;
        let is_limit = request.order_type == OrderType::Limit;
        let ord_type = if is_limit {
            match vike_venues::venue_tif::venue_tif(VENUE, request.time_in_force) {
                vike_venues::venue_tif::TifOutcome::Mapped(w)
                | vike_venues::venue_tif::TifOutcome::Coerced { wire: w, .. } => w,
                vike_venues::venue_tif::TifOutcome::Ignored { .. }
                | vike_venues::venue_tif::TifOutcome::NotEmitted
                | vike_venues::venue_tif::TifOutcome::Unsupported => "limit",
            }
        } else {
            "market"
        };
        let mut params: Vec<(&'static str, Value)> = vec![
            ("instId", json!(self.symbol)),
            ("tdMode", json!(td_mode)),
            ("side", json!(if request.side > 0 { "buy" } else { "sell" })),
            ("ordType", json!(ord_type)),
            (
                "sz",
                json!(format_to_step_f(self.to_contracts(request.qty), self.properties.step_size)),
            ),
            ("clOrdId", json!(request.client_order_id)),
            ("posSide", json!("net")),
            ("reduceOnly", json!(request.reduce_only)),
        ];
        if is_limit {
            params.push((
                "px",
                json!(format_to_step_f(request.price.unwrap_or(0.0), self.properties.tick_size)),
            ));
        }
        // Fee-attribution FD-broker code (unified cross-venue attribution, task 4): a configured
        // code stamps the wire `tag`; absent ⇒ no key at all, byte-identical to before this param
        // existed.
        if let Some(tag) = broker_code {
            params.push(("tag", json!(tag)));
        }
        Ok(params)
    }

    /// Native amend (POST /api/v5/trade/amend-order): re-price and/or re-size a resting order in
    /// place. `newSz` is CONTRACTS floored to lotSz (the same pinned Decimal site as submit),
    /// `newPx` tick-rounded. Returns `[OrderModified]` on the venue's success ack; on failure the
    /// order keeps its old terms and a non-terminal `OrderModifyRejected` (venue reason) is returned. RUST-NATIVE, no Python twin.
    pub fn modify_order(
        &self,
        client_order_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let mut params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(client_order_id))];
        if let Some(q) = new_qty {
            params.push((
                "newSz",
                json!(format_to_step_f(self.to_contracts(q), self.properties.step_size)),
            ));
        }
        if let Some(p) = new_price {
            params.push(("newPx", json!(format_to_step_f(p, self.properties.tick_size))));
        }
        match self.call(PATH_ORDER_AMEND, "POST", &params) {
            Ok(data) => vec![Event::OrderModified(OrderModified {
                client_order_id: client_order_id.to_string(),
                venue_order_id: data
                    .as_array()
                    .and_then(|d| d.first())
                    .and_then(|d0| d0.get("ordId"))
                    .map(json_text)
                    .map(Into::into),
                new_qty,
                new_price,
                ts: 0,
            })],
            // modify failed at the venue — the resting order is unchanged, but the refusal must not
            // vanish: a NON-terminal advisory carrying the venue's reason (as binance perp).
            Err(exc) => vec![modify_failed(client_order_id, &exc)],
        }
    }
}

/// Map ONE `/api/v5/trade/orders-pending` row → a reconcile-seeded [`ManagedOrder`] (status
/// `ACCEPTED` — the venue's resting truth). Pure + fixture-gated, the perp twin of the spot
/// open-order mapping. `clOrdId` is our idempotent client id; side buy⇒+1 / sell⇒−1; `sz` is in
/// CONTRACTS so it is converted to base (`contracts * ct_val`, the reconcile twin of `to_base`) to
/// match the submitted qty; `order_type` is the venue `ordType` lowercased; unpriced/market rows
/// carry `price: None` (OKX sends "" for `px` there).
pub fn map_okx_open_order(o: &Value, ct_val: f64) -> ManagedOrder {
    let side = if o.get("side").and_then(|s| s.as_str()) == Some("buy") { 1 } else { -1 };
    let contracts = o.get("sz").and_then(json_num).unwrap_or(0.0);
    // a non-finite wire size, or an overflowing `sz * ct_val`, would serialize as JSON null and fail
    // the typed decode below
    let qty = contracts * ct_val;
    let qty = if qty.is_finite() { qty } else { 0.0 };
    let price = o.get("px").and_then(|p| p.as_str()).and_then(|p| {
        if matches!(p, "" | "0" | "0.00000000") { None } else { p.parse::<f64>().ok() }
    });
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": o.get("clOrdId").and_then(|c| c.as_str()).unwrap_or(""),
        "venue": "okx",
        "symbol": o.get("instId").and_then(|s| s.as_str()).unwrap_or(""),
        "side": side,
        "qty": qty,
        "order_type": o.get("ordType").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
        "price": price,
    }))
    .expect("static shape");
    let mut mo = ManagedOrder::new(request);
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = o.get("ordId").and_then(|v| v.as_str()).map(str::to_string);
    mo
}

/// The active confirm over the by-clOrdId re-query ([`OkxPerpRest::query_order_ordid`]), in the shared
/// form of `vike_bridge_core::confirm`: found -> `OrderAccepted` with the venue id, `51603` (absent) ->
/// the "venue confirms absent" reject only for a coid the exec thread finished dispatching, a FAILED
/// query -> nothing.
impl<T: OkxTransport + Send> ConfirmByCoid for OkxPerpRest<T> {
    // None: okx documents no age limit on `GET /trade/order` that was measured here.
    const LOOKUP_WINDOW: Option<std::time::Duration> = None;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        vike_bridge_core::confirm::resolve_confirm_by_id(
            coid,
            ts,
            self.query_order_ordid(coid),
            absent_rejects,
        )
    }
}

impl<T: OkxTransport + Send> VenueRest for OkxPerpRest<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        // OKX stops are ALGO orders on a different endpoint — route them there
        if request.order_type == OrderType::Stop {
            return self.submit_stop_algo(request);
        }
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF okx cannot express (GTD/Day — no such ordType) is a LOUD
        // terminal reject; the wire is never touched, never a silent limit-GTC (pinned in
        // `tests/offline/tif_gate.rs`).
        if let Some(reject) = vike_venues::venue_tif::deny_unsupported_tif(VENUE, request) {
            events.push(reject);
            return events;
        }
        // Margin-mode gate: an unsupported requested mode never reaches the wire — it comes back
        // as a synthesized terminal OrderRejected (deny loudly, never silently coerce).
        let params = match self.build_order_params(request, self.broker_code.as_deref()) {
            Ok(p) => p,
            Err(reason) => {
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: reason.into(),
                    ts: request.ts,
                }));
                return events;
            }
        };
        match self.call(PATH_ORDER_CREATE, "POST", &params) {
            Ok(data) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                // data[0].ordId (the unwrapped list)
                venue_order_id: Some(
                    data.as_array()
                        .and_then(|d| d.first())
                        .and_then(|d0| d0.get("ordId"))
                        .map(json_text)
                        .unwrap_or_default()
                        .into(),
                ),
                ts: request.ts,
            })),
            // audit T1: an ambiguous timeout may have been accepted — re-query, don't false-reject.
            Err(exc) if submit_outcome_unknown(exc.code) => {
                events.push(self.resolve_ambiguous_submit(request));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_okx::perp", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(client_order_id))];
        match self.call(PATH_ORDER_CANCEL, "POST", &params) {
            Ok(_) => Ok(()),
            // not in the regular book: maybe a resting STOP (an algo order, which this endpoint does
            // not cancel) — one retry on `cancel-algos`, whose own "unknown" is the idempotent "gone"
            // once the algo book confirms it
            Err(exc) if NOT_FOUND.contains(&exc.code) => {
                tracing::debug!(target: "vike_okx::perp", client_order_id, "cancel: unknown on cancel-order, trying cancel-algos");
                self.cancel_algo(client_order_id)
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

    /// Native OKX amend — overrides the no-op default with `/api/v5/trade/amend-order`. OKX
    /// identifies the order by `clOrdId`, so only the coid is needed from the resting `order`.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        OkxPerpRest::modify_order(self, &order.client_order_id, new_qty, new_price)
    }

    /// Native OKX batch-submit — overrides the fan-out default with `/api/v5/trade/batch-orders`.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        OkxPerpRest::submit_batch(self, requests)
    }

    /// Native OKX batch-cancel — overrides the fan-out default with `/api/v5/trade/cancel-batch-orders`.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        OkxPerpRest::cancel_batch(self, client_order_ids)
    }
}

#[path = "margin_mode_tests.rs"]
#[cfg(test)]
mod margin_mode_tests;

#[path = "properties_unit_tests.rs"]
#[cfg(test)]
mod properties_unit_tests;
