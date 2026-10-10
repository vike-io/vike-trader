//! R5 latency harness — the plan's "p99 core-hop < 10 µs" gate, plus the snapshot-build
//! cost measured separately (provably off the per-event path).
//!
//! The three gate tests — `baseline`, `journal` (append cost isolated, no cadence snapshot) and
//! `journal-snap` (the production journal config, snapshots included) — assert the whole TAIL, not
//! just p99. See [`P99_BUDGET_NS`] and the block of budgets around it for why, and
//! `docs/ops/latency-gate-calibration.md` for the reference measurements every ceiling is derived
//! from.
//!
//! ⚠ The two journal variants no longer share the baseline's 10 µs p99 bar (their test NAMES still
//! say `under_10us`, and are kept only so a CI log greps the same across the change): that literal
//! was never derived for them and `main` itself measured 9 809 ns against it. They gate on
//! [`JOURNAL_P99_NS`] — read THE 2026-08-05 CALIBRATION before touching either:
//! `docs/ops/latency-gate-calibration.md`'s "The 2026-08-05 calibration".
//!
//! Run manually in release (timing-sensitive, skipped in CI's debug run):
//!     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture
//!
//! # GATES vs MEASUREMENTS — this file holds both, and only three of them are gates
//!
//! The three tests named above ASSERT ceilings. Everything else here MEASURES and asserts nothing
//! about timing, and that split is deliberate rather than unfinished work. Calibrating a ceiling
//! honestly on this box is expensive — [`JOURNAL_P99_NS`] took 98 CI attempts to derive, and
//! `docs/ops/latency-gate-calibration.md` is what that cost bought — while an UNCALIBRATED ceiling
//! is a flaky gate, which is worse than no gate at all because people learn to re-run it rather
//! than read it. A measured, persisted, trendable series catches drift for an afternoon's work and
//! leaves the ceiling as a later decision, taken from a baseline instead of from a guess. **Adding
//! a measurement variant here does not mean a budget is coming; adding a ceiling means somebody
//! derived one.**
//!
//! ## Which unprotected paths are measured — and the one that deliberately is NOT
//!
//! Every gate here feeds `Ingest::Event(Event::Fill)`, the venue-REPLY lane, so the rest of what
//! the core does had no series at all. Three unprotected paths were named as candidates. TWO are
//! measured and the THIRD was dropped on the merits — recorded here rather than left in a PR body,
//! because whoever asks "was that path ever scored?" reads this file and not the merge log:
//!
//!   * **the order-submit lane** — [`run_submit_hop`], variants `submit-hop-0resting` /
//!     `submit-hop-32resting`;
//!   * **the snapshot publish** — [`run_snapshot_build`], variants `snapshot-build-0ord` /
//!     `-64ord` / `-512ord`;
//!   * **the recorder flush — NOT measured, and NOT pending.** Two reasons, neither of them
//!     scheduling. (1) It does not belong in THIS binary: a `RecorderSink` flush is a
//!     Parquet/DataFusion write on its own writer thread, disk-bound and milliseconds wide, and
//!     this binary runs under `chrt -f 50` on four reserved cores of the box that also carries
//!     ClickHouse and the live recorders — spending that fence on I/O the fold never performs buys
//!     a number at the cost of the quiet the other eight harnesses depend on. (2) A percentile is
//!     the wrong instrument for it anyway: the failure that actually cost ~110k rows was the
//!     maintenance compactor holding the series lock while the sink DISCARDED batches, and that
//!     shows up as a loss COUNTER — `crates/vike-data/src/rec/live_rec.rs`'s `RecorderHandle`, whose
//!     `dropped` (rows refused at a full channel) and disjoint `discarded` (rows lost writing) are
//!     the two numbers that move — not as a slow flush. A latency series over it would read like
//!     coverage of a hazard it structurally cannot see. If it is ever measured, it belongs next to
//!     the store in `vike-data`, not in a binary that runs on the reserved cores.
//!
//! A fourth series, added for a different question (what does the MOUNT COUNT cost the core through
//! the publish?): [`publish_hop_n`] (labels `publish-hop-n<N>`, one whole publish per sample) and
//! [`publish_burst_n`] (`publish-burst-n<N>`, a 64-message burst, plus the `-quiet` controls that
//! publish nothing), for `N` = 0, 1, 20, 50, 100, 200 mounts. It carries NO ceiling, like every
//! measurement here, and is the instrument for any change to `mount_views` or `build`. Run it on a
//! quiet box in release with `--ignored --test-threads=1 --nocapture --exact publish_hop_n` (or
//! `--exact publish_burst_n`). The idle publish cannot be suppressed without a production change
//! (`run_publish_burst` says why), so the `-quiet` controls send books nothing hears instead.
//!
//! The plain `publish-hop-n<N>` labels move nothing between publishes, so they measure the
//! ALL-UNCHANGED best case of anything that reuses work across publishes. `publish-hop-n<N>-moving`
//! (`N` = 20, 100, 200) is the opposite shape: the LAST mount holds an open position on the engine's
//! own symbol and the harness alternates two books with different mids, so every message moves that
//! position's `unrealized_pnl` and `notional` and no publish can reuse its mount rows. It measures the
//! REBUILD path (compare up to the moved row, the full `mount_views` build, the copy into the
//! snapshot) plus one mount's tick dispatch per message, and is read against itself across `N` and
//! against the plain label of the same `N`.
//!
//! ## The persistence contract, and why a new variant needs no workflow change
//!
//! [`HopStats::report`] writes ONE `LATENCY-GATE` line per variant to the stderr HANDLE.
//! `.github/workflows/ci.yml`'s latency step parses every such line out of each attempt's log and
//! appends one JSON row per (run, attempt, variant) to `/mnt/ci/the CI user/.vike-ci-metrics/latency-series.jsonl` on
//! the latency box — on pass and fail alike — carrying every `k=v` after the marker VERBATIM. So a harness
//! that prints the line is IN the durable series the moment it exists, with the run/commit/box
//! metadata and the attempt's denormalized `baseline_p99_ns` already attached. PROVEN, not assumed:
//! on the day the submit and snapshot variants below were added, that file held exactly the ten
//! labels this file emitted BEFORE them, 172 attempts each. (Read "ten" as of that day and not as a
//! count of what is emitted now — those five new labels are five more, and nothing keeps a number
//! in this paragraph equal to the `report(` call sites below.)
//!
//! Two traps that come with riding that contract, both GATED rather than commented — the mechanism
//! is `crates/vike-core/tests/common/latency_line.rs`'s `check_report_line`:
//!   * a series row records `variant` and NOT the test that emitted it, so two harnesses sharing a
//!     label merge two distributions into one trend line with nothing to separate them afterwards;
//!   * a label carrying whitespace does not produce a MANGLED row — it produces a plausible row
//!     filed under a truncated variant name, silently.
//!
//! And two that belong to whoever READS the series (`ci.yml`'s own query block is the authority): a
//! `pull_request` row's `sha` is GitHub's ephemeral merge commit, so the code trend filters
//! `event=="push" and branch=="main"`; and a row whose `pin` differs from its `pin_requested` was
//! measured on the wrong cores.
//!
//! ## The trap a new harness here will hit first
//!
//! **Never let the engine GROW across a measured loop.** The runtime publishes whenever the core is
//! about to go idle (`crates/vike-core/src/runtime/run_loop.rs`'s `publish_guarded`, reached under
//! `if self.dirty` immediately before `blocking_recv`), and this file's request/response pacing
//! makes the core idle between every single message — so `snapshot::build` runs once per
//! message, and it walks the whole order registry. A harness that submits N orders under N distinct
//! client-order-ids therefore costs O(N²), and its tail measures `IndexMap` growth rather than the
//! lane it claims to. [`run_submit_hop`] pins the registry by REUSING one coid for exactly that
//! reason.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, CoreSnapshot, ExecutionClient, ExecutionEngine, Ingest,
    OrderIntent, PriceCfg, ReconBlock, RiskGate,
};
use vike_model::OrderRequest;
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent};

/// The `LATENCY-GATE` line's own format gate — see that module's doc for what silently breaks
/// without it. `#[path]`, because cargo auto-discovers test binaries from `tests/*.rs` and
/// `tests/*/main.rs` only, so a file one directory down is a plain module rather than a second
/// binary (`crates/bridges/ctrader/tests/` is the in-tree precedent for the layout).
#[path = "common/latency_line.rs"]
mod latency_line;

/// The RT-throttle gap every measured window here opens behind, and the guard that fails a GATED
/// window too long to fit the kernel's RT budget. Read that module's doc before adding a harness
/// or touching the `rt_gap::` lines below: its source gate fails a sample loop that runs outside
/// a window, and a harness whose first statement is not its `RtGap::open`.
#[path = "common/rt_gap.rs"]
mod rt_gap;

/// Hops measured per variant. Every percentile and every budget below is stated against THIS
/// sample size — `hops>100µs = 2` means 2 in 100 000, and p99.9 means "the 100th worst hop".
const HOP_SAMPLES: usize = 100_000;

/// The EMPTY coid the two gate harnesses use. `String::new()` never allocates, so the gate has
/// always measured a hop with NO client-order-id allocation in it — which is exactly why it cannot
/// score the `client_order_id: String -> CompactString` idea (perf audit 2026-07-28, finding #4).
/// See [`coid_alloc_cost_baseline`].
const NO_COID: &str = "";

/// A realistically-shaped LIVE coid: the `ClientOrderIdGenerator` wire form
/// (`<8-hex session><decimal seq>`). 13 bytes — one heap allocation as a `String`, and INSIDE
/// `CompactString`'s 24-byte inline budget.
const LIVE_COID: &str = "deadbeef12345";

// ─────────────────────────────────────────────────────────────────────────────────────────────
// TAIL BUDGETS. Every ceiling below is DERIVED from measurements, and the history that derived
// them — the reference runs, the 2026-08-01 re-measurement (#932), the SIZING RULE, the 2026-08-05
// calibration and its harvest command, the tripwires — lives in
// `docs/ops/latency-gate-calibration.md`, moved there verbatim from this file. Each constant keeps
// a short note and points at its own section there: re-derive a ceiling from a fresh harvest,
// never nudge its literal.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The plan gate itself, unchanged since R5: p99 core-hop < 10 µs — **for the BASELINE variant
/// only** since the 2026-08-05 calibration, which gave the journal variants [`JOURNAL_P99_NS`]. An
/// R5 plan literal rather than a derived ceiling, kept because on the journal-free path it sits
/// 8-30x over what the baseline measures in CI; tightening it is a separate question. Re-derive
/// from a fresh harvest rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`P99_BUDGET_NS`".
const P99_BUDGET_NS: u64 = 10_000;

/// The journal variants' p99 ceiling — **2.0x the worst GREEN journal-variant p99** (9 809 ns) of
/// the 2026-08-05 calibration (98 CI attempts, 80 of them green), rounded up to 20 000. Sized from
/// green observations only, like the three `JOURNAL_*` ratchets below. ⚠ TRIPWIRE: if a GREEN run
/// reports a journal-variant p99 above ~12 000 ns, the noise floor has moved — re-run the harvest
/// and re-derive from the new green distribution rather than nudging this literal. Derivation,
/// harvest command, and what this ceiling gives up:
/// `docs/ops/latency-gate-calibration.md`'s "`JOURNAL_P99_NS`".
const JOURNAL_P99_NS: u64 = 20_000;

/// Baseline p99.9 ceiling — 5x [`P99_BUDGET_NS`], 6.4x the worst of the five pre-#929 reference
/// measurements (7 764 ns; reference run 30689963620 and four more green runs). The TIGHTEST
/// baseline ceiling and the one most likely to flake first: read a red run as a real question, and
/// re-derive from a fresh harvest rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`BASELINE_P999_NS`".
const BASELINE_P999_NS: u64 = 50_000;

/// Baseline max ceiling — 77x the reference run's 25 709 ns (run 30689963620), placed BETWEEN the
/// shielded regime (0.07-0.23 ms under SCHED_FIFO + taskset) and the un-shielded one (2.7-3.4 ms),
/// so it goes red if the `chrt` shielding silently stops applying. Re-derive from a fresh harvest
/// rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`BASELINE_MAX_NS`".
const BASELINE_MAX_NS: u64 = 2_000_000;

/// Baseline `hops > 100 µs` budget, out of [`HOP_SAMPLES`]. Measured **0** in every reference run,
/// and 0 is the TARGET; 2 because the shielded-but-loaded regime ci.yml records produces one.
/// TIGHTEN TO 0 once a run at load 65+ is on record passing with 0. Re-derive from a fresh harvest
/// rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`BASELINE_HOPS_OVER_100US`".
const BASELINE_HOPS_OVER_100US: usize = 2;

// ── JOURNAL VARIANTS: A RATCHET, NOT A BUDGET ───────────────────────────────────────────────
//
// The three `JOURNAL_*` tail constants below record TODAY'S measured tail so that it stops being
// invisible, and they are meant to be LOWERED as the journal's I/O policy improves — **all three
// move together, or none does**. #932 lowered them once, from the 2026-08-01 re-measurement.
// [`JOURNAL_P99_NS`] takes the same discipline but is not a member of the move-together set. What
// the old values recorded, and what is left:
// `docs/ops/latency-gate-calibration.md`'s "Journal variants: a ratchet, not a budget".

/// Journal p99.9 ratchet — **1.8x** the highest post-#932 measurement (54 463 ns; run 30706529680,
/// the gate's first pass through CI). Lowered 250 000 -> 100 000 by #932. ⚠ At
/// [`JOURNAL_HOPS_OVER_100US`] = 50 it is arithmetically IMPLIED and does not bind; it stays as the
/// figure the failure message quotes and the series trends. On a breach, take a blocking call off
/// the fold and never raise it — re-derive from a fresh harvest rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`JOURNAL_P999_NS`".
const JOURNAL_P999_NS: u64 = 100_000;

/// Journal max ratchet — **2.3x** the worst post-#932 measurement of any kind (4 276 748 ns: the
/// ~4 ms timer-tick `park()`, the BINDING constraint on this ceiling) and 22x the worst of real
/// work (456 140 ns), from the 2026-08-01 re-measurement. Lowered 500 000 000 -> 10 000 000 by
/// #932. A depth sentinel only, and the WEAKEST of the three discriminators: read
/// [`JOURNAL_HOPS_OVER_100US`] first on a red run. Lower BOTH together, or neither, and re-derive
/// from a fresh harvest rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`JOURNAL_MAX_NS`".
const JOURNAL_MAX_NS: u64 = 10_000_000;

/// Journal `hops > 100 µs` ratchet, out of [`HOP_SAMPLES`] — **2.5x** the worst count from a GREEN
/// post-#932 rep (20, 2026-08-01 re-measurement), and ~2x below the ~97-per-run signature of a
/// blocking snapshot flush. Lowered 100 -> 50 by #932; THIS is the constant that binds (it implies
/// [`JOURNAL_P999_NS`]). If it starts costing attempts on GREEN-p99 runs, raise it first —
/// re-derived from a fresh harvest, never by nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`JOURNAL_HOPS_OVER_100US`".
const JOURNAL_HOPS_OVER_100US: usize = 50;

fn fill_with_ts(ts: i64) -> FillEvent {
    fill_with_coid(ts, NO_COID)
}

fn fill_with_coid(ts: i64, coid: &str) -> FillEvent {
    FillEvent {
        // A one-char CONSTANT id. This used to be `String::new()` with a comment claiming the
        // emptiness is what skipped dedup-set growth; that was never the mechanism, and `TradeId`
        // now makes it unrepresentable anyway. What actually keeps the set empty is `symbol:
        // "OTHER"` below: `vike_exec::ExecutionEngine::on_event` returns `Fold::Dropped` on the
        // symbol filter BEFORE it touches `seen_trade_ids`, so a repeated id never accumulates and
        // never dedups. One char rather than a descriptive name because the journal variants weigh
        // this struct's serialized size (see the `extra` WAL arithmetic below) — 1 byte per record
        // over the old `""`, and nothing asserts on that figure.
        trade_id: "t".into(),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "OTHER".into(), // symbol-filtered out in the engine: pure dispatch cost
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// The measured hop distribution — the whole thing, so a gate can assert on the TAIL and not only
/// on p99. Percentiles are computed exactly as they were when only p99 was asserted, so the
/// numbers stay comparable with every figure previously quoted in a PR body.
#[derive(Debug, Clone, Copy)]
struct HopStats {
    n: usize,
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    /// WHERE the max sits. A max at hop #0 means a one-time first-use init (the ustr global intern
    /// table used to spike ~3.2 ms there until `prewarm_interner()` moved it to core startup); a
    /// max at a random late index is ordinary OS-scheduler jitter — or, for the journal variant, a
    /// forced-sync chunk boundary.
    max_idx: usize,
    over_100us: usize,
    /// Wall time of the measured loop, the gap before it excluded — see `rt_gap`'s module doc.
    window: std::time::Duration,
    /// Wall time from the END of the gap to the end of the loop (setup + loop, no sleep). Reported
    /// as `busy_ns=` and asserted by NOTHING — `rt_gap`'s module doc says why, and what would
    /// change that.
    busy: std::time::Duration,
}

impl HopStats {
    /// ⚠ `label` earns its place on ONE line — the empty-input panic below. THREE different
    /// mechanisms feed this constructor and only one of them is a hook: [`run_core_hop`] and
    /// [`run_book_hop`] install an `on_dequeued` callback, [`run_submit_hop`] observes through its
    /// [`SubmitProbe`] `ExecutionClient` and installs no hook at all, and [`run_snapshot_build`]
    /// spawns no core thread whatsoever and times an in-loop stopwatch. A message naming any single
    /// one of them sends the first reader of a zero-sample failure — who is reading a CI log, not
    /// this file — hunting for a mechanism that variant does not have. The variant name is what
    /// says which observer to go and read.
    ///
    /// `window` is an `rt_gap::Window`, which only a window opened behind the RT gap can produce,
    /// so no variant can report without one.
    fn from_hops(label: &str, hops_seq: Vec<u64>, window: rt_gap::Window) -> Self {
        assert!(
            !hops_seq.is_empty(),
            "[{label}] no samples were recorded — THIS variant's observer never fired. Read \
             whichever one it uses (the `on_dequeued` hook, the `SubmitProbe` execution client, or \
             the in-loop stopwatch); a hop that was never measured is not a fast hop."
        );
        let n = hops_seq.len();
        let (max_idx, &max) = hops_seq.iter().enumerate().max_by_key(|&(_, &v)| v).unwrap();
        let over_100us = hops_seq.iter().filter(|&&v| v > 100_000).count();
        let mut hops = hops_seq;
        hops.sort_unstable();
        let pct = |p: f64| hops[((hops.len() as f64 * p) as usize).min(hops.len() - 1)];
        Self {
            n,
            p50: pct(0.50),
            p99: pct(0.99),
            p999: pct(0.999),
            max,
            max_idx,
            over_100us,
            window: window.spun(),
            busy: window.busy(),
        }
    }

    /// ONE greppable line per variant, so the tail is TRENDABLE across CI runs rather than
    /// reconstructible only from a failure. Grep `LATENCY-GATE` in any job log; the `key=value`
    /// shape is stable and every duration is in nanoseconds.
    ///
    /// ⚠ Written straight to the stderr HANDLE rather than through `eprintln!`, on purpose.
    /// libtest installs an output capture that `print!`/`eprintln!` route into and that swallows
    /// everything a PASSING test emits unless the runner passes `--nocapture` — which is exactly
    /// the blind spot that let a 141 ms hop pass unnoticed for as long as it did. A gate whose
    /// numbers are only legible when it is red cannot be trended, so this one line is made
    /// unconditional and does not depend on how the CI step happens to invoke the binary.
    /// PROVEN, not assumed: the CI step has NEVER passed `--nocapture` — it invokes
    /// `$RUN "$BIN" --ignored --test-threads=1`, and `grep -rn nocapture .github/` matches only two
    /// PROSE lines, both in ci.yml's own warning against believing otherwise — and run
    /// 30694578143's job log still carries every variant's line.
    fn report(&self, label: &str, extra: &str) {
        let line = self.report_line(label, extra);
        // THE choke point: every variant label in this file passes through here exactly once, so
        // this is where "is this line something the CI series and the box-health canary can both
        // read, under a label no other harness has taken" is answered — for the variants that exist
        // today and for every one added later, with no roster to keep in step. See
        // `crates/vike-core/tests/common/latency_line.rs` for the two silent failure modes and the
        // mutation results behind each rule.
        latency_line::check_report_line(&line, label);
        let mut err = std::io::stderr();
        let _ = err.write_all(line.as_bytes());
        let _ = err.flush();
    }

    /// The line [`HopStats::report`] writes, built without writing it or claiming its label, so its
    /// format has a test that runs on the fast lane
    /// ([`the_report_line_carries_window_ns_and_busy_ns`]).
    fn report_line(&self, label: &str, extra: &str) -> String {
        let Self { n, p50, p99, p999, max, max_idx, over_100us, window, busy } = *self;
        // `window_ns` and `busy_ns` ride into the persisted series like every other `k=v` here
        // (ci.yml's `series_append` carries them verbatim; every reader of that file selects fields
        // by name), so the window lengths `rt_gap` budgets are trendable, not just asserted — and
        // `busy_ns`, which nothing asserts, is trendable at all.
        let window_ns = window.as_nanos();
        let busy_ns = busy.as_nanos();
        format!(
            "LATENCY-GATE variant={label} n={n} p50_ns={p50} p99_ns={p99} p999_ns={p999} \
             max_ns={max} max_at_hop={max_idx} hops_over_100us={over_100us} \
             window_ns={window_ns} busy_ns={busy_ns}{extra}\n"
        )
    }
}

/// `busy_ns=` reaches the line [`HopStats::report`] writes, beside an unchanged `window_ns=`, as a
/// row ci.yml's `series_append` would emit. That line is the ONLY place `busy` is read: no ceiling
/// asserts it (see `rt_gap`'s module doc), so dropping it from the format would otherwise pass
/// everything. NOT `#[ignore]`d: it rides the fast lane with the `latency_line` and `rt_gap` tests,
/// while the the latency box latency job runs `--ignored` and skips it.
#[test]
fn the_report_line_carries_window_ns_and_busy_ns() {
    let stats = HopStats {
        n: 1,
        p50: 1,
        p99: 1,
        p999: 1,
        max: 1,
        max_idx: 0,
        over_100us: 0,
        window: std::time::Duration::from_millis(300),
        busy: std::time::Duration::from_millis(550),
    };
    let line = stats.report_line("unit-busy", " extra=1");
    latency_line::validate_report_line(&line, "unit-busy").expect("a series-safe line");
    let row = latency_line::parse_report_line(&line).expect("a row series_append would emit");
    assert_eq!(row.get("window_ns"), Some(&"300000000"), "window_ns is the loop alone: {line}");
    assert_eq!(row.get("busy_ns"), Some(&"550000000"), "busy_ns must be reported: {line}");
    assert_eq!(row.get("extra"), Some(&"1"), "the variant's extras still follow: {line}");
}

/// Shared core-hop harness for the baseline and the two journal-on gates. Runs [`HOP_SAMPLES`]
/// fills through the real core under request/response pacing (one event in flight at a time) and
/// returns the full hop distribution. `journal` selects the variant: `None` = today's zero-
/// overhead path (byte-identical fold); `Some(..)` opens a write-ahead journal so every
/// exec-lane message is appended before it folds. Because the core is single-threaded and the
/// pacing gates the next send on the previous message's dispatch completing, message `i`'s
/// journal-append cost lands inside message `i+1`'s measured hop — so the DELTA between the two
/// variants on the same box is the true per-message journaling cost (mmap memcpy + serde_json).
fn run_core_hop(label: &str, journal: Option<JournalConfig>) -> HopStats {
    run_core_hop_with_coid(label, journal, NO_COID)
}

/// [`run_core_hop`] parameterized by the coid each fill carries. The two GATE tests call the
/// `NO_COID` wrapper above, so their measured path stays byte-identical to before this parameter
/// existed (`"".to_string()` and `String::new()` are both allocation-free).
fn run_core_hop_with_coid(label: &str, journal: Option<JournalConfig>, coid: &str) -> HopStats {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup so spawn -> first hop below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    // Captured before the config is moved into `CoreConfig`, so the summary line can state the
    // cadence this run actually used. `u64::MAX` (the append-isolating variant) reports 0 expected
    // snaps; `1024` (the production default) reports ~97 over 100 000 hops.
    let snap_every = journal.as_ref().map(|j| j.snapshot_every);
    let journal_on = journal.is_some();
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    // struct-update (not field-reassign-on-default) — the codebase clippy gate forbids
    // reassigning fields on a `Default::default()` value (see journal_wiring.rs / runtime_smoke.rs).
    let cfg = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.ts as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        journal,
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();

    // request/response pacing: one event in flight at a time -> measures the pure hop
    // (send -> core dispatched) including the thread wakeup, without queueing noise
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill_with_coid(ts, coid))).unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    // How much WAL this run writes decides how many `sync_chunk_bytes` boundaries it crosses — i.e.
    // how many chunk syncs the SYNCER thread performs (since #929 it performs all of them; the
    // append only posts a watermark). journal.rs frames each record as `[len][crc][payload]`,
    // 8 bytes plus a `serde_json` payload, and that payload is a `JournalRecordRef::Cmd` WRAPPING
    // this exact `Ingest` — so serializing the `Ingest` alone is a strict LOWER bound (the wrapper
    // adds a variant tag plus `seq`/`now_ms`). A representative 10-digit `ts` is used because the
    // real ones are elapsed-nanos, not zero.
    //
    // `snap_every=` / `snaps_expected=` are the twin of that arithmetic for the CADENCE SNAPSHOT:
    // one `Snap` per `snapshot_every` journaled records, each of which used to take a blocking
    // whole-segment `msync` on this very thread. Diagnostic only; nothing asserts on either.
    let extra = if journal_on {
        let record =
            serde_json::to_vec(&Ingest::Event(Event::Fill(fill_with_coid(1_000_000_000, coid))))
                .expect("Ingest serializes")
                .len()
                + 8;
        let every = snap_every.unwrap_or(u64::MAX);
        format!(
            " wal_bytes_min={} snap_every={every} snaps_expected={}",
            record * N,
            N as u64 / every
        )
    } else {
        String::new()
    };

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, &extra);
    stats
}

/// Assert one variant's whole distribution, not just its p99. Every ceiling is a named constant
/// with its derivation on it; the message repeats the measured reference so a red run is
/// self-explanatory to whoever reads the log first.
///
/// ⚠ `p99_ns` is a PARAMETER rather than [`P99_BUDGET_NS`] since the 2026-08-05 calibration: the
/// baseline runs 10-30x under 10 µs and the journal variants ran at 1.0-2.4x under it, so one
/// literal could not be a calibrated ceiling for both. See [`JOURNAL_P99_NS`].
fn assert_hop_budget(
    label: &str,
    s: &HopStats,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
    hops_over_100us: usize,
) {
    // FIRST, because it is a precondition of every figure below rather than one more ceiling: a
    // gated window too long for the kernel's RT budget can be stalled ~50 ms by the RT throttle
    // INSIDE it. Only the gated variants come through here; measure-only windows are recorded,
    // never failed. See `rt_gap`'s module doc.
    if let Err(why) = rt_gap::gated_window_fits(label, s.window) {
        panic!("{why}");
    }
    assert!(s.p99 < p99_ns, "[{label}] plan gate: p99 core-hop < {p99_ns} ns (got {} ns)", s.p99);
    assert!(
        s.p999 < p999_ns,
        "[{label}] tail gate: p99.9 core-hop < {p999_ns} ns (got {} ns). \
         p99 alone said {} ns — the tail is the number that costs money.",
        s.p999,
        s.p99
    );
    assert!(
        s.max < max_ns,
        "[{label}] tail gate: max core-hop < {max_ns} ns (got {} ns at hop #{}/{}). \
         A max at hop #0 is one-time first-use init; a late index is a real stall.",
        s.max,
        s.max_idx,
        s.n
    );
    assert!(
        s.over_100us <= hops_over_100us,
        "[{label}] tail gate: at most {hops_over_100us} of {} hops may exceed 100 µs (got {})",
        s.n,
        s.over_100us
    );
}

#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn p99_core_hop_under_10us() {
    // The BASELINE variant is the shape a live maker actually runs (journaling is off by default),
    // and the reference run measured it clean — p99.9 2 104 ns, max 25 709 ns, zero hops over
    // 100 µs. So it is gated strictly: see BASELINE_* for each ceiling's derivation.
    let stats = run_core_hop("baseline", None);
    assert_hop_budget(
        "baseline",
        &stats,
        P99_BUDGET_NS,
        BASELINE_P999_NS,
        BASELINE_MAX_NS,
        BASELINE_HOPS_OVER_100US,
    );
}

/// The gate that PROVES the mmap-journal-over-synchronous-SQLite bet: the same core hop with
/// write-ahead journaling ON. A huge segment (no roll) and `snapshot_every = u64::MAX` (no
/// cadence snapshots fire — only the always-on shutdown snap, off the measured path) isolate the
/// per-message APPEND cost. Same `#[ignore]` + release discipline as the baseline; the DELTA vs
/// the baseline is the true journaling cost.
///
/// ⚠ Its tail ceilings are a RATCHET recording today's measured tail, NOT a budget anyone signed
/// off on — read the `JOURNAL_*` block above before treating a green run here as good news.
///
/// ⚠ Its p99 ceiling is [`JOURNAL_P99_NS`], not the 10 µs the FUNCTION NAME says — the name predates
/// the 2026-08-05 calibration and is kept only so a CI log greps the same across that change.
///
/// ⚠ And read what this variant DOES NOT cover, because that blind spot cost a real defect a long
/// life: `snapshot_every = u64::MAX` is not a production setting. `JournalConfig::at` and
/// `[sinks.journal]` both default to **1024**, and a cadence `Snap` runs on the fold thread — so
/// every hop this variant measures is a hop in which no snapshot fired. That is exactly the
/// dimension in which `CoreThread::write_snap`'s blocking flush hid from #929.
/// [`p99_core_hop_under_10us_with_journal_snapshots`] is the variant that covers it; keep BOTH.
/// A 256 MiB segment also yields a 16 MiB sync chunk, so the run crosses ONE chunk boundary — since
/// #929 that costs the measured hop a watermark post, not an `msync`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn p99_core_hop_under_10us_with_journal() {
    let dir = std::env::temp_dir().join(format!("vjl-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal = JournalConfig {
        dir: dir.clone(),
        file: vike_journal::JournalFileConfig {
            segment_bytes: 256 * 1024 * 1024, // huge: no segment roll over the 100k appends
            flush_every: 256,
        },
        snapshot_every: u64::MAX, // no cadence snaps — isolate the per-message append cost
    };
    let stats = run_core_hop("journal", Some(journal));
    // Clean up BEFORE asserting: a red tail must not also leave a 256 MiB segment behind in tmp.
    let _ = std::fs::remove_dir_all(&dir);
    assert_hop_budget(
        "journal",
        &stats,
        JOURNAL_P99_NS,
        JOURNAL_P999_NS,
        JOURNAL_MAX_NS,
        JOURNAL_HOPS_OVER_100US,
    );
}

/// The PRODUCTION-CONFIGURED journal gate — the same core hop as
/// [`p99_core_hop_under_10us_with_journal`], but with the config a real node actually runs.
///
/// ⚠ WHY THIS EXISTS. The variant above pins `snapshot_every = u64::MAX` so that **no cadence
/// snapshot ever fires during the measured hops** — deliberately, to isolate the per-message append
/// cost. But `JournalConfig::at` and the `[sinks.journal]` profile default are **1024**, so a
/// production node takes a cadence `Snap` every 1024 exec-lane records, and `CoreThread::write_snap`
/// runs on the FOLD THREAD. The gate was therefore configured differently from production in
/// exactly the dimension that mattered, and was structurally blind to whatever `write_snap` costs:
/// no snap ever fired inside a measurement.
///
/// It cost a real defect a long life. `write_snap` ended each snapshot with
/// `CommandJournal::flush()` — the blocking whole-mapping `msync` whose own doc says it is "not safe
/// to call from the vike-core fold" — so a journaling node paid one on the fold thread every 1024
/// messages. #929 moved the OTHER two blocking `msync`s (the chunk sync and the roll seal) off that
/// thread and this one survived, invisible, because no harness variant ever fired a snap.
///
/// The config here is [`JournalConfig::at`] VERBATIM (64 MiB segments, `flush_every` 256,
/// `snapshot_every` 1024) rather than a hand-built near-copy, so this test cannot drift away from
/// the production default without the default itself changing. Note that makes it differ from the
/// variant above in TWO knobs, not one — segment size as well as snap cadence — so read the
/// before/after of THIS label across a change, not the cross-variant delta:
///
///   * `segment_bytes` 64 MiB ⇒ `sync_chunk_bytes` = max(64 MiB/16, 4 MiB) = 4 MiB, so the run's
///     ~28 MB of WAL crosses ~6 chunk boundaries instead of the 256 MiB variant's one. Since #929
///     every one of those is a watermark post to the syncer thread, so they cost the measured hop
///     an atomic store and at most one channel send.
///   * no roll either way (~28 MB of WAL into a 64 MiB segment).
///
/// Gated on the same `JOURNAL_*` ceilings as the variant above — including [`JOURNAL_P99_NS`] rather
/// than the 10 µs this function's NAME states (see `docs/ops/latency-gate-calibration.md`): after
/// the snap write leaves the fold path the two variants measure the same shape of work, and holding
/// them to one set of numbers is what makes a re-introduced blocking snap flush fail here.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn p99_core_hop_under_10us_with_journal_snapshots() {
    let dir = std::env::temp_dir().join(format!("vjl-latency-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let stats = run_core_hop("journal-snap", Some(JournalConfig::at(dir.clone())));
    let _ = std::fs::remove_dir_all(&dir);
    assert_hop_budget(
        "journal-snap",
        &stats,
        JOURNAL_P99_NS,
        JOURNAL_P999_NS,
        JOURNAL_MAX_NS,
        JOURNAL_HOPS_OVER_100US,
    );
}

/// Builds per [`run_snapshot_build`] variant. Fewer than [`HOP_SAMPLES`] on purpose: a build is
/// ~1-2 orders of magnitude dearer than a core hop, and this is the sample count the harness this
/// replaced already used, so the p50 stays comparable with whatever anyone quoted from it.
const SNAPSHOT_BUILDS: usize = 10_000;

/// `snapshot::build`'s own cost at a given ORDER COUNT — the publish half of the core hop.
///
/// # ⚠ The harness this replaced encoded a premise its own crate refutes
///
/// It was called `snapshot_build_measured_off_path` and its ceiling carried the comment "the build
/// runs coalesced (~16ms), never per-event". **That is false, and this crate documents it as false
/// in two other places.** `crates/vike-core/src/runtime/journaling.rs`'s `note_event` states it outright
/// ("#887 deferred rendering to publish, on the premise that … **That premise is false.**"), and
/// the latency-gate section of `crates/vike-core/CLAUDE.md` corrects it (the paragraph opening
/// "Publish is NOT only the coalesced ... cadence"): the runtime ALSO publishes
/// whenever the core is about to go idle, ungated by `snapshot_interval`, so on a venue whose
/// events arrive sporadically it publishes PER EVENT. The same premise, held once before, cost
/// #887 a 4.2x tail regression that #896 had to undo.
///
/// So this is not an off-path curiosity: on the request/response pacing every harness in this file
/// uses, `snapshot::build` runs once per message and its cost is INSIDE the next hop the gates
/// measure. Which is also why the old harness's numbers never reached anybody — it reported a MEAN
/// through `println!`, and libtest's capture swallows stdout on a PASS unless the runner passes
/// `--nocapture`, which the CI step does not. Nothing landed in the durable series either: that file
/// held ten variants and `snapshot-build` was not among them.
///
/// # What varies, and what deliberately does not
///
/// ONE axis: the order count. `build` walks the whole registry (`crates/vike-core/src/snapshot/build.rs`'s
/// `build`, one `OrderView` per entry), so this is the axis that decides whether the publish is a
/// fixed cost or a per-order one — the question a trend can actually answer. Positions, marks and
/// the recent ring are held constant, and the ring's length is READ from
/// `CoreConfig::default().recent_events_cap` rather than restated, so the harness cannot drift from
/// the cap a real core publishes under.
///
/// Read the three labels as a slope: `0ord` is the intercept (the fixed cost the journal-free
/// `baseline` hop already pays once per message), `64ord` is the historical configuration, and
/// `512ord` is a wide multi-symbol book. A slope that steepens across releases is the regression
/// this series exists to see.
///
/// # Accounting, precisely
///
/// The window contains `build` and NOTHING else: the previous snapshot is freed BEFORE `t0`, so a
/// sample is never charged for its predecessor's `Vec`/`Arc` teardown. A real publish pays that too
/// (`arc_swap` defers it to whichever thread drops the last handle), so the honest reading is that
/// these numbers are the BUILD, and a publish is the build plus a free. `ReconBlock::default()` is
/// constructed inside the window because `build` takes it by value — it is an empty-`Vec` `Default`,
/// which allocates nothing.
fn run_snapshot_build(label: &str, orders: usize) -> HopStats {
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    for i in 0..orders {
        let coid = format!("o{i}");
        let req = OrderRequest {
            client_order_id: coid.clone(),
            venue: "sim".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "limit".to_string(),
            price: Some(100.0),
            ..OrderRequest::default()
        };
        engine.registry.insert(coid, vike_exec::ManagedOrder::new(req));
    }
    engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "LONG".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    engine.account.set_mark_from("sim", "BTCUSDT", 101.0, MarkSource::VenueMark, 0);
    // The ring holds RENDERED lines (2026-07-29): the runtime formats each event at PUSH, so this
    // harness measures what `snapshot::build` actually does — clone a refcount per entry.
    let recent_cap = CoreConfig::default().recent_events_cap;
    let recent: std::collections::VecDeque<std::sync::Arc<str>> = (0..recent_cap)
        .map(|i| {
            std::sync::Arc::from(
                vike_core::RecentNote::Event(vike_core::EventNote::capture(&Event::OrderFilled(
                    vike_model::events::OrderFilled {
                        client_order_id: format!("o{i}"),
                        fill: fill_with_ts(0),
                        ts: 0,
                    },
                )))
                .render()
                .as_str(),
            )
        })
        .collect();

    let bars = indexmap::IndexMap::new();
    let base = Instant::now();
    let mut samples = Vec::<u64>::with_capacity(SNAPSHOT_BUILDS);
    let mut last: Option<CoreSnapshot> = None;
    let spinning = gap.start_window();
    for seq in 0..SNAPSHOT_BUILDS as u32 {
        drop(last.take()); // the predecessor's free, kept OUT of this sample's window
        let t0 = base.elapsed().as_nanos() as u64;
        let snap = vike_core::snapshot::build(
            u64::from(seq),
            &engine,
            &[],
            10_000.0,
            PriceCfg::default(),
            vike_exec::MarginCallConfig::default().mm_requirement,
            &recent,
            &bars,
            std::sync::Arc::new([]),
            &None,
            0,
            0,
            ReconBlock::default(),
            Vec::new(),
        );
        let t1 = base.elapsed().as_nanos() as u64;
        samples.push(t1.saturating_sub(t0));
        last = Some(snap);
    }
    let window = spinning.close_window();

    let stats = HopStats::from_hops(label, samples, window);
    stats.report(label, &format!(" orders={orders} recent={recent_cap}"));
    // Structural, not timing: prove the harness built what its label claims (and keep the last
    // snapshot alive to the end of the loop, so no build can be optimized away).
    assert_eq!(
        last.expect("SNAPSHOT_BUILDS > 0, so a snapshot was built").orders.len(),
        orders,
        "[{label}] the built snapshot must carry every registry order"
    );
    stats
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what one `CoreSnapshot` publish costs, at three order
/// counts, persisted into the CI series like every other variant here.
///
/// Read [`run_snapshot_build`]'s doc first — in particular why the harness this replaced was named
/// `..._off_path` and why that name was wrong. Deliberately asserts NO timing ceiling beyond the
/// pre-existing sanity one below, for the reason the module doc gives: there is no calibrated budget
/// for this path yet, and an uncalibrated one is a flake.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture snapshot_build
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn snapshot_build_cost_baseline() {
    run_snapshot_build("snapshot-build-0ord", 0);
    let historical = run_snapshot_build("snapshot-build-64ord", 64);
    run_snapshot_build("snapshot-build-512ord", 512);
    // THE PRE-EXISTING SANITY CEILING, PRESERVED — 1 ms, the same figure the replaced harness
    // asserted, on the same 64-order configuration. Restated on p50 rather than on the MEAN it used
    // to bound because a mean on this box is one ~4 ms timer-tick `park()` away from being
    // meaningless ([`JOURNAL_MAX_NS`] documents that park as the binding constraint on its own
    // ceiling, measured at 4.276 ms in an otherwise clean run). It is a liveness check with ~100x
    // headroom over a measured ~10 µs build, NOT a derived budget — see the module doc on why this
    // change adds no new ceiling anywhere.
    //
    // Asserted LAST, after all three variants have reported: a red must still leave its measurements
    // in the series, the same reason the journal gates clean up before asserting.
    assert!(
        historical.p50 < 1_000_000,
        "snapshot build p50 {} ns exceeds the 1 ms sanity ceiling — this is not a budget breach, \
         it is a sign the build is doing something structurally different",
        historical.p50
    );
}

/// Liveness deadline for a MEASUREMENT harness's request/response spin.
///
/// ⚠ **NOT A LATENCY CEILING.** 30 s against a wait that normally completes in ~1 µs is seven orders
/// of magnitude of headroom, so it cannot fire on a slow box — only on a genuine stall. It exists
/// because of what an unbounded `spin_loop` IS in this binary's CI environment: the step runs it
/// under `sudo chrt -f 50` pinned to four physical cores, so a spin that never terminates is not a
/// hung test but a SCHED_FIFO process nothing can preempt. This gate's history holds exactly that —
/// a livelocked binary at 380% CPU with no progress for 5+ minutes (2026-07-29) — plus the separate,
/// worse discovery that while such a process lives a `/proc` scan on the latency box goes from 53 ms to over
/// 120 s, which degrades ClickHouse, the live recorders and the monitoring on the same host. A NEW,
/// unproven harness must not be able to do that to that box.
///
/// The three GATE harnesses' spins are deliberately left UNGUARDED. Their inner loop is what 98
/// calibration attempts and 172 persisted rows were measured through, they have never hung, and
/// perturbing a calibrated measurement to defend against a hazard it has not exhibited is the wrong
/// trade. Adding the guard there is a separate change with its own before/after.
const SPIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Request/response pacing with a deadline: spin until `counter` reaches `target`.
///
/// The clock is read once per 4096 spins, so a wait that completes normally (a few hundred spins)
/// reads it ZERO times and the guard costs the measurement nothing.
fn spin_until(counter: &AtomicU64, target: u64, what: &str) {
    let start = Instant::now();
    let mut spins: u32 = 0;
    while counter.load(Ordering::Acquire) < target {
        std::hint::spin_loop();
        spins = spins.wrapping_add(1);
        if spins.is_multiple_of(4096) && start.elapsed() > SPIN_DEADLINE {
            panic!(
                "liveness guard tripped (NOT a latency ceiling): waited {:?} for {what} to reach \
                 {target}, stuck at {}. The overwhelmingly likely cause is that the order never \
                 reached the venue client at all — a `RiskGate` veto publishes `OrderDenied` and \
                 calls no `submit`, so this harness would wait forever. Read the engine's limits \
                 before reading any timing into this.",
                start.elapsed(),
                counter.load(Ordering::Acquire)
            );
        }
    }
}

/// Timestamping `ExecutionClient` for [`run_submit_hop`]: records the moment the venue-facing
/// `submit` call is ENTERED — the far end of the lane being measured — and retains nothing.
///
/// Retaining nothing is load-bearing rather than tidiness. `RecordingClient::submit` pushes every
/// `OrderRequest` into a `Vec`, and [`HOP_SAMPLES`] of them would put allocator growth on the fold
/// thread inside the lane this harness reports.
struct SubmitProbe {
    /// The harness's shared monotonic base, so a stamp taken here is comparable with the sender's.
    base: Instant,
    hops: Arc<std::sync::Mutex<Vec<u64>>>,
    /// Bumped AFTER the sample is recorded, so a sender that observes the counter can never observe
    /// it without the sample already behind it.
    submitted: Arc<AtomicU64>,
}

impl ExecutionClient for SubmitProbe {
    fn submit(&mut self, request: &OrderRequest) {
        // Stamped FIRST, before the harness's own mutex: that lock is uncontended bookkeeping and
        // must not land inside the measured interval.
        let now_ns = self.base.elapsed().as_nanos() as u64;
        self.hops.lock().unwrap().push(now_ns.saturating_sub(request.ts as u64));
        self.submitted.fetch_add(1, Ordering::Release);
    }
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// ORDER-SUBMIT lane hop (perf-program measurement scaffolding — the sibling of [`run_core_hop`] and
/// [`run_book_hop`], NOT a gate). It measures the path that actually places an order, which no gate
/// in this file has ever touched.
///
/// # Why the gates are blind to it
///
/// Every gate here feeds `Ingest::Event(Event::Fill)` — the venue-REPLY lane. A submit arrives on a
/// different arm entirely, `Ingest::Command(Command::Order(OrderIntent::Submit))`, and runs
/// completely different code: `crates/vike-core/src/runtime/apply.rs`'s `apply_intent` (coid resolve
/// → `vike_model::preflight_order` → contingency-link classify) then
/// `crates/vike-exec/src/execution_engine/mod.rs`'s `submit_order` → `gate_and_register` (the
/// `RiskGate` crossing, a `ManagedOrder::new` and a registry insert) → `client.submit`. None of that
/// is on the fill path, so a regression anywhere in it is invisible to every ceiling above.
///
/// # Where "submit → wire" honestly stops
///
/// At the `ExecutionClient` seam, and no further. Beyond it sits a per-venue `ExecActor` thread, a
/// signer and a blocking REST/WS round trip — network, credentials and a live venue, none of which
/// can be measured deterministically in-process, and all of which would make this a smoke test
/// rather than a series. The seam IS the last point the core controls, which makes it the right
/// boundary: everything this harness reports is something a change in this workspace can move.
///
/// # Accounting, and why it matches the other hop harnesses
///
/// One message in flight, paced on the probe (`submit` entered ⇒ the response). The interval is
/// `[intent stamped] → [venue client entered]`, carried on `OrderRequest.ts` — which survives the
/// `RiskGate` verbatim (`crates/vike-exec/src/risk.rs`'s `check_inner` clones the request and never
/// assigns `ts`), the same trick [`run_core_hop`] plays with `FillEvent.ts` and [`run_book_hop`]
/// with `book.last_seq`. As in those, message `i-1`'s post-submit TAIL — outbox drive, `pump_client`,
/// `drain_delivered` and the idle `CoreSnapshot` publish — lands inside message `i`'s window. That
/// is deliberate here: the publish is exactly what the two variants are meant to price.
///
/// # `resting` is the axis, and the coid is FIXED for a hard reason
///
/// Every measured submit reuses ONE client-order-id, so `registry.insert` REPLACES its entry and the
/// registry stays at `resting + 1` for the whole run. That is not a shortcut: the idle publish walks
/// the registry once per message, so a harness minting a fresh coid per submit would be O(N²) and
/// its tail would report `IndexMap` growth rather than this lane (module doc, "The trap a new
/// harness here will hit first").
///
/// Read the two labels as one delta: `0resting` is the submit path with a trivial publish behind it;
/// `32resting` is the same path with a maker-sized resting book, i.e. what a live quoting strategy
/// actually pays. A delta that grows faster than the order count means the per-message publish, not
/// the submit.
///
/// # Two stated residuals
///
/// The coid MINT is excluded — a submit with an empty coid makes the runtime call
/// `ClientOrderIdGenerator::generate`, and that is precisely the shape that cannot keep the registry
/// pinned. And no venue events arrive here, so the recent-events ring stays EMPTY and each publish
/// is cheaper than a live node's by up to `recent_events_cap` `Arc` clones. Both are floors on the
/// real cost, never overstatements.
fn run_submit_hop(label: &str, resting: usize) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup so spawn -> first submit below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let hops = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N + resting)));
    let submitted = Arc::new(AtomicU64::new(0));

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        SubmitProbe { base, hops: Arc::clone(&hops), submitted: Arc::clone(&submitted) },
        "sim",
        "BTCUSDT",
    );
    // No `on_dequeued` hook: the probe is the observer here, so the fold carries no harness
    // callback at all — one fewer difference from a production core than the other harnesses have.
    let handle = spawn_core(engine, CoreConfig::default());

    // ONE template, cloned per message. `venue`/`symbol` are the engine's own, so every submit
    // routes to the primary engine and never touches the coid→venue map; `RiskLimits::new()` arms
    // no notional, exposure, margin or rate lane, so the gate admits it (the same construction
    // `crates/vike-core/tests/runtime_smoke/headless_path.rs`'s `fifo_order_preserved_across_idle_transitions`
    // proves reaches the registry). `LIVE_COID` is reused so this harness's per-message allocation
    // matches the `coid-live` variant's realistic 13-byte wire form.
    let template = OrderRequest {
        client_order_id: LIVE_COID.to_string(),
        venue: "sim".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ..OrderRequest::default()
    };

    // The RESTING book, pre-seeded outside the measurement: `resting` DISTINCT coids, each of which
    // stays in the registry for the rest of the run and so is walked by every later publish.
    for i in 0..resting {
        let mut req = template.clone();
        req.client_order_id = format!("resting{i}");
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
        spin_until(&submitted, (i + 1) as u64, "the pre-seed submit");
    }
    // Those samples measured a GROWING registry, which is not what this harness reports. Discarding
    // them is safe HERE and nowhere else: `spin_until` returned only after the probe had pushed its
    // last pre-seed sample and then released the counter, so no in-flight write can land behind it.
    hops.lock().unwrap().clear();

    let spinning = gap.start_window();
    for i in 0..N {
        let mut req = template.clone();
        // Stamped AFTER the clone, so the template copy is not charged to the lane; the `Box` that
        // follows IS inside the window, because `OrderIntent::Submit(Box<OrderRequest>)` is the real
        // write contract and a producer genuinely pays for it.
        req.ts = base.elapsed().as_nanos() as i64;
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
        spin_until(&submitted, (resting + i + 1) as u64, "the measured submit");
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hops).unwrap().into_inner().unwrap(), window);
    stats.report(label, &format!(" resting={resting} registry={}", resting + 1));
    // Structural only — this harness prints, it does not gate on timing (see the test doc). A short
    // count means orders were denied rather than submitted, which the deadline guard would usually
    // have caught first.
    assert_eq!(stats.n, N, "[{label}] every measured submit must have reached the venue client");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the order-submit lane, with and without a maker-sized
/// resting book behind it.
///
/// Read [`run_submit_hop`]'s doc for what is measured, where "the wire" stops, and why the
/// client-order-id is fixed. Deliberately asserts NO timing ceiling, exactly like
/// [`book_lane_hop_baseline`] and for the reason the module doc gives: no calibrated budget exists
/// for this path, and an uncalibrated one is a flake rather than a gate.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture submit_lane_hop
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn submit_lane_hop_baseline() {
    run_submit_hop("submit-hop-0resting", 0);
    run_submit_hop("submit-hop-32resting", 32);
}

/// BOOK-lane hop harness (perf-program measurement scaffolding — the companion of
/// [`run_core_hop`], NOT a gate). It measures ONE thing: what a live venue pump pays, end to end,
/// to get one applied depth update from its standing book into the single-writer core.
///
/// **This body now models the `Arc<L2Book>` producer** (perf audit finding #1, the change this
/// harness was landed to score). Before the change the pump kept an OWNED `L2Book` and deep-cloned
/// it into every `BookUpdate` — so the harness deep-cloned a `levels_per_side` template per
/// message, and the core paid the matching full drop. Now the pump keeps ONE `Arc<L2Book>`, stamps
/// through [`Arc::make_mut`] (copy-on-write: a real copy only while the core still holds the
/// previous handle) and sends an `Arc::clone`. The harness mirrors that exactly, so the BEFORE run
/// (on `main`, owned clone) and the AFTER run (here) each measure their own era's real producer —
/// which is the comparison that matters, but note it is NOT a byte-identical harness across the
/// two runs. Run BEFORE and AFTER on the SAME quiet box.
///
/// READING THE RESULT: the clone-dominated hop scales with `levels_per_side` (the 50-level p50
/// materially above the 10-level p50, and both above a fixed floor); an `Arc` hop is fixed-cost, so
/// 10-level and 50-level p50s should collapse onto each other and onto the plain [`run_core_hop`]
/// floor. A 50-level p50 that still tracks depth AFTER the change REFUTES the win — it would mean
/// `Arc::make_mut` is copy-on-writing every message (the core still holding the previous handle at
/// the next mutation), which this harness's one-in-flight pacing makes the WORST case, not the
/// typical one.
///
/// Mechanics, mirroring [`run_core_hop`]'s request/response pacing (one message in flight):
/// the send-time ns stamp rides `book.last_seq` — the lanes `BookUpdate` deliberately carries
/// no ts ("the dispatch clock stamps `now`"), and `last_seq` is a `u64` this harness owns
/// end-to-end (the core never folds deltas into it). The stamp is taken BEFORE the `make_mut`
/// ON PURPOSE: the hop spans the producer-side book work (whatever it now costs), the
/// `BookUpdate` build, the channel enqueue and the core wakeup/dequeue. The `Ingest::Book`
/// DISPATCH cost of message `i` — mid + mark-set + strategy-mount lookup, AND the drop of that
/// message's book handle — lands inside message `i+1`'s hop, exactly like the journal variant's
/// accounting (see [`run_core_hop`]'s doc). That drop accounting is deliberate: the core-side
/// full `BTreeMap` drop is half of what this change removes.
/// A do-nothing strategy whose only job is to EXIST, so the runtime builds a `LiveBroker` per
/// book message. It allocates nothing itself, so the mounted harness measures the runtime's
/// per-message ctx construction rather than any strategy work.
struct BookProbe;

impl vike_model::Strategy<vike_core::LiveBroker> for BookProbe {
    fn on_order_book(&mut self, _b: &mut vike_core::LiveBroker, _book: &vike_model::L2Book) {}
}

/// One [`BookProbe`] mount on `(sim, <symbol>, 1m)`. `controller_id` must be distinct per mount
/// whenever two share a triple (`crates/vike-core/src/runtime/assemble.rs`'s `load_mount_ids`
/// panics on a duplicate mount id); `None` keeps the legacy `{venue}__{symbol}__{interval}` id the
/// single-mount labels have always used.
fn probe_mount(
    symbol: &str,
    controller_id: Option<String>,
    symbols: Vec<vike_core::MountLeg>,
) -> vike_core::StrategyMount {
    vike_core::StrategyMount {
        account: None,
        symbols,
        controller_id,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(BookProbe),
    }
}

/// The book-lane hop WITHOUT a mounted strategy — the original harness, kept as the control.
fn run_book_hop(label: &str, levels_per_side: usize) {
    run_book_hop_inner(label, levels_per_side, Vec::new())
}

/// The book-lane hop WITH a strategy mounted on the book's symbol, optionally DECLARING extra
/// symbols.
///
/// Closes a structural blind spot rather than adding a gate: the core-hop gates build a
/// `CoreConfig` with no `strategy`/`extra_mounts` and feed only `Ingest::Event(Event::Fill)`, so
/// `self.mounts` is empty and NO `LiveBroker` is ever constructed on the measured path. A mounted
/// runtime builds a fresh one for EVERY quote/trade/book message via `drive_strategy_tick`.
///
/// `declared` non-empty additionally exercises `declared_views`, which materializes the per-symbol
/// position/mark/BAR tables a multi-symbol mount reads from — the allocations the multi-symbol lane
/// added to a per-message path. (The bar table joined them when `Broker::bars` stopped discarding
/// its `symbol` argument: one `SeriesKey` build and one `Arc` clone per declared instrument. An
/// UNDECLARED mount still short-circuits on a bool before any of it.)
fn run_book_hop_mounted(label: &str, levels_per_side: usize, declared: &[&str]) {
    let symbols = declared.iter().map(|s| vike_core::MountLeg::same_venue(*s)).collect();
    run_book_hop_inner(label, levels_per_side, vec![probe_mount("OTHER", None, symbols)])
}

/// The book-lane hop with `subscribed` mounts on the book's own `(venue, symbol)` plus `others`
/// mounts on symbols the producer never publishes. `subscribed` = N mounts each built, run and
/// drained per message; `others` = mount slots the dispatch skips (a few ns each) but every publish
/// still builds a view for — see [`book_lane_hop_mounted_n`]'s doc for what that does to the
/// reading.
fn run_book_hop_mounted_n(label: &str, levels_per_side: usize, subscribed: usize, others: usize) {
    let mut mounts: Vec<_> = (0..subscribed)
        .map(|k| probe_mount("OTHER", Some(format!("probe-{k}")), Vec::new()))
        .collect();
    mounts.extend(
        (0..others)
            .map(|k| probe_mount(&format!("ELSE{k}"), Some(format!("else-{k}")), Vec::new())),
    );
    run_book_hop_inner(label, levels_per_side, mounts)
}

/// BOOK-lane BURST throughput (a measurement, NOT a gate): the CORE time per book message under a
/// burst, with the same mounts as [`run_book_hop_mounted_n`].
///
/// The hop harnesses send ONE message and wait for it, so every hop carries the producer's floor
/// (the send, the wakeup, the dequeue: several hundred ns) and a core-side cost smaller than that
/// floor does not show. Here each round sends `BURST` updates back to back through the lossless
/// tick lane and waits until the hook has seen all of them, so the core drains a queue and the
/// round's wall time divided by `BURST` is the per-message drain cost (the producer runs in
/// parallel and is kept trivial: every message carries the SAME standing `Arc<L2Book>`, no
/// `make_mut`, so it is a refcount bump, two short strings and a channel send).
///
/// Accounting: the hook fires on DEQUEUE, before dispatch, so the last message of a round is
/// dispatched while the next round starts (one message's dispatch per round crosses the boundary).
/// The core publishes whenever it goes idle with dirty state: a core that drains slower than the
/// producer sends stays busy through a burst and publishes about once per round, while one that
/// keeps up goes idle between messages and publishes per message, as in the hop harnesses. A sample
/// is therefore never below the producer's per-message send cost: read it as the DRAIN RATE, not as
/// dispatch alone. One sample per round, `N` samples.
fn run_book_burst(label: &str, levels_per_side: usize, subscribed: usize, others: usize) {
    const BURST: u64 = 64;
    // The source gate (`crates/vike-core/tests/common/rt_gap/source_gate.rs`'s `SAMPLE_COUNTS`)
    // recognizes a sample loop only by these bound names, so the round loop runs to `N`.
    const N: usize = 2_000;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let processed = Arc::new(AtomicU64::new(0));
    let mut samples = Vec::<u64>::with_capacity(N);

    // The mounts of `run_book_hop_mounted_n`: `subscribed` on the book's own series, `others` on
    // symbols the producer never publishes.
    let mut mounts: Vec<_> = (0..subscribed)
        .map(|k| probe_mount("OTHER", Some(format!("probe-{k}")), Vec::new()))
        .collect();
    mounts.extend(
        (0..others)
            .map(|k| probe_mount(&format!("ELSE{k}"), Some(format!("else-{k}")), Vec::new())),
    );

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let mut mounts = mounts.into_iter();
    let cfg = CoreConfig {
        // Counts book messages only: no per-message stamp, so the hook adds one atomic add.
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(_) = msg {
                processed_hook.fetch_add(1, Ordering::Release);
            }
        })),
        // The first mount is `strategy`, the rest `extra_mounts` (as in `run_book_hop_inner`).
        strategy: mounts.next(),
        extra_mounts: mounts.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let ticks = handle.tick_sender();

    // ONE standing book with `levels_per_side` levels per side, built before the window and never
    // mutated: every message hands the core the same `Arc`.
    let book = {
        let mut b = vike_model::L2Book::new(0.01);
        let bids: Vec<vike_model::BookLevel> = (0..levels_per_side)
            .map(|i| vike_model::BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1))
            .collect();
        let asks: Vec<vike_model::BookLevel> = (0..levels_per_side)
            .map(|i| vike_model::BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1))
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

    let spinning = gap.start_window();
    for round in 0..N as u64 {
        let t0 = Instant::now();
        for _ in 0..BURST {
            ticks
                .book(vike_exec::BookUpdate {
                    venue: "sim".into(),
                    symbol: "OTHER".into(),
                    book: Arc::clone(&book),
                })
                .unwrap();
        }
        spin_until(&processed, (round + 1) * BURST, "the burst's book messages");
        samples.push(t0.elapsed().as_nanos() as u64 / BURST);
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, samples, window);
    stats.report(label, &format!(" levels_per_side={levels_per_side} burst={BURST}"));
    // structural only: this harness prints, it does not gate on timing (see `book_lane_burst_n`)
    assert_eq!(stats.n, N, "[{label}] every round must record one sample");
}

fn run_book_hop_inner(label: &str, levels_per_side: usize, mounts: Vec<vike_core::StrategyMount>) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup so spawn -> first hop below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let mut mounts = mounts.into_iter();
    let cfg = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(b) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_seq));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        // The first mount is `strategy`, the rest `extra_mounts`. Every mount on the SAME
        // `(venue, symbol)` the producer below publishes drives the strategy lane on every message.
        strategy: mounts.next(),
        extra_mounts: mounts.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let ticks = handle.tick_sender();

    // The ONE standing book, exactly as a live pump holds it: `levels_per_side` populated levels
    // per side (a venue's full standing depth), folded in place and handed out by `Arc`.
    let mut book = {
        let mut b = vike_model::L2Book::new(0.01);
        let bids: Vec<vike_model::BookLevel> = (0..levels_per_side)
            .map(|i| vike_model::BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1))
            .collect();
        let asks: Vec<vike_model::BookLevel> = (0..levels_per_side)
            .map(|i| vike_model::BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1))
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        // Stamped BEFORE the `make_mut` — see the doc.
        let t_ns = base.elapsed().as_nanos() as u64;
        // The producer's per-update mutation. `make_mut` is the whole point: O(1) when the core has
        // already dropped the previous handle, a copy-on-write clone (on THIS thread) when it has
        // not — the live pump's `route_frame(.., Arc::make_mut(&mut book))` in miniature.
        Arc::make_mut(&mut book).last_seq = t_ns;
        ticks
            .book(vike_exec::BookUpdate {
                venue: "sim".into(),
                symbol: "OTHER".into(),
                book: Arc::clone(&book),
            })
            .unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, &format!(" levels_per_side={levels_per_side}"));
    // structural only — this harness prints, it does not gate on timing (see the test doc below)
    assert_eq!(stats.n, N, "every Ingest::Book message must be observed by the hook");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the book-lane hop at two standing depths. Prints the
/// same `LATENCY-GATE` line as the gates and deliberately asserts NO timing ceiling — unlike the
/// two fill gates above, this exists to score the full-book-clone-per-delta finding across the
/// `Arc<L2Book>` lane change, and a number that can flake under contention would poison the
/// comparison (the max under load is scheduler park, ~4ms — see the latency-gate flake note).
/// The existing gate tests above are UNCHANGED in what they measure; run this one manually, in
/// release, like them:
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture book_lane_hop_baseline
///
/// Compare against the SAME command on `main` (which still runs the owned-clone producer), on the
/// same quiet box — see [`run_book_hop`]'s doc for what confirms and what refutes the win. The
/// name is kept from the baseline PR so the two runs are trivially greppable side by side.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_baseline() {
    run_book_hop("book-hop-10lvl", 10);
    run_book_hop("book-hop-50lvl", 50);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what a MOUNTED strategy costs the book lane, and what
/// the multi-symbol declaration adds on top.
///
/// The existing gates are structurally blind here — they mount nothing, so no `LiveBroker` is
/// ever built on the measured path — while a mounted runtime constructs one per market message.
/// Three labels, read as two deltas:
///
/// - `unmounted` -> `mounted` = the runtime's per-message ctx construction.
/// - `mounted` -> `mounted-multi` = `declared_views`, the per-symbol position/mark/bar tables a
///   declared mount reads from. These are the allocations the multi-symbol lane added to a
///   per-message path; an UNDECLARED mount short-circuits on a bool and must show ~0 delta
///   against `mounted`, which is the property worth defending.
///
/// Deliberately asserts NO ceiling, exactly like [`book_lane_hop_baseline`]: a number that flakes
/// under contention would poison the comparison (the max under load is scheduler park, ~4 ms).
/// Run it the same way as the other harnesses in this file — release, `--ignored`, `--nocapture`,
/// filtered with `--exact book_lane_hop_mounted` (a bare filter also matches
/// `book_lane_hop_mounted_n`), on a quiet box.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_mounted() {
    run_book_hop("book-hop-10lvl-unmounted", 10);
    run_book_hop_mounted("book-hop-10lvl-mounted", 10, &[]);
    run_book_hop_mounted("book-hop-10lvl-mounted-multi", 10, &["SECOND"]);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what MANY mounts cost the book lane — the baseline
/// decision 0110's subscription index is judged against, and a reading of what an index would NOT
/// remove.
///
/// - `mounted-x1` -> `mounted-x20` = twenty mounts on the book's series: each built, run and
///   drained per message (`drive_strategy_tick`'s loop), PLUS the publish-side work below.
/// - `mounted-x1` -> `mounted-1of20` = one subscribed mount among twenty. Skipping the other
///   nineteen costs a few ns each; most of this delta is PUBLISH-side: a subscribed step's drain
///   dirties the core, an idle core publishes, and every publish builds a view for EVERY live mount
///   slot (`crates/vike-core/src/runtime/timers.rs`'s `mount_views`) — work a dispatch index does
///   not touch. How often a publish lands (per message, or coalesced to the snapshot interval)
///   depends on how fast the core drains, so the labels can publish at different rates.
///
/// The hop also has a floor (the producer's copy-on-write clone and send); core-side costs below it
/// do not show. Asserts NO ceiling, like the other measurements here. Run it on a quiet box:
/// release, `--ignored`, `--nocapture`, `--exact book_lane_hop_mounted_n`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_mounted_n() {
    run_book_hop_mounted_n("book-hop-10lvl-mounted-x1", 10, 1, 0);
    run_book_hop_mounted_n("book-hop-10lvl-mounted-x20", 10, 20, 0);
    run_book_hop_mounted_n("book-hop-10lvl-mounted-1of20", 10, 1, 19);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the core's time per message on the book lane under a
/// 64-message burst, as the number of mounts and the number subscribed to the book's pair vary.
///
/// The burst twin of [`book_lane_hop_mounted_n`], which cannot see a core-side cost this small: each
/// of its hops carries the producer's floor (the send, the wakeup and the dequeue, about 500 ns).
/// [`run_book_burst`] sends `BURST` (64) messages back to back and waits for all of them, so the
/// core drains a queue and a sample (one round's wall time divided by 64) is the per-message drain
/// cost, with that floor spread out of it.
///
/// Each label is `book-burst-10lvl-<mounts>`, where `<mounts>` reads as subscribed of total mounts:
/// - `x1`: one mount, subscribed;
/// - `x20`: twenty mounts, all subscribed;
/// - `1of20`: one subscribed among twenty (the other nineteen are mounted on symbols the producer
///   never publishes);
/// - `x20of100`: twenty subscribed among one hundred;
/// - `1of100`: one subscribed among one hundred.
///
/// Compare `x1` with `x20` for the cost of a subscribed mount, and `1of20` and `1of100` (or
/// `x20of100`) with their subscribed-only counterparts for the cost of a mount the message does not
/// reach.
///
/// Asserts NO ceiling (only that every round recorded a sample), like the other measurements here.
/// Run it on a quiet box: release, `--ignored`, `--test-threads=1`, `--nocapture`,
/// `--exact book_lane_burst_n`; the `LATENCY-GATE` line of each label carries `p50_ns`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_burst_n() {
    run_book_burst("book-burst-10lvl-x1", 10, 1, 0);
    run_book_burst("book-burst-10lvl-x20", 10, 20, 0);
    run_book_burst("book-burst-10lvl-1of20", 10, 1, 19);
    run_book_burst("book-burst-10lvl-x20of100", 10, 20, 80);
    run_book_burst("book-burst-10lvl-1of100", 10, 1, 99);
}

/// `mounts` [`BookProbe`] mounts for the publish harnesses, each on its OWN symbol (`PUB<k>`) that no
/// message in those harnesses names: no tick lane ever steps one, so the only work they add per
/// message is the publish side (`crates/vike-core/src/runtime/timers.rs`'s `mount_views`, which builds
/// one row per mount, and `crates/vike-core/src/snapshot/build.rs`'s `build`, which carries them).
fn publish_mounts(mounts: usize) -> Vec<vike_core::StrategyMount> {
    (0..mounts)
        .map(|k| probe_mount(&format!("PUB{k}"), Some(format!("pub-{k}")), Vec::new()))
        .collect()
}

/// The position holder of the `-moving` publish labels ([`run_publish_hop`]): it market-buys one
/// unit on the first book of its own symbol and does nothing after. The harness fills that order, so
/// the mount's ledger holds a long position whose `unrealized_pnl` and `notional` follow the engine's
/// quote. Every later book still steps this mount (it hears its own symbol), a per-message cost the
/// plain labels do not have: `-moving` is a worst-case SHAPE, and is read against itself across `N`.
struct OpenOnce {
    done: bool,
}

impl vike_model::Strategy<vike_core::LiveBroker> for OpenOnce {
    fn on_order_book(&mut self, broker: &mut vike_core::LiveBroker, _book: &vike_model::L2Book) {
        if !self.done {
            self.done = true;
            vike_model::Broker::submit_market(broker, "BTCUSDT", 1, 1.0);
        }
    }
}

/// A standing `levels_per_side` book handed out by `Arc`, never mutated (the burst harness's own).
fn standing_book(levels_per_side: usize) -> Arc<vike_model::L2Book> {
    standing_book_at(levels_per_side, 0.0)
}

/// [`standing_book`] with every price moved up by `shift`: a second book whose mid differs, so a
/// harness that alternates the two moves the engine's quote (and so a mounted position's mark) on
/// every message.
fn standing_book_at(levels_per_side: usize, shift: f64) -> Arc<vike_model::L2Book> {
    let mut b = vike_model::L2Book::new(0.01);
    let bids: Vec<vike_model::BookLevel> = (0..levels_per_side)
        .map(|i| vike_model::BookLevel::new(shift + 100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1))
        .collect();
    let asks: Vec<vike_model::BookLevel> = (0..levels_per_side)
        .map(|i| vike_model::BookLevel::new(shift + 100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1))
        .collect();
    b.apply_snapshot(1, &bids, &asks);
    Arc::new(b)
}

/// PUBLISH-side BURST throughput (a measurement, NOT a gate): [`run_book_burst`]'s shape, with
/// `mounts` mounts that no message reaches, so whatever scales with the mount count is publish work.
///
/// `publishes = true` sends every book on the ENGINE's own symbol (`BTCUSDT`): the engine accepts
/// it, so the tick lane writes the mark and dirties the core, and the core publishes whenever it
/// goes idle (and on the `snapshot_interval` cadence while busy). `publishes = false` is the CONTROL:
/// the same message on a symbol no engine accepts and no mount hears (`OTHER`), which dirties
/// nothing, so the core never publishes. That is the only way to suppress the idle publish without
/// changing production code: it ignores `CoreConfig::snapshot_interval` (`run_loop.rs` publishes
/// `if self.dirty` before every `blocking_recv`), so a long interval suppresses only the cadence one.
///
/// The `repaint` hook counts publishes (it runs once per `publish`), and the line carries the count
/// for the window (`publishes=`): how often a publish lands depends on how fast the core drains, so
/// read a sample as the drain rate WITH that many publishes in it, never as one publish's cost
/// ([`run_publish_hop`] measures that). `view_bytes=` is `size_of::<MountView>()`.
fn run_publish_burst(label: &str, mounts: usize, publishes: bool) {
    const BURST: u64 = 64;
    // The source gate (`crates/vike-core/tests/common/rt_gap/source_gate.rs`'s `SAMPLE_COUNTS`)
    // recognizes a sample loop only by these bound names, so the round loop runs to `N`.
    const N: usize = 2_000;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let processed = Arc::new(AtomicU64::new(0));
    let published = Arc::new(AtomicU64::new(0));
    let mut samples = Vec::<u64>::with_capacity(N);

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let published_hook = Arc::clone(&published);
    let mut mount_list = publish_mounts(mounts).into_iter();
    let cfg = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(_) = msg {
                processed_hook.fetch_add(1, Ordering::Release);
            }
        })),
        repaint: Some(Box::new(move || {
            published_hook.fetch_add(1, Ordering::Release);
        })),
        strategy: mount_list.next(),
        extra_mounts: mount_list.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let ticks = handle.tick_sender();
    let book = standing_book(10);
    let symbol = if publishes { "BTCUSDT" } else { "OTHER" };
    let published_before = published.load(Ordering::Acquire);

    let spinning = gap.start_window();
    for round in 0..N as u64 {
        let t0 = Instant::now();
        for _ in 0..BURST {
            ticks
                .book(vike_exec::BookUpdate {
                    venue: "sim".into(),
                    symbol: symbol.into(),
                    book: Arc::clone(&book),
                })
                .unwrap();
        }
        spin_until(&processed, (round + 1) * BURST, "the burst's book messages");
        samples.push(t0.elapsed().as_nanos() as u64 / BURST);
    }
    let window = spinning.close_window();
    let in_window = published.load(Ordering::Acquire) - published_before;
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, samples, window);
    stats.report(
        label,
        &format!(
            " mounts={mounts} burst={BURST} msgs={} publishes={in_window} view_bytes={}",
            N as u64 * BURST,
            std::mem::size_of::<vike_exec::MountView>()
        ),
    );
    // structural only: this harness prints, it does not gate on timing
    assert_eq!(stats.n, N, "[{label}] every round must record one sample");
    if publishes {
        assert!(in_window > 0, "[{label}] the publishing variant must publish");
    } else {
        assert_eq!(in_window, 0, "[{label}] the control must not publish, or it controls nothing");
    }
}

/// ONE PUBLISH per sample (a measurement, NOT a gate): the cost of a whole publish as a function of
/// the mount count, which the burst cannot isolate (it does not control how many publishes land).
///
/// Paced on the `repaint` hook rather than on the dequeue: message `i + 1` is sent only once the
/// publish message `i` caused has run, so the core is idle at every send and publishes exactly once
/// per message (`extra_publishes=` on the line says if anything else published). A sample is
/// `[send] -> [that message's publish called repaint]`: the producer's send, the wakeup, the book
/// dispatch (a mark write on the engine's own symbol, no mount stepped) and the WHOLE publish up to
/// `repaint` (`mount_views`, `snapshot::build`, the `ArcSwap` store and the drop of the previous
/// snapshot it displaces). Nothing of the mount list is freed after `repaint`: the `Vec` that
/// `mount_views` builds is MOVED into the snapshot, so the free that belongs to the list is the
/// displaced snapshot's, inside the `store`, and so inside the sample. Read the labels as a slope
/// over `mounts=`: `n0` is the intercept, and the per-mount cost is `(p50(nN) - p50(n0)) / N`.
///
/// `moving = true` is the `-moving` shape: the LAST mount (see [`OpenOnce`]) holds an open position
/// on the engine's OWN symbol, and the harness alternates two books with different mids, so every
/// message moves that position's mark and so its `unrealized_pnl` and `notional`. Whatever lets a
/// publish reuse its mount rows cannot reuse them here: this is the REBUILD-EVERY-PUBLISH worst
/// case, and the plain labels (nothing moves) are the all-unchanged best case.
fn run_publish_hop(label: &str, mounts: usize, moving: bool) {
    // Fewer than `HOP_SAMPLES`: a 200-mount publish is tens of µs, and the window should stay well
    // under `rt_gap`'s RT budget on the quiet box.
    const N: usize = 5_000;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let published = Arc::new(AtomicU64::new(0));
    let mut samples = Vec::<u64>::with_capacity(N);

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let published_hook = Arc::clone(&published);
    let mut mount_list = if moving {
        // `mounts - 1` idle mounts, then the position holder LAST: a change in the last row is the
        // worst case for a compare that walks the rows in order up to the first one that moved.
        let mut list = publish_mounts(mounts - 1);
        list.push(vike_core::StrategyMount {
            strategy: Box::new(OpenOnce { done: false }),
            ..probe_mount("BTCUSDT", Some("pub-open".to_string()), Vec::new())
        });
        list.into_iter()
    } else {
        publish_mounts(mounts).into_iter()
    };
    let cfg = CoreConfig {
        repaint: Some(Box::new(move || {
            published_hook.fetch_add(1, Ordering::Release);
        })),
        strategy: mount_list.next(),
        extra_mounts: mount_list.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let ticks = handle.tick_sender();
    // Two books: the same one twice for the plain labels (nothing moves), a second book one price
    // unit higher for `-moving`, so alternating them moves the quote on every message.
    let books =
        [standing_book(10), if moving { standing_book_at(10, 1.0) } else { standing_book(10) }];
    let send = |i: u64| {
        ticks
            .book(vike_exec::BookUpdate {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::clone(&books[(i % 2) as usize]),
            })
            .unwrap();
    };
    // One warm-up message and its publish, outside the window: the core starts clean (nothing
    // dirty, so nothing published), and from here on every publish is one message's.
    send(0);
    spin_until(&published, 1, "the warm-up message's publish");
    if moving {
        // The warm-up book made `OpenOnce` submit its market buy (the resting order is in the
        // published registry). Fill it, so the mount owns a LONG position by attribution, and wait
        // for the publish that fill causes. A venue fill is the only way a position reaches a
        // mount's ledger.
        let coid = handle
            .snapshot()
            .orders
            .first()
            .expect("the position holder's market order is registered after the warm-up book")
            .client_order_id
            .clone();
        handle
            .event_sender()
            .blocking_send(Event::Fill(FillEvent {
                trade_id: "open-fill".into(),
                client_order_id: coid,
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                last_qty: 1.0,
                last_px: 100.0,
                commission: 0.0,
                commission_asset: String::new().into(),
                liquidity_side: "taker".to_string().into(),
                ts: 0,
                mark_price: None,
                position_side: "BOTH".into(),
            }))
            .unwrap();
        spin_until(&published, 2, "the opening fill's publish");
        assert!(
            handle.snapshot().mounts.iter().any(|m| m.position != 0.0),
            "[{label}] the last mount must hold an open position, or -moving moves nothing"
        );
    }
    let base = published.load(Ordering::Acquire);

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let t0 = Instant::now();
        send(i + 1);
        spin_until(&published, base + i + 1, "this message's publish");
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    let window = spinning.close_window();
    let extra_publishes = published.load(Ordering::Acquire) - base - N as u64;
    let notional_at_end =
        handle.snapshot().mounts.iter().map(|m| m.notional).fold(0.0_f64, f64::max);
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, samples, window);
    stats.report(
        label,
        &format!(
            " mounts={mounts} moving={} extra_publishes={extra_publishes} view_bytes={}",
            u8::from(moving),
            std::mem::size_of::<vike_exec::MountView>()
        ),
    );
    if moving {
        assert!(notional_at_end > 0.0, "[{label}] the position must still be open and priced");
    }
    // structural only: this harness prints, it does not gate on timing
    assert_eq!(stats.n, N, "[{label}] every message must record one sample");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what the MOUNT COUNT costs the core through the publish
/// alone, under a 64-message burst. Every mount sits on a symbol no message names, so no tick lane
/// steps one. `publish-burst-n<N>` publishes (books on the engine's own symbol);
/// `publish-burst-n<N>-quiet` is the same burst with nothing dirtied, so nothing published: its
/// flatness across `N` is what shows the slope of the publishing labels is publish work. Read
/// [`run_publish_burst`] for the `publishes=` count each line carries.
///
/// Asserts NO ceiling. Run it on a quiet box: release, `--ignored`, `--test-threads=1`,
/// `--nocapture`, `--exact publish_burst_n`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn publish_burst_n() {
    run_publish_burst("publish-burst-n0-quiet", 0, false);
    run_publish_burst("publish-burst-n200-quiet", 200, false);
    run_publish_burst("publish-burst-n0", 0, true);
    run_publish_burst("publish-burst-n1", 1, true);
    run_publish_burst("publish-burst-n20", 20, true);
    run_publish_burst("publish-burst-n50", 50, true);
    run_publish_burst("publish-burst-n100", 100, true);
    run_publish_burst("publish-burst-n200", 200, true);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): ONE publish per sample, as the mount count grows — the
/// per-publish cost [`publish_burst_n`] cannot isolate. Read [`run_publish_hop`] for the accounting.
/// The `-moving` labels re-mark an open position on every message (the rebuild-every-publish worst
/// case); the plain ones move nothing (the all-unchanged best case).
///
/// Asserts NO ceiling. Run it on a quiet box: release, `--ignored`, `--test-threads=1`,
/// `--nocapture`, `--exact publish_hop_n`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn publish_hop_n() {
    run_publish_hop("publish-hop-n0", 0, false);
    run_publish_hop("publish-hop-n1", 1, false);
    run_publish_hop("publish-hop-n20", 20, false);
    run_publish_hop("publish-hop-n50", 50, false);
    run_publish_hop("publish-hop-n100", 100, false);
    run_publish_hop("publish-hop-n200", 200, false);
    run_publish_hop("publish-hop-n20-moving", 20, true);
    run_publish_hop("publish-hop-n100-moving", 100, true);
    run_publish_hop("publish-hop-n200-moving", 200, true);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what does a REAL `client_order_id` cost the core hop?
///
/// The two gate tests above send `client_order_id: String::new()`, and an empty `String` never
/// allocates — so the gate is STRUCTURALLY BLIND to the cost of the coid, and cannot be used to
/// justify or refute the `String -> CompactString` change (perf audit 2026-07-28, finding #4,
/// deliberately NOT taken in this PR — see the PR body). This pair makes that cost visible.
///
/// What each run measures, given the request/response pacing (one message in flight, so message
/// `i`'s DISPATCH and DROP land inside message `i+1`'s hop — the same accounting
/// [`run_core_hop`] documents):
///
/// - `coid-empty` — today's gate path. No coid allocation anywhere.
/// - `coid-live` — a 13-byte coid. As a `String` that is one `malloc` on the SENDER side per
///   message plus one `free` on the FOLD thread when `drain_delivered`'s `Vec<Event>` drops.
///
/// READING THE RESULT: the p50 DELTA between the two labels is the whole prize `CompactString`
/// could win (13 bytes is inside its 24-byte inline budget, so the change would take that delta to
/// ~0). A delta in the noise REFUTES the change — it would be several hundred mechanical edit sites
/// across 22 crates for nothing. Run on a QUIET box and read p50 before p99 (a loaded box gives an
/// 11-13 µs p99 with a healthy p50 — that is contention, not signal).
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture coid_alloc
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn coid_alloc_cost_baseline() {
    run_core_hop_with_coid("coid-empty", None, NO_COID);
    run_core_hop_with_coid("coid-live", None, LIVE_COID);
}
