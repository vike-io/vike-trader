//! Binance live exec half — `ExecutionClient` over the signed REST + the private user-data stream,
//! routing SPOT vs USDⓈ-M PERP by a trailing `.P` on the symbol. The live twin of
//! `bybit::exec` (linear perp) and `okx::exec` (SWAP), assembled from the
//! existing binance parts — NOTHING here is reimplemented, this is the wiring seam:
//!
//! * SPOT (`BTCUSDT`): [`VenueRestBinanceSpot`](crate::execution::rest::spot::VenueRestBinanceSpot) over `/api/v3` +
//!   [`spawn_binance_user_data_with_resync`](crate::execution::user_data::spot::spawn_binance_user_data_with_resync)
//!   (the WS-API executionReport fills pump).
//! * PERP (`BTCUSDT.P`): [`VenueRestBinancePerp`](crate::execution::rest::perp::VenueRestBinancePerp) over `/fapi/v1` (native
//!   modify/batch) + [`spawn_binance_perp_user_data_with_resync`](crate::execution::user_data::perp::spawn_binance_perp_user_data_with_resync)
//!   (the listenKey ORDER_TRADE_UPDATE fills pump).
//! * COIN-M (`BTCUSD_PERP.P`, `BTCUSD_261225.P`): the SAME perp lane on the `dapi` host of the account's network
//!   ([`BinanceUrls::futures_rest`]; `crate::execution::rest::coin_m` carries the USDⓈ-M requests to their dapi twins), mounted at its row's
//!   face and refused when the row cannot value it ([`coin_m_execution_refusal`]).
//!
//! Built on the shared [`ExecutionClientActor`] scaffold: `submit`/`cancel`/`modify` enqueue onto ONE dedicated
//! OS thread that drives the venue REST (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously; the authoritative async
//! fills/cancels return on the private WS pump straight into the core ingest.
//!
//! `.P` ROUTING (the SAME idiom the trade/kline feeds use — see `feed::DataClientSpecBinance::plan`): the
//! trailing `.P` is stripped to the EXCHANGE symbol (`BTCUSDT`) that drives the venue REST/WS
//! endpoints, while the ORIGINAL `.P`-suffixed symbol stays the CORE/series label the fills carry
//! (so a perp's position/order key never collides with its spot twin). A non-`.P` symbol is
//! byte-identical to spot-only behavior.
//!
//! LIVE GATE: absent credentials never reach here — the start only spawns this when
//! `vike_secrets::credentials::api_credentials` yields `Some` for the started account (the
//! codebase's absent-creds-is-the-live-gate rule). An account with no keys does not run.
//!
//! v1 scope: `submit`/`cancel`/`modify`. PERP `modify` is the NATIVE fapi amend (`PUT /fapi/v1/order`,
//! wired via `run_loop`'s `ExecutionClientActorCommand::Modify` → `VenueRestBinancePerp::modify_order`); SPOT `modify` is
//! an atomic `POST /api/v3/order/cancelReplace` under the SAME order client id
//! (`VenueRestBinanceSpot::modify_order`: untouched LIMIT orders only; every other case answers
//! `OrderModifyRejected`, never silence). Native fapi BATCH (submit/cancel) exists
//! on `VenueRestBinancePerp` but is NOT reachable through the shared `ExecutionClientActor` command channel yet — it
//! carries no batch variant, so batches fan out to per-order submits via the `ExecutionClient`
//! default (same limitation as bybit; an `ExecutionClientActorCommand::Batch` in vike-bridge-core is the follow-up).

pub mod event_mapper;
// The perp funding poll's decoder and poller (the account's income history): a signed read, so it rides `exec`
// with the exec client that spawns it.
pub mod funding;
pub mod rest;
pub mod resync;
pub mod user_data;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use crate::family::signer::BinanceHmacSigner;
use crate::refusals::BinanceLeverageRefusal;
use vike_bridge_core::confirm::DispatchLedger;
use vike_bridge_core::exec_actor::funding_poll;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::exec_actor::{
    DEFAULT_FILL_SENTINEL, ExecutionClientActor, ExecutionClientActorCommand, run_loop,
};
use vike_bridge_core::leverage::DEFAULT_LEVERAGE;
use vike_bridge_core::leverage_read::LeverageWriteError;
use vike_bridge_core::per_symbol::{ClientFactory, OrderClientIdSymbols};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::rest::VenueRestRefusing;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncHandle;
use vike_execution::EventSender;
use vike_model::AccountTier;
use vike_model::events::Event;
use vike_model::{MoneyModel, MoneyModelRefusal, NotionalKind, SymbolProperties};
use vike_secrets::credentials::Credentials;
use vike_utility::poller::StopHandle;

use crate::execution::funding::income_poller;
use crate::execution::rest::coin_m::{self, RestTransportDapiPaths};
use crate::execution::rest::perp::{
    PATH_EXCHANGE_INFO as FAPI_PATH_EXCHANGE_INFO, PATH_USER_WEBSOCKET, VenueRestBinancePerp,
    parse_binance_perp_instruments,
};
use crate::execution::rest::spot::{PATH_EXCHANGE_INFO, VenueRestBinanceSpot};
use crate::execution::resync::map_binance_perp_history;
use crate::execution::user_data::perp::spawn_binance_perp_user_data_with_resync;
use crate::execution::user_data::spot::{PATH_WEBSOCKET_API, spawn_binance_user_data_with_resync};
use crate::family::execution::dispatch::{lane_symbol, split_symbol};
use crate::family::execution::{self, FamilyExecutionLog, FamilyExecutionSpec, FamilyGridSource};
use crate::family::transport::RestTransportOnVenueClock;
use vike_bridge_core::signer::VenueClock;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The concrete REST clients this exec half drives (blocking `ureq` + HMAC signer, on one [`VenueClock`]).
/// A futures client rides [`RestTransportDapiPaths`], which is the inner transport request for request unless the client is
/// of the COIN-M book (`Inverse`, [`coin_m::margining_of`]).
type SpotRest = VenueRestBinanceSpot<BinanceHmacSigner, RestTransportOnVenueClock<UreqTransport>>;
type PerpRest = VenueRestBinancePerp<
    BinanceHmacSigner,
    RestTransportOnVenueClock<RestTransportDapiPaths<UreqTransport>>,
>;

/// **The exec half's six addresses, looked up ONCE per start** from the account's `venue_url` rows
/// (`vike_venues::venue_urls::VenueUrls::url`, the one lookup) at the account's type: spot, USDⓈ-M (`fapi`) and
/// COIN-M (`dapi`) signed REST and the spot WS-API session (channel `trade`), and the two futures listenKey
/// sockets (channel `user`). Every client, the grid fetch, the user-data streams, the funding poll, the leverage
/// reader and the reconcile client of one account take their host HERE, so a leverage write and its read-back,
/// an order and its reconcile, can never land on two networks. A `demo` lookup never falls back to a live row
/// (channels `trade` and `user` do not), so no live address is reachable from a demo start.
///
/// `Copy`: each address is `crate::family::static_url`'s, so a factory closure and a thread carry the set by
/// value, as they carried the tier before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinanceUrls {
    /// The type the rows were asked at (the account's tier, `UrlType::of_tier`): the network every futures
    /// client carries (`VenueRestBinancePerp::ty`).
    pub ty: UrlType,
    /// Spot signed-REST origin (`/api/v3`), product `spot`.
    pub spot_rest: &'static str,
    /// USDⓈ-M futures signed-REST origin (`/fapi/v1`), product `usdm`.
    pub fapi_rest: &'static str,
    /// COIN-M futures signed-REST origin (`/dapi/v1`), product `coinm`.
    pub dapi_rest: &'static str,
    /// Spot user-data WS-API session: the row plus [`PATH_WEBSOCKET_API`].
    pub spot_websocket: &'static str,
    /// USDⓈ-M listenKey user-data socket base: the row plus [`PATH_USER_WEBSOCKET`].
    pub fapi_websocket: &'static str,
    /// COIN-M listenKey user-data socket base: the row plus [`PATH_USER_WEBSOCKET`].
    pub dapi_websocket: &'static str,
}

impl BinanceUrls {
    /// The six addresses of an account at `tier` (`Paper` reads as `demo`), from its `venue_url` rows.
    ///
    /// # Errors
    /// A row is missing (or a demo row is the live address): the account does NOT RUN, with the refusal's words.
    pub fn resolve(urls: &VenueUrls, tier: AccountTier) -> Result<BinanceUrls, UrlRefusal> {
        use UrlChannel::{Trade, User};
        use UrlProtocol::{Rest, Websocket};
        let ty = UrlType::of_tier(tier);
        let at = |channel, protocol, product, path: &str| {
            urls.url(ty, channel, protocol, product)
                .map(|base| crate::family::static_url(&format!("{base}{path}")))
        };
        Ok(BinanceUrls {
            ty,
            spot_rest: at(Trade, Rest, "spot", "")?,
            fapi_rest: at(Trade, Rest, "usdm", "")?,
            dapi_rest: at(Trade, Rest, "coinm", "")?,
            spot_websocket: at(Trade, Websocket, "spot", PATH_WEBSOCKET_API)?,
            fapi_websocket: at(User, Websocket, "usdm", PATH_USER_WEBSOCKET)?,
            dapi_websocket: at(User, Websocket, "coinm", PATH_USER_WEBSOCKET)?,
        })
    }

    /// **The futures REST origin of `symbol`**: COIN-M's (`dapi`) for a COIN-M name, else USDⓈ-M's (`fapi`). Every
    /// futures client of this crate takes its host here, so a COIN-M symbol's order, cancel, status, history,
    /// leverage and mode requests all go to its own book, and a USDⓈ-M symbol's to the host they always went to.
    #[must_use]
    pub fn futures_rest(&self, symbol: &str) -> &'static str {
        if crate::instrument::names::is_coin_m(symbol) { self.dapi_rest } else { self.fapi_rest }
    }
}

/// One fresh SPOT REST client for `symbol` (its own transport + signer, so it can live on its own
/// thread), on the account's spot host (`hosts`, the start's [`BinanceUrls`]). `base_asset` is read by
/// nothing on submit/cancel/history (its one reader, the legacy snapshot read, is deleted), so an empty
/// fallback is harmless.
fn mk_spot_rest(
    hosts: BinanceUrls,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    link_id: Option<String>,
    clock: &VenueClock,
) -> SpotRest {
    mk_spot_rest_gated(
        hosts,
        creds,
        symbol,
        properties,
        base_asset,
        link_id,
        crate::transport::ratelimit::spot_rest_gate(),
        clock,
    )
}

/// [`mk_spot_rest`] on a GIVEN order-rate window: the mounted spot client and every foreign-symbol
/// spot client of one exec thread are built on clones of ONE gate, because the venue's order budget
/// is per account, not per symbol.
fn mk_spot_rest_gated(
    hosts: BinanceUrls,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    link_id: Option<String>,
    gate: RateGate,
    clock: &VenueClock,
) -> SpotRest {
    VenueRestBinanceSpot {
        signer: BinanceHmacSigner::new(creds, clock.ms()),
        transport: RestTransportOnVenueClock::new(
            UreqTransport::new("binance").with_rate_gate(gate),
            "binance",
            clock,
            crate::execution::rest::spot::PATH_TIME,
        ),
        base_url: hosts.spot_rest.to_string(),
        symbol: symbol.to_string(),
        properties,
        base_asset: base_asset.to_string(),
        link_id,
    }
}

/// One fresh PERP (fapi) REST client for the EXCHANGE `api_symbol` (its own transport + signer), on the
/// symbol's book of the account's network ([`BinanceUrls::futures_rest`], [`coin_m::margining_of`]).
///
/// `leverage` is the mount-resolved account leverage (`vike_bridge_core::leverage::leverage_for`) — the ONLY consumer is
/// `VenueRestBinancePerp::set_leverage`, which `run_perp` fires once at start.
fn mk_perp_rest(
    hosts: BinanceUrls,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    link_id: Option<String>,
    leverage: f64,
    clock: &VenueClock,
) -> PerpRest {
    mk_perp_rest_gated(
        hosts,
        creds,
        api_symbol,
        properties,
        link_id,
        leverage,
        crate::transport::ratelimit::perp_rest_gate(),
        clock,
    )
}

/// [`mk_perp_rest`] on a GIVEN order-rate window (see [`mk_spot_rest_gated`]).
fn mk_perp_rest_gated(
    hosts: BinanceUrls,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    link_id: Option<String>,
    leverage: f64,
    gate: RateGate,
    clock: &VenueClock,
) -> PerpRest {
    let margining = coin_m::margining_of(api_symbol);
    VenueRestBinancePerp {
        signer: BinanceHmacSigner::new(creds, clock.ms()),
        transport: RestTransportOnVenueClock::new(
            RestTransportDapiPaths(UreqTransport::new("binance").with_rate_gate(gate), margining),
            "binance",
            clock,
            coin_m::time_path_for(api_symbol),
        ),
        base_url: hosts.futures_rest(api_symbol).to_string(),
        ty: hosts.ty,
        margining,
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        link_id,
        algo_order_client_ids: Default::default(),
    }
}

/// **Binance's facts for the family's exec plane** (`crate::family::execution`, shared with aster
/// since the owner's ruling of 2026-11-13): the account's [`BinanceUrls`], the broker link id, the
/// TRADE_LITE switch and, on a COIN-M mount, its settle coin (set by [`run`] once the mount's row is read).
///
/// - The link id rides EVERY client (the submit-capable one prefixes the ids it sends; the confirm, resync
///   and sentinel clients read the ids the venue echoes with it), whatever `places_orders` says.
/// - The user data is the spot WS-API session (`crate::execution::user_data::spot`) and the futures listenKey pump
///   (`crate::execution::user_data::perp`) on the mount's own book: `dapi` for a COIN-M mount, `fapi` for every other.
/// - The perp start heals the account's modes on the DEMO host and posts the leverage
///   ([`perp_start_writes`]).
/// - A foreign perpetual is fenced to the engine's money model ([`coin_fence`]) before its grid is
///   fetched, and its margin type and class leverage are prepared after its client is built
///   (`crate::account::modes::heal_symbol`, `crate::account::class_prep`); its cancel and status take a client with no grid
///   fetch (`light_perp_factory`).
/// - The lines it writes are `vike_binance::execution`'s.
#[derive(Debug, Clone)]
struct BinanceExecution {
    hosts: BinanceUrls,
    link_id: Option<String>,
    trade_lite_fill: bool,
    /// The settle coin of a COIN-M mount, `None` for a USDⓈ-M or spot one.
    coin: Option<String>,
    /// The mount's money model, off its KEPT row (`vike_model::MoneyModel::of_contract`): what [`coin_fence`]
    /// holds every other perpetual to. `None` for a spot pair or a row that states none.
    money: Option<MoneyModel>,
}

impl FamilyExecutionSpec for BinanceExecution {
    const VENUE: &'static str = crate::VENUE;
    type Spot = SpotRest;
    type Perp = PerpRest;

    fn spot_gate(&self) -> RateGate {
        crate::transport::ratelimit::spot_rest_gate()
    }
    fn perp_gate(&self) -> RateGate {
        crate::transport::ratelimit::perp_rest_gate()
    }

    fn spot_rest(
        &self,
        creds: &Credentials,
        symbol: &str,
        properties: SymbolProperties,
        base_asset: &str,
        _places_orders: bool,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> SpotRest {
        let link_id = self.link_id.clone();
        match gate {
            Some(gate) => mk_spot_rest_gated(
                self.hosts, creds, symbol, properties, base_asset, link_id, gate, clock,
            ),
            None => mk_spot_rest(self.hosts, creds, symbol, properties, base_asset, link_id, clock),
        }
    }

    fn perp_rest(
        &self,
        creds: &Credentials,
        api_symbol: &str,
        properties: SymbolProperties,
        _places_orders: bool,
        leverage: f64,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> PerpRest {
        let link_id = self.link_id.clone();
        match gate {
            Some(gate) => mk_perp_rest_gated(
                self.hosts, creds, api_symbol, properties, link_id, leverage, gate, clock,
            ),
            None => {
                mk_perp_rest(self.hosts, creds, api_symbol, properties, link_id, leverage, clock)
            }
        }
    }

    fn fetch_properties(
        &self,
        creds: &Credentials,
        symbol: &str,
    ) -> Option<(SymbolProperties, String)> {
        fetch_binance_properties(self.hosts, creds, symbol).ok()
    }

    fn no_grid_reason(&self, label: &str) -> String {
        no_grid_reason(label)
    }

    /// Decision 0290: an engine's label is a USER name, signed as its kept row's lane name
    /// (`crate::instrument::names::lane_name_of`).
    fn lane_name(&self, label: &str) -> Result<String, String> {
        crate::instrument::names::lane_name_of(label).map_err(|refusal| refusal.to_string())
    }

    fn user_name(&self, lane: &str) -> String {
        crate::instrument::names::user_name_of(lane)
    }

    fn spoken(&self, events: Vec<Event>) -> Vec<Event> {
        crate::instrument::names::speak_user_names(events)
    }

    /// `label` is the order's USER name: its kept row states its money model.
    fn foreign_perp_fence(&self, label: &str) -> Result<(), String> {
        coin_fence(label, self.money.as_ref())
    }

    fn foreign_perp_built(&self, rest: &PerpRest) {
        // The MARGIN TYPE is written, once, before this symbol's first order (this client is built once per
        // symbol and cached): on the DEMO host an isolated symbol is switched to cross when the venue sees it
        // flat (a mainnet client does nothing). Never blocks the order, and a symbol that holds a position is
        // left as it is (`crate::account::modes`).
        let _ = crate::account::modes::heal_symbol(rest);
        // A symbol the account NEVER traded reads leverage 0, and for some symbols (a TradFi equity perpetual
        // capped at 5x, a weekly at 10x) the venue refuses the first order at that default (`-2027`): on the
        // DEMO host the account's configured leverage, capped at the symbol's maximum, is written once before
        // it (`crate::account::class_prep`; a symbol that holds a leverage or a position is never written).
        crate::account::class_prep::report_leverage(
            rest,
            &crate::account::class_prep::ensure_symbol_leverage(rest),
        );
    }

    /// A foreign perp's CANCEL and status take a client with no grid fetch (a symbol and an order client id are all they
    /// send; the grid and leverage riding on it are never read), so they do not wait for `exchangeInfo`. The
    /// lane check is the factory's own (an other-lane symbol is still refused).
    fn light_perp_factory(
        &self,
        creds: &Credentials,
        properties: SymbolProperties,
        gate: RateGate,
        clock: &VenueClock,
    ) -> Option<ClientFactory<PerpRest>> {
        let (hosts, creds, link_id, clock) =
            (self.hosts, creds.clone(), self.link_id.clone(), clock.clone());
        Some(Box::new(move |label| {
            let lane = crate::instrument::names::lane_name_of(label)
                .map_err(|refusal| refusal.to_string())?;
            let api_symbol = lane_symbol("binance", &lane, true)?;
            Ok(mk_perp_rest_gated(
                hosts,
                &creds,
                &api_symbol,
                properties,
                link_id.clone(),
                DEFAULT_LEVERAGE,
                gate.clone(),
                &clock,
            ))
        }))
    }

    fn spot_start(&self, rest: &SpotRest) {
        // The account's clock, read now so the subscribe and the first order sign on it.
        rest.transport.sync(&rest.base_url);
    }

    fn perp_start(&self, rest: &PerpRest, series_symbol: &str, leverage: f64) {
        // The account's clock, read now so the start's writes and the first order sign on it.
        rest.transport.sync(&rest.base_url);
        perp_start_writes(rest, series_symbol, leverage);
    }

    fn spawn_spot_user_data(
        &self,
        creds: &Credentials,
        symbol: &str,
        events: EventSender,
        fence: Arc<SubmitFence>,
        resync: impl FnMut() -> Vec<Event> + Send + 'static,
        on_reconcile: Option<mpsc::Sender<()>>,
        clock: &VenueClock,
    ) -> WebsocketSessionUserResyncHandle {
        // Private WS-API user-data (+ audit-A3 resync): authoritative fills/cancels into ingest through the fence.
        spawn_binance_user_data_with_resync(
            self.hosts.spot_websocket.to_string(),
            creds.api_key.expose().clone(),
            creds.api_secret.expose().clone(),
            symbol.to_string(),
            events,
            fence,
            resync,
            on_reconcile,
            clock.clone(),
            self.link_id.clone(),
        )
    }

    fn spawn_perp_user_data(
        &self,
        creds: &Credentials,
        series_symbol: &str,
        events: EventSender,
        fence: Arc<SubmitFence>,
        resync: impl FnMut() -> Vec<Event> + Send + 'static,
        on_reconcile: Option<mpsc::Sender<()>>,
        _clock: &VenueClock,
    ) -> WebsocketSessionUserResyncHandle {
        // The mount's book: `dapi` for a COIN-M mount, `fapi` for every other, as before. Private listenKey
        // user-data (+ audit-A3 resync): fills carry the CORE `.P` label of their own `s`.
        let coin_m = self.coin.is_some();
        let (book_rest, book_websocket) = if coin_m {
            (self.hosts.dapi_rest, self.hosts.dapi_websocket)
        } else {
            (self.hosts.fapi_rest, self.hosts.fapi_websocket)
        };
        spawn_binance_perp_user_data_with_resync(
            book_rest.to_string(),
            book_websocket.to_string(),
            coin_m,
            creds.api_key.expose().clone(),
            series_symbol.to_string(),
            events,
            fence,
            resync,
            on_reconcile,
            self.trade_lite_fill,
            self.link_id.clone(),
        )
    }

    fn log(line: FamilyExecutionLog) {
        crate::family_execution_log!("vike_binance::execution", line)
    }
}

/// **The one write of the account's leverage for a perpetual the engine was NOT mounted on** (the
/// gatewayhub lane's per-symbol hand-over, `VenueAdapter::set_symbol_leverage`):
/// [`VenueRestBinancePerp::set_leverage`] itself (`POST /fapi/v1/leverage`, the figure cast `as i64`,
/// idempotent on the venue), the verb `run_perp` posts at its start, on a client of its own because
/// `foreign_perp_factory` posts none on purpose. `symbol` is the engine's spelling (`ETHUSDT.P`); a
/// SPOT symbol has no leverage and is refused. `leverage` is a whole number and is written as given:
/// the lane's probe refused a figure above the symbol's maximum. `Err` is the venue's own words, and says
/// whether the outcome is unknown ([`LeverageWriteError`]).
///
/// The tiers it writes at are the two the owner opened (decisions 0182 and 0184): `demo`, and the `live`
/// LANE TWIN (whose node side refuses an opening order on an extra symbol until the exchange was SEEN holding
/// the figure). `hosts` are the tier's own network's ([`BinanceUrls::resolve`] at the account's tier), so no
/// other host is reachable from here.
pub fn write_symbol_leverage(
    creds: &Credentials,
    hosts: BinanceUrls,
    symbol: &str,
    leverage: f64,
    link_id: Option<String>,
) -> Result<(), LeverageWriteError> {
    // a USER name, signed as its kept row's lane name (decision 0290); no row is a refusal in words
    let lane = crate::instrument::names::lane_name_of(symbol)
        .map_err(|refusal| LeverageWriteError::Refused(refusal.to_string()))?;
    let (api_symbol, is_perp) = split_symbol(&lane);
    if !is_perp {
        let refusal = BinanceLeverageRefusal::SpotSymbol { symbol: symbol.to_string() };
        return Err(LeverageWriteError::Refused(refusal.to_string()));
    }
    let rest = mk_perp_rest(
        hosts,
        creds,
        &api_symbol,
        SymbolProperties::default(),
        link_id,
        leverage,
        &VenueClock::default(),
    );
    post_symbol_leverage(&rest)?;
    tracing::info!(target: "vike_binance::execution", %symbol, leverage, "set-leverage POSTED for a perpetual admitted on demand (the gatewayhub reads it back)");
    Ok(())
}

/// **The account's modes put right for a perpetual BEFORE the gatewayhub's read-only leverage probe**
/// (`VenueAdapter::heal_modes`): [`crate::account::modes::heal_at_start`] itself (the account's position mode, then the
/// symbol's margin type; each read first and written only on a mismatch, DEMO host only: a mainnet client reads
/// and writes no mode) on a client of its own, the very switch `run_perp` makes at the start, so the probe
/// reads the mode the engine models instead of the one the start was about to fix. A SPOT symbol has no
/// modes. `Err` is the venue's words for a mode that could not be switched (what blocks it); the lane goes on
/// to the probe, which refuses the figure under its own reason.
pub fn heal_symbol_modes(
    creds: &Credentials,
    hosts: BinanceUrls,
    symbol: &str,
    link_id: Option<String>,
) -> Result<(), String> {
    let lane =
        crate::instrument::names::lane_name_of(symbol).map_err(|refusal| refusal.to_string())?;
    let (api_symbol, is_perp) = split_symbol(&lane);
    if !is_perp {
        return Ok(());
    }
    let rest = mk_perp_rest(
        hosts,
        creds,
        &api_symbol,
        SymbolProperties::default(),
        link_id,
        DEFAULT_LEVERAGE,
        &VenueClock::default(),
    );
    modes_refusal(crate::account::modes::heal_at_start(&rest))
}

/// The words of a mode switch the venue refused, from [`crate::account::modes::heal_at_start`]'s two outcomes; `Ok`
/// for every other outcome (right already, switched, unread, left alone).
pub(crate) fn modes_refusal(
    outcomes: (
        vike_bridge_core::venue_adapter::ModeHealOutcome,
        vike_bridge_core::venue_adapter::ModeHealOutcome,
    ),
) -> Result<(), String> {
    use vike_bridge_core::venue_adapter::ModeHealOutcome::Refused;
    let refused: Vec<String> = [outcomes.0, outcomes.1]
        .into_iter()
        .filter_map(|outcome| if let Refused(why) = outcome { Some(why) } else { None })
        .collect();
    if refused.is_empty() { Ok(()) } else { Err(refused.join("; ")) }
}

/// [`write_symbol_leverage`]'s post and its error words, over any transport. A timeout, a 5xx or a status-unknown
/// code (`transport::error_codes::TAXONOMY`'s `submit_outcome_unknown`) is [`LeverageWriteError::Ambiguous`]: the POST may have landed,
/// so the lane reads the venue back instead of reporting a refusal.
fn post_symbol_leverage<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
) -> Result<(), LeverageWriteError> {
    rest.set_leverage().map_err(|e| {
        let ambiguous = crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(e.code);
        LeverageWriteError::of_set_leverage(&rest.symbol, ambiguous, e)
    })
}

/// Why an order on `label` is refused when its grid cannot be read: never a guessed or BTC-shaped
/// fallback grid.
fn no_grid_reason(label: &str) -> String {
    format!(
        "binance: cannot sign {label:?}: exchangeInfo has no usable grid for it (unknown symbol, a \
         symbol this account's network does not list (the demo list is shorter than mainnet's, TradFi \
         perpetuals above all), or the venue was unreachable); the order was not sent"
    )
}

/// **One money model per engine** (decision 0258 item 7, `vike_model::MoneyModel::admits`), held again for an
/// order the node's door never saw: another perpetual is signed only when its KEPT row's model is the mount's
/// (`engine`), the same kind and the same settle coin. A COIN-M contract of another coin, a USDⓈ-M contract on a
/// COIN-M engine, a COIN-M contract on a USDⓈ-M engine and a USDⓈ-M contract of another settle coin (`BTCUSDC.P`
/// on a `BTCUSDT.P` engine: the owner's ruling of 2026-11-22) are each refused in words before anything is
/// fetched or sent. A mount whose row states no model fences nothing here: every binance futures row is USDⓈ-M
/// or COIN-M with its margin asset, and the door judges first.
pub(crate) fn coin_fence(label: &str, engine: Option<&MoneyModel>) -> Result<(), String> {
    let Some(engine) = engine else { return Ok(()) };
    let theirs = vike_catalog::kept_row_of(crate::VENUE, label)
        .and_then(|row| MoneyModel::of_contract(&row.contract));
    // a contract whose row states no money model is the other kind
    let refusal = theirs.as_ref().map_or(Some(MoneyModelRefusal::Kind), |t| engine.admits(t).err());
    let (t, ours) =
        (theirs.as_ref().map_or("an unnamed coin", |t| t.settle.as_str()), &engine.settle);
    match (refusal, engine.kind) {
        (None, _) => Ok(()),
        (Some(MoneyModelRefusal::SettleCoin), _) => Err(format!(
            "binance: {label:?} settles in {t} but this engine keeps its books in {ours}, and one engine holds \
             one settle coin (the venue keeps one wallet per coin), so the order was not sent. Use an account \
             mounted on a contract of that coin"
        )),
        (Some(MoneyModelRefusal::Kind), NotionalKind::Inverse) => Err(format!(
            "binance: {label:?} is not a COIN-M contract, but this engine is mounted on one and keeps its books \
             in {ours}: an engine counts money one way, so the order was not sent. Use an account mounted on a \
             USDⓈ-M contract"
        )),
        (Some(MoneyModelRefusal::Kind), NotionalKind::Linear) => Err(format!(
            "binance: {label:?} is a COIN-M (coin-margined, {t}) contract and this engine is mounted on a USDⓈ-M \
             one: an engine counts money one way, so the order was not sent. Use an account mounted on a COIN-M \
             contract of that coin"
        )),
    }
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: a COIN-M contract reads its
/// `dapi` row (`crate::execution::rest::coin_m`), any other perp the fapi `/fapi/v1/exchangeInfo`, spot
/// `/api/v3/exchangeInfo`, all on `hosts` (the last two through the family's
/// `crate::family::execution::fetch_grid`). `Err` names the failed read or the missing row: there is no
/// fallback grid, so the caller refuses what it cannot format (owner, 2026-11-13). So the adapter formats
/// orders on the correct grid for ANY symbol on the right venue, and on no other.
///
/// `hosts` are the EFFECTIVE network's — the caller looked them up at the tier the account's network
/// binds (decision 0095: `vike_bridge_core::venue_adapter`'s `bound_tier`, then [`BinanceUrls::resolve`]; STEP 2:
/// `VenueAdapterBinance::start` does it ONCE per start from `AdapterInputs::tier` and
/// threads them, never re-read here). So a mainnet start reads the mainnet grid because its caller said `Live`,
/// not because this function consulted global env behind the caller's back.
pub fn fetch_binance_properties(
    hosts: BinanceUrls,
    creds: &Credentials,
    symbol: &str,
) -> Result<(SymbolProperties, String), String> {
    let (api_symbol, is_perp) = split_symbol(symbol);
    let _ = creds; // exchangeInfo is a PUBLIC endpoint (no signing) — creds kept for signature parity
    if is_perp && crate::instrument::names::is_coin_m(&api_symbol) {
        // A COIN-M contract's grid is its `dapi` row's, on the account's network (`crate::execution::rest::coin_m`).
        return coin_m::fetch_mount(hosts.dapi_rest, symbol)
            .map(|m| (m.properties, m.coin))
            .map_err(|e| e.msg);
    }
    let source = FamilyGridSource {
        venue: "binance",
        spot_rest: hosts.spot_rest,
        perp_rest: hosts.fapi_rest,
        spot_path: PATH_EXCHANGE_INFO,
        perp_path: FAPI_PATH_EXCHANGE_INFO,
        spot_gate: crate::transport::ratelimit::spot_rest_gate,
        perp_gate: crate::transport::ratelimit::perp_rest_gate,
    };
    execution::fetch_grid(&source, symbol, |info, api_symbol| {
        let instrument = parse_binance_perp_instruments(info).get(api_symbol)?.clone();
        Some((instrument.properties, instrument.base_asset))
    })
}

/// Fetch + map recent SPOT order/trade history to events (the audit-A3 replay body). Shared by the
/// pump's reconnect-resync AND the `run_loop` fill-sentinel; both feed the same dedup'd core ingest.
///
/// It reads the mounted `symbol` AND each of `foreign` (the engine's other spot symbols: the admitted ones
/// for the reconnect-resync, the ones a command touched for the gap-sentinel; decision 0174's reopener). The
/// mounted symbol is read FIRST with the requests it has always made; a foreign one of the spot lane with the
/// same two on its own name (`vike_bridge_core::execution_client::resync::replay_foreign`: the walk ends at the
/// first answer that says "slow down"), its fills carrying the symbol-qualified trade id the stream spells. `pub`
/// and generic over the transport like [`perp_history_events`]. The body is the family's
/// (`crate::family::execution::replay_spot_history`); the ids the venue echoes are read with the
/// link id this client's orders are placed under.
pub fn spot_history_events_of<S: Signer, T: RestTransport>(
    rest: &VenueRestBinanceSpot<S, T>,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    execution::replay_spot_history(rest, symbol, foreign)
}

/// Perp twin of [`spot_history_events_of`] (fapi `allOrders` + `userTrades` → `map_binance_perp_history`),
/// plus the conditional orders: `allOrders` does not list an algo order, so `GET /fapi/v1/allAlgoOrders`
/// ([`VenueRestBinancePerp::get_all_algo_orders`], weight 5) supplies the terminal rows of a stop that was
/// cancelled / expired / rejected while the stream was down ([`crate::execution::rest::algo::map_algo_history`]). All
/// three reads are best effort: a failed one costs its own rows only.
/// `pub` and generic over the transport so the offline tests drive it with a scripted one; the
/// production callers pass the `UreqTransport` client.
pub fn perp_history_events<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
    symbol: &str,
) -> Vec<Event> {
    // The ids the venue echoes are read with the link id this client's orders are placed under.
    let link_id = rest.link_id.as_deref();
    execution::perp_history_read(
        rest,
        &rest.symbol,
        &|read| read.unwrap_or_else(|_| serde_json::json!([])),
        |orders, trades| map_binance_perp_history(orders, trades, "binance", symbol, link_id),
    )
}

/// [`perp_history_events`] for the mounted series `symbol` AND each of `foreign` (the engine's other perp
/// series labels: the admitted ones for the reconnect-resync, the ones a command touched for the
/// gap-sentinel; decision 0174's reopener). The mounted symbol is read FIRST with the three requests it has
/// always made; a foreign one of the perp lane with the same three on its exchange symbol
/// (`vike_bridge_core::execution_client::resync::replay_foreign`: the walk ends at the first answer that says
/// "slow down"), its fills and liquidations carrying the symbol-qualified trade id the stream spells. The
/// body is the family's (`crate::family::execution::replay_perp_history`, the Algo Service's read
/// through the client's `FamilySignedRest::conditional_history_rows`). `pub` and generic over the transport
/// like [`perp_history_events`].
pub fn perp_history_events_of<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    execution::replay_perp_history(rest, symbol, foreign)
}

/// **The live Binance exec client** (spot AND perp, chosen by the symbol's `.P` suffix): the shared
/// [`ExecutionClientActor`] itself, built with this venue's hooks (its by-order-client-id confirm, and its restored orders taught
/// to the router), so no wrapper forwards its methods by hand. `submit`/`cancel`/`modify` enqueue onto the
/// REST thread (non-blocking); every venue event returns through the core ingest. Dropping it (or
/// `detach`) stops the REST thread AND the user-data pump — deterministic teardown, the actor's own. The
/// start hands it the HALT sentinel with `ExecutionClientActor::with_halt_path` (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
///
/// The exec thread fetches the instrument grid, starts the user-data pump, then drains commands. `hosts`
/// are the REST/WS addresses of the account's network ([`BinanceUrls::resolve`]). `symbol` is the CORE
/// label — a trailing `.P` selects the USDⓈ-M perp venue, otherwise spot (e.g. `"BTCUSDT.P"` vs
/// `"BTCUSDT"`). The real grid is fetched at startup so any symbol formats correctly; until it loads every
/// order is refused in words and the grid fetched again (`crate::family::execution::await_grid`): there is no
/// fallback grid (owner, 2026-11-13).
///
/// `properties_rec` is opt-in: when present, the REAL fetched instrument grid is
/// recorded into the PIT properties store right after the startup exchangeInfo fetch resolves (via
/// [`crate::instrument::record::record_properties_all`], keyed by the CORE `.P`-preserving label so it matches the
/// series). `properties_rec` is moved into the exec thread (`PropertiesRecorder` is `Send + Sync`).
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight down to the venue's
/// `run_user_resync` call (spot or perp, whichever `symbol` routes to) — `Some` fires the recon
/// trigger after every reconnect's event-replay settles; `None` replays events only.
///
/// `link_id` (unified cross-venue attribution, task 6) is the Binance Broker/Link id resolved ONCE by the
/// caller (`crate::adapter`'s `VenueAdapterBinance::start`, via `attribution_code_from` over
/// `AdapterInputs::secrets`) and stamped onto every order-client-id string the SUBMIT-capable REST client
/// sends the venue; `None` (unconfigured) reproduces the pre-task-6 wire body byte-for-byte.
///
/// `leverage` is the account leverage the PERP driver posts once at start (`POST /fapi/v1/leverage`),
/// resolved ONCE at the start site from the operator's `[risk]` budget via `vike_bridge_core::leverage::leverage_for` — so the venue
/// account and the RiskGate's margin math run on the SAME number. Inert on a SPOT symbol: spot has no
/// leverage endpoint and never reads it.
///
/// `trade_lite_fill` is the `venue.binance.trade_lite_fill` setting, read once by the start through
/// [`crate::execution::user_data::perp::trade_lite_fill`]; inert on a spot symbol.
///
/// `admitted` is the engine's admitted-symbol list: `crate::adapter` passes the SAME `Arc` it hands the
/// reconcile client, which is the one told each admission, and the reconnect-resync replays the history of
/// each symbol on it ([`perp_history_events_of`], `spot_history_events_of`).
///
/// `clock` is the account's one offset from the venue's clock, the SAME one `crate::adapter` hands the
/// reconcile client: the exec thread reads it at its start, and every signer here and the subscribe frame
/// stamp from it.
#[expect(clippy::too_many_arguments)]
pub fn spawn_with_recorder(
    hosts: BinanceUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
    trade_lite_fill: bool,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) -> ExecutionClientActor {
    // `symbol` is the mount's USER name (decision 0290): the bridge speaks its kept row's lane name (`BTCUSD_PERP.P` for
    // `BTCUSD.P`, `BTCUSDT_261225.P` for `BTCUSDT_261225`), whose `.P` picks the futures lane. A name with no row is a
    // client that refuses every command in words and opens nothing (the account's start refuses one first).
    let money = vike_catalog::kept_row_of(crate::VENUE, &symbol)
        .and_then(|row| MoneyModel::of_contract(&row.contract));
    let symbol = match crate::instrument::names::lane_name_of(&symbol) {
        Ok(lane) => lane,
        Err(why) => {
            tracing::error!(target: "vike_binance::execution", %symbol, "{why}");
            let why = why.to_string();
            return ExecutionClientActor::spawn("binance-exec", events.clone(), move |rx| {
                run_loop(&VenueRestRefusing { why }, &events, rx, Vec::new, DEFAULT_FILL_SENTINEL);
            });
        }
    };
    let ledger = Arc::new(DispatchLedger::default());
    // Which symbol each FOREIGN order was submitted on: written by the exec thread's router,
    // read by the confirm worker's, so a status query lands on the order's own symbol.
    let order_client_ids = Arc::new(OrderClientIdSymbols::default());
    let adopt_order_client_ids = order_client_ids.clone();
    let spec = BinanceExecution { hosts, link_id, trade_lite_fill, coin: None, money };
    let confirm = execution::confirm_fn(
        &spec,
        &creds,
        &symbol,
        ledger.clone(),
        order_client_ids.clone(),
        &clock,
    );
    // A perp mount's funding is polled from the income history on a thread of its own, ONE
    // producer per book, on a client of the mount's own book ([`spawn_funding_poll`]: fapi for
    // USDⓈ-M, dapi for COIN-M); the spot lane pays none.
    let (api_symbol, perp) = split_symbol(&symbol.to_uppercase());
    let funding = perp.then(|| {
        spawn_funding_poll(hosts, creds.clone(), api_symbol, clock.clone(), events.clone())
    });
    let actor = ExecutionClientActor::spawn("binance-exec", events.clone(), move |rx| {
        run(
            spec,
            creds,
            symbol,
            events,
            rx,
            properties_rec,
            on_reconcile,
            leverage,
            ledger,
            order_client_ids,
            admitted,
            clock,
        )
    })
    .with_confirm(confirm)
    // Teach the router which symbol each restored order rests on (the core announces them at boot
    // and after a gatewayhub restart): a cancel of an order this process never submitted would
    // otherwise go to the MOUNTED symbol, where the venue answers "unknown order" and the adapter
    // reports a LIVE order cancelled.
    .with_adopt_restored(Arc::new(move |orders| adopt_order_client_ids.adopt(orders)));
    match funding {
        Some(poll) => actor.with_poller(poll),
        None => actor,
    }
}

/// Spawn the funding poll thread ([`crate::execution::funding::income_poller`]) through the shared
/// `vike_bridge_core::exec_actor::funding_poll::spawn_funding_poll` (its cadence, spawn floor and warn-once streak),
/// attached with [`ExecutionClientActor::with_poller`]: its OWN REST client (never the command loop's) for the mount's
/// `api_symbol`, so it sits on the mount's own book ([`BinanceUrls::futures_rest`]: fapi for USDⓈ-M, dapi for COIN-M;
/// channel `trade` at the account's type, never a live fallback: the payments are the account's own), floored at THIS
/// spawn instant (the restart law: a payment before it is in the wallet the first reconcile pass adopts). A failed read
/// warns once per streak and the next one reads the same window again.
///
/// ⚠ One poller per START, account-wide (the engine's symbol filter narrows it): two perp mounts of one binance
/// account would each emit every payment. A mount is one account's lane today; whoever lifts that dedupes this
/// lane per account.
fn spawn_funding_poll(
    hosts: BinanceUrls,
    creds: Credentials,
    api_symbol: String,
    clock: VenueClock,
    events: EventSender,
) -> StopHandle {
    funding_poll::spawn_funding_poll("binance-funding-poll", events, move |thread| {
        // The grid, link id and leverage are inert: this client only GETs the income history.
        let rest = mk_perp_rest(
            hosts,
            &creds,
            &api_symbol,
            SymbolProperties::default(),
            None,
            DEFAULT_LEVERAGE,
            &clock,
        );
        let mut poller = income_poller(&rest, thread.floor_ms());
        thread.run(
            // a payment leaves under its instrument's USER name (decision 0290)
            |now| poller.poll(now).map(crate::instrument::names::speak_user_names),
            |e| {
                tracing::warn!(
                    target: "vike_binance",
                    error = %e,
                    "funding poll read failed; retrying each cadence (warns once per streak)"
                );
            },
        );
    })
}

/// The exec thread: resolve the grid (routed by `.P`), record it, then dispatch to the spot or perp
/// driver. ALL network I/O lives on this thread — off the single-writer core.
// Internal exec-thread plumbing: `env` (rung 5) joins the seven fields the driver already threads
// (creds, symbol, grid, events, command rx, PIT recorder, recon trigger). Bundling them would only
// move the arg list into a struct with no readability gain, so allow the count here.
#[expect(clippy::too_many_arguments)]
fn run(
    spec: BinanceExecution,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<mpsc::Sender<()>>,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) {
    let hosts = spec.hosts;
    // `hosts` arrive EFFECTIVE: the start resolved the tier the account's network binds (decision
    // 0095) once (`bound_tier`), looked the addresses up at that tier (`BinanceUrls::resolve`) and
    // threaded them in, so every downstream host (the REST clients, the user-data WS, the in-thread
    // grid fetch) binds to whatever network the caller selected. This thread deliberately does NOT
    // re-resolve it (STEP 2) — a spawned thread re-reading global state is exactly how a start ends up
    // signing mainnet creds against demo hosts.
    let (api_symbol, is_perp) = split_symbol(&symbol);
    // ⚠ A COIN-M MOUNT IS ITS ROW (`crate::execution::rest::coin_m`): its grid, its face and its settle coin are read off the
    // network's `dapi` list before anything is built, and a COIN-M name whose row cannot be read is REFUSED
    // here (no client, no leverage POST, no socket: the shared loop over `VenueRestRefusing`, which answers every
    // command with the reason in the one form of an arming refusal, owner ruling B-15), never signed on a
    // guessed grid, whose step would be a coin's fraction where the venue counts contracts. The thread stays
    // alive to carry the reason: returning would reject through the actor's dead-thread arm, in words
    // ("venue exec thread unavailable") that name nothing an operator can act on.
    let mounted = if is_perp && crate::instrument::names::is_coin_m(&api_symbol) {
        match coin_m::fetch_mount(hosts.dapi_rest, &symbol) {
            Ok(mount) => Some(mount),
            Err(why) => {
                let reason = coin_m_execution_refusal(&symbol, &why.msg);
                tracing::error!(target: "vike_binance::execution", %symbol, "{reason}");
                run_loop(
                    &VenueRestRefusing { why: reason },
                    &events,
                    rx,
                    Vec::new,
                    DEFAULT_FILL_SENTINEL,
                );
                return;
            }
        }
    } else {
        None
    };
    let coin = mounted.as_ref().map(|m| m.coin.clone());
    // The venue's REAL instrument grid for `symbol` (off the core thread): a COIN-M mount's is the row read
    // above, any other is fetched here, and until it loads every order is refused in words and the fetch
    // asked again. Recorded into the PIT properties store once it loads.
    let mut mounted = mounted.map(|mount| (mount.properties, mount.coin));
    let Some((properties, base_asset)) = execution::await_grid(
        &spec,
        &creds,
        &symbol,
        &order_client_ids,
        &clock,
        &events,
        rx,
        &properties_rec,
        || mounted.take().map_or_else(|| fetch_binance_properties(hosts, &creds, &symbol), Ok),
    ) else {
        return;
    };
    // A COIN-M mount's settle coin: every client sits on the `dapi` host of the network
    // ([`BinanceUrls::futures_rest`]) and the user-data stream is dapi's (its listenKey and socket). The
    // foreign-symbol factory holds every other perpetual to the mount's money model ([`coin_fence`]).
    let spec = BinanceExecution { coin, ..spec };
    if is_perp {
        execution::run_perp(
            &spec,
            &creds,
            &symbol,
            &api_symbol,
            properties,
            events,
            rx,
            on_reconcile,
            leverage,
            ledger,
            order_client_ids,
            admitted,
            &clock,
        );
    } else {
        // SPOT has no set-leverage endpoint and no TRADE_LITE frame — `leverage` and
        // `trade_lite_fill` are not read on this lane.
        execution::run_spot(
            &spec,
            &creds,
            &symbol,
            properties,
            &base_asset,
            events,
            rx,
            on_reconcile,
            ledger,
            order_client_ids,
            admitted,
            &clock,
        );
    }
}

/// **The refusal a COIN-M MOUNT gets when its own `dapi` row cannot value it** (`why`: the list could not be
/// read, the contract is not TRADING on the account's network, or the row states no face or no coin).
///
/// The function keeps its name because the older records cite it (decisions 0170 and 0230). What it refuses
/// shrank with decision 0258's binance amendment: a COIN-M contract whose row reads is MOUNTED
/// (`crate::execution::rest::coin_m`); what is left is one whose grid, face (`contractSize` US dollars a contract) or settle
/// coin is unknown, and any guess (a guessed grid, a linear value, the common face of 100,
/// which is 10x wrong on every coin but BTC) would size every order wrong.
fn coin_m_execution_refusal(symbol: &str, why: &str) -> String {
    format!(
        "binance: cannot mount the COIN-M (coin-margined) contract {symbol:?}: {why}. Its grid, its face \
         (contractSize US dollars a contract) and its settle coin are its row's, and a guess would size every \
         order wrong, so nothing is sent (the account is retried at its next start)"
    )
}

/// **What the perp exec thread writes to the venue account at its start**, in order: the account's position
/// mode and the mounted symbol's margin type ([`crate::account::modes::heal_at_start`]: read, written only on a
/// mismatch, probed at EVERY start whether or not a leverage is stated, never blocking, and only on the DEMO
/// host: a mainnet client reads and writes no mode, owner ruling 2026-10-26), then the leverage (which a
/// mainnet start still posts, as it always did). The modes come first so the leverage lands on the mode the
/// engine models.
///
/// fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just leaves the
/// account's current leverage in place and orders still submit. UNCHANGED handling: the value is the
/// operator's, but a rejection still only warns and never kills the start.
pub(crate) fn perp_start_writes<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
    series_symbol: &str,
    leverage: f64,
) {
    let _ = crate::account::modes::heal_at_start(rest);
    if let Err(e) = rest.set_leverage() {
        let holds = crate::transport::error_codes::start_leverage_words(e.code);
        tracing::warn!(target: "vike_binance::execution", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — {holds} while the RiskGate sizes against the configured one");
    }
}

#[cfg(test)]
mod tests {
    //! Venue-specific wiring tests: `.P` host routing and rung-5 host resolution, asserted with NO
    //! network. The generic `run_loop` command→event tests (mock `VenueRest`) moved WITH `run_loop`
    //! to `vike_bridge_core::exec_actor::run_loop_tests`; the binance reality-tie
    //! (`capabilities_for("binance").supports_modify`) lives in the lib `capabilities_test`.
    use super::*;
    use vike_model::now_ms;

    /// The addresses a fresh box's rows give an account at `tier`: the release's seed
    /// (`vike_venues::venue_urls::VENUE_URL_SEED`).
    fn seeded(tier: AccountTier) -> BinanceUrls {
        BinanceUrls::resolve(&VenueUrls::seed("binance"), tier).expect("the seed holds every row")
    }

    /// `.P` routing (NO network): the trailing `.P` selects the fapi hosts + strips to the exchange
    /// symbol; a bare symbol stays spot. This is the exact decision `run` makes before it dispatches
    /// to `run_perp` / `run_spot`, so a perp symbol provably reaches the fapi REST + WS endpoints.
    #[test]
    fn dot_p_routes_to_fapi_hosts_and_strips_symbol() {
        // perp: `.P` stripped, fapi hosts chosen
        assert_eq!(split_symbol("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
        // spot: unchanged, non-perp
        assert_eq!(split_symbol("BTCUSDT"), ("BTCUSDT".to_string(), false));
        let demo = seeded(AccountTier::Demo);
        assert_eq!(demo.futures_rest("BTCUSDT"), demo.fapi_rest);
        assert_eq!(coin_m::margining_of("BTCUSDT"), vike_model::Margining::Linear);
    }

    /// **The addresses the drivers dial are byte for byte the ones they dialled before they were rows**
    /// (the release's seed, path included), and ONLY `Live` selects mainnet; `Demo`/`Paper` (and, via the
    /// absent-creds-is-the-live-gate rule, any uncredentialed run) stay on the exact demo hosts.
    #[test]
    fn each_tiers_hosts_are_the_addresses_dialled_before_they_were_rows() {
        let live = seeded(AccountTier::Live);
        assert_eq!(
            [live.spot_rest, live.fapi_rest, live.dapi_rest],
            ["https://api.binance.com", "https://fapi.binance.com", "https://dapi.binance.com"]
        );
        assert_eq!(
            [live.spot_websocket, live.fapi_websocket, live.dapi_websocket],
            [
                "wss://ws-api.binance.com/ws-api/v3",
                "wss://fstream.binance.com/ws",
                "wss://dstream.binance.com/ws"
            ]
        );
        assert_eq!(live.ty, UrlType::Live);
        for tier in [AccountTier::Demo, AccountTier::Paper] {
            let demo = seeded(tier);
            assert_eq!(
                [demo.spot_rest, demo.fapi_rest, demo.dapi_rest],
                [
                    "https://demo-api.binance.com",
                    "https://demo-fapi.binance.com",
                    "https://demo-dapi.binance.com"
                ],
                "{tier:?} must stay on demo hosts"
            );
            assert_eq!(
                [demo.spot_websocket, demo.fapi_websocket, demo.dapi_websocket],
                [
                    "wss://demo-ws-api.binance.com/ws-api/v3",
                    "wss://fstream.binancefuture.com/ws",
                    "wss://dstream.binancefuture.com/ws"
                ],
                "{tier:?} must stay on demo hosts"
            );
            assert_eq!(demo.ty, UrlType::Demo);
        }
    }

    /// **A demo start never reaches a live address**: its six lookups are channels `trade` and `user`, which
    /// never fall back to the live rows, so a table that holds only live rows starts no demo account (the
    /// refusal names the row), and an empty table starts none at all.
    #[test]
    fn a_demo_start_with_no_demo_rows_is_refused_and_never_falls_back_to_live() {
        let live_only = VenueUrls::seed_typed("binance", UrlType::Live);
        assert!(BinanceUrls::resolve(&live_only, AccountTier::Live).is_ok());
        let why = BinanceUrls::resolve(&live_only, AccountTier::Demo).unwrap_err().to_string();
        assert!(why.contains("binance") && why.contains("demo trade rest"), "{why}");
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert!(BinanceUrls::resolve(&VenueUrls::default(), tier).is_err(), "{tier:?}");
        }
    }

    /// The leverage this arm POSTs to `/fapi/v1/leverage` is the OPERATOR's `[risk] max_leverage`,
    /// and an unset profile still yields the historical literal (`2.0`) — the property that makes
    /// this change safe to ship onto a live account. NO network: `leverage_for` is the pure
    /// decision `crate::adapter`'s `VenueAdapterBinance::start` calls once, before anything is spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        vike_bridge_core::policy_pins::assert_leverage_follows_the_risk_budget(
            vike_bridge_core::leverage::leverage_for,
            DEFAULT_LEVERAGE,
            2.0,
            "the historical hardcoded /fapi/v1/leverage value",
        );
    }

    /// **The family plane's lines keep binance's target, `vike_binance::execution`, and their words and fields.**
    /// A `tracing` target is a literal, so the family's drivers write through `BinanceExecution::log`
    /// (`crate::family_execution_log!` with this module's target); before the move these two lines were this
    /// file's own `run` and `run_spot`/`run_perp`, under the same target.
    #[test]
    fn the_family_planes_lines_are_written_under_vike_binance_execution() {
        let ((), logs) = vike_log::capture::captured(|| {
            BinanceExecution::log(FamilyExecutionLog::ResyncReplayed { symbols: 2, events: 5 });
        });
        type Line = (String, String, String, Vec<(String, String)>);
        let lines: Vec<Line> = logs
            .iter()
            .map(|e| {
                let fields = e.fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                (e.level.to_string(), e.target.clone(), e.message.clone(), fields)
            })
            .collect();
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            lines,
            vec![
                (
                    "INFO".to_string(),
                    "vike_binance::execution".to_string(),
                    "reconnect resync: replayed the history of the admitted symbols after the mounted one"
                        .to_string(),
                    vec![pair("events", "5"), pair("symbols", "2")],
                ),
            ]
        );
    }

    /// The gap-sentinel both drivers hand `run_loop` is bridge-core's mechanism default — the five
    /// seconds this file's private `FILL_SENTINEL` held before the number moved there — so a retune
    /// of that default is a deliberate edit here too, beside the cross-pin in vike-bridge-core.
    #[test]
    fn the_drivers_gap_sentinel_is_the_five_second_default() {
        assert_eq!(DEFAULT_FILL_SENTINEL, std::time::Duration::from_secs(5));
    }

    /// **A perpetual admitted on demand is written through the bridge's OWN set-leverage verb.** The
    /// client is the order client's build for the EXCHANGE symbol (the `.P` stripped, upper-cased) on
    /// the tier's fapi host, the POST names that symbol and a whole-number leverage, a refusal keeps the
    /// venue's words, and a SPOT symbol is refused before any client is built.
    #[test]
    fn a_perpetual_admitted_on_demand_is_written_through_the_bridges_own_set_leverage() {
        crate::instrument::names::kept_list_for_tests();
        use std::cell::RefCell;

        /// `(method, path, params)` of every call.
        type Sent = Vec<(String, String, Vec<(String, String)>)>;
        struct Wire {
            sent: RefCell<Sent>,
            answer: Result<serde_json::Value, vike_bridge_core::transport::VenueApiError>,
        }
        impl RestTransport for Wire {
            fn signed(
                &self,
                _base_url: &str,
                path: &str,
                method: &str,
                params: &[(&str, String)],
                _signer: &dyn Signer,
            ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
                let params = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
                self.sent.borrow_mut().push((method.to_string(), path.to_string(), params));
                self.answer.clone()
            }
            vike_bridge_core::no_public_call!(unreachable: "a set-leverage write reads nothing public");
        }
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };

        // The client: the exchange symbol, the figure, the tier's host, the order window.
        let demo = mk_perp_rest(
            seeded(AccountTier::Demo),
            &creds,
            &split_symbol("ethusdt.p".to_uppercase().as_str()).0,
            SymbolProperties::default(),
            None,
            10.0,
            &VenueClock::default(),
        );
        assert_eq!((demo.symbol.as_str(), demo.leverage), ("ETHUSDT", 10.0));
        assert_eq!(demo.base_url, seeded(AccountTier::Demo).fapi_rest);
        assert_eq!((demo.ty, demo.margining), (UrlType::Demo, vike_model::Margining::Linear));

        let over = |answer| VenueRestBinancePerp {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Wire { sent: RefCell::new(Vec::new()), answer },
            base_url: demo.base_url.clone(),
            ty: demo.ty,
            margining: demo.margining,
            symbol: "ETHUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: 10.0,
            link_id: None,
            algo_order_client_ids: Default::default(),
        };

        // A clean write: ONE POST to /fapi/v1/leverage naming the exchange symbol and a whole number.
        let rest = over(Ok(serde_json::json!({ "leverage": 10, "symbol": "ETHUSDT" })));
        assert_eq!(post_symbol_leverage(&rest), Ok(()));
        let sent = rest.transport.sent.borrow();
        assert_eq!(sent.len(), 1, "one write");
        assert_eq!(
            (sent[0].0.as_str(), sent[0].1.as_str()),
            ("POST", crate::execution::rest::perp::PATH_SET_LEVERAGE)
        );
        assert!(sent[0].2.contains(&("symbol".to_string(), "ETHUSDT".to_string())));
        assert!(sent[0].2.contains(&("leverage".to_string(), "10".to_string())));
        drop(sent);

        // A refusal is the venue's words and the symbol.
        let rest = over(Err(vike_bridge_core::transport::VenueApiError {
            code: -4028,
            msg: "Leverage 125 is not valid".to_string(),
        }));
        let why = post_symbol_leverage(&rest).unwrap_err();
        assert!(matches!(why, LeverageWriteError::Refused(_)), "{why:?}");
        let why = why.to_string();
        assert!(why.contains("ETHUSDT") && why.contains("Leverage 125 is not valid"), "{why}");

        // A timeout (the transport's sentinel) or a status-unknown venue code (-1007) is AMBIGUOUS, not a
        // refusal: the POST may have landed, so the lane reads the venue back.
        for code in [vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS, -1007] {
            let rest = over(Err(vike_bridge_core::transport::VenueApiError {
                code,
                msg: "timed out".to_string(),
            }));
            let got = post_symbol_leverage(&rest).unwrap_err();
            assert!(matches!(got, LeverageWriteError::Ambiguous(_)), "{code}: {got:?}");
            assert!(got.to_string().contains("ETHUSDT"), "{got}");
        }

        // A SPOT symbol has no leverage: refused in words, before any client exists.
        let why = write_symbol_leverage(&creds, seeded(AccountTier::Demo), "ETHUSDT", 10.0, None)
            .unwrap_err();
        let spot = BinanceLeverageRefusal::SpotSymbol { symbol: "ETHUSDT".to_string() };
        assert_eq!(why, LeverageWriteError::Refused(spot.to_string()));
    }

    /// **A leverage write lands on the network of its tier and on no other** (the owner opened `live` for
    /// binance's lane twin, 2026-10-19: a `live` account's `POST /fapi/v1/leverage` is a REAL account's).
    /// The client the exec thread posts through is the one `mk_perp_rest` builds; a recording wire accepts
    /// ONLY the origin its tier selects and answers a venue-shaped error for any other, so a `live` client
    /// that reached the demo host, or a `demo` one that reached mainnet, fails here by name. The reader the
    /// gatewayhub confirms with takes the same origin ([`BinanceUrls::futures_rest`]).
    #[test]
    fn the_leverage_write_goes_to_the_host_of_its_tier_and_a_mismatch_is_refused() {
        use std::cell::RefCell;
        use std::rc::Rc;
        use vike_bridge_core::transport::VenueApiError;

        /// `(origin, method, path)` of every signed call.
        type Seen = Rc<RefCell<Vec<(String, String, String)>>>;
        struct Wire {
            accepts: &'static str,
            seen: Seen,
        }
        impl RestTransport for Wire {
            fn signed(
                &self,
                base_url: &str,
                path: &str,
                method: &str,
                _params: &[(&str, String)],
                _signer: &dyn Signer,
            ) -> Result<serde_json::Value, VenueApiError> {
                self.seen.borrow_mut().push((
                    base_url.to_string(),
                    method.to_string(),
                    path.to_string(),
                ));
                if base_url == self.accepts {
                    Ok(serde_json::json!({ "leverage": 10, "symbol": "BTCUSDT" }))
                } else {
                    Err(VenueApiError {
                        code: -1,
                        msg: format!("HOST MISMATCH: {base_url} is not {}", self.accepts),
                    })
                }
            }
            vike_bridge_core::no_public_call!(unreachable: "a set-leverage write reads nothing public");
        }
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let over = |base_url: &str, accepts: &'static str, seen: &Seen| VenueRestBinancePerp {
            signer: BinanceHmacSigner::new(&creds, || 0),
            transport: Wire { accepts, seen: Rc::clone(seen) },
            base_url: base_url.to_string(),
            ty: UrlType::Live,
            margining: vike_model::Margining::Linear,
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: 10.0,
            link_id: None,
            algo_order_client_ids: Default::default(),
        };

        let (demo, live) =
            (seeded(AccountTier::Demo).fapi_rest, seeded(AccountTier::Live).fapi_rest);
        for (tier, own, other) in [(AccountTier::Demo, demo, live), (AccountTier::Live, live, demo)]
        {
            // The origin the PRODUCTION builder selects for the tier, handed to the recording wire.
            let built = mk_perp_rest(
                seeded(tier),
                &creds,
                "BTCUSDT",
                SymbolProperties::default(),
                None,
                10.0,
                &VenueClock::default(),
            );
            assert_eq!(built.base_url, own, "{tier:?}: the exec thread's client");
            assert_eq!(
                seeded(tier).futures_rest("BTCUSDT"),
                own,
                "{tier:?}: the reader and the clients agree"
            );

            let seen: Seen = Rc::default();
            assert!(over(&built.base_url, own, &seen).set_leverage().is_ok(), "{tier:?}");
            assert_eq!(
                *seen.borrow(),
                [(
                    own.to_string(),
                    "POST".to_string(),
                    crate::execution::rest::perp::PATH_SET_LEVERAGE.to_string()
                )],
                "{tier:?}: ONE POST, on the tier's own origin"
            );

            // The reverse is refused by name: the same tier's wire turns the other network away, and the
            // post is not reported as done.
            let seen: Seen = Rc::default();
            let err = over(other, own, &seen).set_leverage().unwrap_err();
            assert!(err.msg.contains("HOST MISMATCH"), "{tier:?}: {}", err.msg);
            let why = post_symbol_leverage(&over(other, own, &seen)).unwrap_err().to_string();
            assert!(why.contains("HOST MISMATCH") && why.contains("BTCUSDT"), "{tier:?}: {why}");
        }
        // Paper and Demo are the demo network; only Live is mainnet (default-safe).
        assert_eq!(seeded(AccountTier::Paper).fapi_rest, demo);
        assert_ne!(live, demo);
    }

    /// **A COIN-M symbol's futures clients sit on the `dapi` host of their tier, every other symbol's where it
    /// always was** (decision 0258's binance amendment): the order, confirm, leverage, mode and foreign-symbol
    /// clients all take their host from [`BinanceUrls::futures_rest`], and their book (the COIN-M rewrite, the
    /// clock path) from the same name ([`coin_m::margining_of`], [`coin_m::time_path_for`]), never from the host
    /// string.
    #[test]
    fn a_coin_m_symbols_clients_sit_on_dapi_and_every_other_where_it_was() {
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let properties = SymbolProperties::default();
        let clock = VenueClock::default();
        for tier in [AccountTier::Demo, AccountTier::Paper, AccountTier::Live] {
            let hosts = seeded(tier);
            for symbol in ["BTCUSD_PERP", "ETHUSD_PERP", "BTCUSD_261225"] {
                let rest = mk_perp_rest(hosts, &creds, symbol, properties, None, 2.0, &clock);
                assert_eq!(rest.base_url, hosts.dapi_rest, "{tier:?} {symbol}");
                assert_eq!(
                    (rest.ty, rest.margining),
                    (hosts.ty, vike_model::Margining::Inverse),
                    "{symbol}"
                );
                assert_eq!(rest.transport.time_path(), "/dapi/v1/time", "{symbol}");
                assert_eq!(
                    hosts.futures_rest(&format!("{symbol}.P")),
                    hosts.dapi_rest,
                    "the series label too"
                );
            }
            for symbol in ["BTCUSDT", "BTCUSDT_261225", "NVDAUSDT", "BTCUSDC"] {
                let rest = mk_perp_rest(hosts, &creds, symbol, properties, None, 2.0, &clock);
                assert_eq!(rest.base_url, hosts.fapi_rest, "{tier:?} {symbol}: the USDⓈ-M host");
                assert_eq!(rest.margining, vike_model::Margining::Linear, "{symbol}");
                assert_eq!(rest.transport.time_path(), "/fapi/v1/time", "{symbol}");
            }
        }
    }

    /// **One money model per engine, held again in the bridge** (decision 0258 item 7): a COIN-M engine signs
    /// COIN-M contracts of its own coin only, a USDⓈ-M engine USDⓈ-M contracts of its own settle coin only, each
    /// refusal in words before anything is fetched. The four sentences are pinned whole: they ride the journaled
    /// `OrderRejected`. The last one is the owner's ruling of 2026-11-22 (`BTCUSDC.P` on a `BTCUSDT.P` engine),
    /// worded as the settle-coin refusal already was.
    #[test]
    fn a_foreign_perpetual_of_another_money_or_coin_is_refused_before_it_is_fetched() {
        // decision 0290's user names: a COIN-M perpetual `ETHUSD.P`, a COIN-M quarterly `BTCUSD_261225`
        crate::instrument::names::kept_list_for_tests();
        let model = |user: &str| {
            vike_catalog::kept_row_of(crate::VENUE, user)
                .and_then(|row| MoneyModel::of_contract(&row.contract))
                .expect("a kept row with a money model")
        };
        let (coin_m, usdt) = (model("BTCUSD.P"), model("BTCUSDT.P"));
        assert_eq!(coin_fence("BTCUSD_261225", Some(&coin_m)), Ok(()), "a quarterly of the coin");
        assert_eq!(coin_fence("ETHUSDT.P", Some(&usdt)), Ok(()), "linear on linear of its coin");
        assert_eq!(
            coin_fence("ETHUSD.P", Some(&coin_m)),
            Err("binance: \"ETHUSD.P\" settles in ETH but this engine keeps its books in BTC, and one engine \
                 holds one settle coin (the venue keeps one wallet per coin), so the order was not sent. Use an \
                 account mounted on a contract of that coin"
                .to_string()),
            "another coin"
        );
        assert_eq!(
            coin_fence("ETHUSDT.P", Some(&coin_m)),
            Err("binance: \"ETHUSDT.P\" is not a COIN-M contract, but this engine is mounted on one and keeps its \
                 books in BTC: an engine counts money one way, so the order was not sent. Use an account mounted \
                 on a USDⓈ-M contract"
                .to_string()),
            "a linear contract"
        );
        assert_eq!(
            coin_fence("BTCUSD.P", Some(&usdt)),
            Err("binance: \"BTCUSD.P\" is a COIN-M (coin-margined, BTC) contract and this engine is mounted on a \
                 USDⓈ-M one: an engine counts money one way, so the order was not sent. Use an account mounted on \
                 a COIN-M contract of that coin"
                .to_string()),
            "COIN-M on a USDⓈ-M engine"
        );
        assert_eq!(
            coin_fence("BTCUSDC.P", Some(&usdt)),
            Err("binance: \"BTCUSDC.P\" settles in USDC but this engine keeps its books in USDT, and one engine \
                 holds one settle coin (the venue keeps one wallet per coin), so the order was not sent. Use an \
                 account mounted on a contract of that coin"
                .to_string()),
            "a USDⓈ-M contract of another settle coin"
        );
        // The factory runs the fence before its grid fetch: no network for a refused symbol.
        let refuse = execution::foreign_perp_factory(
            BinanceExecution {
                hosts: seeded(AccountTier::Demo),
                link_id: None,
                trade_lite_fill: false,
                coin: Some("BTC".to_string()),
                money: Some(coin_m),
            },
            Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            2.0,
            crate::transport::ratelimit::perp_rest_gate(),
            VenueClock::default(),
        );
        let err = refuse("ETHUSD.P").err().expect("refused offline");
        assert!(err.contains("settles in ETH"), "{err}");
    }

    /// The refusal of a COIN-M mount whose row cannot value it names the contract and the cause, and says
    /// nothing is sent.
    #[test]
    fn a_coin_m_mount_that_its_row_cannot_value_is_refused_in_words() {
        let why = coin_m_execution_refusal(
            "BTCUSD_PERP.P",
            "the network's COIN-M list has no TRADING row for X",
        );
        assert!(why.contains("BTCUSD_PERP.P") && why.contains("no TRADING row"), "{why}");
        assert!(why.contains("contractSize") && why.contains("nothing is sent"), "{why}");
    }

    /// **Both lanes' clients sign on the one clock they are built with, and read it from their own host's
    /// path.** A clock read an hour ahead moves the stamp of each; `crate::family::transport`'s tests hold what
    /// that read does and that zero skew moves no byte.
    #[test]
    fn the_clients_of_both_lanes_sign_on_the_clock_they_are_built_with() {
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        let properties = SymbolProperties::default();
        let clock = VenueClock::default();
        let demo = seeded(AccountTier::Demo);
        let spot = mk_spot_rest(demo, &creds, "BTCUSDT", properties, "BTC", None, &clock);
        let perp = mk_perp_rest(demo, &creds, "BTCUSDT", properties, None, 2.0, &clock);
        assert_eq!(spot.transport.time_path(), "/api/v3/time");
        assert_eq!(perp.transport.time_path(), "/fapi/v1/time");

        const AN_HOUR_MS: i64 = 3_600_000;
        clock.read_if_due(now_ms(), |_| Ok::<_, String>(AN_HOUR_MS)).unwrap();
        for (lane, query) in [
            ("spot", spot.signer.prepare(&[], "GET", "/x").query),
            ("perp", perp.signer.prepare(&[], "GET", "/x").query),
        ] {
            let stamp: i64 = query
                .split('&')
                .find_map(|p| p.strip_prefix("timestamp="))
                .unwrap()
                .parse()
                .unwrap();
            assert!((stamp - now_ms() - AN_HOUR_MS).abs() < 60_000, "{lane}: {query}");
        }
    }
}
