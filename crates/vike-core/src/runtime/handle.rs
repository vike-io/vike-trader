//! `CoreHandle` + `CommandSink` — the owner/GUI-facing side of the core — split out of the runtime fold module (behavior byte-identical; the
//! block moved verbatim). `use super::*` re-exports the parent runtime module's full
//! import set + items, so nothing about resolution changes.

use super::*;

/// Owner handle: command entry, event/market senders, the snapshot cell, join.
pub struct CoreHandle {
    /// The ingest lane. Public ONLY so `vike_tradehub_core`'s reconcile manager can take its `downgrade()` (a WEAK
    /// sender that never keeps the core alive); every other
    /// path in goes through the methods here, which count a refusal in `rejected`.
    pub ingest: mpsc::Sender<Ingest>,
    pub(crate) market: Arc<MarketConflation>,
    pub(crate) snapshot: Arc<ArcSwap<CoreSnapshot>>,
    pub(crate) rejected: Arc<AtomicU64>,
    pub(crate) join: std::thread::JoinHandle<()>,
}

impl CoreHandle {
    /// GUI command path: `try_send`, NEVER blocks. Failure is returned AND counted.
    pub fn try_command(&self, cmd: Command) -> Result<(), CommandRejected> {
        try_push_command(&self.ingest, &self.rejected, cmd)
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
        // Class (b): a typed ORDER command silently lost is an order that never existed for the
        // operator. Cold path (a producer thread, never the fold), so a closed lane is announced.
        if self.ingest.blocking_send(Ingest::Command(c)).is_err() {
            tracing::warn!(
                target: "vike_core::handle",
                "send_command: core has exited, command not delivered"
            );
        }
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

    /// Lossless shutdown: deliver `Command::Shutdown` (blocking — must not be dropped),
    /// then join the core thread. Every queued message ahead of it is still folded.
    pub fn shutdown_and_join(self) {
        // Core already exited = already shut down; a `join` Err is a core panic its hook reported.
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
    /// Non-blocking command path — the SAME push as [`CoreHandle::try_command`]: `try_send` into
    /// the ingest lane, a full queue counted in the SAME `rejected` counter the snapshot surfaces
    /// (`CommandRejected::Busy`), a closed lane (core exited) `CommandRejected::Gone`. Never blocks
    /// and never panics — a per-connection server thread is never back-pressured by the core.
    pub fn try_command(&self, cmd: Command) -> Result<(), CommandRejected> {
        try_push_command(&self.ingest, &self.rejected, cmd)
    }

    /// LOSSLESS command path — the EXACT body of [`CoreHandle::send_command`]: `blocking_send` into
    /// the same ingest lane, so every command queued ahead still folds first and a full lane waits
    /// rather than dropping. A closed lane (core already exited) is `Err(CoreGone)`: the command was NOT
    /// delivered, and the caller owns telling its operator (`vike-tradehub`'s stdio loop replies with an
    /// error instead of an ack).
    ///
    /// For an OPERATOR control channel that must not lose a typed order to a momentarily full lane —
    /// `vike-tradehub`'s stdio loop, which used `CoreHandle::send_command` verbatim until the
    /// graceful-stop change moved it onto its own thread and it needed an owned, `'static` sender.
    /// ⚠ The REMOTE server path deliberately keeps [`Self::try_command`]: a network peer must never
    /// be able to back-pressure a per-connection thread, which is a different question from whether
    /// a human at a terminal may wait for their own command.
    ///
    /// Plain std threads only (no tokio runtime on the caller's thread): `blocking_send` panics
    /// inside one.
    pub fn send_blocking(&self, cmd: Command) -> Result<(), vike_execution::CoreGone> {
        self.ingest.blocking_send(Ingest::Command(cmd)).map_err(|_| vike_execution::CoreGone)
    }
}

/// The one non-blocking command push behind [`CoreHandle::try_command`] and
/// [`CommandSink::try_command`]: `try_send` into `ingest`; a full lane counts one in `rejected`
/// and is `CommandRejected::Busy`, a closed lane (core exited) is `CommandRejected::Gone`.
fn try_push_command(
    ingest: &mpsc::Sender<Ingest>,
    rejected: &AtomicU64,
    cmd: Command,
) -> Result<(), CommandRejected> {
    ingest.try_send(Ingest::Command(cmd)).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => {
            rejected.fetch_add(1, Ordering::Relaxed);
            CommandRejected::Busy
        }
        mpsc::error::TrySendError::Closed(_) => CommandRejected::Gone,
    })
}
