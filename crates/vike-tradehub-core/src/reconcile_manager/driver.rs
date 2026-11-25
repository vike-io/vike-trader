//! The `th-core-recon` driver thread: spawn, the trigger/interval/audit select loop, and its handle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::liveness::ReconcileLegLiveness;
use super::{
    NOT_READY_RETRY_FOR, ReconcileConfig, ReconcileLeg, ReconcileLegChange, ReconcileManager,
};
use vike_core::CoreHandle;

/// Spawn the reconcile runtime driver. After `config.startup_delay`, runs ONE startup pass
/// (fetch + enqueue per venue) on a dedicated `th-core-recon` thread; the thread then stays alive,
/// selecting between the stop flag, the Task-13 reconcile-trigger channel (`ReconcileDriver::trigger`/
/// [`ReconcileDriver::reconcile_trigger`]), and (Task 16) the `interval`/`audit_interval` timer arms
/// below. The interval cadence re-runs the SAME startup pass over every leg; **an external poke
/// (typically a venue reconnect) names ONE account** (its route key, the `ReconcileLeg::leg_key` of
/// its leg) and runs that leg alone (`ReconcileManager::run_retry_pass`'s path), so a reconnect of one
/// account no longer makes every account of the node refetch. A burst of pokes is coalesced into one
/// pass over the legs they named; a poke naming no leg runs every leg, as every poke used to. It
/// self-exits either on [`ReconcileDriver::shutdown`] or once the core is gone
/// (the weak ingest sender fails to upgrade) — checked on every poll tick, not only inside a pass,
/// so an un-shutdown driver never leaks a thread past the core's lifetime. Holds a WEAK ingest
/// sender throughout, so it never keeps the core alive.
///
/// **Reconciliation-activation Task 7: adopting an external trigger channel.** `trigger` lets a
/// caller pre-create the `on_reconcile` `mpsc` pair BEFORE this driver exists and hand `Sender`
/// clones to venue bridges at spawn time. This matters because the composition root builds every
/// live venue's exec client (and its `run_user_resync` call, deep inside) well before
/// `spawn_core_multi`/`spawn_reconcile` run — `crates/vike-tradehub-core/src/node/build.rs`'s `assemble_runtime` executes
/// `make_engine` for every venue and returns the pre-created pair in `RuntimeAssembly::reconcile_trigger`, which
/// `vike-tradehub` hands to this function. (⚠ This cited `vike-app/src/main.rs`'s `App::new`
/// until 2026-09-28: that assembly moved into `assemble_runtime`, and the desktop has mounted no venue
/// since #1727.) `Some((tx, rx))` ADOPTS the given pair verbatim: `rx` drives this driver's
/// trigger-poll loop below and `tx` seeds `ReconcileDriver.trigger`, so
/// [`ReconcileDriver::reconcile_trigger`] keeps handing out clones of the SAME channel the caller
/// already threaded into venues, rather than a second, disconnected one.
/// `None` (every pre-Task-7 call site) reproduces the original behavior byte-for-byte: this
/// function mints its own private pair, exactly as before.
pub fn spawn_reconcile(
    handle: &CoreHandle,
    clients: Vec<ReconcileLeg>,
    config: ReconcileConfig,
    trigger: Option<(mpsc::Sender<String>, mpsc::Receiver<String>)>,
) -> ReconcileDriver {
    spawn_reconcile_on(&handle.ingest, clients, config, trigger)
}

/// [`spawn_reconcile`] over the core's ingest lane itself, for a driver started by whoever holds only that lane (a
/// RUNNING node starting reconciliation for the first account that wants it, decision 0169): the driver keeps
/// a WEAK sender either way.
pub(crate) fn spawn_reconcile_on(
    ingest: &tokio::sync::mpsc::Sender<vike_execution::Ingest>,
    clients: Vec<ReconcileLeg>,
    config: ReconcileConfig,
    trigger: Option<(mpsc::Sender<String>, mpsc::Receiver<String>)>,
) -> ReconcileDriver {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let (trigger_tx, trigger_rx) = trigger.unwrap_or_else(mpsc::channel);
    // The mailbox a RUNNING driver is given more legs through (decision 0169): drained on the poll tick and
    // before every pass, by the driver thread alone, so `clients` is still touched by one thread only.
    let (legs_tx, legs_rx) = mpsc::channel::<ReconcileLegChange>();
    // WEAK: must NOT keep the ingest channel open, or the core's
    // "every strong sender dropped -> clean break" exit would never fire.
    let health = config.health.clone();
    // `last_pass_ms: None` — the staleness baseline is seeded by the FIRST pass, not here; see
    // `ReconcileLegLiveness::last_pass_ms`. `last_balance_ms: None` is stronger than a baseline: it means
    // NO balance claim is made for this venue until it has actually returned one, so a venue that
    // reports no balance at all can never go balance-stale.
    let liveness = clients.iter().map(ReconcileLegLiveness::for_leg).collect();
    // `mut` because the pass now carries per-venue liveness state. Verified safe without interior
    // mutability: this value is constructed ONCE and moved into the driver thread below, so it has
    // exactly one owner for its whole life.
    let mut manager =
        ReconcileManager { clients, config, ingest: ingest.downgrade(), health, liveness };
    let thread = std::thread::Builder::new()
        .name("th-core-recon".into())
        .spawn(move || {
            if sleep_watching(&stop_t, manager.config.startup_delay) {
                return; // stopped (or core gone) during the startup delay
            }
            let mut retry = NotReadyRetry::new(manager.config.not_ready_retry);
            drain_leg_changes(&mut manager, &legs_rx);
            retry.after_pass(manager.run_startup_pass(&stop_t));
            // Task 13: react to on-demand pokes (a venue reconnect, typically) with the SAME pass —
            // no separate logic. Task 16: TWO more timer arms on this same select loop, both `None`
            // by default (each `next_*` stays `None`, so `wait` below is always exactly
            // `select_poll` and every branch below is a no-op for that arm — byte-identical to
            // pre-Task-16 behavior when neither `interval` nor `audit_interval` is configured).
            // Deadlines reschedule relative to `Instant::now()` at each fire (not the previous
            // deadline), so a late tick (thread briefly descheduled) resumes the cadence from
            // "now" instead of bursting catch-up passes.
            let select_poll = Duration::from_millis(50);
            let mut next_interval = manager.config.interval.map(|d| Instant::now() + d);
            let mut next_audit = manager.config.audit_interval.map(|d| Instant::now() + d);
            while !stop_t.load(Ordering::Relaxed) {
                drain_leg_changes(&mut manager, &legs_rx);
                let now = Instant::now();
                let mut wait = select_poll;
                if let Some(deadline) = next_interval {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                if let Some(deadline) = next_audit {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                match trigger_rx.recv_timeout(wait) {
                    Ok(first) => {
                        // Coalesce a burst of pokes (e.g. several accounts reconnecting together)
                        // into one pass over the legs they name rather than one per poke.
                        let poked: Vec<String> =
                            std::iter::once(first).chain(trigger_rx.try_iter()).collect();
                        drain_leg_changes(&mut manager, &legs_rx);
                        let legs = manager.legs_of(&poked);
                        if legs.is_empty() {
                            retry.after_pass(manager.run_startup_pass(&stop_t));
                        } else {
                            let not_ready = manager.run_retry_pass(&stop_t, &legs);
                            retry.after_legs(&legs, not_ready);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if manager.ingest.upgrade().is_none() {
                            return; // core gone — self-exit like the startup-pass path
                        }
                        let now = Instant::now();
                        if let (Some(deadline), Some(d)) = (next_interval, manager.config.interval)
                            && now >= deadline
                        {
                            retry.after_pass(manager.run_startup_pass(&stop_t));
                            next_interval = Some(now + d);
                        } else if retry.due(now) {
                            // A leg was NOT READY last pass (link down, prefetch owed, feed mid-gap):
                            // ask again soon, not at the next interval tick - THAT leg only, so a stuck
                            // one does not make the healthy ones refetch every period (W13c).
                            let legs = retry.legs().to_vec();
                            retry.after_pass(manager.run_retry_pass(&stop_t, &legs));
                        }
                        if let (Some(deadline), Some(d)) =
                            (next_audit, manager.config.audit_interval)
                            && now >= deadline
                        {
                            manager.run_audit_tick();
                            next_audit = Some(now + d);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return, // every trigger handle dropped
                }
            }
        })
        .expect("spawn th-core recon driver");
    ReconcileDriver { stop, trigger: trigger_tx, legs: legs_tx, handle: thread }
}

/// Apply every leg change waiting in the mailbox. Not a clock read, not a fetch: the driver thread only.
fn drain_leg_changes(manager: &mut ReconcileManager, rx: &mpsc::Receiver<ReconcileLegChange>) {
    while let Ok(change) = rx.try_recv() {
        manager.apply_leg_change(change);
    }
}

/// The retry clock of a pass that skipped a leg as NOT READY ([`ReconcileConfig::not_ready_retry`]): the next
/// try (of the legs that were NOT READY, no other) one period after the pass, until
/// [`NOT_READY_RETRY_FOR`] after the FIRST skip of the streak, and cleared by the first pass that skipped
/// nothing. A zero period retries nothing.
struct NotReadyRetry {
    period: Duration,
    next: Option<Instant>,
    give_up_at: Option<Instant>,
    /// The legs (indices into the manager's clients) the last pass found NOT READY.
    legs: Vec<usize>,
}

impl NotReadyRetry {
    fn new(period: Duration) -> Self {
        Self { period, next: None, give_up_at: None, legs: Vec::new() }
    }

    /// Record how the pass that just ran went (`not_ready`: the legs it found NOT READY).
    fn after_pass(&mut self, not_ready: Vec<usize>) {
        if not_ready.is_empty() || self.period.is_zero() {
            self.next = None;
            self.give_up_at = None;
            self.legs.clear();
            return;
        }
        self.legs = not_ready;
        let now = Instant::now();
        let give_up = *self.give_up_at.get_or_insert(now + NOT_READY_RETRY_FOR);
        self.next = (now < give_up).then(|| now + self.period);
    }

    /// Record a pass over `visited` alone (a poke's): the legs it found NOT READY join the ones still owed a retry
    /// that it did not visit, so one account's pass never cancels another's retry.
    fn after_legs(&mut self, visited: &[usize], not_ready: Vec<usize>) {
        let mut legs: Vec<usize> =
            self.legs.iter().copied().filter(|leg| !visited.contains(leg)).collect();
        legs.extend(not_ready);
        legs.sort_unstable();
        legs.dedup();
        self.after_pass(legs);
    }

    /// The legs the next retry runs.
    fn legs(&self) -> &[usize] {
        &self.legs
    }

    /// Whether the retry is due at `now`.
    fn due(&self, now: Instant) -> bool {
        self.next.is_some_and(|at| now >= at)
    }
}

/// Sleep for `duration`, polling `stop` in short slices so shutdown is observed promptly. Returns true
/// if `stop` was raised (caller should exit). A zero duration returns immediately.
fn sleep_watching(stop: &AtomicBool, duration: Duration) -> bool {
    if duration.is_zero() {
        return stop.load(Ordering::Relaxed);
    }
    let poll = duration.min(Duration::from_millis(50)).max(Duration::from_millis(1));
    let deadline = Instant::now() + duration;
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

/// Join half of the reconcile driver: raise stop, join the thread. The driver self-exits once the core is gone (checked on every trigger-poll tick,
/// not only inside a pass), so an explicit shutdown is optional in that case — dropping the driver
/// is safe. Absent that, the driver thread stays alive (Task 13: it is listening for reconcile
/// pokes), so a live core's driver should be shut down explicitly when the venue bridges it serves
/// are torn down.
pub struct ReconcileDriver {
    stop: Arc<AtomicBool>,
    /// Task 13: the reconcile-trigger sender. Cloned out via [`ReconcileDriver::reconcile_trigger`] and
    /// handed to a venue bridge's `run_user_resync(..., on_reconcile)` so a reconnect can poke
    /// this driver into an on-demand pass. Kept here (not just inside the thread) so the channel
    /// stays open — and thus the trigger loop stays alive — for as long as the `ReconcileDriver` itself
    /// does; dropping the driver without cloning out a sender lets `Disconnected` end the thread.
    /// Task 7 (reconciliation-activation): this is `trigger_tx` from [`spawn_reconcile`]'s `trigger`
    /// param when the caller adopted an external pair (main.rs's usual case, so venues already
    /// hold clones of THIS exact sender before the driver ever existed), or a freshly minted one
    /// when `trigger: None` (every pre-Task-7 caller, e.g. this crate's own tests). A poke is the
    /// route key of the account whose leg to reconcile.
    trigger: mpsc::Sender<String>,
    /// The mailbox of [`ReconcileLegChange`]s a running driver applies on its own thread (decision 0169): the
    /// node's way to give it the reconcile leg of an account started after the driver was.
    legs: mpsc::Sender<ReconcileLegChange>,
    handle: std::thread::JoinHandle<()>,
}

impl ReconcileDriver {
    /// A clonable handle for every lease of the node: `driver.reconcile_trigger()` (all accounts share one manager
    /// thread). A poke sends the route key of the account to reconcile (`ReconcileLeg::leg_key`); a key that names no
    /// leg (an empty one) asks for a pass over every leg. Sending never blocks (unbounded channel) and a poke after the
    /// driver has shut down is silently dropped.
    pub fn reconcile_trigger(&self) -> mpsc::Sender<String> {
        self.trigger.clone()
    }

    /// A clonable handle to the driver's leg mailbox ([`ReconcileLegChange`]). A send after the driver ended is
    /// an `Err` the caller may ignore.
    pub fn leg_sender(&self) -> mpsc::Sender<ReconcileLegChange> {
        self.legs.clone()
    }

    /// Supervisor probe: true once the driver thread has exited (core gone, or after `shutdown`).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop the driver thread and join it. Idempotent with the driver's own self-exit.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        // A `join` Err is a driver panic its hook already reported; nothing to recover at teardown.
        let _ = self.handle.join();
    }

    /// Raise the stop flag and return at once: no new pass or leg starts, a fetch already in flight runs
    /// on. The first half of a teardown whose second half is [`ReconcileDriver::shutdown_within`] AFTER the
    /// core has stopped, so a venue call that blocks for seconds cannot hold the core's own teardown
    /// (its `cancel_orders_on_shutdown` sweep) behind it.
    pub fn begin_shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// [`ReconcileDriver::shutdown`] with a bound: raise the stop flag, then wait up to `bound` for the thread
    /// to end. `true` = it ended (and was joined). `false` = it is still inside a venue call at the
    /// bound and is abandoned: it holds only a weak ingest sender and exits as soon as the call
    /// returns, so nothing keeps the process (or the core) alive on its account.
    pub fn shutdown_within(self, bound: Duration) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        // A `join` Err is a driver panic its hook already reported; nothing to recover at teardown.
        vike_utility::thread::join_within(self.handle, bound).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One account's pass keeps the retry another leg is owed: the legs it visited leave the list, the ones it
    /// found NOT READY join it, and the rest stay.
    #[test]
    fn a_poked_pass_keeps_the_retry_of_the_legs_it_did_not_visit() {
        let mut retry = NotReadyRetry::new(Duration::from_secs(5));
        retry.after_pass(vec![1, 3]);
        retry.after_legs(&[0, 1], Vec::new());
        assert_eq!(retry.legs(), [3], "leg 1 was visited and ready; leg 3 is still owed");
        retry.after_legs(&[2], vec![2]);
        assert_eq!(retry.legs(), [2, 3]);
        retry.after_legs(&[2, 3], Vec::new());
        assert!(retry.legs().is_empty() && retry.next.is_none(), "nothing owed: no retry armed");
    }
}
