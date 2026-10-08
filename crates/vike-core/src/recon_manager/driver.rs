//! The `vt-core-recon` driver thread: spawn, the trigger/interval/audit select loop, and its handle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::liveness::VenueLiveness;
use super::{ReconConfig, ReconLeg, ReconManager};
use crate::runtime::CoreHandle;

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
/// clones to venue bridges at spawn time. This matters because the composition root builds every
/// live venue's exec client (and its `run_resync_supervisor` call, deep inside) well before
/// `spawn_core_multi`/`spawn_recon` run — `crates/vike-mount/src/node/build.rs`'s `build_node` executes
/// `make_engine` for every venue and returns the pre-created pair in `Node::recon_trigger`, which
/// `vike-tradehub` hands to this function. (⚠ This cited `vike-app/src/main.rs`'s `App::new`
/// until 2026-09-28: that assembly moved into `build_node`, and the desktop has mounted no venue
/// since #1727.) `Some((tx, rx))` ADOPTS the given pair verbatim: `rx` drives this driver's
/// trigger-poll loop below and `tx` seeds `ReconDriver.trigger`, so
/// [`ReconDriver::reconcile_trigger`] keeps handing out clones of the SAME channel the caller
/// already threaded into venues, rather than a second, disconnected one.
/// `None` (every pre-Task-7 call site) reproduces the original behavior byte-for-byte: this
/// function mints its own private pair, exactly as before.
pub fn spawn_recon(
    handle: &CoreHandle,
    clients: Vec<ReconLeg>,
    config: ReconConfig,
    trigger: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> ReconDriver {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let (trigger_tx, trigger_rx) = trigger.unwrap_or_else(mpsc::channel);
    // WEAK like spawn_periodic_reconcile: must NOT keep the ingest channel open, or the core's
    // "every strong sender dropped -> clean break" exit would never fire.
    let health = config.health.clone();
    // `last_pass_ms: None` — the staleness baseline is seeded by the FIRST pass, not here; see
    // `VenueLiveness::last_pass_ms`. `last_balance_ms: None` is stronger than a baseline: it means
    // NO balance claim is made for this venue until it has actually returned one, so a venue that
    // reports no balance at all can never go balance-stale.
    let liveness = clients
        .iter()
        .map(|leg| VenueLiveness {
            venue: leg.venue.clone(),
            // The LEG key, so a two-account box's escalation lines name which account went
            // stale. Equal to `venue` on every single-account box, where it renders nothing extra.
            leg_key: leg.leg_key().to_string(),
            last_pass_ms: None,
            last_balance_ms: None,
            degraded: None,
            last_escalation_ms: 0,
        })
        .collect();
    // `mut` because the pass now carries per-venue liveness state. Verified safe without interior
    // mutability: this value is constructed ONCE and moved into the driver thread below, so it has
    // exactly one owner for its whole life.
    let mut mgr =
        ReconManager { clients, config, ingest: handle.ingest.downgrade(), health, liveness };
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
