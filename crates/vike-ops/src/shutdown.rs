//! Bounded, parallel shutdown orchestration — the pure, renderer-agnostic core of the desktop's
//! window-close teardown (`vike-desktop`'s `run_bounded_teardown`), and of the daemons' teardowns
//! too (`vike-tradehub`, the datahub's recorder). Extracted from `vike-app/src/main.rs`'s `on_exit`
//! (PR #589) and lifted here so it runs in CI: `vike-app` was compile-checked but never TESTED in
//! CI (wgpu build weight), this crate is (same reason `dom_math`/`feed_lifecycle`/`sync_group` were
//! moved down — so their unit tests run).
//!
//! # The contract
//!
//! [`run_with_deadline`] runs a set of independent shutdown `tasks` concurrently, then a single
//! sequential `then` continuation once every task has finished, and bounds the WHOLE sequence by one
//! overall wall-clock `deadline`:
//!
//! 1. **Parallel fan-out.** Each task in `tasks` is spawned on its own thread. In the desktop these
//!    are the per-venue feed `shutdown()` calls; running them concurrently makes the set cost about
//!    one socket read-timeout instead of the per-venue sum (the >6s → ~1.5s fix).
//! 2. **Sequential tail.** Once every task thread has joined, `then` runs (still on the orchestration
//!    thread). It is the LOAD-BEARING SEQUENTIAL stage: in `vike-app` it was recorder → recon →
//!    materializer → core, in that exact order — a live feed thread must never observe a closed
//!    recorder or core ingest channel, so the tail must run strictly AFTER the feed tasks, never
//!    concurrently with them. That is why it is a distinct stage and not just another `tasks` entry.
//!    (The desktop's four are all `None` since it lost its local core; the daemon's core teardown
//!    relies on the same ordering.)
//! 3. **Bounded return.** The calling thread waits at most `deadline` for the whole sequence to
//!    signal completion, then returns REGARDLESS — a single overall deadline, NOT per-task. If the
//!    deadline elapses first, any still-running task/tail threads are abandoned: the caller returns,
//!    the process exits, and the OS reaps them (nothing on a shutdown path must outlive the process).
//!    This bound is the regression guard — without it a task parked in a blocking read holds exit
//!    open indefinitely (the original >6s hang).
//!
//! # What the outcome tells a caller, and what it cannot
//!
//! ⚠ [`ShutdownOutcome::HardCapped`]'s `still_running` counts the PARALLEL tasks ONLY. It is `0` for
//! the entire duration of the sequential tail, so `HardCapped { still_running: 0 }` never meant
//! "nothing was outstanding" — it meant "the TAIL was still running". A live `vike-tradehub` stop
//! printed exactly that as `hard-capped at the 10s deadline; 0 task(s) still in flight`, which is
//! self-contradictory on its face and sent an operator looking for a straggler that did not exist
//! (the alpaca+ctrader live rehearsal (PR #1407), Finding C). [`ShutdownStage`] is the missing
//! fact, and a caller's message must name it.
//!
//! The WAIT was legitimate in that incident: the tail really was running (the core join drops the
//! `CommandJournal`, whose `Drop` joins the `vjl-sync` thread — an unbounded join inside the tail).
//! So the defect was the REPORT, not the bound, and the bound is deliberately unchanged.
//!
//! [`ShutdownOutcome::Aborted`] is the third answer, split out of what used to be a bare `Err(_)`:
//! the orchestration thread died without signalling. It returns in microseconds, so reporting it as
//! a deadline hard cap named a budget nothing had spent.
//!
//! The orchestration runs on a throwaway thread and the calling thread waits on a one-shot channel,
//! because std has no timed `JoinHandle::join`. This mirrors `on_exit`'s original structure exactly;
//! only the eframe/App-specific glue (WHICH handles to tear down, the cooperative `AtomicBool` signal
//! that gates `ensure_feed_on`, the 1500 ms policy value) stays in `vike-desktop`.
//!
//! No logging, no I/O, no eframe/egui — pure std threading, so CI covers the logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Which STAGE of the sequence was in progress — the fact `still_running` structurally cannot
/// carry, and whose absence made the hard-cap message self-contradictory.
///
/// `still_running` counts only the PARALLEL tasks. Once they have all joined it is `0` forever,
/// including for the entire duration of the sequential tail — so `HardCapped { still_running: 0 }`
/// read as "the deadline elapsed with nothing outstanding", which is false and unactionable. It
/// always meant "the TAIL was still running". This enum says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStage {
    /// Still fanning out / joining the parallel `tasks`.
    Tasks,
    /// Every task joined; the sequential `then` tail is running. THE stage a `still_running: 0`
    /// hard cap is actually in.
    Tail,
    /// The tail returned. Only observable on the [`ShutdownOutcome::Aborted`] path (a completed
    /// sequence signals and yields [`ShutdownOutcome::Graceful`] instead).
    Done,
}

impl ShutdownStage {
    /// A phrase for a log line: what was holding the shutdown open.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            ShutdownStage::Tasks => "parallel tasks were still joining",
            ShutdownStage::Tail => {
                "the sequential tail was still running (in vike-tradehub: post-feeds flush, then \
                 the recon-driver stop, then the core join — which drops the CommandJournal and \
                 joins its vjl-sync thread)"
            }
            ShutdownStage::Done => "the sequence had finished",
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            0 => ShutdownStage::Tasks,
            1 => ShutdownStage::Tail,
            _ => ShutdownStage::Done,
        }
    }
}

/// The result of a bounded shutdown run — did the whole sequence finish within the deadline, did
/// the deadline hard-cap it, or did the orchestration itself die?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Every task finished, the `then` tail ran, and the sequence signalled completion before the
    /// deadline. The common case — a graceful teardown that flushed everything.
    Graceful,
    /// The deadline elapsed before the sequence finished; the caller returned and abandoned whatever
    /// was still running.
    HardCapped {
        /// Count of parallel `tasks` still in flight at the deadline (0..=`tasks.len()`).
        ///
        /// ⚠ `0` does NOT mean "nothing was outstanding" — read [`ShutdownStage`]. It means every
        /// PARALLEL task finished; `stage` is what was actually holding the deadline.
        still_running: usize,
        /// Which stage was in progress at the deadline. This is the field a message must name.
        stage: ShutdownStage,
    },
    /// The orchestration thread DIED without signalling — it panicked (most likely inside `then`)
    /// or never spawned at all.
    ///
    /// ⚠ Distinct from [`ShutdownOutcome::HardCapped`] because it returns essentially INSTANTLY
    /// rather than at the deadline, and means something is broken rather than slow. Folding the two
    /// together (a bare `Err(_)` arm on `recv_timeout`) produced a "hard-capped at the 10s
    /// deadline" line milliseconds into a panicking teardown — a message naming a budget nothing
    /// had spent.
    Aborted {
        /// The stage reached before the orchestration thread died.
        stage: ShutdownStage,
    },
}

/// Run `tasks` concurrently, then `then` once they have all joined, bounding the whole sequence by a
/// single overall `deadline`; return whether it finished gracefully or was hard-capped.
///
/// See the module doc for the full contract. `then` is the load-bearing sequential tail that must run
/// strictly after every task (pass a no-op closure if there is none). Still-running threads past the
/// deadline are abandoned by design — the caller returns and the process exits.
pub fn run_with_deadline(
    tasks: Vec<Box<dyn FnOnce() + Send + 'static>>,
    then: Box<dyn FnOnce() + Send + 'static>,
    deadline: Duration,
) -> ShutdownOutcome {
    // Tasks still in flight — each task thread decrements this on completion, so at the deadline it
    // is exactly the number of stragglers to report. Read only on the calling thread after the wait.
    let remaining = Arc::new(AtomicUsize::new(tasks.len()));
    // Which stage the orchestration thread is in. `still_running` cannot express this: it is `0`
    // for the whole of the tail, which is why a tail that outran the deadline used to report
    // "0 task(s) still in flight" and read as a contradiction.
    let stage = Arc::new(AtomicU8::new(0));

    // Throwaway orchestration thread (mirrors `on_exit`'s `vt-app-shutdown` thread): fan the tasks
    // out, join them all, run the sequential tail, then signal done. The calling thread never blocks
    // on a raw join — it waits on this one-shot channel with the deadline instead.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let remaining_for_thread = Arc::clone(&remaining);
    let stage_for_thread = Arc::clone(&stage);
    let spawned = thread::Builder::new().name("vt-app-shutdown".into()).spawn(move || {
        let task_threads: Vec<_> = tasks
            .into_iter()
            .map(|task| {
                let remaining = Arc::clone(&remaining_for_thread);
                thread::spawn(move || {
                    // ⚠ The decrement is a GUARD, not a statement after `task()`. A task that
                    // PANICS unwinds past a trailing `fetch_sub`, leaving the count permanently
                    // inflated — so a teardown whose feed shutdown panicked would report phantom
                    // stragglers at every later deadline, blaming the wrong stage. A guard's `Drop`
                    // runs on the unwind path too.
                    struct Done(Arc<AtomicUsize>);
                    impl Drop for Done {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                    let _done = Done(remaining);
                    task();
                })
            })
            .collect();
        for t in task_threads {
            let _ = t.join();
        }
        // Load-bearing: the tail runs only AFTER every task above has joined.
        stage_for_thread.store(1, Ordering::Relaxed);
        then();
        stage_for_thread.store(2, Ordering::Relaxed);
        // The receiver may already be gone (deadline elapsed first) — expected; drop the error.
        let _ = done_tx.send(());
    });
    // A thread that never spawned cannot advance the stage or signal; report it as the abort it is
    // rather than waiting out a deadline nothing is working against.
    if spawned.is_err() {
        return ShutdownOutcome::Aborted { stage: ShutdownStage::Tasks };
    }

    // Bounded guarantee: wait at most `deadline` for the sequence to signal done, then return
    // regardless.
    match done_rx.recv_timeout(deadline) {
        Ok(()) => ShutdownOutcome::Graceful,
        // The deadline genuinely elapsed. `stage` says what was holding it; `still_running` counts
        // only the parallel half and is `0` for any tail-stage cap.
        Err(mpsc::RecvTimeoutError::Timeout) => ShutdownOutcome::HardCapped {
            still_running: remaining.load(Ordering::Relaxed),
            stage: ShutdownStage::from_code(stage.load(Ordering::Relaxed)),
        },
        // ⚠ NOT a hard cap: the sender was dropped without a send, so the orchestration thread
        // panicked. This returns in microseconds, and reporting it as "hard-capped at the deadline"
        // names a budget nothing spent — the second half of the message defect.
        Err(mpsc::RecvTimeoutError::Disconnected) => ShutdownOutcome::Aborted {
            stage: ShutdownStage::from_code(stage.load(Ordering::Relaxed)),
        },
    }
}

#[path = "shutdown_tests.rs"]
#[cfg(test)]
mod shutdown_tests;
