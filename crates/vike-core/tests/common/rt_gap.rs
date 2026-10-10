//! THE RT-THROTTLE GAP — the one door every measured window in
//! `crates/vike-core/tests/runtime_latency.rs` goes through, and the kernel budget it keeps that
//! harness under.
//!
//! # The defect
//!
//! CI runs the harness under `chrt -f 50` (`.github/workflows/ci.yml`'s latency step), so its
//! threads are SCHED_FIFO: the busy-spinning sender, and the `vt-core` worker and journal syncer it
//! spawns, which inherit the policy. the latency box's kernel (6.8, no `CONFIG_RT_GROUP_SCHED`, so the global
//! budget applies per CPU) lets RT tasks run [`KERNEL_RT_RUNTIME_MS`] of every
//! [`KERNEL_RT_PERIOD_MS`] (`kernel.sched_rt_runtime_us` / `kernel.sched_rt_period_us`). A CPU that
//! has run RT for longer than that inside one period is THROTTLED: its RT tasks stop until the
//! period ends, a stall of up to 50 ms. The harness used to spin back to back from its first window
//! to its last (~4 s), so the throttle fired about once per attempt and, roughly one time in eight,
//! landed on a measured hop: a 46-50 ms `max_ns` with exactly one `hops_over_100us` and p50, p99 and
//! p99.9 untouched. From 2026-09-01 to 2026-10-03 that was 72 of 712 attempts, 13 of them in a GATED
//! variant: attempt 1 red, absorbed by the 3x retry at 3-10 minutes of the `latency-gate` group each
//! time. What ties it to the throttle rather than to load: no stall above 50.13 ms anywhere in the
//! persisted series, 1 ms quantization with a sub-millisecond phase shared by every stall of an
//! epoch, no stall earlier than 0.94 s into the binary (the throttle cannot fire before 0.95 s of
//! spinning), and two stalls in one attempt always a whole number of seconds apart.
//!
//! # The fix: a gap, not a knob
//!
//! Before every window the measuring thread SLEEPS [`RT_GAP`], a real `nanosleep`. The previous
//! window's worker and syncer were joined before it, so while it sleeps the harness has no runnable
//! RT thread at all and every CPU it used gets that much non-RT time. `sched_yield` would not do (a
//! lone FIFO task's yield returns at once), and neither would a microsecond nap: the kernel needs
//! 50 ms of non-RT time per period. The kernel's budget itself is left alone; it is what kept the latency box
//! alive when a cancelled run orphaned a FIFO spinner. Only the harness stops walking into it.
//!
//! Nothing measured changes. The gap sits outside every window, and it is slept at the TOP of each
//! harness, before its setup, so the sequence from `spawn_core` to the first hop (and with it hop
//! #0, which is the max of most variants) is byte-identical to before the gap existed. No sample is
//! dropped, no ceiling moves, and a real stall inside a window still counts.
//!
//! # The two numbers, both derived from the kernel's pair
//!
//! Call the RT-busy stretch between two gaps `B` (setup + window + teardown). At every period
//! boundary the kernel takes the budget off a CPU's accumulated RT time, so at most the throttle's
//! own detection overshoot (one 1 ms tick) carries into the next period, and the RT time inside any
//! one period is at most `max(B, period - gap)`. The throttle needs that to exceed the budget, so
//! it fires only if `gap < period - budget` (50 ms) or `B > budget` (950 ms). Each constant keeps
//! the same 50 ms inside its edge:
//!
//!   * [`RT_GAP`] = 2 x (period - budget) = 100 ms. The margin covers RT work that still lands on a
//!     CPU during the gap (kernel RT threads, a join that completes late) and the tick the kernel
//!     accounts in.
//!   * [`RT_WINDOW_MAX`] = budget - (period - budget) = 900 ms. The margin covers the part of `B`
//!     the window does not time: spawn, journal open and close, the 100 000-sample sort, the report.
//!
//! ⚠ **Both margins are arguments in RT CPU TIME** — what the kernel's budget counts — while
//! everything here can only read a WALL clock, and for the journal variants the two are far
//! apart. Measured (below), the `journal` harness spends 0.33-0.73 s of wall time OUTSIDE its loop,
//! nowhere near 50 ms: the journal's open (`posix_fallocate` of a 256 MiB segment, then pre-faulting
//! a 64 MiB window of it) and its close (the dropping journal's `msync`). Much of that is I/O wait,
//! which the RT budget does not count, and that is what keeps the argument standing; how much of it
//! is CPU, and how it splits between open and close, nobody has measured. [`Window::busy`] is
//! recorded to settle the SECOND question and cannot settle the first: it is a wall clock too, so
//! it splits that wall time between the open and the close but counts an I/O wait exactly as it
//! counts a spin. The CPU share needs per-thread on-CPU time (`/proc/thread-self/schedstat`
//! deltas, or kernel tracing), which nothing here reads.
//!
//! Cost: one gap per window, 15 windows today, so ~1.5 s on a ~4 s binary.
//!
//! # The guard, and which windows it fails
//!
//! A gap protects only a window that fits the budget after it. A GATED window (`baseline`,
//! `journal`, `journal-snap`) that spins longer than [`RT_WINDOW_MAX`] fails its test LOUD through
//! [`gated_window_fits`], which `crates/vike-core/tests/runtime_latency.rs`'s `assert_hop_budget`
//! calls before any ceiling, so a change that lengthens a gated window cannot walk back into the
//! throttle silently. It judges the LOOP alone ([`Window::spun`], `window_ns=`).
//!
//! MEASURED from job-log timestamps over 699 attempts (2026-09-03 to 2026-10-04, the latency job's
//! log for every `journal` row in the persisted series): from the start of each gated test to its
//! report line, gap excluded, i.e. setup + loop + teardown, `baseline` / `journal` /
//! `journal-snap` ran at most 0.16 / 0.94 / 0.51 s. Their loops alone ran at most about 0.06 /
//! 0.25 / 0.3 s (exact where an attempt carries `window_ns`, p50 x N x 1.15 elsewhere), so the guard
//! does not fire today. `journal`'s 0.94 s is ONE contended attempt (run 36804340336); in each
//! attempt that carries `window_ns` its setup + teardown came to 0.33-0.43 s around a 0.11-0.20 s
//! loop. (This paragraph said "at most 0.06 / 0.73 / 0.42 s with their setup included" from 13
//! attempts; 699 moved the `journal` figure by 0.2 s.)
//!
//! # `busy_ns`: a MEASUREMENT, not a guard
//!
//! Every report line also carries `busy_ns=` ([`Window::busy`]): the wall time from the RETURN of
//! [`RtGap::open`] — the sleep is over and not in it — to [`OpenWindow::close_window`], i.e. the
//! harness's setup plus its loop. It is recorded, in the persisted series like every other `k=v`,
//! and NOTHING asserts it. The guard stays on the loop because a wall clock cannot tell the journal
//! open's I/O wait from spinning, so a wall-clock guard over setup + loop would invent reds on a
//! contended box that the kernel's budget never sees.
//!
//! What it is for: after ~150 attempts, `busy_ns - window_ns` is each harness's setup, and with
//! the teardown that is left (test start to report, minus the gap, minus `busy_ns`) it settles how
//! the journal's 0.33-0.73 s outside the loop splits between the open and the close, in WALL time.
//! It does not settle how much of either is CPU, the quantity the RT budget counts: that is the
//! same wall-clock blindness that keeps the guard off it. THE DECISION RULE, written down so it is
//! not re-derived: move the guard to judge [`Window::busy`] instead of [`Window::spun`] ONLY if
//! the worst `busy_ns` of every gated variant stays well under 800 ms over those ~150 attempts. If
//! it does not, the guard stays on the loop, and bounding the setup becomes a design question (a
//! smaller warm window for the 256 MiB variant, or reading per-thread CPU time instead of a wall
//! clock), not a constant to nudge.
//!
//! Measure-only windows are recorded (`window_ns=` and `busy_ns=` on their report line, and so in
//! the persisted series) and never failed, because they assert no timing. ⚠ One of them already
//! exceeds the limit: `snapshot-build-512ord` spins ~0.6-1.2 s, so ITS `max_ns` can still carry a
//! ~50 ms throttle stall. That is an artefact on an unasserted series, not a gate defect; splitting
//! it behind a second gap would change its sample shape, which this change deliberately does not do.
//!
//! # Structural, not remembered
//!
//! A [`Window`], the only window `HopStats::from_hops` accepts, comes only from
//! [`OpenWindow::close_window`]; an [`OpenWindow`] only from [`RtGap::start_window`], which
//! consumes the gap; and an [`RtGap`] only from [`RtGap::open`], after it has slept. So no report
//! line can exist without a gap before its window. What the types cannot see (WHICH code runs
//! inside the window, whether the sleep passed in is the real one, whether the gap was slept
//! BEFORE the setup rather than just before the window, and whether EACH window a harness opens
//! reaches a report through its own bindings, which is the only way its `busy_ns` reaches the
//! series) is [`every_measured_loop_in_the_harness_runs_inside_a_window`]'s job: it reads the
//! harness source.
//!
//! # Deliberately vike-free
//!
//! The same arrangement as `crates/vike-core/tests/common/latency_line.rs`: std only, so this file
//! also compiles and runs on its own (`rustc --edition 2024 --test` on it).

use std::assert_matches;
use std::time::{Duration, Instant};

/// the latency box's `kernel.sched_rt_period_us` (1 000 000), in ms: the period the RT budget is counted over.
const KERNEL_RT_PERIOD_MS: u64 = 1_000;

/// the latency box's `kernel.sched_rt_runtime_us` (950 000), in ms: the RT run time one CPU may spend inside
/// one period before the kernel throttles every RT task on it until the period ends.
const KERNEL_RT_RUNTIME_MS: u64 = 950;

/// The non-RT time per period the throttle needs to stay quiet: 50 ms.
const KERNEL_RT_SLACK_MS: u64 = KERNEL_RT_PERIOD_MS - KERNEL_RT_RUNTIME_MS;

/// The sleep before every window: TWICE the slack, i.e. 100 ms. See the module doc for why a
/// period then holds at most `period - gap` = 900 ms of RT time around a gap.
const RT_GAP: Duration = Duration::from_millis(2 * KERNEL_RT_SLACK_MS);

/// The longest a GATED window may spin: the budget minus the slack once more, i.e. 900 ms, leaving
/// 50 ms of the budget for the setup and teardown the window does not time. Derived, not measured:
/// raising it hands the window back to the throttle.
const RT_WINDOW_MAX: Duration = Duration::from_millis(KERNEL_RT_RUNTIME_MS - KERNEL_RT_SLACK_MS);

/// Proof that the gap was slept. Made only by [`RtGap::open`]; spent by [`RtGap::start_window`].
#[must_use = "a gap is slept for ONE window: open that window with `start_window`"]
pub struct RtGap<C> {
    clock: C,
    /// When the gap ENDED: read after the sleep returned, so [`Window::busy`] counts all of the
    /// setup that follows and none of the sleep.
    opened: Instant,
}

impl<C: Fn() -> Instant> RtGap<C> {
    /// Sleep [`RT_GAP`] through `sleep`, then return the right to time ONE window with `clock`.
    ///
    /// The harness passes `std::thread::sleep` and `Instant::now`, and
    /// [`every_measured_loop_in_the_harness_runs_inside_a_window`] fails if a call site passes
    /// anything else. The parameters exist so the tests below can prove the ORDER and the
    /// ACCOUNTING with a fake clock instead of a real 100 ms.
    pub fn open(sleep: impl FnOnce(Duration), clock: C) -> Self {
        sleep(RT_GAP);
        let opened = clock();
        Self { clock, opened }
    }

    /// Start timing the window: everything between this and [`OpenWindow::close_window`] is the
    /// spinning the kernel counts, and the gap is already behind it.
    pub fn start_window(self) -> OpenWindow<C> {
        let started = (self.clock)();
        OpenWindow { clock: self.clock, opened: self.opened, started }
    }
}

/// A window being timed. Made only by [`RtGap::start_window`].
#[must_use = "close the window with `close_window` to get the `Window` the report needs"]
pub struct OpenWindow<C> {
    clock: C,
    opened: Instant,
    started: Instant,
}

impl<C: Fn() -> Instant> OpenWindow<C> {
    /// Stop timing. The result carries the window alone (the gap is not in it) and, beside it, the
    /// busy stretch from the gap's end (the setup is in it, the gap is not).
    pub fn close_window(self) -> Window {
        let closed = (self.clock)();
        Window {
            spun: closed.saturating_duration_since(self.started),
            busy: closed.saturating_duration_since(self.opened),
        }
    }
}

/// The wall time one measured window spun, and the busy stretch around it. Constructible only by
/// [`OpenWindow::close_window`].
#[derive(Debug, Clone, Copy)]
pub struct Window {
    spun: Duration,
    busy: Duration,
}

impl Window {
    /// How long the window spun: the sample loop alone. `window_ns=`, and what the guard judges.
    pub fn spun(self) -> Duration {
        self.spun
    }

    /// From the END of the gap to the end of the loop: setup plus loop, never the sleep. `busy_ns=`,
    /// RECORDED and never asserted — see the module doc's `busy_ns` section for why, and for the
    /// rule that would make it a guard.
    pub fn busy(self) -> Duration {
        self.busy
    }
}

/// The guard for a GATED window: `Err` (with the message the test panics on) if it spun longer
/// than [`RT_WINDOW_MAX`], `Ok` at or under it.
pub fn gated_window_fits(label: &str, spun: Duration) -> Result<(), String> {
    if spun <= RT_WINDOW_MAX {
        return Ok(());
    }
    Err(format!(
        "[{label}] RT-throttle precondition: this GATED window spun {spun:?}, over the \
         {RT_WINDOW_MAX:?} RT_WINDOW_MAX in crates/vike-core/tests/common/rt_gap.rs. Past it the \
         kernel's RT throttle ({KERNEL_RT_RUNTIME_MS} ms of SCHED_FIFO per {KERNEL_RT_PERIOD_MS} ms \
         period on the latency box) can stop the measuring threads for up to {KERNEL_RT_SLACK_MS} ms INSIDE \
         the window, so its max and its hop count stop measuring the code. Read p50/p99 on this \
         variant's report line first: if they rose, the hops themselves got slower and that is what \
         lengthened the window. Do NOT raise the limit (it is derived from the kernel's budget, not \
         measured): shorten the window, or split it behind a second gap."
    ))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The gate's own tests. NOT `#[ignore]`d: pure and instant, they ride the FAST CI lane, while the
// the latency box latency job runs the binary with `--ignored` and so skips every one of them.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// What the fake sleep, the fake clock and the fake spin did, in order.
#[cfg(test)]
#[derive(Debug, PartialEq)]
enum Ev {
    Sleep(Duration),
    Clock,
    Setup(Duration),
    Spin(Duration),
}

/// A deterministic stand-in for the sleep and the clock. Sleeping ADVANCES the clock, exactly as a
/// real sleep would, so a window that timed the gap would show it.
#[cfg(test)]
struct FakeTime {
    base: Instant,
    now: std::cell::Cell<Duration>,
    log: std::cell::RefCell<Vec<Ev>>,
}

#[cfg(test)]
impl FakeTime {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            now: std::cell::Cell::new(Duration::ZERO),
            log: std::cell::RefCell::new(Vec::new()),
        }
    }
    fn sleep(&self, d: Duration) {
        self.log.borrow_mut().push(Ev::Sleep(d));
        self.now.set(self.now.get() + d);
    }
    fn clock(&self) -> Instant {
        self.log.borrow_mut().push(Ev::Clock);
        self.base + self.now.get()
    }
    fn spin(&self, d: Duration) {
        self.log.borrow_mut().push(Ev::Spin(d));
        self.now.set(self.now.get() + d);
    }
    /// The harness's setup between the gap and the window (spawn, journal open), in fake time.
    fn setup(&self, d: Duration) {
        self.log.borrow_mut().push(Ev::Setup(d));
        self.now.set(self.now.get() + d);
    }
}

/// One harness through the REAL `RtGap` code path, with `t`'s fake sleep and clock: the gap,
/// `setup` of work before the window opens, then a window spinning `spun`.
#[cfg(test)]
fn fake_window(t: &FakeTime, setup: Duration, spun: Duration) -> Window {
    let gap = RtGap::open(|d| t.sleep(d), || t.clock());
    t.setup(setup);
    let open = gap.start_window();
    t.spin(spun);
    open.close_window()
}

/// The order and the size: a sleep of at least 100 ms, THEN the clock read that ends the gap, the
/// setup, the window's first clock read, the spin, the last read. The 100 ms is a literal on
/// purpose, so the test cannot follow a mutated [`RT_GAP`] down.
#[test]
fn the_gap_sleeps_at_least_100ms_before_the_window_opens() {
    let t = FakeTime::new();
    let _ = fake_window(&t, Duration::from_millis(250), Duration::from_millis(300));
    let log = t.log.borrow();
    assert_matches!(
        log.first(), Some(Ev::Sleep(d)) if *d >= Duration::from_millis(100),
        "the window must open only after a sleep of at least 100 ms; the sequence was {log:?}"
    );
    assert_eq!(
        log[1..],
        [
            Ev::Clock,
            Ev::Setup(Duration::from_millis(250)),
            Ev::Clock,
            Ev::Spin(Duration::from_millis(300)),
            Ev::Clock
        ],
        "after the sleep: the clock read that ends the gap, the setup, the window's clock read, \
         the spin, one clock read, and nothing else"
    );
}

/// The accounting: the window is the spin and only the spin, even though the (fake) clock moved by
/// the gap and the setup as well. `window_ns=` means exactly what it meant before `busy` existed.
#[test]
fn the_window_times_the_spin_and_not_the_gap() {
    let t = FakeTime::new();
    let w = fake_window(&t, Duration::from_millis(250), Duration::from_millis(300));
    assert_eq!(w.spun(), Duration::from_millis(300));
    assert!(
        t.now.get() >= Duration::from_millis(650),
        "the fake clock must have advanced by the sleep and the setup too, or this proves nothing"
    );
}

/// `busy` runs from the RETURN of `RtGap::open` to `close_window`: the setup between the gap and
/// the window is in it, exactly. Literals, so the arithmetic cannot follow a mutation down.
#[test]
fn busy_counts_the_setup_between_the_gap_and_the_window() {
    let t = FakeTime::new();
    let w = fake_window(&t, Duration::from_millis(250), Duration::from_millis(300));
    assert_eq!(
        w.busy(),
        Duration::from_millis(550),
        "busy must be setup (250 ms) + loop (300 ms), from the gap's end to the window's close"
    );
}

/// ...and the sleep is NOT in it: with no setup, busy is the loop alone although the clock moved by
/// the whole gap first. A clock read taken before the sleep would make this 400 ms.
#[test]
fn busy_never_counts_the_sleep() {
    let t = FakeTime::new();
    let w = fake_window(&t, Duration::ZERO, Duration::from_millis(300));
    assert_eq!(w.busy(), Duration::from_millis(300), "the gap's sleep leaked into busy");
    assert!(
        t.now.get() >= Duration::from_millis(400),
        "the fake clock must have advanced by the sleep, or this test proves nothing"
    );
}

/// A gated window one nanosecond over 900 ms is red. The 900 ms is a literal, so a raised
/// [`RT_WINDOW_MAX`] turns this test red instead of carrying it along.
#[test]
fn a_gated_window_one_nanosecond_over_900ms_fails() {
    let t = FakeTime::new();
    let w = fake_window(&t, Duration::ZERO, Duration::from_millis(900) + Duration::from_nanos(1));
    let err = gated_window_fits("unit-test", w.spun()).unwrap_err();
    assert!(err.contains("RT-throttle"), "the message must name the cause: {err}");
}

/// ...and a gated window of exactly 900 ms passes, so a LOWERED limit is caught as well.
#[test]
fn a_gated_window_of_exactly_900ms_passes() {
    let t = FakeTime::new();
    let w = fake_window(&t, Duration::ZERO, Duration::from_millis(900));
    assert_eq!(gated_window_fits("unit-test", w.spun()), Ok(()));
}

// ── The source gate: what the types cannot see ───────────────────────────────────────────────

#[path = "rt_gap/source_gate.rs"]
#[cfg(test)]
mod source_gate;
