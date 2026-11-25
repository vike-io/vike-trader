//! `ReconcileManager` — the reconciliation runtime driver that ties the pure reconcile engine
//! (`vike_execution::recon`) into the live single-writer core. Phase-3 startup-cadence wiring.
//!
//! **Design (Option A — reports-in, compute-on-fold).** `recon::diff`/`recon::resolve` need the
//! CURRENT local engine state (`ReconcileLocalView`), which lives inside the `Engine` on the core
//! FOLD thread. This manager runs on its OWN thread and does ONLY the blocking REST report fetch
//! (`ReconcileClient::fetch_*`) — it must never touch the p99<10µs fold. It then enqueues the raw
//! reports as [`vike_execution::Command::ReconcileReports`]; the fold thread reads its own
//! `local_view()`, runs the pure diff/resolve, and folds the synthesized events through the same
//! `on_event` path real venue events use (`runtime::CoreThread::reconcile_reports`). This keeps
//! diff/resolve on the same thread as the state they read — no cross-thread local-state snapshot,
//! no staleness window — and reuses the existing command choke point rather than adding a
//! query/reply lane.
//!
//! **Single-writer invariant.** This thread NEVER mutates engine/`Account` state: it fetches
//! (blocking I/O) and enqueues a `Command`. All state mutation stays on the fold thread. It holds
//! a WEAK ingest sender (a `downgrade()` of [`vike_core::CoreHandle::ingest`]) so it never
//! keeps the core alive — it self-exits when the core is gone or on [`ReconcileDriver::shutdown`].
//!
//! **Cadence.** A startup pass (one fetch+enqueue per venue after `startup_delay`), PLUS
//! (Task 13) an on-demand pass whenever the driver's reconcile-trigger channel is poked —
//! typically by a reconnect (a bridge's `run_user_resync` fires it after its own event-replay
//! settles, complementing rather than replacing that replay: see
//! `vike_bridge_core::websocket_session::user::run_user_resync`'s doc for the blind spots neither half
//! covers alone; the gatewayhub relays it to the account's lease as `ReconcileNow`). **A poke names
//! ONE account** (its route key), and the on-demand pass is the exact same fetch→diff/resolve over
//! that account's leg alone (the NOT READY retry's path, `ReconcileManager::run_retry_pass`), so one
//! account's reconnect never makes every account of the node refetch (the connection-sync plan,
//! 2026-11-21); the startup and interval passes still visit every leg.
//!
//! **Task 16 (continuous audits).** Two MORE timer arms on the same select loop, both optional
//! and both `None` by default (byte-identical to pre-Task-16 behavior):
//!   - [`ReconcileConfig::interval`]: re-runs [`ReconcileManager::run_startup_pass`] on a fixed cadence,
//!     in ADDITION to startup + trigger — same pass, same [`ReconcileManager::should_reconcile`]
//!     health gate, just fired more often. This is what makes a reconcile pass IDEMPOTENT under
//!     continuous cadence load-bearing rather than merely convenient: a venue trade_id already
//!     folded diffs to zero divergences (`vike_execution::recon::diff` dedups fills by trade_id
//!     against the local engine's `seen_trade_ids`), so a tick that finds nothing new folds
//!     nothing new (see `crates/vike-tradehub-core/tests/recon/reconcile_continuous_audit.rs`).
//!   - [`ReconcileConfig::audit_interval`]: a lighter DELEGATED tick — see
//!     [`ReconcileManager::run_audit_tick`] — that does NOT run a reconcile pass at all. It pokes the
//!     core's own pre-existing stuck-order watchdog waker (`Ingest::Watchdog`, already spawned by
//!     `runtime::spawn_core` whenever `CoreConfig::submit_ack_timeout` is set) rather than
//!     reimplementing any sweep logic here.
//!
//! The internal-book-vs-venue-book audit (comparing local positions against venue-reported
//! positions) is explicitly OUT of scope for a separate mechanism: [`ReconcileManager::run_startup_pass`]
//! already diffs `PositionStatusReport`s against local state on every pass (`Divergence::
//! PositionDrift`/`ExternalOnlyPosition` in `vike_execution::recon::diff`) — the interval cadence above
//! is what turns that existing diff into a continuous audit, no new plumbing needed.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use vike_execution::recon::{ReconcileBalanceTolerance, ReconcilePolicy};

mod driver;
mod first_balance;
mod liveness;
mod manager;

pub(crate) use driver::spawn_reconcile_on;
pub use driver::{ReconcileDriver, spawn_reconcile};
pub use first_balance::{
    FIRST_BALANCE_BOUND, ReconcileFirstBalance, await_first_balances, wait_for_first_balances,
};
pub use manager::{ReconcileLeg, ReconcileLegChange, ReconcileManager};

/// Neutral outage classification the reconcile driver gates on. Deliberately NOT
/// `vike_bridge_core::stream_health::StreamHealth` — `vike-core` depends on
/// `vike-execution` + `vike-model` only (down-only layering) and must not pull in `vike-bridge-core`.
/// This enum is the same generic-boundary trick [`ReconcileDriver::reconcile_trigger`] (Task 13) used
/// for its trigger channel: the app root, which DOES see both crates, is the one place that maps
/// the real bridge health signal onto this closure (see [`ReconcileConfig::health`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileHealth {
    /// Local network + venue feed both look connected — reconcile passes run normally.
    Healthy,
    /// Local network down OR a venue feed is mid-gap (resyncing) — reconcile passes are
    /// suppressed for this poll/poke so a blip doesn't fire a reconcile storm against reports
    /// the venue itself hasn't caught up on yet.
    Degraded,
}

/// A per-venue reconcile health probe (`venue -> ReconcileHealth`), wired by the app root to each
/// venue's real feed health. Factored into an alias so the `ReconcileConfig`/`ReconcileManager` `health`
/// fields don't trip `clippy::type_complexity`.
pub type ReconcileHealthProbe = Arc<dyn Fn(&str) -> ReconcileHealth + Send + Sync>;

/// Reconcile driver configuration. `policy`, `lookback_ms`, `startup_delay`, `health`, `interval`,
/// `audit_interval`, and `generate_missing_orders` are all live.
pub struct ReconcileConfig {
    /// Fold-vs-quarantine policy per divergence kind (default = Synthesize everything).
    pub policy: ReconcilePolicy,
    /// How far back (ms) to request order/fill reports at pass time (`since = now - lookback_ms`).
    pub lookback_ms: i64,
    /// Delay before the startup pass runs (0 = immediately).
    pub startup_delay: Duration,
    /// Task 15: optional PER-VENUE health probe consulted before each venue's leg of a pass. The
    /// `&str` is the venue being reconciled — so a bybit-disconnected moment suppresses only bybit's
    /// leg while binance's still runs (the earlier binance-only gate let an unhealthy venue reconcile
    /// as long as binance was up). `None` = always [`ReconcileHealth::Healthy`] — byte-identical to
    /// pre-Task-15 behavior. Wired by the app root to each venue's real feed health (`StreamHealth`),
    /// which `vike-core` cannot name directly (see [`ReconcileHealth`]'s doc); an
    /// unknown venue should map to `Healthy` so an un-gated venue is never blocked.
    pub health: Option<ReconcileHealthProbe>,
    /// Task 16: continuous re-reconcile cadence. `Some(d)` re-runs the SAME
    /// [`ReconcileManager::run_startup_pass`] every `d`, on top of startup + trigger passes. `None`
    /// (default) = no interval timer arm at all — byte-identical to pre-Task-16 behavior.
    pub interval: Option<Duration>,
    /// Synthesize adoption events for a venue order with no local match
    /// (`vike_model::ReconcileDivergenceKind::UnknownOrder`), instead of `resolve`'s default empty
    /// catch-all for that kind. Threaded verbatim into each pass's [`vike_execution::ReconcileReports`]
    /// (the fold thread reads `ReconcileConfig` via that payload, not this struct directly — see
    /// `ReconcileManager::run_startup_pass`). `false` (default) is byte-identical to before this flag
    /// existed: under `hybrid`, `UnknownOrder` still quarantines with EMPTY proposed events, so an
    /// operator confirm has nothing to adopt.
    ///
    /// `true` enables the NARROW adoption semantics of `vike_execution::recon::resolve`'s
    /// `ReconcileAdoptContext` (its module doc is the authority): only a TERMINAL unknown order with
    /// executed qty and no fill report in the same pass synthesizes anything that folds (a
    /// decorative accept + one cumulative `Fill`, deterministic `EXT-ORD-*` trade_id); a live or
    /// fill-lane-covered unknown order folds NOTHING (its executions arrive as `MissingFill`
    /// divergences with real venue trade-ids), surfacing only a dedup-keyed held alert under
    /// `hybrid`/`quarantine` for operator visibility/adoption. Once adopted, recurring passes are
    /// a true no-op (no events, no counter drift, no new alert rows).
    pub generate_missing_orders: bool,
    /// Task 16: periodic in-flight-timeout audit cadence. `Some(d)` pokes the core's existing
    /// stuck-order watchdog waker every `d` (see [`ReconcileManager::run_audit_tick`] — DELEGATED, not
    /// a reimplemented sweep). `None` (default) = no audit timer arm.
    pub audit_interval: Option<Duration>,
    /// Feature 2 (`VIKE_RECONCILE_BALANCE`): promote venue balance from a silent authoritative
    /// overwrite to a first-class DIFFED dimension. Threaded verbatim into each pass's
    /// [`vike_execution::ReconcileReports`] (the fold thread reads `ReconcileConfig` only through that
    /// payload — see [`ReconcileManager::run_startup_pass`]). `false` (default) is byte-identical to
    /// before this flag: `reconcile_reports` takes the legacy silent seed. `true` ⇒ venue cash is
    /// diffed against the realized-PnL-corrected local balance and any drift routed through
    /// `policy` (quarantined by default — a surprise cash move is never auto-folded).
    pub reconcile_balance: bool,
    /// Feature 2 money tolerance for the first-class cash reconcile diff — the abs/rel bands
    /// `vike_execution::recon::diff_balance` compares against (only consulted when `reconcile_balance` is
    /// `true`). Env-tuned per deployment via `VIKE_RECONCILE_BALANCE_TOL_{ABS,REL}` (see
    /// `crates/vike-tradehub/src/reconcile_config.rs`'s `build_reconcile_config`);
    /// [`ReconcileBalanceTolerance::default`] (the conservative constant) when unset. Threaded verbatim into each
    /// pass's [`vike_execution::ReconcileReports`] alongside `reconcile_balance` (the fold thread reads
    /// it only through that payload — see [`ReconcileManager::run_startup_pass`]).
    pub balance_tol: ReconcileBalanceTolerance,
    /// **How soon a pass that skipped a leg as NOT READY is tried again** (W13b finding 6): the next
    /// interval tick is 60 s by default, and the first pass of a start has to land inside the 120 s window
    /// in which a restored order mutes its strategy's tagged resubmit, so two skipped passes would lose it.
    /// The driver repeats the pass on this period while any leg is NOT READY, for at most
    /// `NOT_READY_RETRY_FOR` (60 s) from the first skip, then falls back to the interval alone. Nothing changes
    /// for a venue that is never NOT READY (every in-process client).
    pub not_ready_retry: Duration,
}

/// The longest a streak of NOT READY passes is retried at `ReconcileConfig::not_ready_retry`'s period: half
/// the restored-tag window (`vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS`, 120 s), so the
/// retries end while a late success could still beat it.
pub(crate) const NOT_READY_RETRY_FOR: Duration = Duration::from_secs(60);

impl fmt::Debug for ReconcileConfig {
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
            .field("not_ready_retry", &self.not_ready_retry)
            .finish()
    }
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        ReconcileConfig {
            policy: ReconcilePolicy::default(),
            lookback_ms: vike_model::MS_PER_DAY,
            startup_delay: Duration::from_secs(0),
            health: None,
            interval: None,
            generate_missing_orders: false,
            audit_interval: None,
            reconcile_balance: false,
            balance_tol: ReconcileBalanceTolerance::default(),
            not_ready_retry: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod first_balance_tests;
#[cfg(test)]
mod leg_change_tests;
#[cfg(test)]
mod staleness_escalation_tests;

#[cfg(test)]
use liveness::{ReconcileLegLiveness, staleness_threshold};
#[cfg(test)]
use liveness::{STALENESS_CEILING, STALENESS_FLOOR, SUPPRESSED_REASON};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use vike_execution::Ingest;
#[cfg(test)]
use vike_execution::recon::ReconcileClient;
