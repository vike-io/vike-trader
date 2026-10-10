//! `ReconManager` — the reconciliation runtime driver that ties the pure reconcile engine
//! (`vike_exec::recon`) into the live single-writer core. Phase-3 startup-cadence wiring.
//!
//! **Design (Option A — reports-in, compute-on-fold).** `recon::diff`/`recon::resolve` need the
//! CURRENT local engine state (`LocalView`), which lives inside the `ExecutionEngine` on the core
//! FOLD thread. This manager runs on its OWN thread and does ONLY the blocking REST report fetch
//! (`ReconClient::fetch_*`) — it must never touch the p99<10µs fold. It then enqueues the raw
//! reports as [`vike_exec::Command::ReconcileReports`]; the fold thread reads its own
//! `local_view()`, runs the pure diff/resolve, and folds the synthesized events through the same
//! `on_event` path real venue events use (`runtime::CoreThread::reconcile_reports`). This keeps
//! diff/resolve on the same thread as the state they read — no cross-thread local-state snapshot,
//! no staleness window — and reuses the existing command choke point rather than adding a
//! query/reply lane.
//!
//! **Single-writer invariant.** This thread NEVER mutates engine/`Account` state: it fetches
//! (blocking I/O) and enqueues a `Command`. All state mutation stays on the fold thread. It holds
//! a WEAK ingest sender (like [`crate::runtime::CoreHandle::spawn_periodic_reconcile`]) so it never
//! keeps the core alive — it self-exits when the core is gone or on [`ReconDriver::shutdown`].
//!
//! **Cadence.** A startup pass (one fetch+enqueue per venue after `startup_delay`), PLUS
//! (Task 13) an on-demand pass whenever a venue bridge pokes the driver's reconcile-trigger
//! channel — typically wired to a reconnect (a bridge's `run_resync_supervisor` fires it after
//! its own event-replay settles, complementing rather than replacing that replay: see
//! `vike_bridge_core::user_data::run_resync_supervisor`'s doc for the blind spots neither half
//! covers alone). Both cadences share [`ReconManager::run_startup_pass`] — the trigger loop calls
//! the exact same fetch→diff/resolve pass, it just runs it more than once.
//!
//! **Task 16 (continuous audits).** Two MORE timer arms on the same select loop, both optional
//! and both `None` by default (byte-identical to pre-Task-16 behavior):
//!   - [`ReconConfig::interval`]: re-runs [`ReconManager::run_startup_pass`] on a fixed cadence,
//!     in ADDITION to startup + trigger — same pass, same [`ReconManager::should_reconcile`]
//!     health gate, just fired more often. This is what makes a reconcile pass IDEMPOTENT under
//!     continuous cadence load-bearing rather than merely convenient: a venue trade_id already
//!     folded diffs to zero divergences (`vike_exec::recon::diff` dedups fills by trade_id
//!     against the local engine's `seen_trade_ids`), so a tick that finds nothing new folds
//!     nothing new (see `crates/vike-core/tests/recon/recon_continuous_audit.rs`).
//!   - [`ReconConfig::audit_interval`]: a lighter DELEGATED tick — see
//!     [`ReconManager::run_audit_tick`] — that does NOT run a reconcile pass at all. It pokes the
//!     core's own pre-existing stuck-order watchdog waker (`Ingest::Watchdog`, already spawned by
//!     `runtime::spawn_core` whenever `CoreConfig::submit_ack_timeout` is set) rather than
//!     reimplementing any sweep logic here.
//!
//! The internal-book-vs-venue-book audit (comparing local positions against venue-reported
//! positions) is explicitly OUT of scope for a separate mechanism: [`ReconManager::run_startup_pass`]
//! already diffs `PositionStatusReport`s against local state on every pass (`Divergence::
//! PositionDrift`/`ExternalOnlyPosition` in `vike_exec::recon::diff`) — the interval cadence above
//! is what turns that existing diff into a continuous audit, no new plumbing needed.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use vike_exec::recon::{BalanceTol, ReconPolicy};

mod driver;
mod liveness;
mod manager;

pub use driver::{ReconDriver, spawn_recon};
pub use manager::{ReconLeg, ReconManager};

/// Neutral outage classification the reconcile driver gates on. Deliberately NOT
/// `vike_bridge_core::stream_health::StreamHealth`/`ConnectivityProbe` — `vike-core` depends on
/// `vike-exec` + `vike-model` only (down-only layering) and must not pull in `vike-bridge-core`.
/// This enum is the same generic-boundary trick [`ReconDriver::reconcile_trigger`] (Task 13) used
/// for its trigger channel: the app root, which DOES see both crates, is the one place that maps
/// the real bridge health signal onto this closure (see [`ReconConfig::health`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconHealth {
    /// Local network + venue feed both look connected — reconcile passes run normally.
    Healthy,
    /// Local network down OR a venue feed is mid-gap (resyncing) — reconcile passes are
    /// suppressed for this poll/poke so a blip doesn't fire a reconcile storm against reports
    /// the venue itself hasn't caught up on yet.
    Degraded,
}

/// A per-venue reconcile health probe (`venue -> ReconHealth`), wired by the app root to each
/// venue's real feed health. Factored into an alias so the `ReconConfig`/`ReconManager` `health`
/// fields don't trip `clippy::type_complexity`.
pub type HealthProbe = Arc<dyn Fn(&str) -> ReconHealth + Send + Sync>;

/// Reconcile driver configuration. `policy`, `lookback_ms`, `startup_delay`, `health`, `interval`,
/// `audit_interval`, and `generate_missing_orders` are all live.
pub struct ReconConfig {
    /// Fold-vs-quarantine policy per divergence kind (default = Synthesize everything).
    pub policy: ReconPolicy,
    /// How far back (ms) to request order/fill reports at pass time (`since = now - lookback_ms`).
    pub lookback_ms: i64,
    /// Delay before the startup pass runs (0 = immediately).
    pub startup_delay: Duration,
    /// Task 15: optional PER-VENUE health probe consulted before each venue's leg of a pass. The
    /// `&str` is the venue being reconciled — so a bybit-disconnected moment suppresses only bybit's
    /// leg while binance's still runs (the earlier binance-only gate let an unhealthy venue reconcile
    /// as long as binance was up). `None` = always [`ReconHealth::Healthy`] — byte-identical to
    /// pre-Task-15 behavior. Wired by the app root to each venue's real feed health (`StreamHealth`/
    /// `ConnectivityProbe`), which `vike-core` cannot name directly (see [`ReconHealth`]'s doc); an
    /// unknown venue should map to `Healthy` so an un-gated venue is never blocked.
    pub health: Option<HealthProbe>,
    /// Task 16: continuous re-reconcile cadence. `Some(d)` re-runs the SAME
    /// [`ReconManager::run_startup_pass`] every `d`, on top of startup + trigger passes. `None`
    /// (default) = no interval timer arm at all — byte-identical to pre-Task-16 behavior.
    pub interval: Option<Duration>,
    /// Synthesize adoption events for a venue order with no local match
    /// (`vike_exec::recon::DivergenceKind::UnknownOrder`), instead of `resolve`'s default empty
    /// catch-all for that kind. Threaded verbatim into each pass's [`vike_exec::ReconcileReports`]
    /// (the fold thread reads `ReconConfig` via that payload, not this struct directly — see
    /// `ReconManager::run_startup_pass`). `false` (default) is byte-identical to before this flag
    /// existed: under `hybrid`, `UnknownOrder` still quarantines with EMPTY proposed events, so an
    /// operator confirm has nothing to adopt.
    ///
    /// `true` enables the NARROW adoption semantics of `vike_exec::recon::resolve`'s
    /// `AdoptContext` (its module doc is the authority): only a TERMINAL unknown order with
    /// executed qty and no fill report in the same pass synthesizes anything that folds (a
    /// decorative accept + one cumulative `Fill`, deterministic `EXT-ORD-*` trade_id); a live or
    /// fill-lane-covered unknown order folds NOTHING (its executions arrive as `MissingFill`
    /// divergences with real venue trade-ids), surfacing only a dedup-keyed held alert under
    /// `hybrid`/`quarantine` for operator visibility/adoption. Once adopted, recurring passes are
    /// a true no-op (no events, no counter drift, no new alert rows).
    pub generate_missing_orders: bool,
    /// Task 16: periodic in-flight-timeout audit cadence. `Some(d)` pokes the core's existing
    /// stuck-order watchdog waker every `d` (see [`ReconManager::run_audit_tick`] — DELEGATED, not
    /// a reimplemented sweep). `None` (default) = no audit timer arm.
    pub audit_interval: Option<Duration>,
    /// Feature 2 (`VIKE_RECONCILE_BALANCE`): promote venue balance from a silent authoritative
    /// overwrite to a first-class DIFFED dimension. Threaded verbatim into each pass's
    /// [`vike_exec::ReconcileReports`] (the fold thread reads `ReconConfig` only through that
    /// payload — see [`ReconManager::run_startup_pass`]). `false` (default) is byte-identical to
    /// before this flag: `reconcile_reports` takes the legacy silent seed. `true` ⇒ venue cash is
    /// diffed against the realized-PnL-corrected local balance and any drift routed through
    /// `policy` (quarantined by default — a surprise cash move is never auto-folded).
    pub reconcile_balance: bool,
    /// Feature 2 money tolerance for the first-class cash reconcile diff — the abs/rel bands
    /// `vike_exec::recon::diff_balance` compares against (only consulted when `reconcile_balance` is
    /// `true`). Env-tuned per deployment via `VIKE_RECONCILE_BALANCE_TOL_{ABS,REL}` (see
    /// `crates/vike-tradehub/src/reconcile_config.rs`'s `build_recon_config`);
    /// [`BalanceTol::default`] (the conservative constant) when unset. Threaded verbatim into each
    /// pass's [`vike_exec::ReconcileReports`] alongside `reconcile_balance` (the fold thread reads
    /// it only through that payload — see [`ReconManager::run_startup_pass`]).
    pub balance_tol: BalanceTol,
}

impl fmt::Debug for ReconConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReconConfig")
            .field("policy", &self.policy)
            .field("lookback_ms", &self.lookback_ms)
            .field("startup_delay", &self.startup_delay)
            .field("health", &self.health.as_ref().map(|_| "<fn>"))
            .field("interval", &self.interval)
            .field("generate_missing_orders", &self.generate_missing_orders)
            .field("audit_interval", &self.audit_interval)
            .field("reconcile_balance", &self.reconcile_balance)
            .field("balance_tol", &self.balance_tol)
            .finish()
    }
}

impl Default for ReconConfig {
    fn default() -> Self {
        ReconConfig {
            policy: ReconPolicy::default(),
            lookback_ms: 24 * 60 * 60 * 1_000, // one day
            startup_delay: Duration::from_secs(0),
            health: None,
            interval: None,
            generate_missing_orders: false,
            audit_interval: None,
            reconcile_balance: false,
            balance_tol: BalanceTol::default(),
        }
    }
}

#[cfg(test)]
mod staleness_escalation_tests;

#[cfg(test)]
use liveness::{
    STALENESS_CEILING, STALENESS_FLOOR, SUPPRESSED_REASON, VenueLiveness, staleness_threshold,
};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use vike_exec::Ingest;
#[cfg(test)]
use vike_exec::recon::ReconClient;
