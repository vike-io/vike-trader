//! Binance `ReconcileClient` (Task 7 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_execution::recon::ReconcileClient`) for BOTH spot and USDS-M perp.
//!
//! This is now a THIN venue skin over the shared family rung ([`crate::family::recon`]): the pure
//! report parsers and the fetch/dispatch client body live there, venue-parameterized. Aster is a
//! byte-identical wire fork, so both venues share the ONE copy; the sole per-venue delta is the
//! endpoint PATHS ([`BINANCE_RECONCILE`]) — Binance splits fapi across `/fapi/v1/*` (orders/trades/
//! commissionRate) and `/fapi/v2/*` (positionRisk/balance).
//!
//! This module keeps only:
//!   - [`BINANCE_RECONCILE`]: the venue's [`ReconcileSpec`] (endpoint table + `"binance"` label).
//!   - `ReconcileClientBinance`: a named wrapper over [`ReconcileClientBinanceFamily`] pinned to that spec, so the
//!     public `::spot(..)`/`::perp(..)` constructor API and the `ReconcileManager` wiring are unchanged.
//!   - the 1-arg `parse_*` re-exports that inject `"binance"`, so `tests/offline/reconcile_client_parse.rs`
//!     keeps proving the shared parsers against Binance's golden bodies (they are proven again
//!     against Aster's in `vike-aster`).
//!
//! Reconciles against SPOT `GET /api/v3/openOrders` + `GET /api/v3/myTrades`, and PERP
//! `GET /fapi/v1/openOrders` + `GET /fapi/v1/userTrades` (less the trades of the
//! `GET /fapi/v1/forceOrders` liquidation and ADL orders, and of any order whose `GET /fapi/v1/order`
//! client id is a venue liquidation one) + `GET /fapi/v2/positionRisk`; balances via
//! `GET /fapi/v2/balance` (perp) / `GET /api/v3/account` (spot); live fees via
//! `commissionRates` on `/api/v3/account` (spot) / `GET /fapi/v1/commissionRate` (perp). See the
//! family rung doc for the parsing contract and the `signed`/`signed_requery` gating precedent.
//!
//! ⚠ The spot fee lane above is why [`ReconcilePaths`] carries a `spot_commission_rate` slot that
//! [`BINANCE_RECONCILE`] sets to `None`: aster needs a per-symbol endpoint because its `/api/v3/account`
//! fork drops `commissionRates`, and `None` is what keeps THIS venue on the account body it already
//! fetches — one parameterless request, exactly as before the slot existed. See that field's doc.

// An operator's read of the account's open orders (the adapter's `venue_read`): signed GETs, so exec-plane.
pub(crate) mod venue_read;

use std::sync::Arc;

use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;
use vike_execution::recon::ReconcileClient;
use vike_model::{FeeSchedule, FillReport, Margining, OrderStatusReport, PositionStatusReport};

use crate::VENUE;
use crate::execution::rest::coin_m::RestTransportDapiPaths;
use crate::family::recon::{self, ReconcileClientBinanceFamily, ReconcilePaths, ReconcileSpec};
use crate::family::transport::RestTransportOnVenueClock;
use vike_bridge_core::signer::VenueClock;

/// Binance's reconcile endpoint table. Note the fapi split — orders/trades/commissionRate under
/// `/fapi/v1`, positionRisk/balance under `/fapi/v2` — the ONE real divergence from Aster's single
/// `/fapi/v3` namespace.
pub const BINANCE_RECONCILE: ReconcileSpec = ReconcileSpec {
    venue: VENUE,
    paths: ReconcilePaths {
        spot_open_orders: crate::execution::rest::spot::PATH_OPEN_ORDERS,
        spot_my_trades: "/api/v3/myTrades",
        spot_account: crate::execution::rest::spot::PATH_ACCOUNT,
        // `None` — Binance's spot `/api/v3/account` body carries `commissionRates{maker,taker}`, so
        // the spot fee lane prices off the SAME body `fetch_balance` already fetches and needs no
        // per-symbol endpoint. This is the arm that keeps binance byte-identical to before the slot
        // existed; `crates/bridges/aster/tests/offline/reconcile_fee_lane_routing.rs`'s
        // `binance_spot_fee_lane_still_reads_the_account_body` is the machine gate for it (it lives
        // in vike-aster because only that side depends on both crates, so only it can record and
        // compare BOTH venues' request lists).
        // Do NOT "unify" it with aster's `Some` arm: even if Binance served an equivalent
        // per-symbol endpoint, routing through it would be a real behavior change (a second
        // request per pass, and a per-symbol rate replacing an account-wide one that is already
        // free), and nothing in this tree has verified such an endpoint against the live venue.
        spot_commission_rate: None,
        spot_exchange_info: crate::execution::rest::spot::PATH_EXCHANGE_INFO,
        perp_open_orders: "/fapi/v1/openOrders",
        perp_user_trades: "/fapi/v1/userTrades",
        perp_force_orders: "/fapi/v1/forceOrders",
        perp_order: crate::execution::rest::perp::PATH_ORDER,
        perp_positions: crate::execution::rest::perp::PATH_POSITIONS,
        perp_balance: crate::execution::rest::perp::PATH_BALANCE,
        perp_commission_rate: Some("/fapi/v1/commissionRate"),
    },
};

// --- 1-arg parser re-exports (inject the "binance" venue) --------------------------------------
// These keep `tests/offline/reconcile_client_parse.rs` proving the shared family parsers against Binance's
// golden bodies. The parsing lives in `family::recon`; here we only bind the venue string.

/// See [`recon::parse_spot_open_orders`]; `link_id` is the broker link id the account's orders carry.
pub fn parse_spot_open_orders(
    body: &str,
    link_id: Option<&str>,
) -> Result<Vec<OrderStatusReport>, String> {
    recon::parse_spot_open_orders(body, VENUE, link_id)
}

/// See [`recon::parse_perp_open_orders`]; `link_id` is the broker link id the account's orders carry.
pub fn parse_perp_open_orders(
    body: &str,
    link_id: Option<&str>,
) -> Result<Vec<OrderStatusReport>, String> {
    recon::parse_perp_open_orders(body, VENUE, link_id)
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

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"binance"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH HMAC `ureq` REST client
/// dedicated to reconcile reads, reusing the SAME `Credentials` the venue's `ExecutionClient`/
/// `spawn_with_recorder` already builds from. Routes spot vs the USDⓈ-M perp by the SAME `.P`
/// suffix `execution/mod.rs::split_symbol` uses (a trailing `.P` selects perp, exchange symbol stripped of
/// the suffix). The hosts are `hosts` — the SAME addresses the start looked up for the exec path
/// (`crate::execution`'s `BinanceUrls::resolve` at the account's tier; decision 0095: `live` means
/// mainnet), resolved ONCE per start by `crate::adapter`'s `VenueAdapterBinance::start`, so
/// reconcile stays in lockstep with exec instead of re-reading anything for itself.
/// Always `Some` — construction here is pure/infallible (no network); `Option` is kept so
/// the return type matches every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    hosts: crate::execution::BinanceUrls,
) -> Option<Box<dyn ReconcileClient>> {
    reconcile_client_linked(creds, symbol, hosts, None)
}

/// [`recon_client`] for a start that knows the broker link id its orders are placed under
/// (`crate::adapter`): both lanes then name a fill (a triggered stop's, a plain order's) only when its
/// order's id is OURS (`x-<link id>-<order client id>` or a bare order client id), never one placed by hand.
pub fn reconcile_client_linked(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    hosts: crate::execution::BinanceUrls,
    link_id: Option<String>,
) -> Option<Box<dyn ReconcileClient>> {
    reconcile_client_sharing(creds, symbol, hosts, link_id, Arc::default(), &VenueClock::default())
}

/// [`reconcile_client_linked`] over an admitted-symbol list and a clock the CALLER also holds: the adapter
/// hands the same `Arc` to the exec thread, so the symbol the engine tells this client about
/// (`ReconcileClient::admit_symbol`) is also one whose history the reconnect-resync replays, and the same
/// [`VenueClock`], so an account's reconcile reads sign on the time its orders do.
pub fn reconcile_client_sharing(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    hosts: crate::execution::BinanceUrls,
    link_id: Option<String>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: &VenueClock,
) -> Option<Box<dyn ReconcileClient>> {
    let (book, api_symbol) = Book::of(symbol);
    Some(Box::new(
        reader(creds, book, api_symbol, hosts, clock).with_link_id(link_id).with_admitted(admitted),
    ))
}

/// **The three books one key pair trades**, each with lists of its own: spot (`/api`), USDⓈ-M (`/fapi`, linear) and
/// COIN-M (`/dapi`, inverse). A reconcile client reads one; the operator's open-orders read asks all three
/// (`crate::reconcile::venue_read`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Book {
    Spot,
    UsdM,
    CoinM,
}

impl Book {
    /// The book an engine symbol (`BTCUSDT`, `BTCUSDT.P`, `BTCUSD_PERP.P`) routes to, and its exchange symbol: the `.P`
    /// lane marker is a futures book, a COIN-M name (`crate::execution::rest::coin_m::is_coin_m`) the inverse one.
    fn of(symbol: &str) -> (Book, &str) {
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, symbol);
        let book = match (is_perp, crate::execution::rest::coin_m::is_coin_m(api_symbol)) {
            (false, _) => Book::Spot,
            (true, false) => Book::UsdM,
            (true, true) => Book::CoinM,
        };
        (book, api_symbol)
    }

    /// The book's name in an operator's sentence.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Book::Spot => "spot",
            Book::UsdM => "USDⓈ-M",
            Book::CoinM => "COIN-M",
        }
    }
}

/// The client [`reconcile_client_sharing`] boxes, before the link id and the admitted list: `book`'s, on its host and
/// transport, reading `symbol` (an exchange symbol). `crate::reconcile::venue_read` builds one per book with no symbol
/// and asks it for the account's open orders.
pub(crate) fn reader(
    creds: &vike_secrets::credentials::Credentials,
    book: Book,
    symbol: &str,
    hosts: crate::execution::BinanceUrls,
    clock: &VenueClock,
) -> ReconcileClientBinance<
    crate::family::signer::BinanceHmacSigner,
    RestTransportOnVenueClock<RestTransportDapiPaths<vike_bridge_core::transport::UreqTransport>>,
> {
    use crate::execution::rest::{coin_m, perp, spot};
    use crate::transport::ratelimit::{perp_rest_gate, spot_rest_gate};
    let signer = crate::family::signer::BinanceHmacSigner::new(creds, clock.ms());
    // Reconcile reads take the SAME addresses (decision 0095: the tier's `AdapterInputs::tier`)
    // the exec path binds to (looked up by the start, not re-resolved here), so a mainnet start
    // reconciles against the mainnet account. A COIN-M mount reads its own book (`dapi`, the host
    // `crate::execution`'s `BinanceUrls::futures_rest` gives the exec clients of the same name).
    // A spot client's paths are no futures paths: `Linear` is the pass-through.
    let (margining, gate, time_path, base) = match book {
        Book::Spot => (Margining::Linear, spot_rest_gate(), spot::PATH_TIME, hosts.spot_rest),
        Book::UsdM => (Margining::Linear, perp_rest_gate(), perp::PATH_TIME, hosts.fapi_rest),
        Book::CoinM => (Margining::Inverse, perp_rest_gate(), coin_m::PATH_TIME, hosts.dapi_rest),
    };
    let transport = RestTransportOnVenueClock::new(
        RestTransportDapiPaths(
            vike_bridge_core::transport::UreqTransport::new("binance").with_rate_gate(gate),
            margining,
        ),
        VENUE,
        clock,
        time_path,
    );
    match book {
        Book::Spot => ReconcileClientBinance::spot(signer, transport, base, symbol),
        Book::UsdM | Book::CoinM => ReconcileClientBinance::perp(signer, transport, base, symbol),
    }
}

// --- the client --------------------------------------------------------------------------------

/// One reconcile client per venue-symbol, routed `Spot`/`Perp` — a named wrapper over the shared
/// [`ReconcileClientBinanceFamily`], pinned to [`BINANCE_RECONCILE`]. Constructed exactly as before
/// (`ReconcileClientBinance::spot`/`::perp`); all fetch/parse logic delegates to the family rung.
pub struct ReconcileClientBinance<S: Signer, T: RestTransport>(
    ReconcileClientBinanceFamily<S, T>,
    /// How the fill naming reads (the perp lane's Algo Service history, both lanes' by-id order reads)
    /// have been going across passes, and what they answered ([`crate::execution::rest::algo::AlgoNaming`]).
    crate::execution::rest::algo::AlgoNaming,
    /// The settle coin of a COIN-M perp client (`BTCUSD_PERP` settles in `BTC`, read off the name by
    /// `vike_catalog::inverse_settle_coin`), `None` for every other: that engine keeps its books in the coin
    /// (decision 0258), so [`ReconcileClient::fetch_balance`] reports the coin's wallet row. Every other read is the
    /// USDⓈ-M one, carried to dapi by the client's transport (`crate::execution::rest::coin_m::RestTransportDapiPaths`, built for its book).
    Option<String>,
);

impl<S: Signer, T: RestTransport> ReconcileClientBinance<S, T> {
    /// Tell the client the broker link id the account's orders are placed under (the start knows it:
    /// `attribution_code_from`); it lives on the family client, which every read of an echoed id asks.
    /// Without it a prefixed (`x-`) id is never taken for ours by the fill naming (`crate::execution::rest::algo`'s
    /// `own_order_client_id`).
    #[must_use]
    pub fn with_link_id(mut self, link_id: Option<String>) -> Self {
        self.0 = self.0.with_link_id(link_id);
        self
    }

    /// Keep the admitted-symbol list in `admitted` instead of a private one, so the holder of the other
    /// end (the exec thread's resync and gap-sentinel) sees every symbol this client is told.
    #[must_use]
    pub fn with_admitted(mut self, admitted: Arc<ReconcileAdmittedSymbols>) -> Self {
        self.0 = self.0.with_admitted(admitted);
        self
    }

    pub fn spot(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        ReconcileClientBinance(
            ReconcileClientBinanceFamily::spot(
                BINANCE_RECONCILE,
                signer,
                transport,
                base_url,
                symbol,
            ),
            Default::default(),
            None,
        )
    }

    pub fn perp(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        let symbol = symbol.into();
        // The settle coin of a COIN-M contract, read off the NAME like the door's (held to every row).
        let coin = vike_catalog::inverse_settle_coin(VENUE, &symbol);
        ReconcileClientBinance(
            ReconcileClientBinanceFamily::perp(
                BINANCE_RECONCILE,
                signer,
                transport,
                base_url,
                symbol,
            ),
            Default::default(),
            coin,
        )
    }

    /// **Every open order of the ACCOUNT on this client's book**, asked with no `symbol` (an operator's read,
    /// `crate::reconcile::venue_read`; the reconcile reads stay per symbol): spot `GET /api/v3/openOrders`; perp
    /// `GET /fapi/v1/openOrders` and the Algo Service's `GET /fapi/v1/openAlgoOrders`, which lists the resting
    /// stops and take-profits the first does not. A COIN-M client's transport carries the first to
    /// `/dapi/v1/openOrders` and answers the second empty without a call (dapi has no Algo Service). The rows go
    /// through the reconcile parsers, so the two reads cannot disagree about a row. Any failure fails the read, in
    /// words naming `book` (the one this client was built for). On
    /// the short-timeout path (`signed_requery_raw`): one read an operator asks for stalls a gatewayhub thread five
    /// seconds a request at most and spends nothing of the order-rate window.
    pub(crate) fn account_open_orders(&self, book: Book) -> Result<Vec<OrderStatusReport>, String> {
        let read = |path: &str| {
            let list = path.rsplit('/').next().unwrap_or(path);
            self.0
                .signed_requery_raw(path, &[])
                .map_err(|e| format!("binance {} {list} could not be read: {e}", book.word()))
        };
        let paths = BINANCE_RECONCILE.paths;
        match self.0 {
            ReconcileClientBinanceFamily::Spot { .. } => {
                parse_spot_open_orders(&read(paths.spot_open_orders)?, self.0.link_id())
            }
            ReconcileClientBinanceFamily::Perp { .. } => {
                let link_id = self.0.link_id();
                let mut rows = parse_perp_open_orders(&read(paths.perp_open_orders)?, link_id)?;
                let algo = crate::execution::rest::algo::PATH_OPEN_ALGO_ORDERS;
                rows.extend(crate::execution::rest::algo::parse_perp_open_algo_orders(
                    &read(algo)?,
                    VENUE,
                    link_id,
                )?);
                Ok(rows)
            }
        }
    }
}

impl<S: Signer, T: RestTransport + Send> ReconcileClient for ReconcileClientBinance<S, T> {
    /// The family's `openOrders` reports, and — on the PERP lane — the Algo Service's
    /// `openAlgoOrders` ones (`GET /fapi/v1/openOrders` is not documented to list a conditional
    /// order, so without this read a resting stop is invisible to every reconcile pass and a restore
    /// reads it as gone). ⚠ A failed algo read fails the whole report ([`crate::execution::rest::algo::fetch_open_algo_reports`]).
    /// `aster` has no such read: the extension is HERE, not in `ReconcileClientBinanceFamily`.
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let mut reports = self.0.fetch_order_status_reports(since)?;
        if matches!(self.0, ReconcileClientBinanceFamily::Perp { .. }) {
            reports.extend(crate::execution::rest::algo::fetch_open_algo_reports(&self.0)?);
        }
        Ok(reports)
    }

    /// The family's `userTrades`/`myTrades` fills, each named with its order's client id
    /// ([`crate::execution::rest::algo::name_algo_children`]): both endpoints name the order by id and carry no order client id,
    /// so without it a stop that filled while the node was down looks like one the venue stopped
    /// listing, and a plain order's fill (BOTH lanes) cannot be joined to its order. Aster has no such
    /// read and keeps the family's fills as they are.
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut fills = self.0.fetch_fill_reports(since)?;
        crate::execution::rest::algo::name_algo_children(
            &self.0,
            &self.1,
            self.0.link_id(),
            &mut fills,
        )?;
        Ok(fills)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.0.fetch_position_status_reports()
    }

    /// ⚠ **A COIN-M mount reports its SETTLE COIN's wallet** (`balance` of the coin's row of
    /// `/dapi/v1/balance`, `crate::execution::rest::coin_m::parse_coin_balance`): decision 0258 adopts it as the coin ledger's
    /// cash. Every other mount reads the family's USDT figure, as before.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let Some(coin) = self.2.as_deref() else { return self.0.fetch_balance() };
        let body = self
            .0
            .signed_raw(crate::execution::rest::perp::PATH_BALANCE, &[])
            .map_err(|e| e.to_string())?;
        crate::execution::rest::coin_m::parse_coin_balance(&body, coin)
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.0.fetch_fee_rates()
    }

    /// ⚠ Forwarded by hand, like every method here: a wrapper that omits it inherits the trait's
    /// no-op and the engine's admitted symbols are silently never reconciled.
    fn admit_symbol(&self, symbol: &str) {
        self.0.admit_symbol(symbol);
    }

    /// ⚠ Forwarded by hand too (decision 0205): the engine gave a symbol back.
    fn forget_symbol(&self, symbol: &str) {
        self.0.forget_symbol(symbol);
    }

    /// ⚠ Forwarded by hand as well: the default is `None` ("no list"), which the gatewayhub would not name.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.0.admitted_symbols()
    }

    /// **BOTH lanes list the trigger kinds they place** (decision 0192 item 4: the restore declares an
    /// absent stop gone by silence only on this claim).
    ///
    /// - PERP: the stop (`STOP_MARKET`) and the stop-limit (`STOP`) rest on the Algo Service, which
    ///   `GET /fapi/v1/openOrders` does not list, and [`Self::fetch_order_status_reports`] appends
    ///   `openAlgoOrders` (strictly: a failed algo read fails the whole report), so the report lists
    ///   each with its bare order client id and its kind (`execution/rest/algo_tests.rs`'s
    ///   `the_open_list_parses_to_reports_on_the_series_label`; the conformance harness' resting-kind
    ///   row in `crates/vike-bridge-core/tests/bridge_conformance.rs`). A TRIGGERED stop-limit leaves
    ///   `openAlgoOrders` and its child LIMIT is listed by `openOrders` under the stop's own
    ///   `clientAlgoId` (measured, `crate::execution::rest::perp`'s `VenueRestBinancePerp::cancel_member`).
    /// - SPOT: the stop (`STOP_LOSS`) and the stop-limit (`STOP_LOSS_LIMIT`) are ordinary spot orders,
    ///   and `GET /api/v3/openOrders` (the family's report read) lists every open order of the symbol
    ///   whatever its type (the venue's page; MEASURED on the demo host 2026-11-08: both rows listed
    ///   with `stopPrice`, `isWorking: false`, `status: NEW`), which the family parser reads as
    ///   `OrderType::Stop`/`StopLimit` with the bare order client id (`spot_trigger_listing_tests` below). The
    ///   take-profits (`TAKE_PROFIT`, `TAKE_PROFIT_LIMIT`) are ordinary spot orders too, read as
    ///   `OrderType::TakeProfit`; their rows are the measured shape with the take-profit words, NOT
    ///   captures, until `tests/binance_smokes/take_profit_smoke.rs` runs.
    /// - PERP take-profits (`TAKE_PROFIT_MARKET`, `TAKE_PROFIT`) rest on the Algo Service like the stop,
    ///   and `openAlgoOrders` reads both as `OrderType::TakeProfit` (`execution/rest/algo_tests.rs`); also unmeasured.
    fn lists_trigger_orders(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod spot_trigger_listing_tests {
    //! The SPOT lane's proof for its half of [`ReconcileClient::lists_trigger_orders`]: the
    //! `GET /api/v3/openOrders` rows the demo host answered for a resting `STOP_LOSS_LIMIT` and
    //! `STOP_LOSS` (2026-11-08, `tests/binance_smokes/perp_algo_smoke.rs`'s
    //! `spot_stop_limit_on_the_demo_host`, verbatim but for a broker-stamped id on the second) parse
    //! to LIVE reports of their kind carrying the bare order client id, and the real clients of both lanes claim.
    use super::*;
    use vike_model::{OrderStatus, OrderType};

    const MEASURED: &str = r#"[
        {"clientOrderId":"vtsslL22402550","cummulativeQuoteQty":"0.00000000","executedQty":"0.00000000","icebergQty":"0.00000000","isWorking":false,"orderId":14608754892,"orderListId":-1,"origQty":"0.00450000","origQuoteOrderQty":"0.00000000","price":"2504.64000000","selfTradePreventionMode":"EXPIRE_MAKER","side":"BUY","status":"NEW","stopPrice":"2459.92000000","symbol":"ETHUSDT","time":1794122402685,"timeInForce":"GTC","type":"STOP_LOSS_LIMIT","updateTime":1794122402685,"workingTime":-1},
        {"clientOrderId":"x-LINK-vtsslS22402550","cummulativeQuoteQty":"0.00000000","executedQty":"0.00000000","icebergQty":"0.00000000","isWorking":false,"orderId":14608754893,"orderListId":-1,"origQty":"0.00450000","origQuoteOrderQty":"0.00000000","price":"0.00000000","selfTradePreventionMode":"EXPIRE_MAKER","side":"BUY","status":"NEW","stopPrice":"2459.92000000","symbol":"ETHUSDT","time":1794122402953,"timeInForce":"GTC","type":"STOP_LOSS","updateTime":1794122402953,"workingTime":-1}
    ]"#;

    #[test]
    fn the_spot_open_orders_list_the_stop_and_the_stop_limit_it_places() {
        let reports =
            parse_spot_open_orders(MEASURED, Some("LINK")).expect("the measured body parses");
        let got: Vec<_> = reports
            .iter()
            .map(|r| (r.order_client_id.as_deref(), r.order_type.clone(), r.status, r.qty, r.side))
            .collect();
        assert_eq!(
            got,
            [
                (Some("vtsslL22402550"), OrderType::StopLimit, OrderStatus::Accepted, 0.0045, 1),
                (Some("vtsslS22402550"), OrderType::Stop, OrderStatus::Accepted, 0.0045, 1),
            ]
        );
        assert_eq!(reports[0].venue_order_id.as_str(), "14608754892");
        assert!(reports.iter().all(|r| r.symbol == "ETHUSDT" && r.filled_qty == 0.0));
        // the measured terms: the stop-limit's limit and level, the stop's level ("0.00000000" no price)
        let terms: Vec<_> =
            reports.iter().map(|r| (r.price, r.trigger_price, r.trail, r.trail_percent)).collect();
        assert_eq!(
            terms,
            [(Some(2504.64), Some(2459.92), None, None), (None, Some(2459.92), None, None)]
        );

        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let urls = vike_venues::venue_urls::VenueUrls::seed("binance");
        let demo = crate::execution::BinanceUrls::resolve(&urls, vike_model::AccountTier::Demo)
            .expect("the seed");
        for symbol in ["ETHUSDT", "ETHUSDT.P"] {
            let client = recon_client(&creds, symbol, demo).expect("offline");
            assert!(client.lists_trigger_orders(), "{symbol}: the lane lists what it places");
        }
    }

    /// The take-profits the spot lane places (`TAKE_PROFIT`, `TAKE_PROFIT_LIMIT`, the second an iceberg)
    /// list as LIVE take-profit reports with the bare order client id. ⚠ The rows are [`MEASURED`]'s shape with
    /// the take-profit words and an `icebergQty`, NOT captures (no take-profit has been placed on the
    /// demo host yet).
    #[test]
    fn the_spot_open_orders_list_the_take_profits_it_places() {
        let body = MEASURED
            .replacen("vtsslL22402550", "vttpL22402550", 1)
            .replacen(r#""type":"STOP_LOSS_LIMIT""#, r#""type":"TAKE_PROFIT_LIMIT""#, 1)
            .replacen(r#""icebergQty":"0.00000000""#, r#""icebergQty":"0.00150000""#, 1)
            .replacen("x-LINK-vtsslS22402550", "x-LINK-vttpM22402550", 1)
            .replacen(r#""type":"STOP_LOSS""#, r#""type":"TAKE_PROFIT""#, 1);
        let reports =
            parse_spot_open_orders(&body, Some("LINK")).expect("the take-profit rows parse");
        let got: Vec<_> = reports
            .iter()
            .map(|r| (r.order_client_id.as_deref(), r.order_type.clone(), r.status))
            .collect();
        assert_eq!(
            got,
            [
                (Some("vttpL22402550"), OrderType::TakeProfit, OrderStatus::Accepted),
                (Some("vttpM22402550"), OrderType::TakeProfit, OrderStatus::Accepted),
            ]
        );
    }
}
