//! Bounded, parallel shutdown orchestration — the pure, renderer-agnostic core of `vike-app`'s
//! window-close teardown. Extracted from `vike-app/src/main.rs`'s `on_exit` (PR #589) and lifted
//! here so it runs in CI: `vike-app` is compile-checked but never TESTED in CI (wgpu build weight),
//! this crate is (same reason `dom_math`/`feed_lifecycle`/`sync_group` were moved down — so their
//! unit tests run).
//!
//! # The contract
//!
//! [`run_with_deadline`] runs a set of independent shutdown `tasks` concurrently, then a single
//! sequential `then` continuation once every task has finished, and bounds the WHOLE sequence by one
//! overall wall-clock `deadline`:
//!
//! 1. **Parallel fan-out.** Each task in `tasks` is spawned on its own thread. In `vike-app` these
//!    are the per-venue feed `shutdown()` calls; running them concurrently makes the set cost about
//!    one socket read-timeout instead of the per-venue sum (the >6s → ~1.5s fix).
//! 2. **Sequential tail.** Once every task thread has joined, `then` runs (still on the orchestration
//!    thread). It is the LOAD-BEARING SEQUENTIAL stage: in `vike-app`, recorder → recon →
//!    materializer → core, in that exact order — a live feed thread must never observe a closed
//!    recorder or core ingest channel, so the tail must run strictly AFTER the feed tasks, never
//!    concurrently with them. That is why it is a distinct stage and not just another `tasks` entry.
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
//! that gates `ensure_feed_on`, the 1500 ms policy value) stays in `vike-app`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    /// A boxed task that sleeps `ms` then runs — the fake for a venue feed's `shutdown()`.
    fn sleepy(ms: u64) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || thread::sleep(Duration::from_millis(ms)))
    }

    /// A task that BLOCKS until the test drops its sender — i.e. deterministically "still running"
    /// at any deadline, with no sleep racing the scheduler.
    ///
    /// The sleep-based version of this (`sleepy(2000)` against a 200ms deadline) was fine for the
    /// SLOW side but pinned the FAST side to a 10ms-vs-200ms margin, and `mixed_tasks_…` asserts an
    /// EXACT still-running count: one 10ms task not scheduled inside 200ms on a loaded runner and
    /// the count reads 3 instead of 2. That is precisely what failed on this PR's own CI, and the
    /// PR makes vike-ops' tests run on EVERY Rust change — so a rare flake would have become a
    /// constant one. Blocking on a channel removes the race from the slow side entirely; the fast
    /// side is now an empty closure with a full second to be scheduled.
    fn blocked(rx: std::sync::mpsc::Receiver<()>) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || {
            // Returns Err the moment the test drops the sender, so the thread never outlives it.
            let _ = rx.recv();
        })
    }

    fn noop() -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(|| {})
    }

    // All tasks finish comfortably inside the deadline -> Graceful, and it returns PROMPTLY (as soon
    // as the work is done), nowhere near the deadline.
    #[test]
    fn all_within_deadline_is_graceful_and_prompt() {
        let deadline = Duration::from_millis(200);
        let start = Instant::now();
        let outcome = run_with_deadline(vec![sleepy(10), sleepy(10), sleepy(10)], noop(), deadline);
        let elapsed = start.elapsed();

        assert_eq!(outcome, ShutdownOutcome::Graceful);
        // Returned when the 10ms tasks finished, not when the 200ms deadline forced it.
        assert!(elapsed < Duration::from_millis(150), "should return promptly, took {elapsed:?}");
    }

    // THE regression test: one task is far slower than the deadline. The call must hard-cap at ~the
    // deadline and NOT wait for the slow task (the >6s window-close hang this whole change prevents).
    #[test]
    fn slow_task_is_hard_capped_without_waiting() {
        let deadline = Duration::from_millis(200);
        let start = Instant::now();
        // 2s task vs 200ms deadline — if it waited for the task, elapsed would be ~2s.
        let outcome = run_with_deadline(vec![sleepy(10), sleepy(2000)], noop(), deadline);
        let elapsed = start.elapsed();

        match outcome {
            ShutdownOutcome::HardCapped { still_running, stage } => {
                assert!(still_running >= 1);
                assert_eq!(stage, ShutdownStage::Tasks, "a slow TASK caps in the task stage");
            }
            other => panic!("expected HardCapped, got {other:?}"),
        }
        // Well under the 2000ms slow task: proves it returned at the deadline, not on task completion.
        assert!(
            elapsed < Duration::from_millis(1000),
            "must not wait for the slow task, took {elapsed:?}"
        );
    }

    // A mix of fast and slow tasks -> the still-running count is exactly the number of slow ones.
    #[test]
    fn mixed_tasks_report_correct_still_running_count() {
        // 1s deadline, and the two "slow" tasks block on a channel rather than sleeping: they are
        // still running at the deadline BY CONSTRUCTION, not by out-sleeping it. The two fast tasks
        // are empty closures, so the only timing assumption left is "an empty closure gets scheduled
        // within a second" — see `blocked`'s doc for why the previous 10ms/200ms margin flaked.
        let deadline = Duration::from_millis(1000);
        let (tx_a, rx_a) = std::sync::mpsc::channel();
        let (tx_b, rx_b) = std::sync::mpsc::channel();

        let outcome =
            run_with_deadline(vec![noop(), blocked(rx_a), noop(), blocked(rx_b)], noop(), deadline);
        assert_eq!(
            outcome,
            ShutdownOutcome::HardCapped { still_running: 2, stage: ShutdownStage::Tasks }
        );

        // Release the blocked threads so they never outlive the test.
        drop((tx_a, tx_b));
    }

    /// The load-bearing ordering: the sequential tail runs only AFTER every parallel task has
    /// finished (in vike-app: recorder/recon/materializer/core must never race a still-live feed;
    /// `crates/vike-tradehub/src/tradehub_cli.rs`'s `join_core` states the same reliance in prose).
    ///
    /// ⚠ **A LOG'S ORDER CANNOT PROVE THAT**, and this test used to assert nothing else. Three
    /// zero-duration mutex pushes never create an interleaving in which a concurrent tail would be
    /// observable out of order, so `log.len() == 4` plus `log.last() == "tail"` were satisfied by
    /// mere thread-spawn latency. Measured against a build with the tail started BEFORE the join
    /// loop: the log-only assertions passed 75.4% of the time (66.8% for a variant that
    /// re-propagates the tail panic on join), while every other test in this module stayed green
    /// 20/20 — and nothing else covers the ordering either: every caller a test reaches
    /// (`crates/vike-tradehub/tests/daemon/headless_lifecycle.rs`,
    /// `crates/vike-tradehub/tests/daemon/multi_mount_profile.rs`,
    /// `crates/vike-datahub/src/recorder.rs`) passes `Vec::new()` tasks, so the two call
    /// sites that pass real tasks are exactly the two no test runs.
    ///
    /// So the precondition is asserted INSIDE the tail, against an in-flight counter seeded to the
    /// task count BEFORE the run: only a task's own completion decrements it, so it reads non-zero
    /// at every instant until the last task has returned, and there is no zero for an early tail to
    /// be fooled by. The assertion panics on the orchestration thread, which then drops `done_tx`
    /// without sending — so a concurrent tail surfaces as `Aborted { stage: Tail }` rather than as
    /// a coin flip. (A broken build that SWALLOWS the tail panic fails on the log instead: the
    /// panic precedes the tail's own push, so the "tail" mark is missing.)
    ///
    /// The tasks are held open with `blocked` — see its doc for why the margin here may not be a
    /// sleep — and released only once every one of them has reported entry, so no task can finish
    /// early and hand a racing tail the zero the counter exists to deny it.
    ///
    /// ⚠ This is the RUNS-LAST half of the claim and not the whole of it: the release above is
    /// itself a window, so a broken build whose tail reads the counter strictly after that chain
    /// would still pass here. `crates/vike-ops/src/shutdown.rs`'s
    /// `the_tail_does_not_run_while_any_task_is_still_in_flight` carries the other half — no
    /// release at all, therefore no window — and neither test alone is the guarantee.
    #[test]
    fn tail_runs_after_all_tasks() {
        const TASKS: usize = 3;

        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        // Seeded BEFORE the run, decremented only by a task's own completion. `Relaxed` is sound
        // both ways: the real build's `join` publishes every decrement to the tail, and a stale
        // read on a broken build can only be non-zero — the panic this test wants.
        let in_flight = Arc::new(AtomicUsize::new(TASKS));
        // Entry reports; the releaser below waits for all of them before it unblocks anything.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();

        let mut holds = Vec::with_capacity(TASKS);
        let mut tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            let (hold_tx, hold_rx) = std::sync::mpsc::channel();
            holds.push(hold_tx);
            let hold = blocked(hold_rx);
            let log = Arc::clone(&log);
            let in_flight = Arc::clone(&in_flight);
            let entered = entered_tx.clone();
            let task: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
                let _ = entered.send(());
                hold();
                // Logged at COMPLETION, so the log's order is completion order, not entry order.
                log.lock().unwrap().push("task");
                in_flight.fetch_sub(1, Ordering::Relaxed);
            });
            tasks.push(task);
        }
        // Every surviving sender is now a task's; the releaser's `recv` can therefore see the
        // channel close if a task never runs at all.
        drop(entered_tx);

        let tail_log = Arc::clone(&log);
        let tail_in_flight = Arc::clone(&in_flight);
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            // THE assertion. A tail that runs while any task is still in flight dies here, and the
            // panic is what the caller sees as `Aborted { stage: Tail }`.
            let in_flight_now = tail_in_flight.load(Ordering::Relaxed);
            assert_eq!(in_flight_now, 0, "the tail ran with {in_flight_now} task(s) in flight");
            tail_log.lock().unwrap().push("tail");
        });

        // The release has to come from a THIRD thread: the calling thread is inside
        // `run_with_deadline` for the whole run. Waiting for every entry first is what holds the
        // tasks open across the window in which a broken build would run the tail.
        let releaser = thread::spawn(move || {
            // A disconnect (a task that never ran) returns Err at once, so this cannot hang; the
            // holds drop either way, and no blocked thread outlives the test.
            for _ in 0..TASKS {
                let _ = entered_rx.recv();
            }
            drop(holds);
        });

        // 1s, not 200ms: this asserts ORDERING and the graceful outcome, never latency, so the
        // deadline only has to outlast three task threads, one releaser rendezvous and the tail.
        // On a loaded shared CI runner even that can miss a 200ms budget — observed as
        // `HardCapped { still_running: 1 }` — which is a false failure, not a shutdown bug. The
        // deadline-FIRES tests below keep their tight budgets; those want it to trip.
        let outcome = run_with_deadline(tasks, tail, Duration::from_millis(1000));

        assert_eq!(outcome, ShutdownOutcome::Graceful);
        // The other half of the claim: the tail actually RAN (the assertion inside it is vacuous
        // if it never did), and it ran last.
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4, "3 tasks + tail");
        assert_eq!(log.last(), Some(&"tail"), "tail must run strictly after all tasks");
        let _ = releaser.join();
    }

    /// The half `tail_runs_after_all_tasks` structurally CANNOT carry: the tail does not run early.
    ///
    /// The two are a pair and neither is the whole guarantee.
    /// `crates/vike-ops/src/shutdown.rs`'s `tail_runs_after_all_tasks` carries "the tail RUNS, and
    /// runs last" — but proving that it runs at all means releasing its tasks, and a broken build
    /// whose tail reads the in-flight counter strictly after that release chain can still slip
    /// through it (a rare interleaving, not an impossible one). This test gives up the "runs" half
    /// entirely and buys airtightness with it: the holds are NEVER dropped while the call is
    /// running, so the counter reads `TASKS` at EVERY instant of the run, and there is no
    /// interleaving — none — in which an early tail sees a zero: it panics. Where that panic LANDS
    /// is the one thing this test does not control — `then()` is called INLINE on the orchestration
    /// thread, so it dies with the tail and the outcome is `Aborted { stage: Tail }` instead of the
    /// `HardCapped { still_running: TASKS, stage: ShutdownStage::Tasks }` asserted below. A build
    /// that moved the tail onto its own thread AND swallowed the panic would report the asserted
    /// outcome anyway; the sibling catches THAT shape, because its log then carries no "tail" mark.
    ///
    /// ⚠ The deadline here is a FLOOR, not a margin — the opposite of every other budget in this
    /// module. Nothing can finish, so the asserted outcome carries no timing assumption at all;
    /// the 300ms exists to give a BROKEN build's tail time to be scheduled and reach its
    /// assertion. Shortening it toward zero is what would make this test lie.
    ///
    /// No thread outlives the test, and the ORDER of the last two statements is what makes that
    /// true without weakening the assertion: the senders are dropped only AFTER the flag is read
    /// (the `mixed_tasks_report_correct_still_running_count` idiom, and on the failure path too —
    /// they are locals the unwind drops). Dropping them earlier would BE the release this test
    /// exists not to perform. Freed, the three parked tasks decrement and exit, the orchestration
    /// thread joins them and runs the tail LATE — where the counter really is zero, so it passes
    /// silently rather than panicking into a test that has already returned.
    #[test]
    fn the_tail_does_not_run_while_any_task_is_still_in_flight() {
        const TASKS: usize = 3;

        // Same idiom as the sibling: seeded before the run, decremented only by a task's own
        // completion — except that here nothing can complete while the call is in progress.
        let in_flight = Arc::new(AtomicUsize::new(TASKS));
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut holds = Vec::with_capacity(TASKS);
        let mut tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            let (hold_tx, hold_rx) = std::sync::mpsc::channel();
            holds.push(hold_tx);
            let hold = blocked(hold_rx);
            let in_flight = Arc::clone(&in_flight);
            let task: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
                hold();
                in_flight.fetch_sub(1, Ordering::Relaxed);
            });
            tasks.push(task);
        }

        let tail_in_flight = Arc::clone(&in_flight);
        let tail_ran = Arc::clone(&ran);
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            let in_flight_now = tail_in_flight.load(Ordering::Relaxed);
            assert_eq!(in_flight_now, 0, "the tail ran with {in_flight_now} task(s) in flight");
            tail_ran.store(true, Ordering::Relaxed);
        });

        let outcome = run_with_deadline(tasks, tail, Duration::from_millis(300));

        assert_eq!(
            outcome,
            ShutdownOutcome::HardCapped { still_running: TASKS, stage: ShutdownStage::Tasks }
        );
        // Sound BECAUSE the holds are still held: no task can have completed, so the tail cannot
        // have run — this reads a fact, not a race.
        assert!(!ran.load(Ordering::Relaxed), "the tail ran during the fan-out");

        // Release the blocked threads so they never outlive the test.
        drop(holds);
    }

    // The tail is inside the deadline too: a stuck tail (e.g. a wedged core join) must not hang exit.
    // Tasks all finish (still_running == 0) but the slow tail trips the deadline.
    #[test]
    fn slow_tail_is_also_bounded() {
        let deadline = Duration::from_millis(200);
        let start = Instant::now();
        let slow_tail: Box<dyn FnOnce() + Send + 'static> =
            Box::new(|| thread::sleep(Duration::from_millis(2000)));
        let outcome = run_with_deadline(vec![sleepy(10), sleepy(10)], slow_tail, deadline);
        let elapsed = start.elapsed();

        // ⚠ `still_running: 0` AND `stage: Tail` — together they are readable. Alone, the count
        // said "nothing was outstanding" while the deadline had just elapsed.
        assert_eq!(
            outcome,
            ShutdownOutcome::HardCapped { still_running: 0, stage: ShutdownStage::Tail }
        );
        assert!(
            elapsed < Duration::from_millis(1000),
            "a stuck tail must not hang exit, took {elapsed:?}"
        );
    }

    /// THE Finding C regression: a teardown with NOTHING outstanding finishes WELL inside the
    /// deadline and reports `Graceful` — it does not sit out the budget and then claim a hard cap.
    ///
    /// The live daemon burned its whole `shutdown_deadline_ms` on every stop and printed
    /// "hard-capped … 0 task(s) still in flight". Nothing caught it because no test asserted the
    /// PROMPTNESS of the no-work case against a deadline long enough for the difference to show:
    /// `empty_tasks_runs_tail_and_is_graceful` used a 1 s budget and asserted only the outcome, so
    /// a run that took the full second would still have passed.
    #[test]
    fn a_teardown_with_nothing_outstanding_is_graceful_well_inside_the_deadline() {
        let deadline = Duration::from_secs(10);
        let start = Instant::now();
        let outcome = run_with_deadline(vec![noop(), noop()], noop(), deadline);
        let elapsed = start.elapsed();

        assert_eq!(outcome, ShutdownOutcome::Graceful, "nothing was outstanding");
        assert!(
            elapsed < Duration::from_millis(500),
            "a teardown with no work must not spend the deadline, took {elapsed:?}"
        );
    }

    /// A genuinely-stuck TAIL still hard-caps, and the stage NAMES it — the actionable half of the
    /// message. The count stays `0`, which is correct and is exactly why it cannot be the whole
    /// report.
    #[test]
    fn a_stuck_tail_hard_caps_and_the_stage_names_the_tail() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            let _ = rx.recv();
        });
        let outcome = run_with_deadline(vec![noop()], tail, Duration::from_millis(300));

        match outcome {
            ShutdownOutcome::HardCapped { still_running, stage } => {
                assert_eq!(still_running, 0, "every parallel task DID finish");
                assert_eq!(stage, ShutdownStage::Tail);
                assert!(stage.describe().contains("tail"), "{}", stage.describe());
            }
            other => panic!("expected HardCapped, got {other:?}"),
        }
        drop(tx);
    }

    /// A panicking TAIL is `Aborted`, not `HardCapped`, and it returns at once. Folding the two
    /// together produced a "hard-capped at the 10s deadline" line milliseconds into a broken
    /// teardown.
    #[test]
    fn a_panicking_tail_aborts_promptly_rather_than_reporting_a_hard_cap() {
        let deadline = Duration::from_secs(10);
        let start = Instant::now();
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(|| panic!("tail blew up"));
        let outcome = run_with_deadline(vec![noop()], tail, deadline);
        let elapsed = start.elapsed();

        match outcome {
            ShutdownOutcome::Aborted { stage } => assert_eq!(stage, ShutdownStage::Tail),
            other => panic!("a panicking tail must not read as a deadline cap, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_millis(500),
            "an abort returns at once; it must never claim the deadline, took {elapsed:?}"
        );
    }

    /// A panicking TASK must not inflate `still_running` — the count is decremented by a `Drop`
    /// guard, which runs on the unwind path. With a trailing `fetch_sub` this reported a phantom
    /// straggler and blamed the wrong stage.
    #[test]
    fn a_panicking_task_does_not_inflate_the_still_running_count() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let panicky: Box<dyn FnOnce() + Send + 'static> = Box::new(|| panic!("task blew up"));
        // The tail blocks so the run reaches its deadline and we can read the count.
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            let _ = rx.recv();
        });
        let outcome = run_with_deadline(vec![panicky, noop()], tail, Duration::from_millis(300));

        match outcome {
            ShutdownOutcome::HardCapped { still_running, stage } => {
                assert_eq!(
                    still_running, 0,
                    "a panicking task still finished — it must be counted out"
                );
                assert_eq!(stage, ShutdownStage::Tail);
            }
            other => panic!("expected HardCapped, got {other:?}"),
        }
        drop(tx);
    }

    // An idle app (no live feeds) -> no tasks, the tail still runs, Graceful.
    #[test]
    fn empty_tasks_runs_tail_and_is_graceful() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_tail = Arc::clone(&ran);
        let tail: Box<dyn FnOnce() + Send + 'static> =
            Box::new(move || ran_tail.store(true, Ordering::Relaxed));

        // same reasoning as `tail_runs_after_all_tasks`: a semantics assertion, not a timing one.
        let outcome = run_with_deadline(Vec::new(), tail, Duration::from_millis(1000));

        assert_eq!(outcome, ShutdownOutcome::Graceful);
        assert!(ran.load(Ordering::Relaxed), "the tail must run even with no tasks");
    }
}
