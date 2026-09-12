//! Binance `ReconClient` (Task 7 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_exec::recon::ReconClient`) for BOTH spot and USDS-M perp.
//!
//! This is now a THIN venue skin over the shared family rung ([`crate::family::recon`]): the pure
//! report parsers and the fetch/dispatch client body live there, venue-parameterized. Aster is a
//! byte-identical wire fork, so both venues share the ONE copy; the sole per-venue delta is the
//! endpoint PATHS ([`BINANCE_RECON`]) — Binance splits fapi across `/fapi/v1/*` (orders/trades/
//! commissionRate) and `/fapi/v2/*` (positionRisk/balance).
//!
//! This module keeps only:
//!   - [`BINANCE_RECON`]: the venue's [`ReconSpec`] (endpoint table + `"binance"` label).
//!   - `BinanceReconClient`: a named wrapper over [`FamilyReconClient`] pinned to that spec, so the
//!     public `::spot(..)`/`::perp(..)` constructor API and the `ReconManager` wiring are unchanged.
//!   - the 1-arg `parse_*` re-exports that inject `"binance"`, so `tests/offline/recon_client_parse.rs`
//!     keeps proving the shared parsers against Binance's golden bodies (they are proven again
//!     against Aster's in `vike-aster`).
//!
//! Reconciles against SPOT `GET /api/v3/openOrders` + `GET /api/v3/myTrades`, and PERP
//! `GET /fapi/v1/openOrders` + `GET /fapi/v1/userTrades` + `GET /fapi/v2/positionRisk`; balances via
//! `GET /fapi/v2/balance` (perp) / `GET /api/v3/account` (spot); live fees via
//! `commissionRates` on `/api/v3/account` (spot) / `GET /fapi/v1/commissionRate` (perp). See the
//! family rung doc for the parsing contract and the `signed`/`signed_requery` gating precedent.
//!
//! ⚠ The spot fee lane above is why [`ReconPaths`] carries a `spot_commission_rate` slot that
//! [`BINANCE_RECON`] sets to `None`: aster needs a per-symbol endpoint because its `/api/v3/account`
//! fork drops `commissionRates`, and `None` is what keeps THIS venue on the account body it already
//! fetches — one parameterless request, exactly as before the slot existed. See that field's doc.

use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use crate::VENUE;
use crate::family::recon::{self, FamilyReconClient, ReconPaths, ReconSpec};

/// Binance's reconcile endpoint table. Note the fapi split — orders/trades/commissionRate under
/// `/fapi/v1`, positionRisk/balance under `/fapi/v2` — the ONE real divergence from Aster's single
/// `/fapi/v3` namespace.
pub const BINANCE_RECON: ReconSpec = ReconSpec {
    venue: VENUE,
    paths: ReconPaths {
        spot_open_orders: crate::spot::PATH_OPEN_ORDERS,
        spot_my_trades: "/api/v3/myTrades",
        spot_account: crate::spot::PATH_ACCOUNT,
        // `None` — Binance's spot `/api/v3/account` body carries `commissionRates{maker,taker}`, so
        // the spot fee lane prices off the SAME body `fetch_balance` already fetches and needs no
        // per-symbol endpoint. This is the arm that keeps binance byte-identical to before the slot
        // existed; `crates/bridges/aster/tests/recon_fee_lane_routing.rs`'s
        // `binance_spot_fee_lane_still_reads_the_account_body` is the machine gate for it (it lives
        // in vike-aster because only that side depends on both crates, so only it can record and
        // compare BOTH venues' request lists).
        // Do NOT "unify" it with aster's `Some` arm: even if Binance served an equivalent
        // per-symbol endpoint, routing through it would be a real behavior change (a second
        // request per pass, and a per-symbol rate replacing an account-wide one that is already
        // free), and nothing in this tree has verified such an endpoint against the live venue.
        spot_commission_rate: None,
        perp_open_orders: "/fapi/v1/openOrders",
        perp_user_trades: "/fapi/v1/userTrades",
        perp_positions: crate::perp::PATH_POSITIONS,
        perp_balance: crate::perp::PATH_BALANCE,
        perp_commission_rate: Some("/fapi/v1/commissionRate"),
    },
};

// --- 1-arg parser re-exports (inject the "binance" venue) --------------------------------------
// These keep `tests/offline/recon_client_parse.rs` proving the shared family parsers against Binance's
// golden bodies. The parsing lives in `family::recon`; here we only bind the venue string.

/// See [`recon::parse_spot_open_orders`].
pub fn parse_spot_open_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    recon::parse_spot_open_orders(body, VENUE)
}

/// See [`recon::parse_perp_open_orders`].
pub fn parse_perp_open_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    recon::parse_perp_open_orders(body, VENUE)
}

/// See [`recon::parse_spot_my_trades`].
pub fn parse_spot_my_trades(body: &str) -> Result<Vec<FillReport>, String> {
    recon::parse_spot_my_trades(body, VENUE)
}

/// See [`recon::parse_perp_user_trades`].
pub fn parse_perp_user_trades(body: &str) -> Result<Vec<FillReport>, String> {
    recon::parse_perp_user_trades(body, VENUE)
}

/// See [`recon::parse_perp_position_risk`].
pub fn parse_perp_position_risk(body: &str) -> Result<Vec<PositionStatusReport>, String> {
    recon::parse_perp_position_risk(body, VENUE)
}

/// See [`recon::parse_perp_balance`].
pub fn parse_perp_balance(body: &str) -> Result<Option<f64>, String> {
    recon::parse_perp_balance(body)
}

/// See [`recon::parse_spot_balance`].
pub fn parse_spot_balance(body: &str) -> Result<Option<f64>, String> {
    recon::parse_spot_balance(body)
}

/// See [`recon::parse_spot_fee_rates`].
pub fn parse_spot_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    recon::parse_spot_fee_rates(body)
}

/// See [`recon::parse_perp_fee_rates`].
pub fn parse_perp_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    recon::parse_perp_fee_rates(body)
}

// --- the factory -------------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_mount::build_recon_client`'s `"binance"` match arm: a FRESH HMAC `ureq` REST client
/// dedicated to reconcile reads, reusing the SAME `Credentials` the venue's `ExecutionClient`/
/// `spawn_with_recorder` already builds from. Routes spot vs the USDⓈ-M perp by the SAME `.P`
/// suffix `exec.rs::split_symbol` uses (a trailing `.P` selects perp, exchange symbol stripped of
/// the suffix). Host pair (spot + fapi) is selected by `mainnet` — the SAME already-resolved
/// `BINANCE_MAINNET` verdict the mount threaded into the exec path
/// (`crate::exec::mainnet_enabled`, resolved ONCE per mount by `vike_mount::make_engine`), so
/// reconcile stays in lockstep with exec instead of re-reading global env for itself; `false` ⇒ the
/// demo hosts (`spot::DEMO_REST`/`perp::DEMO_FAPI_REST`), byte-identical to before the switch
/// existed. Always `Some` — construction here is pure/infallible (no network); `Option` is kept so
/// the return type matches every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_bridge_core::Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<Box<dyn ReconClient>> {
    let (api_symbol, is_perp) = vike_catalog::split_perp(symbol);
    let api_symbol = api_symbol.to_string();
    let signer = vike_bridge_core::BinanceHmacSigner::new(creds, vike_model::now_ms);
    let gate = if is_perp {
        crate::ratelimit::perp_rest_gate()
    } else {
        crate::ratelimit::spot_rest_gate()
    };
    let transport = vike_bridge_core::UreqTransport::new("binance").with_rate_gate(gate);
    // Reconcile reads follow the SAME resolved `BINANCE_MAINNET` verdict the exec path binds to
    // (threaded in by the mount, not re-read here), so a mainnet mount reconciles against the
    // mainnet account; `false` ⇒ the demo hosts, byte-identical to before the switch existed.
    let (spot_base, perp_base) = if mainnet {
        (crate::spot::MAINNET_REST, crate::perp::MAINNET_FAPI_REST)
    } else {
        (crate::spot::DEMO_REST, crate::perp::DEMO_FAPI_REST)
    };
    let client: Box<dyn ReconClient> = if is_perp {
        Box::new(BinanceReconClient::perp(signer, transport, perp_base, api_symbol))
    } else {
        Box::new(BinanceReconClient::spot(signer, transport, spot_base, api_symbol))
    };
    Some(client)
}

// --- the client --------------------------------------------------------------------------------

/// One reconcile client per venue-symbol, routed `Spot`/`Perp` — a named wrapper over the shared
/// [`FamilyReconClient`], pinned to [`BINANCE_RECON`]. Constructed exactly as before
/// (`BinanceReconClient::spot`/`::perp`); all fetch/parse logic delegates to the family rung.
pub struct BinanceReconClient<S: Signer, T: RestTransport>(FamilyReconClient<S, T>);

impl<S: Signer, T: RestTransport> BinanceReconClient<S, T> {
    pub fn spot(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BinanceReconClient(FamilyReconClient::spot(
            BINANCE_RECON,
            signer,
            transport,
            base_url,
            symbol,
        ))
    }

    pub fn perp(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BinanceReconClient(FamilyReconClient::perp(
            BINANCE_RECON,
            signer,
            transport,
            base_url,
            symbol,
        ))
    }
}

impl<S: Signer, T: RestTransport + Send> ReconClient for BinanceReconClient<S, T> {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.0.fetch_order_status_reports(since)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.0.fetch_fill_reports(since)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.0.fetch_position_status_reports()
    }

    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.0.fetch_balance()
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.0.fetch_fee_rates()
    }
}
