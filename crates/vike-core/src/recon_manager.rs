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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::WeakSender;

use vike_exec::recon::{BalanceTol, MassStatus, ReconClient, ReconPolicy};
use vike_exec::{Command, Ingest, ReconcileReports};

use crate::runtime::CoreHandle;

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
    /// `vike-app`'s `reconcile_config::build_recon_config`); [`BalanceTol::default`] (the
    /// conservative constant) when unset. Threaded verbatim into each pass's
    /// [`vike_exec::ReconcileReports`] alongside `reconcile_balance` (the fold thread reads it only
    /// through that payload — see [`ReconManager::run_startup_pass`]).
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

/// The reconcile manager: owns the per-venue [`ReconClient`]s + config + a WEAK ingest sender.
/// Lives on the driver thread; its only side effect is enqueuing [`Command::ReconcileReports`].
pub struct ReconManager {
    clients: Vec<(String, Box<dyn ReconClient>)>,
    config: ReconConfig,
    ingest: WeakSender<Ingest>,
    /// Task 15: mirrors `config.health` (hoisted onto the struct so [`ReconManager::should_reconcile`]
    /// doesn't need to reach through `config` — same shape as `ingest`/`clients`).
    health: Option<HealthProbe>,
}

impl ReconManager {
    /// Task 15 gate, consulted before EACH VENUE's leg of a pass (startup, trigger-driven, AND Task
    /// 16's interval-driven — see [`ReconManager::run_startup_pass`]). `true` (reconcile this venue)
    /// when no probe is wired (`health: None` — back-compat, byte-identical to pre-Task-15 behavior)
    /// or the probe reports [`ReconHealth::Healthy`] for `venue`; `false` (suppress this venue's leg)
    /// when it reports [`ReconHealth::Degraded`] (that venue's feed mid-gap, or local network down).
    /// Suppression DEFERS this venue's leg only — it sets no pending flag and never blocks a DIFFERENT
    /// healthy venue in the same pass, so a Degraded venue simply isn't reconciled until the next
    /// poke lands while it reads Healthy.
    fn should_reconcile(&self, venue: &str) -> bool {
        match &self.health {
            None => true,
            Some(probe) => probe(venue) == ReconHealth::Healthy,
        }
    }

    /// One reconcile pass: for each venue client, blocking-fetch the atomic mass-status bundle
    /// (orders + positions + fills via [`ReconClient::fetch_mass_status`], whose default composes
    /// the same three per-report fetches) and enqueue it for the fold thread to diff/resolve.
    /// Blocking REST runs HERE (never the fold). A fetch
    /// error skips that venue with a pass-boundary warning; a dropped core (weak upgrade / send
    /// fails) ends the pass. Called once at startup, again (Task 13) on every reconcile-trigger
    /// poke, and again (Task 16) on every [`ReconConfig::interval`] tick — always the SAME pass
    /// over ALL configured venues, not just the one that reconnected (simplest correct behavior; a
    /// per-venue-targeted pass is out of scope if ever needed). Idempotent by construction: a
    /// report the fold thread has already seen (fill trade_id already in `seen_trade_ids`, order
    /// already terminal, position already matching) diffs to zero divergences, so a redundant pass
    /// folds nothing new (see `tests/recon_continuous_audit.rs`). Task 15: gated PER VENUE by
    /// [`ReconManager::should_reconcile`] — a Degraded probe suppresses only THAT venue's leg (no
    /// fetch, no enqueue for it), so a healthy venue in the same pass still reconciles.
    fn run_startup_pass(&self, stop: &AtomicBool) {
        // Wall clock: reconcile `since` bounds are venue REST timestamps, so a wall clock is correct
        // here (unlike the core's injectable fold clock).
        let now_ms = vike_model::clock::now_ms();
        let since = (now_ms - self.config.lookback_ms).max(0);
        for (venue, client) in &self.clients {
            if stop.load(Ordering::Relaxed) || self.ingest.upgrade().is_none() {
                return;
            }
            // Per-venue health gate: skip only THIS venue's leg when its feed is Degraded; other
            // venues in the same pass are unaffected.
            if !self.should_reconcile(venue) {
                tracing::warn!(
                    target: "vike_core::reconcile",
                    venue = %venue,
                    "reconcile leg suppressed: venue health probe reports Degraded"
                );
                continue;
            }
            // Atomic mass-status snapshot (the Nautilus `ExecutionMassStatus` graft): one bundled
            // fetch of orders + positions + fills. The default `fetch_mass_status` composes the same
            // three per-report fetches in the same order (orders → fills → positions), so this is
            // byte-identical to the pre-seam per-report fetch for every current venue; a venue with a
            // real single-call snapshot endpoint may override it for cross-report consistency. A
            // fetch error still skips only THIS venue's leg with a pass-boundary warning, exactly as
            // the per-report fetches did.
            let MassStatus { orders, positions, fills } = match client.fetch_mass_status(since) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(target: "vike_core::reconcile", venue = %venue, "mass-status report fetch failed: {e}");
                    continue;
                }
            };
            // Task 3: the venue's authoritative cash. `None` (default trait impl / an un-wired
            // client / a fetch error) is swallowed here rather than skipping the whole pass — a
            // venue that doesn't report balance still gets its order/fill/position divergences
            // diffed and folded, it just leaves `Account` balance untouched.
            let balance = client.fetch_balance().unwrap_or(None);
            // Upgrade at the last moment: a core that exited mid-fetch ends the pass losslessly.
            let Some(tx) = self.ingest.upgrade() else { return };
            let reports = ReconcileReports {
                venue: venue.clone(),
                since,
                orders,
                fills,
                positions,
                policy: self.config.policy.clone(),
                balance,
                generate_missing_orders: self.config.generate_missing_orders,
                reconcile_balance: self.config.reconcile_balance,
                balance_tol: self.config.balance_tol,
                // `None` = "this venue's sole account", which is the truth for every client this
                // workspace can build: `self.clients` is keyed by CANONICAL venue (the same key
                // `should_reconcile` probes for feed health above, and the only key a per-venue
                // health map has), and `vike_run::build_node` mounts at most one `ReconClient` per
                // venue. A second account of one exchange would need its own client HERE, carrying
                // its own route key — that is the one edit that turns this into `Some`, and the
                // fold thread already knows what to do with it (`ReconcileReports::route`).
                route_key: None,
            };
            // Plain std thread (no tokio runtime) → blocking_send is legal and lossless.
            if tx
                .blocking_send(Ingest::Command(Command::ReconcileReports(Box::new(reports))))
                .is_err()
            {
                return; // core exited
            }
        }
    }

    /// Task 16 [`ReconConfig::audit_interval`] tick — DELEGATED, not a reimplemented sweep. This
    /// does NOT run a reconcile pass and does NOT itself decide anything is stuck: it enqueues
    /// [`Ingest::Watchdog`], the same message `runtime::spawn_core`'s own `vt-core-watchdog`
    /// thread already injects on a cadence whenever `CoreConfig::submit_ack_timeout` is
    /// configured. `Ingest::Watchdog` is a pure WAKER on the fold thread (`Ingest::Watchdog =>
    /// {}` in the dispatch match) — its only effect is making the core reach its drain-loop
    /// boundary sooner, where [`crate::runtime`]'s `DeadlineTimerWheel` runs the existing
    /// `sweep_stuck_orders` if-and-when it is actually due. So: with `submit_ack_timeout` unset
    /// (the default), this poke is inert — this manager has no visibility into `CoreConfig` (and
    /// must not; down-only layering) and does not need any to stay correct. Best-effort: a failed
    /// upgrade/send is silently dropped, same as any other poke on the weak ingest sender.
    fn run_audit_tick(&self) {
        if let Some(tx) = self.ingest.upgrade() {
            let _ = tx.blocking_send(Ingest::Watchdog);
        }
    }
}

/// Spawn the reconcile runtime driver. After `config.startup_delay`, runs ONE startup pass
/// (fetch + enqueue per venue) on a dedicated `vt-core-recon` thread; the thread then stays alive,
/// selecting between the stop flag, the Task-13 reconcile-trigger channel (`ReconDriver::trigger`/
/// [`ReconDriver::reconcile_trigger`]), and (Task 16) the `interval`/`audit_interval` timer arms
/// below, so an external poke (typically a venue reconnect) OR the interval cadence re-runs the
/// SAME startup pass. It self-exits either on [`ReconDriver::shutdown`] or once the core is gone
/// (the weak ingest sender fails to upgrade) — checked on every poll tick, not only inside a pass,
/// so an un-shutdown driver never leaks a thread past the core's lifetime. Holds a WEAK ingest
/// sender throughout, so it never keeps the core alive.
///
/// **Reconciliation-activation Task 7: adopting an external trigger channel.** `trigger` lets a
/// caller pre-create the `on_reconcile` `mpsc` pair BEFORE this driver exists and hand `Sender`
/// clones to venue bridges at spawn time. This matters because the app root builds every live
/// venue's exec client (and its `run_resync_supervisor` call, deep inside) well before
/// `spawn_core_multi`/`spawn_recon` run — see `vike-app/src/main.rs`'s `App::new`, where
/// `make_engine` executes long before this function is called. `Some((tx, rx))` ADOPTS the given
/// pair verbatim: `rx` drives this driver's trigger-poll loop below and `tx` seeds
/// `ReconDriver.trigger`, so [`ReconDriver::reconcile_trigger`] keeps handing out clones of the
/// SAME channel the caller already threaded into venues, rather than a second, disconnected one.
/// `None` (every pre-Task-7 call site) reproduces the original behavior byte-for-byte: this
/// function mints its own private pair, exactly as before.
pub fn spawn_recon(
    handle: &CoreHandle,
    clients: Vec<(String, Box<dyn ReconClient>)>,
    config: ReconConfig,
    trigger: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> ReconDriver {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let (trigger_tx, trigger_rx) = trigger.unwrap_or_else(mpsc::channel);
    // WEAK like spawn_periodic_reconcile: must NOT keep the ingest channel open, or the core's
    // "every strong sender dropped -> clean break" exit would never fire.
    let health = config.health.clone();
    let mgr = ReconManager { clients, config, ingest: handle.ingest.downgrade(), health };
    let thread = std::thread::Builder::new()
        .name("vt-core-recon".into())
        .spawn(move || {
            if sleep_watching(&stop_t, mgr.config.startup_delay) {
                return; // stopped (or core gone) during the startup delay
            }
            mgr.run_startup_pass(&stop_t);
            // Task 13: react to on-demand pokes (a venue reconnect, typically) with the SAME pass —
            // no separate logic. Task 16: TWO more timer arms on this same select loop, both `None`
            // by default (each `next_*` stays `None`, so `wait` below is always exactly
            // `select_poll` and every branch below is a no-op for that arm — byte-identical to
            // pre-Task-16 behavior when neither `interval` nor `audit_interval` is configured).
            // Deadlines reschedule relative to `Instant::now()` at each fire (not the previous
            // deadline), so a late tick (thread briefly descheduled) resumes the cadence from
            // "now" instead of bursting catch-up passes.
            let select_poll = Duration::from_millis(50);
            let mut next_interval = mgr.config.interval.map(|d| Instant::now() + d);
            let mut next_audit = mgr.config.audit_interval.map(|d| Instant::now() + d);
            while !stop_t.load(Ordering::Relaxed) {
                let now = Instant::now();
                let mut wait = select_poll;
                if let Some(deadline) = next_interval {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                if let Some(deadline) = next_audit {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                match trigger_rx.recv_timeout(wait) {
                    Ok(()) => {
                        // Coalesce a burst of pokes (e.g. several venues reconnecting together)
                        // into one pass rather than one per poke.
                        while trigger_rx.try_recv().is_ok() {}
                        mgr.run_startup_pass(&stop_t);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if mgr.ingest.upgrade().is_none() {
                            return; // core gone — self-exit like the startup-pass path
                        }
                        let now = Instant::now();
                        if let (Some(deadline), Some(d)) = (next_interval, mgr.config.interval)
                            && now >= deadline
                        {
                            mgr.run_startup_pass(&stop_t);
                            next_interval = Some(now + d);
                        }
                        if let (Some(deadline), Some(d)) = (next_audit, mgr.config.audit_interval)
                            && now >= deadline
                        {
                            mgr.run_audit_tick();
                            next_audit = Some(now + d);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return, // every trigger handle dropped
                }
            }
        })
        .expect("spawn vt-core recon driver");
    ReconDriver { stop, trigger: trigger_tx, handle: thread }
}

/// Sleep for `dur`, polling `stop` in short slices so shutdown is observed promptly. Returns true
/// if `stop` was raised (caller should exit). A zero duration returns immediately.
fn sleep_watching(stop: &AtomicBool, dur: Duration) -> bool {
    if dur.is_zero() {
        return stop.load(Ordering::Relaxed);
    }
    let poll = dur.min(Duration::from_millis(50)).max(Duration::from_millis(1));
    let deadline = Instant::now() + dur;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Join half of the reconcile driver. Mirrors [`crate::runtime::ReconcileDriver`]: raise stop, join
/// the thread. The driver self-exits once the core is gone (checked on every trigger-poll tick,
/// not only inside a pass), so an explicit shutdown is optional in that case — dropping the driver
/// is safe. Absent that, the driver thread stays alive (Task 13: it is listening for reconcile
/// pokes), so a live core's driver should be shut down explicitly when the venue bridges it serves
/// are torn down.
pub struct ReconDriver {
    stop: Arc<AtomicBool>,
    /// Task 13: the reconcile-trigger sender. Cloned out via [`ReconDriver::reconcile_trigger`] and
    /// handed to a venue bridge's `run_resync_supervisor(..., on_reconcile)` so a reconnect can poke
    /// this driver into an on-demand pass. Kept here (not just inside the thread) so the channel
    /// stays open — and thus the trigger loop stays alive — for as long as the `ReconDriver` itself
    /// does; dropping the driver without cloning out a sender lets `Disconnected` end the thread.
    /// Task 7 (reconciliation-activation): this is `trigger_tx` from [`spawn_recon`]'s `trigger`
    /// param when the caller adopted an external pair (main.rs's usual case, so venues already
    /// hold clones of THIS exact sender before the driver ever existed), or a freshly minted one
    /// when `trigger: None` (every pre-Task-7 caller, e.g. this crate's own tests).
    trigger: mpsc::Sender<()>,
    handle: std::thread::JoinHandle<()>,
}

impl ReconDriver {
    /// A clonable handle a venue bridge can pass as `run_resync_supervisor`'s `on_reconcile`
    /// argument: `driver.reconcile_trigger()` per bridge (all venues share one manager thread and
    /// one pass — see the module doc). Sending never blocks (unbounded channel) and a poke after
    /// the driver has shut down is silently dropped.
    pub fn reconcile_trigger(&self) -> mpsc::Sender<()> {
        self.trigger.clone()
    }

    /// Supervisor probe: true once the driver thread has exited (core gone, or after `shutdown`).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop the driver thread and join it. Idempotent with the driver's own self-exit.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}
