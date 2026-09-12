//! OkxPerpRest — signed V5 SWAP submit/cancel/reconcile + set-leverage. Exact port of
//! `exec/okx/perp_client.py` (+ the shared flow and the spot client's unwrap/NOT_FOUND).
//!
//! SWAP deltas: instId `BTC-USDT-SWAP`, tdMode honors `OrderRequest.margin_mode` via
//! [`swap_td_mode`] (unset = the historical `"cross"` byte-for-byte; `Isolated` sends
//! `"isolated"`; `Cash` is DENIED — spot-only on OKX), `ordType` carries the requested TIF
//! (tif step-2 FLIPPED: unset/GTC → `"limit"` byte-identical, Ioc/Fok → the `ioc`/`fok`
//! ordTypes, GTD/Day loud-denied at submit via `deny_unsupported_tif`), `sz` in CONTRACTS
//! (`to_contracts` — the SECOND pinned Decimal wire site: base/ctVal floored to lotSz,
//! FRACTIONAL contracts allowed), posSide=net, reduceOnly bool, NO tgtCcy; set-leverage
//! re-raises on ANY non-'0' code (OKX is idempotent — no benign code confirmed);
//! /api/v5/account/positions reconcile where `pos` is ALREADY SIGNED contracts.

use indexmap::IndexMap;
use rust_decimal::prelude::*;
use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_exec::{ManagedOrder, OrderStatus, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{MarginMode, OrderRequest, SymbolProperties};

use crate::transport::{OkxTransport, unwrap_okx};
use vike_bridge_core::format::{format_to_step_f, py_f64_str};
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;

/// Canonical venue id — the okx `venue_tif` row key (tif step-2).
pub const VENUE: &str = "okx";

pub const PATH_ORDER_CREATE: &str = "/api/v5/trade/order";
pub const PATH_ORDER_CANCEL: &str = "/api/v5/trade/cancel-order";
pub const PATH_ORDER_AMEND: &str = "/api/v5/trade/amend-order";
pub const PATH_BATCH_ORDERS: &str = "/api/v5/trade/batch-orders";
pub const PATH_CANCEL_BATCH: &str = "/api/v5/trade/cancel-batch-orders";
pub const PATH_ORDER_ALGO: &str = "/api/v5/trade/order-algo";
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
/// `vike_mount::server_time`'s okx row: okx stamps `OK-ACCESS-TIMESTAMP` on every private request
/// and rejects one outside its window with `50102` ([`crate::error_codes`]).
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

/// `OKX_MAINNET` env-flag name — the exact-`"1"` opt-in that flips this venue's PRIVATE exec/WS/
/// recon path onto MAINNET (real funds). Parsed by the ONE converged workspace rule (STEP 2, see
/// `vike_bridge_core::mainnet`'s module doc): the EXACT string `"1"`, read from the real process
/// env OR the workspace `.env` map, process env winning. ⚠ STEP 2 CHANGED this venue — an
/// `OKX_MAINNET=1` line in the workspace `.env` used to parse as UNSET here (the `.env` is never
/// exported to process env) and silently kept okx on demo; it now ARMS mainnet, exactly as an
/// exported flag does. OKX is unusual — demo and mainnet SHARE the REST host ([`REST`]); demo is
/// selected by the `x-simulated-trading: 1` header, so switching to mainnet means DROPPING that
/// header (`simulated = false`) AND using the mainnet private WS ([`MAINNET_WS`] instead of
/// [`DEMO_WS`]). Default (unset in BOTH sources) stays demo, byte-identical to before this switch
/// existed. The const AND both reads stay HERE, resolvable by the settings-registry gate; only the
/// PARSE is shared (see `vike_bridge_core::mainnet`'s env-boundary note).
pub const MAINNET_ENV: &str = "OKX_MAINNET";

/// Pure `OKX_MAINNET` predicate over the two already-read source values (`process` = process env,
/// `map` = the workspace `.env` map): armed ONLY by the EXACT string `"1"` from either, with the
/// process value winning when both are set — so any other value, and an absent flag, stays on demo.
/// Split out from the reads ([`mainnet_enabled`]) so it is unit-testable without mutating global
/// env. Delegates to the ONE shared converged rule (`vike_bridge_core::mainnet::mainnet_for`) keyed
/// on this venue's table row, so neither the value grammar nor the source set can drift from the
/// declared workspace rule.
pub fn mainnet_from(process: Option<&str>, map: Option<&str>) -> bool {
    vike_bridge_core::mainnet::mainnet_for(VENUE, process, map)
}

/// Reads [`MAINNET_ENV`] from the real process env AND from the caller-supplied workspace `.env`
/// map → `true` only on the exact `"1"` (the shared fold, [`mainnet_from`]); absent from both ⇒
/// `false` (demo), so the default path is byte-identical to before this flag existed. `vars` is the
/// already-loaded `.env` map the mount owns (`load_workspace_dotenv`) — this fn performs the
/// process-env read only, never file I/O, so the settings-registry gate keeps resolving
/// `OKX_MAINNET` through the const.
///
/// Called ONCE per mount (`vike_mount::make_engine`); the resolved `bool` is then threaded into
/// every adapter site that needs it ([`simulated`]/[`ws_url`], the grid pre-fetch, the exec spawn,
/// the funding poller, the recon client) instead of each spawned thread re-reading global env.
pub fn mainnet_enabled(vars: &std::collections::HashMap<String, String>) -> bool {
    mainnet_from(
        std::env::var(MAINNET_ENV).ok().as_deref(),
        vars.get(MAINNET_ENV).map(String::as_str),
    )
}

/// Whether the REST transport must send the `x-simulated-trading: 1` header — `true` for demo,
/// `false` for mainnet. It is the exact inverse of the mainnet flag: `UreqOkxTransport::new(true)`
/// (its historical, hardcoded value) is precisely `simulated(false)`, so an unset flag is
/// byte-identical.
pub fn simulated(mainnet: bool) -> bool {
    !mainnet
}

/// The PRIVATE user-data WS URL, selected by the mainnet flag. **Default-safe:** `false` ⇒ the
/// exact demo WS OKX used before this switch existed; `true` ⇒ the mainnet private WS.
pub fn ws_url(mainnet: bool) -> &'static str {
    if mainnet { MAINNET_WS } else { DEMO_WS }
}

#[cfg(test)]
mod endpoint_switch_tests {
    use super::*;

    /// Default-safe: unset / non-`"1"` values stay demo; ONLY the exact `"1"` arms mainnet — from
    /// EITHER source, with the process value winning when both are set (STEP 2: the workspace
    /// `.env` map is a real source now, where a `.env`-only line used to be a silent no-op).
    #[test]
    fn mainnet_from_requires_exact_one() {
        assert!(!mainnet_from(None, None), "unset ⇒ demo");
        for v in ["", "0", "true", "TRUE", "yes"] {
            assert!(!mainnet_from(Some(v), None), "process {v:?} ⇒ demo (exact-1 only)");
            assert!(!mainnet_from(None, Some(v)), "map {v:?} ⇒ demo (exact-1 only)");
        }
        assert!(mainnet_from(Some("1"), None), "exact 1 ⇒ mainnet");
        assert!(mainnet_from(None, Some("1")), ".env-only 1 ⇒ mainnet (STEP-2 gain)");
        assert!(!mainnet_from(Some("0"), Some("1")), "disarming process masks an arming .env");
        assert!(mainnet_from(Some("1"), Some("0")), "arming process beats a disarming .env");
    }

    /// The `.env`-map read is threaded from the CALLER's map, not from process env — proven with a
    /// map this test owns (no global-env mutation, so it is safe under a parallel test runner).
    /// Only the direction a stray exported flag cannot spoof is asserted; the negative cases live
    /// in the env-free `mainnet_from` test above.
    #[test]
    fn mainnet_enabled_consults_the_caller_supplied_dotenv_map() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(MAINNET_ENV.to_string(), "1".to_string());
        assert!(mainnet_enabled(&vars), ".env-map `1` must arm mainnet");
    }

    /// Flag unset ⇒ demo WS + the `x-simulated-trading` header PRESENT (byte-identical to the
    /// historical `UreqOkxTransport::new(true)`); flag set ⇒ mainnet WS + NO sim header. The REST
    /// host is shared and never changes.
    #[test]
    fn switch_selects_ws_and_sim_header() {
        // demo (default)
        assert_eq!(ws_url(false), DEMO_WS);
        assert!(simulated(false), "demo ⇒ x-simulated-trading header present");
        assert_eq!(ws_url(false), "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999");
        // mainnet
        assert_eq!(ws_url(true), MAINNET_WS);
        assert!(!simulated(true), "mainnet ⇒ NO sim header");
        assert_eq!(ws_url(true), "wss://ws.okx.com:8443/ws/v5/private");
        // REST host is shared across both.
        assert_eq!(REST, "https://www.okx.com");
    }

    /// This venue HAS a row in the shared per-venue switch table, so [`mainnet_from`]'s fold is
    /// actually gated by it (a switchless venue can never be armed). Removing okx's row would
    /// silently pin this venue to demo forever — this pin makes that a deliberate edit.
    #[test]
    fn okx_has_a_row_in_the_shared_switch_table() {
        assert_eq!(
            vike_bridge_core::mainnet::mainnet_switch_for(VENUE),
            Some(vike_bridge_core::mainnet::MainnetSwitch)
        );
    }
}

/// cancel: filled / already-canceled / does-not-exist family — swallowed
const NOT_FOUND: [i64; 3] = [51400, 51401, 51402];

/// Map the request's margin mode → the OKX SWAP `tdMode` wire value — the margin-mode STEP-2
/// un-hardcode for the `SwitchMechanism::PerOrderField` okx row of
/// `vike_model::venue_margin_support` (the capability-map playbook; TIF-playbook shape).
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
}

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
                properties: SymbolProperties {
                    tick_size: f("tickSz"),
                    step_size: f("lotSz"),
                    min_qty: f("minSz"),
                    max_qty: f("maxMktSz"),
                    // Everything else absent. Notably `contract_size`: OKX sizes in CONTRACTS and
                    // already carries its own contracts→base factor (`ct_val`, applied at
                    // `to_contracts`), deliberately NOT duplicated here (absent = multiplier 1.0).
                    // Also a flat grid (ONE `tickSz`, no tiers), no min-notional, and no venue
                    // taker hold. FRU rather than an exhaustive literal so a new
                    // `SymbolProperties` field costs this parser nothing.
                    ..Default::default()
                },
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
            },
        );
    }
    out
}

#[cfg(test)]
mod leverage_cap_tests {
    use super::*;
    use serde_json::json;

    /// The REAL `/api/v5/public/instruments?instType=SWAP&instId=BTC-USDT-SWAP` row, captured live
    /// 2026-08-05 (trimmed to the keys this parser reads plus `lever`). Every numeric is a STRING,
    /// including `lever` — the venue-wide convention `json_num` decodes.
    fn live_row() -> Value {
        json!({"data": [{
            "instType": "SWAP", "instId": "BTC-USDT-SWAP", "instFamily": "BTC-USDT",
            "ctType": "linear", "ctVal": "0.01", "ctValCcy": "BTC", "ctMult": "1",
            "lever": "100",
            "lotSz": "0.01", "minSz": "0.01", "tickSz": "0.1", "maxMktSz": "35000",
            "settleCcy": "USDT", "state": "live"
        }]})
    }

    /// The cap rides the response the exec thread ALREADY fetches for `ctVal` and the tick/lot grid
    /// — the whole reason okx clamps for free — and its arrival changes nothing else.
    #[test]
    fn parses_lever_from_the_live_instruments_shape() {
        let instruments = parse_okx_perp_instruments(&live_row());
        let inst = &instruments["BTC-USDT-SWAP"];
        assert_eq!(inst.max_leverage, Some(100.0), "top-level `lever` \"100\"");
        // …and the pre-existing fields are exactly what they were before the field existed.
        assert_eq!(inst.properties.tick_size, 0.1);
        assert_eq!(inst.properties.step_size, 0.01);
        assert_eq!(inst.properties.min_qty, 0.01);
        assert_eq!(inst.properties.max_qty, 35000.0);
        assert_eq!(inst.ct_val, 0.01);
        assert_eq!(inst.ct_mult, 1.0);
        assert_eq!(inst.base_asset, "BTC");
    }

    /// Absent / malformed ⇒ `None` (UNKNOWN), never `0.0`. OKX omits `lever` on SPOT and OPTION,
    /// and an empty string is its idiom for "not applicable" — both must read as unknown, because
    /// `None` is what makes the clamp a no-op instead of de-leveraging an account to 0.
    #[test]
    fn an_absent_or_not_applicable_lever_is_unknown() {
        let payload = json!({"data": [
            {"instId": "NOLEVER-USDT-SWAP", "ctValCcy": "N", "lotSz": "1"},
            {"instId": "EMPTY-USDT-SWAP", "ctValCcy": "E", "lever": ""},
            {"instId": "JUNK-USDT-SWAP", "ctValCcy": "J", "lever": "n/a"},
            {"instId": "NULL-USDT-SWAP", "ctValCcy": "X", "lever": null},
        ]});
        let got = parse_okx_perp_instruments(&payload);
        for id in ["NOLEVER-USDT-SWAP", "EMPTY-USDT-SWAP", "JUNK-USDT-SWAP", "NULL-USDT-SWAP"] {
            assert_eq!(got[id].max_leverage, None, "{id} must be UNKNOWN, not 0.0");
        }
    }

    /// End to end at the seam that matters: the parsed cap, fed to the shared clamp, is exactly
    /// `min(requested, cap)` — and an instrument without one changes nothing.
    #[test]
    fn the_parsed_cap_drives_the_shared_clamp() {
        use vike_bridge_core::leverage::clamp_to_venue_cap;
        let capped = parse_okx_perp_instruments(&live_row())["BTC-USDT-SWAP"].max_leverage;
        assert_eq!(clamp_to_venue_cap(2.0, capped, VENUE, "BTC-USDT-SWAP"), 2.0, "under the cap");
        assert_eq!(clamp_to_venue_cap(125.0, capped, VENUE, "BTC-USDT-SWAP"), 100.0, "clamped");
        let payload = json!({"data": [{"instId": "NOCAP-USDT-SWAP", "ctValCcy": "N"}]});
        let unknown = parse_okx_perp_instruments(&payload)["NOCAP-USDT-SWAP"].max_leverage;
        assert_eq!(clamp_to_venue_cap(125.0, unknown, VENUE, "NOCAP-USDT-SWAP"), 125.0, "unknown");
    }
}

pub struct OkxPerpRest<T: OkxTransport> {
    pub signer: OkxV5Signer,
    pub transport: T,
    pub base_url: String,
    /// instId form: BTC-USDT-SWAP
    pub symbol: String,
    pub properties: SymbolProperties,
    pub ct_val: f64,
    pub leverage: f64,
    /// OKX Fully-Disclosed-Broker attribution code, resolved ONCE at mount from
    /// `OKX_BROKER_CODE`/`OKX_BUILDER_CODE` via `attribution_code_from` (see
    /// `vike_bridge_core::credentials`). `None` (unset, or fails the venue's
    /// `AttributionMechanic` validation) ⇒ `build_order_params` emits no `tag` key —
    /// byte-identical to before this field existed.
    pub broker_code: Option<String>,
}

/// ⚠ **The same grid, restated in BASE units** — for any consumer that judges a `qty` in base
/// rather than one that is about to hit the wire.
///
/// `parse_okx_perp_instruments` fills `step_size`/`min_qty`/`max_qty` from OKX's `lotSz`/
/// `minSz`/`maxMktSz`, which are counts of CONTRACTS. [`Self::to_contracts`] is the reason that
/// is right: it takes a BASE qty, divides by `ct_val`, and floors on that contracts grid. Every
/// wire-side consumer wants exactly those raw numbers.
///
/// But `OrderRequest.qty` is BASE, and so is everything the pre-trade gate reasons about. A
/// consumer that hands the raw grid to `vike_exec::RiskLimits::from_properties` is asking the
/// gate to floor a base quantity on a contracts step — for BTC-USDT-SWAP (`ct_val` 0.01) that
/// grid is 100x too coarse, so a legitimate 0.015 BTC order floors to 0.01 and a size the venue
/// would accept is refused outright. That is the bug this exists to stop, and it is spelled
/// HERE, once, next to `to_contracts`, rather than re-derived at each mount.
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
    /// (top-level code 51603 / NOT_FOUND family, or empty data), `Err` = query failed.
    fn query_order_ordid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
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
            Ok(data) => {
                Ok(data.as_array().and_then(|d| d.first()).and_then(|d0| d0.get("ordId")).map(
                    |v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    },
                ))
            }
            Err(exc) if exc.code == 51603 || NOT_FOUND.contains(&exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1).
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_order_ordid(&request.client_order_id),
        )
    }

    /// base qty → CONTRACTS, floored to the lotSz step. THE second pinned Decimal wire
    /// site (plan §f64-policy). FRACTIONAL contracts are legal (lotSz e.g. 0.01) — never
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
    /// consumes okx's `vike_bridge_core::tif::venue_tif` row: Gtc stays NotEmitted
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
        let is_limit = request.order_type.eq_ignore_ascii_case("limit");
        let ord_type = if is_limit {
            match vike_bridge_core::tif::venue_tif(VENUE, request.time_in_force) {
                vike_bridge_core::tif::TifOutcome::Mapped(w)
                | vike_bridge_core::tif::TifOutcome::Coerced { wire: w, .. } => w,
                vike_bridge_core::tif::TifOutcome::Ignored { .. }
                | vike_bridge_core::tif::TifOutcome::NotEmitted
                | vike_bridge_core::tif::TifOutcome::Unsupported => "limit",
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
    /// order keeps its old terms and `[]` is returned (nothing vanishes). RUST-NATIVE, no Python twin.
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
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .map(Into::into),
                new_qty,
                new_price,
                ts: 0,
            })],
            // modify failed at the venue — the resting order is unchanged; nothing to surface
            Err(_) => Vec::new(),
        }
    }

    /// Native batch-submit (POST /api/v5/trade/batch-orders — a JSON ARRAY body, ≤20/req). Emits
    /// [OrderSubmitted] for every order, then per-order [OrderAccepted|OrderRejected] mapped from
    /// the response's `data[i].sCode` (matched by clOrdId). Chunks at BATCH_MAX; a whole-request
    /// failure rejects that chunk so NO order silently vanishes. RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::with_capacity(requests.len() * 2);
        for r in requests {
            events.push(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: r.client_order_id.clone(),
                ts: r.ts,
            }));
        }
        // Per-order gates, applied BEFORE chunking so a denied order never enters a wire chunk:
        // the tif step-2 gate (`deny_unsupported_tif` — GTD/Day have no okx ordType) and the
        // margin-mode gate (Cash can't ride the SWAP wire). The rest batch as before.
        let mut sendable: Vec<(&OrderRequest, Vec<(&'static str, Value)>)> =
            Vec::with_capacity(requests.len());
        for r in requests {
            if let Some(reject) = vike_bridge_core::tif::deny_unsupported_tif(VENUE, r) {
                events.push(reject);
                continue;
            }
            // TODO(unified-venue-attribution): batch submits don't yet carry the FD-broker `tag`
            // (scoped to the single-order path this task covers); a follow-up should thread
            // `self.broker_code.as_deref()` here too so batch orders attribute identically.
            match self.build_order_params(r, None) {
                Ok(p) => sendable.push((r, p)),
                Err(reason) => events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: r.client_order_id.clone(),
                    reason: reason.into(),
                    ts: r.ts,
                })),
            }
        }
        for chunk in sendable.chunks(BATCH_MAX) {
            let body_orders: Vec<Value> = chunk
                .iter()
                .map(|(_, params)| {
                    Value::Object(params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
                })
                .collect();
            let body = serde_json::to_string(&body_orders).expect("array body");
            match self.transport.signed_json(
                &self.base_url,
                PATH_BATCH_ORDERS,
                "POST",
                &body,
                &self.signer,
            ) {
                Ok(resp) => {
                    let data =
                        resp.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                    for (r, _) in chunk {
                        let row = data.iter().find(|d| {
                            d.get("clOrdId").and_then(|c| c.as_str())
                                == Some(r.client_order_id.as_str())
                        });
                        match row {
                            Some(d) if d.get("sCode").and_then(|s| s.as_str()) == Some("0") => {
                                events.push(Event::OrderAccepted(OrderAccepted {
                                    client_order_id: r.client_order_id.clone(),
                                    venue_order_id: d
                                        .get("ordId")
                                        .and_then(|o| o.as_str())
                                        .map(Into::into),
                                    ts: r.ts,
                                }));
                            }
                            Some(d) => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: d
                                    .get("sMsg")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("batch reject")
                                    .into(),
                                ts: r.ts,
                            })),
                            None => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: "no batch ack".to_string().into(),
                                ts: r.ts,
                            })),
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                    for (r, _) in chunk {
                        events.push(self.resolve_ambiguous_submit(r));
                    }
                }
                Err(exc) => {
                    for (r, _) in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            client_order_id: r.client_order_id.clone(),
                            reason: exc.msg.clone().into(),
                            ts: r.ts,
                        }));
                    }
                }
            }
        }
        events
    }

    /// Native batch-cancel (POST /api/v5/trade/cancel-batch-orders — JSON ARRAY body, ≤20/req).
    /// Per-order "already gone" is non-fatal (the authoritative OrderCanceled arrives on the
    /// user-data WS, mirroring cancel_order's idempotence); a whole-request/network failure returns
    /// the first error. RUST-NATIVE.
    pub fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in client_order_ids.chunks(BATCH_MAX) {
            let body_orders: Vec<Value> =
                chunk.iter().map(|c| json!({ "instId": self.symbol, "clOrdId": c })).collect();
            let body = serde_json::to_string(&body_orders).expect("array body");
            if let Err(e) = self.transport.signed_json(
                &self.base_url,
                PATH_CANCEL_BATCH,
                "POST",
                &body,
                &self.signer,
            ) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// The stop-algo body, pure (the algo twin of [`Self::build_order_params`], extracted so the
    /// wire shape is pinnable without a transport). Base params byte-identical to the historical
    /// inline vec; a requested `trigger_by` appends `slTriggerPxType` off okx's
    /// `vike_bridge_core::trigger::venue_trigger_by` row (`last`/`mark`/`index` — all three
    /// express, nothing to deny). `None` emits no param — the venue default (`last`) rules,
    /// byte-identical to before the field existed.
    fn build_stop_algo_params(&self, request: &OrderRequest, td_mode: &str) -> Vec<(&str, Value)> {
        let mut params: Vec<(&str, Value)> = vec![
            ("instId", json!(self.symbol)),
            ("tdMode", json!(td_mode)),
            ("side", json!(if request.side > 0 { "buy" } else { "sell" })),
            ("ordType", json!("conditional")),
            (
                "sz",
                json!(format_to_step_f(self.to_contracts(request.qty), self.properties.step_size)),
            ),
            ("posSide", json!("net")),
            ("reduceOnly", json!(request.reduce_only)),
            ("algoClOrdId", json!(request.client_order_id)),
            (
                "slTriggerPx",
                json!(format_to_step_f(
                    request.trigger_price.unwrap_or(0.0),
                    self.properties.tick_size
                )),
            ),
            ("slOrdPx", json!("-1")), // -1 = fire as a market order on trigger
        ];
        if let Some(wire) = request
            .trigger_by
            .and_then(|s| vike_bridge_core::trigger::venue_trigger_by(VENUE, s).wire())
        {
            params.push(("slTriggerPxType", json!(wire)));
        }
        params
    }

    /// Native conditional STOP (a bracket stop-loss leg) via /api/v5/trade/order-algo — OKX places
    /// stops as ALGO orders, not on the regular /order endpoint. `ordType:"conditional"` with an
    /// `slTriggerPx` and a market `slOrdPx` ("-1"); `algoClOrdId` carries the coid so the accept
    /// maps back. Returns [Submitted, Accepted(algoId)|Rejected]. RUST-NATIVE, no Python twin.
    pub fn submit_stop_algo(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // Same margin-mode gate as the regular order path (unset = the historical "cross").
        let td_mode = match swap_td_mode(request.margin_mode) {
            Ok(m) => m,
            Err(reason) => {
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: reason.into(),
                    ts: request.ts,
                }));
                return events;
            }
        };
        let params = self.build_stop_algo_params(request, td_mode);
        match self.call(PATH_ORDER_ALGO, "POST", &params) {
            Ok(data) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: data
                    .as_array()
                    .and_then(|d| d.first())
                    .and_then(|d0| d0.get("algoId"))
                    .and_then(|a| a.as_str())
                    .map(Into::into),
                ts: request.ts,
            })),
            // audit T1: a stop-algo is keyed by algoClOrdId on the order-algo endpoint, NOT queryable
            // by clOrdId — so an ambiguous timeout resolves OPTIMISTICALLY (never a false terminal;
            // authoritative state still arrives on the user-data WS). Precise algo requery is a
            // follow-up (see the OKX stop-algo lifecycle audit finding).
            Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                events.push(Event::OrderAccepted(OrderAccepted {
                    client_order_id: request.client_order_id.clone(),
                    venue_order_id: None,
                    ts: request.ts,
                }));
            }
            Err(exc) => events.push(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: exc.msg.into(),
                ts: request.ts,
            })),
        }
        events
    }

    /// USDT TOTAL cash (`cashBal`, free + frozen — matches Bybit walletBalance / Binance
    /// balance). Default-safe: any failure → 0.0.
    fn fetch_usdt_balance(&self) -> f64 {
        let Ok(result) = self.call(PATH_ACCOUNT, "GET", &[]) else {
            return 0.0;
        };
        for acct in result.as_array().unwrap_or(&vec![]) {
            for d in acct.get("details").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
                if d.get("ccy").and_then(|c| c.as_str()) == Some("USDT") {
                    return d.get("cashBal").and_then(json_num).unwrap_or(0.0);
                }
            }
        }
        0.0
    }

    /// Signed GET /api/v5/trade/orders-pending {instType:SWAP, instId} → the venue's resting orders
    /// as reconcile-seeded [`ManagedOrder`]s (`sz` CONTRACTS → base via `ct_val`, so the seeded qty
    /// matches how the order was submitted). Perps previously returned an EMPTY `open_orders`, so
    /// `apply_snapshot`'s stale-order reap could never fire on a perp; this closes that. Best-effort
    /// by contract: a REST hiccup returns `Vec::new()` (skip the order-fetch, KEEP the position
    /// snapshot) rather than failing the whole reconcile — the "REST hiccup ⇒ skip tick" convention.
    /// Reconcile path only, already off the fold.
    fn fetch_open_orders(&self) -> Vec<ManagedOrder> {
        let Ok(data) = self.call(
            PATH_ORDERS_PENDING,
            "GET",
            &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
        ) else {
            return Vec::new();
        };
        data.as_array()
            .map(|rows| rows.iter().map(|o| map_okx_open_order(o, self.ct_val)).collect())
            .unwrap_or_default()
    }

    /// GET positions: `pos` is signed CONTRACTS → base via ct_val. net → one BOTH leg;
    /// hedge long/short rows carry sides. Flat → zero row.
    pub fn reconcile_positions(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let data = self.call(
            PATH_POSITIONS,
            "GET",
            &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
        )?;
        let bal = self.fetch_usdt_balance();
        // Best-effort resting-order fetch (empty on a REST hiccup) — feeds apply_snapshot's
        // stale-order reap. Fetched once; moved into whichever return path fires.
        let open_orders = self.fetch_open_orders();
        // (base_qty, avg, mark, side, reported margin mode + isolated margin balance)
        type Leg = (f64, f64, f64, String, (vike_model::MarginMode, Option<f64>));
        let mut legs: Vec<Leg> = Vec::new();
        for p in data.as_array().unwrap_or(&vec![]) {
            let contracts = p.get("pos").and_then(json_num).unwrap_or(0.0); // already signed
            if contracts == 0.0 {
                continue;
            }
            let side_lbl = match p.get("posSide").and_then(|s| s.as_str()).unwrap_or("net") {
                "long" => "LONG",
                "short" => "SHORT",
                _ => "BOTH",
            };
            legs.push((
                self.to_base(contracts),
                p.get("avgPx").and_then(json_num).unwrap_or(0.0),
                p.get("markPx").and_then(json_num).unwrap_or(0.0),
                side_lbl.to_string(),
                // `mgnMode`/`margin` off the SAME row — the shared read
                // `recon_client::parse_mgn_mode` fixture-tests. The REQUEST side honors
                // `OrderRequest.margin_mode` via `swap_td_mode` (unset = "cross").
                crate::recon_client::parse_mgn_mode(p),
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
        let hedge = legs.iter().any(|(_, _, _, sd, _)| sd != "BOTH");
        Ok(ReconcileSnapshot {
            positions: legs.iter().map(|(q, ..)| (self.symbol.clone(), *q)).collect(),
            open_orders,
            position_avg_px: legs.iter().map(|(_, a, ..)| (self.symbol.clone(), *a)).collect(),
            position_mark_px: legs.iter().map(|(_, _, m, ..)| (self.symbol.clone(), *m)).collect(),
            position_sides: if hedge {
                legs.iter().map(|(_, _, _, sd, _)| (self.symbol.clone(), sd.clone())).collect()
            } else {
                Vec::new()
            },
            balance: bal,
            position_margin: legs
                .iter()
                .map(|(.., (mode, iso))| (self.symbol.clone(), *mode, *iso))
                .collect(),
        })
    }

    pub fn last_price(&self) -> Result<f64, VenueApiError> {
        let data = self.transport.public(
            &self.base_url,
            PATH_TICKER,
            &[("instId", self.symbol.clone())],
        )?;
        let data = unwrap_okx(data)?;
        Ok(data
            .as_array()
            .and_then(|d| d.first())
            .and_then(|t| t.get("last"))
            .and_then(json_num)
            .unwrap_or(0.0))
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
    let price = o.get("px").and_then(|p| p.as_str()).and_then(|p| {
        if matches!(p, "" | "0" | "0.00000000") { None } else { p.parse::<f64>().ok() }
    });
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": o.get("clOrdId").and_then(|c| c.as_str()).unwrap_or(""),
        "venue": "okx",
        "symbol": o.get("instId").and_then(|s| s.as_str()).unwrap_or(""),
        "side": side,
        "qty": contracts * ct_val,
        "order_type": o.get("ordType").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
        "price": price,
    }))
    .expect("static shape");
    let mut mo = ManagedOrder::new(request);
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = o.get("ordId").and_then(|v| v.as_str()).map(str::to_string);
    mo
}

impl<T: OkxTransport + Send> VenueRest for OkxPerpRest<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        // OKX stops are ALGO orders on a different endpoint — route them there
        if request.order_type.eq_ignore_ascii_case("stop") {
            return self.submit_stop_algo(request);
        }
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF okx cannot express (GTD/Day — no such ordType) is a LOUD
        // terminal reject; the wire is never touched, never a silent limit-GTC (pinned in
        // `tests/offline/tif_gate.rs`).
        if let Some(reject) = vike_bridge_core::tif::deny_unsupported_tif(VENUE, request) {
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
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default()
                        .into(),
                ),
                ts: request.ts,
            })),
            // audit T1: an ambiguous timeout may have been accepted — re-query, don't false-reject.
            Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
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
            Err(exc) if NOT_FOUND.contains(&exc.code) => {
                tracing::debug!(target: "vike_okx::perp", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// ACTIVELY confirm one order's status (audit ex1 residual) — REUSES the exact ambiguous-submit
    /// re-query (`query_order_ordid` by clOrdId) + the shared `resolve_ambiguous_submit` mapping, so
    /// no new REST is written: `Ok(Some(id))` → managed `OrderAccepted`, `Ok(None)` → true terminal
    /// reject, `Err` → optimistic accept. Runs on the ExecActor's own confirm worker.
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        vec![vike_bridge_core::resolve_ambiguous_submit(
            client_order_id,
            ts,
            self.query_order_ordid(client_order_id),
        )]
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

#[cfg(test)]
mod margin_mode_tests {
    //! The margin-mode step-2 un-hardcode, offline: `swap_td_mode`'s total mapping, the
    //! byte-identity of the unset/Cross default vs the pre-flip hardcode, and the loud
    //! no-wire deny for Cash (the `NoWire` transport PANICS on any REST call, proving a
    //! denied mode never leaves the process). The golden-fixture pins live in
    //! `tests/offline/r6_okx_parity.rs`; these are the scripted-transport twins.

    use super::*;
    use vike_bridge_core::credentials::Credentials;

    /// A transport that must never be reached: every verb panics. Used to prove the
    /// margin-mode deny happens BEFORE any wire call.
    struct NoWire;
    impl OkxTransport for NoWire {
        fn signed(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, Value)],
            _signer: &OkxV5Signer,
        ) -> Result<Value, VenueApiError> {
            panic!("denied order must not reach the wire (signed)");
        }
        fn signed_json(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _body: &str,
            _signer: &OkxV5Signer,
        ) -> Result<Value, VenueApiError> {
            panic!("denied order must not reach the wire (signed_json)");
        }
        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            panic!("denied order must not reach the wire (public)");
        }
    }

    fn client() -> OkxPerpRest<NoWire> {
        OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: NoWire,
            base_url: "https://stub".into(),
            symbol: "BTC-USDT-SWAP".into(),
            properties: SymbolProperties {
                tick_size: 0.1,
                step_size: 0.01,
                min_qty: 0.01,
                ..Default::default()
            },
            ct_val: 0.01,
            leverage: 2.0,
            broker_code: None,
        }
    }

    fn limit_req(mode: Option<MarginMode>) -> OrderRequest {
        OrderRequest {
            client_order_id: "vtMgnT1".into(),
            venue: "okx".into(),
            symbol: "BTC-USDT-SWAP".into(),
            side: 1,
            qty: 0.0002,
            order_type: "limit".into(),
            price: Some(30000.0),
            margin_mode: mode,
            ..Default::default()
        }
    }

    #[test]
    fn swap_td_mode_total_mapping() {
        assert_eq!(swap_td_mode(None).unwrap(), "cross", "unset = the pre-flip hardcode");
        assert_eq!(swap_td_mode(Some(MarginMode::Cross)).unwrap(), "cross");
        assert_eq!(swap_td_mode(Some(MarginMode::Isolated)).unwrap(), "isolated");
        let err = swap_td_mode(Some(MarginMode::Cash)).unwrap_err();
        assert!(err.contains("cash"), "{err}");
    }

    /// THE ADAPTER TIE for `VenueCaps::default_margin_mode` — the reason that field moved out of
    /// the venue-OFFERS table and into the enforced one.
    ///
    /// Every other field on the margin axis is vendor-doc transcription with nothing in the repo
    /// to check it against. `default_margin_mode` is different in kind: it claims something about
    /// bytes THIS adapter emits, and okx is the one venue whose builder actually emits them
    /// (`swap_td_mode` → `tdMode`). So the declaration is asserted against the REAL builder rather
    /// than against another declaration — the shape
    /// `crates/bridges/deribit/src/exec.rs`'s `modify_is_default_noop_for_deribit` uses to tie
    /// `supports_modify` to a real `rest.modify_order` call.
    ///
    /// What is proven here, precisely: the mode the table NAMES as vike's default maps through the
    /// real builder to the exact wire value an UNSET request produces. Declaring `Isolated` while
    /// the builder still emits `"cross"` fails; changing the builder's unset arm to `"isolated"`
    /// while the table still says `Cross` fails too. Neither side can move alone.
    #[test]
    fn okx_default_margin_mode_matches_the_td_mode_builder() {
        let declared = vike_model::caps_for(VENUE).default_margin_mode;

        // The declared default, driven through the real builder, is what an unset request sends.
        assert_eq!(
            swap_td_mode(Some(declared)).expect("declared default must be a mode okx accepts"),
            swap_td_mode(None).expect("unset always builds"),
            "okx: default_margin_mode must name the mode an unset request actually wires"
        );

        // ...and that is `"cross"` today — spelled out so the pair above cannot pass by both
        // sides drifting together.
        assert_eq!(swap_td_mode(None).unwrap(), "cross");
        assert_eq!(declared, MarginMode::Cross);

        // The same value, through the FULL order path rather than the helper alone: tdMode is the
        // second param and carries exactly the declared default.
        let params = client().build_order_params(&limit_req(None), None).expect("unset builds");
        assert_eq!(params[1], ("tdMode", json!(swap_td_mode(Some(declared)).unwrap())));

        // Intra-table: an explicit request for the default is preflight-honorable.
        assert!(vike_model::caps_for(VENUE).margin_modes.contains(&declared));
    }

    /// BYTE-IDENTITY: an unset margin_mode builds EXACTLY the params it always built (tdMode
    /// "cross"), and an explicit Cross is indistinguishable from unset. Isolated flips ONLY the
    /// tdMode pair.
    #[test]
    fn unset_and_cross_are_byte_identical_isolated_flips_only_tdmode() {
        let c = client();
        let unset = c.build_order_params(&limit_req(None), None).expect("unset builds");
        let cross =
            c.build_order_params(&limit_req(Some(MarginMode::Cross)), None).expect("cross builds");
        assert_eq!(unset, cross, "explicit Cross must equal the unset default");
        assert_eq!(unset[1], ("tdMode", json!("cross")));

        let iso = c
            .build_order_params(&limit_req(Some(MarginMode::Isolated)), None)
            .expect("isolated builds");
        assert_eq!(iso[1], ("tdMode", json!("isolated")));
        for (i, (pair_unset, pair_iso)) in unset.iter().zip(iso.iter()).enumerate() {
            if i != 1 {
                assert_eq!(pair_unset, pair_iso, "only tdMode may differ (index {i})");
            }
        }
        assert_eq!(unset.len(), iso.len());
    }

    /// Unified cross-venue attribution (task 4): a configured FD-broker code stamps `tag` onto the
    /// wire body; absent, the key never appears at all — byte-identical to before `broker_code`
    /// existed.
    #[test]
    fn tag_is_stamped_when_broker_code_present_and_absent_otherwise() {
        let c = client();
        let with = c
            .build_order_params(&limit_req(None), Some("5328c82e5542BCDE"))
            .expect("with-code builds");
        assert!(with.iter().any(|(k, v)| *k == "tag" && v == &json!("5328c82e5542BCDE")));
        let without = c.build_order_params(&limit_req(None), None).expect("without-code builds");
        assert!(without.iter().all(|(k, _)| *k != "tag"));
    }

    #[test]
    fn cash_never_reaches_the_wire_on_submit() {
        let c = client();
        let events = c.submit_order(&limit_req(Some(MarginMode::Cash)));
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], Event::OrderSubmitted(_)));
        match &events[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, "vtMgnT1");
                assert!(r.reason.contains("cash"), "{}", r.reason);
            }
            other => panic!("want OrderRejected, got {other:?}"),
        }
    }

    #[test]
    fn cash_never_reaches_the_wire_on_stop_algo() {
        let c = client();
        let mut req = limit_req(Some(MarginMode::Cash));
        req.order_type = "stop".into();
        req.trigger_price = Some(25000.0);
        let events = c.submit_order(&req); // routes to submit_stop_algo
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], Event::OrderSubmitted(_)));
        assert!(matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("cash")));
    }

    /// Trigger-source pins on the pure stop-algo body: `None` emits NO `slTriggerPxType`
    /// (byte-identical to the pre-`trigger_by` params — the venue default `last` rules); a
    /// requested source appends the authority row's wire string as the tail param, base
    /// params untouched.
    #[test]
    fn stop_algo_trigger_by_maps_and_default_stays_bare() {
        let c = client();
        let stop = |tb: Option<vike_model::TriggerBy>| {
            let mut r = limit_req(None);
            r.order_type = "stop".into();
            r.price = None;
            r.trigger_price = Some(25000.0);
            r.trigger_by = tb;
            r
        };
        let bare = c.build_stop_algo_params(&stop(None), "cross");
        assert!(
            bare.iter().all(|(k, _)| *k != "slTriggerPxType"),
            "None = venue default, no param"
        );
        assert_eq!(bare.last().unwrap().0, "slOrdPx", "unchanged tail");
        for (tb, wire) in [
            (vike_model::TriggerBy::Last, "last"),
            (vike_model::TriggerBy::Mark, "mark"),
            (vike_model::TriggerBy::Index, "index"),
        ] {
            let params = c.build_stop_algo_params(&stop(Some(tb)), "cross");
            assert_eq!(params.last().unwrap(), &("slTriggerPxType", json!(wire)), "{tb:?}");
            assert_eq!(&params[..params.len() - 1], &bare[..], "{tb:?}: base params untouched");
        }
    }

    #[test]
    fn batch_of_denied_modes_rejects_all_without_wire() {
        let c = client();
        let mut r2 = limit_req(Some(MarginMode::Cash));
        r2.client_order_id = "vtMgnT2".into();
        let events = OkxPerpRest::submit_batch(&c, &[limit_req(Some(MarginMode::Cash)), r2]);
        // 2 Submitted then 2 Rejected — no chunk is ever sent (NoWire would panic).
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(matches!(events[0], Event::OrderSubmitted(_)));
        assert!(matches!(events[1], Event::OrderSubmitted(_)));
        assert!(matches!(&events[2], Event::OrderRejected(r) if r.client_order_id == "vtMgnT1"));
        assert!(matches!(&events[3], Event::OrderRejected(r) if r.client_order_id == "vtMgnT2"));
    }
}

#[cfg(test)]
mod properties_unit_tests {
    use super::*;

    /// ⚠ **`to_contracts` and the pre-trade gate measure `qty` in DIFFERENT UNITS**, and this pins
    /// the conversion between them.
    ///
    /// `parse_okx_perp_instruments` fills `step_size`/`min_qty`/`max_qty` from `lotSz`/`minSz`/
    /// `maxMktSz`, which count CONTRACTS — correct for `to_contracts`, which divides a BASE qty by
    /// `ct_val` and floors on exactly that grid. But `OrderRequest.qty` is BASE, so a consumer that
    /// hands the raw grid to `vike_exec::RiskLimits::from_properties` floors base quantities on a
    /// contracts step. `vike-mount`'s okx arm did precisely that.
    ///
    /// The numbers below are BTC-USDT-SWAP's real shape (`ct_val` 0.01 BTC): the gate was applying
    /// a 0.01 BTC step where the venue's true base step is 0.0001 BTC — 100x too coarse — and a
    /// 0.01 BTC minimum where the real one is 0.0001 BTC.
    #[test]
    fn the_gate_grid_is_scaled_from_contracts_to_base() {
        let contracts_grid = SymbolProperties {
            tick_size: 0.1,    // quote per base — must NOT scale
            step_size: 0.01,   // lotSz, CONTRACTS
            min_qty: 0.01,     // minSz, CONTRACTS
            max_qty: 1_000.0,  // maxMktSz, CONTRACTS
            min_notional: 5.0, // quote — must NOT scale
            ..Default::default()
        };
        let base = properties_in_base(&contracts_grid, 0.01);

        assert_eq!(base.step_size, 0.0001, "lotSz 0.01 contracts x 0.01 BTC = 0.0001 BTC");
        assert_eq!(base.min_qty, 0.0001, "minSz 0.01 contracts x 0.01 BTC = 0.0001 BTC");
        assert_eq!(base.max_qty, 10.0, "maxMktSz 1000 contracts x 0.01 BTC = 10 BTC");
        // ⚠ The two fields that must NOT move. Scaling either would be the same bug inverted.
        assert_eq!(base.tick_size, 0.1, "tick_size is quote-per-base, not a quantity");
        assert_eq!(base.min_notional, 5.0, "min_notional is quote, not a quantity");
    }

    /// The scaled grid must be the EXACT inverse of the wire conversion: `to_contracts` divides a
    /// base qty by `ct_val` and floors on the contracts step, so one base step must be exactly one
    /// contracts lot. That round trip is the whole law, stated here as an identity.
    ///
    /// ⚠ It is asserted on the ARITHMETIC rather than by calling `to_contracts`, because that method
    /// lives on `OkxPerpRest` and needs a signer, a transport and a base URL — constructing one here
    /// would test the harness. The identity below is what `to_contracts`'s `raw / ct / step` depends
    /// on, and `properties_in_base` is its only other consumer.
    ///
    /// NON-VACUOUS: on the raw grid the gate's step is 0.01 BTC, so a 0.0001 BTC order rounds to
    /// zero — the ratio asserted here is exactly the 100x the bug applied.
    #[test]
    fn one_base_step_is_exactly_one_contracts_lot() {
        let ct_val = 0.01;
        let contracts = SymbolProperties {
            step_size: 0.01,
            min_qty: 0.01,
            max_qty: 1_000.0,
            ..Default::default()
        };
        let base = properties_in_base(&contracts, ct_val);

        assert_eq!(base.step_size / ct_val, contracts.step_size, "one base step == one lot");
        assert_eq!(base.min_qty / ct_val, contracts.min_qty, "the base minimum == minSz lots");
        assert_eq!(base.max_qty / ct_val, contracts.max_qty, "the base maximum == maxMktSz lots");
        // ...and the scaling is a genuine change, not an identity that would make this vacuous.
        assert!(base.step_size < contracts.step_size, "ct_val < 1 must SHRINK the base step");
    }

    /// A degenerate `ct_val` returns the grid unchanged — today's behaviour — rather than zeroing
    /// the step (which would make every order infinitely divisible) or producing NaN.
    #[test]
    fn a_degenerate_ct_val_leaves_the_grid_alone() {
        let g = SymbolProperties { step_size: 0.01, min_qty: 0.01, ..Default::default() };
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let out = properties_in_base(&g, bad);
            assert_eq!(out.step_size, g.step_size, "ct_val {bad} must not alter the grid");
            assert_eq!(out.min_qty, g.min_qty, "ct_val {bad} must not alter the grid");
        }
    }
}
