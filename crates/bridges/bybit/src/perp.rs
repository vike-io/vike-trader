//! BybitPerpRest — signed V5 LINEAR perp submit/cancel/reconcile + set-leverage. Exact
//! port of `exec/bybit/perp_client.py` (+ the shared crypto_client flow + spot unwrap).
//!
//! Linear deltas: category=linear, positionIdx=0 (one-way; hedge 1/2 is 5f), reduceOnly
//! as a JSON BOOL, qtyStep filters, set-leverage swallowing 110043 ("leverage not
//! modified"), and /v5/position/list signed reconcile — size is UNSIGNED, the SIGN comes
//! from side Buy/Sell; hedge legs from positionIdx 1/2.

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_exec::{ManagedOrder, OrderStatus, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, SymbolProperties};

use crate::transport::{BybitTransport, unwrap_envelope};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
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
/// `vike_mount::server_time`'s bybit row: bybit stamps `X-BAPI-TIMESTAMP` on every private request
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

/// `BYBIT_MAINNET` env-flag name — the exact-`"1"` opt-in that flips this venue's PRIVATE exec/WS/
/// recon path from the demo host onto MAINNET (real funds). Parsed by the ONE converged workspace
/// rule (STEP 2, see `vike_bridge_core::mainnet`'s module doc): the EXACT string `"1"`, read from
/// the real process env OR the workspace `.env` map, process env winning. ⚠ STEP 2 CHANGED this
/// venue — a `BYBIT_MAINNET=1` line in the workspace `.env` used to parse as UNSET here (the `.env`
/// is never exported to process env) and silently kept bybit on the demo host; it now ARMS mainnet,
/// exactly as an exported flag does. Default (unset in BOTH sources) stays on the demo host,
/// byte-identical to before this switch existed. The const AND both reads stay HERE, resolvable by
/// the settings-registry gate; only the PARSE is shared (see `vike_bridge_core::mainnet`'s
/// env-boundary note).
pub const MAINNET_ENV: &str = "BYBIT_MAINNET";

/// Pure `BYBIT_MAINNET` predicate over the two already-read source values (`process` = process env,
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
/// `BYBIT_MAINNET` through the const.
///
/// Called ONCE per mount (`vike_mount::make_engine`); the resolved `bool` is then threaded into
/// every adapter site that needs it ([`endpoints`], the grid pre-fetch, the exec spawn, the funding
/// poller, the recon client) instead of each spawned thread re-reading global env for itself.
pub fn mainnet_enabled(vars: &std::collections::HashMap<String, String>) -> bool {
    mainnet_from(
        std::env::var(MAINNET_ENV).ok().as_deref(),
        vars.get(MAINNET_ENV).map(String::as_str),
    )
}

/// The `(rest_base, ws_base)` pair this venue's private exec/user-data path binds to, selected by
/// the resolved mainnet flag. **Default-safe:** `false` ⇒ the exact demo hosts bybit used before
/// this switch existed; `true` ⇒ mainnet (real funds). Bybit has no `x-simulated-trading`-style
/// header — demo is a wholly separate host — so switching the host is the entire change.
pub fn endpoints(mainnet: bool) -> (&'static str, &'static str) {
    if mainnet { (MAINNET_REST, MAINNET_WS) } else { (DEMO_REST, DEMO_WS) }
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

    /// Flag unset ⇒ the demo host pair binance/bybit used before this existed (byte-identical);
    /// flag set ⇒ the mainnet pair.
    #[test]
    fn endpoints_default_is_demo_set_is_mainnet() {
        assert_eq!(endpoints(false), (DEMO_REST, DEMO_WS));
        assert_eq!(endpoints(true), (MAINNET_REST, MAINNET_WS));
        // Pin the concrete demo hosts so a URL edit can't silently move the default path.
        assert_eq!(endpoints(false).0, "https://api-demo.bybit.com");
        assert_eq!(endpoints(false).1, "wss://stream-demo.bybit.com/v5/private");
        // Pin the mainnet hosts (from Bybit's v5 REST/WS docs).
        assert_eq!(endpoints(true).0, "https://api.bybit.com");
        assert_eq!(endpoints(true).1, "wss://stream.bybit.com/v5/private");
    }

    /// This venue HAS a row in the shared per-venue switch table, so [`mainnet_from`]'s fold is
    /// actually gated by it (a switchless venue can never be armed). Removing bybit's row would
    /// silently pin this venue to demo forever — this pin makes that a deliberate edit.
    #[test]
    fn bybit_has_a_row_in_the_shared_switch_table() {
        assert_eq!(
            vike_bridge_core::mainnet::mainnet_switch_for(VENUE),
            Some(vike_bridge_core::mainnet::MainnetSwitch)
        );
    }
}

/// order-not-exists / too-late-to-cancel — swallowed by cancel (unknown ≠ rejection)
const NOT_FOUND: [i64; 2] = [110001, 170213];
const LEVERAGE_NOT_MODIFIED: i64 = 110043; // already at target — benign

/// Twin of `parse_bybit_perp_instruments`: qtyStep (NOT basePrecision) +
/// minNotionalValue (NOT minOrderAmt), plus base_asset from baseCoin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BybitInstrument {
    pub properties: SymbolProperties,
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
}

pub fn parse_bybit_perp_instruments(payload: &Value) -> IndexMap<String, BybitInstrument> {
    let mut out = IndexMap::new();
    let rows = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array());
    for entry in rows.unwrap_or(&vec![]) {
        let symbol = entry.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let num = |section: &str, key: &str| -> f64 {
            entry.get(section).and_then(|d| d.get(key)).and_then(json_num).unwrap_or(0.0)
        };
        out.insert(
            symbol,
            BybitInstrument {
                properties: SymbolProperties {
                    tick_size: num("priceFilter", "tickSize"),
                    step_size: num("lotSizeFilter", "qtyStep"),
                    min_qty: num("lotSizeFilter", "minOrderQty"),
                    max_qty: num("lotSizeFilter", "maxOrderQty"),
                    min_notional: num("lotSizeFilter", "minNotionalValue"),
                    // Everything else absent: Bybit LINEAR perps are 1:1 with the base asset
                    // (multiplier 1.0), `priceFilter` reports ONE `tickSize` (flat grid, no
                    // tiers), and no venue taker hold. FRU rather than an exhaustive literal so a
                    // new `SymbolProperties` field costs this parser nothing.
                    ..Default::default()
                },
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
            },
        );
    }
    out
}

#[cfg(test)]
mod leverage_cap_tests {
    use super::*;
    use serde_json::json;

    /// The REAL `/v5/market/instruments-info?category=linear&symbol=BTCUSDT` row shape, captured
    /// live 2026-08-05 (trimmed to the keys this parser reads plus the leverage filter). Note
    /// `maxLeverage` is a decimal STRING with trailing zeros — `json_num` handles it.
    fn live_row() -> serde_json::Value {
        json!({"result": {"list": [{
            "symbol": "BTCUSDT",
            "contractType": "LinearPerpetual",
            "status": "Trading",
            "baseCoin": "BTC",
            "quoteCoin": "USDT",
            "leverageFilter": {"minLeverage": "1", "maxLeverage": "100.00", "leverageStep": "0.01"},
            "priceFilter": {"minPrice": "0.10", "maxPrice": "1999999.80", "tickSize": "0.10"},
            "lotSizeFilter": {
                "maxOrderQty": "1500.000", "minOrderQty": "0.001", "qtyStep": "0.001",
                "postOnlyMaxOrderQty": "1500.000", "maxMktOrderQty": "150.000",
                "minNotionalValue": "5"
            }
        }]}})
    }

    /// The cap is parsed out of the response the exec thread ALREADY fetches — the whole reason
    /// bybit clamps for free — and the rest of the grid is untouched by its arrival.
    #[test]
    fn parses_max_leverage_from_the_live_instruments_info_shape() {
        let instruments = parse_bybit_perp_instruments(&live_row());
        let inst = &instruments["BTCUSDT"];
        assert_eq!(inst.max_leverage, Some(100.0), "leverageFilter.maxLeverage \"100.00\"");
        // …and the pre-existing fields are exactly what they were before the field existed.
        assert_eq!(inst.properties.tick_size, 0.10);
        assert_eq!(inst.properties.step_size, 0.001);
        assert_eq!(inst.properties.min_qty, 0.001);
        assert_eq!(inst.properties.max_qty, 1500.0);
        assert_eq!(inst.properties.min_notional, 5.0);
        assert_eq!(inst.base_asset, "BTC");
    }

    /// A non-integer ceiling round-trips verbatim — bybit's `leverageStep` is `0.01`, so a
    /// fractional cap is a real venue value, and truncating it would clamp BELOW what the venue
    /// allows.
    #[test]
    fn a_fractional_cap_is_not_truncated() {
        let payload = json!({"result": {"list": [
            {"symbol": "ALTUSDT", "baseCoin": "ALT", "leverageFilter": {"maxLeverage": "12.50"}}
        ]}});
        assert_eq!(parse_bybit_perp_instruments(&payload)["ALTUSDT"].max_leverage, Some(12.5));
    }

    /// Absent / malformed ⇒ `None` (UNKNOWN), never `0.0`. `None` is what makes the clamp a no-op,
    /// so this is the arm that keeps a venue-shape change from silently de-leveraging an account.
    #[test]
    fn an_absent_or_malformed_leverage_filter_is_unknown() {
        let payload = json!({"result": {"list": [
            // no leverageFilter at all (the shape the r6 golden fixture carries)
            {"symbol": "NOFILTER", "baseCoin": "N", "lotSizeFilter": {"qtyStep": "0.01"}},
            // present but empty
            {"symbol": "EMPTYFILTER", "baseCoin": "E", "leverageFilter": {}},
            // present, unparseable
            {"symbol": "JUNK", "baseCoin": "J", "leverageFilter": {"maxLeverage": "n/a"}},
            // present, null
            {"symbol": "NULLCAP", "baseCoin": "X", "leverageFilter": {"maxLeverage": null}},
        ]}});
        let got = parse_bybit_perp_instruments(&payload);
        for sym in ["NOFILTER", "EMPTYFILTER", "JUNK", "NULLCAP"] {
            assert_eq!(got[sym].max_leverage, None, "{sym} must be UNKNOWN, not 0.0");
        }
    }

    /// End to end at the seam that matters: the parsed cap, fed to the shared clamp, is exactly
    /// `min(requested, cap)` — and a row without one changes nothing.
    #[test]
    fn the_parsed_cap_drives_the_shared_clamp() {
        use vike_bridge_core::leverage::clamp_to_venue_cap;
        let capped = parse_bybit_perp_instruments(&live_row())["BTCUSDT"].max_leverage;
        assert_eq!(clamp_to_venue_cap(2.0, capped, VENUE, "BTCUSDT"), 2.0, "under the cap");
        assert_eq!(clamp_to_venue_cap(150.0, capped, VENUE, "BTCUSDT"), 100.0, "clamped to 100x");
        let payload = json!({"result": {"list": [{"symbol": "NOCAP", "baseCoin": "N"}]}});
        let unknown = parse_bybit_perp_instruments(&payload)["NOCAP"].max_leverage;
        assert_eq!(
            clamp_to_venue_cap(150.0, unknown, VENUE, "NOCAP"),
            150.0,
            "unknown ⇒ unchanged"
        );
    }
}

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
    /// both endpoints confirm absent → the true terminal reject; `Err` = the query itself failed →
    /// optimistic accept upstream, never a false terminal. The history fallback rides the same
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
        let ot = request.order_type.to_ascii_lowercase();
        let is_limit = ot == "limit";
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
            match vike_bridge_core::tif::venue_tif(VENUE, request.time_in_force) {
                vike_bridge_core::tif::TifOutcome::Mapped(w)
                | vike_bridge_core::tif::TifOutcome::Coerced { wire: w, .. } => {
                    params.push(("timeInForce", json!(w)));
                }
                vike_bridge_core::tif::TifOutcome::Ignored { wire } => {
                    params.push(("timeInForce", json!(wire)));
                }
                vike_bridge_core::tif::TifOutcome::NotEmitted
                | vike_bridge_core::tif::TifOutcome::Unsupported => {}
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
        if ot == "stop" {
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
    /// `[OrderModified]` on the venue's ack; `[]` on failure (order keeps its terms). RUST-NATIVE,
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
            Err(_) => Vec::new(),
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
            match vike_bridge_core::tif::deny_unsupported_tif(VENUE, r) {
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
                        let msg = envelope
                            .get("retMsg")
                            .and_then(|m| m.as_str())
                            .unwrap_or("batch reject")
                            .to_string();
                        for r in chunk {
                            events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: msg.clone().into(),
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
                        } else {
                            events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: ext_list
                                    .get(i)
                                    .and_then(|e| e.get("msg"))
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("batch order reject")
                                    .into(),
                                ts: r.ts,
                            }));
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                    for r in chunk {
                        events.push(self.resolve_ambiguous_submit(r));
                    }
                }
                Err(exc) => {
                    for r in chunk {
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
        let mut legs: Vec<(f64, f64, f64, String, vike_model::MarginMode)> = Vec::new();
        for p in result.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]) {
            let size = p.get("size").and_then(json_num).unwrap_or(0.0).abs();
            if size == 0.0 {
                continue;
            }
            let idx = match p.get("positionIdx") {
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                Some(Value::String(x)) => x.parse::<i64>().unwrap_or(0),
                _ => 0,
            };
            let side_lbl = match idx {
                1 => "LONG",
                2 => "SHORT",
                _ => "BOTH", // 0/other -> one-way BOTH leg
            };
            let sign =
                if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
            legs.push((
                sign * size,
                p.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                p.get("markPrice").and_then(json_num).unwrap_or(0.0),
                side_lbl.to_string(),
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

impl<T: BybitTransport + Send> VenueRest for BybitPerpRest<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF bybit cannot express (GTD/Day) is a LOUD terminal reject —
        // the wire is never touched, never a silent GTC (pinned in `tests/offline/tif_gate.rs`).
        if let Some(reject) = vike_bridge_core::tif::deny_unsupported_tif(VENUE, request) {
            events.push(reject);
            return events;
        }
        let params = self.build_order_params(request);
        match self.call(PATH_ORDER_CREATE, "POST", &params) {
            Ok(result) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some(
                    result
                        .get("orderId")
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
                tracing::warn!(target: "vike_bybit::perp", code = exc.code, msg = %exc.msg, "submit rejected");
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

    /// ACTIVELY confirm one order's status (audit ex1 residual) — REUSES the exact ambiguous-submit
    /// re-query (`query_order_orderlinkid` realtime→history) + the shared `resolve_ambiguous_submit`
    /// mapping, so no new REST is written: `Ok(Some(id))` → managed `OrderAccepted`, `Ok(None)` →
    /// true terminal reject, `Err` → optimistic accept. Runs on the ExecActor's own confirm worker.
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        vec![vike_bridge_core::resolve_ambiguous_submit(
            client_order_id,
            ts,
            self.query_order_orderlinkid(client_order_id),
        )]
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
