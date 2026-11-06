//! [`QueuedSink`]: the bounded queue + one delivery thread that keeps a slow sink off the caller.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{AlertSink, FiredAlert};
#[cfg(doc)]
use super::{UreqTransport, WebhookSink, queued_ureq_webhook_sinks};

/// How deep a [`QueuedSink`]'s queue is when the caller does not choose
/// ([`queued_ureq_webhook_sinks`] passes it).
///
/// **Sized against the worst episode the producer can raise in one tick, not against a guess.** The
/// recorder dispatches ONE alert per silent series per tick
/// (`crates/vike-recorder/src/alerts.rs`'s `RecorderAlerts::on_silence`), gated per series rather
/// than per rule, and its series set is one entry per (symbol, stream) — it scales with a profile's
/// family glob, so a venue-wide outage raises the whole set at once. The arithmetic that produced
/// this fix used a hundred series (100 x 2 targets x 10 s ≈ 33 minutes parked); the the CI box profile
/// that motivated the watchdog subscribes far fewer, and no larger episode has been MEASURED — this
/// number is sized against the arithmetic, not against a tape. 128 swallows a hundred-series first
/// tick whole and still bounds the memory at a few hundred KiB of `FiredAlert`.
///
/// It is a BOUND, not a promise: past it alerts are dropped, counted and logged — see
/// [`QueuedSink`]'s overflow policy.
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

/// How long a delivery worker waits for its next alert before re-reading its stop flag. It is the
/// CEILING on how long [`QueuedSinkStop::shutdown`] takes when the worker is IDLE — not
/// microseconds: an idle worker is parked inside `recv_timeout` and re-reads the flag only when
/// that wait returns, so a stop raised while it sleeps costs up to one full poll, and an owner that
/// stops several targets one after another pays up to one poll EACH. Small for that reason; an idle
/// worker costs one wakeup per tenth of a second, which is nothing beside the thread.
///
/// Public so an owner can size its stop budget from this constant rather than from a restated
/// number; `crates/vike-recorder/src/alerts.rs`'s `stop_delivery` is the one that does.
pub const STOP_POLL: Duration = Duration::from_millis(100);

/// How often [`QueuedSinkStop::shutdown`] re-asks whether the worker has finished. `JoinHandle`
/// has no join-with-timeout, so the wait is a poll — see that method for why it must be bounded.
const JOIN_POLL: Duration = Duration::from_millis(5);

/// **A bounded queue and one delivery thread in front of a slow sink** — the decorator that makes
/// the [`AlertSink`] contract's "must NEVER block the caller for long" true of a sink that POSTs.
///
/// ## The defect this exists to remove
///
/// `AlertEngine::dispatch` is a nested SERIAL loop — every fired alert, times every sink, inline on
/// the caller's thread — and [`WebhookSink`]'s `deliver` performs a blocking HTTP POST whose only
/// bound is [`UreqTransport`]'s 10 s global timeout. On the recorder that caller is the single
/// `loop { rt.tick(..) … }` in `crates/vike-datahub/src/recorder.rs`'s `run` — the same loop
/// that polls the stop flag — and its producer raises one alert per SILENT SERIES, so the first
/// tick of a venue outage makes every silent series due at once: 100 series x 2 webhook targets x
/// 10 s is ~2000 s, i.e. **~33 minutes parked inside alert delivery, observing no stop flag**. A
/// `systemctl stop` in that window reaches its `TimeoutStopSec=` and SIGKILL lands on the writer.
///
/// ## Why a BOUND, and why not the other two fixes
///
/// The root cause is a missing bound, not missing parallelism, and the three candidates were
/// weighed in that light:
///
/// * **A cap on dispatches per tick** bounds the loop but not the STALL — one alert x one dead
///   endpoint is still 10 s on the caller's thread, and the deferred remainder needs a queue to be
///   deferred INTO, which is this type with worse ergonomics.
/// * **Coalescing N silent series into one incident message** is a real improvement for the human
///   on the pager, and it is NOT this crate's decision to make: the per-series repeat gate lives in
///   `crates/vike-recorder/src/liveness.rs`'s `SilenceWatch::alertable`, the prefix scope lives in
///   the RULE, and `crates/vike-recorder/src/alerts.rs`'s module doc argues at length that six
///   silent series must read as six rather than as one. It also does not bound anything — one alert
///   x two targets x 10 s is 20 s of a stop flag going unread, which is a smaller number, not a
///   bound.
/// * **This**: make `deliver` an enqueue. The caller's cost becomes a `try_send` (a lock, a move, a
///   wakeup) whatever the endpoint does, which is the property a stop flag needs. Coalescing
///   remains open ON TOP of it, upstream, where the incident is actually known.
///
/// ## The overflow policy — LOUD, and it drops the NEWEST
///
/// The queue is `sync_channel(capacity)` and `deliver` uses `try_send`, so a full queue drops the
/// arriving alert rather than blocking (blocking would hand the stall straight back). Every drop
/// increments [`dropped`](Self::dropped) and emits a `tracing::warn!` naming the target and the
/// rule — a dropped alert is a safety-relevant loss in a watchdog whose whole subject is a failure
/// that is silent by nature, so it may be dropped but must never be dropped QUIETLY.
///
/// Newest-first is the deliberate choice: within one incident the FIRST alerts are the ones that
/// page a human, and the queue fills only when the endpoint is already failing to keep up, i.e.
/// when the later alerts are same-incident repeats. Dropping the oldest instead would let a long
/// outage push the alert that STARTED it out of the queue unsent.
///
/// "Every drop is counted" is a property of the TYPE, not of the worker being polite about it: the
/// queue carries `Envelope`s (a private wrapper — the alert plus the drop counter it charges
/// itself to), and an envelope that is destroyed still holding its alert counts
/// itself. So the alerts a stopping worker leaves in the buffer are counted when the receiver is
/// dropped — including one a concurrent `deliver` slipped in AFTER the worker's last look at the
/// queue and BEFORE that drop, the window an explicit drain-then-drop would lose in silence. A
/// producer that arrives after the drop gets `Disconnected` and counts its own.
///
/// A [`QueuedSink`] never queues an alert its inner sink would discard: it consults
/// [`AlertSink::accepts`] first, so a Telegram target's queue is never filled with alerts routed
/// only to Discord.
///
/// ## What it deliberately does NOT do
///
/// * It does **not** retry. A failed POST is the inner sink's business and stays logged-not-retried;
///   a retry queue in front of a pager is how a five-minute outage becomes a thousand duplicate
///   pages an hour later.
/// * It does **not** deduplicate or coalesce. See above — that decision belongs upstream, where the
///   incident is known.
/// * It does **not** preserve ordering ACROSS sinks. Each wrapped sink has its own thread, on
///   purpose: a stalled Telegram must not delay a healthy Discord. Ordering WITHIN one sink is FIFO.
/// * It does **not** guarantee delivery of anything still queued at shutdown — see
///   [`QueuedSinkStop::shutdown`], which bounds the wait and counts what it abandons.
pub struct QueuedSink {
    /// the target name, for diagnostics only (drops, shutdown lines, the thread's name).
    name: String,
    /// kept beside the worker's own clone SOLELY to answer [`AlertSink::accepts`] on the caller's
    /// thread — this handle never delivers.
    inner: Arc<dyn AlertSink>,
    /// A bare `SyncSender`, no lock: `try_send` takes `&self` and the sender is `Sync`, so
    /// [`AlertSink::deliver`]'s `&self` sends directly; a `Mutex` here would be paid for on the
    /// caller's thread — the one thread this type exists to keep cheap.
    tx: SyncSender<Envelope>,
    dropped: Arc<AtomicU64>,
}

/// What actually travels through a [`QueuedSink`]'s channel: the alert plus the drop counter it
/// charges itself to if it is destroyed still holding the alert.
///
/// This is how "every drop is counted" survives the one path a worker cannot see: the receiver's
/// own `Drop` destroys whatever is buffered, and an alert can be buffered between the worker's
/// last `try_recv` and that drop (a concurrent `deliver` whose `try_send` succeeded into the
/// freed slot). A count taken by the worker would miss it; a count taken by the ENVELOPE cannot.
/// The worker calls [`Envelope::disarm`] after the inner sink returns — after, not before, so an
/// inner sink that breaks the never-panic contract drops its envelope during the unwind and the
/// alert it was holding is counted rather than vanishing with the thread.
struct Envelope {
    alert: Option<FiredAlert>,
    dropped: Arc<AtomicU64>,
}

impl Envelope {
    /// The alert, for delivery. `None` only after [`disarm`](Self::disarm), which the worker calls
    /// once and only once.
    fn alert(&self) -> Option<&FiredAlert> {
        self.alert.as_ref()
    }

    /// Delivered: this envelope no longer counts itself.
    fn disarm(&mut self) {
        self.alert = None;
    }
}

impl Drop for Envelope {
    fn drop(&mut self) {
        if self.alert.is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl QueuedSink {
    /// Wrap `inner` in a bounded queue served by one named thread, returning the sink to register
    /// and the [`QueuedSinkStop`] its owner must shut down.
    ///
    /// `capacity` is clamped UP to 1: `sync_channel(0)` is a rendezvous channel, where `try_send`
    /// succeeds only if the worker is already parked in `recv` — that would drop almost every alert
    /// while looking like a queue.
    ///
    /// # Panics
    /// If the OS refuses a thread. That is a process-is-doomed condition at mount time, and the
    /// alternative — returning a `Result` the caller degrades through — would need this type to
    /// hold a synchronous fallback, i.e. to keep the very code path the queue exists to remove.
    pub fn spawn(
        name: impl Into<String>,
        capacity: usize,
        inner: Box<dyn AlertSink>,
    ) -> (Self, QueuedSinkStop) {
        let name = name.into();
        let inner: Arc<dyn AlertSink> = Arc::from(inner);
        let (tx, rx) = sync_channel::<Envelope>(capacity.max(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));

        let worker = std::thread::Builder::new()
            .name(format!("vike-alert-{name}"))
            .spawn({
                let (worker_name, worker_inner) = (name.clone(), inner.clone());
                let (stopping, busy, delivered, dropped) =
                    (stopping.clone(), busy.clone(), delivered.clone(), dropped.clone());
                move || {
                    drain(&worker_name, worker_inner, rx, &stopping, &busy, &delivered, &dropped);
                }
            })
            .expect("spawning an alert delivery thread");

        let sink = QueuedSink { name: name.clone(), inner, tx, dropped: dropped.clone() };
        let stop = QueuedSinkStop { name, stopping, busy, worker, delivered, dropped };
        (sink, stop)
    }

    /// How many alerts this sink has DROPPED rather than handed to its worker (a full queue, or a
    /// worker that died). Never resets — it is a session total.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// One drop, counted and said out loud. Never the alert BODY: the body is already in the log
    /// through the producer's own record, and a drop line is about the delivery, not the incident.
    fn record_drop(&self, alert: &FiredAlert, reason: &'static str) {
        let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!(
            target_name = %self.name,
            rule = %alert.rule_id,
            reason,
            dropped_total = total,
            "alert DROPPED — the delivery queue could not take it, so this alert was never sent"
        );
    }
}

impl AlertSink for QueuedSink {
    /// Enqueue and return. The only work on the caller's thread is a routing check, a clone and a
    /// `try_send` — no wire, no wait, whatever the endpoint is doing.
    fn deliver(&self, alert: &FiredAlert) {
        if !self.accepts(alert) {
            return;
        }
        let envelope = Envelope { alert: Some(alert.clone()), dropped: self.dropped.clone() };
        // A refused send hands the envelope back inside the error. It is DISARMED before it goes
        // out of scope so the drop is counted exactly once, by `record_drop`, which also says it.
        let (mut envelope, reason) = match self.tx.try_send(envelope) {
            Ok(()) => return,
            Err(TrySendError::Full(e)) => (e, "queue full"),
            // The worker panicked (its inner sink broke the never-panic contract) or was already
            // joined. Either way this is not recoverable here and must not be silent.
            Err(TrySendError::Disconnected(e)) => (e, "delivery worker gone"),
        };
        envelope.disarm();
        self.record_drop(alert, reason);
    }

    /// Delegated, so the decorator and the sink it wraps can never disagree about routing.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        self.inner.accepts(alert)
    }
}

/// The worker body: deliver until told to stop, then let the queue count what is left.
///
/// The stop flag is read BEFORE each `recv`, and `recv_timeout` bounds how long an idle worker can
/// sleep through one — so a shutdown costs at most [`STOP_POLL`] plus whatever a delivery already in
/// flight still needs. That in-flight delivery is the residual [`QueuedSinkStop::shutdown`] bounds,
/// and `busy` is how it tells the two apart when it gives up.
fn drain(
    name: &str,
    inner: Arc<dyn AlertSink>,
    rx: Receiver<Envelope>,
    stopping: &AtomicBool,
    busy: &AtomicBool,
    delivered: &AtomicU64,
    dropped: &AtomicU64,
) {
    while !stopping.load(Ordering::Relaxed) {
        match rx.recv_timeout(STOP_POLL) {
            Ok(mut envelope) => {
                if let Some(alert) = envelope.alert() {
                    busy.store(true, Ordering::Relaxed);
                    inner.deliver(alert);
                    busy.store(false, Ordering::Relaxed);
                }
                // AFTER the sink returned: a sink that panics drops an ARMED envelope on the way
                // out, so the alert it was holding is counted rather than lost with the thread.
                envelope.disarm();
                delivered.fetch_add(1, Ordering::Relaxed);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every sender is gone: the engine holding this sink was dropped without a shutdown.
            // Nothing can arrive any more, so exiting is the whole of it.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
    // Stopping. Whatever is still queued will NOT be sent, and that is a loss the shutdown line
    // must state rather than let a reader infer it from a delivered count. The count is NOT taken
    // by draining the queue here — that leaves a window between the last look and the receiver's
    // drop in which a concurrent `deliver` can still land one, and it would be destroyed uncounted.
    // Dropping the receiver disconnects the channel and destroys every buffered [`Envelope`], each
    // of which counts itself, so the delta across the drop IS the abandoned set, late arrivals
    // included; a producer arriving after it gets `Disconnected` and counts its own. (Such a
    // producer's own count can land inside this delta and inflate THIS line by one; the session
    // total an outcome reports is exact either way, because both charge the same counter once.)
    let before = dropped.load(Ordering::Relaxed);
    drop(rx);
    let abandoned = dropped.load(Ordering::Relaxed) - before;
    if abandoned > 0 {
        tracing::warn!(
            target_name = %name,
            abandoned,
            "alert delivery stopped with alerts still queued — they were never sent"
        );
    }
}

/// What a [`QueuedSinkStop::shutdown`] did. Returned rather than logged so the CALLER decides the
/// wording and the level: this crate has no idea whether an abandoned pager matters more or less
/// than whatever else its owner is tearing down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedSinkOutcome {
    /// The worker finished and was joined. `dropped` is the session total, including anything still
    /// queued when the stop was raised.
    Joined { name: String, delivered: u64, dropped: u64 },
    /// The worker did not finish inside `budget` and the thread was left running; the process is
    /// expected to exit shortly. `dropped` is the total as of the give-up instant; the worker may
    /// still increment it afterwards.
    ///
    /// `mid_delivery` says WHAT was abandoned, because the two cases are different losses and the
    /// owner's disclosure must not conflate them: `true` means the worker was inside its inner
    /// sink's `deliver` at the give-up instant — an alert on the wire that may never arrive;
    /// `false` means it was IDLE, parked in its poll and merely not yet joined, because the budget
    /// it was handed was shorter than one [`STOP_POLL`] — nothing was in flight, and the thread
    /// exits on its own within one poll. An owner that stops several targets against one shared
    /// deadline hits the second case whenever an earlier target ate the deadline, and it must not
    /// then report a page that was never being sent as one that may not have arrived.
    Abandoned { name: String, delivered: u64, dropped: u64, budget: Duration, mid_delivery: bool },
}

impl QueuedSinkOutcome {
    /// The target this outcome is about.
    pub fn name(&self) -> &str {
        match self {
            QueuedSinkOutcome::Joined { name, .. } | QueuedSinkOutcome::Abandoned { name, .. } => {
                name
            }
        }
    }

    /// Alerts this target actually handed to its inner sink.
    pub fn delivered(&self) -> u64 {
        match self {
            QueuedSinkOutcome::Joined { delivered, .. }
            | QueuedSinkOutcome::Abandoned { delivered, .. } => *delivered,
        }
    }

    /// Alerts this target never sent — a full queue, a dead worker, or a queue abandoned at stop.
    pub fn dropped(&self) -> u64 {
        match self {
            QueuedSinkOutcome::Joined { dropped, .. }
            | QueuedSinkOutcome::Abandoned { dropped, .. } => *dropped,
        }
    }
}

/// The other half of [`QueuedSink::spawn`]: the handle that stops the worker. Held by whoever owns
/// the teardown, NOT by the engine — the engine owns `Box<dyn AlertSink>`es and has no shutdown of
/// its own to hang this on.
///
/// Dropping it without calling [`shutdown`](Self::shutdown) does not leak the thread forever: the
/// worker also exits when every sender is gone, i.e. when the engine holding the sink is dropped.
/// It does mean nothing is joined and nothing is REPORTED, which is why the owner should call it.
pub struct QueuedSinkStop {
    name: String,
    stopping: Arc<AtomicBool>,
    /// set by the worker around its inner `deliver`, read once at the give-up instant — see
    /// [`QueuedSinkOutcome::Abandoned`]'s `mid_delivery`.
    busy: Arc<AtomicBool>,
    worker: JoinHandle<()>,
    delivered: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl QueuedSinkStop {
    /// The target this handle stops.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raise the stop flag and wait up to `budget` for the worker to finish, joining it if it does.
    ///
    /// ⚠ **The wait is BOUNDED and the fallback is to ABANDON the thread**, which is deliberate and
    /// is the one place this type trades a guarantee away. `JoinHandle` offers no join-with-timeout,
    /// and a worker caught mid-POST cannot be interrupted — a plain `join()` would therefore add up
    /// to [`UreqTransport`]'s 10 s to its owner's teardown, and on the recorder that teardown is
    /// already sized against a systemd `TimeoutStopSec=`: a graceful stop that outruns that budget
    /// is SIGKILLed halfway, which loses buffered rows. So the caller names a budget, and past it
    /// the outcome says the thread was left running.
    ///
    /// Abandoning costs at most one un-POSTed page and leaks nothing that survives the process: the
    /// worker holds no lock, no file and no store handle — only a socket the OS closes on exit.
    /// That is the correct thing to lose, and it is REPORTED rather than assumed — and reported
    /// PRECISELY: the outcome carries whether the worker was actually inside a delivery at the
    /// give-up instant, because a `budget` shorter than one [`STOP_POLL`] abandons an IDLE worker
    /// too (it cannot have woken to see the flag yet), and that is a thread left to exit on its
    /// own, not a page left on the wire.
    ///
    /// What the wait costs in the NORMAL case — nothing queued, nothing in flight — is up to one
    /// [`STOP_POLL`], not microseconds: the worker re-reads the flag only when its `recv_timeout`
    /// returns. A caller stopping several handles one after another pays up to one poll each.
    pub fn shutdown(self, budget: Duration) -> QueuedSinkOutcome {
        self.stopping.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + budget;
        while !self.worker.is_finished() {
            if Instant::now() >= deadline {
                return QueuedSinkOutcome::Abandoned {
                    name: self.name,
                    delivered: self.delivered.load(Ordering::Relaxed),
                    dropped: self.dropped.load(Ordering::Relaxed),
                    budget,
                    mid_delivery: self.busy.load(Ordering::Relaxed),
                };
            }
            std::thread::sleep(JOIN_POLL);
        }
        // `is_finished` is true, so this join returns at once. A panicked worker is swallowed: it
        // was already reported through the disconnected-sender drops, and a teardown may not unwind
        // on one.
        let _ = self.worker.join();
        QueuedSinkOutcome::Joined {
            name: self.name,
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}
