//! AsterSpotRest — signed spot REST submit/cancel/connect. Ported from `vike-binance`'s
//! `spot.rs`: Aster's spot API is a Binance-spot fork, so the endpoint paths, order params, and
//! `exchangeInfo` filter shapes are byte-identical to Binance's `/api/v3/*`. The two differences
//! from the template are (1) auth — generic over `S: Signer` here, with the concrete EIP-712
//! `AsterSigner` injected by `exec.rs` instead of Binance's HMAC signer, and (2) the base URL,
//! which arrives via the `base_url` field (set by `exec::mk_spot_rest` from `urls::urls_for`)
//! rather than a hardcoded const.
//!
//! The WS executionReport (Aster's listenKey user-data stream) is the SOLE source of fills; the
//! REST POST/DELETE response is ACK-ONLY. `submit_order` returns
//! [OrderSubmitted, OrderAccepted|OrderRejected] for the session to pump into the core ingest.
//! qty/price are formatted to the symbol's step/tick as decimal strings to dodge
//! BAD_PRECISION-class rejects.

use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_exec::{ManagedOrder, OrderStatus, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, SymbolProperties};

use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The pure, byte-identical-between-binance/aster mappers live once in the shared
// `vike_binance::family::order_map` (mappers rung): `json_id` (used internally below), and
// `parse_symbol_properties` re-exported so `crate::spot::parse_symbol_properties` stays valid.
use vike_binance::family::order_map::json_id;
pub use vike_binance::family::order_map::parse_symbol_properties;

// Homed in `crate::urls` since the split-plane Phase-5 seam (this module is `exec`-gated, the
// venue id is not); imported here like any other consumer — no `crate::spot::VENUE` re-export
// (no pub-use shims on a move; call sites spell `crate::urls::VENUE`).
use crate::urls::VENUE;
pub const PATH_ORDER: &str = "/api/v3/order";
pub const PATH_OPEN_ORDERS: &str = "/api/v3/openOrders";
/// Aster's spot account-trade-history path. ⚠ **NOT Binance's `/api/v3/myTrades`** — Aster renamed
/// the endpoint to `userTrades` (matching its own fapi naming) and serves NO `myTrades` at all:
/// `GET /api/v3/myTrades` returns a plain 404 HTML page on `sapi.asterdex.com`, while
/// `/api/v3/userTrades` reaches the v3 signature layer like every other signed path here.
///
/// Source: <https://github.com/asterdex/api-docs> — `V3(Recommended)/EN/aster-finance-spot-api-v3.md`,
/// § "Account trade history (USER_DATA)", read 2026-08-05; the string `myTrades` does not occur in
/// that document. Confirmed live the same day (unauthenticated GETs against `sapi.asterdex.com`):
/// `/api/v3/myTrades` → 404 HTML, `/api/v3/userTrades` → 400 `{"code":-1102,...'nonce'...}`, the
/// SAME error `/api/v3/account` and `/api/v3/openOrders` give — i.e. the route exists and only the
/// signature was missing.
///
/// ⚠ Its ROW GRAMMAR diverges too — see [`vike_binance::family::recon::fill_side`].
pub const PATH_SPOT_USER_TRADES: &str = "/api/v3/userTrades";
pub const PATH_ACCOUNT: &str = "/api/v3/account";
/// Aster's per-symbol SPOT fee endpoint — the venue's own price list for this account.
///
/// ⚠ **This exists because Aster's [`PATH_ACCOUNT`] body does NOT carry `commissionRates`.**
/// Binance's does, which is why the shared family prices Binance's spot fees straight off the
/// account body it already fetches. Aster's fork dropped the object: measured live 2026-08-05
/// (mainnet, HTTP 200) that body carries exactly `balances`, `canBurnAsset`, `canDeposit`,
/// `canTrade`, `canWithdraw`, `feeTier`, `updateTime` — the only fee-shaped field is a tier INDEX
/// carrying no rate — so the account-body fee parser answered `Ok(None)` on this venue forever.
///
/// `GET /api/v3/commissionRate` (weight 20, `symbol` REQUIRED) returns
/// `{symbol, makerCommissionRate, takerCommissionRate}` as fractions — the same body grammar the
/// perp `commissionRate` endpoint uses, parsed by
/// [`vike_binance::family::recon::parse_commission_rate_body`].
///
/// Source: <https://github.com/asterdex/api-docs> — `V3(Recommended)/EN/aster-finance-spot-api-v3.md`,
/// § "Get symbol fees", read 2026-08-05. Confirmed live the same day against the mainnet account
/// (read-only signed GET, `BTCUSDT`): maker 0.5 bps / taker 4 bps, digit-for-digit Aster's published
/// spot schedule. ⚠ The rate is PER-SYMBOL, not account-wide — that document's own response example
/// prices `APXUSDT` at 2 bps / 7 bps — so this endpoint can legitimately disagree with the flat
/// published schedule on some other symbol. That is a finding to read, not a parser bug.
pub const PATH_COMMISSION_RATE: &str = "/api/v3/commissionRate";
pub const PATH_TICKER: &str = "/api/v3/ticker/price";
pub const PATH_TIME: &str = "/api/v3/time";

/// The REST half of the live spot client (ACK-only); fills come from the listenKey user-data WS.
/// Signer/transport are seams: offline tests stub the transport with canned JSON; the testnet
/// smoke uses `UreqTransport` with the real `AsterSigner`.
pub struct AsterSpotRest<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub base_asset: String,
    /// Unified cross-venue attribution (task 8): the Aster Code `(builder address, feeRate)` pair
    /// stamped onto every order when configured. `None` (the default at every mount lacking
    /// `ASTER_BUILDER_CODE`) means `build_order_params` emits neither `builder` nor `feeRate` —
    /// byte-identical to every order shape before this field existed. See `perp.rs`'s twin field
    /// for the approval helper (`AsterPerpRest::approve_builder`) — the grant is account-wide, not
    /// spot/perp-specific, so one helper on the fapi client covers both.
    pub builder: Option<(String, String)>,
}

impl<S: Signer, T: RestTransport> AsterSpotRest<S, T> {
    /// `build_order_params` hook — pure, golden-gated. Delegates to the shared, byte-identical
    /// [`vike_binance::family::order_map::build_spot_order_params`] (mappers rung), passing this
    /// client's `symbol`/`properties`; `newOrderRespType=ACK` and the response's fills[] is
    /// deliberately ignored (WS is the sole fill source).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        let mut params = vike_binance::family::order_map::build_spot_order_params(
            request,
            &self.symbol,
            &self.properties,
            VENUE, // aster's tif row is still Ignored{GTC} — bytes unchanged (unflipped)
            // Aster does not use the Binance broker-prefix coid mechanic (unified cross-venue
            // attribution, task 6). Aster's OWN attribution mechanic (Aster Code, task 8) is the
            // `self.builder` push below, not this shared family builder's `link_id`.
            None,
        );
        // Unified cross-venue attribution (task 8): Aster Code is a builder address + feeRate pair
        // on the order params (the HL-style builder model on this Binance-fork REST API), NOT a
        // coid prefix. `None` (unconfigured) pushes neither key — byte-identical.
        if let Some((addr, rate)) = &self.builder {
            params.push(("builder", addr.clone()));
            params.push(("feeRate", rate.clone()));
        }
        params
    }

    /// Shared submit flow: Submitted → REST → Accepted|Rejected. The returned events go into the
    /// core ingest — never applied locally.
    pub fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        let params = self.build_order_params(request);
        match self.transport.signed(&self.base_url, PATH_ORDER, "POST", &params, &self.signer) {
            Ok(resp) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some(resp.get("orderId").map(json_id).unwrap_or_default().into()),
                ts: request.ts,
            })),
            // audit T1: an ambiguous timeout may have been accepted at the venue — re-query instead
            // of emitting a false terminal reject (which would leave a phantom position).
            Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                events.push(self.resolve_ambiguous_submit(request));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_aster::spot", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    /// Audit A3 resync: recent order states (`GET /api/v3/allOrders`) for the post-reconnect
    /// history replay. On the short-timeout requery transport so the resync can't stall.
    pub fn get_all_orders(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/api/v3/allOrders",
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit A3 resync: recent fills ([`PATH_SPOT_USER_TRADES`], `GET /api/v3/userTrades`) for the
    /// post-reconnect history replay.
    ///
    /// ⚠ This was the SECOND casualty of the `myTrades` path bug, and the quieter one: `exec.rs`'s
    /// resync calls this with `.unwrap_or_else(|_| json!([]))`, so the 404 was swallowed into an
    /// empty trade array and every post-reconnect spot fill replay silently replayed NOTHING.
    pub fn get_my_trades(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            PATH_SPOT_USER_TRADES,
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit T1: re-query the venue for the order by our idempotent client_order_id to resolve an
    /// ambiguous (timed-out) submit. `Ok(Some(id))` = live, `Ok(None)` = venue confirms absent
    /// (-2013/-2011), `Err` = query failed. Runs on `signed_requery` (the short-timeout transport)
    /// so a double-timeout can't stall the core toward ~60s.
    fn query_order_orderid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", coid.to_string())];
        match self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER,
            "GET",
            &params,
            &self.signer,
        ) {
            Ok(resp) => Ok(resp.get("orderId").map(json_id)),
            Err(exc) if exc.code == -2013 || exc.code == -2011 => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1): live → managed
    /// OrderAccepted; venue-confirmed-absent → true terminal OrderRejected; query inconclusive →
    /// optimistic OrderAccepted (never a false terminal).
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_order_orderid(&request.client_order_id),
        )
    }

    /// Cancel by client-order-id. "Unknown order" (-2011) is swallowed (already gone ≠
    /// failure — the R6 gate's "unknown ≠ rejection" rule); other codes propagate.
    pub fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("origClientOrderId", client_order_id.to_string()),
        ];
        match self.transport.signed(&self.base_url, PATH_ORDER, "DELETE", &params, &self.signer) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == -2011 => {
                // order not found — idempotent
                tracing::debug!(target: "vike_aster::spot", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// Spot reconcile: free balance + open orders (locked sell qty added back — Aster/Binance
    /// "free" excludes it) + ticker mark seeded as avg_px so an immediate close is ~0 PnL instead
    /// of garbage.
    pub fn connect(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let account =
            self.transport.signed(&self.base_url, PATH_ACCOUNT, "GET", &[], &self.signer)?;
        let mut free = 0.0;
        for b in account.get("balances").and_then(|b| b.as_array()).unwrap_or(&vec![]) {
            if b.get("asset").and_then(|a| a.as_str()) == Some(self.base_asset.as_str()) {
                free = b.get("free").and_then(json_num).unwrap_or(0.0);
                break;
            }
        }

        let raw = self.transport.signed(
            &self.base_url,
            PATH_OPEN_ORDERS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        )?;
        let mut locked_sell_qty = 0.0;
        let mut orders: Vec<ManagedOrder> = Vec::new();
        for o in raw.as_array().unwrap_or(&vec![]) {
            let side = if o.get("side").and_then(|s| s.as_str()) == Some("BUY") { 1 } else { -1 };
            let orig_qty = o.get("origQty").and_then(json_num).unwrap_or(0.0);
            let executed_qty = o.get("executedQty").and_then(json_num).unwrap_or(0.0);
            if side < 0 {
                locked_sell_qty += (orig_qty - executed_qty).max(0.0);
            }
            // price None for "", "0", "0.00000000" (unpriced/market rows)
            let price = o.get("price").and_then(|p| p.as_str()).and_then(|p| {
                if matches!(p, "" | "0" | "0.00000000") {
                    None
                } else {
                    p.parse::<f64>().ok()
                }
            });
            let request: OrderRequest = serde_json::from_value(serde_json::json!({
                "client_order_id": o.get("clientOrderId").and_then(|c| c.as_str()).unwrap_or(""),
                "venue": VENUE,
                "symbol": self.symbol,
                "side": side,
                "qty": orig_qty,
                "order_type": o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
                "price": price,
            }))
            .expect("static shape");
            let mut mo = ManagedOrder::new(request);
            mo.status = OrderStatus::Accepted;
            mo.venue_order_id = Some(o.get("orderId").map(json_id).unwrap_or_default());
            orders.push(mo);
        }

        let seeded_size = free + locked_sell_qty; // "free" is the free portion only
        let ticker = self.transport.public(
            &self.base_url,
            PATH_TICKER,
            &[("symbol", self.symbol.clone())],
        )?;
        let mark_px = ticker.get("price").and_then(json_num).unwrap_or(0.0);
        Ok(ReconcileSnapshot {
            positions: vec![(self.symbol.clone(), seeded_size)],
            open_orders: orders,
            position_avg_px: vec![(self.symbol.clone(), mark_px)],
            position_mark_px: Vec::new(),
            position_sides: Vec::new(),
            balance: 0.0,
            // Spot carries no margin mode on the wire — empty = carry priors (default Cross).
            position_margin: Vec::new(),
        })
    }

    /// GET /api/v3/time → server-minus-local offset (ms). Pure query — applying the correction to
    /// the concrete signer's clock skew is the caller's job (`exec.rs` calls
    /// `AsterSigner::set_offset_us` with this value converted to µs; the generic `Signer` trait
    /// here has no `set_offset_*` method to call directly).
    pub fn server_time_offset(&self, local_now_ms: i64) -> Result<i64, VenueApiError> {
        let resp = self.transport.public(&self.base_url, PATH_TIME, &[])?;
        let server = resp.get("serverTime").and_then(|t| t.as_i64()).unwrap_or(local_now_ms);
        Ok(server - local_now_ms)
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for AsterSpotRest<S, T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        AsterSpotRest::submit_order(self, request)
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        AsterSpotRest::cancel_order(self, client_order_id)
    }
}

/// Back-compat name for the exec/smoke wiring.
pub type LiveAsterSpotClient<S, T> = LiveRestClient<AsterSpotRest<S, T>>;

#[cfg(test)]
mod order_param_tests {
    use super::*;
    use vike_model::OrderRequest;

    fn rest() -> AsterSpotRest<vike_bridge_core::BinanceHmacSigner, vike_bridge_core::UreqTransport>
    {
        // A concrete signer/transport just to own the struct for pure param-building (no network).
        let creds = vike_bridge_core::Credentials {
            api_key: "k".into(),
            api_secret: "s".into(),
            passphrase: None,
        };
        AsterSpotRest {
            signer: vike_bridge_core::BinanceHmacSigner::new(&creds, || 0),
            transport: vike_bridge_core::UreqTransport::new("aster"),
            base_url: "https://sapi.asterdex-testnet.com".into(),
            symbol: "BTCUSDT".into(),
            properties: vike_model::SymbolProperties::default(),
            base_asset: "BTC".into(),
            builder: None,
        }
    }

    #[test]
    fn limit_order_params_are_binance_shaped() {
        let req = OrderRequest {
            client_order_id: "c-1".to_string(),
            venue: "aster".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1, // +1 buy / -1 sell
            qty: 0.01,
            order_type: "limit".to_string(), // "market" | "limit" | "stop"
            price: Some(50000.0),
            ..Default::default()
        };
        let p = rest().build_order_params(&req);
        let m: std::collections::HashMap<_, _> = p.into_iter().collect();
        assert_eq!(m.get("symbol").map(String::as_str), Some("BTCUSDT"));
        assert_eq!(m.get("side").map(String::as_str), Some("BUY"));
        assert_eq!(m.get("type").map(String::as_str), Some("LIMIT"));
        assert_eq!(m.get("timeInForce").map(String::as_str), Some("GTC"));
        assert_eq!(m.get("newClientOrderId").map(String::as_str), Some("c-1"));
        assert!(m.contains_key("price") && m.contains_key("quantity"));
    }

    #[test]
    fn request_tif_is_ignored_limits_rest_gtc() {
        // The Ignored{GTC} aster row of `vike_bridge_core::tif::venue_tif` (aster shares the
        // binance family builder): the request TIF is NEVER read — a limit asking Fok still
        // rests GTC. Honoring it is a step-2 wire change behind demo smokes.
        let req = OrderRequest {
            client_order_id: "c-tif".to_string(),
            venue: VENUE.to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.01,
            order_type: "limit".to_string(),
            price: Some(50000.0),
            time_in_force: vike_model::TimeInForce::Fok,
            ..Default::default()
        };
        let m: std::collections::HashMap<_, _> =
            rest().build_order_params(&req).into_iter().collect();
        assert_eq!(m.get("timeInForce").map(String::as_str), Some("GTC"));
        assert_eq!(
            vike_bridge_core::tif::venue_tif(VENUE, vike_model::TimeInForce::Fok),
            vike_bridge_core::tif::TifOutcome::Ignored { wire: "GTC" }
        );
    }

    /// Unified cross-venue attribution (task 8): the spot twin of `perp.rs`'s
    /// `aster_order_carries_builder_and_feerate_when_configured` — Aster Code stamps
    /// `builder`+`feeRate` when configured and stamps NEITHER key when unset.
    #[test]
    fn aster_order_carries_builder_and_feerate_when_configured() {
        let req = OrderRequest {
            client_order_id: "c-1".to_string(),
            venue: "aster".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.01,
            order_type: "limit".to_string(),
            price: Some(50000.0),
            ..Default::default()
        };

        let mut with_builder = rest();
        with_builder.builder = Some(("0xbuilder".to_string(), "0".to_string()));
        let with = with_builder.build_order_params(&req);
        assert!(with.iter().any(|(k, v)| *k == "builder" && v == "0xbuilder"));
        assert!(with.iter().any(|(k, v)| *k == "feeRate" && v == "0"));

        let without = rest().build_order_params(&req);
        assert!(without.iter().all(|(k, _)| *k != "builder" && *k != "feeRate"));
    }
}
