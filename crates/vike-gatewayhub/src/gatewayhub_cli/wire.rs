//! **Phase: the wire** — the hook [`super::run`] hands [`super::run_with`]. It turns everything the
//! earlier phases produced ([`Ready`]) into a SERVING daemon and owns the rest of the process: the
//! hub keys, the venue lanes, the listener, the accept loop and the stop.
//!
//! # The order inside the hook
//!
//! 1. **The hub keys** (`super::keys::gateway_keys`). No keys, no start: the refusal exits 2 before
//!    anything is built or bound, because a daemon that bound a socket and answered nothing looks
//!    healthy to a unit that waits for it, and one that answered without authenticating would let
//!    any local process spend the credentials it holds.
//! 2. **The OANDA startup line** (`announce_oanda_lane`): the ONE announcing read of the practice
//!    token, dropped at once, so the log says whether the credentialed history lane is armed. The
//!    lane itself reads the token per CONNECTION, never at startup. **IG's** (`announce_ig_lane`) is
//!    the same line from the presence word alone: no value of its three fields is read at startup.
//!    **cTrader's** (`announce_ctrader_lane`) is the same again, from the demo account's token pair.
//! 3. **The lanes** (`production_lanes`): `BridgeData` over the venue settings rows the boot read,
//!    `VenueRest` over `flags.hyperliquid_hip3`, the credential store handle and the shadow
//!    budget's snapshot. Building a lane dials nothing (a venue client is built on its first
//!    `DataOpen`), but the Polymarket bridge reads its egress from a process-wide cell the moment a
//!    client or a Gamma agent exists, which is why `super::declare_polymarket_egress` runs in
//!    [`super::run_with`] ABOVE this hook.
//! 4. **The bind**: exactly [`Ready::bind`], the addresses the bind guard already resolved and
//!    checked. The address STRING is never re-resolved here: a second resolution could answer
//!    differently and walk around the guard (`serve` checks the bound listener once more).
//! 5. **`serve`** until the stop flag goes up, the drain, and the exit status: 0 for a clean stop,
//!    1 for a bind or serve failure (something was tried and failed), 2 for a refusal before
//!    anything opened.
//!
//! # A build without the `data` feature has no venue lane
//!
//! It links no bridge, so there is nothing to serve and `serve_wire` says so and exits 2 — the
//! refused-configuration status — rather than serving an empty roster: every client would be told
//! the daemon serves nothing, and a unit would count it as up.

use std::process::ExitCode;

use super::Ready;

#[cfg(any(feature = "data", test))]
use std::net::{SocketAddr, TcpListener};
#[cfg(feature = "data")]
use std::sync::Arc;
#[cfg(feature = "data")]
use std::time::Instant;

#[cfg(any(feature = "data", test))]
use vike_gatewayhub_client::proto::BudgetCounter;
#[cfg(any(feature = "data", test))]
use vike_hub_proto::auth::HubKeys;

#[cfg(any(feature = "data", test))]
use crate::budget::ledger::Snapshot;
#[cfg(any(feature = "data", test))]
use crate::server::accept::serve;
#[cfg(any(feature = "data", test))]
use crate::server::bind::check_loopback;
#[cfg(any(feature = "data", test))]
use crate::server::lanes::Lanes;
#[cfg(any(feature = "data", test))]
use crate::server::limits::ServerConfig;

/// The wire hook of a build that links the venue bridges: refuse without keys, announce, build the
/// lanes, serve. See the module doc for the order.
#[cfg(feature = "data")]
pub(super) fn serve_wire(ready: Ready) -> ExitCode {
    let keys = match super::keys::gateway_keys(&ready.booted.store) {
        Ok(keys) => keys,
        Err(why) => {
            tracing::error!(
                "vike-gatewayhub: {why}; refusing to start. The daemon is always keyed: it holds \
                 every venue connection, so it has no unauthenticated mode"
            );
            return ExitCode::from(2);
        }
    };
    announce_oanda_lane(&ready.booted);
    announce_ig_lane(&ready.booted);
    announce_ctrader_lane(&ready.booted);
    let (lanes, pump_health) = production_lanes(&ready);
    // The trade surface (W9b): mounted only in a `trade` build that holds the `Trade` key. Absence of the
    // key is the switch, and the advertisement follows the mount (`server::features::trade_features`).
    #[cfg(feature = "trade")]
    if keys.has(vike_hub_proto::auth::Scope::Trade) {
        let setup = trade_setup(&ready, pump_health);
        return serve_ready_with_trade(ready, keys, lanes, setup);
    }
    #[cfg(not(feature = "trade"))]
    let _ = pump_health;
    serve_ready(ready, keys, lanes)
}

/// The wire hook of a build WITHOUT the `data` feature: no bridge is linked, so there is no venue
/// lane to serve. Says so and exits 2 — see the module doc.
#[cfg(not(feature = "data"))]
pub(super) fn serve_wire(ready: Ready) -> ExitCode {
    tracing::error!(
        bind = ?ready.bind,
        "vike-gatewayhub: built without the `data` feature: no venue lane, so nothing could be \
         served. The boot succeeded and the bind guard accepted the address; exiting with an error \
         so nothing mistakes this process for a running gateway. Rebuild with: cargo build -p \
         vike-gatewayhub --features data"
    );
    ExitCode::from(2)
}

/// Bind [`Ready::bind`] and serve `lanes` under `keys` until the stop flag, then return the process's
/// exit status. The seam a test drives with scripted lanes: `serve_wire` is this plus the keys read
/// and the production lanes.
#[cfg(any(feature = "data", test))]
pub(super) fn serve_ready(ready: Ready, keys: HubKeys, lanes: Lanes) -> ExitCode {
    let listener = match bind_logged(&ready) {
        Ok(listener) => listener,
        Err(code) => return code,
    };
    // `ServerConfig::PRODUCTION`: every policy number is the server's own start value. The idle read
    // timeout is the node constant, not a settings row; whether this daemon should honour the shared
    // idle-timeout row is an OPEN question (no row is read here until it is decided).
    exit_of(serve(listener, keys, lanes, ServerConfig::PRODUCTION, ready.stop.flag()))
}

/// [`serve_ready`] with the trade lane mounted (`server::accept::serve_with_trade`): the same bind, the
/// same stop, the same exit statuses.
#[cfg(all(feature = "data", feature = "trade"))]
fn serve_ready_with_trade(
    ready: Ready,
    keys: HubKeys,
    lanes: Lanes,
    setup: crate::server::trade::TradeSetup,
) -> ExitCode {
    let listener = match bind_logged(&ready) {
        Ok(listener) => listener,
        Err(code) => return code,
    };
    exit_of(crate::server::accept::serve_with_trade(
        listener,
        keys,
        lanes,
        ServerConfig::PRODUCTION,
        setup,
        ready.stop.flag(),
    ))
}

/// Bind [`Ready::bind`], logging a failure; the `Err` is the exit status.
#[cfg(any(feature = "data", test))]
fn bind_logged(ready: &Ready) -> Result<TcpListener, ExitCode> {
    bind_ready(&ready.bind).map_err(|e| {
        tracing::error!(
            bind = ?ready.bind, error = %e,
            "vike-gatewayhub: failed to bind the listener"
        );
        ExitCode::FAILURE
    })
}

/// The process status of a serve: 0 for a clean stop, 1 for an error.
#[cfg(any(feature = "data", test))]
fn exit_of(served: std::io::Result<()>) -> ExitCode {
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-gatewayhub: the server ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// The trade lane's setup, built from what the earlier phases produced (a `trade` build only). Dials
/// nothing: a venue client is built on a lease's `AdapterStart`.
///
/// * the lane is `RefreshedTrade` (a `BridgeTrade`) over [`crate::registry::trade::REGISTRY`] and the source
///   `super::vars::trade_source`: the deployment map (the store's deployment names the bridges look up, the
///   resolved flags folded over them), the account directory (the `account` rows and EVERY account's
///   credentials by field), the `venue_setting` rows and the flags, RE-READ at each step a tradehub restart
///   drives (its arming table, preflight, tier check and `AdapterStart`), so a credential rotation or a
///   setting edit is picked up by a tradehub restart as it was when the tradehub read them at its own boot;
/// * the project's state directory is the boot's (`LIVE-*.lock`, `gateway-id`), the HALT sentinel's path the
///   one resolver's (`vike_boot::halt::halt_path_from_env`, after the boot declared the project), and the
///   budget the shadow ledger every lane counts against.
#[cfg(all(feature = "data", feature = "trade"))]
fn trade_setup(
    ready: &Ready,
    pump_health: Arc<dyn crate::health::VenueHealth>,
) -> crate::server::trade::TradeSetup {
    let store = &ready.booted.store;
    let source = super::vars::trade_source(
        ready.booted.store.clone(),
        ready.booted.settings_dir.clone(),
        ready.booted.settings.flags,
    );
    let lane = crate::trade::adapters::refreshed::RefreshedTrade::new(
        crate::registry::trade::REGISTRY,
        source,
    )
    .with_account_rows(account_rows(store.clone()))
    .with_health(pump_health)
    .with_symbol_facts(Arc::new(symbol_facts(ready.booted.settings.flags.hyperliquid_hip3)))
    // A hyperliquid API-wallet approval that is ending or gone is told to the operator: a log line, and
    // one alert a day through the store's alert targets. A warning only (`agent_watch`'s module doc).
    .with_agent_watch(Arc::new(crate::trade::adapters::agent_watch::AgentWatch::new(
        crate::trade::adapters::agent_watch::store_notifier(store.clone()),
    )));
    crate::server::trade::TradeSetup {
        lane: Arc::new(lane),
        state_dir: ready.booted.state_dir.clone(),
        halt_path: Some(vike_boot::halt::halt_path_from_env()),
        budget: Arc::clone(&ready.budget),
    }
}

/// **The trade lane's `SymbolFacts` source**: the PUBLIC instrument catalog the rest lane serves
/// (`CatalogProviders::real`, keyless endpoints only), so a symbol's facts are asked of the venue with no
/// account secret in reach. The lane keeps each listing for ten minutes. A binance or aster spot symbol, every
/// bybit symbol and every polymarket outcome token are read ONE at a time (`with_symbol_lister`), not out of a list.
#[cfg(all(feature = "data", feature = "trade"))]
fn symbol_facts(hyperliquid_hip3: bool) -> crate::trade::adapters::symbol_facts::SymbolFacts {
    use crate::rest::FetchCtx;
    use vike_gatewayhub_client::proto::FetchFailure;
    let providers = crate::rest::catalog::CatalogProviders::real(hyperliquid_hip3);
    let facts = crate::trade::adapters::symbol_facts::SymbolFacts::new(Arc::new(move |venue| {
        // binance and aster: the whole-list read is the USDS-M futures list alone (a `.P` ask; binance's carries its
        // dated futures beside the perpetuals, aster's is its perpetuals; fapi ignores
        // `symbol=`). Spot is read one symbol at a time below, and the picker's spot+perp list (tolerant of
        // one endpoint failing, which is right for a picker) is never the facts' source: a half list kept as
        // a success would answer the missing lane "no such instrument".
        if venue == "binance" {
            return vike_binance::catalog::BinanceCatalog.perp_listing().map_err(|e| e.0);
        }
        // binance's DEMO futures host: the list a `demo` account can sign (it is not mainnet's).
        if venue == crate::trade::adapters::symbol_facts::BINANCE_DEMO_LIST {
            return vike_binance::catalog::BinanceCatalog.demo_perp_listing().map_err(|e| e.0);
        }
        // binance's COIN-M book: read only to refuse a COIN-M symbol by name (never answered as facts).
        if venue == crate::trade::adapters::symbol_facts::BINANCE_COIN_M_LIST {
            return vike_binance::catalog::BinanceCatalog.coin_m_listing().map_err(|e| e.0);
        }
        if venue == "aster" {
            return vike_aster::catalog::AsterCatalog.perp_listing().map_err(|e| e.0);
        }
        // bybit has no whole-list read: every symbol is `instruments-info?symbol=` (below).
        if venue == "bybit" {
            return Err("bybit: instruments are read one symbol at a time".to_string());
        }
        // okx's catalog is four listings plus one per option family; its facts need the SWAP rows only.
        if venue == "okx" {
            return vike_okx::catalog::OkxCatalog::list_swap_instruments().map_err(|e| {
                format!("{venue}: the venue's instrument list could not be fetched: {}", e.0)
            });
        }
        // deribit signs on the TESTNET book whatever the public catalog (mainnet) lists, so its facts
        // are the testnet's futures-kind rows (perpetuals and dated futures; an option is read one at a
        // time below): the grid the door admits is the grid the adapter signs on.
        if venue == "deribit" {
            return vike_deribit::exec::fetch_testnet_futures().map_err(|e| {
                format!("{venue}: the venue's instrument list could not be fetched: {e}")
            });
        }
        // hyperliquid's `demo` accounts trade the TESTNET, whose universe is not mainnet's (19 mainnet coins are
        // not listed there, and the same coin has another asset index): an account on that network is asked for
        // the testnet's own `meta` (`SymbolFacts::facts_on`), core markets only (the facts refuse HIP-3 anyway).
        if venue == crate::trade::adapters::symbol_facts::HYPERLIQUID_TESTNET_LIST {
            use vike_catalog::CatalogProvider as _;
            return vike_hyperliquid::feeds::catalog::HyperliquidCatalog::for_network(
                vike_hyperliquid::base::config::Network::Testnet,
            )
            .list_instruments()
            .map_err(|e| {
                format!("{venue}: the testnet's instrument list could not be fetched: {}", e.0)
            });
        }
        providers.list(venue, &FetchCtx::new()).map_err(|e| match e {
            FetchFailure::Fetch(text) | FetchFailure::Refused(text) => text,
        })
    }));
    // One symbol at a time where the venue filters (`SymbolFacts::answered_one_at_a_time` says which asks):
    // binance/aster spot `exchangeInfo?symbol=`, bybit `instruments-info?symbol=`.
    facts.with_symbol_lister(Arc::new(|venue, symbol| match venue {
        "binance" => vike_binance::catalog::BinanceCatalog.spot_symbol(symbol).map_err(|e| e.0),
        "aster" => vike_aster::catalog::AsterCatalog.spot_symbol(symbol).map_err(|e| e.0),
        // bybit: `PAIR.S` is the SPOT pair (decision 0231), every other name a derivative (a perpetual, a dated
        // future, a coin-margined contract) whose row the venue's `symbol=` read answers in either book.
        "bybit" => match symbol.strip_suffix(".S") {
            Some(pair) => vike_bybit::catalog::spot_symbol(pair),
            None => vike_bybit::catalog::perp_symbol(symbol),
        }
        .map_err(|e| e.0),
        // okx: a SPOT pair or a dated future by a point read; a SWAP stays in the one bulk list above
        "okx" => vike_okx::catalog::OkxCatalog::instrument_rows(symbol).map_err(|e| e.0),
        // deribit: an option (the venue lists thousands), by its own row on the testnet the adapter signs on.
        "deribit" => vike_deribit::exec::fetch_testnet_instrument(symbol),
        // polymarket lists tens of thousands of outcome tokens: one token's market, by Gamma's point lookup.
        "polymarket" => vike_polymarket::catalog::outcome_rows(symbol),
        // hyperliquid: a builder-dex coin (`dex:COIN`), by its own dex alone; the key says which network.
        "hyperliquid" | crate::trade::adapters::symbol_facts::HYPERLIQUID_TESTNET_LIST => {
            hyperliquid_dex_rows(venue, symbol)
        }
        _ => Err(format!("{venue}: no one-symbol read")),
    }))
}

/// **The catalog rows of the builder dex a `dex:COIN` names**, on the network the listing key says
/// (`HYPERLIQUID_TESTNET_LIST` is the testnet, the bare slug mainnet). Two keyless reads
/// (`HyperliquidCatalog::dex_instruments`); a dex the venue does not list answers no rows.
#[cfg(all(feature = "data", feature = "trade"))]
fn hyperliquid_dex_rows(venue: &str, symbol: &str) -> Result<Vec<vike_model::Instrument>, String> {
    use vike_hyperliquid::base::config::Network;
    let network = if venue == crate::trade::adapters::symbol_facts::HYPERLIQUID_TESTNET_LIST {
        Network::Testnet
    } else {
        Network::Mainnet
    };
    let Some((dex, _)) = symbol.split_once(':').filter(|(dex, _)| !dex.is_empty()) else {
        return Err(format!("{venue}: `{symbol}` is not a builder-dex coin (`dex:COIN`)"));
    };
    vike_hyperliquid::feeds::catalog::HyperliquidCatalog::dex_instruments(network, dex)
        .map_err(|e| e.0)
}

/// **A fresh read of the `account` table**, for the supervised IBKR link's predicate (decision 0164 verdict
/// 7(b)): it is asked before every reconnect attempt, so an `account.active` flip made after the daemon
/// started stops the reconnect within one attempt. ROWS ONLY: the key names and the credential values are
/// not read (the predicate decides a tier, and a value read here would only be dropped again).
#[cfg(all(feature = "data", feature = "trade"))]
fn account_rows(
    store: vike_secrets::CredentialStore,
) -> crate::trade::adapters::bridge_trade::AccountRows {
    Arc::new(move || {
        vike_secrets::account_directory::AccountDirectory::read(
            store.accounts(),
            Ok::<_, String>(None),
            Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
        )
    })
}

/// Bind exactly `bind`: the addresses `boot::check_bind` resolved and found loopback. The
/// guard is asked once more of the SAME set (the one decision, `server::bind::check_loopback`), and
/// the string the row held is not consulted again.
#[cfg(any(feature = "data", test))]
pub(super) fn bind_ready(bind: &[SocketAddr]) -> std::io::Result<TcpListener> {
    check_loopback(bind).map_err(std::io::Error::from)?;
    TcpListener::bind(bind)
}

/// The production lanes, built from what the earlier phases produced. Dials nothing.
///
/// * `BridgeData::new(venue_settings)`: the settings come from [`Ready::venue_settings`], the boot's
///   ONE read of the `venue_setting` table; the lane reads no store. It also hosts the tick pumps and
///   serves Hyperliquid's network; the credentialed streams (wave 2, W10) are added from the credential
///   store handle and read their account per open.
/// * `VenueRest::new(hyperliquid_hip3)`: the wire's `CatalogList` carries no such flag, so this daemon
///   reads `flags.hyperliquid_hip3` itself, once, as the datahub's catalog did, and it cannot travel
///   from the datahub. The credential store handle arms the credentialed OANDA lane (a location and
///   a scope, no value), and the shadow budget's snapshot is what the status verb reports.
#[cfg(feature = "data")]
fn production_lanes(ready: &Ready) -> (Lanes, Arc<dyn crate::health::VenueHealth>) {
    // The strategy lane's credentialed streams read their account from the credential store, per open,
    // and rotate ctrader's grant into the project state directory this boot declared.
    let credentialed = crate::data::credentialed::CredentialedLane::new(
        crate::data::accounts::StoreAccounts::source(ready.booted.store.clone()),
        vike_boot::halt::declared_project_state_dir(),
        &ready.venue_settings,
    );
    // The ONE pump hub of the daemon is built here (not inside `BridgeData::new`) so the trade lane can read
    // its health: the reconcile health gate's evidence (`crate::health`, R31).
    let pumps = crate::data::pump::PumpHub::new(crate::data::pump::real_pump_table());
    let data = crate::data::venues::BridgeData::new(ready.venue_settings.clone())
        .with_pumps(Arc::clone(&pumps))
        .with_credentialed(credentialed);
    let budget = Arc::clone(&ready.budget);
    let rest = crate::rest::lane::VenueRest::new(ready.booted.settings.flags.hyperliquid_hip3)
        .with_credential_store(ready.booted.store.clone())
        .with_budget_snapshot(Arc::new(move || budget_counters(&budget.snapshot(Instant::now()))));
    (Lanes { data: Arc::new(data), rest: Arc::new(rest) }, pumps)
}

/// **The startup line that says whether the OANDA history lane is armed** (decision 0097's verdict 6,
/// re-homed with the lane by decision 0164): ONE announcing read of the practice token — the read
/// that logs the credential store's permission finding — dropped inside the `map` before anything is
/// logged, so the line reports PRESENCE and never a value. The prefix `oanda history lane:` is what an
/// operator greps; the wording after it is `oanda_history_lane_line`'s.
#[cfg(feature = "data")]
fn announce_oanda_lane(booted: &vike_boot::Booted) {
    use crate::rest::oanda::{oanda_history_lane_line, read_oanda_history_token};
    use vike_oanda::HistoryTokenError;

    let presence = read_oanda_history_token(&booted.store, true).map(drop);
    let line = oanda_history_lane_line(booted.settings_dir.as_deref(), presence);
    match presence {
        Err(HistoryTokenError::StoreUnreadable) => tracing::warn!("vike-gatewayhub: {line}"),
        Ok(()) | Err(HistoryTokenError::NotConfigured) => tracing::info!("vike-gatewayhub: {line}"),
    }
}

/// **The startup line that says whether the IG history lane is armed** (decision 0194): the PRESENCE
/// word of the demo account's three fields, asked of the store as booleans — no value of the three is
/// ever read at startup, so none can reach the line. The prefix `ig history lane:` is what an operator
/// greps; the wording after it is `ig_history_lane_line`'s. An unreadable store is a `warn`, as OANDA's.
#[cfg(feature = "data")]
fn announce_ig_lane(booted: &vike_boot::Booted) {
    use crate::rest::ig::{ig_history_lane_line, ig_history_presence};
    use vike_gatewayhub_client::credential::CredentialPresence;

    let presence = ig_history_presence(&booted.store);
    let line = ig_history_lane_line(booted.settings_dir.as_deref(), presence);
    match presence {
        CredentialPresence::Unreadable => tracing::warn!("vike-gatewayhub: {line}"),
        _ => tracing::info!("vike-gatewayhub: {line}"),
    }
}

/// **The startup line that says whether the cTrader history lane is armed** (decision 0097's verdict 6, as
/// for OANDA's and IG's): one PRESENCE probe of the demo account's token pair, a word and never a value, so
/// the log says whether a credentialed fetch for ctrader will be answered. The prefix `ctrader history lane:`
/// is what an operator greps; the wording after it is `ctrader_history_lane_line`'s.
#[cfg(feature = "data")]
fn announce_ctrader_lane(booted: &vike_boot::Booted) {
    use crate::rest::ctrader::{ctrader_history_lane_line, ctrader_history_presence};
    use vike_gatewayhub_client::credential::CredentialPresence;

    let presence = ctrader_history_presence(&booted.store);
    let line = ctrader_history_lane_line(booted.settings_dir.as_deref(), presence);
    match presence {
        CredentialPresence::Unreadable => tracing::warn!("vike-gatewayhub: {line}"),
        _ => tracing::info!("vike-gatewayhub: {line}"),
    }
}

/// The shadow ledger as the status verb's counters: two rows per budget line, one per POOL.
///
/// The ledger splits a line's ceiling into an order reserve and a shared pool and tracks the two
/// windows, not the four traffic classes (`crate::budget::line`), so the rows are `order` (the
/// reserve) and `shared` (what live, interactive and bulk traffic draw together, an order's overflow
/// included). `cap` is the POOL'S size, a margin under the venue's published ceiling, so `used`
/// and `cap` are comparable; `would_throttle` is the line's count of requests judged over their
/// split, which is judged on the shared pool, so it rides that row. The axis is not a wire field:
/// within one venue and host the budget scope names it (orders per account, WebSocket sends per
/// connection, REST weight per IP).
///
/// Wave 1's budget refuses nothing, and no lane spends against it yet, so the rows are empty until a
/// call site does.
#[cfg(any(feature = "data", test))]
pub(super) fn budget_counters(snapshot: &Snapshot) -> Vec<BudgetCounter> {
    let mut rows = Vec::with_capacity(snapshot.lines.len() * 2);
    for line in &snapshot.lines {
        let row = |class: &str, used: u64, cap: u64, would_throttle: u64| BudgetCounter {
            venue: line.key.venue.clone(),
            host: line.key.host.to_string(),
            scope: line.key.scope.as_str().to_string(),
            class: class.to_string(),
            used,
            cap,
            window_ms: line.window_ms,
            would_throttle,
        };
        rows.push(row("order", line.order_used, line.order_reserve, 0));
        rows.push(row("shared", line.shared_used, line.shared_pool, line.would_throttle));
    }
    rows
}
