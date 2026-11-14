//! Aster live exec half — `ExecutionClient` over the signed REST, routing SPOT vs USDⓈ-M PERP by a
//! trailing `.P` on the symbol. The plane is the FAMILY's, shared with binance
//! (`vike_binance::family::execution`, the owner's ruling of 2026-11-13: the confirm, the
//! foreign-symbol factories, the grid fetch, the spot and perp drivers and the history replay): this
//! module is aster's facts for it (`AsterExecution`), its client builds, the actor and the thread's
//! start. NOTHING here is reimplemented.
//!
//! Two structural differences from binance (see `execution/rest/spot.rs`/`execution/rest/perp.rs`):
//!   * base URLs are the account's own exec rows, resolved ONCE at the start
//!     ([`AsterUrls::exec`], testnet vs mainnet by the account's tier) and threaded as that value
//!     through `AsterExecution`, `mk_*_rest` and [`fetch_aster_properties`] — NOT hardcoded DEMO consts;
//!   * requests are signed by the v3 EIP-712 [`SignerAster`](crate::transport::sign::SignerAster), not
//!     Binance HMAC (the `nonce` is µs, minted inside the signer).
//!
//! * SPOT (`BTCUSDT`): [`VenueRestAsterSpot`](crate::execution::rest::spot::VenueRestAsterSpot) over `/api/v3`.
//! * PERP (`BTCUSDT.P`): [`VenueRestAsterPerp`](crate::execution::rest::perp::VenueRestAsterPerp) over `/fapi/v3` (native
//!   modify/batch), plus a one-shot set-leverage at start.
//!
//! Built on the shared [`ExecutionClientActor`] scaffold: `submit`/`cancel`/`modify` enqueue onto ONE dedicated
//! OS thread that drives the venue REST (blocking, off the single-writer core). `submit_order` emits
//! `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously into the core ingest.
//!
//! `.P` ROUTING (the SAME idiom the market feeds use): the trailing `.P` is stripped to the EXCHANGE
//! symbol (`BTCUSDT`) that drives the venue REST endpoints, while the ORIGINAL `.P`-suffixed symbol
//! stays the CORE/series label the fills carry (so a perp's position/order key never collides with
//! its spot twin). A non-`.P` symbol is byte-identical to spot-only behavior.
//!
//! LIVE GATE: absent credentials never reach here — the composition root only spawns this when
//! `load_aster_credentials` yields `Some`. An account holding no `USER` + `PRIVATE_KEY` does not run.
//!
//! SCOPE (Phase 4-6 + audit A3). The REST submit/cancel/modify path is wired end-to-end here; the
//! listenKey user-data FILLS pump (spot + perp, Phase 5 / Task 9) streams authoritative fills/cancels
//! back through the family's `run_spot`/`run_perp`; per-order rate gates (`crate::transport::ratelimit`, Phase 6 /
//! Task 12) sit on every REST transport; and the PIT `SymbolProperties` recorder is wired into `run`'s
//! startup fetch (the family's `await_grid`, under the venue key `aster`). Audit A3 (history
//! gap-replay): on a WS reconnect the resync supervisor replays recent REST order/trade history
//! ([`spot_history_events_of`]/[`perp_history_events_of`]) so a fill/cancel/expire/reject that landed
//! during the reconnect gap is recovered (the core dedups the overlap by `trade_id` + FSM). The same
//! replay drives the `run_loop` gap-sentinel (a fill the first-connect subscribe race lost).

pub mod event_mapper;
pub mod rest;
pub mod resync;
pub mod user_data;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use vike_bridge_core::confirm::DispatchLedger;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::exec_actor::{ExecutionClientActor, ExecutionClientActorCommand};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncHandle;
use vike_execution::EventSender;
use vike_model::SymbolProperties;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use vike_binance::family::execution::dispatch::split_symbol;
use vike_binance::family::execution::{
    self, FamilyExecutionLog, FamilyExecutionSpec, FamilyGridSource,
};
use vike_binance::family::transport::RestTransportOnVenueClock;

use crate::execution::rest::perp::{VenueRestAsterPerp, parse_aster_perp_instruments};
use crate::execution::rest::spot::VenueRestAsterSpot;
use crate::transport::sign::SignerAster;
use crate::transport::urls::AsterUrls;

/// FALLBACK cross-margin leverage the perp exec thread sets once at start — the literal this arm
/// posted to `/fapi/v3/leverage` unconditionally before the operator's budget was threaded in
/// (testnet default; matches the smoke). Used ONLY when the operator expressed no leverage intent,
/// so a start with no `[risk] max_leverage` is byte-identical to every start before
/// [`leverage_for`] existed. ⚠ This venue starts against MAINNET in practice (only a `live`-tier
/// aster account holds credentials), which is exactly why the unset path must not move. NEVER read on
/// the submit path — `VenueRestAsterPerp::leverage` is consulted by `set_leverage` and nothing else.
const DEFAULT_LEVERAGE: f64 = 2.0;

/// The account leverage this arm POSTs at startup, resolved from the operator's `[risk]` budget.
///
/// Pure, and `pub` on purpose — the SAME shape binance uses: `VenueAdapterAster::start` calls this
/// ONCE per start with the `ProfileRisk` `vike-tradehub-core` hands it, then threads the resolved `f64` into
/// [`spawn_with_recorder`]. Nothing inside the adapter reaches for the
/// profile, so the number the venue account is configured with and the number the RiskGate sizes
/// against (`ProfileRisk::initial_margin_requirement`, `initial_margin = 1.0 / max_leverage`) can never disagree — that
/// disagreement was the bug this exists to close.
///
/// Unset (`None`, or a profile that never mentions `max_leverage`) ⇒ [`DEFAULT_LEVERAGE`],
/// byte-identical to before. See `vike_bridge_core::leverage` for the full rule.
///
/// ⚠ **NOT clamped to a venue ceiling, unlike bybit/okx** — `vike_bridge_core::leverage::
/// clamp_to_venue_cap` exists and this arm deliberately does not call it, for the same reason
/// binance does not (Aster's perp API is a Binance-fapi fork, so it inherits the gap verbatim).
/// VERIFIED LIVE 2026-08-05: `GET /fapi/v3/exchangeInfo` (735 KB, every symbol) contains ZERO
/// case-insensitive `leverage` matches, and `/fapi/v3/leverageBracket` answers `{"code":-1102,
/// "msg":"Mandatory parameter 'nonce' was not sent…"}` — i.e. a WHOLE NEW EIP-712-SIGNED request
/// on the start path, not a field ride-along, and on a venue that starts against MAINNET in
/// practice. Its brackets are risk-tiered like binance's, so a single flat cap does not exist to
/// be read. A too-high request is still rejected by the VENUE, which `AsterExecution`'s perp start
/// already warns of.
pub fn leverage_for(profile: Option<&vike_model::ProfileRisk>) -> f64 {
    vike_bridge_core::leverage::resolve_startup_leverage(profile, DEFAULT_LEVERAGE)
}

/// The concrete REST clients this exec half drives: blocking `ureq` + the v3 EIP-712 `SignerAster`.
/// The signer's µs nonce (Aster's `nonce` is µs, ±10 s of server time) comes from the client's one
/// [`VenueClock`], which a driver reads at its start.
type SpotRest = VenueRestAsterSpot<SignerAster, RestTransportOnVenueClock<UreqTransport>>;
type PerpRest = VenueRestAsterPerp<SignerAster, RestTransportOnVenueClock<UreqTransport>>;

/// One fresh SPOT REST client for `symbol` (its own transport + signer, so it can live on its own
/// thread), on the sapi base of `urls`. `base_asset` only matters for `connect()`
/// reconcile (unused on submit/cancel here), so an empty fallback is harmless.
fn mk_spot_rest(
    urls: AsterUrls,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    builder: Option<(String, String)>,
    clock: &VenueClock,
) -> SpotRest {
    mk_spot_rest_gated(
        urls,
        creds,
        symbol,
        properties,
        base_asset,
        builder,
        crate::transport::ratelimit::spot_rest_gate(),
        clock,
    )
}

/// [`mk_spot_rest`] on a GIVEN order-rate window: the mounted spot client and every foreign-symbol
/// spot client of one exec thread are built on clones of ONE gate, because the venue's order budget
/// is per account, not per symbol.
fn mk_spot_rest_gated(
    urls: AsterUrls,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    builder: Option<(String, String)>,
    gate: RateGate,
    clock: &VenueClock,
) -> SpotRest {
    VenueRestAsterSpot {
        signer: SignerAster::new(creds, clock.us()),
        transport: RestTransportOnVenueClock::new(
            UreqTransport::new("aster").with_rate_gate(gate),
            "aster",
            clock,
            crate::execution::rest::spot::PATH_TIME,
        ),
        base_url: urls.sapi_rest.to_string(),
        symbol: symbol.to_string(),
        properties,
        base_asset: base_asset.to_string(),
        builder,
    }
}

/// One fresh PERP (fapi) REST client for the EXCHANGE `api_symbol` (its own transport + signer), on
/// the fapi base of `urls`.
///
/// `leverage` is the mount-resolved account leverage ([`leverage_for`]) — the ONLY consumer is
/// `VenueRestAsterPerp::set_leverage`, which the perp start fires once.
fn mk_perp_rest(
    urls: AsterUrls,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    builder: Option<(String, String)>,
    leverage: f64,
    clock: &VenueClock,
) -> PerpRest {
    mk_perp_rest_gated(
        urls,
        creds,
        api_symbol,
        properties,
        builder,
        leverage,
        crate::transport::ratelimit::perp_rest_gate(),
        clock,
    )
}

/// [`mk_perp_rest`] on a GIVEN order-rate window (see [`mk_spot_rest_gated`]).
fn mk_perp_rest_gated(
    urls: AsterUrls,
    creds: &Credentials,
    api_symbol: &str,
    properties: SymbolProperties,
    builder: Option<(String, String)>,
    leverage: f64,
    gate: RateGate,
    clock: &VenueClock,
) -> PerpRest {
    VenueRestAsterPerp {
        signer: SignerAster::new(creds, clock.us()),
        transport: RestTransportOnVenueClock::new(
            UreqTransport::new("aster").with_rate_gate(gate),
            "aster",
            clock,
            crate::execution::rest::perp::PATH_TIME,
        ),
        base_url: urls.fapi_rest.to_string(),
        symbol: api_symbol.to_string(),
        properties,
        leverage,
        builder,
    }
}

/// **Aster's facts for the family's exec plane** (`vike_binance::family::execution`): the account's
/// exec hosts ([`AsterUrls::exec`]) and the Aster Code `(builder address, feeRate)` pair.
///
/// - The builder pair rides an ORDER-PLACING client only (the mounted one and each foreign one); a
///   client that only reads — the confirm, the resync, the gap sentinel — never builds an order's
///   params, so it carries none ([`AsterExecution::builder_for`]).
/// - The user data is two listenKey pumps (`crate::execution::user_data::spot`, `crate::execution::user_data::perp`).
/// - The perp start READS the position mode (`VenueRestAsterPerp::check_position_mode`: a hedge account is
///   said, never switched — the only order-capable aster key pair is a live one) and posts the leverage;
///   nothing else is written to the account.
/// - A foreign perpetual needs no fence and no preparation, and every foreign cancel or status takes the
///   full factory: none of binance's COIN-M fence, demo-host mode heals or no-fetch client is aster's.
/// - The lines it writes are `vike_aster::execution`'s.
#[derive(Debug, Clone)]
struct AsterExecution {
    urls: AsterUrls,
    builder: Option<(String, String)>,
}

impl AsterExecution {
    /// The builder pair for a client that places orders; none for one that only reads.
    fn builder_for(&self, places_orders: bool) -> Option<(String, String)> {
        if places_orders { self.builder.clone() } else { None }
    }
}

impl FamilyExecutionSpec for AsterExecution {
    const VENUE: &'static str = crate::transport::urls::VENUE;
    const DEFAULT_LEVERAGE: f64 = DEFAULT_LEVERAGE;
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
        places_orders: bool,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> SpotRest {
        let builder = self.builder_for(places_orders);
        match gate {
            Some(gate) => mk_spot_rest_gated(
                self.urls, creds, symbol, properties, base_asset, builder, gate, clock,
            ),
            None => mk_spot_rest(self.urls, creds, symbol, properties, base_asset, builder, clock),
        }
    }

    fn perp_rest(
        &self,
        creds: &Credentials,
        api_symbol: &str,
        properties: SymbolProperties,
        places_orders: bool,
        leverage: f64,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> PerpRest {
        let builder = self.builder_for(places_orders);
        match gate {
            Some(gate) => mk_perp_rest_gated(
                self.urls, creds, api_symbol, properties, builder, leverage, gate, clock,
            ),
            None => {
                mk_perp_rest(self.urls, creds, api_symbol, properties, builder, leverage, clock)
            }
        }
    }

    fn fetch_properties(
        &self,
        creds: &Credentials,
        symbol: &str,
    ) -> Option<(SymbolProperties, String)> {
        fetch_aster_properties(self.urls, creds, symbol).ok()
    }

    fn no_grid_reason(&self, label: &str) -> String {
        format!(
            "aster: cannot sign {label:?}: exchangeInfo has no usable grid for it (unknown symbol, or \
             the venue was unreachable); the order was not sent"
        )
    }

    fn spot_start(&self, rest: &SpotRest) {
        // The account's clock, read now so the listenKey and the first order sign on it.
        rest.transport.sync(&rest.base_url);
    }

    fn perp_start(&self, rest: &PerpRest, series_symbol: &str, leverage: f64) {
        // The account's clock, read now so the write below, the listenKey and the first order sign on it.
        rest.transport.sync(&rest.base_url);
        // Every order goes out ONE-WAY: a hedge-mode account is said at `error` (read only, never switched).
        let _ = rest.check_position_mode();
        // fapi change-leverage is idempotent (HTTP-200 even at target); best-effort — a failure just
        // leaves the account's current leverage in place and orders still submit. UNCHANGED handling:
        // the value is now the operator's, but a rejection still only warns and never kills the start.
        if let Err(e) = rest.set_leverage() {
            tracing::warn!(target: "vike_aster::execution", symbol = %series_symbol, leverage, code = e.code, msg = %e.msg, "set_leverage failed (continuing) — the account keeps its CURRENT leverage while the RiskGate sizes against the configured one");
        }
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
        crate::execution::user_data::spot::spawn_aster_user_data_with_resync(
            self.urls.sapi_rest.to_string(),
            creds.clone(),
            self.urls.sapi_websocket.to_string(),
            symbol.to_string(),
            events,
            fence,
            resync,
            on_reconcile,
            clock.clone(),
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
        clock: &VenueClock,
    ) -> WebsocketSessionUserResyncHandle {
        crate::execution::user_data::perp::spawn_aster_perp_user_data_with_resync(
            self.urls.fapi_rest.to_string(),
            creds.clone(),
            self.urls.fapi_websocket.to_string(),
            series_symbol.to_string(),
            events,
            fence,
            resync,
            on_reconcile,
            clock.clone(),
        )
    }

    fn log(line: FamilyExecutionLog) {
        vike_binance::family_execution_log!("vike_aster::execution", line)
    }
}

/// Fetch `symbol`'s REAL grid + base asset, ROUTING by the `.P` suffix: perp reads the fapi
/// `/fapi/v3/exchangeInfo` (through [`parse_aster_perp_instruments`]), spot reads
/// `/api/v3/exchangeInfo`, both on the account's exec hosts `urls` ([`AsterUrls::exec`]: the grid the
/// account's orders format on is read on the account's network), each on its lane's order-rate window.
/// The body is the family's (`vike_binance::family::execution::fetch_grid`). `Err` names the failed read
/// or the missing row: there is no fallback grid (owner, 2026-11-13). So the adapter formats orders on the
/// correct grid for ANY symbol on the right venue, and on no other.
pub fn fetch_aster_properties(
    urls: AsterUrls,
    creds: &Credentials,
    symbol: &str,
) -> Result<(SymbolProperties, String), String> {
    let _ = creds; // exchangeInfo is a PUBLIC endpoint (no signing) — creds kept for signature parity
    let source = FamilyGridSource {
        venue: "aster",
        spot_rest: urls.sapi_rest,
        perp_rest: urls.fapi_rest,
        spot_path: crate::transport::urls::SPOT_PATH_EXCHANGE_INFO,
        perp_path: crate::transport::urls::PERP_PATH_EXCHANGE_INFO,
        spot_gate: crate::transport::ratelimit::spot_rest_gate,
        perp_gate: crate::transport::ratelimit::perp_rest_gate,
    };
    execution::fetch_grid(&source, symbol, |info, api_symbol| {
        let instrument = parse_aster_perp_instruments(info).get(api_symbol)?.clone();
        Some((instrument.properties, instrument.base_asset))
    })
}

/// Fetch + map recent SPOT order/trade history to events (the audit-A3 replay body), the venue face
/// of the family's `replay_spot_history`: the mounted `symbol` first with the requests it has always
/// made (`allOrders` and aster's `userTrades`), then each of `foreign` of the spot lane, its fills
/// carrying the symbol-qualified trade id the stream spells; aster stamps no broker prefix, so no link
/// id. Shared by the pump's reconnect-resync AND the `run_loop` fill-sentinel. `pub` and generic over
/// the transport so the offline tests drive it with a scripted one.
pub fn spot_history_events_of<S: vike_bridge_core::signer::Signer, T: RestTransport>(
    rest: &VenueRestAsterSpot<S, T>,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    execution::replay_spot_history(rest, symbol, foreign)
}

/// Perp twin of [`spot_history_events_of`] (fapi `allOrders` + `userTrades`, the family's
/// `replay_perp_history`): the mounted series `symbol` first, then each of `foreign` (series labels,
/// `.P` included) on its exchange symbol, fills and liquidations carrying the symbol-qualified trade id
/// the stream spells.
pub fn perp_history_events_of<S: vike_bridge_core::signer::Signer, T: RestTransport>(
    rest: &VenueRestAsterPerp<S, T>,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    execution::replay_perp_history(rest, symbol, foreign)
}

/// **The live Aster exec client** (spot AND perp, chosen by the symbol's `.P` suffix): the shared
/// [`ExecutionClientActor`] itself, built with this venue's hooks (its by-order-client-id confirm, the family's `confirm_fn`
/// over `AsterExecution`, and its restored orders taught to the router), so no wrapper forwards its
/// methods by hand. `submit`/`cancel`/`modify` enqueue onto the REST thread (non-blocking); every venue
/// event returns through the core ingest. Dropping it (or `detach`) stops the REST thread —
/// deterministic teardown, the actor's own. The start hands it the HALT sentinel with
/// `ExecutionClientActor::with_halt_path` (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
///
/// The exec thread fetches the instrument grid on `urls`' hosts — the account's exec rows,
/// [`AsterUrls::exec`] — then drains commands. `symbol` is the CORE label — a trailing `.P` selects the
/// USDⓈ-M perp venue, otherwise spot (e.g. `"BTCUSDT.P"` vs `"BTCUSDT"`). The real grid is fetched at startup
/// so any symbol formats correctly; until it loads every order is refused in words and the grid fetched
/// again (the family's `await_grid`): there is no fallback grid (owner, 2026-11-13).
///
/// `properties_rec` is opt-in: when present, the REAL fetched instrument grid is
/// recorded into the PIT properties store right after the startup exchangeInfo fetch resolves (keyed by
/// the CORE `.P`-preserving label so it matches the series). `properties_rec` is moved into the exec
/// thread (`PropertiesRecorder` is `Send + Sync`).
///
/// `builder` — unified cross-venue attribution (task 8): the Aster Code `(builder address, feeRate)` pair,
/// resolved by the caller from `attribution_code_from(vars, "aster")` + `ASTER_BUILDER_FEE_RATE`. `None`
/// (unconfigured, the default) reproduces every order shape from before this parameter existed — see
/// `execution/rest/perp.rs`/`execution/rest/spot.rs`'s `build_order_params`.
///
/// `leverage` is the account leverage the PERP driver posts once at start (`POST /fapi/v3/leverage`),
/// resolved ONCE at the start site from the operator's `[risk]` budget via [`leverage_for`] — so the venue
/// account and the RiskGate's margin math run on the SAME number. Inert on a SPOT symbol: spot has no
/// leverage endpoint and never reads it.
///
/// `admitted` is the engine's admitted-symbol list: `crate::adapter` passes the SAME `Arc` it hands the
/// reconcile client, which is the one told each admission, and the reconnect-resync replays the history of
/// each symbol on it ([`perp_history_events_of`], [`spot_history_events_of`]).
///
/// `clock` is the account's one offset from the venue's clock, the SAME one `crate::adapter` hands the
/// reconcile client: the exec thread reads it at its start, and every signer here and the listenKey auth
/// take their nonce from it.
pub fn spawn_with_recorder(
    urls: AsterUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) -> ExecutionClientActor {
    let spec = AsterExecution { urls, builder };
    let ledger = Arc::new(DispatchLedger::default());
    // Which symbol each FOREIGN order was submitted on: written by the exec thread's router, read by
    // the confirm worker's, so a status query lands on the order's own symbol.
    let order_client_ids = Arc::new(OrderClientIdSymbols::default());
    let adopt_order_client_ids = order_client_ids.clone();
    let confirm = execution::confirm_fn(
        &spec,
        &creds,
        &symbol,
        ledger.clone(),
        order_client_ids.clone(),
        &clock,
    );
    ExecutionClientActor::spawn("aster-exec", events.clone(), move |rx| {
        run(
            spec,
            creds,
            symbol,
            events,
            rx,
            properties_rec,
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
    .with_adopt_restored(Arc::new(move |orders| adopt_order_client_ids.adopt(orders)))
}

/// The exec thread: resolve the grid (routed by `.P`), then dispatch to the family's spot or perp driver.
/// ALL network I/O lives on this thread — off the single-writer core. Aster wires no reconcile trigger
/// to its pumps (`on_reconcile` is `None`): the exec driver replays events only.
#[expect(clippy::too_many_arguments)]
fn run(
    spec: AsterExecution,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: VenueClock,
) {
    let (api_symbol, is_perp) = split_symbol(&symbol);
    // The venue's REAL instrument grid for `symbol` (off the core thread): until it loads every order is
    // refused in words and the fetch asked again, so orders format on the right tick/step or not at all.
    let Some((properties, base_asset)) = execution::await_grid(
        &spec,
        &creds,
        &symbol,
        &order_client_ids,
        &clock,
        &events,
        &rx,
        &properties_rec,
        || fetch_aster_properties(spec.urls, &creds, &symbol),
    ) else {
        return;
    };
    if is_perp {
        execution::run_perp(
            &spec,
            &creds,
            &symbol,
            &api_symbol,
            properties,
            events,
            rx,
            None,
            leverage,
            ledger,
            order_client_ids,
            admitted,
            &clock,
        );
    } else {
        // SPOT has no set-leverage endpoint — `leverage` is deliberately not threaded here.
        execution::run_spot(
            &spec,
            &creds,
            &symbol,
            properties,
            &base_asset,
            events,
            rx,
            None,
            ledger,
            order_client_ids,
            admitted,
            &clock,
        );
    }
}

#[cfg(test)]
mod tests {
    //! Aster-specific exec tests: `.P` SPOT/PERP routing + testnet host selection, asserted with NO
    //! network. The generic command→event wiring tests (a mock `VenueRest` proving `run_loop` routes
    //! Submit/Cancel/Modify and fires the gap-sentinel) live WITH the shared `run_loop` in
    //! `vike_bridge_core::exec_actor::run_loop_tests` — aster drives that SAME `run_loop`, so they are
    //! not re-hosted here (the pattern binance/bybit/okx/deribit already follow after the MockRest
    //! consolidation). The aster native-modify reality tie (`capabilities_for("aster").supports_modify`) lives
    //! in the lib `capabilities_test`.
    use super::*;
    use vike_bridge_core::exec_actor::DEFAULT_FILL_SENTINEL;

    /// `.P` routing (NO network): the trailing `.P` selects the perp venue + strips to the exchange
    /// symbol; a bare symbol stays spot. This is the exact decision `run` makes before it dispatches
    /// to the family's `run_perp` / `run_spot`. The hosts the drivers pin are the account's exec rows
    /// ([`AsterUrls::exec`], not hardcoded consts), so a `Demo` run provably reaches the Aster
    /// testnet REST/WS endpoints, and the clients are built on exactly those.
    #[test]
    fn dot_p_routes_to_fapi_hosts_and_strips_symbol() {
        // perp: `.P` stripped, perp flag set
        assert_eq!(split_symbol("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
        // spot: unchanged, non-perp
        assert_eq!(split_symbol("BTCUSDT"), ("BTCUSDT".to_string(), false));
        // the testnet hosts the drivers pin (the endpoints `run_perp`/`run_spot` actually use)
        let u = crate::transport::urls::seeded::testnet();
        assert_eq!(u.fapi_rest, "https://fapi.asterdex-testnet.com");
        assert_eq!(u.sapi_rest, "https://sapi.asterdex-testnet.com");
        assert_eq!(u.fapi_websocket, "wss://fstream.asterdex-testnet.com");
        assert_eq!(u.sapi_websocket, "wss://sstream.asterdex-testnet.com");
        let clock = VenueClock::default();
        let properties = SymbolProperties::default();
        let creds = test_credentials();
        assert_eq!(
            mk_spot_rest(u, &creds, "BTCUSDT", properties, "", None, &clock).base_url,
            u.sapi_rest
        );
        assert_eq!(
            mk_perp_rest(u, &creds, "BTCUSDT", properties, None, 2.0, &clock).base_url,
            u.fapi_rest
        );
    }

    /// The EIP-712 spec's test key, no account's.
    fn test_credentials() -> Credentials {
        Credentials {
            api_key: "0xMASTER".to_string().into(),
            api_secret: "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                .to_string()
                .into(),
            passphrase: None,
        }
    }

    /// The leverage this arm POSTs to `/fapi/v3/leverage` is the OPERATOR's `[risk] max_leverage`,
    /// and an unset profile still yields the historical literal (`2.0`). ⚠ On THIS venue that
    /// unset-is-unchanged property guards a MAINNET account (only a `live`-tier aster account
    /// holds credentials), so it is the one assertion here that protects real money. NO network:
    /// `leverage_for` is the pure decision `VenueAdapterAster::start` calls once, before anything is
    /// spawned.
    #[test]
    fn perp_leverage_follows_the_operator_risk_budget() {
        vike_bridge_core::policy_pins::assert_leverage_follows_the_risk_budget(
            leverage_for,
            DEFAULT_LEVERAGE,
            2.0,
            "the historical hardcoded /fapi/v3/leverage value",
        );
    }

    /// **The family plane's lines keep aster's target, `vike_aster::execution`, and their words and fields.**
    /// A `tracing` target is a literal, so the family's drivers write through `AsterExecution::log`
    /// (`vike_binance::family_execution_log!` with this crate's target); before the move these two lines
    /// were this file's own `run` and `run_spot`/`run_perp`, under the same target.
    #[test]
    fn the_family_planes_lines_are_written_under_vike_aster_execution() {
        let ((), logs) = vike_log::capture::captured(|| {
            AsterExecution::log(FamilyExecutionLog::ResyncReplayed { symbols: 2, events: 5 });
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
                    "vike_aster::execution".to_string(),
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

    /// **Both lanes' clients take their nonce from the clock they are built with, and read it from their
    /// own host's path.** Nothing here opens a socket: the clock is told its answer, and a signer only
    /// prepares. The key is the EIP-712 spec's test key, no account's.
    #[test]
    fn the_clients_of_both_lanes_sign_on_the_clock_they_are_built_with() {
        use vike_bridge_core::signer::Signer;

        let creds = test_credentials();
        let urls = crate::transport::urls::seeded::testnet();
        let properties = SymbolProperties::default();
        let clock = VenueClock::default();
        let spot = mk_spot_rest(urls, &creds, "BTCUSDT", properties, "BTC", None, &clock);
        let perp = mk_perp_rest(urls, &creds, "BTCUSDT", properties, None, 2.0, &clock);
        assert_eq!(spot.transport.time_path(), "/api/v3/time");
        assert_eq!(perp.transport.time_path(), "/fapi/v1/time");

        const AN_HOUR_MS: i64 = 3_600_000;
        clock.read_if_due(vike_model::now_ms(), |_| Ok::<_, String>(AN_HOUR_MS)).unwrap();
        for (lane, query) in [
            ("spot", spot.signer.prepare(&[], "GET", "/x").query),
            ("perp", perp.signer.prepare(&[], "GET", "/x").query),
        ] {
            let nonce: i64 =
                query.split('&').find_map(|p| p.strip_prefix("nonce=")).unwrap().parse().unwrap();
            let ahead_ms = nonce / 1000 - vike_model::now_ms();
            assert!((ahead_ms - AN_HOUR_MS).abs() < 60_000, "{lane}: {ahead_ms} ms ahead");
        }
    }
}
