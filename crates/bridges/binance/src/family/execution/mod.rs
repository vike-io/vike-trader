//! **The exec plane of a family venue, ONCE** (R7b; the owner's ruling of 2026-11-13, "both parts,
//! behaviour as now"): the wiring between the `ExecutionClientActor`'s command thread and the venue's signed REST
//! clients, which binance's `crate::execution` and aster's `vike_aster::execution` carried as near-copies.
//!
//! - [`confirm_fn`]: the active by-order-client-id confirm, ONE client per exec client behind a `VenueRestPerSymbol`;
//! - [`foreign_perp_factory`] / [`foreign_spot_factory`]: a signing client for another symbol of the
//!   same account, on that symbol's own fetched grid and the mounted client's order-rate window;
//! - [`await_grid`] and [`fetch_grid`]: the startup grid, recorded once it loads; until it does, every order
//!   is refused in words and the grid fetched again (`vike_bridge_core::venue_adapter::await_properties`);
//! - [`run_spot`] / [`run_perp`]: the three REST clients (submit / resync supervisor / gap sentinel), the
//!   start's account writes, the user-data pump with its reconnect resync, then the command loop;
//! - [`replay_spot_history`] / [`replay_perp_history`]: the audit-A3 replay of the mounted symbol and
//!   then of each foreign one, the body of the reconnect resync and of the gap sentinel.
//!
//! **Every venue fact is a [`FamilyExecutionSpec`] item, never a branch here.** A spec is a VALUE each
//! venue builds at its start (its host table, its attribution, and on binance a COIN-M mount's settle
//! coin and the TRADE_LITE switch), and its items are: the venue word; its two REST client builds
//! (aster's builder pair rides an order-placing client only, binance's broker link id every client); the
//! grid fetch and the words of a symbol it cannot read; the user-data spawners (aster's listenKey pumps,
//! binance's spot WS-API session and futures listenKey pump); the start's account writes (aster READS the
//! position mode and posts the leverage; binance heals its demo modes and posts the leverage); what a
//! foreign perpetual needs before and after its client is built (binance's settle-coin fence, margin type
//! and class leverage; nothing on aster); binance's no-fetch client for a foreign cancel or status
//! (`light_perp_factory`); and the venue's log target ([`FamilyExecutionSpec::log`]). The bridge floors
//! live on the clients ([`crate::family::execution::signed_rest::FamilySignedRest::floor`]: off on aster spot, the trigger
//! floor alone on aster perp), the Algo Service in binance's `crate::execution::rest::algo` (decision 0186).
//!
//! **Log targets stay the venue's.** A `tracing` target is a literal baked into a `static`, so generic
//! code cannot write under `vike_aster::execution`: every line this module writes goes through
//! [`FamilyExecutionSpec::log`], which each venue implements as [`crate::family_execution_log!`] with its
//! own target, and the words of each line live once, in that macro.
//!
//! What stays each venue's: its `spawn_with_recorder` (the `ExecutionClientActor` itself, `with_confirm` and the
//! venue's own extras: binance's funding poll), its `run` (binance's COIN-M mount and its refusal), its
//! client builds and its one-venue verbs.

pub mod confirm;
pub mod dispatch;
pub mod event_mapper;
pub mod order_map;
pub mod order_word;
pub mod rest_reads;
pub mod resync;
pub mod signed_rest;
pub mod spot_fee;
pub mod user_data;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use serde_json::Value;
use vike_bridge_core::confirm::{
    ConfirmByOrderClientId, DispatchLedger, VenueRestDispatch, confirm_closure,
};
use vike_bridge_core::exec_actor::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
use vike_bridge_core::exec_actor::{
    ConfirmFn, DEFAULT_FILL_SENTINEL, ExecutionClientActorCommand, run_loop,
};
use vike_bridge_core::execution_client::resync::{ReplayRows, replay_foreign};
use vike_bridge_core::per_symbol::{
    ClientFactory, NEGATIVE_TTL, OrderClientIdSymbols, VenueRestPerSymbol,
};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::venue_adapter::await_properties;
use vike_bridge_core::websocket_session::user::{
    RESYNC_HISTORY_LIMIT, WebsocketSessionUserResyncHandle,
};
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_model::{SymbolProperties, now_ns};
use vike_secrets::credentials::Credentials;

use crate::execution::rest::per_symbol::refused_no_client;
use crate::family::execution::dispatch::{lane_symbol, split_symbol};
use crate::family::execution::signed_rest::{FamilySignedRest, get_all_orders_for, get_trades_for};

/// A line the family's exec plane writes, under the VENUE's target ([`FamilyExecutionSpec::log`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyExecutionLog {
    /// INFO: a reconnect replayed the history of the admitted symbols after the mounted one's.
    ResyncReplayed { symbols: usize, events: usize },
}

/// **The body of a venue's [`FamilyExecutionSpec::log`]**: `line` written under the literal `target`,
/// in the words the family's exec plane has always used. A macro because a `tracing` target must be a
/// constant at its call site: binance writes `crate::family_execution_log!("vike_binance::execution", line)`,
/// aster `vike_binance::family_execution_log!("vike_aster::execution", line)`.
#[macro_export]
macro_rules! family_execution_log {
    ($target:literal, $line:expr) => {
        match $line {
            $crate::family::execution::FamilyExecutionLog::ResyncReplayed { symbols, events } => {
                ::tracing::info!(
                    target: $target,
                    symbols,
                    events,
                    "reconnect resync: replayed the history of the admitted symbols after the mounted one"
                )
            }
        }
    };
}

/// **One family venue's exec-plane facts** (see the module doc): a value the venue builds once per start
/// and the drivers here carry, clone into a factory, and ask.
pub trait FamilyExecutionSpec: Clone + Send + Sync + 'static {
    /// The venue word: `lane_symbol`'s refusals, the PIT grid record, the transport's name.
    const VENUE: &'static str;
    /// The leverage a client that never posts one carries (the confirm's): the venue's fallback.
    const DEFAULT_LEVERAGE: f64;
    /// The venue's spot REST client.
    type Spot: FamilySignedRest + VenueRest + ConfirmByOrderClientId + Send + 'static;
    /// The venue's perp REST client.
    type Perp: FamilySignedRest + VenueRest + ConfirmByOrderClientId + Send + 'static;

    /// A fresh spot order-rate window: the one the mounted spot client and every foreign one share
    /// (the venue's order budget is per account, not per symbol).
    fn spot_gate(&self) -> RateGate;
    /// [`Self::spot_gate`]'s perp twin.
    fn perp_gate(&self) -> RateGate;

    /// One spot client for `symbol` (its own transport and signer, on the account's spot host), on
    /// `gate` or, `None`, on an order-rate window of its own. `places_orders`: it submits (the mounted
    /// and the foreign clients) or only reads (the confirm, the resync, the sentinel), for a venue whose
    /// attribution rides an order alone.
    fn spot_rest(
        &self,
        creds: &Credentials,
        symbol: &str,
        properties: SymbolProperties,
        base_asset: &str,
        places_orders: bool,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> Self::Spot;

    /// [`Self::spot_rest`]'s perp twin, for the EXCHANGE `api_symbol` (no `.P`); `leverage` is what the
    /// client's `set_leverage` would post.
    fn perp_rest(
        &self,
        creds: &Credentials,
        api_symbol: &str,
        properties: SymbolProperties,
        places_orders: bool,
        leverage: f64,
        gate: Option<RateGate>,
        clock: &VenueClock,
    ) -> Self::Perp;

    /// `symbol`'s REAL grid and base asset on the account's network, routed by its `.P`; `None` on any
    /// failure or an unknown symbol.
    fn fetch_properties(
        &self,
        creds: &Credentials,
        symbol: &str,
    ) -> Option<(SymbolProperties, String)>;

    /// Why an order on `label` is refused when its grid cannot be read: never a guessed grid.
    fn no_grid_reason(&self, label: &str) -> String;

    /// A foreign perpetual's refusal BEFORE its grid is fetched (binance's one-settle-coin fence).
    fn foreign_perp_fence(&self, _label: &str) -> Result<(), String> {
        Ok(())
    }

    /// What a foreign perpetual's freshly built client does before its first order (binance's
    /// margin-type and class-leverage preparation on the demo host).
    fn foreign_perp_built(&self, _rest: &Self::Perp) {}

    /// A factory for the client a foreign perpetual's CANCEL or STATUS takes with no grid fetch
    /// (`VenueRestPerSymbol::with_light_factory`), or `None` to use the full factory for those too.
    fn light_perp_factory(
        &self,
        _credentials: &Credentials,
        _properties: SymbolProperties,
        _gate: RateGate,
        _clock: &VenueClock,
    ) -> Option<ClientFactory<Self::Perp>> {
        None
    }

    /// The spot start, on the mounted client: the account's clock read, so the user-data stream and the
    /// first order sign on it.
    fn spot_start(&self, rest: &Self::Spot);

    /// The perp start, on the mounted client: the clock read, then the start's account writes.
    fn perp_start(&self, rest: &Self::Perp, series_symbol: &str, leverage: f64);

    /// The spot user-data pump with its reconnect `resync`, its fills into ingest through `fence`.
    fn spawn_spot_user_data(
        &self,
        creds: &Credentials,
        symbol: &str,
        events: EventSender,
        fence: Arc<SubmitFence>,
        resync: impl FnMut() -> Vec<Event> + Send + 'static,
        on_reconcile: Option<mpsc::Sender<()>>,
        clock: &VenueClock,
    ) -> WebsocketSessionUserResyncHandle;

    /// [`Self::spawn_spot_user_data`]'s perp twin, its fills labelled from `series_symbol`.
    fn spawn_perp_user_data(
        &self,
        creds: &Credentials,
        series_symbol: &str,
        events: EventSender,
        fence: Arc<SubmitFence>,
        resync: impl FnMut() -> Vec<Event> + Send + 'static,
        on_reconcile: Option<mpsc::Sender<()>>,
        clock: &VenueClock,
    ) -> WebsocketSessionUserResyncHandle;

    /// Write `line` under the venue's own target: [`crate::family_execution_log!`] with it.
    fn log(line: FamilyExecutionLog);
}

/// **The venue's active order-status re-query for `ExecutionClientActor::with_confirm`**: the core's
/// `Command::ConfirmOrder` lands here on a short-lived worker thread (blocking REST, never on the core
/// fold) and runs the SAME by-order-client-id lookup the ambiguous-submit arm uses (`vike_bridge_core::confirm`,
/// with `crate::family::execution::confirm`'s status words): found -> `OrderAccepted` (+ the terminal when the venue already
/// closed it), absent -> the "venue confirms absent" reject ONLY once the exec thread has finished this
/// order client id's submit (`ledger`), a FAILED query -> nothing. ONE REST client serves every confirm (one TLS
/// pool), bounded and backed off by `confirm_closure`; it reads only (`places_orders` false: binance's
/// carries its link id, because the venue stored the PREFIXED order client id, aster's no builder pair), on an
/// EMPTY grid (a status GET never reads one), signing on the exec client's `clock`.
///
/// The one client is a `VenueRestPerSymbol`: an order client id the exec thread recorded as a FOREIGN order (`order_client_ids`)
/// is asked about on its own symbol, any other on the mounted one. The foreign-symbol clients here read
/// no grid, so building one costs no network.
pub fn confirm_fn<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    symbol: &str,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    clock: &VenueClock,
) -> ConfirmFn {
    if split_symbol(symbol).1 {
        confirm_closure(
            grid_less_perp(spec, creds, symbol, false, order_client_ids, clock),
            ledger,
            |_| true,
        )
    } else {
        confirm_closure(
            grid_less_spot(spec, creds, symbol, false, order_client_ids, clock),
            ledger,
            |_| true,
        )
    }
}

/// **A PERP engine's command-loop client with NO grid**: the mounted symbol's, and every other symbol's built
/// on first use with no network, routed by `order_client_ids` as the trading loop's router routes. For the calls that
/// format no price and no size: the active confirm (`places_orders` false) and a cancel sent while the
/// mounted grid has not loaded ([`await_grid`], `places_orders` true: the command loop's own build). Never
/// handed an order to place.
fn grid_less_perp<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    symbol: &str,
    places_orders: bool,
    order_client_ids: Arc<OrderClientIdSymbols>,
    clock: &VenueClock,
) -> VenueRestPerSymbol<V::Perp> {
    let no_grid = SymbolProperties::default();
    let (api_symbol, _) = split_symbol(symbol);
    let (factory_spec, factory_credentials, factory_clock) =
        (spec.clone(), creds.clone(), clock.clone());
    let rest = spec.perp_rest(
        creds,
        &api_symbol,
        no_grid,
        places_orders,
        V::DEFAULT_LEVERAGE,
        None,
        clock,
    );
    let make: ClientFactory<V::Perp> = Box::new(move |label| {
        let api_symbol = lane_symbol(V::VENUE, label, true)?;
        Ok(factory_spec.perp_rest(
            &factory_credentials,
            &api_symbol,
            no_grid,
            places_orders,
            V::DEFAULT_LEVERAGE,
            None,
            &factory_clock,
        ))
    });
    VenueRestPerSymbol::new(symbol, rest, make, order_client_ids, refused_no_client)
}

/// [`grid_less_perp`]'s spot twin.
fn grid_less_spot<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    symbol: &str,
    places_orders: bool,
    order_client_ids: Arc<OrderClientIdSymbols>,
    clock: &VenueClock,
) -> VenueRestPerSymbol<V::Spot> {
    let no_grid = SymbolProperties::default();
    let (factory_spec, factory_credentials, factory_clock) =
        (spec.clone(), creds.clone(), clock.clone());
    let rest = spec.spot_rest(creds, symbol, no_grid, "", places_orders, None, clock);
    let make: ClientFactory<V::Spot> = Box::new(move |label| {
        let api_symbol = lane_symbol(V::VENUE, label, false)?;
        Ok(factory_spec.spot_rest(
            &factory_credentials,
            &api_symbol,
            no_grid,
            "",
            places_orders,
            None,
            &factory_clock,
        ))
    });
    VenueRestPerSymbol::new(symbol, rest, make, order_client_ids, refused_no_client)
}

/// **The `VenueRestPerSymbol` factory for a PERP engine**: a signing client for another perpetual of the
/// SAME account (same keys, host, attribution, leverage value and order-rate window as the mounted
/// client) on that symbol's own fetched grid. The grid fetch is one blocking public `exchangeInfo` GET on
/// the exec thread, bounded by the transport's timeout; the venue's fence runs before it
/// ([`FamilyExecutionSpec::foreign_perp_fence`]) and its preparation after the build
/// ([`FamilyExecutionSpec::foreign_perp_built`]).
///
/// ⚠ NO `set_leverage` here, deliberately: this factory also runs after every restart, when the account
/// may already hold a position on this symbol at a leverage set on the venue; posting the MOUNTED
/// symbol's configured value would move that position's liquidation price. `leverage` is carried, never
/// posted for the new symbol.
pub fn foreign_perp_factory<V: FamilyExecutionSpec>(
    spec: V,
    creds: Credentials,
    leverage: f64,
    gate: RateGate,
    clock: VenueClock,
) -> ClientFactory<V::Perp> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(V::VENUE, &label, true)?;
        spec.foreign_perp_fence(&label)?;
        let (properties, _) =
            spec.fetch_properties(&creds, &label).ok_or_else(|| spec.no_grid_reason(&label))?;
        let rest = spec.perp_rest(
            &creds,
            &api_symbol,
            properties,
            true,
            leverage,
            Some(gate.clone()),
            &clock,
        );
        spec.foreign_perp_built(&rest);
        Ok(rest)
    })
}

/// [`foreign_perp_factory`]'s spot twin. The new client signs on the engine's one `clock`, which its first
/// signed request reads if nothing has answered yet.
pub fn foreign_spot_factory<V: FamilyExecutionSpec>(
    spec: V,
    creds: Credentials,
    gate: RateGate,
    clock: VenueClock,
) -> ClientFactory<V::Spot> {
    Box::new(move |label| {
        let label = label.to_uppercase();
        let api_symbol = lane_symbol(V::VENUE, &label, false)?;
        let (properties, base_asset) =
            spec.fetch_properties(&creds, &label).ok_or_else(|| spec.no_grid_reason(&label))?;
        Ok(spec.spot_rest(
            &creds,
            &api_symbol,
            properties,
            &base_asset,
            true,
            Some(gate.clone()),
            &clock,
        ))
    })
}

/// Where a family venue reads one symbol's grid ([`fetch_grid`]): the account's spot and futures REST
/// origins, each lane's `exchangeInfo` path and order-rate window (the one-shot fetch is gated for
/// symmetry with the order path it formats for), and the venue word its transport is named by.
#[derive(Debug, Clone, Copy)]
pub struct FamilyGridSource<'a> {
    pub venue: &'static str,
    pub spot_rest: &'a str,
    pub perp_rest: &'a str,
    pub spot_path: &'static str,
    pub perp_path: &'static str,
    pub spot_gate: fn() -> RateGate,
    pub perp_gate: fn() -> RateGate,
}

/// **`symbol`'s REAL grid and base asset**, routed by its `.P`: perp reads the futures `exchangeInfo`
/// (`perp_row` reads the venue's own instrument row of the upper-cased EXCHANGE symbol out of it), spot
/// reads the spot one (`crate::family::execution::order_map::parse_symbol_properties`, the base asset off its first
/// entry). Both are PUBLIC GETs on the hosts of `source`. `Err` names what failed: the read (the
/// transport's code and words) or a list with no row for the symbol. Never a guessed grid.
pub fn fetch_grid(
    source: &FamilyGridSource<'_>,
    symbol: &str,
    perp_row: impl FnOnce(&Value, &str) -> Option<(SymbolProperties, String)>,
) -> Result<(SymbolProperties, String), String> {
    let (api_symbol, is_perp) = split_symbol(symbol);
    let (rest, path, gate) = if is_perp {
        (source.perp_rest, source.perp_path, source.perp_gate)
    } else {
        (source.spot_rest, source.spot_path, source.spot_gate)
    };
    let info = UreqTransport::new(source.venue)
        .with_rate_gate(gate())
        .public(rest, path, &[("symbol", api_symbol.clone())])
        .map_err(|e| format!("{path} could not be read: code {} {}", e.code, e.msg))?;
    let exchange = api_symbol.to_uppercase();
    let unlisted = || format!("{path} lists no usable row for {exchange}");
    if is_perp {
        perp_row(&info, &exchange).ok_or_else(unlisted)
    } else {
        let properties = *crate::family::execution::order_map::parse_symbol_properties(&info)
            .get(&exchange)
            .ok_or_else(unlisted)?;
        let base_asset = info
            .get("symbols")
            .and_then(|s| s.as_array())
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("baseAsset"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        Ok((properties, base_asset))
    }
}

/// **The grid the exec thread signs on**, and its base asset: `fetch`'s answer, recorded into the PIT
/// properties store under the CORE (`.P`-preserving) label as soon as it loads. Until it does, a new order or
/// an amend is refused in words, a cancel is SENT through the command loop's client built with no grid
/// ([`grid_less_perp`] / [`grid_less_spot`], routed by `order_client_ids`), and `fetch` is asked again after each
/// [`NEGATIVE_TTL`] (`vike_bridge_core::venue_adapter::await_properties`): there is no fallback grid (owner,
/// 2026-11-13). `None`: the client was shut down first, and the exec thread returns.
pub fn await_grid<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    symbol: &str,
    order_client_ids: &Arc<OrderClientIdSymbols>,
    clock: &VenueClock,
    events: &EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    properties_rec: &Option<Arc<vike_data::PropertiesRecorder>>,
    fetch: impl FnMut() -> Result<(SymbolProperties, String), String>,
) -> Option<(SymbolProperties, String)> {
    let order_client_ids = Arc::clone(order_client_ids);
    let (real, base) = if split_symbol(symbol).1 {
        let light = || grid_less_perp(spec, creds, symbol, true, order_client_ids, clock);
        await_properties(V::VENUE, symbol, events, rx, NEGATIVE_TTL, light, fetch)?
    } else {
        let light = || grid_less_spot(spec, creds, symbol, true, order_client_ids, clock);
        await_properties(V::VENUE, symbol, events, rx, NEGATIVE_TTL, light, fetch)?
    };
    crate::family::instrument::record_properties_all(
        properties_rec,
        V::VENUE,
        [(symbol.to_uppercase(), real)],
        now_ns(),
    );
    Some((real, base))
}

/// **The SPOT driver**: three REST clients (submit / resync supervisor / gap sentinel) on the one `clock`,
/// read at the start ([`FamilyExecutionSpec::spot_start`]), the user-data pump with its audit-A3 resync,
/// then the command loop. The submit client and every foreign-symbol client built later share ONE
/// order-rate window; the resync and the sentinel each read on a window of its own.
///
/// From the command loop on, each order is signed on the ORDER's symbol: the mounted symbol (or none)
/// goes to the mounted client, any other spot symbol to a client built on first use
/// ([`foreign_spot_factory`]). After a reconnect the pump replays the mounted symbol, then EVERY symbol
/// the engine admitted (`admitted`, the list the reconcile client is told, read at the reconnect); five
/// seconds after the last command the gap sentinel replays the mounted symbol, then the foreign symbols
/// the commands since the last look named (not every admitted one: a sentinel per command over sixteen
/// symbols would be a burst of requests for nothing). The fence both threads share:
/// `vike_bridge_core::exec_actor::submit_fence`'s module doc.
#[expect(clippy::too_many_arguments)]
pub fn run_spot<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    symbol: &str,
    properties: SymbolProperties,
    base_asset: &str,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: &VenueClock,
) {
    let gate = spec.spot_gate();
    let rest =
        spec.spot_rest(creds, symbol, properties, base_asset, true, Some(gate.clone()), clock);
    let resync_rest = spec.spot_rest(creds, symbol, properties, base_asset, false, None, clock);
    let sentinel_rest = spec.spot_rest(creds, symbol, properties, base_asset, false, None, clock);
    spec.spot_start(&rest);
    let sentinel_order_client_ids = Arc::clone(&order_client_ids);
    let routed = VenueRestPerSymbol::new(
        symbol,
        rest,
        foreign_spot_factory(spec.clone(), creds.clone(), gate, clock.clone()),
        order_client_ids,
        refused_no_client,
    );
    let resync_symbol = symbol.to_string();
    let sentinel_symbol = symbol.to_string();
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let feed = spec.spawn_spot_user_data(
        creds,
        symbol,
        events.clone(),
        Arc::clone(&fence),
        move || {
            let foreign = admitted.symbols();
            let events = replay_spot_history(&resync_rest, &resync_symbol, &foreign);
            if !foreign.is_empty() {
                V::log(FamilyExecutionLog::ResyncReplayed {
                    symbols: foreign.len(),
                    events: events.len(),
                });
            }
            events
        },
        on_reconcile,
        clock,
    );
    run_loop(
        &VenueRestOrderSubmitFence::new(&VenueRestDispatch { rest: routed, ledger }, &fence),
        &events,
        rx,
        || {
            replay_spot_history(
                &sentinel_rest,
                &sentinel_symbol,
                &sentinel_order_client_ids.take_touched(),
            )
        },
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

/// **The PERP driver**: [`run_spot`]'s shape on the EXCHANGE `api_symbol`, with the start's account writes
/// ([`FamilyExecutionSpec::perp_start`]) after the clock read, the venue's no-fetch client for a foreign
/// cancel or status ([`FamilyExecutionSpec::light_perp_factory`]) beside [`foreign_perp_factory`], and
/// every execution the pump and the replay emit labelled with the CORE `.P` series label, so it folds into
/// the perp position and never collides with its spot twin (`super::perp_series_symbol` is the one
/// spelling). All three clients carry the mount's `leverage`; only the start posts it.
#[expect(clippy::too_many_arguments)]
pub fn run_perp<V: FamilyExecutionSpec>(
    spec: &V,
    creds: &Credentials,
    series_symbol: &str,
    api_symbol: &str,
    properties: SymbolProperties,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    on_reconcile: Option<mpsc::Sender<()>>,
    leverage: f64,
    ledger: Arc<DispatchLedger>,
    order_client_ids: Arc<OrderClientIdSymbols>,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: &VenueClock,
) {
    let gate = spec.perp_gate();
    let rest =
        spec.perp_rest(creds, api_symbol, properties, true, leverage, Some(gate.clone()), clock);
    let resync_rest = spec.perp_rest(creds, api_symbol, properties, false, leverage, None, clock);
    let sentinel_rest = spec.perp_rest(creds, api_symbol, properties, false, leverage, None, clock);
    spec.perp_start(&rest, series_symbol, leverage);
    let light = spec.light_perp_factory(creds, properties, gate.clone(), clock);
    let sentinel_order_client_ids = Arc::clone(&order_client_ids);
    let routed = VenueRestPerSymbol::new(
        series_symbol,
        rest,
        foreign_perp_factory(spec.clone(), creds.clone(), leverage, gate, clock.clone()),
        order_client_ids,
        refused_no_client,
    );
    let routed = match light {
        Some(light) => routed.with_light_factory(light),
        None => routed,
    };
    let resync_symbol = series_symbol.to_string();
    let sentinel_symbol = series_symbol.to_string();
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let feed = spec.spawn_perp_user_data(
        creds,
        series_symbol,
        events.clone(),
        Arc::clone(&fence),
        move || {
            let foreign = admitted.symbols();
            let events = replay_perp_history(&resync_rest, &resync_symbol, &foreign);
            if !foreign.is_empty() {
                V::log(FamilyExecutionLog::ResyncReplayed {
                    symbols: foreign.len(),
                    events: events.len(),
                });
            }
            events
        },
        on_reconcile,
        clock,
    );
    run_loop(
        &VenueRestOrderSubmitFence::new(&VenueRestDispatch { rest: routed, ledger }, &fence),
        &events,
        rx,
        || {
            replay_perp_history(
                &sentinel_rest,
                &sentinel_symbol,
                &sentinel_order_client_ids.take_touched(),
            )
        },
        DEFAULT_FILL_SENTINEL,
    );
    let _ = feed.shutdown();
}

/// **The audit-A3 SPOT replay** (the reconnect resync's and the gap sentinel's body): `allOrders` then the
/// fills of the mounted `symbol`, with the requests it has always made, then the same two for each of
/// `foreign` on the spot lane (`vike_bridge_core::execution_client::resync::replay_foreign`: the walk ends at
/// the first answer that says "slow down"), each a best-effort read whose failure costs its own rows.
/// The ids the venue echoes are read with the client's link id, and a foreign fill carries the
/// symbol-qualified trade id the stream spells (`super::fill_trade_id`).
pub fn replay_spot_history<C: FamilySignedRest>(
    rest: &C,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    let link_id = rest.link_id();
    replay_foreign(
        symbol,
        foreign,
        |label| crate::family::execution::resync::on_lane(label, false),
        crate::family::execution::resync::says_slow_down,
        |rows| {
            let orders = rows(get_all_orders_for(rest, rest.symbol(), RESYNC_HISTORY_LIMIT));
            let trades = rows(get_trades_for(rest, rest.symbol(), RESYNC_HISTORY_LIMIT));
            crate::family::execution::resync::map_history(
                &orders,
                &trades,
                C::VENUE,
                symbol,
                link_id,
            )
        },
        |label, rows| {
            let exchange = vike_venues::split_perp(label).0;
            let orders = rows(get_all_orders_for(rest, exchange, RESYNC_HISTORY_LIMIT));
            let trades = rows(get_trades_for(rest, exchange, RESYNC_HISTORY_LIMIT));
            crate::family::execution::resync::map_history_of(
                &orders,
                &trades,
                C::VENUE,
                label,
                symbol,
                link_id,
            )
        },
    )
}

/// [`replay_spot_history`]'s perp twin: the mounted series `symbol` (`.P` included) first, on its
/// EXCHANGE symbol, then each of `foreign` on the perp lane, each read by [`perp_history_read`]; fills and
/// liquidations of a foreign symbol carry the symbol-qualified trade id the stream spells.
pub fn replay_perp_history<C: FamilySignedRest>(
    rest: &C,
    symbol: &str,
    foreign: &[String],
) -> Vec<Event> {
    let link_id = rest.link_id();
    replay_foreign(
        symbol,
        foreign,
        |label| crate::family::execution::resync::on_lane(label, true),
        crate::family::execution::resync::says_slow_down,
        |rows| {
            perp_history_read(rest, rest.symbol(), rows, |orders, trades| {
                crate::family::execution::resync::map_perp_history(
                    orders,
                    trades,
                    C::VENUE,
                    symbol,
                    link_id,
                )
            })
        },
        |label, rows| {
            let exchange = vike_venues::split_perp(label).0;
            perp_history_read(rest, exchange, rows, |orders, trades| {
                crate::family::execution::resync::map_perp_history_of(
                    orders,
                    trades,
                    C::VENUE,
                    label,
                    symbol,
                    link_id,
                )
            })
        },
    )
}

/// One perp symbol's history: `allOrders`, the fills, then the venue's conditional orders
/// ([`FamilySignedRest::conditional_history_rows`]: binance's `allAlgoOrders`), in that order and each
/// through `rows`; then `map` of the first two, then the conditional orders' events.
pub fn perp_history_read<C: FamilySignedRest>(
    rest: &C,
    exchange: &str,
    rows: ReplayRows<'_>,
    map: impl FnOnce(&Value, &Value) -> Vec<Event>,
) -> Vec<Event> {
    let orders = rows(get_all_orders_for(rest, exchange, RESYNC_HISTORY_LIMIT));
    let trades = rows(get_trades_for(rest, exchange, RESYNC_HISTORY_LIMIT));
    let conditional = rest.conditional_history_rows(exchange, rows);
    let mut events = map(&orders, &trades);
    if let Some(conditional) = conditional {
        events.extend(rest.conditional_history_events(&conditional));
    }
    events
}
