//! VenueRestAsterSpot — signed spot REST submit/cancel. Aster's spot API is a Binance-spot fork, so the
//! endpoint paths, order params, and `exchangeInfo` filter shapes are byte-identical to Binance's
//! `/api/v3/*` (but for [`PATH_SPOT_USER_TRADES`]), and the request bodies are the family's
//! (`vike_binance::family::execution::signed_rest`, the owner's ruling of 2026-11-13): this client is its
//! parts, its paths and its builder pair. The two differences from binance's client are (1) auth —
//! generic over `S: Signer` here, with the concrete EIP-712 `SignerAster` injected by `execution/mod.rs`
//! instead of Binance's HMAC signer, and (2) the base URL, which arrives via the `base_url` field
//! (set by `execution::mk_spot_rest` from the account's exec rows, `transport::urls::AsterUrls::exec`).
//!
//! The WS executionReport (Aster's listenKey user-data stream) is the SOLE source of fills; the
//! REST POST/DELETE response is ACK-ONLY. `submit_order` returns
//! [OrderSubmitted, OrderAccepted|OrderRejected] for the session to pump into the core ingest.
//! qty/price are formatted to the symbol's step/tick as decimal strings to dodge
//! BAD_PRECISION-class rejects.

use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::rest::{ExecutionClientRest, VenueRest};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The submit, cancel, confirm and resync reads are the family's bodies
// (`vike_binance::family::execution::signed_rest`), over this client's parts and facts (`FamilySignedRest` below).
// `parse_symbol_properties` is named at `vike_binance::family::execution::order_map` by its callers.
use vike_binance::family::execution::signed_rest::{FamilyRestPaths, FamilySignedRest};

// Homed in `crate::transport::urls` since the split-plane Phase-5 seam (this module is `exec`-gated, the
// venue id is not); imported here like any other consumer — no `crate::execution::rest::spot::VENUE` re-export
// (no pub-use shims on a move; call sites spell `crate::transport::urls::VENUE`).
use crate::transport::urls::VENUE;
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
/// smoke uses `UreqTransport` with the real `SignerAster`.
pub struct VenueRestAsterSpot<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub base_asset: String,
    /// Unified cross-venue attribution (task 8): the Aster Code `(builder address, feeRate)` pair
    /// stamped onto every order when configured. `None` (the default at every start lacking
    /// `ASTER_BUILDER_CODE`) means `build_order_params` emits neither `builder` nor `feeRate` —
    /// byte-identical to every order shape before this field existed. See `execution/rest/perp.rs`'s twin field
    /// for the approval helper (`VenueRestAsterPerp::approve_builder`) — the grant is account-wide, not
    /// spot/perp-specific, so one helper on the fapi client covers both.
    pub builder: Option<(String, String)>,
}

impl<S: Signer, T: RestTransport> VenueRestAsterSpot<S, T> {
    /// `build_order_params` hook — pure, golden-gated. Delegates to the shared, byte-identical
    /// [`vike_binance::family::execution::order_map::build_spot_order_params`] (mappers rung), passing this
    /// client's `symbol`/`properties`; `newOrderRespType=ACK` and the response's fills[] is
    /// deliberately ignored (WS is the sole fill source).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        let mut params = vike_binance::family::execution::order_map::build_spot_order_params(
            request,
            &self.symbol,
            &self.properties,
            VENUE, // aster's tif row is still Ignored{GTC} — bytes unchanged (unflipped)
            // Aster does not use the Binance broker-prefix order client id mechanic (unified cross-venue
            // attribution, task 6). Aster's OWN attribution mechanic (Aster Code, task 8) is the
            // `self.builder` push below, not this shared family builder's `link_id`.
            None,
        );
        // A plain (market) stop is `STOP_MARKET` with its `stopPrice` (owner ruling F-4): on aster's spot API
        // `STOP` is the LIMIT stop and needs `price` and `stopPrice`, so the family builder's bare `type=STOP`, with
        // neither, went out unbuildable (aster's spot v3 docs, "Place order": `STOP_MARKET` takes quantity +
        // stopPrice). The other trigger kinds stay off this lane: the `ASTER` caps row admits market, limit, stop.
        if request.order_type == OrderType::Stop {
            if let Some(ty) = params.iter_mut().find(|(k, _)| *k == "type") {
                ty.1 = "STOP_MARKET".to_string();
            }
            params.push((
                "stopPrice",
                format_to_step_f(request.trigger_price.unwrap_or(0.0), self.properties.tick_size),
            ));
        }
        // Unified cross-venue attribution (task 8): Aster Code is a builder address + feeRate pair
        // on the order params (the HL-style builder model on this Binance-fork REST API), NOT a
        // order client id prefix. `None` (unconfigured) pushes neither key — byte-identical.
        if let Some((addr, rate)) = &self.builder {
            params.push(("builder", addr.clone()));
            params.push(("feeRate", rate.clone()));
        }
        params
    }

    /// The family's submit ([`vike_binance::family::execution::signed_rest::submit_order`]): Submitted → REST →
    /// Accepted|Rejected, with NO bridge floor on this lane (binance's spot floors are not aster's) and
    /// the venue's words, no cause, on a refusal. The returned events go into the core ingest.
    pub fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        vike_binance::family::execution::signed_rest::submit_order(self, request)
    }

    /// Cancel by order-client-id, the bare id ([`vike_binance::family::execution::signed_rest::cancel_by_order_client_id`]):
    /// "unknown order" is "already gone", everything else propagates.
    pub fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        vike_binance::family::execution::signed_rest::cancel_by_order_client_id(
            self,
            order_client_id,
        )
    }
}

/// Aster spot's parts and facts for the family's signed REST bodies: `/api/v3` with aster's own
/// [`PATH_SPOT_USER_TRADES`], the bare order client id (no broker stamp), NO bridge floor and no
/// refusal cause (the trait's defaults), and its own log target. The resync reads `allOrders` and
/// `userTrades` through [`vike_binance::family::execution::signed_rest::get_all_orders_for`] and
/// [`vike_binance::family::execution::signed_rest::get_trades_for`]: ⚠ the `myTrades` path bug was the quieter
/// casualty there, since the replay's best-effort read swallowed the 404 into an empty replay.
impl<S: Signer, T: RestTransport> FamilySignedRest for VenueRestAsterSpot<S, T> {
    const VENUE: &'static str = VENUE;
    const PATHS: FamilyRestPaths = FamilyRestPaths {
        order: PATH_ORDER,
        all_orders: "/api/v3/allOrders",
        trades: PATH_SPOT_USER_TRADES,
        leverage: "",
        batch: "",
    };

    fn transport(&self) -> &dyn RestTransport {
        &self.transport
    }
    fn signer(&self) -> &dyn Signer {
        &self.signer
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }
    fn order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        self.build_order_params(request)
    }
    fn log_refused(&self, exc: &VenueApiError) {
        tracing::warn!(target: "vike_aster::execution::rest::spot", code = exc.code, msg = %exc.msg, "submit rejected");
    }
}

impl<S: Signer + Send, T: RestTransport + Send> ConfirmByOrderClientId
    for VenueRestAsterSpot<S, T>
{
    const LOOKUP_WINDOW: Option<std::time::Duration> = None; // none stated by the venue

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        vike_binance::family::execution::signed_rest::confirm_when(
            self,
            order_client_id,
            ts,
            absent_rejects,
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for VenueRestAsterSpot<S, T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        VenueRestAsterSpot::submit_order(self, request)
    }
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        VenueRestAsterSpot::cancel_order(self, order_client_id)
    }
    /// ACTIVELY confirm one order by its client id (the by-order-client-id status GET): found -> `OrderAccepted`
    /// (+ the terminal when the venue already closed it), absent -> the "venue confirms absent"
    /// reject, a FAILED query -> NOTHING (the watchdog ladder and reconcile decide). This trait door
    /// assumes the submit was fully dispatched; the exec thread's confirm closure goes through
    /// [`ConfirmByOrderClientId::confirm_when`] with the real answer.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }
}

/// Back-compat name for the exec/smoke wiring.
pub type LiveAsterSpotClient<S, T> = ExecutionClientRest<VenueRestAsterSpot<S, T>>;

#[cfg(test)]
mod order_param_tests {
    use super::*;
    use crate::execution::rest::perp::order_param_tests::{assert_builder_stamped, limit, signer};
    use vike_model::OrderRequest;

    fn rest() -> VenueRestAsterSpot<
        vike_binance::family::signer::BinanceHmacSigner,
        vike_bridge_core::transport::UreqTransport,
    > {
        VenueRestAsterSpot {
            signer: signer(),
            transport: vike_bridge_core::transport::UreqTransport::new("aster"),
            base_url: "https://sapi.asterdex-testnet.com".into(),
            symbol: "BTCUSDT".into(),
            properties: vike_model::SymbolProperties::default(),
            base_asset: "BTC".into(),
            builder: None,
        }
    }

    #[test]
    fn limit_order_params_are_binance_shaped() {
        let p = rest().build_order_params(&limit(1));
        let m: std::collections::HashMap<_, _> = p.into_iter().collect();
        assert_eq!(m.get("symbol").map(String::as_str), Some("BTCUSDT"));
        assert_eq!(m.get("side").map(String::as_str), Some("BUY"));
        assert_eq!(m.get("type").map(String::as_str), Some("LIMIT"));
        assert_eq!(m.get("timeInForce").map(String::as_str), Some("GTC"));
        assert_eq!(m.get("newClientOrderId").map(String::as_str), Some("c-1"));
        assert!(m.contains_key("price") && m.contains_key("quantity"));
    }

    #[test]
    fn request_time_in_force_is_ignored_limits_rest_gtc() {
        // The Ignored{GTC} aster row of `vike_venues::venue_time_in_force::venue_time_in_force` (aster shares the
        // binance family builder): the request TIF is NEVER read — a limit asking Fok still
        // rests GTC. Honoring it is a step-2 wire change behind demo smokes.
        let req = OrderRequest {
            order_client_id: "c-tif".to_string(),
            venue: VENUE.to_string(),
            time_in_force: vike_model::TimeInForce::Fok,
            ..limit(1)
        };
        let m: std::collections::HashMap<_, _> =
            rest().build_order_params(&req).into_iter().collect();
        assert_eq!(m.get("timeInForce").map(String::as_str), Some("GTC"));
        assert_eq!(
            vike_venues::venue_time_in_force::venue_time_in_force(
                VENUE,
                vike_model::TimeInForce::Fok
            ),
            vike_venues::venue_time_in_force::TimeInForceOutcome::Ignored { wire: "GTC" }
        );
    }

    /// **A spot stop is `STOP_MARKET` with its trigger as `stopPrice`, on the tick grid** (owner ruling F-4): the
    /// family builder's bare `STOP` is aster's LIMIT stop, and it went out with no `stopPrice` and no `price`.
    #[test]
    fn a_spot_stop_is_stop_market_with_its_stop_price() {
        let mut client = rest();
        client.properties.tick_size = 0.1;
        let stop = OrderRequest {
            order_type: vike_model::OrderType::Stop,
            price: None,
            trigger_price: Some(48_000.04),
            ..limit(-1)
        };
        let m: std::collections::HashMap<_, _> =
            client.build_order_params(&stop).into_iter().collect();
        assert_eq!(m.get("type").map(String::as_str), Some("STOP_MARKET"));
        assert_eq!(m.get("stopPrice").map(String::as_str), Some("48000.0"));
        assert!(!m.contains_key("price") && !m.contains_key("timeInForce"), "{m:?}");
    }

    /// Unified cross-venue attribution (task 8): the spot twin of `execution/rest/perp.rs`'s
    /// `aster_order_carries_builder_and_feerate_when_configured` — Aster Code stamps
    /// `builder`+`feeRate` when configured and stamps NEITHER key when unset.
    #[test]
    fn aster_order_carries_builder_and_feerate_when_configured() {
        let mut with_builder = rest();
        with_builder.builder = Some(("0xbuilder".to_string(), "0".to_string()));
        let with = with_builder.build_order_params(&limit(1));
        assert_builder_stamped(&with, &rest().build_order_params(&limit(1)));
    }
}
