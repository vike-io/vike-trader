//! `CoreHandle` + `ReconcileDriver` — the owner/GUI-facing side of the core — split out of the runtime fold module (behavior byte-identical; the
//! block moved verbatim). `use super::*` re-exports the parent runtime module's full
//! import set + items, so nothing about resolution changes.

use super::*;

/// Owner handle: command entry, event/market senders, the snapshot cell, join.
pub struct CoreHandle {
    pub(crate) ingest: mpsc::Sender<Ingest>,
    pub(crate) market: Arc<Conflated>,
    pub(crate) snapshot: Arc<ArcSwap<CoreSnapshot>>,
    pub(crate) rejected: Arc<AtomicU64>,
    pub(crate) join: std::thread::JoinHandle<()>,
}

impl CoreHandle {
    /// GUI command path: `try_send`, NEVER blocks. Failure is returned AND counted.
    pub fn try_command(&self, cmd: Command) -> Result<(), CommandRejected> {
        self.ingest.try_send(Ingest::Command(cmd)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                CommandRejected::Busy
            }
            mpsc::error::TrySendError::Closed(_) => CommandRejected::Gone,
        })
    }

    pub fn event_sender(&self) -> EventSender {
        EventSender { ingest: self.ingest.clone(), route_key: None }
    }

    /// A narrow, cloneable [`CommandSink`] over this core's ingest lane — the command-only
    /// capability an OUT-OF-PROCESS control surface (the `vike-tradehub` control server) holds to
    /// lower a remote peer's order command into the SAME single-writer ingest lane
    /// [`Self::try_command`] feeds, WITHOUT the snapshot cell / market slot / join handle a full
    /// `CoreHandle` owns (so it can neither read state nor shut the core down).
    pub fn command_sink(&self) -> CommandSink {
        CommandSink { ingest: self.ingest.clone(), rejected: Arc::clone(&self.rejected) }
    }

    /// Lossless command path: `blocking_send` a [`Command`] into the same ingest lane the
    /// event/bar/tick senders feed (unlike [`Self::try_command`], which never blocks and is the
    /// GUI's path). Every command queued ahead of it still folds first; a closed lane (core
    /// already exited) is ignored, matching [`Self::shutdown_and_join`]. Used by session control
    /// and tests that need a command to be delivered losslessly.
    pub fn send_command(&self, c: Command) {
        let _ = self.ingest.blocking_send(Ingest::Command(c));
    }

    pub fn market_sender(&self) -> MarketSender {
        MarketSender { inner: Arc::clone(&self.market), ingest: self.ingest.clone() }
    }

    pub fn bar_sender(&self) -> BarSender {
        BarSender { ingest: self.ingest.clone() }
    }

    /// Lossless L2/tick lane for HFT feeds (quotes, trades, book updates).
    pub fn tick_sender(&self) -> TickSender {
        TickSender { ingest: self.ingest.clone() }
    }

    /// Latest published snapshot (the GUI's per-repaint read).
    pub fn snapshot(&self) -> Arc<CoreSnapshot> {
        self.snapshot.load_full()
    }

    /// The cell itself — hand to the GUI so it needs no reference back to the handle.
    pub fn snapshot_cell(&self) -> Arc<ArcSwap<CoreSnapshot>> {
        Arc::clone(&self.snapshot)
    }

    /// Supervisor probe: false once the core thread has exited.
    pub fn is_alive(&self) -> bool {
        !self.join.is_finished()
    }

    /// Opt-in CONTINUOUS drift cadence (audit exec#2 follow-up). Spawn a background thread that,
    /// every `interval`, runs `fetch_snapshot` — a venue `reconcile_positions()`-style REST fetch,
    /// executed ON THIS DRIVER THREAD, NEVER the core fold — and, when it yields `Some`, delivers
    /// that snapshot as `Command::ApplySnapshot` into this core's ingest lane. The existing
    /// [`ExecutionEngine::diff_snapshot`](vike_exec::ExecutionEngine::diff_snapshot) then re-runs at
    /// the core's single ApplySnapshot choke point, so venue-vs-local divergence re-surfaces in the
    /// recent-events ring on EVERY tick — not just at startup. Reuses the SAME command + diff the
    /// startup reconcile uses: no new `Command` verb, `Event` variant, wire change, or fold-thread
    /// work.
    ///
    /// This is the venue-agnostic, opt-in cadence. The RECONNECT-triggered re-snapshot (fetch on
    /// every WS re-open) stays DEFERRED: the venue user-data pump + its A3 resync supervisor
    /// (`vike-bridge-core::user_data`) hold only an [`EventSender`], not a command lane, so wiring a
    /// snapshot there would need new command plumbing threaded through every bridge — the fork
    /// mirrored on `Command::ConfirmOrder`. A modest `interval` here already bounds post-reconnect
    /// drift to one tick.
    ///
    /// Default OFF: nothing spawns this unless a binary opts in, so zero behavior change otherwise.
    /// Bounded + self-cleaning: the driver holds a WEAK ingest sender (like the audit-C3 watchdog)
    /// so it never keeps the core alive — it self-exits when the core is gone (`upgrade`/send fails)
    /// or on [`ReconcileDriver::shutdown`]. `fetch_snapshot` returning `None` (a REST error the
    /// caller swallowed, or nothing to report) simply skips that tick.
    pub fn spawn_periodic_reconcile(
        &self,
        interval: Duration,
        mut fetch_snapshot: impl FnMut() -> Option<ReconcileSnapshot> + Send + 'static,
    ) -> ReconcileDriver {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        // WEAK like the watchdog: the driver must NOT keep the ingest channel open, or the core's
        // "every strong sender dropped -> clean break" exit would never fire.
        let weak = self.ingest.downgrade();
        let handle = std::thread::Builder::new()
            .name("vt-core-reconcile".into())
            .spawn(move || {
                // Sub-`interval` poll so stop is observed promptly even when `interval` is long.
                let poll = interval.min(Duration::from_millis(250)).max(Duration::from_millis(1));
                let mut last = Instant::now();
                loop {
                    std::thread::sleep(poll);
                    if stop_t.load(Ordering::Relaxed) {
                        break;
                    }
                    if last.elapsed() < interval {
                        continue;
                    }
                    last = Instant::now();
                    // Core gone? The weak upgrade fails once the CoreHandle (its strong ingest
                    // sender) is dropped — self-exit even on a `None`-returning fetch.
                    if weak.upgrade().is_none() {
                        break;
                    }
                    if let Some(snap) = fetch_snapshot() {
                        let Some(tx) = weak.upgrade() else { break };
                        // Plain std thread (no tokio runtime) → blocking_send is legal and lossless;
                        // a closed lane (core exited mid-fetch) breaks the loop.
                        if tx
                            .blocking_send(Ingest::Command(Command::ApplySnapshot(Box::new(snap))))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            })
            .expect("spawn vt-core reconcile driver");
        ReconcileDriver { stop, handle }
    }

    /// Lossless shutdown: deliver `Command::Shutdown` (blocking — must not be dropped),
    /// then join the core thread. Every queued message ahead of it is still folded.
    pub fn shutdown_and_join(self) {
        let _ = self.ingest.blocking_send(Ingest::Command(Command::Shutdown));
        let _ = self.join.join();
    }
}

/// A narrow, cloneable command entry into the core: ONLY the ingest lane + the shared rejected
/// counter, handed out by [`CoreHandle::command_sink`]. It exposes exactly [`Self::try_command`] —
/// the non-blocking GUI path — and NOTHING else, so an out-of-process control surface (the
/// `vike-tradehub` control server) can submit order commands without holding — or being able to
/// misuse — the snapshot cell, the market conflation slot, or the core's join handle. `Clone` so the
/// accept loop can hand one per connection, exactly as it clones the publisher/keys.
#[derive(Clone)]
pub struct CommandSink {
    ingest: mpsc::Sender<Ingest>,
    rejected: Arc<AtomicU64>,
}

impl CommandSink {
    /// Non-blocking command path — the EXACT body of [`CoreHandle::try_command`]: `try_send` into
    /// the ingest lane, a full queue counted in the SAME `rejected` counter the snapshot surfaces
    /// (`CommandRejected::Busy`), a closed lane (core exited) `CommandRejected::Gone`. Never blocks
    /// and never panics — a per-connection server thread is never back-pressured by the core.
    pub fn try_command(&self, cmd: Command) -> Result<(), CommandRejected> {
        self.ingest.try_send(Ingest::Command(cmd)).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                CommandRejected::Busy
            }
            mpsc::error::TrySendError::Closed(_) => CommandRejected::Gone,
        })
    }

    /// LOSSLESS command path — the EXACT body of [`CoreHandle::send_command`]: `blocking_send` into
    /// the same ingest lane, so every command queued ahead still folds first and a full lane waits
    /// rather than dropping. A closed lane (core already exited) is ignored, matching
    /// [`CoreHandle::shutdown_and_join`].
    ///
    /// For an OPERATOR control channel that must not lose a typed order to a momentarily full lane —
    /// `vike-tradehub`'s stdio loop, which used `CoreHandle::send_command` verbatim until the
    /// graceful-stop change moved it onto its own thread and it needed an owned, `'static` sender.
    /// ⚠ The REMOTE server path deliberately keeps [`Self::try_command`]: a network peer must never
    /// be able to back-pressure a per-connection thread, which is a different question from whether
    /// a human at a terminal may wait for their own command.
    ///
    /// Plain std threads only (no tokio runtime on the caller's thread), the same constraint
    /// [`CoreHandle::spawn_periodic_reconcile`]'s driver thread already documents.
    pub fn send_blocking(&self, cmd: Command) {
        let _ = self.ingest.blocking_send(Ingest::Command(cmd));
    }
}

/// Join half of an opt-in periodic reconcile driver ([`CoreHandle::spawn_periodic_reconcile`]).
/// Deterministic teardown mirrors the venue feed handles: raise stop, join the thread. The driver
/// also self-exits on its own once the core is gone, so an explicit shutdown is optional (drop is
/// safe — the thread ends when its weak ingest sender can no longer upgrade).
pub struct ReconcileDriver {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl ReconcileDriver {
    /// Supervisor probe: true once the driver thread has exited (e.g. it self-exited because the
    /// core was dropped). Mirrors [`CoreHandle::is_alive`]'s use of `JoinHandle::is_finished`.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop the driver thread and join it. Idempotent with the driver's own self-exit: joining an
    /// already-finished thread returns immediately.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}
