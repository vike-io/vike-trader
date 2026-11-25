//! **Phase: the wire** — the hook [`super::run`] hands [`super::run_with`]. It turns everything the
//! earlier phases produced ([`Ready`]) into a SERVING daemon and owns the rest of the process: the
//! hub keys, the venue lanes, the listener, the accept loop and the stop.
//!
//! # The order inside the hook
//!
//! 1. **The hub keys** (`super::keys::gateway_keys`). No keys, no start: the refusal exits before
//!    anything is built or bound — 78 (setup needed: one line naming `vike-cli gatewayhub setup`) when
//!    no key was ever minted, 2 when the store would not read — because a daemon that bound a socket and answered nothing looks
//!    healthy to a unit that waits for it, and one that answered without authenticating would let
//!    any local process spend the credentials it holds.
//! 2. **The FX login gate's clock**, the wall clock as a value (no FX venue logs in while the FX market is closed,
//!    `vike_venues::session::fx_login_closed_until`; a test that drives this hook pins a weekday instead, through
//!    `serve_wire_on`), handed to every lane below that builds an FX venue's session, then **the
//!    history lanes' startup lines** (`announce_credentialed_lanes`, one per credentialed venue's
//!    spec, in the registry's order). OANDA's is its ONE announcing read of the practice token, dropped
//!    at once, so the log says whether the credentialed history lane is armed; the lane itself reads the
//!    token per CONNECTION, never at startup. IG's is the same line from the presence word alone (no
//!    value of its three fields is read at startup), and cTrader's the same again, from the demo
//!    account's token pair.
//! 3. **The session history's writer** (`crate::link_writer`), over this store. Dials nothing, and a store that will not
//!    take a row costs the row: it can refuse no start. Then the bridges' read budget (`crate::budget::reads`'s
//!    `install`): their history replays spend this daemon's one ledger.
//! 4. **The lanes** (`production_lanes`): `DataLaneBridge` over the venue settings rows the boot read,
//!    `VenueRest` over `flags.hyperliquid_hip3`, the credential store handle and the shadow
//!    budget's snapshot. Building a lane dials nothing (a venue client is built on its first
//!    `DataOpen`), but the Polymarket bridge reads its egress from a process-wide cell the moment a
//!    client or a Gamma agent exists, which is why `super::declare_polymarket_egress` runs in
//!    [`super::run_with`] ABOVE this hook. A `trade` build that holds the Trade key sets the trade
//!    lane up here too (`trade_setup`, which dials nothing either) and serves it beside them.
//! 5. **The bind**: exactly [`Ready::bind`], the addresses the bind guard already resolved and
//!    checked. The address STRING is never re-resolved here: a second resolution could answer
//!    differently and walk around the guard (`serve` checks the bound listener once more).
//! 6. **`serve`** until the stop flag goes up, the drain, and the exit status: 0 for a clean stop,
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
use vike_hub_protocol::auth::HubKeys;

#[cfg(any(feature = "data", test))]
use crate::budget::ledger::BudgetSnapshot;
#[cfg(any(feature = "data", test))]
use crate::server::accept::serve;
#[cfg(any(feature = "data", test))]
use crate::server::bind::check_loopback;
#[cfg(any(feature = "data", test))]
use crate::server::lanes::ServerLanes;
#[cfg(any(feature = "data", test))]
use crate::server::limits::ServerConfig;

/// The wire hook of a build that links the venue bridges: refuse without keys, announce, build the
/// lanes, serve. See the module doc for the order.
#[cfg(feature = "data")]
pub(super) fn serve_wire(ready: Ready) -> ExitCode {
    serve_wire_on(ready, vike_model::now_ms)
}

/// [`serve_wire`] with the FX login gate on `fx_clock`: the wall clock in production, a fixed weekday in the test that
/// drives this hook in-process. The clock is a value handed to the trade lane, the credentialed streams and the
/// credentialed history lanes, which hand it to each FX venue session they build.
#[cfg(feature = "data")]
pub(super) fn serve_wire_on(ready: Ready, fx_clock: fn() -> i64) -> ExitCode {
    let keys = match required_keys(&ready) {
        Ok(keys) => keys,
        Err(code) => return code,
    };
    // No FX venue logs in while the FX market is closed (owner ruling 2026-11-13): the clock every lane below hands
    // each FX venue session it builds.
    let fx_clock: Arc<dyn vike_model::Clock + Send + Sync> = Arc::new(fx_clock);
    announce_credentialed_lanes(&ready.booted, &ready.venue_settings);
    // Before any venue session can open, and held to the end of this hook: its drop is the stop's gate.
    let _links = ready.booted.settings_dir.clone().map(crate::link_writer::LinkWriter::start);
    // The bridges' history replays spend this daemon's ledger, before any venue session opens (`crate::budget::reads`).
    crate::budget::reads::install(Arc::clone(&ready.budget));
    // The datahub's KEPT instrument lists (`<project>/instruments`), which a bridge reads where it would otherwise read
    // a symbol's NAME (bybit's book and venue name: `vike_bybit::instrument::class::venue_symbol`). Before any venue
    // session opens.
    if let Some(dir) =
        vike_paths::state_path::instruments_dir_beside(ready.booted.settings_dir.as_deref())
    {
        vike_catalog::use_kept_lists(dir);
    }
    // The settings database's integrity check at this start and its nightly copy, on a thread of their own; they halt
    // nothing (`crate::store_backup`).
    if let Some(dir) = ready.booted.settings_dir.clone() {
        crate::store_backup::start(dir);
    }
    // The venue addresses' probe, at this start and once a day; it halts nothing (`crate::url_probe`).
    // The store handle is for the account ROWS: a row no active account dials is not in use.
    let _url_probe = ready
        .booted
        .settings_dir
        .clone()
        .map(|dir| crate::url_probe::UrlProber::start(dir, ready.booted.store.clone()));
    let (lanes, pump_health) = production_lanes(&ready, &fx_clock);
    // The trade surface (W9b): mounted only in a `trade` build that holds the `Trade` key. Absence of the
    // key is the switch, and the advertisement follows the mount (`server::features::trade_features`).
    #[cfg(feature = "trade")]
    if keys.has(vike_hub_protocol::auth::Scope::Trade) {
        let setup = trade_setup(&ready, pump_health, fx_clock);
        return serve_ready_with_trade(ready, keys, lanes, setup);
    }
    #[cfg(not(feature = "trade"))]
    let _ = (pump_health, fx_clock);
    serve_ready(ready, keys, lanes)
}

/// **The hub keys this daemon serves under, or the exit status of a start refused without them** (step 1 of the
/// module doc): both refusals are parked (`vike_utility::cli::OPERATOR_NEEDED_EXIT`), each in its own words.
#[cfg(feature = "data")]
fn required_keys(ready: &Ready) -> Result<HubKeys, ExitCode> {
    super::keys::gateway_keys(&ready.booted.store).map_err(|why| {
        match &why {
            // Setup alone cures this one, so a restart loop never would: the operator-needed status (78, which the
            // units' `RestartPreventExitStatus=` and the server install's supervisor park on, decision 0260), ONE line.
            super::keys::GatewayKeysRefusal::NotProvisioned { .. } => tracing::error!(
                "vike-gatewayhub: setup needed: run `vike-cli gatewayhub setup` in this project, then start \
                 this daemon again — {why}"
            ),
            // A store that exists and will not read (its permissions, its contents): an operator mends it, and
            // a restart meets it again, so it is parked too, under its own words.
            super::keys::GatewayKeysRefusal::StoreUnreadable => tracing::error!(
                "vike-gatewayhub: {why}; refusing to start. The daemon is always keyed: it holds \
                 every venue connection, so it has no unauthenticated mode"
            ),
        }
        ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)
    })
}

/// **The wire hook over lanes the caller built** (`super::run_scripted`, TEST SUPPORT): [`required_keys`]'s refusal,
/// then [`serve_ready`] over `lanes`. No venue lane, no store backup and no venue-address probe, so nothing here dials
/// out.
#[cfg(all(feature = "data", feature = "test-support"))]
pub(super) fn serve_scripted(ready: Ready, lanes: ServerLanes) -> ExitCode {
    match required_keys(&ready) {
        Ok(keys) => serve_ready(ready, keys, lanes),
        Err(code) => code,
    }
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
pub(super) fn serve_ready(ready: Ready, keys: HubKeys, lanes: ServerLanes) -> ExitCode {
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
    lanes: ServerLanes,
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
/// * the lane is `RefreshedTrade` (a `TradeLaneBridge`) over [`crate::registry::trade::REGISTRY`] and the source
///   `super::vars::trade_source`: the deployment map (the store's deployment names the bridges look up), the
///   account directory (the `account` rows and EVERY account's credentials by field), the `venue_setting` rows
///   and the resolved flags (`TradeLaneSnapshot::flags`), RE-READ at each step a tradehub restart
///   drives (its arming table, preflight, tier check and `AdapterStart`), so a credential rotation or a
///   setting edit is picked up by a tradehub restart as it was when the tradehub read them at its own boot;
/// * the project's state directory is the boot's (`LIVE-*.lock`, `gateway-id`), the HALT sentinel's path the
///   one resolver's (`vike_boot::halt::halt_path_from_env`, after the boot declared the project), and the
///   budget the shadow ledger this lane's dispatch spends against, the only lane that does.
#[cfg(all(feature = "data", feature = "trade"))]
fn trade_setup(
    ready: &Ready,
    pump_health: Arc<dyn crate::health::VenueHealth>,
    fx_clock: Arc<dyn vike_model::Clock + Send + Sync>,
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
    // No FX venue's start, credential read, facts read or session logs in while the FX market is closed.
    .with_fx_clock(fx_clock)
    .with_symbol_facts(Arc::new(symbol_facts(
        ready.booted.settings.flags.hyperliquid_hip3,
        &ready.venue_settings,
    )))
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
///
/// `venue_settings` is the boot's one read of every venue's rows: each read below dials its venue's
/// address rows (`crate::data::venues::venue_urls`, decision 0265).
#[cfg(all(feature = "data", feature = "trade"))]
fn symbol_facts(
    hyperliquid_hip3: bool,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> crate::trade::adapters::symbol_facts::SymbolPropertiesCache {
    use crate::data::venues::venue_urls;
    use crate::rest::FetchContext;
    use vike_data::source::SourceError;
    let providers = crate::rest::catalog::CatalogProviders::real(hyperliquid_hip3, venue_settings);
    let urls = |venue: &str| venue_urls(venue_settings, venue);
    let (deribit, hyperliquid) = (urls("deribit"), urls("hyperliquid"));
    let (deribit_one, hyperliquid_one, polymarket) =
        (deribit.clone(), hyperliquid.clone(), urls("polymarket"));
    // The CEX readers, built ONCE on their venue's address rows; a refused build answers every read of
    // that venue with the refusal, naming the row.
    let refused = |venue: &str, why: vike_venues::venue_urls::UrlRefusal| {
        format!("{venue}: the symbol facts are NOT RUNNING: {why}")
    };
    let binance = vike_binance::catalog::CatalogProviderBinance::from_urls(&venue_urls(
        venue_settings,
        "binance",
    ))
    .map_err(|why| refused("binance", why));
    let aster =
        vike_aster::catalog::CatalogProviderAster::from_urls(&venue_urls(venue_settings, "aster"))
            .map_err(|why| refused("aster", why));
    let (bybit_urls, okx_urls) =
        (venue_urls(venue_settings, "bybit"), venue_urls(venue_settings, "okx"));
    let (lister_binance, lister_aster, lister_bybit, lister_okx) =
        (binance.clone(), aster.clone(), bybit_urls, okx_urls.clone());
    let facts =
        crate::trade::adapters::symbol_facts::SymbolPropertiesCache::new(Arc::new(move |venue| {
            // binance and aster: the whole-list read is the USDS-M futures list alone (a `.P` ask; binance's carries its
            // dated futures beside the perpetuals, aster's is its perpetuals; fapi ignores
            // `symbol=`). Spot is read one symbol at a time below, and the picker's spot+perp list (tolerant of
            // one endpoint failing, which is right for a picker) is never the facts' source: a half list kept as
            // a success would answer the missing lane "no such instrument".
            if venue == "binance" {
                return binance.clone()?.perp_listing().map_err(|e| e.0.to_string());
            }
            // binance's DEMO futures host: the list a `demo` account can sign (it is not mainnet's).
            if venue == vike_binance::symbol_properties::BINANCE_DEMO_LIST {
                return binance.clone()?.demo_perp_listing().map_err(|e| e.0.to_string());
            }
            // binance's COIN-M book, mainnet's and the DEMO host's: a COIN-M symbol's facts are its row there.
            if venue == vike_binance::symbol_properties::BINANCE_COIN_M_LIST {
                return binance.clone()?.coin_m_listing().map_err(|e| e.0.to_string());
            }
            if venue == vike_binance::symbol_properties::BINANCE_DEMO_COIN_M_LIST {
                return binance.clone()?.demo_coin_m_listing().map_err(|e| e.0.to_string());
            }
            if venue == "aster" {
                return aster.clone()?.perp_listing().map_err(|e| e.0.to_string());
            }
            // bybit has no whole-list read: every symbol is `instruments-info?symbol=` (below).
            if venue == "bybit" {
                return Err("bybit: instruments are read one symbol at a time".to_string());
            }
            // okx's catalog is four listings plus one per option family; its facts need the SWAP rows only. A `demo`
            // account's list is the simulated-trading one (`vike_okx::symbol_properties::OKX_DEMO_LIST`: same host, a header), not mainnet's.
            if venue == "okx" || venue == vike_okx::symbol_properties::OKX_DEMO_LIST {
                let demo = venue == vike_okx::symbol_properties::OKX_DEMO_LIST;
                return vike_okx::catalog::CatalogProviderOkx::list_swap_instruments(
                    &okx_urls, demo,
                )
                .map_err(|e| {
                    format!("{venue}: the venue's instrument list could not be fetched: {}", e.0)
                });
            }
            // deribit signs on the TESTNET book whatever the public catalog (mainnet) lists, so its facts
            // are the testnet's futures-kind rows (perpetuals and dated futures; an option is read one at a
            // time below): the grid the door admits is the grid the adapter signs on.
            if venue == "deribit" {
                return vike_deribit::execution::fetch_testnet_futures(&deribit).map_err(|e| {
                    format!("{venue}: the venue's instrument list could not be fetched: {e}")
                });
            }
            // hyperliquid's `demo` accounts trade the TESTNET, whose universe is not mainnet's (19 mainnet coins are
            // not listed there, and the same coin has another asset index): an account on that network is asked for
            // the testnet's own `meta` (`SymbolFacts::facts_on`), core markets only (the facts refuse HIP-3 anyway).
            if venue == vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST {
                use vike_catalog::CatalogProvider as _;
                return vike_hyperliquid::catalog::CatalogProviderHyperliquid::for_network(
                    vike_model::VenueNetwork::Testnet,
                    &hyperliquid,
                )
                .list_instruments()
                .map_err(|e| {
                    format!("{venue}: the testnet's instrument list could not be fetched: {}", e.0)
                });
            }
            providers.list(venue, &FetchContext::new()).map_err(|e| match e {
                SourceError::Fetch(text) | SourceError::Refused(text) => text.to_string(),
            })
        }));
    // One symbol at a time where the venue filters (the venue's `SymbolPropertiesListSpec::one_at_a_time` says which asks):
    // binance/aster spot `exchangeInfo?symbol=`, bybit `instruments-info?symbol=`.
    facts.with_symbol_lister(Arc::new(move |venue, symbol| match venue {
        "binance" => lister_binance.clone()?.spot_symbol(symbol).map_err(|e| e.0.to_string()),
        "aster" => lister_aster.clone()?.spot_symbol(symbol).map_err(|e| e.0.to_string()),
        // bybit: a USER name (decision 0290) whose kept row names the book and the venue's name: a SPOT pair is read
        // in the spot list, every other row (a perpetual, a dated future, a coin-margined contract) by the venue's
        // `symbol=` read, which answers in either derivative book.
        "bybit" => {
            let (wire, book) = vike_bybit::instrument::class::venue_symbol(symbol)?;
            match book {
                vike_bybit::history::Category::Spot => {
                    vike_bybit::catalog::spot_symbol(&lister_bybit, &wire)
                }
                _ => vike_bybit::catalog::perp_symbol(&lister_bybit, &wire),
            }
            .map_err(|e| e.0.to_string())
        }
        // okx: a SPOT pair or a dated future by a point read; a SWAP stays in the one bulk list above. The key
        // says which network: `OKX_DEMO_LIST` is the simulated-trading one, the bare slug mainnet.
        "okx" | vike_okx::symbol_properties::OKX_DEMO_LIST => {
            let demo = venue == vike_okx::symbol_properties::OKX_DEMO_LIST;
            vike_okx::catalog::CatalogProviderOkx::instrument_rows(&lister_okx, symbol, demo)
                .map_err(|e| e.0.to_string())
        }
        // deribit: an option (the venue lists thousands), by its own row on the testnet the adapter signs on.
        "deribit" => vike_deribit::execution::fetch_testnet_instrument(&deribit_one, symbol),
        // polymarket lists tens of thousands of outcome tokens: one token's market, by Gamma's point lookup.
        "polymarket" => vike_polymarket::catalog::outcome_rows(&polymarket, symbol),
        // hyperliquid: a builder-dex coin (`dex:COIN`), by its own dex alone; the key says which network.
        "hyperliquid" | vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST => {
            hyperliquid_dex_rows(&hyperliquid_one, venue, symbol)
        }
        _ => Err(format!("{venue}: no one-symbol read")),
    }))
}

/// **The catalog rows of the builder dex a `dex:COIN` names**, on the network the listing key says
/// (`HYPERLIQUID_TESTNET_LIST` is the testnet, the bare slug mainnet). Two keyless reads
/// (`CatalogProviderHyperliquid::dex_instruments`) at that network's address rows of `urls`; a dex the venue
/// does not list answers no rows.
#[cfg(all(feature = "data", feature = "trade"))]
fn hyperliquid_dex_rows(
    urls: &vike_venues::venue_urls::VenueUrls,
    venue: &str,
    symbol: &str,
) -> Result<Vec<vike_model::Instrument>, String> {
    use vike_model::VenueNetwork;
    let network = if venue == vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST {
        VenueNetwork::Testnet
    } else {
        VenueNetwork::Mainnet
    };
    let Some((dex, _)) = symbol.split_once(':').filter(|(dex, _)| !dex.is_empty()) else {
        return Err(format!("{venue}: `{symbol}` is not a builder-dex coin (`dex:COIN`)"));
    };
    vike_hyperliquid::catalog::CatalogProviderHyperliquid::dex_instruments(network, urls, dex)
        .map_err(|e| e.0.to_string())
}

/// **A fresh read of the `account` table**, for the supervised IBKR link's predicate (decision 0164 verdict
/// 7(b)): it is asked before every reconnect attempt, so an `account.active` flip made after the daemon
/// started stops the reconnect within one attempt. ROWS ONLY: the key names and the credential values are
/// not read (the predicate decides a tier, and a value read here would only be dropped again).
#[cfg(all(feature = "data", feature = "trade"))]
fn account_rows(
    store: vike_secrets::CredentialStore,
) -> crate::trade::adapters::bridge_trade::AccountDirectorySource {
    Arc::new(move || {
        vike_secrets::account_directory::AccountDirectory::read(
            store.accounts(),
            Ok::<_, String>(None),
            Ok::<_, String>(vike_secrets::account_credentials::CredentialRegistry::new()),
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
/// * `DataLaneBridge::new(venue_settings)`: the settings come from [`Ready::venue_settings`], the boot's
///   ONE read of the `venue_setting` table; the lane reads no store. It also hosts the tick pumps and
///   serves Hyperliquid's network; the credentialed streams (wave 2, W10) are added from the credential
///   store handle and read their account per open.
/// * `VenueRest::new(hyperliquid_hip3, venue_settings)`, its sources built from the same read's
///   address rows (decision 0265): the wire's `CatalogList` carries no such flag, so this daemon
///   reads `flags.hyperliquid_hip3` itself, once, as the datahub's catalog did, and it cannot travel
///   from the datahub. The credential store handle arms the credentialed OANDA lane (a location and
///   a scope, no value), the venues' `venue_url` rows out of [`Ready::venue_settings`] are where the
///   credentialed lanes dial, and the shadow budget's snapshot is what the status verb reports.
/// * `fx_clock`, the FX login gate's clock, goes to the credentialed streams and the credentialed history
///   lanes, whose IG and cTrader sessions log in on it.
#[cfg(feature = "data")]
fn production_lanes(
    ready: &Ready,
    fx_clock: &Arc<dyn vike_model::Clock + Send + Sync>,
) -> (ServerLanes, Arc<dyn crate::health::VenueHealth>) {
    // The strategy lane's credentialed streams read their account from the credential store, per open,
    // and rotate ctrader's grant into the project state directory this boot declared.
    let credentialed = crate::data::credentialed::CredentialedStreamLane::new(
        crate::data::accounts::AccountSourceStore::source(ready.booted.store.clone()),
        vike_boot::halt::declared_project_state_dir(),
        &ready.venue_settings,
        Some(Arc::clone(fx_clock)),
    );
    // The ONE pump hub of the daemon is built here (not inside `DataLaneBridge::new`) so the trade lane can read
    // its health: the reconcile health gate's evidence (`crate::health`, R31).
    let pumps = crate::data::feed_tick::FeedTickHub::new(crate::data::feed_tick::feed_tick_table(
        ready.venue_settings.clone(),
    ));
    let data = crate::data::venues::DataLaneBridge::new(ready.venue_settings.clone())
        .with_feed_ticks(Arc::clone(&pumps))
        .with_credentialed(credentialed);
    let budget = Arc::clone(&ready.budget);
    let hip3 = ready.booted.settings.flags.hyperliquid_hip3;
    let rest = crate::rest::lane::RestLaneBridge::new(hip3, &ready.venue_settings)
        .with_credential_store(
            ready.booted.store.clone(),
            &ready.venue_settings,
            Some(Arc::clone(fx_clock)),
        )
        .with_budget_snapshot(Arc::new(move || budget_counters(&budget.snapshot(Instant::now()))));
    (ServerLanes { data: Arc::new(data), rest: Arc::new(rest) }, pumps)
}

/// **The startup lines that say whether each credentialed history lane is armed** (decision 0097's
/// verdict 6, re-homed with the lanes by decision 0164, and decision 0194), in the registry's order: each
/// venue's spec (`crate::rest::lane::credentialed_specs`, over the boot's store handle) reads what its
/// line needs — OANDA's is ONE announcing read of the practice token, the read that logs the credential
/// store's permission finding, dropped before the line is built; IG's and cTrader's are PRESENCE words,
/// asked of the store as booleans — so a line reports PRESENCE and never a value. A store that will not
/// read is a `warn`, every other state an `info`. The prefix `<venue> history lane:` is what an operator
/// greps; the wording after it is the venue's bridge's.
#[cfg(feature = "data")]
pub(super) fn announce_credentialed_lanes(
    booted: &vike_boot::Booted,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) {
    use vike_model::CredentialPresence;

    // A presence word each, never a login: no clock is needed to say it.
    for spec in crate::rest::lane::credentialed_specs(&booted.store, venue_settings, None) {
        let (presence, line) = spec.startup_line(booted.settings_dir.as_deref());
        match presence {
            CredentialPresence::Unreadable => tracing::warn!("vike-gatewayhub: {line}"),
            _ => tracing::info!("vike-gatewayhub: {line}"),
        }
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
/// The budget refuses nothing and only the trade dispatch spends against it, so the rows count orders
/// alone and stay empty until an order-class command goes out.
#[cfg(any(feature = "data", test))]
pub(super) fn budget_counters(snapshot: &BudgetSnapshot) -> Vec<BudgetCounter> {
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
