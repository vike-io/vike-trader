//! A SHARED CLOSE (decision 0271, its limit (a) closed): a mount's decision at a close it shares with one of its
//! INPUTS (a 1h decision at 12:00 and the 4h input bar that closes at 12:00) sees that input bar, as its backtest
//! does, whichever of the two the feed delivers first.
//!
//! # The wait
//!
//! When a mount's own bar closes at `T` and one of its inputs' NEXT bar (the one after the newest it holds) closes
//! exactly at `T` too (`CoreThread::shared_close_input`, the inputs' own rule), that mount's BAR-STEP DISPATCH (its
//! `on_bar` and the orders it buffers) is HELD ([`CoreThread::park_shared_close`]): everything else of the bar
//! runs on time (the paper fills at its open, the mark, the sweeps, the conditionals, every other mount's step), and
//! ticks, fills and the other mounts keep flowing while it waits. It is released, exactly once, by the first of:
//!
//! - **the input's bar arriving** (`CoreThread::feed_inputs`, once no input of the mount still waits for a bar
//!   closing at `T`): the step runs inside that bar's dispatch, and reads it;
//! - **the grace passing**, [`SHARED_CLOSE_GRACE_MS`] after the mount's own bar arrived, at the drain-loop boundary
//!   ([`CoreThread::release_due_shared_closes`]): the step runs on the input bar before it, and the timeout is said
//!   ONCE (a WARN and a ring line naming the mount, the input and the instant) and journaled;
//! - **the mount's next bar arriving first** (a bar shorter than the grace): the held step runs before that bar's
//!   fills and its own step, said as a timeout too ([`CoreThread::flush_shared_closes_of`]).
//!
//! The held step keeps the mount's closed series as it stood at the close (an `Arc` of it), so its `on_bar` sees
//! the bars it would have seen then, at the index it would have had; its position, equity and inputs are read when
//! it runs.
//!
//! # Replay
//!
//! A timeout is journaled WRITE-AHEAD of the step it releases (`vike_journal::JournalRecord::SharedCloseTimeout`, at
//! the boundary's clock), a decision marker like `ScheduleFire`: the step's orders ride their own `StrategySubmit`
//! records in the order the live core wrote them, so a replay re-applies them in that order, and a replay core,
//! which hears no bar and mounts no strategy that reads an input, never holds or re-decides a step (nothing here
//! reads its clock: the boundary check is behind `shared_closes.is_empty()`).
//!
//! # A wake
//!
//! A core that hears nothing for the grace would reach the boundary only at its next message, so a held step asks
//! for a wake at its grace's end: one short named thread ([`WAKE_THREAD`]) that sleeps the grace and sends the core
//! an `Ingest::Watchdog` (the waker message, a no-op dispatch) through a WEAK sender that never keeps the lane open
//! (`CoreThread::self_wake`, set by `spawn_core`). One per held step, never per message.
//!
//! # The cost
//!
//! A core whose mounts read no input: one `Option` read per mount per bar step, one `is_empty()` per bar message
//! and per drained batch. A held step: one `Vec` push, a `Bar` and `SeriesKey` clone, an `Arc` refcount and a
//! thread, all once per shared close. A restart forgets a held step (its bar is gone, as a bar that arrived while
//! the node was down is), and an unmount drops it.

use super::*;

/// **How long a mount's bar step waits at a shared close for the input bar that closes at the same instant**: 2 s.
///
/// Long enough to cover the normal spread between two closes of one instant: both come over the strategy lane from
/// the same bar keeper, usually milliseconds apart, and a cTrader trendbar closes only when the next period's first
/// snapshot arrives (`crates/bridges/ctrader/src/feed/bar.rs`'s `TrendbarCloses`), the first tick after the
/// close, well under a second on a traded symbol. Short enough that a decision is late by at most a thin slice of the
/// shortest bar a mount trades in practice (2 s of a 1m bar, under 0.06% of a 1h one), and that a lost input bar
/// costs one late decision, never a stuck mount. A bar shorter than it is released by its own next bar instead.
pub const SHARED_CLOSE_GRACE_MS: i64 = 2_000;

/// The name of the thread that wakes a core at a held step's grace's end (at most 15 bytes).
const WAKE_THREAD: &str = "th-core-close";

/// How much later than the grace the wake lands, so the boundary's clock is past the deadline when it comes.
const WAKE_SLACK_MS: u64 = 5;

/// One mount bar step a shared close holds ([`CoreThread::shared_closes`]).
#[derive(Debug, Clone)]
pub(crate) struct ParkedClose {
    /// The mount's slot.
    pub(crate) mount: usize,
    /// The series its bar closed on, and the bar.
    key: SeriesKey,
    bar: Bar,
    /// The mount's closed series as it stood at the close, the bar last.
    bars: Arc<Vec<Bar>>,
    /// The decision's instant: the bar's close.
    close: i64,
    /// The core clock at which the grace has passed.
    deadline: i64,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// HOLD mount `mount`'s step on `bar` of `key`, which closes at `close`, shared with an input bar that has not
    /// arrived (the module doc). Its grace runs from this dispatch's clock; a core spawned by `spawn_core` asks for
    /// a wake at its end.
    #[cold]
    #[inline(never)]
    pub(crate) fn park_shared_close(
        &mut self,
        mount: usize,
        key: &SeriesKey,
        bar: &Bar,
        close: i64,
    ) {
        let bars = Arc::clone(&self.bars.get(key).expect("series just appended").closed);
        let deadline = self.engine.now_ms.saturating_add(SHARED_CLOSE_GRACE_MS);
        self.shared_closes.push(ParkedClose {
            mount,
            key: key.clone(),
            bar: bar.clone(),
            bars,
            close,
            deadline,
        });
        if let Some(wake) = self.self_wake.clone() {
            let sleep =
                std::time::Duration::from_millis(SHARED_CLOSE_GRACE_MS as u64 + WAKE_SLACK_MS);
            // A thread that cannot start costs the wake only: the next message reaches the boundary.
            let _ = std::thread::Builder::new().name(WAKE_THREAD.into()).spawn(move || {
                std::thread::sleep(sleep);
                if let Some(tx) = wake.upgrade() {
                    // Full = the core is busy, so it reaches the boundary anyway; Closed = shutting down.
                    let _ = tx.try_send(Ingest::Watchdog);
                }
            });
        }
    }

    /// Release every held step none of whose mount's inputs still waits for a bar closing at its close: an input
    /// bar was just filed (`feed_inputs`). Each runs here, inside that bar's dispatch, and reads it.
    pub(crate) fn release_arrived_shared_closes(&mut self) {
        let mut k = 0;
        while k < self.shared_closes.len() {
            let p = &self.shared_closes[k];
            if self.input_awaited(p.mount, p.close).is_some() {
                k += 1;
                continue;
            }
            let p = self.shared_closes.remove(k);
            self.step_parked(p);
        }
    }

    /// **The boundary's release** (`run_loop.rs`, behind `shared_closes.is_empty()`): every held step whose grace
    /// passed at `now` runs, on the input bars it has, its timeout said and journaled first. Guarded like a
    /// message (`handle`): a strategy's panic here puts the core in its safe state, never past the thread.
    pub(crate) fn release_due_shared_closes(&mut self, now: i64) {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            while let Some(k) = self.shared_closes.iter().position(|p| p.deadline <= now) {
                let p = self.shared_closes.remove(k);
                self.say_shared_close_timeout(&p, now);
                self.step_parked(p);
            }
            self.dispatch_applied_fills();
        }));
        if let Err(payload) = outcome {
            self.enter_safe_state(payload_text(&*payload).to_owned());
            self.drain_poisoned();
        }
        self.drain_delivered();
    }

    /// A later bar of `key` (opened at `bar_timestamp`) arrived while a step on an earlier one is still held: that step
    /// runs FIRST, before this bar's fills and its own step, said as a timeout (its input did not come within the
    /// bar). A re-close of the held bar itself releases nothing.
    #[cold]
    #[inline(never)]
    pub(crate) fn flush_shared_closes_of(&mut self, key: &SeriesKey, bar_timestamp: i64) {
        let now = self.engine.now_ms;
        while let Some(k) =
            self.shared_closes.iter().position(|p| &p.key == key && p.bar.timestamp < bar_timestamp)
        {
            let p = self.shared_closes.remove(k);
            self.say_shared_close_timeout(&p, now);
            self.step_parked(p);
        }
    }

    /// Mount `mount` is about to step on another bar while a step of its own is still held: that one runs first,
    /// said as a timeout.
    #[cold]
    #[inline(never)]
    pub(crate) fn flush_shared_closes_of_mount(&mut self, mount: usize) {
        let now = self.engine.now_ms;
        while let Some(k) = self.shared_closes.iter().position(|p| p.mount == mount) {
            let p = self.shared_closes.remove(k);
            self.say_shared_close_timeout(&p, now);
            self.step_parked(p);
        }
    }

    /// Run a held step, on the series it closed on: nothing for a mount unmounted since.
    fn step_parked(&mut self, p: ParkedClose) {
        if self.mounts.get(p.mount).is_some_and(Option::is_some) {
            self.step_mount_on_bar_over(p.mount, &p.key, &p.bar, p.bars);
        }
    }

    /// Say ONCE that held step `p` goes ahead without its input's bar (a WARN, a ring line), journaled write-ahead of
    /// the step (`SharedCloseTimeout`, at `now`).
    #[cold]
    fn say_shared_close_timeout(&mut self, p: &ParkedClose, now: i64) {
        let mount_id = self.mount_ids.get(p.mount).cloned().unwrap_or_default();
        let input = self
            .input_awaited(p.mount, p.close)
            .map(|k| self.input_name(p.mount, k).to_string())
            .unwrap_or_default();
        if let Some(j) = self.journal.as_mut() {
            j.append_shared_close_timeout(now, &mount_id, p.close, &input).expect("journal append");
            self.journaled_since_snap += 1;
        }
        tracing::warn!(
            target: "vike_core::core",
            mount_id = %mount_id,
            input = %input,
            close_ms = p.close,
            grace_ms = SHARED_CLOSE_GRACE_MS,
            "SHARED CLOSE TIMEOUT: the input's bar closing at the decision did not arrive in time; the decision \
             goes ahead on the input's bar before it (decision 0271)"
        );
        self.note(format!(
            "SHARED CLOSE TIMEOUT: `{mount_id}` decided at {} ms without `{input}`'s bar closing then: it did \
             not arrive within {SHARED_CLOSE_GRACE_MS} ms, so the decision read the bar before it",
            p.close
        ));
    }
}
