//! The node assembly: [`build_node`] and the types it is configured by and returns
//! (`src/node/accounts.rs`: accounts and the armed set; `src/node/mounts.rs`: journal records and
//! leg derivation; `src/node/build.rs`: the build).
//!
//! [`build_node`] is the daemon's mount assembly: one [`vike_exec::ExecutionEngine`] per wired
//! market ([`NodeConfig::markets`]) through [`crate::make_engine`] (so this module names NO bridge
//! crate), the `recon_clients` list, the single-writer core ([`vike_core::spawn_core_multi`]) over a
//! caller-built [`vike_core::CoreConfig`], and the live-event forwarder (the lane every live exec
//! client pushes into, relayed into core ingest after spawn). ⚠ The desktop has no local core
//! (#1610): `crates/vike-desktop/tests/app_local_core_gate.rs` refuses this call in the shell.
//!
//! ## It names no venue
//! WHICH markets it mounts is the composition root's table ([`NodeConfig::markets`], beside
//! [`NodeConfig::registry`]): one [`WiredMarket`] per venue with its default symbol, reconnect poke
//! and engine rank (docs/decisions/0098). The assembly is one loop over the rows.
//!
//! ## The boundary — what stays with the CALLER
//! build_node does NOT create the market-data feeds, the `BookStore`/`TradeStore`, the
//! `CoreSinkAdapter` or the [`vike_core::ReconDriver`]: they belong to the composition root
//! (`vike-tradehub`). ⚠ Not `vike-app`: the desktop builds no core and mounts no feed (#1610). Feeds
//! name the bridge crates DIRECTLY (`vike_binance::market_feed::Feeds`, …), which this crate must
//! not; the `ReconDriver` reads the feed-status map (`recon_feed_statuses`), so it is mounted beside
//! them from [`Node::recon_clients`] + [`Node::recon_trigger`]. A headless caller mounts recon with an
//! empty status map (every venue Healthy) or skips it, as `crate::build_paper_maker_core` does.
//!
//! ## Inputs/outputs
//! Everything `make_engine` reads is a [`NodeConfig`] field; everything a caller wires the process
//! from is a [`Node`] field.
//!
//! ## The startup preflight
//! [`build_node`] runs [`crate::startup::run_startup_preflight`] before the first `make_engine`, so
//! no caller has to remember it. The report is ENFORCED per venue ([`build_node_with_preflight`],
//! the injected-report twin): a venue's hard FAIL withholds its credentials, so it mounts paper; a
//! global no-go only logs, and the mount proceeds. It adds no
//! [`NodeConfig`]/[`Node`] field: `crates/vike-tradehub/src/tradehub_cli/live_mount.rs` builds and
//! destructures both with exhaustive literals, so a new field breaks that caller.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_core::{CoreConfig, CoreHandle};

mod accounts;
mod build;
mod mounts;

pub use accounts::{
    MountAccount, account_symbols_for, armed_live_venues, mount_accounts,
    refuse_unarmed_live_venues,
};
pub use build::{build_node, build_node_with_preflight};
pub use mounts::{MOUNT_FAILED, journal_venue_mounts};

/// One market [`build_node`] wires: the venue, its DEFAULT engine's symbol, and two per-venue
/// facts. Rows are the composition root's (`vike-tradehub`'s `WIRED_MARKETS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiredMarket {
    /// A `vike_model::VENUES` id.
    pub venue: &'static str,
    /// The symbol the venue's DEFAULT engine mounts on.
    pub symbol: &'static str,
    /// Whether the mount gets the shared reconnect trigger ([`Node::recon_trigger`]). The effective
    /// poke is this AND `vike_bridge_core::venue_mount::VenueDeclaration::takes_recon_trigger`; a
    /// mismatch is inert or silently loses reconnect-driven reconcile, so `vike-tradehub`'s table
    /// test holds them equal. Without the poke a venue reconciles on the interval only.
    pub reconnect_poke: bool,
    /// Position of the DEFAULT engine in the core and of its reconcile leg in
    /// [`Node::recon_clients`]: ascending, smallest = primary; independent of the table (MOUNT)
    /// order. Distinct per table (debug [`build_node`] panics; release holds ties in mount order).
    pub engine_rank: u8,
}

/// The INPUTS the node assembly reads. Consumed BY VALUE: it owns the move-only [`CoreConfig`]
/// (`Box<dyn FnMut>` closures), which `spawn_core_multi` takes. The reconnect-trigger channel is
/// no field: the assembly builds it and returns it in [`Node::recon_trigger`].
pub struct NodeConfig {
    /// The venue REGISTRY (one [`crate::VenueRow`] per roster venue) from the composition root that
    /// names the bridges (`vike-tradehub`'s `REGISTRY`, docs/decisions/0096); this crate names no
    /// bridge, so every venue call `build_node` makes passes it.
    pub registry: &'static [crate::VenueRow],
    /// The WIRED MARKETS in MOUNT order (the order [`armed_live_venues`] claims locks in);
    /// `vike-tradehub`'s `WIRED_MARKETS`. [`WiredMarket::engine_rank`] sets the HELD order. At
    /// least one row (first-ranked = primary); no venue or `engine_rank` twice (debug panics).
    pub markets: &'static [WiredMarket],
    /// The credential map the root loaded
    /// (`vike_bridge_core::credentials::load_workspace_secrets_from_env`), every mount's
    /// `MountEnv::vars`. Absent credentials ARE the live gate: an empty map mounts every venue
    /// PAPER (`vike-tradehub`'s `build_node_paper.rs`).
    pub vars: HashMap<String, String>,
    /// The opt-in PIT-`SymbolProperties` recorder (`flags.record_properties`), CONSTRUCTED by a
    /// binary (`vike_data::PropertiesRecorder::open` names the concrete store; this crate
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
    /// the BINARY and projected onto [`crate::MountPolicy`], threaded VERBATIM into every
    /// `make_engine`.
    ///
    /// Not [`Self::risk_profile`]: a run profile is per-RUN and operator-owned (file, env, CLI); a
    /// policy is per-MACHINE, admin-owned, with **no env or CLI layer** — why `vike_config::Policy`
    /// is its own type.
    ///
    /// ⚠ [`crate::MountPolicy::default()`] (no `policy` rows) mounts EVERY venue paper whatever
    /// its credentials: `MountPolicy::venues`, the ARMING CEILING, defaults to `paper` (a ceiling
    /// whose absence armed everything is none). Other fields keep each venue's compiled-in literal.
    /// The fold: `crate::make_engine`'s doc; `crate::venue_arming_migration` warns a credentialled
    /// box with no `policy.venues` rows.
    pub policy: crate::MountPolicy,
}

/// The spawned node: the live [`CoreHandle`] plus the handles a caller wires the process from.
/// Destructure into same-named locals (`let Node { handle: core, recon_clients, recon_trigger,
/// live_venues, forwarder_stop } = node;`).
pub struct Node {
    /// The single-writer live core (the `vt-core` runtime).
    pub handle: CoreHandle,
    /// One leg per credentialed venue with a reconcile client (EMPTY on pure paper); the caller
    /// mounts `vike_core::spawn_recon` from it beside the feed-status map.
    /// ⚠ ONE ROW PER ACCOUNT, not per venue: `vike_core::ReconLeg` carries venue and route key as
    /// separate facts, both stamped on every payload. No `policy.accounts` rows ⇒ the same rows as
    /// ever, each `route_key: None`.
    pub recon_clients: Vec<vike_core::ReconLeg>,
    /// The reconnect-trigger channel (`Some` iff [`NodeConfig::recon_enabled`]): its `Sender` is in
    /// every poking live venue's resync supervisor (binance/bybit/okx/hyperliquid), and
    /// `vike_core::spawn_recon` ADOPTS the `(tx, rx)` pair, so pokes reach the SAME driver.
    pub recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    /// Venues with a credential-gated LIVE exec client — the DOM's `● LIVE` badge set.
    pub live_venues: HashSet<String>,
    /// Raised BEFORE core shutdown so the forwarder drains-and-drops (no teardown deadlock;
    /// `src/node/build.rs`'s TEARDOWN SAFETY note).
    pub forwarder_stop: Arc<AtomicBool>,
}

/// [`build_node`] failure: the forwarder thread spawn, a LIVE venue refusing to start without an
/// account-dependent risk budget (`max_notional_per_order`/`max_total_exposure`;
/// [`crate::require_live_risk_budget`]'s doc: why no universal default), or a refused arming.
///
/// **`Display` is the operator-facing contract, not a label.** The caller (vike-tradehub's
/// `VIKE_TRADEHUB_LIVE=1` arm) PRINTS it and exits, never `.expect`s: the risk-budget miss is a
/// FIRST-run mistake. `RiskBudget` forwards [`crate::MountError`]'s `Display` VERBATIM (resolver,
/// missing keys, a working `[risk]` example); wrapping would corrupt its TOML block.
/// `crates/vike-mount/tests/risk_budget_diagnostic.rs` pins it.
#[derive(Debug)]
pub enum NodeError {
    /// The `live-event-forward` OS thread could not be spawned.
    ForwarderSpawn(std::io::Error),
    /// A live venue mount refused to start over a missing account-dependent risk budget.
    RiskBudget(crate::MountError),
    /// **A venue armed live that the pre-mount armed set did not name** — the
    /// [`refuse_unarmed_live_venues`] backstop: [`armed_live_venues`]'s probe UNDER-counted, so no
    /// `vike_ops::live_lock` sentinel guards an account with a real exec session — the Danger-2
    /// accident the lock exists to prevent.
    UnarmedLiveVenues {
        /// The venues that armed live outside the armed set, sorted.
        venues: Vec<String>,
        /// The pre-mount armed set the caller locked from, for the diagnostic.
        armed: Vec<String>,
    },
    /// **A strategy mount named an account this box will not arm.** The one refusal that is not a
    /// degrade (`refuse_unarmed_mount_accounts` argues it): falling back to the DEFAULT account
    /// trades the wrong book silently. Names the venue, the account and the mount's symbol.
    Mount(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::ForwarderSpawn(e) => write!(f, "spawn live-event-forward thread: {e}"),
            NodeError::RiskBudget(e) => write!(f, "{e}"),
            NodeError::UnarmedLiveVenues { venues, armed } => write!(
                f,
                "refusing the node: {venues:?} armed a LIVE exec client, but the pre-mount armed \
                 set was {armed:?} — so no live-account lock was claimed for {venues:?} and a \
                 second live process on those accounts would not be refused. This is a probe \
                 UNDER-COUNT: `vike_mount::would_mount_live_under` carries no row matching the \
                 arm that fired. Add the row (it lives beside the arm's own credential loader), \
                 or remove the arm."
            ),
            NodeError::Mount(m) => write!(f, "refusing the node: {m}"),
        }
    }
}

impl std::error::Error for NodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NodeError::ForwarderSpawn(e) => Some(e),
            NodeError::RiskBudget(e) => Some(e),
            // No inner error: `Display` carries the whole diagnostic.
            NodeError::UnarmedLiveVenues { .. } | NodeError::Mount(_) => None,
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
