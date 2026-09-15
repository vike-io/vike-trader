//! Aster `ReconClient` (Task 12) — the venue-facing report seam (`vike_exec::recon::ReconClient`)
//! for BOTH spot and USDⓈ-M perp.
//!
//! This is now a THIN venue skin over the shared family rung (`vike_binance::family::recon`): the
//! pure report parsers and the fetch/dispatch client body live there, venue-parameterized. Aster's
//! perp report shapes are byte-identical Binance forks, so both venues share the ONE copy; the
//! per-venue deltas are the endpoint PATHS ([`ASTER_RECON`]) — Aster keeps every fapi path under a
//! single `/fapi/v3/*` namespace (Binance splits `/fapi/v1/*` orders/trades/commissionRate from
//! `/fapi/v2/*` positionRisk/balance) — and, on the SPOT fill lane only, the row grammar. Auth also
//! differs (v3 EIP-712 wallet signatures via [`crate::signing::AsterSigner`] rather than Binance
//! HMAC), but that's a `Signer` seam, invisible here.
//!
//! ## ⚠ The spot fill lane is NOT a Binance fork (fixed 2026-08-05)
//!
//! Aster's spot account-trade endpoint diverges from the Binance template on BOTH axes, and the
//! first live reconcile smoke (`tests/aster_reconcile_smoke.rs`) is what found it:
//!
//! 1. **Path** — Binance's `/api/v3/myTrades` does not exist on `sapi.asterdex.com` (a plain 404).
//!    The endpoint is [`crate::spot::PATH_SPOT_USER_TRADES`] = `/api/v3/userTrades`. Because
//!    `vike_exec::recon::run_pass` short-circuits on the FIRST fetch `Err`, that 404 aborted the
//!    ENTIRE spot reconcile pass — orders and positions never reached `diff` either — on every pass
//!    since this client was written.
//! 2. **Row grammar** — the rows are FUTURES-shaped: `side` (`"BUY"`/`"SELL"`) + `maker`, NOT
//!    Binance spot's `isBuyer` + `isMaker`. Fixing only the path would have been WORSE than the
//!    404: `isBuyer` reads absent ⇒ `false` ⇒ every aster spot fill reported as a SELL, silently.
//!    The family reads both spellings through the documented union
//!    [`recon::fill_side`]/[`recon::fill_is_maker`].
//!
//! Source for both: <https://github.com/asterdex/api-docs> —
//! `V3(Recommended)/EN/aster-finance-spot-api-v3.md`, § "Account trade history (USER_DATA)", read
//! 2026-08-05. Its verbatim response example is the fixture in `tests/recon_client_parse.rs`'s
//! `spot_user_trade_maps_the_real_aster_wire_shape`.
//!
//! This module keeps only:
//!   - [`ASTER_RECON`]: the venue's `ReconSpec` (endpoint table + `"aster"` label).
//!   - `AsterReconClient`: a named wrapper over `FamilyReconClient` pinned to that spec, so the
//!     public `::spot(..)`/`::perp(..)` constructor API and the mount wiring are unchanged.
//!   - the 1-arg `parse_*` re-exports that inject `"aster"`, so `tests/recon_client_parse.rs`
//!     keeps proving the shared parsers against Aster's golden bodies.
//!
//! ## The SPOT fee lane — wired 2026-08-05, and it is a different endpoint from binance's
//!
//! This client was ported from a pre-#360 binance template, before binance's fork gained the live
//! fee lane — so aster reconcile historically refreshed NO fees. Routing through the shared client
//! restored the lane's SHAPE, but not its content: binance prices spot off `commissionRates` on
//! `/api/v3/account`, and **aster's fork of that endpoint does not carry the object**. Measured live
//! 2026-08-05 (HTTP 200) and confirmed against the published response example, aster's
//! `/api/v3/account` body carries exactly `balances`/`canBurnAsset`/`canDeposit`/`canTrade`/
//! `canWithdraw`/`feeTier`/`updateTime` — no `commissionRates` object and no legacy
//! `makerCommission` integers, the one fee-shaped field being a tier INDEX carrying no rate. So
//! `parse_spot_fee_rates` answered `Ok(None)` here on every pass, and the lane was inert.
//!
//! It is inert no longer. [`ASTER_RECON`] now sets the shared [`ReconPaths`]'s
//! `spot_commission_rate` slot to [`crate::spot::PATH_COMMISSION_RATE`]
//! (`GET /api/v3/commissionRate`, weight 20, `symbol` required), whose
//! `makerCommissionRate`/`takerCommissionRate` body the shared
//! [`recon::parse_commission_rate_body`] already parsed for the perp lane. `fetch_fee_rates` on the
//! spot lane returns this account's REAL per-symbol rates instead of `Ok(None)`.
//!
//! ⚠ **Binance is untouched by that slot and must stay so.** Its `spot_commission_rate` is `None`,
//! which routes it down the verbatim pre-slot `spot_account` arm — no second endpoint, no
//! per-symbol param, no extra request per pass.
//! `crates/bridges/aster/tests/recon_fee_lane_routing.rs` is the machine gate: it records the paths
//! each venue's spot fee lane actually requests and fails if binance's is anything but the single
//! parameterless account read it always was.
//!
//! ⚠ **The rate is PER-SYMBOL.** The static `vike_model::fee_schedule_for("aster")` row is one flat
//! pair; this lane answers for the symbol the client was constructed with. They agree for `BTCUSDT`
//! (both 0.5 / 4 bps, measured), and the venue's own docs show they need not agree everywhere —
//! see [`crate::spot::PATH_COMMISSION_RATE`].
//!
//! ## The PERP fee lane stays inert — deliberately, and NOT for want of an endpoint
//!
//! [`ASTER_RECON`]'s `perp_commission_rate` is still `None`, so `fetch_fee_rates` on the perp lane
//! short-circuits to `Ok(None)` and issues no request. `tests/aster_reconcile_smoke.rs`'s
//! `aster_perp_reconcile_fetch_smoke` pins that with a zero-network assertion.
//!
//! What changed is the REASON. The endpoint is no longer unverified: Aster documents
//! `GET /fapi/v3/commissionRate` (weight 20, `symbol` required) in
//! `V3(Recommended)/EN/aster-finance-futures-api-v3.md` § "User Commission Rate (USER_DATA)"
//! (<https://github.com/asterdex/api-docs>, read 2026-08-05), and a read-only probe answered HTTP
//! 200 for mainnet `BTCUSDT` with `{symbol, makerCommissionRate: "0", takerCommissionRate:
//! "0.000400"}` — `tests/aster_reconcile_smoke.rs`'s `aster_perp_commission_rate_probe`.
//!
//! It stays unwired for two reasons that survive that verification. First, wiring it is a behavior
//! change on a LIVE-MONEY venue's reconcile path — a new per-pass request against a real account —
//! and that is a separate decision from proving the endpoint answers. Second, it would not reach
//! what actually consumes the perp fee: `vike_model::fee_schedule_for("aster-perp")` is a STATIC
//! default read on every paper/backtest mount, where no `ReconClient` exists and `fetch_fee_rates`
//! is never called at all. Wiring the lane would refresh fees only in the one configuration
//! (`VIKE_RECONCILE=1`) that is already the least likely to be mispriced. See that row's own doc for
//! the residual risk and its direction.

use vike_binance::family::recon::{self, FamilyReconClient, ReconPaths, ReconSpec};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_bridge_core::{Credentials, Environment};
use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use crate::urls::VENUE;

/// Aster's reconcile endpoint table. Every fapi path lives under one `/fapi/v3/*` namespace — the
/// ONE real divergence from the Binance template (which splits `/fapi/v1` orders/trades from
/// `/fapi/v2` positionRisk/balance).
///
/// `spot_commission_rate` is `Some` — the ONE slot where aster's table diverges from binance's by a
/// BODY difference rather than a renamed route. See [`crate::spot::PATH_COMMISSION_RATE`]: aster's
/// `/api/v3/account` fork carries no `commissionRates` object, so the account-body fee lane binance
/// uses answers `Ok(None)` here forever, and the venue prices spot per-symbol on its own endpoint.
///
/// `perp_commission_rate` is `None`, still — see this module's doc for the full reasoning. Aster
/// DOES document `GET /fapi/v3/commissionRate`, and it answers live, but its per-account value is
/// not what the perp fee row needs to be sourced from (the row is a static default consulted on
/// every paper/backtest mount, where no recon client exists at all). Wiring it would ALSO be a
/// behavior change on a live-money venue that this change deliberately keeps out of scope; the
/// shared `parse_commission_rate_body` parser is ready either way.
pub const ASTER_RECON: ReconSpec = ReconSpec {
    venue: VENUE,
    paths: ReconPaths {
        spot_open_orders: crate::spot::PATH_OPEN_ORDERS,
        spot_my_trades: crate::spot::PATH_SPOT_USER_TRADES,
        spot_account: crate::spot::PATH_ACCOUNT,
        spot_commission_rate: Some(crate::spot::PATH_COMMISSION_RATE),
        perp_open_orders: crate::perp::PATH_PERP_OPEN_ORDERS,
        perp_user_trades: crate::perp::PATH_PERP_USER_TRADES,
        perp_positions: crate::perp::PATH_POSITIONS,
        perp_balance: crate::perp::PATH_BALANCE,
        perp_commission_rate: None,
    },
};

// --- 1-arg parser re-exports (inject the "aster" venue) ----------------------------------------
// These keep `tests/recon_client_parse.rs` proving the shared family parsers against Aster's golden
// bodies. The parsing lives in `vike_binance::family::recon`; here we only bind the venue string.

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

/// See [`recon::parse_spot_fee_rates`] — the ACCOUNT-BODY fee extractor.
///
/// ⚠ **Aster's live spot fee lane no longer goes through this**, and the fixture below it that
/// feeds a `commissionRates` object is a HYPOTHETICAL, not a captured aster body: the real
/// `/api/v3/account` on this venue carries no such object (module doc, measured 2026-08-05), which
/// is exactly why [`ASTER_RECON`] routes spot fees to [`crate::spot::PATH_COMMISSION_RATE`]
/// instead. Kept exported because it proves the SHARED parser stays fail-soft on aster's real body
/// (`Ok(None)`), which is the property that made the old inert lane harmless rather than wrong.
pub fn parse_spot_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    recon::parse_spot_fee_rates(body)
}

/// See [`recon::parse_commission_rate_body`] — the `commissionRate` ENDPOINT body grammar
/// (`makerCommissionRate`/`takerCommissionRate` as fractions).
///
/// The `perp_` in this name is now historical: on aster this exact parser serves the LIVE SPOT lane
/// ([`ASTER_RECON`]'s `spot_commission_rate`), while the perp lane it is named for stays unwired.
/// The name is kept because `tests/recon_client_parse.rs`'s golden-body fixtures are written against
/// it and the grammar is the same either way.
pub fn parse_perp_fee_rates(body: &str) -> Result<Option<FeeSchedule>, String> {
    recon::parse_commission_rate_body(body)
}

// --- the client --------------------------------------------------------------------------------

/// One reconcile client per venue-symbol, routed `Spot`/`Perp` — a named wrapper over the shared
/// [`FamilyReconClient`], pinned to [`ASTER_RECON`]. Constructed exactly as before
/// (`AsterReconClient::spot`/`::perp`); all fetch/parse logic delegates to the family rung.
pub struct AsterReconClient<S: Signer, T: RestTransport>(FamilyReconClient<S, T>);

impl<S: Signer, T: RestTransport> AsterReconClient<S, T> {
    pub fn spot(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        AsterReconClient(FamilyReconClient::spot(ASTER_RECON, signer, transport, base_url, symbol))
    }

    pub fn perp(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        AsterReconClient(FamilyReconClient::perp(ASTER_RECON, signer, transport, base_url, symbol))
    }
}

impl<S: Signer, T: RestTransport + Send> ReconClient for AsterReconClient<S, T> {
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

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_mount::make_engine`'s `("aster", _)` arm: builds the AGENT-WALLET (EIP-712) signer +
/// rate-gated `ureq` transport from the RESOLVED `env` + agent creds, so it hits the SAME network
/// the exec client does (testnet vs mainnet — Aster has no standard `{VENUE}_DEMO_*` shape, so its
/// creds/env are resolved by the caller via `signing::load_aster_credentials`, not this factory).
/// Routes spot vs the USDⓈ-M perp by the SAME `.P` suffix `exec::split_symbol`-style convention
/// every arm uses. Always `Some` — construction here is pure/infallible (no network); `Option` is
/// kept so the return type matches every other bridge's `recon_client` (deribit's genuinely can
/// fail).
pub fn recon_client(
    env: Environment,
    creds: &Credentials,
    symbol: &str,
) -> Option<Box<dyn ReconClient>> {
    let (api_symbol, is_perp) = vike_catalog::split_perp(symbol);
    let api_symbol = api_symbol.to_string();
    let signer = crate::signing::AsterSigner::new(creds, vike_model::now_us);
    let u = crate::urls::urls_for(env);
    let client: Box<dyn ReconClient> = if is_perp {
        Box::new(AsterReconClient::perp(
            signer,
            vike_bridge_core::UreqTransport::new("aster")
                .with_rate_gate(crate::ratelimit::perp_rest_gate()),
            u.fapi_rest,
            api_symbol,
        ))
    } else {
        Box::new(AsterReconClient::spot(
            signer,
            vike_bridge_core::UreqTransport::new("aster")
                .with_rate_gate(crate::ratelimit::spot_rest_gate()),
            u.sapi_rest,
            api_symbol,
        ))
    };
    Some(client)
}
