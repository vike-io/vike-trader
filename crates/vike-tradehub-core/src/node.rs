//! The runtime assembly: [`assemble_runtime`] and the types it is configured by and returns
//! (`src/node/accounts.rs`: accounts and the armed set; `src/node/mounts.rs`: journal records and
//! leg derivation; `src/node/build.rs`: the build).
//!
//! [`assemble_runtime`] is the daemon's runtime assembly: one [`vike_exec::ExecutionEngine`] per wired
//! market ([`wired_markets`]) through [`crate::make_engine`] (so this module names NO bridge
//! crate), the `recon_clients` list, the single-writer core ([`vike_core::spawn_core_multi`]) over a
//! caller-built [`vike_core::CoreConfig`], and the live-event forwarder (the lane every live exec
//! client pushes into, relayed into core ingest after spawn). ⚠ The desktop has no local core
//! (#1610): `crates/vike-desktop/tests/app_local_core_gate.rs` refuses this call in the shell.
//!
//! ## It names no venue
//! WHICH markets it mounts is [`wired_markets`]'s answer over [`RuntimeConfig::registry`]: one
//! [`WiredMarket`] per venue [`wired_markets`] returns, with its default symbol and engine rank
//! (docs/decisions/0098). The assembly is one loop over the rows.
//!
//! ## The boundary — what stays with the CALLER
//! assemble_runtime does NOT create the market-data feeds, the `BookStore`/`TradeStore`, the
//! `CoreSinkAdapter` or the [`crate::ReconDriver`]: they belong to the composition root
//! (`vike-tradehub`). ⚠ Not `vike-app`: the desktop builds no core and mounts no feed (#1610). Feeds
//! name the bridge crates DIRECTLY (`vike_binance::market_feed::Feeds`, …), which this crate must
//! not; the `ReconDriver` reads the feed-status map (`recon_feed_statuses`), so it is mounted beside
//! them from [`AssembledRuntime::recon_clients`] + [`AssembledRuntime::recon_trigger`]. A headless caller mounts recon with an
//! empty status map (every venue Healthy) or skips it, as `crate::build_paper_maker_core` does.
//!
//! ## Inputs/outputs
//! Everything `make_engine` reads is a [`RuntimeConfig`] field; everything a caller wires the process
//! from is a [`AssembledRuntime`] field.
//!
//! ## The startup preflight
//! [`assemble_runtime`] runs [`crate::startup::run_startup_preflight`] before the first `make_engine`, so
//! no caller has to remember it. The report is ENFORCED per venue ([`assemble_runtime_with_preflight`],
//! the injected-report twin): a venue's hard FAIL withholds its credentials and stops its
//! `demo`/`live` accounts (decision 0156: they do not run, with `PreflightFailed` as the reason); a
//! global no-go only logs, and the mount proceeds. It adds no
//! [`RuntimeConfig`]/[`AssembledRuntime`] field: `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs` builds and
//! destructures both with exhaustive literals, so a new field breaks that caller.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_core::{CoreConfig, CoreHandle};

mod accounts;
mod build;
mod mounts;

pub use accounts::{
    RuntimeAccount, account_symbols_for, armed_live_venues, refuse_unarmed_live_venues,
    runnable_accounts,
};
pub use build::{assemble_runtime, assemble_runtime_with_preflight};
pub use mounts::journal_venue_mounts;

/// One market [`assemble_runtime`] mounts: the venue, its DEFAULT engine's symbol and where that engine
/// sits in the core — the view [`wired_markets`] builds from a `vike_model` `VENUE_WIRING` row
/// that carries an engine (`crates/vike-venues/src/venue_wiring.rs`). Nothing declares one by
/// hand: every fact on it is that table's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiredMarket {
    /// A `vike_model::VENUES` id.
    pub venue: &'static str,
    /// The symbol the venue's DEFAULT engine mounts on.
    pub symbol: &'static str,
    /// Position of the DEFAULT engine in the core and of its reconcile leg in
    /// [`AssembledRuntime::recon_clients`]: ascending, smallest = primary; independent of the table (MOUNT)
    /// order. Distinct per table (debug [`assemble_runtime`] panics; release holds ties in mount order).
    pub engine_rank: u8,
}

/// **The markets a build MOUNTS**: the `vike_model` `VENUE_WIRING` rows that carry an engine, in
/// row (MOUNT) order, kept where `registry` holds a [`crate::VenueRow::Adapter`] for the venue — a
/// `FeatureAbsent` row (a bridge this binary was built without) or no row drops it. The one place
/// the wiring table meets the build: [`assemble_runtime`] mounts these, [`armed_live_venues`] claims
/// locks over them, and the xEMM live mount validates against them.
#[must_use]
pub fn wired_markets(registry: &[crate::VenueRow]) -> Vec<WiredMarket> {
    vike_venues::venue_wiring::VENUE_WIRING
        .iter()
        .filter_map(|&(venue, wiring)| {
            let (symbol, engine_rank) = wiring.engine()?;
            matches!(
                registry.iter().find(|r| r.venue() == venue),
                Some(crate::VenueRow::Adapter(_))
            )
            .then_some(WiredMarket { venue, symbol, engine_rank })
        })
        .collect()
}

/// The INPUTS the runtime assembly reads. Consumed BY VALUE: it owns the move-only [`CoreConfig`]
/// (`Box<dyn FnMut>` closures), which `spawn_core_multi` takes. The reconnect-trigger channel is
/// no field: the assembly builds it and returns it in [`AssembledRuntime::recon_trigger`].
pub struct RuntimeConfig {
    /// The venue REGISTRY (one [`crate::VenueRow`] per roster venue) from the composition root that
    /// names the bridges (`vike-tradehub`'s `REGISTRY`, docs/decisions/0096); this crate names no
    /// bridge, so every venue call `assemble_runtime` makes passes it.
    pub registry: &'static [crate::VenueRow],
    /// The credential map the root read (`vike_secrets::CredentialStore::read_all`, under the
    /// root's `VIKE_NO_LIVE_KEYS` scope), every mount's
    /// `AdapterEnv::vars`. Absent credentials ARE the live gate: under an empty map no `demo`/`live`
    /// account runs (`vike-tradehub`'s `assemble_runtime_paper.rs`).
    pub vars: HashMap<String, String>,
    /// The opt-in PIT-`SymbolProperties` recorder (`flags.record_properties`), CONSTRUCTED by a
    /// binary (`vike_store::open_properties_recorder` names the concrete store; this crate
    /// stays trait-only) and shared by every `make_engine`. `None` = no recorder, byte-identical.
    /// ⚠ No caller constructs one today: the daemon passes `None` (docs/decisions/0084 — the
    /// surviving recorder runs inside the datahub).
    pub properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// Seed equity for the `extra` engines; the PRIMARY's rides `core_config.seed_cash` (the daemon
    /// passes its primary mount's `seed_cash` for both).
    pub seed_cash: f64,
    /// The caller's reconcile-gate verdict (`vike_tradehub::reconcile_config::reconcile_gate`: ON
    /// for a mount arming a live account, `flags.reconcile_off` wins, `flags.reconcile` /
    /// `VIKE_RECONCILE=1` forces). Builds the reconnect-trigger channel; the caller reuses it for
    /// its `ReconDriver`.
    pub recon_enabled: bool,
    /// The fully-built [`CoreConfig`] (`seed_cash` plus every safety/observer knob), CONSUMED at
    /// `spawn_core_multi`.
    pub core_config: CoreConfig,
    /// The operator's `[risk]` budget, threaded VERBATIM into every `make_engine` (one resolved
    /// profile — the daemon's ACTIVE `run` row — for all venues). `None` leaves `RiskLimits` as
    /// `make_engine` builds them (venue grid + `im_requirement` rescue only); `Some` merges the same
    /// table a backtest would read via `vike_model::ProfileRisk::apply_to`
    /// (`crate::make_engine`'s doc).
    pub risk_profile: Option<vike_model::ProfileRisk>,
    /// This MACHINE's hard ceilings: the `policy` rows in `<project>/settings/db/vike.db`, loaded by
    /// the BINARY and projected onto [`crate::AdapterPolicy`], threaded VERBATIM into every
    /// `make_engine`.
    ///
    /// Not [`Self::risk_profile`]: a run profile is per-RUN and operator-owned (file, env, CLI); a
    /// policy is per-MACHINE, admin-owned, with **no env or CLI layer** — why `vike_config::Policy`
    /// is its own type.
    ///
    /// ⚠ [`crate::AdapterPolicy::default()`] (no `policy` rows, no account table read) runs NO
    /// account whatever its credentials: an account trades only at the tier its ACTIVE `account`
    /// row states ([`crate::AdapterPolicy::accounts`]), and a gate whose absence armed everything is
    /// none. Other fields keep each venue's compiled-in literal. The fold: `crate::make_engine`'s
    /// doc; `crate::report_table_decision` and `crate::report_not_running` name each account that
    /// does not run.
    pub policy: crate::AdapterPolicy,
}

/// The assembled runtime: the live [`CoreHandle`] plus the handles a caller wires the process from.
/// Destructure into same-named locals (`let AssembledRuntime { handle: core, recon_clients, recon_trigger,
/// live_venues, forwarder_stop, not_running } = node;`).
pub struct AssembledRuntime {
    /// The single-writer live core (the `vt-core` runtime).
    pub handle: CoreHandle,
    /// One leg per credentialed venue with a reconcile client (EMPTY on pure paper); the caller
    /// mounts `vike_tradehub_core::spawn_recon` from it beside the feed-status map.
    /// ⚠ ONE ROW PER ACCOUNT, not per venue: `vike_tradehub_core::ReconLeg` carries venue and route key as
    /// separate facts, both stamped on every payload. No labelled account ⇒ the same rows as
    /// ever, each `route_key: None`.
    pub recon_clients: Vec<crate::ReconLeg>,
    /// The reconnect-trigger channel (`Some` iff [`RuntimeConfig::recon_enabled`]): its `Sender` is in
    /// every trigger-firing live venue's resync supervisor (binance/bybit/okx/hyperliquid), and
    /// `vike_tradehub_core::spawn_recon` ADOPTS the `(tx, rx)` pair, so triggers reach the SAME driver.
    pub recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    /// Venues with a credential-gated LIVE exec client — the DOM's `● LIVE` badge set.
    pub live_venues: HashSet<String>,
    /// Raised BEFORE core shutdown so the forwarder drains-and-drops (no teardown deadlock;
    /// `src/node/build.rs`'s TEARDOWN SAFETY note).
    pub forwarder_stop: Arc<AtomicBool>,
    /// **Every account of a wired market that does NOT RUN, with why** (decision 0156): no engine
    /// for it in the core, and no paper engine in its place. The core holds the same list for its
    /// refusals; the composition root publishes it (`vike-tradehub`'s observe frame), so a client
    /// can show each account's reason instead of a bare "not running".
    pub not_running: Vec<crate::NotRunning>,
}

/// [`assemble_runtime`] failure: the forwarder thread spawn, a LIVE venue refusing to start without an
/// account-dependent risk budget (`max_notional_per_order`/`max_total_exposure`;
/// [`crate::require_live_risk_budget`]'s doc: why no universal default), or a refused arming.
///
/// **`Display` is the operator-facing contract, not a label.** The caller (vike-tradehub's
/// `VIKE_TRADEHUB_LIVE=1` arm) PRINTS it and exits, never `.expect`s: the risk-budget miss is a
/// FIRST-run mistake. `RiskBudget` forwards [`crate::EngineError`]'s `Display` VERBATIM (resolver,
/// missing keys, a working `[risk]` example); wrapping would corrupt its TOML block.
/// `crates/vike-tradehub-core/tests/risk_budget_diagnostic.rs` pins it.
#[derive(Debug)]
pub enum RuntimeError {
    /// The `live-event-forward` OS thread could not be spawned.
    ForwarderSpawn(std::io::Error),
    /// A live venue mount refused to start over a missing account-dependent risk budget.
    RiskBudget(crate::EngineError),
    /// **A venue armed live that the pre-assembly armed set did not name** — the
    /// [`refuse_unarmed_live_venues`] backstop: [`armed_live_venues`]'s probe UNDER-counted, so no
    /// `vike_boot::live_lock` sentinel guards an account with a real exec session — the Danger-2
    /// accident the lock exists to prevent.
    UnarmedLiveVenues {
        /// The venues that armed live outside the armed set, sorted.
        venues: Vec<String>,
        /// The pre-assembly armed set the caller locked from, for the diagnostic.
        armed: Vec<String>,
    },
    /// **A strategy mount names an account this box does not run** — the default account included
    /// since decision 0156 (`refuse_unarmed_mount_accounts` argues it): falling back to the DEFAULT
    /// account trades the wrong book silently, and a strategy cannot run on an account that has no
    /// engine. Names the venue, the account, the mount's symbol and the reason.
    Mount(String),
    /// **No account of any wired market runs** (decision 0156): the core needs a primary engine,
    /// and an account that does not run is never given a paper one. Carries every account and its
    /// reason, so the refusal says what to change.
    NoAccountRuns {
        /// Every account the node knows, each with why it does not run.
        not_running: Vec<crate::NotRunning>,
    },
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::ForwarderSpawn(e) => write!(f, "spawn live-event-forward thread: {e}"),
            RuntimeError::RiskBudget(e) => write!(f, "{e}"),
            RuntimeError::UnarmedLiveVenues { venues, armed } => write!(
                f,
                "refusing the runtime: {venues:?} armed a LIVE exec client, but the pre-assembly armed \
                 set was {armed:?} — so no live-account lock was claimed for {venues:?} and a \
                 second live process on those accounts would not be refused. This is a probe \
                 UNDER-COUNT: `vike_tradehub_core::would_go_live_under` carries no row matching the \
                 arm that fired. Add the row (it lives beside the arm's own credential loader), \
                 or remove the arm."
            ),
            RuntimeError::Mount(m) => write!(f, "refusing the runtime: {m}"),
            RuntimeError::NoAccountRuns { not_running } => {
                write!(
                    f,
                    "refusing the runtime: no account of any wired market runs — an account runs at \
                     the tier its active `account` row states (paper, demo or live) or not at all, \
                     and none here does. `vike-cli secrets accounts` lists the rows; `vike-backend \
                     venues` names each block. The accounts:"
                )?;
                for nr in not_running {
                    write!(f, "\n  {nr}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RuntimeError::ForwarderSpawn(e) => Some(e),
            RuntimeError::RiskBudget(e) => Some(e),
            // No inner error: `Display` carries the whole diagnostic.
            RuntimeError::UnarmedLiveVenues { .. }
            | RuntimeError::Mount(_)
            | RuntimeError::NoAccountRuns { .. } => None,
        }
    }
}

#[cfg(test)]
use accounts::refuse_unarmed_mount_accounts;
#[cfg(test)]
use mounts::{in_engine_order, legs_for_venue};

#[path = "node_tests.rs"]
#[cfg(test)]
mod node_tests;
