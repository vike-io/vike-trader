//! VenueRestOkx — signed V5 SWAP submit/cancel + set-leverage. Exact port of
//! `exec/okx/perp_client.py` (+ the shared flow and the spot client's unwrap).
//!
//! SWAP deltas: instId `BTC-USDT-SWAP`, tdMode honors `OrderRequest.margin_mode` via
//! [`swap_td_mode`] (unset = the historical `"cross"` byte-for-byte; `Isolated` sends
//! `"isolated"`; `Cash` is DENIED — spot-only on OKX), `ordType` carries the requested TIF
//! (tif step-2 FLIPPED: unset/GTC → `"limit"` byte-identical, Ioc/Fok → the `ioc`/`fok`
//! ordTypes, GTD/Day loud-denied at submit via `deny_unsupported_time_in_force`), `sz` in CONTRACTS
//! (`to_contracts` — one of the pinned Decimal wire sites, see
//! `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES`: base/ctVal floored to lotSz,
//! FRACTIONAL contracts allowed), posSide=net, reduceOnly bool, NO tgtCcy; set-leverage
//! re-raises on ANY non-'0' code (OKX is idempotent — no benign code confirmed).
//! Positions, open orders and balance are read by `crate::reconcile`'s `ReconcileClientOkx`.

use indexmap::IndexMap;
use rust_decimal::prelude::*;
use serde_json::{Value, json};

use vike_bridge_core::confirm::{ConfirmByOrderClientId, found_by_state, resolve_confirm};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{MarginMode, OrderRequest, OrderType, SymbolProperties};

use crate::execution::event_mapper::{map_okx_algo_order, map_okx_order};
use crate::instrument::{InstrumentType, SpotMode};
use crate::signer::OkxV5Signer;
use crate::transport::client::unwrap_okx;
use crate::transport::error_codes::{
    TAXONOMY, cancel_says_gone, modify_failed, server_had_trouble,
};
use vike_bridge_core::format::{format_to_step_f, py_f64_str};
use vike_bridge_core::json::{json_number, json_text};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::feed::tick::{PATH_WEBSOCKET_BUSINESS, PRODUCT_BUSINESS};

/// The id `key` of a venue row, or `None` when it names none (absent, `null` or empty).
fn row_id(row: &Value, key: &str) -> Option<String> {
    row.get(key).filter(|id| !id.is_null()).map(json_text).filter(|id| !id.is_empty())
}

/// [`row_id`] of an unwrapped `data` list's first row; `None` for no row too.
fn first_row_id(data: &Value, key: &str) -> Option<String> {
    row_id(data.as_array()?.first()?, key)
}

mod batch;
mod class;
mod stop_algo;

pub mod per_symbol;

/// Canonical venue id — the okx `venue_time_in_force` row key (tif step-2).
pub const VENUE: &str = "okx";

/// The code of a read refused because its instId names no class (OKX-1): local, no venue code, so its kind is
/// `ErrorKind::Unknown`, a refusal and never a retry.
pub const E_UNCLASSED_INSTRUMENT: i64 = -9_002;

/// The `instType` word of `instrument`'s class for a read that names one, or the refusal in words: an id of no known
/// form is never read as a SWAP (OKX-1), and nothing is sent.
fn read_inst_type(instrument: &str) -> Result<&'static str, VenueApiError> {
    InstrumentType::of_inst_id_or_refusal(instrument)
        .map(InstrumentType::inst_type)
        .map_err(|refusal| VenueApiError { code: E_UNCLASSED_INSTRUMENT, msg: refusal.to_string() })
}

/// Does okx's capability row list `trailing`? The ONE gate of every trailing-only request this crate makes
/// (the reconcile's trailing reads, the business socket's `algo-advance` subscription, the start probe's
/// trailing read): those requests are unmeasured on the venue, so a box whose row is closed (until the
/// flip after the demo smoke) sends exactly the requests it sent before the trailing order existed.
#[must_use]
pub fn trailing_listed() -> bool {
    vike_venues::venue_caps::capabilities_for(VENUE).supported_order_kinds.contains(&"trailing")
}

pub const PATH_ORDER_CREATE: &str = "/api/v5/trade/order";
pub const PATH_ORDER_CANCEL: &str = "/api/v5/trade/cancel-order";
pub const PATH_ORDER_AMEND: &str = "/api/v5/trade/amend-order";
pub const PATH_BATCH_ORDERS: &str = "/api/v5/trade/batch-orders";
pub const PATH_ORDER_ALGO: &str = "/api/v5/trade/order-algo";
/// Cancel untriggered algo orders — the ONLY cancel of a stop; its body is a JSON ARRAY (≤ 10).
pub const PATH_CANCEL_ALGOS: &str = "/api/v5/trade/cancel-algos";
/// Amend an untriggered stop or trigger algo order — the ONLY amend of a stop, a stop-limit or a
/// take-profit (`amend-order` never reaches one); a JSON OBJECT body, by `algoClOrdId`.
pub const PATH_AMEND_ALGOS: &str = "/api/v5/trade/amend-algos";
/// Untriggered algo orders — `orders-pending` is not documented to list them; `ordType` is required.
pub const PATH_ORDERS_ALGO_PENDING: &str = "/api/v5/trade/orders-algo-pending";
/// Finished algo orders (`state` = `effective` | `canceled` | `order_failed` is required; `ordType` too):
/// the stop's own record, whose `ordIdList` names the order it spawned when it fired.
pub const PATH_ORDERS_ALGO_HISTORY: &str = "/api/v5/trade/orders-algo-history";
/// Recent orders + fills — used by the audit-A3 history replay.
pub const PATH_ORDERS_HISTORY: &str = "/api/v5/trade/orders-history";
pub const PATH_FILLS_HISTORY: &str = "/api/v5/trade/fills-history";
/// OKX caps batch-orders at 20 items per request.
pub const BATCH_MAX: usize = 20;
pub const PATH_POSITIONS: &str = "/api/v5/account/positions";
/// Live (resting) orders.
pub const PATH_ORDERS_PENDING: &str = "/api/v5/trade/orders-pending";
pub const PATH_SET_LEVERAGE: &str = "/api/v5/account/set-leverage";
pub const PATH_ACCOUNT: &str = "/api/v5/account/balance";
pub const PATH_TICKER: &str = "/api/v5/market/ticker";
pub const PATH_INSTRUMENTS: &str = "/api/v5/public/instruments";
pub const PATH_BILLS: &str = "/api/v5/account/bills";
/// PUBLIC (keyless) server clock. The startup preflight's clock leg reads it through
/// `crate::adapter`'s `VenueAdapterOkx::server_time_ms`: okx stamps `OK-ACCESS-TIMESTAMP` on every
/// private request and rejects one outside its window with `50102` ([`crate::transport::error_codes`]).
///
/// ⚠ The stamp is `data[0].ts` — epoch ms as a **STRING inside an ARRAY** — so `as_i64()` reads
/// `None` and a correct parse needs `as_str()` + `parse::<i64>()` plus an empty-`data` guard.
/// Measured from the CI box on 2026-08-08: `{"code":"0","data":[{"ts":"1786218814806"}],"msg":""}`,
/// byte-identical in shape with and without the `x-simulated-trading: 1` header (demo and mainnet
/// share one REST host, [`OkxNet::rest`], so both tiers read one host).
pub const PATH_TIME: &str = "/api/v5/public/time";

/// The PRIVATE user-data socket's path on the account's `user` `ws` row (product `""`).
pub const PATH_WEBSOCKET_PRIVATE: &str = "/ws/v5/private";
/// The query OKX's DEMO sockets are dialled with, private and business alike. Appended by code for
/// the DEMO type only, never held in a row (a `venue_url` row is a base: no query).
const DEMO_WEBSOCKET_QUERY: &str = "?brokerId=9999";

/// Whether the REST transport must send the `x-simulated-trading: 1` header — `true` for demo,
/// `false` for mainnet. It is the exact inverse of whether the start resolved the LIVE (mainnet)
/// tier: `rest_transport(true)` (its historical, hardcoded value) is precisely
/// `simulated(false)`, so a demo-ceiling start is byte-identical to before this existed.
///
/// ⚠ **The account's tier IS the network for this venue** (decision 0095, as amended) —
/// `crate::adapter`'s `VenueAdapterOkx::start` resolves the account's [`OkxNet`] ONCE per start from
/// the tier (`AdapterInputs::tier`, the account row's `tier` as `vike-tradehub-core` folds it — never an
/// `OKX_MAINNET` flag any more), and that one value is then threaded into every adapter site that
/// calls this — the grid pre-fetch, the exec spawn, the funding poller, the recon client — instead of
/// each spawned thread reading anything for itself. OKX is unusual — demo and mainnet SHARE the REST
/// host (the seed's okx demo `trade` `rest` row equals the live one, an allow-listed key of
/// `vike_venues::venue_urls::DEMO_SHARES_LIVE`); demo is selected by this header, which follows the
/// TYPE and never the host, so switching to mainnet means DROPPING it AND dialling the live private
/// and business sockets ([`OkxNet::resolve`]).
pub fn simulated(mainnet: bool) -> bool {
    !mainnet
}

/// **The addresses ONE start dials, resolved once** from the account's `venue_url` rows at its type
/// ([`OkxNet::resolve`]): the REST base and the two private sockets, the sockets as dialled (path
/// and, on the demo type, [`DEMO_WEBSOCKET_QUERY`] included). `mainnet` is the TYPE (`live`); it alone
/// decides the `x-simulated-trading` header ([`simulated`]), because demo and mainnet share the REST
/// host and no address can say which network a request is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkxNet {
    /// The account's type is `live`: the real-money network, no `x-simulated-trading` header.
    pub mainnet: bool,
    /// The REST base every signed (and the grid's public) request of the account goes to.
    pub rest: String,
    /// The PRIVATE socket (`orders`): [`PATH_WEBSOCKET_PRIVATE`] on the `user` `ws` row.
    pub private_websocket: String,
    /// The BUSINESS socket (`orders-algo`, a stop's own state; the private socket carries only the
    /// child order a fired stop spawns), same login as the private one:
    /// `crate::feed::tick::PATH_WEBSOCKET_BUSINESS` on the `user` `ws` `business` row. Port 443 where the
    /// private socket's row is still on 8443: the venue's notice on the 8443 demo socket (code 64010)
    /// says that port "will be retired soon", and the demo accepted the business socket on 443 on
    /// 2026-10-24.
    pub business_websocket: String,
}

impl OkxNet {
    /// The account's addresses at the type `tier` dials (`UrlType::of_tier`: `paper` reads as
    /// `demo`): REST at `trade`, both sockets at `user`. Neither channel falls back from demo to live
    /// (`vike_venues::venue_urls::VenueUrls::url`), so a demo account can reach no live address here.
    ///
    /// # Errors
    /// The lookup's refusal: a missing row, or a demo `user` address that equals a live one. The
    /// account is NOT RUNNING, with the refusal's words as the reason.
    pub fn resolve(urls: &VenueUrls, tier: vike_model::AccountTier) -> Result<OkxNet, UrlRefusal> {
        let ty = UrlType::of_tier(tier);
        let query = if ty == UrlType::Demo { DEMO_WEBSOCKET_QUERY } else { "" };
        let ws = |product: &str, path: &str| -> Result<String, UrlRefusal> {
            Ok(format!(
                "{}{path}{query}",
                urls.url(ty, UrlChannel::User, UrlProtocol::Websocket, product)?
            ))
        };
        Ok(OkxNet {
            mainnet: ty == UrlType::Live,
            rest: urls.url(ty, UrlChannel::Trade, UrlProtocol::Rest, "")?.to_string(),
            private_websocket: ws("", PATH_WEBSOCKET_PRIVATE)?,
            business_websocket: ws(PRODUCT_BUSINESS, PATH_WEBSOCKET_BUSINESS)?,
        })
    }
}

#[path = "endpoint_switch_tests.rs"]
#[cfg(test)]
mod endpoint_switch_tests;

/// Map the request's margin mode → the OKX SWAP `tdMode` wire value — the margin-mode STEP-2
/// un-hardcode for the `MarginModeSwitch::PerOrderField` okx row of
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
    /// none. Read by the exec thread's INVERSE refusal ([`crate::execution::inverse_swap_refusal`]): an
    /// inverse swap counts a contract in USD (`ctVal` 100 USD on `BTC-USD-SWAP`), and
    /// [`VenueRestOkx::to_contracts`] divides a BASE quantity by it.
    pub contract_type: Option<String>,
    /// The row's own `instType` word (`SPOT`, `SWAP`, `FUTURES`, `OPTION`), verbatim; `None` when the row
    /// states none. Held against the class the instId's SHAPE names before anything is signed
    /// ([`crate::instrument::InstrumentType`], `crate::execution`'s `foreign_rest`).
    pub inst_type: Option<String>,
    /// The row's `state` (`live`, `suspend`, `preopen`, `rebase`, ...), verbatim; only `live` takes an order.
    pub state: Option<String>,
    /// The row's `settleCcy`: the currency a derivative's margin and PnL are in (`USDT`; the coin on an
    /// inverse contract; `USDC` or `USD` on the others). Absent (empty) on a SPOT row.
    pub settle_ccy: Option<String>,
    /// A SPOT row's `quoteCcy`; absent on a derivative.
    pub quote_currency: Option<String>,
}

impl OkxInstrument {
    /// Whether the row states a contract multiplier other than 1: such a contract is worth
    /// `ctVal x ctMult`, and every linear sizing here divides by `ctVal` alone. An absent `ctMult`
    /// (0.0) is the usual 1.
    pub(crate) fn multiplied(&self) -> bool {
        self.ct_mult.is_finite() && self.ct_mult > 0.0 && (self.ct_mult - 1.0).abs() > 1e-12
    }
}

/// The exec plane's per-instId map over a `public/instruments` payload. The grid (and its class,
/// from the row's own `instType`) is `crate::catalog::row_grid` — the SAME row read the catalog's
/// `parse_instruments` uses, so identity is decided once (decision 0126).
///
/// ⚠ **This parser does not only ever see SWAP rows, which is why the class is read per ROW.**
/// [`crate::execution::fetch_okx_instrument_row`] asks `instType=SWAP`, but the function is a pure
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
        let f = |key: &str| -> f64 { entry.get(key).and_then(json_number).unwrap_or(0.0) };
        let word = |key: &str| -> Option<String> {
            entry.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
        };
        // A SPOT row counts size in the BASE coin itself: it sends `ctVal` and `ctValCcy` empty, and one
        // unit of `sz` is one unit of `baseCcy`. So its "contract" is 1.0 base, which keeps `to_contracts`,
        // `okx_grid_in_base` and every report's rescale the identity for a spot pair, one arithmetic for
        // all three classes.
        let spot = entry.get("instType").and_then(|t| t.as_str()) == Some("SPOT");
        out.insert(
            inst_id,
            OkxInstrument {
                // `contract_size` stays absent here: OKX sizes in CONTRACTS and this map carries
                // its own contracts→base factor (`ct_val`, applied at `to_contracts`), deliberately
                // NOT duplicated (absent = multiplier 1.0) — see `row_grid`'s doc.
                properties: SymbolProperties {
                    // a SPOT row states `maxMktSz` in the QUOTE currency, which is no base maximum
                    max_qty: if spot { 0.0 } else { crate::catalog::row_grid(entry).max_qty },
                    ..crate::catalog::row_grid(entry)
                },
                base_asset: entry
                    .get(if spot { "baseCcy" } else { "ctValCcy" })
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
                ct_val: if spot { 1.0 } else { f("ctVal") },
                ct_mult: f("ctMult"),
                inst_type: word("instType"),
                state: word("state"),
                settle_ccy: word("settleCcy"),
                quote_currency: word("quoteCcy"),
                // The account-leverage ceiling. NOT routed through `f` (the absent-is-0.0 closure
                // every grid field above uses): the ONE consumer
                // (`vike_bridge_core::leverage::clamp_to_venue_cap`) must distinguish "unknown,
                // change nothing" from a real ceiling, and `0.0` cannot express the former.
                max_leverage: entry.get("lever").and_then(json_number),
                // a SPOT row sends `ctType` as "" (it has no contract): that is no kind, not an unknown one
                contract_type: entry
                    .get("ctType")
                    .and_then(|v| v.as_str())
                    .filter(|_| !spot)
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

pub struct VenueRestOkx<T: RestTransportJson> {
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

impl<T: RestTransportJson> VenueRestOkx<T> {
    fn call(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let response = self.transport.signed(&self.base_url, path, method, params, &self.signer)?;
        unwrap_okx(response)
    }

    /// Signed GET returning the unwrapped `data` list — the funding poller's seam.
    pub fn call_get(&self, path: &str, params: &[(&str, Value)]) -> Result<Value, VenueApiError> {
        self.call(path, "GET", params)
    }

    /// Audit A3 resync: recent orders (`GET /api/v5/trade/orders-history`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the unwrapped `data` list.
    pub fn get_orders_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        self.get_orders_history_for(&self.symbol, limit)
    }

    /// [`Self::get_orders_history`] for ANOTHER instrument of the account (an admitted one: the resync and
    /// the gap-sentinel replay each instrument the engine trades, `crate::execution`'s `okx_history_events_of`).
    /// The request is the mounted instrument's, byte for byte, with that `instId`.
    pub fn get_orders_history_for(
        &self,
        instrument: &str,
        limit: u32,
    ) -> Result<Value, VenueApiError> {
        // the venue answers `51015` to an `instType` that is not the instrument's (MEASURED, 2026-10-30)
        let params: Vec<(&str, Value)> = vec![
            ("instType", json!(read_inst_type(instrument)?)),
            ("instId", json!(instrument)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_ORDERS_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// Audit A3 resync: recent fills (`GET /api/v5/trade/fills-history`) for the history replay.
    pub fn get_fills_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        self.get_fills_history_for(&self.symbol, limit)
    }

    /// [`Self::get_fills_history`] for ANOTHER instrument of the account (see
    /// [`Self::get_orders_history_for`]).
    pub fn get_fills_history_for(
        &self,
        instrument: &str,
        limit: u32,
    ) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("instType", json!(read_inst_type(instrument)?)),
            ("instId", json!(instrument)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_FILLS_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// Audit T1: re-query one order by clOrdId (= our idempotent order_client_id) on the
    /// short-timeout requery transport. `Ok(Some((id, events)))` = the venue has it: its venue id and what
    /// the stream's own mapper says of its state, `Ok(None)` = venue confirms absent
    /// (top-level code 51603 / the [`cancel_says_gone`] family: its own word, and the only thing that says so), `Err` =
    /// query failed, or a 2xx that names no order (silence on both paths; the stuck-order ladder
    /// decides). The two-endpoint form below is the confirm's; the submit-timeout path uses the
    /// regular-book half alone.
    ///
    /// ⚠ A row of this read carries the order's last fill (`fillSz` in CONTRACTS, `tradeId`) and `fee`,
    /// never the stream's `fillFee` (`tests/fixtures/algo/rest_orders_history.json`). Through
    /// [`map_okx_order`]'s fill arm that is a fill at the wrong size with no fee, so the row goes in with
    /// no fill: this read states the order's STATE, and its fills ride the stream and the replay.
    ///
    /// **"Absent" needs BOTH endpoints:** the regular book first (the ordinary order's one call), then
    /// the algo endpoint — a resting stop is not in the regular book, so the regular `51603` alone would
    /// reject a live stop. A FAILED algo query is `Err` (says nothing), never "absent".
    ///
    /// ⚠ This is the ACTIVE CONFIRM's lookup (`ConfirmByOrderClientId::confirm_when`) ONLY. The submit-timeout
    /// path ([`Self::resolve_ambiguous_submit`]) asks [`Self::query_regular_ordid`] alone: an ordinary
    /// submit can never be a stop (`submit_order` hands stops to `submit_stop_algo`, which asks the
    /// algo book alone), and a failed algo query there would turn the venue's clean "absent" into an
    /// id-less accept of an order that never reached OKX.
    fn query_order_ordid(
        &self,
        order_client_id: &str,
    ) -> Result<Option<(String, Vec<Event>)>, VenueApiError> {
        // asked by this order client id, so the answer is about it; the mappers name the order off the row
        if let Some((id, mut row)) = self.query_regular_ordid(order_client_id)? {
            row["clOrdId"] = json!(order_client_id);
            row["fillSz"] = json!("0");
            return Ok(Some((id, map_okx_order(&row, VENUE, &self.symbol))));
        }
        let Some((id, mut row)) = self.lookup_algo(order_client_id)? else { return Ok(None) };
        row["algoClOrdId"] = json!(order_client_id);
        Ok(Some((id, map_okx_algo_order(&row))))
    }

    /// The regular-book half of [`Self::query_order_ordid`] (`GET /trade/order` by `clOrdId`): the row and
    /// its `ordId`.
    fn query_regular_ordid(
        &self,
        order_client_id: &str,
    ) -> Result<Option<(String, Value)>, VenueApiError> {
        let params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(order_client_id))];
        let response = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_CREATE,
            "GET",
            &params,
            &self.signer,
        );
        match response.and_then(unwrap_okx) {
            Ok(data) => {
                let id = first_row_id(&data, "ordId").ok_or_else(|| VenueApiError {
                    code: E_TIMEOUT_AMBIGUOUS,
                    msg: format!("GET {PATH_ORDER_CREATE} answered 2xx with no ordId"),
                })?;
                Ok(Some((id, data[0].clone())))
            }
            Err(exc) if exc.code == 51603 || cancel_says_gone(exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1). Regular book only,
    /// as it always was: see [`Self::query_order_ordid`] for why the algo endpoint is not asked here.
    /// `code` is what left it unknown: after a code that says the server had trouble
    /// ([`server_had_trouble`]) a query that FAILS too says nothing, because a busy venue most likely took
    /// nothing and an id-less accept is a phantom no sweep ever finds; the order stays pre-ack for the
    /// watchdog's confirm and backstop.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest, code: i64) -> Vec<Event> {
        let query = self.query_regular_ordid(&request.order_client_id);
        if server_had_trouble(code)
            && let Err(e) = &query
        {
            tracing::warn!(
                target: "vike_okx::execution::rest", order_client_id = %request.order_client_id, code,
                query_code = e.code, "the venue had trouble and the re-query failed too; saying nothing"
            );
            return Vec::new();
        }
        let id = query.map(|found| found.map(|(id, _row)| id));
        vike_bridge_core::resolve_ambiguous_submit(&request.order_client_id, request.timestamp, id)
            .into_iter()
            .collect()
    }

    /// base qty → CONTRACTS, floored to the lotSz step. One of the pinned Decimal wire sites
    /// (plan §f64-policy; `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES`
    /// is the list). FRACTIONAL contracts are legal (lotSz e.g. 0.01) — never
    /// round to a whole contract or sub-0.5-contract orders floor to 0.
    pub fn to_contracts(&self, base_quantity: f64) -> f64 {
        let (Some(raw), Some(contract), Some(step)) = (
            Decimal::from_str(&py_f64_str(base_quantity)).ok(),
            Decimal::from_str(&py_f64_str(self.ct_val)).ok(),
            Decimal::from_str(&py_f64_str(self.properties.step_size)).ok(),
        ) else {
            return 0.0;
        };
        if contract.is_zero() || step.is_zero() {
            return 0.0;
        }
        // Python: (raw / ct) // step * step — Decimal // truncates toward zero
        let contracts = (raw / contract / step).trunc() * step;
        contracts.to_f64().unwrap_or(0.0)
    }

    /// signed contracts → base (plain float multiply, matching Python `_to_base`).
    pub fn to_base(&self, contracts: f64) -> f64 {
        contracts * self.ct_val
    }

    /// POST set-leverage (mgnMode=cross). Re-raise on ANY non-'0' code — OKX repeat
    /// calls return '0' (idempotent); no benign code is swallowed without demo proof.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        let leverage = if self.leverage == (self.leverage as i64) as f64 {
            format!("{}", self.leverage as i64)
        } else {
            py_f64_str(self.leverage)
        };
        let params: Vec<(&str, Value)> = vec![
            ("instId", json!(self.symbol)),
            ("lever", json!(leverage)),
            ("mgnMode", json!("cross")),
        ];
        self.call(PATH_SET_LEVERAGE, "POST", &params).map(|_| ())
    }

    /// SWAP order: sz in CONTRACTS, tdMode from [`swap_td_mode`] (unset = the historical
    /// `"cross"`), posSide=net, reduceOnly, NO tgtCcy. `Err` = the requested margin mode cannot
    /// ride the wire (Cash on SWAP) — the caller MUST surface it as a terminal `OrderRejected`.
    ///
    /// tif step-2 (FLIPPED): OKX has no TIF field — the TIF rides `ordType`, and the limit path
    /// consumes okx's `vike_venues::venue_time_in_force::venue_time_in_force` row: Gtc stays NotEmitted
    /// (`ordType:"limit"`, the venue default good-till-cancel rules — byte-identical to
    /// pre-flip, pinned in `r6_okx_parity.rs`), Ioc/Fok map to the `ioc`/`fok` ordTypes (still
    /// limit orders: px required and sent). Gtd/Day are Unsupported and gated at submit
    /// (`deny_unsupported_time_in_force`) — if one reaches this builder anyway, `ordType:"limit"` is
    /// emitted (the venue default), the gate being the loud guard. Market orders keep today's
    /// `ordType:"market"` (the table's resting-path scope; stops route to the algo endpoint).
    pub fn build_order_params(
        &self,
        request: &OrderRequest,
        broker_code: Option<&str>,
    ) -> Result<Vec<(&'static str, Value)>, String> {
        self.build_order_params_in(request, broker_code, SpotMode::Cross)
    }

    /// [`Self::build_order_params`] for a given `tdMode` attempt of a SPOT pair ([`SpotMode`]: `cross` first,
    /// `cash` for the account level that refuses it, `class::post_order_params`); a SWAP or FUTURES body
    /// ignores it. The class is the instId's own ([`InstrumentType::of_inst_id_or_refusal`]: an id of no known form is
    /// refused in words, never signed as a SWAP): a SPOT body states no
    /// `posSide` and no `reduceOnly`, and a MARKET one states `tgtCcy=base_ccy` (`class::shape_spot_body`).
    pub fn build_order_params_in(
        &self,
        request: &OrderRequest,
        broker_code: Option<&str>,
        spot_mode: SpotMode,
    ) -> Result<Vec<(&'static str, Value)>, String> {
        let class = InstrumentType::of_inst_id_or_refusal(&self.symbol)?;
        let td_mode = class::td_mode(class, request.margin_mode, spot_mode)?;
        // Only a market and a limit order have a word on this endpoint. Any other kind (a market-to-limit,
        // an unrecognised word; a stop, stop-limit or take-profit through the batch) is refused by name,
        // never sent as the market order it once fell into; `submit_order` hands stops, stop-limits and
        // take-profits to the algo endpoint.
        let is_limit = match &request.order_type {
            OrderType::Limit => true,
            OrderType::Market => false,
            other => {
                return Err(format!(
                    "okx's order endpoint takes market and limit orders only; a {other} order is not \
                     built here"
                ));
            }
        };
        let ord_type = if is_limit {
            match vike_venues::venue_time_in_force::venue_time_in_force(
                VENUE,
                request.time_in_force,
            ) {
                vike_venues::venue_time_in_force::TimeInForceOutcome::Mapped(w)
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Coerced {
                    wire: w, ..
                } => w,
                vike_venues::venue_time_in_force::TimeInForceOutcome::Ignored { .. }
                | vike_venues::venue_time_in_force::TimeInForceOutcome::NotEmitted
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Unsupported => "limit",
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
            ("clOrdId", json!(request.order_client_id)),
            ("posSide", json!("net")),
            ("reduceOnly", json!(request.reduce_only)),
        ];
        if is_limit {
            params.push((
                "px",
                json!(format_to_step_f(request.price.unwrap_or(0.0), self.properties.tick_size)),
            ));
        }
        if class == InstrumentType::Spot {
            class::shape_spot_body(&mut params, is_limit);
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
    /// ⚠ The success ack is only "request accepted": an amend that fails after it is the `orders` push's
    /// `amendResult` `-1`, which puts the old terms back (`crate::execution::event_mapper`'s module doc).
    pub fn modify_order(
        &self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let mut params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(order_client_id))];
        if let Some(q) = new_qty {
            params.push((
                "newSz",
                json!(format_to_step_f(self.to_contracts(q), self.properties.step_size)),
            ));
        }
        if let Some(p) = new_price {
            params.push(("newPx", json!(format_to_step_f(p, self.properties.tick_size))));
        }
        // `Self::call`, with the reply row's `ts` read first (epoch ms as a string: when the venue
        // finished the amend, a refused one too), since the unwrap's `VenueApiError` keeps no time.
        let reply =
            self.transport.signed(&self.base_url, PATH_ORDER_AMEND, "POST", &params, &self.signer);
        let stated = reply
            .as_ref()
            .ok()
            .and_then(|r| r.get("data"))
            .and_then(|d| d.get(0))
            .and_then(|d0| d0.get("ts"))
            .and_then(|t| json_text(t).parse().ok());
        match reply.and_then(unwrap_okx) {
            Ok(data) => vec![Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: data
                    .as_array()
                    .and_then(|d| d.first())
                    .and_then(|d0| d0.get("ordId"))
                    .map(json_text)
                    .map(Into::into),
                new_qty,
                new_price,
                new_trigger_price: None,
                timestamp: vike_bridge_core::rest::amend_answer_time(stated),
            })],
            // modify failed at the venue — the resting order is unchanged, but the refusal must not
            // vanish: a NON-terminal advisory carrying the venue's reason (as binance perp).
            Err(exc) => vec![modify_failed(order_client_id, &exc, stated)],
        }
    }
}

/// The active confirm over the by-clOrdId re-query ([`VenueRestOkx::query_order_ordid`]), in the shared
/// form of `vike_bridge_core::confirm`: found is read by its state, `51603` (absent) ->
/// the "venue confirms absent" reject only for an order client id the exec thread finished dispatching, a FAILED
/// query -> nothing.
impl<T: RestTransportJson + Send> ConfirmByOrderClientId for VenueRestOkx<T> {
    // None: okx documents no age limit on `GET /trade/order` that was measured here.
    const LOOKUP_WINDOW: Option<std::time::Duration> = None;

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let query = self.query_order_ordid(order_client_id);
        resolve_confirm(order_client_id, ts, query, absent_rejects, |(id, mapped)| {
            found_by_state(order_client_id, ts, Some(id), mapped)
        })
    }
}

impl<T: RestTransportJson + Send> VenueRest for VenueRestOkx<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        // Delivery gate (a dated future only, read off the order's USER name): an opening order inside the last day,
        // and anything after the delivery, is a terminal reject in words; nothing is signed, a stop algo included
        // (`crate::instrument::expiry_refusal`).
        if let Some(reason) = crate::instrument::expiry_refusal(
            &request.symbol,
            vike_model::now_ms(),
            request.reduce_only,
        ) {
            return vec![
                Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: request.order_client_id.clone(),
                    timestamp: request.timestamp,
                }),
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::NotSent.word(),
                    ),
                }),
            ];
        }
        // OKX stops are ALGO orders on a different endpoint — route them there; a stop-limit is the
        // same `conditional` algo with its limit in `slOrdPx`, a take-profit its `tpTriggerPx`/`tpOrdPx`;
        // a trailing order is the `move_order_stop` algo (`callbackSpread` for a price trail or
        // `callbackRatio` for a percent one, `activePx`), and a trailing_limit is refused there by
        // name (okx's trailing order fires a market order).
        if matches!(
            request.order_type,
            OrderType::Stop
                | OrderType::StopLimit
                | OrderType::TakeProfit
                | OrderType::Trailing
                | OrderType::TrailingLimit
        ) {
            return self.submit_stop_algo(request);
        }
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        })];
        // tif step-2 gate: a TIF okx cannot express (GTD/Day — no such ordType) is a LOUD
        // terminal reject; the wire is never touched, never a silent limit-GTC (pinned in
        // `tests/offline/time_in_force_gate.rs`).
        if let Some(reject) =
            vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, request)
        {
            events.push(reject);
            return events;
        }
        // Margin-mode gate: an unsupported requested mode never reaches the wire — it comes back
        // as a synthesized terminal OrderRejected (deny loudly, never silently coerce).
        let params = match self.build_order_params(request, self.broker_code.as_deref()) {
            Ok(p) => p,
            Err(reason) => {
                events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: request.timestamp,
                    // shortcut: a malformed instId (NOT SENT) gets it too; split the error if one reaches here
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueCapability.word(),
                    ),
                }));
                return events;
            }
        };
        match self.post_order_params(PATH_ORDER_CREATE, request, params, |mode| {
            self.build_order_params_in(request, self.broker_code.as_deref(), mode)
        }) {
            Ok(data) => match first_row_id(&data, "ordId") {
                Some(id) => events.push(Event::OrderAccepted(OrderAccepted {
                    order_client_id: request.order_client_id.clone(),
                    venue_order_id: Some(id.into()),
                    timestamp: request.timestamp,
                })),
                // a 2xx that names no order: it may rest, so ask, as for a timeout
                None => events.extend(self.resolve_ambiguous_submit(request, E_TIMEOUT_AMBIGUOUS)),
            },
            // audit T1: an ambiguous timeout may have been accepted — re-query, don't false-reject.
            Err(exc) if TAXONOMY.submit_outcome_unknown(exc.code) => {
                events.extend(self.resolve_ambiguous_submit(request, exc.code));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_okx::execution::rest", order_client_id = %request.order_client_id, code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: crate::account::modes::explain_refusal(
                        &exc.msg,
                        self.transport.network(),
                    )
                    .into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(exc.refusal_id_word()),
                }))
            }
        }
        events
    }

    /// `sCode 0` on `cancel-order` is [`OrderCancelAck::Requested`](vike_bridge_core::rest::OrderCancelAck::Requested),
    /// never `Canceled`: the venue took the request, and the `orders` channel's `canceled` row (read 3 to 13 ms BEFORE
    /// the reply returned on the demo host, 2026-11-23, `crates/bridges/okx/tests/okx_smokes/okx_cancel_reply_smoke.rs`'s
    /// `okx_a_cancel_reply_and_the_streams_canceled_row`) or the gap-sentinel's replay ends the order; a fill that
    /// beat the cancel ends it `Filled`. A stop's `cancel-algos` keeps its own answer.
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        let params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("clOrdId", json!(order_client_id))];
        match self.call(PATH_ORDER_CANCEL, "POST", &params) {
            Ok(_) => Ok(vike_bridge_core::rest::OrderCancelAck::Requested),
            // not in the regular book: maybe a resting STOP (an algo order, which this endpoint does
            // not cancel) — one retry on `cancel-algos`, whose own "unknown" is the idempotent "gone"
            // once the algo book confirms it
            Err(exc) if cancel_says_gone(exc.code) => {
                tracing::debug!(target: "vike_okx::execution::rest", order_client_id, "cancel: unknown on cancel-order, trying cancel-algos");
                self.cancel_algo(order_client_id)
            }
            Err(exc) => Err(exc),
        }
    }

    /// ACTIVELY confirm one order's status, in the shared form of `vike_bridge_core::confirm`: this
    /// trait door assumes the submit was dispatched ([`ConfirmByOrderClientId::confirm_when`] with `true`); the
    /// exec client's confirm closure (`crate::execution`) goes through `confirm_when` with the ledger's
    /// real answer. Runs on the ExecutionClientActor's own confirm worker.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }

    /// Native OKX amend — overrides the no-op default with `/api/v5/trade/amend-order`. OKX
    /// identifies the order by `clOrdId`, so only the order client id is needed from the resting `order`. A
    /// stop, a stop-limit and a take-profit rest on the ALGO book, which `amend-order` does not reach:
    /// they go to `amend-algos` (`stop_algo.rs`'s `amend_algo`; a stop's `new_price` is its trigger). A
    /// trailing order rests on the algo book too and `amend-algos` excludes it: refused there in words.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        if matches!(
            order.order_type,
            OrderType::Stop
                | OrderType::StopLimit
                | OrderType::TakeProfit
                | OrderType::Trailing
                | OrderType::TrailingLimit
        ) {
            return self.amend_algo(order, new_qty, new_price, None);
        }
        VenueRestOkx::modify_order(self, &order.order_client_id, new_qty, new_price)
    }

    /// A stop-limit's or a take-profit's TRIGGER moved in place, with its size and limit:
    /// `amend-algos` (`stop_algo.rs`'s `amend_algo`). Any other kind is refused there in words.
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Vec<Event> {
        self.amend_algo(order, new_qty, new_price, Some(new_trigger_price))
    }

    /// Native OKX batch-submit — overrides the fan-out default with `/api/v5/trade/batch-orders`.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        VenueRestOkx::submit_batch(self, requests)
    }
}

#[path = "margin_mode_tests.rs"]
#[cfg(test)]
mod margin_mode_tests;
