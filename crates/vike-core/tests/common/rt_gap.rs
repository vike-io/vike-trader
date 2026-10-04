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
    assert!(
        matches!(log.first(), Some(Ev::Sleep(d)) if *d >= Duration::from_millis(100)),
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

/// The harness this gate reads, at compile time: it sits one directory up in the same crate's
/// `tests/`, ships wherever this file ships, and a rename breaks the build rather than the gate.
#[cfg(test)]
const HARNESS: &str = include_str!("../runtime_latency.rs");

/// The bounds that make a `for` loop a SAMPLE loop, i.e. the old direct pattern of a measured
/// window: `for i in 0..N`, `0..N as u64`, `0..SNAPSHOT_BUILDS as u32`. Every harness in the file
/// names its sample count through one of these.
#[cfg(test)]
const SAMPLE_COUNTS: [&str; 3] = ["N", "HOP_SAMPLES", "SNAPSHOT_BUILDS"];

/// `src` with every comment and the CONTENTS of every string and char literal blanked to spaces
/// (same length, same newlines), so prose and messages can neither satisfy nor trip a rule. Raw
/// strings are not special-cased: the harness has none, and a lexer that went wrong on one would
/// trip the non-vacuity asserts below rather than pass quietly.
#[cfg(test)]
fn code_only(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, at: usize| {
        if out[at] != b'\n' {
            out[at] = b' ';
        }
    };
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                blank(&mut out, i);
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            while i < b.len() && !b[i..].starts_with(b"*/") {
                blank(&mut out, i);
                i += 1;
            }
            for _ in 0..2 {
                if i < b.len() {
                    blank(&mut out, i);
                    i += 1;
                }
            }
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    blank(&mut out, i);
                    i += 1;
                }
                if i < b.len() {
                    blank(&mut out, i);
                    i += 1;
                }
            }
            i += 1; // the closing quote stays
        } else if b[i] == b'\'' && b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
            out[i + 1] = b' ';
            out[i + 2] = b' ';
            i += 4;
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            out[i + 1] = b' ';
            i += 3;
        } else {
            i += 1;
        }
    }
    // Only ASCII bytes were written, and only over whole comments or literal contents, so no
    // multi-byte character was ever cut in half.
    String::from_utf8(out).expect("blanking keeps the text UTF-8")
}

/// Byte offset to 1-based line number, for the messages.
#[cfg(test)]
fn line_of(code: &str, at: usize) -> usize {
    code[..at].matches('\n').count() + 1
}

/// The byte offset of every `for` loop whose range runs from 0 to a [`SAMPLE_COUNTS`] bound.
#[cfg(test)]
fn sample_loops(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("for ") {
        let at_word_start =
            code[..at].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let Some(brace) = code[at..].find('{') else { continue };
        let head = &code[at..at + brace];
        let Some(range) = head.find(" in ").map(|k| head[k + 4..].trim_start()) else { continue };
        let Some(bound) = range.strip_prefix("0..") else { continue };
        let ident: String =
            bound.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if at_word_start && SAMPLE_COUNTS.contains(&ident.as_str()) {
            out.push(at);
        }
    }
    out
}

/// The span of the body of the function whose signature starts with `sig`, if there is one.
#[cfg(test)]
fn fn_body(code: &str, sig: &str) -> Option<std::ops::Range<usize>> {
    body_from(code, code.find(sig)?)
}

/// The span (`{` to its matching `}`) of the body of the item whose signature starts at `at`.
#[cfg(test)]
fn body_from(code: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let open = at + code[at..].find('{')?;
    let mut depth = 0usize;
    for (k, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open..open + k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every function in `code` whose body opens a window: (offset of its `fn`, its body span).
#[cfg(test)]
fn window_fns(code: &str) -> Vec<(usize, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("fn ") {
        let at_word_start =
            code[..at].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        // A body-less declaration (`fn f();`) has no body of its own to read.
        let has_body = matches!(
            (code[at..].find('{'), code[at..].find(';')),
            (Some(brace), semi) if semi.is_none_or(|semi| brace < semi)
        );
        if !(at_word_start && has_body) {
            continue;
        }
        if let Some(body) =
            body_from(code, at).filter(|b| code[b.clone()].contains(".start_window()"))
        {
            out.push((at, body));
        }
    }
    out
}

/// The text between the `(` at `open` and its matching `)`, if it closes.
#[cfg(test)]
fn call_args(code: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (k, c) in code[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open + 1..open + k]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A character that can sit inside a Rust identifier.
#[cfg(test)]
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The `let` statement around the expression `text[at..end]`, when it reads
/// `let [mut] <name> = <lead><expression>;` with a plain identifier for `<name>`: the name, the
/// lead, and the offset just past the `;`. `lead` is whatever precedes the expression inside
/// the initializer (a receiver, for a method call), trimmed. `None` for any other shape: a bare
/// expression statement, a pattern, a type ascription, or an expression that does not end the
/// statement.
#[cfg(test)]
fn let_around(text: &str, at: usize, end: usize) -> Option<(&str, &str, usize)> {
    let stmt = text[..at].rfind([';', '{', '}']).map_or(0, |k| k + 1);
    let rest = text[stmt..at].trim_start().strip_prefix("let ")?.trim_start();
    let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
    let (name, lead) = rest.split_once('=')?;
    let name = name.trim();
    let after = text[end..].trim_start();
    let semi = text.len() - after.len();
    (!name.is_empty() && name.chars().all(is_ident) && after.starts_with(';')).then_some((
        name,
        lead.trim(),
        semi + 1,
    ))
}

/// The offset of the first `let [mut] <name>` at or after `from` in `text` (the binding is shadowed
/// from there on), or `text.len()` if `name` is never bound again.
#[cfg(test)]
fn rebound_at(text: &str, from: usize, name: &str) -> usize {
    text[from..]
        .match_indices("let ")
        .map(|(k, _)| from + k)
        .find(|&at| {
            let word_start = text[..at].chars().next_back().is_none_or(|c| !is_ident(c));
            let rest = text[at + "let ".len()..].trim_start();
            let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
            let binds = rest
                .strip_prefix(name)
                .is_some_and(|after| after.chars().next().is_none_or(|c| !is_ident(c)));
            word_start && binds
        })
        .unwrap_or(text.len())
}

/// Rule 6 for ONE window, the one whose `.start_window()` sits at `start` in the function body
/// `text`: follow it BY BINDING to a report line. `next_start` is where the function's next window
/// opens (or the body's end), so this window's own `.close_window()` must come before it. `Err`
/// says where the chain breaks.
#[cfg(test)]
fn window_reaches_report(text: &str, start: usize, next_start: usize) -> Result<(), String> {
    const CLOSE: &str = ".close_window()";
    const FROM_HOPS: &str = "HopStats::from_hops";
    let close = text[start..next_start]
        .find(CLOSE)
        .map(|k| start + k)
        .ok_or("is not closed in this function before its next window opens")?;
    // The receiver is not checked: position already pairs this close with this window, and the
    // compiler refuses a close on any `OpenWindow` but the one still open.
    let (window, _, after_close) = let_around(text, close, close + CLOSE.len()).ok_or(
        "is closed outside a `let <window> = <open>.close_window();` statement, so the `Window` \
         it returns cannot be followed",
    )?;
    let window_scope = rebound_at(text, after_close, window);
    let mut why = format!(
        "is bound to `{window}`, which is never handed to `HopStats::from_hops(..)` as its last \
         argument"
    );
    for (k, _) in text[after_close..window_scope].match_indices(&format!("{FROM_HOPS}(")) {
        let call = after_close + k;
        let paren = call + FROM_HOPS.len();
        let Some(args) = call_args(text, paren) else { continue };
        if !args.split_whitespace().collect::<String>().ends_with(&format!(",{window}")) {
            continue;
        }
        let call_end = paren + args.len() + 2; // the `(`, the arguments, the `)`
        let (stats, after_call) = match let_around(text, call, call_end) {
            Some((stats, "", after_call)) if stats != "_" => (stats, after_call),
            Some(("_", "", _)) => {
                why = format!(
                    "hands `{window}` to `HopStats::from_hops(..)` but binds the stats to `_`, \
                     which drops them"
                );
                continue;
            }
            _ => {
                why = format!(
                    "hands `{window}` to `HopStats::from_hops(..)`, but the stats it builds are \
                     not bound by `let <stats> = HopStats::from_hops(..);`, so they are dropped"
                );
                continue;
            }
        };
        let reported = format!("{stats}.report(");
        let stats_scope = rebound_at(text, after_call, stats);
        let called = text[after_call..stats_scope].match_indices(&reported).any(|(k, _)| {
            let at = after_call + k;
            text[..at].chars().next_back().is_none_or(|c| !is_ident(c) && c != '.')
        });
        if called {
            return Ok(());
        }
        why = format!("builds `{stats}` from `{window}`, but `{stats}.report(..)` is never called");
    }
    Err(why)
}

/// Everything wrong with how a harness file opens its windows, one line per violation; empty means
/// clean. Six rules:
///   1. every `RtGap::open(` passes the REAL sleep and the REAL clock, verbatim;
///   2. every function that opens a window opens its gap FIRST: its first statement (after any
///      `const` items) is the `RtGap::open(` call, so the gap precedes all setup — `spawn_core`
///      above all — and hop #0 measures what it measured before the gap existed;
///   3. every `.start_window()` is followed by its `.close_window()` before the next one opens;
///   4. every sample loop lies inside a window, and every window holds exactly ONE sample loop
///      (so a window cannot be a decoy around nothing, nor merge two windows into one long one);
///   5. every `spin_loop()` lies inside a window, or inside the `spin_until` helper that windows
///      call;
///   6. EVERY window a function opens — each one, not only the first — is followed BY BINDING to a
///      report: `let <window> = <open>.close_window();`, that `<window>` handed to
///      `HopStats::from_hops(..)` as the last argument, the result bound by
///      `let <stats> = HopStats::from_hops(..);` (not `_`, not left as a bare statement), and
///      `<stats>.report(..)` called, with neither name rebound in between. A `.report(` on any
///      other value — another window's stats included — does not count. That report line is the
///      ONLY place `busy_ns` (and `window_ns`) reach the persisted series, since nothing asserts
///      `busy`, so a window that was measured and never reported would drop the field silently.
#[cfg(test)]
fn window_violations(src: &str) -> Vec<String> {
    let code = code_only(src);
    let mut bad = Vec::new();

    for (_, body) in window_fns(&code) {
        let text = &code[body.clone()];
        let starts: Vec<usize> = text.match_indices(".start_window()").map(|(k, _)| k).collect();
        for (n, &start) in starts.iter().enumerate() {
            let next_start = starts.get(n + 1).copied().unwrap_or(text.len());
            if let Err(why) = window_reaches_report(text, start, next_start) {
                bad.push(format!(
                    "line {}: the window opened here {why}. Bind its `Window` \
                     (`let <window> = <open>.close_window();`), hand it to \
                     `HopStats::from_hops(..)` as the last argument, bind the result \
                     (`let <stats> = HopStats::from_hops(..);`) and call `<stats>.report(..)`: \
                     that line is the only way this window's `window_ns` and `busy_ns` reach the \
                     persisted series",
                    line_of(&code, body.start + start)
                ));
            }
        }
    }

    for (at, body) in window_fns(&code) {
        // Skip the `{`, then any `const NAME: T = ...;` items, to the first real statement.
        let mut rest = code[body.start + 1..body.end].trim_start();
        while rest.starts_with("const ") {
            rest = rest.split_once(';').map_or("", |(_, after)| after).trim_start();
        }
        let first = rest.split_once(';').map_or(rest, |(stmt, _)| stmt);
        if !first.contains("RtGap::open(") {
            bad.push(format!(
                "line {}: this function opens a window, but its first statement is not the \
                 `RtGap::open(..)` call. The gap must be slept at the TOP of the harness, before \
                 any setup (`spawn_core` above all): slept after it, the core thread finishes \
                 starting during the gap and hop #0 stops measuring what it measured before",
                line_of(&code, at)
            ));
        }
    }

    for (at, _) in code.match_indices("RtGap::open(") {
        let args: String =
            code[at..].chars().take_while(|&c| c != ')').filter(|c| !c.is_whitespace()).collect();
        if args != "RtGap::open(std::thread::sleep,Instant::now" {
            bad.push(format!(
                "line {}: `RtGap::open` must be passed `std::thread::sleep, Instant::now`, the real \
                 sleep and clock; a stand-in here would skip the gap the kernel needs",
                line_of(&code, at)
            ));
        }
    }

    let mut marks: Vec<(usize, bool)> = code
        .match_indices(".start_window()")
        .map(|(at, _)| (at, true))
        .chain(code.match_indices(".close_window()").map(|(at, _)| (at, false)))
        .collect();
    marks.sort_unstable();
    let mut spans = Vec::new();
    let mut opened: Option<usize> = None;
    for (at, is_start) in marks {
        match (opened, is_start) {
            (None, true) => opened = Some(at),
            (Some(start), false) => {
                spans.push(start..at);
                opened = None;
            }
            _ => bad.push(format!(
                "line {}: windows must alternate `.start_window()` / `.close_window()`; this one \
                 does not",
                line_of(&code, at)
            )),
        }
    }
    if let Some(start) = opened {
        bad.push(format!("line {}: a window opened here is never closed", line_of(&code, start)));
    }
    let inside = |at: usize| spans.iter().any(|r| r.contains(&at));

    let loops = sample_loops(&code);
    for &at in &loops {
        if !inside(at) {
            bad.push(format!(
                "line {}: a sample loop runs OUTSIDE a window. Open an `RtGap` at the top of the \
                 harness and put this loop, and nothing else, between `start_window()` and \
                 `close_window()`",
                line_of(&code, at)
            ));
        }
    }
    for span in &spans {
        let n = loops.iter().filter(|&&at| span.contains(&at)).count();
        if n != 1 {
            bad.push(format!(
                "line {}: a window must hold exactly ONE sample loop; this one holds {n}",
                line_of(&code, span.start)
            ));
        }
    }

    let helper = fn_body(&code, "fn spin_until(");
    for (at, _) in code.match_indices("spin_loop()") {
        if !inside(at) && !helper.as_ref().is_some_and(|r| r.contains(&at)) {
            bad.push(format!(
                "line {}: a spin OUTSIDE a window. Spinning is what the kernel's RT budget counts, \
                 so it belongs inside a timed window behind a gap",
                line_of(&code, at)
            ));
        }
    }
    bad
}

/// THE GATE: every measured window in the real harness goes through `RtGap`, so a harness added
/// later cannot escape the gap by copying the old direct loop.
#[test]
fn every_measured_loop_in_the_harness_runs_inside_a_window() {
    let bad = window_violations(HARNESS);
    assert!(
        bad.is_empty(),
        "crates/vike-core/tests/runtime_latency.rs has a measured window that breaks a rule of the \
         RT gap (see crates/vike-core/tests/common/rt_gap.rs's module doc for why each exists):\n{}",
        bad.join("\n")
    );
    // Non-vacuity: a gate whose patterns stopped matching the file would pass on anything.
    let code = code_only(HARNESS);
    assert!(
        !sample_loops(&code).is_empty() && code.contains(".start_window()"),
        "the gate found no sample loop or no window in the harness: its patterns no longer match \
         the file, so it is checking nothing"
    );
    assert!(
        fn_body(&code, "fn spin_until(").is_some(),
        "the `spin_until` helper exemption names a function the harness no longer has"
    );
    assert!(
        !window_fns(&code).is_empty(),
        "the open-the-gap-first rule found no function that opens a window: its `fn` walk no \
         longer matches the file, so that rule is checking nothing"
    );
}

/// A miniature harness shaped like the real one, for the planted-violation tests below.
#[cfg(test)]
const COMPLIANT: &str = r#"
fn spin_until(c: &AtomicU64, target: u64) {
    while c.load(Ordering::Acquire) < target {
        std::hint::spin_loop();
    }
}
fn run(label: &str) {
    const N: usize = HOP_SAMPLES;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let handle = spawn_core(engine, cfg);
    for i in 0..resting {
        spin_until(&submitted, i);
    }
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    let stats = HopStats::from_hops(label, Arc::try_unwrap(hops).unwrap(), window);
    stats.report(label, "");
    // for i in 0..N { std::hint::spin_loop(); } is prose, not code
    let s = "for i in 0..N { std::hint::spin_loop() } .close_window()";
}
"#;

#[test]
fn the_source_gate_passes_a_compliant_harness_and_ignores_prose() {
    assert_eq!(window_violations(COMPLIANT), Vec::<String>::new());
}

/// The old direct pattern: the loop runs on its own and the window is opened around nothing.
#[test]
fn the_source_gate_refuses_a_sample_loop_outside_its_window() {
    let src = COMPLIANT
        .replace("    let spinning = gap.start_window();\n", "")
        .replace("spinning.close_window()", "gap.start_window().close_window()");
    let bad = window_violations(&src);
    assert!(bad.iter().any(|v| v.contains("sample loop runs OUTSIDE")), "{bad:?}");
    assert!(bad.iter().any(|v| v.contains("holds 0")), "{bad:?}");
    assert!(bad.iter().any(|v| v.contains("spin OUTSIDE")), "{bad:?}");
}

#[test]
fn the_source_gate_refuses_a_spin_outside_a_window() {
    let src = COMPLIANT.replace(
        "    let window = spinning.close_window();\n",
        "    let window = spinning.close_window();\n    while busy() { std::hint::spin_loop(); }\n",
    );
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("spin OUTSIDE"), "{bad:?}");
}

/// The hole the types and the other rules left open: the gap slept AFTER `spawn_core`, right before
/// the window. Every window is still behind a gap, so only this rule sees that hop #0 changed.
#[test]
fn the_source_gate_refuses_a_gap_opened_after_setup() {
    let open = "    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);\n";
    let spawn = "    let handle = spawn_core(engine, cfg);\n";
    let src = COMPLIANT.replace(&format!("{open}{spawn}"), &format!("{spawn}{open}"));
    assert_ne!(src, COMPLIANT, "the fixture must carry the open-then-spawn pair this test swaps");
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("first statement"), "{bad:?}");
}

/// A window that is measured and never reported: its `busy_ns` (asserted by nothing) would simply
/// never reach the series. Both halves of rule 6, each on its own.
#[test]
fn the_source_gate_refuses_a_window_that_never_reaches_the_report() {
    let report = "    stats.report(label, \"\");\n";
    let unreported = COMPLIANT.replace(report, "");
    assert_ne!(unreported, COMPLIANT, "the fixture must carry the report call this test deletes");
    let bad = window_violations(&unreported);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");

    let elsewhere = COMPLIANT.replace(".unwrap(), window);", ".unwrap(), stale_window);");
    assert_ne!(elsewhere, COMPLIANT, "the fixture must carry the from_hops call this test edits");
    let bad = window_violations(&elsewhere);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");
}

/// A SECOND window in the same function, compliant on its own: its own gap, one sample loop, its
/// own `Window`, stats and report. [`two_windows`] appends it after the first window's report.
#[cfg(test)]
const SECOND_WINDOW: &str = r#"    let gap2 = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let spinning2 = gap2.start_window();
    for i in 0..N as u64 {
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window2 = spinning2.close_window();
    let stats2 = HopStats::from_hops(label, Arc::try_unwrap(hops2).unwrap(), window2);
    stats2.report(label, "");
"#;

/// The first window's report line in [`COMPLIANT`], which the rule-6 fixtures edit.
#[cfg(test)]
const FIRST_REPORT: &str = "    stats.report(label, \"\");\n";

/// [`COMPLIANT`] with [`SECOND_WINDOW`] after its report: one function, two windows, two reports.
#[cfg(test)]
fn two_windows() -> String {
    assert!(COMPLIANT.contains(FIRST_REPORT), "the fixture must carry the first window's report");
    COMPLIANT.replace(FIRST_REPORT, &format!("{FIRST_REPORT}{SECOND_WINDOW}"))
}

/// The 1-based line of the first `needle` in `src`, to pin WHICH window a violation names.
#[cfg(test)]
fn line_in(src: &str, needle: &str) -> usize {
    line_of(src, src.find(needle).expect("the fixture carries the needle"))
}

/// The control for the rule-6 tests below: two windows in one function, each reported through its
/// own bindings, are clean. A gate that went red here would make a correct harness unwritable.
#[test]
fn the_source_gate_passes_a_function_that_reports_each_of_two_windows() {
    assert_eq!(window_violations(&two_windows()), Vec::<String>::new());
}

/// Rule 6 follows the BINDING to the report: a `.report(` on some other value, or on the OTHER
/// window's stats, does not report this window.
#[test]
fn the_source_gate_refuses_a_report_called_on_an_unrelated_value() {
    let unrelated = COMPLIANT.replace(FIRST_REPORT, "    baseline.report(label, \"\");\n");
    assert_ne!(unrelated, COMPLIANT, "the fixture must carry the report call this test edits");
    let bad = window_violations(&unrelated);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("`stats.report(..)` is never called"), "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");

    // The first window "reported" through the second window's stats: both report lines would be
    // the second window's, and the first window's `busy_ns` would never reach the series.
    let src = two_windows();
    let borrowed = src.replacen(FIRST_REPORT, "    stats2.report(label, \"\");\n", 1);
    assert_ne!(borrowed, src, "the fixture must carry the first window's report");
    let bad = window_violations(&borrowed);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("`stats.report(..)` is never called"), "{bad:?}");
}

/// EVERY window in a function is checked, not only the first: a second window that never reports
/// is red, whether its stats are built and left unreported or it never reaches `from_hops` at all.
/// The violation names the SECOND window's line.
#[test]
fn the_source_gate_checks_every_window_in_a_function_not_only_the_first() {
    let src = two_windows();
    let second = format!("line {}:", line_in(&src, "gap2.start_window()"));

    let unreported = src.replace("    stats2.report(label, \"\");\n", "");
    assert_ne!(unreported, src, "the fixture must carry the second window's report");
    let bad = window_violations(&unreported);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&second), "the violation must name the second window: {bad:?}");
    assert!(bad[0].contains("`stats2.report(..)` is never called"), "{bad:?}");

    let stats2 =
        "    let stats2 = HopStats::from_hops(label, Arc::try_unwrap(hops2).unwrap(), window2);\n";
    let never_handed = unreported.replace(stats2, "");
    assert_ne!(never_handed, unreported, "the fixture must carry the second window's from_hops");
    let bad = window_violations(&never_handed);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&second), "the violation must name the second window: {bad:?}");
    assert!(bad[0].contains("`window2`, which is never handed"), "{bad:?}");
}

/// A window whose stats are built and then DROPPED — bound to `_`, or never bound at all — is red
/// even when another window in the same function supplies a `.report(` for a text match to find.
#[test]
fn the_source_gate_refuses_a_window_whose_stats_are_dropped() {
    let src = two_windows();
    let first = format!("line {}:", line_in(&src, "gap.start_window()"));
    let bound = "    let stats = HopStats::from_hops(";
    assert!(src.contains(bound), "the fixture must carry the first window's from_hops binding");
    let unreported = src.replace(FIRST_REPORT, "");

    let to_underscore = unreported.replace(bound, "    let _ = HopStats::from_hops(");
    let bad = window_violations(&to_underscore);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&first), "the violation must name the first window: {bad:?}");
    assert!(bad[0].contains("binds the stats to `_`"), "{bad:?}");

    let unbound = unreported.replace(bound, "    HopStats::from_hops(");
    let bad = window_violations(&unbound);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&first), "the violation must name the first window: {bad:?}");
    assert!(bad[0].contains("not bound"), "{bad:?}");
}

#[test]
fn the_source_gate_refuses_a_gap_opened_with_a_stand_in_sleep() {
    let src = COMPLIANT.replace("std::thread::sleep", "|_| {}");
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("the real"), "{bad:?}");
}
