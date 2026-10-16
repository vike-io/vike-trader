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
//!    lane itself reads the token per CONNECTION, never at startup.
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
    .with_health(pump_health);
    crate::server::trade::TradeSetup {
        lane: Arc::new(lane),
        state_dir: ready.booted.state_dir.clone(),
        halt_path: Some(vike_boot::halt::halt_path_from_env()),
        budget: Arc::clone(&ready.budget),
    }
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
