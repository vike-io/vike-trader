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
//!     shows up as a loss COUNTER — `crates/vike-store/src/live_rec.rs`'s `RecorderHandle`, whose
//!     `dropped` (rows refused at a full channel) and disjoint `discarded` (rows lost writing) are
//!     the two numbers that move — not as a slow flush. A latency series over it would read like
//!     coverage of a hazard it structurally cannot see. If it is ever measured, it belongs next to
//!     the store in `vike-store`, not in a binary that runs on the reserved cores.
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
//! A fifth, for an engine that trades more than its mounted symbol (decision 0173's on-demand
//! admission, capped at 16): `snapshot-build-16sym` ([`snapshot_build_many_symbols`]) and
//! `submit-hop-16sym` / `submit-hop-16sym-foreign` ([`submit_lane_hop_many_symbols`], the second
//! submitting on an admitted symbol), each symbol holding an open position. Every other variant
//! here admits nothing, so none of them can see the per-publish `extra_symbols` clone or the
//! per-foreign-position work. `snapshot-build-16sym-priced` and `-priced-wide`
//! ([`snapshot_build_many_symbols_priced`]) are the build again over a price board with a cell per
//! held symbol (and, for `-wide`, many more), which the others leave empty. No ceiling, like the
//! rest.
//!
//! A sixth, for the QUOTE lane's price-board write ([`quote_lane_burst`], labels
//! `quote-burst-1cell` and `quote-burst-16cells`): a back-to-back burst of quotes over an engine
//! that accepts 1 or 16 symbols, so every message writes the board's existing cell. No ceiling; the
//! instrument for a change to `PriceBoard`'s `set_*` path.
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
//! order-client-ids therefore costs O(N²), and its tail measures `IndexMap` growth rather than the
//! lane it claims to. [`run_submit_hop`] pins the registry by REUSING one order client id for exactly that
//! reason.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::MarkSource;
use vike_execution::latency_budget::P99_BUDGET_NS;
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Command, CoreSnapshot, Engine, ExecutionClient, Ingest, OrderIntent,
    PriceBoardConfig, ReconcileBlock, RiskGate,
};
use vike_model::OrderRequest;
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderSubmitted};

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

/// The EMPTY order client id the two gate harnesses use. `String::new()` never allocates, so the gate has
/// always measured a hop with NO order-client-id allocation in it — which is exactly why it cannot
/// score the `order_client_id: String -> CompactString` idea (perf audit 2026-07-28, finding #4).
/// See [`order_client_id_alloc_cost_baseline`].
const NO_ORDER_CLIENT_ID: &str = "";

/// A realistically-shaped LIVE order client id: the `OrderClientIdGenerator` wire form
/// (`<8-hex session><decimal seq>`). 13 bytes — one heap allocation as a `String`, and INSIDE
/// `CompactString`'s 24-byte inline budget.
const LIVE_ORDER_CLIENT_ID: &str = "deadbeef12345";

/// A v2-shaped LIVE order client id (decision 0270): strategy `00A3`, account `07`, a 12-character unique part, a
/// long's close, take-profit leg 1. 24 bytes, the longest id that stays INSIDE `CompactString`'s
/// 24-byte inline budget (only a replacement's `R<n>` spills): D8's question for the latency job.
const LIVE_ORDER_CLIENT_ID_V2: &str = "S00A307k3x9q2m7d1p4CLTP1";

// ─────────────────────────────────────────────────────────────────────────────────────────────
// TAIL BUDGETS. Every ceiling below is DERIVED from measurements, and the history that derived
// them — the reference runs, the 2026-08-01 re-measurement (#932), the SIZING RULE, the 2026-08-05
// calibration and its harvest command, the tripwires — lives in
// `docs/ops/latency-gate-calibration.md`, moved there verbatim from this file. Each constant keeps
// a short note and points at its own section there: re-derive a ceiling from a fresh harvest,
// never nudge its literal.
// ─────────────────────────────────────────────────────────────────────────────────────────────

// The plan gate itself, `P99_BUDGET_NS` (p99 core-hop < 10 µs, the BASELINE variant only), is
// DECLARED in a library, `crates/vike-execution/src/latency_budget.rs` (the crate whose lane senders
// start the hop), so `vike-docs` publishes the very constant this harness asserts without a
// text-parsing pin or a `vike-core` edge; it is imported at the top of this file and used below like
// any sibling ceiling.

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

fn fill_with_timestamp(ts: i64) -> FillEvent {
    fill_with_order_client_id(ts, NO_ORDER_CLIENT_ID)
}

fn fill_with_order_client_id(ts: i64, order_client_id: &str) -> FillEvent {
    FillEvent {
        // A one-char CONSTANT id. This used to be `String::new()` with a comment claiming the
        // emptiness is what skipped dedup-set growth; that was never the mechanism, and `TradeId`
        // now makes it unrepresentable anyway. What actually keeps the set empty is `symbol:
        // "OTHER"` below: `vike_execution::Engine::on_event` returns `Fold::Dropped` on the
        // symbol filter BEFORE it touches `seen_trade_ids`, so a repeated id never accumulates and
        // never dedups. One char rather than a descriptive name because the journal variants weigh
        // this struct's serialized size (see the `extra` WAL arithmetic below) — 1 byte per record
        // over the old `""`, and nothing asserts on that figure.
        trade_id: "t".into(),
        order_client_id: order_client_id.to_string(),
        venue: "sim".into(),
        symbol: "OTHER".into(), // symbol-filtered out in the engine: pure dispatch cost
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
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
    /// WHERE the max sits. A max at hop #0 means a one-time first-use init (the global intern
    /// table, then `ustr`'s, used to spike ~3.2 ms there until `prewarm_interner()` moved it to core startup); a
    /// max at a random late index is ordinary OS-scheduler jitter — or, for the journal variant, a
    /// forced-sync chunk boundary.
    max_index: usize,
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
    fn from_hops(label: &str, hops_sequence: Vec<u64>, window: rt_gap::Window) -> Self {
        assert!(
            !hops_sequence.is_empty(),
            "[{label}] no samples were recorded — THIS variant's observer never fired. Read \
             whichever one it uses (the `on_dequeued` hook, the `SubmitProbe` execution client, or \
             the in-loop stopwatch); a hop that was never measured is not a fast hop."
        );
        let n = hops_sequence.len();
        let (max_index, &max) = hops_sequence.iter().enumerate().max_by_key(|&(_, &v)| v).unwrap();
        let over_100us = hops_sequence.iter().filter(|&&v| v > 100_000).count();
        let mut hops = hops_sequence;
        hops.sort_unstable();
        let pct = |p: f64| hops[((hops.len() as f64 * p) as usize).min(hops.len() - 1)];
        Self {
            n,
            p50: pct(0.50),
            p99: pct(0.99),
            p999: pct(0.999),
            max,
            max_index,
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
        let Self { n, p50, p99, p999, max, max_index, over_100us, window, busy } = *self;
        // `window_ns` and `busy_ns` ride into the persisted series like every other `k=v` here
        // (ci.yml's `series_append` carries them verbatim; every reader of that file selects fields
        // by name), so the window lengths `rt_gap` budgets are trendable, not just asserted — and
        // `busy_ns`, which nothing asserts, is trendable at all.
        let window_ns = window.as_nanos();
        let busy_ns = busy.as_nanos();
        format!(
            "LATENCY-GATE variant={label} n={n} p50_ns={p50} p99_ns={p99} p999_ns={p999} \
             max_ns={max} max_at_hop={max_index} hops_over_100us={over_100us} \
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
        max_index: 0,
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
    run_core_hop_with_order_client_id(label, journal, NO_ORDER_CLIENT_ID)
}

/// [`run_core_hop`] parameterized by the order client id each fill carries. The two GATE tests call the
/// `NO_ORDER_CLIENT_ID` wrapper above, so their measured path stays byte-identical to before this parameter
/// existed (`"".to_string()` and `String::new()` are both allocation-free).
fn run_core_hop_with_order_client_id(
    label: &str,
    journal: Option<JournalConfig>,
    order_client_id: &str,
) -> HopStats {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup so spawn -> first hop below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    // Captured before the config is moved into `CoreConfig`, so the summary line can state the
    // cadence this run actually used. `u64::MAX` (the append-isolating variant) reports 0 expected
    // snaps; `8192` (the production default since 2026-11-19, 1024 before) reports ~12 over 100 000 hops.
    let snap_every = journal.as_ref().map(|j| j.snapshot_every);
    let journal_on = journal.is_some();
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine = Engine::new(
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
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        journal,
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();

    // request/response pacing: one event in flight at a time -> measures the pure hop
    // (send -> core dispatched) including the thread wakeup, without queueing noise
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill_with_order_client_id(ts, order_client_id))).unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    // How much WAL this run writes decides how many `sync_chunk_bytes` boundaries it crosses — i.e.
    // how many chunk syncs the SYNCER thread performs (since #929 it performs all of them; the
    // append only posts a watermark). journal.rs frames each record as `[len][crc][payload]`,
    // 8 bytes plus a msgpack payload (format 34), and that payload is a `JournalRecordRef::Command` WRAPPING
    // this exact `Ingest` — so encoding the `Ingest` alone is a strict LOWER bound (the wrapper
    // adds a variant tag plus `seq`/`now_ms`). A representative 10-digit `ts` is used because the
    // real ones are elapsed-nanos, not zero.
    //
    // `snap_every=` / `snaps_expected=` are the twin of that arithmetic for the CADENCE SNAPSHOT:
    // one `Snapshot` per `snapshot_every` journaled records, each of which used to take a blocking
    // whole-segment `msync` on this very thread. Diagnostic only; nothing asserts on either.
    let extra = if journal_on {
        let ingest =
            Ingest::Event(Event::Fill(fill_with_order_client_id(1_000_000_000, order_client_id)));
        let record = vike_journal::payload_of_value(
            &serde_json::to_value(&ingest).expect("Ingest serializes"),
        )
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

    let hops = Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap();
    print_timeline(label, &hops, snap_every);
    let stats = HopStats::from_hops(label, hops, window);
    stats.report(label, &extra);
    stats
}

/// A diagnostic, not a gate: the per-hop timeline
/// behind a variant's max, written to stderr after the window closed. `TOP` = the 12 slowest hops with
/// their offset after the last cadence snap (`index % snap_every`, the snap's own message lands in the hop
/// at offset 0); `TL` = every hop within 12 of the max; `SLOW` = how many hops took over 100 µs / 1 ms,
/// and the snap offsets of the ones over 1 ms.
fn print_timeline(label: &str, hops: &[u64], snap_every: Option<u64>) {
    let every = snap_every.filter(|&e| e != u64::MAX).unwrap_or(0) as usize;
    let off = |i: usize| if every == 0 { i } else { i % every };
    let mut index: Vec<usize> = (0..hops.len()).collect();
    index.sort_unstable_by_key(|&i| std::cmp::Reverse(hops[i]));
    let mut err = std::io::stderr();
    for &i in index.iter().take(12) {
        let _ = writeln!(err, "TOP {label} hop={i} ns={} snap_off={}", hops[i], off(i));
    }
    if let Some(&m) = index.first() {
        let lo = m.saturating_sub(12);
        for (i, ns) in hops.iter().enumerate().skip(lo).take(25) {
            let _ =
                writeln!(err, "TL {label} hop={i} ns={ns}{}", if i == m { " <= max" } else { "" });
        }
    }
    let over_ms: Vec<String> = (0..hops.len())
        .filter(|&i| hops[i] > 1_000_000)
        .map(|i| format!("{i}/{}", off(i)))
        .collect();
    let over_100us = hops.iter().filter(|&&h| h > 100_000).count();
    let _ = writeln!(
        err,
        "SLOW {label} over_100us={over_100us} over_1ms={} at(hop/snap_off)={}",
        over_ms.len(),
        over_ms.join(",")
    );
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
        s.max_index,
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
/// `[sinks.journal]` both default to **8192** (1024 until 2026-11-19), and a cadence `Snapshot` runs on the fold thread — so
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
/// cost. But `JournalConfig::at` and the `[sinks.journal]` profile default are **8192** (1024 until 2026-11-19), so a
/// production node takes a cadence `Snapshot` every 8192 exec-lane records, and `CoreThread::write_snap`
/// runs on the FOLD THREAD (its encode on the journal's write thread since that day). The gate was therefore configured differently from production in
/// exactly the dimension that mattered, and was structurally blind to whatever `write_snap` costs:
/// no snap ever fired inside a measurement.
///
/// It cost a real defect a long life. `write_snap` ended each snapshot with
/// `Journal::flush()` — the blocking whole-mapping `msync` whose own doc says it is "not safe
/// to call from the vike-core fold" — so a journaling node paid one on the fold thread every 1024
/// messages. #929 moved the OTHER two blocking `msync`s (the chunk sync and the roll seal) off that
/// thread and this one survived, invisible, because no harness variant ever fired a snap.
///
/// The config here is [`JournalConfig::at`] VERBATIM (64 MiB segments, `flush_every` 256,
/// `snapshot_every` 8192) rather than a hand-built near-copy, so this test cannot drift away from
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

// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE FOREIGN-QUOTED VARIANTS (multi-currency; measurements, no ceiling, like every variant below the
// three gates). The gates above fold nothing into an account (their fills are symbol-filtered out),
// so none of the code a symbol quoted in a currency other than the account's adds is on their path
// except the per-publish rate table. These variants put it there: fills that FOLD on admitted
// symbols quoted in four foreign currencies (the fee branch that converts a commission, the contract
// value that carries the rate), each symbol holding a position, the engine on a 4x engine-wide margin
// rate with every other symbol stating its own (so the published block does the full margin fold),
// and, in the `-refresh` forms, a rate refresh (the same admission with a new rate and a new
// `rate_as_of_ms`) every [`FX_REFRESH_EVERY`] hops. Read them against the SAME label measured on the
// base: this file compiles against a core that has none of the new fields, because the admissions
// are built from JSON and the per-symbol margin row is also written straight onto the limits, so on
// the base the same shape runs with the quote keys ignored (the symbols are then quoted in the
// account's own currency) and the delta is what the multi-currency code adds.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A rate refresh every this many hops: about 3-4 ms apart at the baseline's pace, so ~300x denser than
/// the production cadence (about one a minute while a foreign-quoted position is held). The point
/// is that the cold command's cost (a copy-on-write of the contract-value grid, the strings it
/// clones) is exercised and lands in the hop after it, which a once-a-second cadence would put in
/// one hop of a hundred thousand and no tail percentile would show.
const FX_REFRESH_EVERY: usize = 2_000;

/// The currencies the quoted symbols cycle through and the rate of each (account currency per unit,
/// USD): JPY, GBP, CHF, CAD.
const FX_QUOTES: [(&str, f64); 4] =
    [("JPY", 1.0 / 150.0), ("GBP", 1.35), ("CHF", 1.12), ("CAD", 0.73)];

/// The margin rate every even-numbered quoted symbol states for itself (the odd ones follow the
/// engine-wide 0.25): a 2x symbol.
const FX_SYMBOL_INITIAL_MARGIN: f64 = 0.5;

/// The `i`-th quoted symbol's name.
fn fx_symbol(i: usize) -> String {
    format!("FXQ{i}")
}

/// The admission of the `i`-th quoted symbol at `as_of_ms`, built from JSON so this file compiles
/// against a core without the quote fields (see the section header). Every even symbol also states
/// its own margin rate.
fn fx_admission(route_key: &str, i: usize, as_of_ms: i64) -> vike_execution::SymbolAdmission {
    let (currency, rate) = FX_QUOTES[i % FX_QUOTES.len()];
    let mut admission = serde_json::json!({
        "route_key": route_key,
        "symbol": fx_symbol(i),
        "grid": {"tick_size": 0.01, "lot_size": 0.0001},
        "initial_mark": 100.0,
        "quote_currency": currency,
        "quote_rate": rate,
        "rate_as_of_ms": as_of_ms,
    });
    if i.is_multiple_of(2) {
        admission["initial_margin_requirement"] = serde_json::json!(FX_SYMBOL_INITIAL_MARGIN);
    }
    serde_json::from_value(admission).expect("a SymbolAdmission")
}

/// An engine on a 4x engine-wide margin rate that has admitted `quoted` symbols quoted in the
/// currencies above, each holding one marked long position. Like [`admit_foreign_symbols`], done
/// BEFORE the core spawns so no event traffic lands in the run. The even symbols' own margin row is
/// written onto the limits as well as sent in the admission, so the base (which ignores the
/// admission's key) holds the same state.
fn fx_engine(quoted: usize) -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { initial_margin_requirement: Some(0.25), ..RiskLimits::new() }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    engine.quote_asset = "USD".to_string();
    for i in 0..quoted {
        let symbol = fx_symbol(i);
        let admission = fx_admission(&engine.route_key, i, 0);
        assert_eq!(
            engine.admit_symbol(&admission),
            vike_execution::AdmitOutcome::Admitted,
            "{symbol} must join the engine (is `quoted` above the cap?)"
        );
        if i.is_multiple_of(2) {
            engine
                .gate
                .limits
                .initial_margin_by_symbol
                .insert(symbol.clone(), FX_SYMBOL_INITIAL_MARGIN);
        }
        engine.account.positions.insert(
            ("sim".into(), symbol.as_str().into(), vike_model::events::PositionSide::Long),
            vike_execution::PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
        );
        engine.account.set_mark_from("sim", &symbol, 101.0, MarkSource::VenueMark, 0);
    }
    engine
}

/// The `i`-th hop's fill: on quoted symbol `i % quoted`, a DISTINCT trade id (so the fill folds
/// instead of dedup-dropping), buying and selling in turn, with a commission in no named asset on
/// even hops and in the symbol's own quote currency on odd ones. With `third` every commission is in
/// a THIRD currency instead: the next quote currency of the cycle, which the book also holds a rate
/// for (a core without the third-currency rule leaves such a fee out of the ledger and counts it).
fn fx_fill(i: usize, quoted: usize, third: bool) -> FillEvent {
    let k = i % quoted;
    FillEvent {
        trade_id: vike_model::events::TradeId::new(format!("fx{i}")).expect("a non-empty id"),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: fx_symbol(k).as_str().into(),
        side: if (i / quoted).is_multiple_of(2) { 1 } else { -1 },
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.05,
        commission_asset: if third {
            FX_QUOTES[(k + 1) % FX_QUOTES.len()].0.into()
        } else if i.is_multiple_of(2) {
            String::new().into()
        } else {
            FX_QUOTES[k % FX_QUOTES.len()].0.into()
        },
        liquidity_side: String::new().into(),
        timestamp: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Long,
    }
}

/// [`run_core_hop`] on an engine that trades `quoted` foreign-quoted symbols (see the section header):
/// fills that fold, one per hop, and, when `refresh_every` is not `0`, a rate refresh admission every
/// that many hops. A hop is the fill's send-to-dequeue, so the fold of the previous message (and the
/// refresh's admission, and the publish) lands inside it, as in every sibling variant.
fn run_core_hop_fx(
    label: &str,
    journal: Option<JournalConfig>,
    quoted: usize,
    refresh_every: usize,
    third: bool,
) -> HopStats {
    const N: usize = HOP_SAMPLES;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let journal_on = journal.is_some();
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));
    let engine = fx_engine(quoted);
    let route_key = engine.route_key.clone();
    // Built before the window: the ids and the symbols are the only per-hop allocation, and they
    // are not the measurement.
    let mut fills: Vec<Option<FillEvent>> =
        (0..N).map(|i| Some(fx_fill(i, quoted, third))).collect();
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        // The core's clock IS the harness clock, so a refresh's `rate_as_of_ms` is never in the
        // engine's future and never stale.
        clock: Box::new(move || base.elapsed().as_millis() as i64),
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        journal,
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    // Messages sent so far, fills and refreshes both: the pacing waits for the core to have
    // dequeued every one before the next goes out.
    let mut sent: u64 = 0;
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        if refresh_every > 0 && i > 0 && i % refresh_every as u64 == 0 {
            let as_of_ms = base.elapsed().as_millis() as i64;
            let which = i as usize / refresh_every % quoted;
            let refresh = fx_admission(&route_key, which, as_of_ms);
            handle.send_command(Command::AdmitSymbol(Box::new(refresh)));
            sent += 1;
            spin_until(&processed, sent, "the rate refresh");
        }
        let mut fill = fills[i as usize].take().expect("each fill goes out once");
        fill.timestamp = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill)).unwrap();
        sent += 1;
        spin_until(&processed, sent, "the fill");
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();
    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(
        label,
        &format!(
            " quoted={quoted} refresh_every={refresh_every} journal={journal_on} third={third}"
        ),
    );
    stats
}

/// Four quoted symbols, no refresh: the fold and the publish of an engine holding foreign-quoted
/// positions, nothing cold in the run.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn fx_hop_quoted_symbols() {
    run_core_hop_fx("fx-4sym", None, 4, 0, false);
}

/// Four quoted symbols, every commission in a THIRD currency the book holds a rate for (decision 0183,
/// phase 2): the branch that converts it. On a base without the rule the same shape leaves the fee out
/// and counts it, so read the delta against the same label there.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn fx_hop_quoted_symbols_third_fee() {
    run_core_hop_fx("fx-4sym-third-fee", None, 4, 0, true);
}

/// The cap's worth of quoted symbols with a rate refresh every [`FX_REFRESH_EVERY`] hops: the
/// worst case of the in-memory path. Read against `fx-4sym` for the refresh and the 16 symbols, and
/// against the same label on the base for what the multi-currency code adds.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn fx_hop_quoted_symbols_refresh() {
    run_core_hop_fx("fx-16sym-refresh", None, FOREIGN_SYMBOLS, FX_REFRESH_EVERY, false);
}

/// The same with the production journal configuration ([`JournalConfig::at`]): every refresh is a
/// journaled admission (the first raises the segment header), and the cadence `Snapshot` carries the rate
/// table.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn fx_hop_quoted_symbols_refresh_journal_snapshots() {
    let dir = std::env::temp_dir().join(format!("vjl-latency-fx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    run_core_hop_fx(
        "fx-16sym-refresh-journal-snap",
        Some(JournalConfig::at(dir.clone())),
        FOREIGN_SYMBOLS,
        FX_REFRESH_EVERY,
        false,
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The engine symbol of the `fx-mount-foreign` variant.
const FX_MOUNT: &str = "BTCUSDT";

/// EUR per ONE USD, the rate of the `fx-mount-foreign` engine's mount (quoted in USD on a EUR ledger).
const FX_MOUNT_RATE: f64 = 0.86;

/// The mount's own admission at `as_of_ms`: the mounted symbol quoted in USD on a EUR ledger, the
/// engine's own symbol, which a candidate rates like any foreign-quoted symbol and a base answers
/// `Mounted` and writes nothing for (built from JSON, like [`fx_admission`], so this file compiles
/// against a core that has none of the mounted-symbol amendment).
fn fx_mount_admission(route_key: &str, as_of_ms: i64) -> vike_execution::SymbolAdmission {
    let admission = serde_json::json!({
        "route_key": route_key,
        "symbol": FX_MOUNT,
        "grid": {"tick_size": 0.01, "lot_size": 0.0001},
        "initial_mark": 100.0,
        "quote_currency": "USD",
        "quote_rate": FX_MOUNT_RATE,
        "rate_as_of_ms": as_of_ms,
    });
    serde_json::from_value(admission).expect("a SymbolAdmission")
}

/// A EUR ledger mounted on [`FX_MOUNT`] (quoted in USD) with its mount rated and one marked long
/// position on it. Done BEFORE the core spawns, like [`fx_engine`]. The currency is told through
/// `set_account_currency` (it exists on a base), and the mount's rate rides the admission, so the SAME
/// harness runs on both sides of the A/B: on the base the mount stays booked at par in its own money
/// (the admission is a no-op there) and on the candidate it is a foreign-quoted symbol.
fn fx_mount_engine() -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { initial_margin_requirement: Some(0.25), ..RiskLimits::new() }),
        RecordingClient::default(),
        "sim",
        FX_MOUNT,
    );
    engine.set_account_currency("EUR").expect("a currency code");
    assert_eq!(
        engine.admit_symbol(&fx_mount_admission(&engine.route_key, 0)),
        vike_execution::AdmitOutcome::Mounted,
        "the mounted symbol's admission"
    );
    engine.account.positions.insert(
        ("sim".into(), FX_MOUNT.into(), vike_model::events::PositionSide::Long),
        vike_execution::PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    engine.account.set_mark_from("sim", FX_MOUNT, 101.0, MarkSource::VenueMark, 0);
    engine
}

/// The `i`-th hop's fill on the MOUNT: a distinct trade id, buying and selling in turn, with a
/// commission in no named asset on even hops and in the mount's quote currency (USD) on odd ones.
fn fx_mount_fill(i: usize) -> FillEvent {
    FillEvent {
        trade_id: vike_model::events::TradeId::new(format!("fxm{i}")).expect("a non-empty id"),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: FX_MOUNT.into(),
        side: if i.is_multiple_of(2) { 1 } else { -1 },
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.05,
        commission_asset: if i.is_multiple_of(2) { String::new().into() } else { "USD".into() },
        liquidity_side: String::new().into(),
        timestamp: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Long,
    }
}

/// [`run_core_hop_fx`] on an engine whose MOUNTED symbol is the foreign-quoted one: fills that fold on the
/// mount (the fee branch that converts, the contract value that carries the rate), one per hop, and the
/// mount's own rate refresh every [`FX_REFRESH_EVERY`] hops. The shape of an engine on a EUR FXCM, OANDA
/// or cTrader account, where the mount is permanently foreign-quoted and so pays what `fx-4sym` shows for
/// every fill it folds. Read against the SAME label on the base, where the identical run folds the mount
/// at par (no fx book at all): the delta is what the mounted-symbol amendment costs a non-USD account.
fn run_core_hop_fx_mount(label: &str) -> HopStats {
    const N: usize = HOP_SAMPLES;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));
    let engine = fx_mount_engine();
    let route_key = engine.route_key.clone();
    let mut fills: Vec<Option<FillEvent>> = (0..N).map(|i| Some(fx_mount_fill(i))).collect();
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        // The core's clock IS the harness clock, so a refresh's `rate_as_of_ms` is never in the
        // engine's future and never stale.
        clock: Box::new(move || base.elapsed().as_millis() as i64),
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    let mut sent: u64 = 0;
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        if i > 0 && i % FX_REFRESH_EVERY as u64 == 0 {
            let as_of_ms = base.elapsed().as_millis() as i64;
            handle.send_command(Command::AdmitSymbol(Box::new(fx_mount_admission(
                &route_key, as_of_ms,
            ))));
            sent += 1;
            spin_until(&processed, sent, "the mount's rate refresh");
        }
        let mut fill = fills[i as usize].take().expect("each fill goes out once");
        fill.timestamp = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill)).unwrap();
        sent += 1;
        spin_until(&processed, sent, "the fill");
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();
    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, &format!(" refresh_every={FX_REFRESH_EVERY}"));
    stats
}

/// The mounted symbol quoted in a currency other than the ledger's (a EUR account on a USD-quoted mount):
/// the engine's permanent state on a non-USD FXCM, OANDA or cTrader account (decision 0183, the
/// mounted-symbol amendment). Fills fold on the mount and the rate refreshes. Non-gating. Read against the
/// same label on the base.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn fx_hop_mount_foreign() {
    run_core_hop_fx_mount("fx-mount-foreign");
}

/// [`run_core_hop`] on a core of TWO engines, the second (`oanda`) TOLD its currency (decision 0183, item 13):
/// the publish after every fill builds both venue blocks and folds the totals of the PRIMARY's currency group.
/// `told == "USD"` is the control (one group: the filters pass both blocks); `"EUR"` makes a second group, so
/// the second block is left out of the totals and `venue_block` copies a non-dollar key. A hop is the fill's
/// send-to-dequeue as everywhere, so the previous message's publish lands inside it. Before the key was
/// cached in the engine this path interned the code once per engine per publish, which no other variant
/// (they all tell `"USD"`, or nobody) could show.
fn run_core_hop_currency(label: &str, told: &str) -> HopStats {
    const N: usize = HOP_SAMPLES;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));
    let engine_on = |venue: &str, symbol: &str| {
        Engine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            venue,
            symbol,
        )
    };
    let mut other = engine_on("oanda", "EURUSD");
    other.set_account_currency(told).expect("a currency code");
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        ..CoreConfig::default()
    };
    let handle =
        vike_core::spawn_core_multi(engine_on("sim", "BTCUSDT"), vec![(1_000.0, other)], config);
    let sender = handle.event_sender();
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        sender
            .blocking_send(Event::Fill(fill_with_order_client_id(ts, NO_ORDER_CLIENT_ID)))
            .unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();
    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, &format!(" told={told}"));
    stats
}

/// MEASUREMENT (not a gate): the currency-group key on the publish path. `ccy-usd-told` is the control (two
/// engines, one group), `ccy-eur-told` the second group. Read the pair against `baseline` for the second
/// engine's publish and against each other for the group. Asserts NO ceiling, like the other measurements.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn core_hop_currency_groups() {
    run_core_hop_currency("ccy-usd-told", "USD");
    run_core_hop_currency("ccy-eur-told", "EUR");
}

/// Builds per [`run_snapshot_build`] variant. Fewer than [`HOP_SAMPLES`] on purpose: a build is
/// ~1-2 orders of magnitude dearer than a core hop, and this is the sample count the harness this
/// replaced already used, so the p50 stays comparable with whatever anyone quoted from it.
const SNAPSHOT_BUILDS: usize = 10_000;

/// Foreign symbols the `16sym` variants admit: the engine's own cap, so the label names the
/// WORST case an engine can be in (decision 0173) and cannot drift from it
/// ([`the_many_symbol_variants_sit_at_the_engine_cap`]).
const FOREIGN_SYMBOLS: usize = vike_execution::MAX_EXTRA_SYMBOLS;

/// Price-board cells of the `-priced-wide` form: far more than the engine holds positions in.
const WIDE_BOARD_CELLS: usize = 128;

/// The `i`-th foreign symbol's name; distinct from the mounted `BTCUSDT` and from each other.
fn foreign_symbol(i: usize) -> String {
    format!("FOREIGN{i}USDT")
}

/// Admit `foreign` symbols into `engine` and give each ONE open long position with a venue mark,
/// so the per-foreign-symbol work a publish does is all live: `build` clones `extra_symbols`, and
/// `resolve_equity` and `position_views` take one entry per position.
///
/// Through `Engine::admit_symbol`, the body of `Command::AdmitSymbol`
/// (`crates/vike-core/src/runtime/dispatch.rs` calls it and adds one ring note), so the engine ends
/// in the state the command leaves; the symbols' grids ride `grid_by_symbol` as they do live.
/// Done BEFORE the measured window and, for a core, BEFORE `spawn_core`, so no event traffic and no
/// ring note lands in the run: the recent-events ring stays as empty as in every sibling variant,
/// and the delta against the `0` form of the same label is the foreign symbols alone. The position
/// is written straight onto the account, like the mounted one in [`run_snapshot_build`]; the
/// fill/admit command path itself is `tests/wiring/admit_symbol.rs`'s subject.
fn admit_foreign_symbols<C: ExecutionClient>(engine: &mut Engine<C>, foreign: usize) {
    for i in 0..foreign {
        let symbol = foreign_symbol(i);
        let admission = vike_execution::SymbolAdmission {
            route_key: engine.route_key.clone(),
            symbol: symbol.clone(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.0001),
                ..Default::default()
            },
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        };
        assert_eq!(
            engine.admit_symbol(&admission),
            vike_execution::AdmitOutcome::Admitted,
            "{symbol} must join the engine (is `foreign` above the cap?)"
        );
        engine.account.positions.insert(
            ("sim".into(), symbol.as_str().into(), vike_model::events::PositionSide::Long),
            vike_execution::PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
        );
        engine.account.set_mark_from("sim", &symbol, 101.0, MarkSource::VenueMark, 0);
    }
}

/// NOT `#[ignore]`d: the `16sym` labels are only honest while they sit at the cap, and this runs
/// on the fast lane (the the latency box latency job runs `--ignored` and skips it).
#[test]
fn the_many_symbol_variants_sit_at_the_engine_cap() {
    assert_eq!(FOREIGN_SYMBOLS, 16, "the `16sym` labels name the cap; rename them with it");
}

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
/// A SECOND axis, `foreign` (default `0`, which is every label above): symbols admitted beyond the
/// mounted one, each holding one marked position ([`admit_foreign_symbols`]). It prices what an
/// on-demand engine adds to EVERY publish: the `extra_symbols` `String` clone per venue block, one
/// `resolve_equity` step and one `position_views` row per foreign position, and the
/// `top_positions` copy. Read `snapshot-build-16sym` against `snapshot-build-0ord`, same `orders`.
///
/// A THIRD axis, `board_cells` (default `0`: an empty price board, so every position resolves
/// `Missing` without a lookup): the board a live engine has, a cell per held symbol and `board_cells`
/// in all. It prices the resolver's lookup per position, which only a filled board exercises.
///
/// # Accounting, precisely
///
/// The window contains `build` and NOTHING else: the previous snapshot is freed BEFORE `t0`, so a
/// sample is never charged for its predecessor's `Vec`/`Arc` teardown. A real publish pays that too
/// (`arc_swap` defers it to whichever thread drops the last handle), so the honest reading is that
/// these numbers are the BUILD, and a publish is the build plus a free. `ReconcileBlock::default()` is
/// constructed inside the window because `build` takes it by value — it is an empty-`Vec` `Default`,
/// which allocates nothing.
fn run_snapshot_build(label: &str, orders: usize, foreign: usize, board_cells: usize) -> HopStats {
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    for i in 0..orders {
        let order_client_id = format!("o{i}");
        let req = OrderRequest {
            order_client_id: order_client_id.clone(),
            venue: "sim".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            ..OrderRequest::default()
        };
        engine.registry.insert(order_client_id, vike_model::OrderRecord::new(req));
    }
    engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Long),
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
    );
    engine.account.set_mark_from("sim", "BTCUSDT", 101.0, MarkSource::VenueMark, 0);
    admit_foreign_symbols(&mut engine, foreign);
    if board_cells > 0 {
        // The price board a live engine fills from its feeds: a cell for every symbol it holds, so
        // the resolver finds a mark where the account-only form above leaves every position
        // unpriced, and cells for symbols it holds nothing in (a chart feed, a watched symbol) up to
        // `board_cells` in all. Those come FIRST: the board is insertion-ordered, so a scan for a
        // held symbol walks past every one of them.
        for i in (1 + foreign)..board_cells {
            engine.price_board.set_mark("sim", &format!("BOARD{i}USDT"), 101.0, 0);
        }
        engine.price_board.set_mark("sim", "BTCUSDT", 101.0, 0);
        for i in 0..foreign {
            engine.price_board.set_mark("sim", &foreign_symbol(i), 101.0, 0);
        }
    }
    // The ring holds RENDERED lines (2026-07-29): the runtime formats each event at PUSH, so this
    // harness measures what `snapshot::build` actually does — clone a refcount per entry.
    let recent_cap = CoreConfig::default().recent_events_cap;
    let recent: std::collections::VecDeque<std::sync::Arc<str>> = (0..recent_cap)
        .map(|i| {
            std::sync::Arc::from(
                vike_core::RecentNote::Event(vike_core::EventNote::capture(&Event::OrderFilled(
                    vike_model::events::OrderFilled {
                        order_client_id: format!("o{i}"),
                        fill: fill_with_timestamp(0),
                        timestamp: 0,
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
    // Kept across the builds, as the core keeps it across its publishes.
    let mut windows = vike_core::snapshot::OrderWindows::default();
    let spinning = gap.start_window();
    for seq in 0..SNAPSHOT_BUILDS as u32 {
        drop(last.take()); // the predecessor's free, kept OUT of this sample's window
        let t0 = base.elapsed().as_nanos() as u64;
        let snap = vike_core::snapshot::build(
            u64::from(seq),
            &engine,
            &[],
            10_000.0,
            PriceBoardConfig::default(),
            vike_model::MarginCallConfig::default().maintenance_margin_requirement,
            &recent,
            &bars,
            std::sync::Arc::new([]),
            &None,
            ReconcileBlock::default(),
            Vec::new(),
            &mut windows,
        );
        let t1 = base.elapsed().as_nanos() as u64;
        samples.push(t1.saturating_sub(t0));
        last = Some(snap);
    }
    let window = spinning.close_window();

    let stats = HopStats::from_hops(label, samples, window);
    let board_key =
        if board_cells > 0 { format!(" board_cells={board_cells}") } else { String::new() };
    stats.report(
        label,
        &format!(" orders={orders} recent={recent_cap} foreign={foreign}{board_key}"),
    );
    // Structural, not timing: prove the harness built what its label claims (and keep the last
    // snapshot alive to the end of the loop, so no build can be optimized away).
    let last = last.expect("SNAPSHOT_BUILDS > 0, so a snapshot was built");
    assert_eq!(
        last.orders.len(),
        orders,
        "[{label}] the built snapshot must carry every registry order"
    );
    assert_eq!(
        last.portfolio.venues[0].extra_symbols.len(),
        foreign,
        "[{label}] the block must carry every admitted symbol (the clone `build` pays per publish)"
    );
    assert_eq!(
        last.positions.len(),
        1 + foreign,
        "[{label}] the mounted position plus one per foreign symbol"
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
    run_snapshot_build("snapshot-build-0ord", 0, 0, 0);
    let historical = run_snapshot_build("snapshot-build-64ord", 64, 0, 0);
    run_snapshot_build("snapshot-build-512ord", 512, 0, 0);
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

/// PERF-PROGRAM MEASUREMENT (not a gate): `snapshot::build` with [`FOREIGN_SYMBOLS`] admitted
/// symbols, each with an open position (decision 0173's cap, the worst case), against
/// `snapshot-build-0ord` as its control. `snapshot-build-0ord` is the same engine with nothing
/// admitted, so the pair's delta is the two per-publish costs on-demand symbols add. Read
/// [`run_snapshot_build`]'s "A SECOND axis". No timing ceiling, for the reason the module doc gives.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --test-threads=1 --nocapture --exact snapshot_build_many_symbols
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn snapshot_build_many_symbols() {
    run_snapshot_build("snapshot-build-16sym", 0, FOREIGN_SYMBOLS, 0);
}

/// PERF-PROGRAM MEASUREMENT (not a gate): [`snapshot_build_many_symbols`] on an engine whose PRICE
/// BOARD holds a cell for every symbol it holds (`snapshot-build-16sym-priced`), against
/// `snapshot-build-0sym-priced` (nothing admitted, the one mounted cell) as its control. The
/// unpriced forms leave the board empty, so every position resolves `Missing` without a lookup;
/// a live engine's board has a cell per symbol, and each position's resolve is a lookup in it.
/// `snapshot-build-16sym-priced-wide` adds [`WIDE_BOARD_CELLS`] cells in all (symbols watched, not
/// held), which is what a scan-per-position lookup pays for and a hashed one does not.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --test-threads=1 --nocapture --exact snapshot_build_many_symbols_priced
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn snapshot_build_many_symbols_priced() {
    run_snapshot_build("snapshot-build-0sym-priced", 0, 0, 1);
    run_snapshot_build("snapshot-build-16sym-priced", 0, FOREIGN_SYMBOLS, 1 + FOREIGN_SYMBOLS);
    run_snapshot_build("snapshot-build-16sym-priced-wide", 0, FOREIGN_SYMBOLS, WIDE_BOARD_CELLS);
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
                 calls no `submit`, so this harness would wait forever (so does the core's refusal \
                 of an explicit order client id it holds answered or finished, `answered_order_client_id_status`: a \
                 submit probe that answers its orders trips it). Read the engine's limits \
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
        self.hops.lock().unwrap().push(now_ns.saturating_sub(request.timestamp as u64));
        self.submitted.fetch_add(1, Ordering::Release);
    }
    fn cancel(&mut self, _order_client_id: &str) {}
}

/// ORDER-SUBMIT lane hop (perf-program measurement scaffolding — the sibling of [`run_core_hop`] and
/// [`run_book_hop`], NOT a gate). It measures the path that actually places an order, which no gate
/// in this file has ever touched.
///
/// # Why the gates are blind to it
///
/// Every gate here feeds `Ingest::Event(Event::Fill)` — the venue-REPLY lane. A submit arrives on a
/// different arm entirely, `Ingest::Command(Command::Order(OrderIntent::Submit))`, and runs
/// completely different code: `crates/vike-core/src/runtime/apply.rs`'s `apply_intent` (order client id resolve
/// → `vike_venues::preflight_order` → contingency-link classify) then
/// `crates/vike-execution/src/execution_engine/mod.rs`'s `submit_order` → `gate_and_register` (the
/// `RiskGate` crossing, a `OrderRecord::new` and a registry insert) → `client.submit`. None of that
/// is on the fill path, so a regression anywhere in it is invisible to every ceiling above.
///
/// # Where "submit → wire" honestly stops
///
/// At the `ExecutionClient` seam, and no further. Beyond it sits a per-venue `ExecutionClientActor` thread, a
/// signer and a blocking REST/WS round trip — network, credentials and a live venue, none of which
/// can be measured deterministically in-process, and all of which would make this a smoke test
/// rather than a series. The seam IS the last point the core controls, which makes it the right
/// boundary: everything this harness reports is something a change in this workspace can move.
///
/// # Accounting, and why it matches the other hop harnesses
///
/// One message in flight, paced on the probe (`submit` entered ⇒ the response). The interval is
/// `[intent stamped] → [venue client entered]`, carried on `OrderRequest.ts` — which survives the
/// `RiskGate` verbatim (`crates/vike-execution/src/risk.rs`'s `check_inner` clones the request and never
/// assigns `ts`), the same trick [`run_core_hop`] plays with `FillEvent.ts` and [`run_book_hop`]
/// with `book.last_sequence`. As in those, message `i-1`'s post-submit TAIL — outbox drive, `pump_client`,
/// `drain_delivered` and the idle `CoreSnapshot` publish — lands inside message `i`'s window. That
/// is deliberate here: the publish is exactly what the two variants are meant to price.
///
/// # `resting` is the axis, and the order client id is FIXED for a hard reason
///
/// Every measured submit reuses ONE order-client-id, so `registry.insert` REPLACES its entry and the
/// registry stays at `resting + 1` for the whole run. That is not a shortcut: the idle publish walks
/// the registry once per message, so a harness minting a fresh order client id per submit would be O(N²) and
/// its tail would report `IndexMap` growth rather than this lane (module doc, "The trap a new
/// harness here will hit first").
///
/// Read the two labels as one delta: `0resting` is the submit path with a trivial publish behind it;
/// `32resting` is the same path with a maker-sized resting book, i.e. what a live quoting strategy
/// actually pays. A delta that grows faster than the order count means the per-message publish, not
/// the submit.
///
/// ⚠ **Since decision 0207 the fixed order client id is also what the core's held-order-client-id refusal lets through, and
/// that is why this harness is unchanged.** `lower_submit` refuses an explicit order client id its engine already
/// holds ANSWERED or FINISHED (`crates/vike-core/src/runtime/refusals.rs`'s `answered_order_client_id_status`,
/// one `registry.get` per explicit-order-client-id submit, which is the added work this hop now prices) and lets
/// an UNANSWERED one (`INITIALIZED`, `SUBMITTED`) through as the in-flight retry. The [`SubmitProbe`]
/// answers nothing, so the order under [`LIVE_ORDER_CLIENT_ID`] stays `INITIALIZED` for the whole run and every
/// measured submit is lowered exactly as before. The alternative this harness was asked to take, cycling
/// a few order client ids, cannot work against a refusal of every held id: the registry never evicts, so the
/// (K+1)-th use of K order client ids is a held id and a pinned registry needs the one thing that refusal forbids;
/// N distinct ids instead is the O(N²) trap above. A probe that DID answer (an `Accepted` event)
/// would trip the liveness guard at the second sample, by design.
///
/// # `foreign` and `symbol`: the second axis
///
/// `foreign` symbols are admitted (each with an open position) before the core spawns
/// ([`admit_foreign_symbols`]); `symbol` is the symbol every measured submit carries: the mounted
/// `BTCUSDT`, or an admitted one. `foreign = 0` is every label that predates the axis.
///
/// # Two stated residuals
///
/// The order client id MINT is excluded — a submit with an empty order client id makes the runtime call
/// `OrderClientIdGenerator::generate`, and that is precisely the shape that cannot keep the registry
/// pinned. And no venue events arrive here, so the recent-events ring stays EMPTY and each publish
/// is cheaper than a live node's by up to `recent_events_cap` `Arc` clones. Both are floors on the
/// real cost, never overstatements.
///
/// `journal` runs the core with a journal
/// (every submit is then journaled write-ahead before it reaches the probe), `pace` sleeps between
/// two submits (an idle journal writer, the sparse production shape) and `n` is the sample count.
/// The four labels above pass `None, None, HOP_SAMPLES`, their path as before.
fn run_submit_hop(
    label: &str,
    resting: usize,
    foreign: usize,
    symbol: &str,
    journal: Option<JournalConfig>,
    pace: Option<std::time::Duration>,
    n: usize,
) {
    // The RT gap, slept BEFORE any setup so spawn -> first submit below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    #[expect(
        non_snake_case,
        reason = "the rt_gap source gate finds a sample loop by its bound `N`"
    )]
    let N: usize = n;
    let base = Instant::now();
    let hops = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N + resting)));
    let submitted = Arc::new(AtomicU64::new(0));

    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        SubmitProbe { base, hops: Arc::clone(&hops), submitted: Arc::clone(&submitted) },
        "sim",
        "BTCUSDT",
    );
    admit_foreign_symbols(&mut engine, foreign);
    // ⚠ An order on an ADMITTED symbol needs a price-board mark or the gate vetoes it
    // (`no-reference-price`, decision 0173: its `on_demand` grid row never takes the order's own
    // price as a mark), and a vetoed order never reaches the probe, so the liveness guard trips. A
    // production admission carries one (`SymbolAdmission::initial_mark`, the node's observed
    // price), so the traded foreign symbol is marked exactly as that seeds it. ONLY the traded
    // symbol: `admit_foreign_symbols` leaves the other admitted positions unpriced on the board,
    // and the `submit-hop-16sym` publish work stays what earlier runs measured.
    if foreign > 0 && symbol != "BTCUSDT" {
        engine.price_board.set_mark("sim", symbol, 101.0, 0);
    }
    // No `on_dequeued` hook: the probe is the observer here, so the fold carries no harness
    // callback at all — one fewer difference from a production core than the other harnesses have.
    let handle = spawn_core(engine, CoreConfig { journal, ..CoreConfig::default() });
    let cell = handle.snapshot_cell();

    // ONE template, cloned per message. `venue` is the engine's own and `symbol` is the mounted one
    // or an admitted one (`foreign`), so every submit routes to the primary engine and never
    // touches the order client id→venue map; `RiskLimits::new()` arms
    // no notional, exposure, margin or rate lane, so the gate admits it (the same construction
    // `crates/vike-core/tests/runtime_smoke/headless_path.rs`'s `fifo_order_preserved_across_idle_transitions`
    // proves reaches the registry). `LIVE_ORDER_CLIENT_ID` is reused so this harness's per-message allocation
    // matches the `order-client-id-live` variant's realistic 13-byte wire form.
    let template = OrderRequest {
        order_client_id: LIVE_ORDER_CLIENT_ID.to_string(),
        venue: "sim".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..OrderRequest::default()
    };

    // The RESTING book, pre-seeded outside the measurement: `resting` DISTINCT order client ids, each of which
    // stays in the registry for the rest of the run and so is walked by every later publish.
    for i in 0..resting {
        let mut req = template.clone();
        req.order_client_id = format!("resting{i}");
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
        if let Some(p) = pace {
            std::thread::sleep(p);
        }
        req.timestamp = base.elapsed().as_nanos() as i64;
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
        spin_until(&submitted, (resting + i + 1) as u64, "the measured submit");
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hops).unwrap().into_inner().unwrap(), window);
    // `publishes=` is the core's publish count, `CoreSnapshot::seq`: a publish runs only when the
    // core goes idle with the next message not yet queued (or on the 16 ms cadence), so a submit
    // tail longer than the sender's round trip SKIPS it, and a variant whose `publishes` is far
    // below `n` is not pricing a per-message publish at all. Read it beside every p50 here.
    let published = cell.load_full();
    stats.report(
        label,
        &format!(
            " resting={resting} registry={} foreign={foreign} publishes={}",
            resting + 1,
            published.sequence
        ),
    );
    // The core published the admitted symbols and their positions, so the many-symbol labels price
    // what they name (an admission that never took would read as a fast hop).
    assert_eq!(
        published.portfolio.venues[0].extra_symbols.len(),
        foreign,
        "[{label}] the published block must carry every admitted symbol"
    );
    assert_eq!(published.positions.len(), foreign, "[{label}] one position per foreign symbol");
    // Structural only — this harness prints, it does not gate on timing (see the test doc). A short
    // count means orders were denied rather than submitted, which the deadline guard would usually
    // have caught first.
    assert_eq!(stats.n, N, "[{label}] every measured submit must have reached the venue client");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the order-submit lane, with and without a maker-sized
/// resting book behind it.
///
/// Read [`run_submit_hop`]'s doc for what is measured, where "the wire" stops, and why the
/// order-client-id is fixed. Deliberately asserts NO timing ceiling, exactly like
/// [`book_lane_hop_baseline`] and for the reason the module doc gives: no calibrated budget exists
/// for this path, and an uncalibrated one is a flake rather than a gate.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture submit_lane_hop
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn submit_lane_hop_baseline() {
    run_submit_hop("submit-hop-0resting", 0, 0, "BTCUSDT", None, None, HOP_SAMPLES);
    run_submit_hop("submit-hop-32resting", 32, 0, "BTCUSDT", None, None, HOP_SAMPLES);
}

/// A measurement, not a gate: THE ORDER PATH with the production
/// journal on (`JournalConfig::at` under the temp dir, so `TMPDIR` picks the filesystem). Every
/// submit is journaled write-ahead, then reaches the probe: the hop is submit -> handed to the venue
/// client. `submit-hop-journal` sends back to back (the journal's writer never idles);
/// `submit-hop-journal-sparse` sleeps 2 ms before each of 2 000 submits (an idle writer, which is the
/// shape of a live node). Read each against `submit-hop-0resting` of the same session. No ceiling.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn submit_lane_hop_journal() {
    for (label, pace, n) in [
        ("submit-hop-journal", None, HOP_SAMPLES),
        ("submit-hop-journal-sparse", Some(std::time::Duration::from_millis(2)), 2_000),
    ] {
        let dir = std::env::temp_dir().join(format!("vjl-latency-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        run_submit_hop(label, 0, 0, "BTCUSDT", Some(JournalConfig::at(dir.clone())), pace, n);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A measurement, not a gate (decision 0279): the gate's `journal-snap` shape and the order path with the asynchronous journal and its durable
/// copy, each followed by a `FENCE` line: how many sends waited on the write thread, the
/// distribution of those waits (log2 buckets, so the quoted p50/p99 are bucket upper bounds) and the
/// write thread's longest hand-off — the evidence that no order waits behind a sync. No ceiling.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn journal_fence_stats() {
    use vike_journal::measure;
    let shapes: [(&str, Option<std::time::Duration>, usize, bool); 3] = [
        ("journal-snap-fenced", None, HOP_SAMPLES, false),
        ("submit-hop-journal-fenced", None, HOP_SAMPLES, true),
        (
            "submit-hop-journal-sparse-fenced",
            Some(std::time::Duration::from_millis(2)),
            2_000,
            true,
        ),
    ];
    for (label, pace, n, submit) in shapes {
        let dir = std::env::temp_dir().join(format!("vjl-latency-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        measure::reset();
        if submit {
            run_submit_hop(label, 0, 0, "BTCUSDT", Some(JournalConfig::at(dir.clone())), pace, n);
        } else {
            run_core_hop(label, Some(JournalConfig::at(dir.clone())));
        }
        let _ = std::fs::remove_dir_all(&dir);
        print_fence_stats(label, &measure::stats());
    }
}

/// The journal cadence of [`snapshot_step_cost`]: often enough to time dozens of `Snapshot`s in one short window.
const SNAPSHOT_STEP_EVERY: u64 = 64;

/// PERF-PROGRAM MEASUREMENT (not a gate): what a cadence `Snapshot` costs the CORE thread, the owner's A/B of
/// 2026-11-19 for the research note's 1b (its encode moved to the journal's write thread, decision 0279 amended that
/// day). The engine rests `orders` orders, so its `Snapshot` is the build runner (~40 KB at 100) and does not grow (the
/// fills are symbol-filtered out); the journal takes one every [`SNAPSHOT_STEP_EVERY`] fills, which come back to back
/// (the record after a `Snapshot` arrives at once: the worst case for one still being encoded) or `pace` apart (a
/// live maker's journaled records are milliseconds apart). The `LATENCY-GATE` line is the fill hops; the `SNAPSTEP`
/// line is `vike_journal::measure`'s: the core's step (`write_snap`), the write thread's encode, and the core's
/// settle (the copy into its stage, and how many waited for a running encode). Read a label on the build before the
/// step against the same label after it. No ceiling.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --test-threads=1 --nocapture --exact snapshot_step_cost
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn snapshot_step_cost() {
    for (label, orders, pace_us) in [
        ("snapshot-step-100ord", 100, 0),
        ("snapshot-step-100ord-paced", 100, 300),
        ("snapshot-step-400ord", 400, 0),
    ] {
        run_snapshot_step(label, orders, std::time::Duration::from_micros(pace_us));
    }
}

/// One shape of [`snapshot_step_cost`].
fn run_snapshot_step(label: &str, orders: usize, pace: std::time::Duration) {
    // A short window (under a second paced), so an RT run never meets the kernel's throttle.
    const N: usize = 3_000;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let dir = std::env::temp_dir().join(format!("vjl-latency-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal = JournalConfig {
        dir: dir.clone(),
        file: vike_journal::JournalFileConfig {
            segment_bytes: 256 * 1024 * 1024,
            flush_every: 256,
        },
        snapshot_every: SNAPSHOT_STEP_EVERY,
    };
    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    for i in 0..orders {
        let order_client_id = format!("o{i}");
        let req = OrderRequest {
            order_client_id: order_client_id.clone(),
            venue: "sim".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            ..OrderRequest::default()
        };
        engine.registry.insert(order_client_id, vike_model::OrderRecord::new(req));
    }
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Event(Event::Fill(f)) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(f.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        journal: Some(journal),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    // After the core's opening `Snapshot`; the exit one at the stop is counted (one more step than cadences).
    vike_journal::measure::reset();
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill_with_timestamp(ts))).unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
        let next = Instant::now() + pace;
        while Instant::now() < next {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();
    let s = vike_journal::measure::stats();
    let _ = std::fs::remove_dir_all(&dir);
    let hops = Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap();
    print_timeline(label, &hops, Some(SNAPSHOT_STEP_EVERY));
    let stats = HopStats::from_hops(label, hops, window);
    stats.report(
        label,
        &format!(" orders={orders} snap_every={SNAPSHOT_STEP_EVERY} pace_us={}", pace.as_micros()),
    );
    let mean = |sum: u64, n: u64| sum.checked_div(n).unwrap_or(0);
    let _ = writeln!(
        std::io::stderr(),
        "SNAPSTEP {label} steps={} step_mean_ns={} step_max_ns={} encodes={} encode_mean_ns={} \
         encode_max_ns={} settles={} settle_waited={} settle_mean_ns={} settle_max_ns={}",
        s.snapshot_steps,
        mean(s.snapshot_step_ns, s.snapshot_steps),
        s.snapshot_step_max_ns,
        s.snapshot_encodes,
        mean(s.snapshot_encode_ns, s.snapshot_encodes),
        s.snapshot_encode_max_ns,
        s.snapshot_settles,
        s.snapshot_settle_waited,
        mean(s.snapshot_settle_ns, s.snapshot_settles),
        s.snapshot_settle_max_ns,
    );
}

/// One `FENCE` line on stderr for [`journal_fence_stats`].
fn print_fence_stats(label: &str, s: &vike_journal::measure::Stats) {
    let at = |q: f64| {
        let want = (s.fence_waited as f64 * q).ceil() as u64;
        let mut seen = 0;
        for &(upper, n) in &s.fence_buckets {
            seen += n;
            if seen >= want.max(1) {
                return upper;
            }
        }
        0
    };
    let _ = writeln!(
        std::io::stderr(),
        "FENCE {label} waited={} wait_p50_le_ns={} wait_p99_le_ns={} wait_max_ns={} \
         write_max_ns={} buckets={:?}",
        s.fence_waited,
        at(0.50),
        at(0.99),
        s.fence_max_ns,
        s.write_max_ns,
        s.fence_buckets
    );
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the order-submit lane on an engine with [`FOREIGN_SYMBOLS`]
/// symbols admitted (decision 0173's cap), each holding an open position. Two labels, one engine
/// shape: `submit-hop-16sym` submits on the MOUNTED symbol (what the admitted symbols cost a submit
/// that never touches them: the per-publish `extra_symbols` clone and per-position work), and
/// `submit-hop-16sym-foreign` submits on the first admitted symbol (the same plus the per-symbol
/// grid row, `accepts_symbol` scan and foreign-symbol gate path). Both read against
/// `submit-hop-0resting`, whose registry is the same size (one order client id, reused). See
/// [`admit_foreign_symbols`] for how the engine is prepared and why no ring note rides along.
/// No timing ceiling, for the reason the module doc gives.
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --test-threads=1 --nocapture --exact submit_lane_hop_many_symbols
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn submit_lane_hop_many_symbols() {
    run_submit_hop("submit-hop-16sym", 0, FOREIGN_SYMBOLS, "BTCUSDT", None, None, HOP_SAMPLES);
    run_submit_hop(
        "submit-hop-16sym-foreign",
        0,
        FOREIGN_SYMBOLS,
        &foreign_symbol(0),
        None,
        None,
        HOP_SAMPLES,
    );
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
/// the send-time ns stamp rides `book.last_sequence` — the lanes `BookUpdate` deliberately carries
/// no ts ("the dispatch clock stamps `now`"), and `last_sequence` is a `u64` this harness owns
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
    fn on_order_book(&mut self, _b: &mut vike_core::LiveBroker, _book: &vike_marketdata::L2Book) {}
}

/// One [`BookProbe`] mount on `(sim, <symbol>, 1m)`. `controller_id` must be distinct per mount
/// whenever two share a triple (`crates/vike-core/src/runtime/assemble.rs`'s `load_mount_ids`
/// panics on a duplicate mount id); `None` keeps the legacy `{venue}__{symbol}__{interval}` id the
/// single-mount labels have always used.
fn probe_mount(
    symbol: &str,
    controller_id: Option<String>,
    symbols: Vec<vike_core::MountLeg>,
) -> vike_core::Mount {
    vike_core::Mount {
        order_client_id_code: None,
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
/// added to a per-message path. (The bar table joined them when `OrderBroker::bars` stopped discarding
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

    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let mut mounts = mounts.into_iter();
    let config = CoreConfig {
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
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // ONE standing book with `levels_per_side` levels per side, built before the window and never
    // mutated: every message hands the core the same `Arc`.
    let book = {
        let mut b = vike_marketdata::L2Book::new(0.01);
        let bids: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        let asks: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

    let spinning = gap.start_window();
    for round in 0..N as u64 {
        let t0 = Instant::now();
        for _ in 0..BURST {
            ticks
                .book(vike_execution::BookUpdate {
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

/// QUOTE-lane BURST (a measurement, NOT a gate): the CORE time per quote when every message writes
/// an EXISTING price-board cell, over `cells` accepted symbols (the engine's own plus `cells - 1`
/// extras) taken round-robin. Nothing is mounted and no position is held, so what a message costs
/// is the lane itself: the route lookup, the board write and the idle publish.
///
/// Same accounting as [`run_book_burst`] (one sample is a round of `BURST` messages divided by
/// `BURST`, never below the producer's send cost). The producer builds each [`vike_execution::QuoteUpdate`]
/// itself, as a live pump does. The first round writes each cell for the first time; the other
/// `N - 1` rounds of the run are the existing-cell path, which is the one the sample reads.
fn run_quote_burst(label: &str, cells: usize) {
    const BURST: u64 = 64;
    // Recognized as a sample loop by name (see `run_book_burst`).
    const N: usize = 2_000;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let processed = Arc::new(AtomicU64::new(0));
    let mut samples = Vec::<u64>::with_capacity(N);

    let mut engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    // `extra_symbols` is what `accepts_symbol` reads; set directly, no admission command, no ring
    // note, no position (see `Engine::published_extra_symbols` for why it is public).
    engine.extra_symbols = (1..cells).map(foreign_symbol).collect();
    let names: Vec<String> =
        std::iter::once("BTCUSDT".to_string()).chain((1..cells).map(foreign_symbol)).collect();
    let processed_hook = Arc::clone(&processed);
    let config = CoreConfig {
        // Counts quote messages only: no per-message stamp, so the hook adds one atomic add.
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Quote(_) = msg {
                processed_hook.fetch_add(1, Ordering::Release);
            }
        })),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    let spinning = gap.start_window();
    for round in 0..N as u64 {
        let t0 = Instant::now();
        for k in 0..BURST as usize {
            ticks
                .quote(vike_execution::QuoteUpdate {
                    venue: "sim".into(),
                    symbol: names[k % names.len()].clone(),
                    quote: vike_marketdata::TickQuote {
                        timestamp: round as i64,
                        local_timestamp: 0,
                        bid: 100.0,
                        ask: 100.01,
                        bid_size: 1.0,
                        ask_size: 1.0,
                        symbol: Default::default(),
                    },
                })
                .unwrap();
        }
        spin_until(&processed, (round + 1) * BURST, "the burst's quote messages");
        samples.push(t0.elapsed().as_nanos() as u64 / BURST);
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, samples, window);
    stats.report(label, &format!(" cells={cells} burst={BURST}"));
    // structural only: this harness prints, it does not gate on timing (see `quote_lane_burst`)
    assert_eq!(stats.n, N, "[{label}] every round must record one sample");
}

fn run_book_hop_inner(label: &str, levels_per_side: usize, mounts: Vec<vike_core::Mount>) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup so spawn -> first hop below is unchanged (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let mut mounts = mounts.into_iter();
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(b) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_sequence));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        // The first mount is `strategy`, the rest `extra_mounts`. Every mount on the SAME
        // `(venue, symbol)` the producer below publishes drives the strategy lane on every message.
        strategy: mounts.next(),
        extra_mounts: mounts.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // The ONE standing book, exactly as a live pump holds it: `levels_per_side` populated levels
    // per side (a venue's full standing depth), folded in place and handed out by `Arc`.
    let mut book = {
        let mut b = vike_marketdata::L2Book::new(0.01);
        let bids: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        let asks: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
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
        Arc::make_mut(&mut book).last_sequence = t_ns;
        ticks
            .book(vike_execution::BookUpdate {
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

/// A MAKER-shaped probe: on its first book it places a tagged bid and ask on its own symbol through
/// the order seam (`OrderBroker::place`), and on every later book it amends both by tag
/// (`OrderBroker::amend`), one tick out and back so every amend moves the price, as the spread
/// maker re-prices its two quotes. What [`BookProbe`] cannot see: the drain's tagged lane per
/// message (the tag lookup, the venue amend, and on a place the busy check, decision 0277's P1).
#[derive(Default)]
struct QuoteProbe {
    placed: bool,
    out: bool,
}

impl vike_model::Strategy<vike_core::LiveBroker> for QuoteProbe {
    fn on_order_book(&mut self, b: &mut vike_core::LiveBroker, book: &vike_marketdata::L2Book) {
        use vike_model::{OrderBroker, OrderPlacement};
        let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else { return };
        self.out = !self.out;
        let step = if self.out { 0.01 } else { 0.0 };
        let (bid, ask) = (bid.price - step, ask.price + step);
        if self.placed {
            OrderBroker::amend(b, "bid", None, Some(bid), None);
            OrderBroker::amend(b, "ask", None, Some(ask), None);
        } else {
            OrderBroker::place(b, "", "bid", 1, 1.0, OrderPlacement::Limit { price: bid }, false);
            OrderBroker::place(b, "", "ask", -1, 1.0, OrderPlacement::Limit { price: ask }, false);
            self.placed = true;
        }
    }
}

/// The venue client of [`run_book_hop_quoting`]: it keeps the ids of the orders it was sent (the
/// probe's two, before the window) and COUNTS the amends, retaining nothing per message
/// ([`SubmitProbe`]'s reason). It amends in place, as bybit does, so the engine sends every amend.
struct QuoteClient {
    order_client_ids: Arc<std::sync::Mutex<Vec<String>>>,
    submits: Arc<AtomicU64>,
    amends: Arc<AtomicU64>,
}

impl ExecutionClient for QuoteClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.order_client_ids.lock().unwrap().push(request.order_client_id.clone());
        self.submits.fetch_add(1, Ordering::Release);
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn modify(
        &mut self,
        _order: &OrderRequest,
        _new_quantity: Option<f64>,
        _new_price: Option<f64>,
    ) {
        self.amends.fetch_add(1, Ordering::Release);
    }
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        Some(vike_venues::AmendSemantics::InPlaceTotal)
    }
}

/// The book-lane hop with a QUOTING mount on the book's own series ([`QuoteProbe`]): what a maker
/// that re-prices two tagged quotes per book costs the lane, read against `book-hop-10lvl-mounted`
/// (a mount that sends nothing) of the same session. The pacing and the stamp are
/// [`run_book_hop_inner`]'s. Before the window, one book places the two quotes, the venue's
/// `Submitted` and `Accepted` are folded for both, and a second book proves the amends reach the
/// client; every measured book then amends both. The registry stays at two orders.
fn run_book_hop_quoting(label: &str, levels_per_side: usize) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup, as every harness here (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let books = Arc::new(AtomicU64::new(0));
    let events = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N + 2)));
    let order_client_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (submits, amends) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));

    let client = QuoteClient {
        order_client_ids: Arc::clone(&order_client_ids),
        submits: Arc::clone(&submits),
        amends: Arc::clone(&amends),
    };
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    );
    let (books_hook, events_hook, hop_hook) =
        (Arc::clone(&books), Arc::clone(&events), Arc::clone(&hop_ns));
    let mut mount = probe_mount("BTCUSDT", None, Vec::new());
    mount.strategy = Box::new(QuoteProbe::default());
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| match msg {
            Ingest::Book(b) => {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_sequence));
                books_hook.fetch_add(1, Ordering::Release);
            }
            Ingest::Event(_) => {
                events_hook.fetch_add(1, Ordering::Release);
            }
            _ => {}
        })),
        strategy: Some(mount),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    let mut book = standing_book(levels_per_side);
    let send = |book: &mut Arc<vike_marketdata::L2Book>| {
        // Stamped BEFORE the `make_mut`, as `run_book_hop_inner` stamps.
        let t_ns = base.elapsed().as_nanos() as u64;
        Arc::make_mut(book).last_sequence = t_ns;
        ticks
            .book(vike_execution::BookUpdate {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::clone(book),
            })
            .unwrap();
    };

    // The quotes go on, and the venue answers both, outside the window.
    send(&mut book);
    spin_until(&submits, 2, "the probe's two quotes");
    let sender = handle.event_sender();
    let placed = order_client_ids.lock().unwrap().clone();
    for order_client_id in &placed {
        let id = || order_client_id.clone();
        sender
            .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                order_client_id: id(),
                timestamp: 0,
            }))
            .unwrap();
        sender
            .blocking_send(Event::OrderAccepted(OrderAccepted {
                order_client_id: id(),
                venue_order_id: None,
                timestamp: 0,
            }))
            .unwrap();
    }
    // The four answers are dequeued, so the book after them is dispatched against two resting
    // quotes: its amends reach the client.
    spin_until(&events, 4, "the venue's answers");
    send(&mut book);
    spin_until(&amends, 2, "the first amends");
    let warm = books.load(Ordering::Acquire);
    hop_ns.lock().unwrap().clear();

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        send(&mut book);
        spin_until(&books, warm + i + 1, "this book");
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, &format!(" levels_per_side={levels_per_side} quotes=2"));
    // structural only, like every book-hop measurement: every measured book amended both quotes
    assert_eq!(stats.n, N, "every Ingest::Book message must be observed by the hook");
    assert_eq!(amends.load(Ordering::Acquire), 2 * (N as u64 + 1), "two amends per book");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what a QUOTING mount costs the book lane over a mount
/// that sends nothing — [`run_book_hop_quoting`] against `book-hop-10lvl-mounted`, run in the same
/// session. Added with decision 0277's step 6 (the makers on the order seam, P1's busy check in the
/// drain), whose change [`BookProbe`] cannot see. Asserts NO ceiling, like the other measurements:
/// release, `--ignored`, `--nocapture`, `--exact book_lane_hop_quoting`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_quoting() {
    run_book_hop_quoting("book-hop-10lvl-mounted-quoting", 10);
}

/// How many round trips the sandboxed quoting harness times. Each is a pipe hop each way (tens of µs),
/// so a 100 000-sample loop would spin past the RT throttle's budget (`rt_gap`); 10 000 stay inside it.
const SANDBOX_SAMPLES: usize = 10_000;

/// A mount whose strategy is the strategy FILE `probe`, run in a real sandboxed child process (plan 2b
/// F5: `vike_tradehub_core::remote_strategy`), or `None` with a SKIP line when the probe was not prepared./// `crates/vike-strategy-sandbox/tests/live_child.rs -- --prepare-latency` prepares it: it builds the
/// probe files (cargo, so never under this binary's `chrt`) and leaves them, with the program that runs
/// them, in `sandbox-latency/` BESIDE the test binaries (`target/<profile>/deps/`, where this binary is
/// too), so a measured binary copied out of its lane takes that folder along. The child inherits this
/// thread's scheduling and pin: it is part of what is measured.
fn sandbox_mount(
    symbol: &str,
    probe: &str,
) -> Option<(vike_core::Mount, vike_tradehub_core::remote_strategy::RemoteHandle)> {
    use vike_tradehub_core::remote_strategy::{RemoteSpec, RemoteStrategy, SandboxCommand};
    // This binary plays the tradehub, so the boot's step 0 first: no inherited descriptor reaches the child.
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let me = std::env::current_exe().expect("this binary's path");
    let dir = me.parent().expect("this binary's directory").join("sandbox-latency");
    let artifact = dir.join(format!("{probe}.so"));
    let Some(program) =
        std::fs::read_to_string(dir.join("program")).ok().filter(|_| artifact.exists())
    else {
        println!(
            "SKIP sandbox {probe}: not prepared beside this binary (run `cargo test -p vike-strategy-sandbox --release \
             --test live_child -- --prepare-latency` first)"
        );
        return None;
    };
    let command = SandboxCommand {
        program: program.trim().into(),
        // `live <builds> <artifact>`: the probe is opened beneath its folder, no link followed.
        args: vec![
            "strategy-sandbox".into(),
            "live".into(),
            dir.into_os_string(),
            format!("{probe}.so").into(),
        ],
    };
    let spec = RemoteSpec {
        file: format!("{probe}.rs"),
        sha256: String::new(),
        params: String::new(),
        facts: vike_model::params::MountFacts {
            symbol: symbol.into(),
            venue: "sim".into(),
            interval: "1m".into(),
            ..Default::default()
        },
        controller_id: None,
        // The child is `crates/vike-strategy-sandbox/tests/live_child.rs`'s binary, which states its `BUILD`.
        build: "live_child".into(),
    };
    let (proxy, remote) =
        RemoteStrategy::open(spec, command.launcher()).expect("the sandboxed probe opens");
    let mut mount = probe_mount(symbol, None, Vec::new());
    mount.strategy = Box::new(proxy);
    Some((mount, remote))
}

/// PERF-PROGRAM MEASUREMENT (not a gate): the book-lane hop with [`BookProbe`] mounted as a SANDBOXED
/// strategy file. Read `sandbox-mounted` against `book-hop-10lvl-mounted` of the same session: the delta is
/// the proxy's fold cost (the read-side copy and the queue push; the child's work is off the fold). Asserts
/// NO ceiling; SKIPs unless the probe was prepared ([`sandbox_mount`]). `--exact book_lane_hop_sandboxed`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_sandboxed() {
    let Some((mount, _remote)) = sandbox_mount("OTHER", "book_probe") else { return };
    run_book_hop_inner("sandbox-mounted", 10, vec![mount]);
}

/// The book-lane hop with [`QuoteProbe`] mounted as a SANDBOXED strategy file, and the ROUND TRIP of each
/// book: from the send to its two amends reaching the venue client (event, pipe, child hook, pipe, the
/// core's reply door, the drain, the venue). One book in flight, as in [`run_book_hop_quoting`], whose
/// warm-up this mirrors.
fn run_book_hop_sandbox_quoting(
    label: &str,
    trip_label: &str,
    mount: vike_core::Mount,
    remote: vike_tradehub_core::remote_strategy::RemoteHandle,
) {
    const N: usize = SANDBOX_SAMPLES;
    // The RT gap, slept BEFORE any setup, as every harness here (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let events = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N + 2)));
    let order_client_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (submits, amends) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let client = QuoteClient {
        order_client_ids: Arc::clone(&order_client_ids),
        submits: Arc::clone(&submits),
        amends: Arc::clone(&amends),
    };
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    );
    let (events_hook, hop_hook) = (Arc::clone(&events), Arc::clone(&hop_ns));
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| match msg {
            Ingest::Book(b) => {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_sequence));
            }
            Ingest::Event(_) => {
                events_hook.fetch_add(1, Ordering::Release);
            }
            _ => {}
        })),
        strategy: Some(mount),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    remote.attach(handle.event_sender());
    let ticks = handle.tick_sender();
    let mut book = standing_book(10);
    let send = |book: &mut Arc<vike_marketdata::L2Book>| {
        let t_ns = base.elapsed().as_nanos() as u64;
        Arc::make_mut(book).last_sequence = t_ns;
        ticks
            .book(vike_execution::BookUpdate {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::clone(book),
            })
            .unwrap();
    };

    // The quotes go on and the venue answers both, outside the window; then one book's amends.
    send(&mut book);
    spin_until(&submits, 2, "the sandboxed probe's two quotes");
    let sender = handle.event_sender();
    for order_client_id in &order_client_ids.lock().unwrap().clone() {
        let id = || order_client_id.clone();
        sender
            .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                order_client_id: id(),
                timestamp: 0,
            }))
            .unwrap();
        sender
            .blocking_send(Event::OrderAccepted(OrderAccepted {
                order_client_id: id(),
                venue_order_id: None,
                timestamp: 0,
            }))
            .unwrap();
    }
    spin_until(&events, 4, "the venue's answers");
    send(&mut book);
    spin_until(&amends, 2, "the first amends");
    hop_ns.lock().unwrap().clear();
    let mut trip_ns = Vec::with_capacity(N);

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let sent = Instant::now();
        send(&mut book);
        spin_until(&amends, 2 * (i + 2), "this book's amends");
        trip_ns.push(sent.elapsed().as_nanos() as u64);
    }
    let window = spinning.close_window();
    handle.shutdown_and_join();

    let stats =
        HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap(), window);
    stats.report(label, " levels_per_side=10 quotes=2 sandboxed=1");
    let trip = HopStats::from_hops(trip_label, trip_ns, window);
    trip.report(trip_label, " levels_per_side=10 quotes=2 event_to_applied_intents=1");
    // structural only: every measured book was dequeued, and each one's two amends reached the venue
    assert_eq!(stats.n, N, "every Ingest::Book message must be observed by the hook");
    assert_eq!(amends.load(Ordering::Acquire), 2 * (N as u64 + 1), "two amends per book");
}

/// PERF-PROGRAM MEASUREMENT (not a gate): what a SANDBOXED quoting mount costs the book lane
/// (`sandbox-mounted-quoting`, read against `book-hop-10lvl-mounted-quoting` of the same session) and its
/// round trip, event to applied intents (`sandbox-roundtrip`: p50/p99/max of the whole hop through the
/// child, plan 2b §4's "15-40 µs p50, under 150 µs p99" estimate). Asserts NO ceiling; SKIPs unless the
/// probe was prepared ([`sandbox_mount`]). `--exact book_lane_hop_sandboxed_quoting`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_sandboxed_quoting() {
    let Some((mount, remote)) = sandbox_mount("BTCUSDT", "quote_probe") else { return };
    run_book_hop_sandbox_quoting("sandbox-mounted-quoting", "sandbox-roundtrip", mount, remote);
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

/// PERF-PROGRAM MEASUREMENT (not a gate): what the quote lane's price-board write costs per message.
/// `quote-burst-1cell` writes one cell (the board scans it), `quote-burst-16cells` rotates over 16
/// (past `LINEAR_SCAN_CELLS`, so the board hashes). Compare a head with its base on the p50 in one
/// interleaved session, under the discipline of `crates/vike-core/CLAUDE.md`'s "Measuring a hot-path
/// change honestly"; asserts NO ceiling. Run: release, `--ignored`, `--test-threads=1`,
/// `--nocapture`, `--exact quote_lane_burst`.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn quote_lane_burst() {
    run_quote_burst("quote-burst-1cell", 1);
    run_quote_burst("quote-burst-16cells", 16);
}

/// The book-lane hop on a core of `labelled` extra LABELLED engines (`sim#L<k>`), each admitted the
/// book's symbol `OTHER` before the core spawns, so every message pays the fan-out of
/// `CoreThread::admitted_fanout` to `labelled` engines (the default engine does not accept `OTHER`:
/// the routed write is a miss). `labelled == 0` is the control: no labelled account, so the arm pays
/// one `is_empty()` and nothing else.
fn run_book_hop_labelled(label: &str, levels_per_side: usize, labelled: usize) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine_of = |route_key: &str| {
        let mut e = Engine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        e.route_key = route_key.to_string();
        e
    };
    let extras: Vec<_> = (0..labelled)
        .map(|k| {
            let route_key = format!("sim#L{k}");
            let mut e = engine_of(&route_key);
            e.admit_symbol(&vike_execution::SymbolAdmission {
                route_key,
                symbol: "OTHER".into(),
                grid: vike_model::SymbolGrid {
                    tick_size: Some(0.01),
                    lot_size: Some(0.001),
                    ..vike_model::SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                initial_mark_ms: None,
                quote_currency: None,
                quote_rate: None,
                rate_as_of_ms: None,
                initial_margin_requirement: None,
                release: false,
                margin_mode: None,
            });
            (1_000.0, e)
        })
        .collect();
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(b) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_sequence));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        ..CoreConfig::default()
    };
    let handle = vike_core::spawn_core_multi(engine_of("sim"), extras, config);
    let ticks = handle.tick_sender();

    let mut book = {
        let mut b = vike_marketdata::L2Book::new(0.01);
        let bids: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        let asks: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let t_ns = base.elapsed().as_nanos() as u64;
        Arc::make_mut(&mut book).last_sequence = t_ns;
        ticks
            .book(vike_execution::BookUpdate {
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
    stats.report(label, &format!(" levels_per_side={levels_per_side} labelled={labelled}"));
    // structural only — this harness prints, it does not gate on timing (see the test doc below)
    assert_eq!(stats.n, N, "every Ingest::Book message must be observed by the hook");
}

/// MEASUREMENT (not a gate): what the labelled-account fan-out costs the book lane (decision 0174).
/// `labelled-x0` is the control (one default account, nothing admitted: the arm's single
/// `is_empty()`), `x1` and `x3` fan every message out to one and three labelled engines. Asserts NO
/// ceiling, like the other measurements here. Release, `--ignored`, `--nocapture`,
/// `--exact book_lane_hop_labelled_fanout`, on a quiet box.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_labelled_fanout() {
    run_book_hop_labelled("book-hop-10lvl-labelled-x0", 10, 0);
    run_book_hop_labelled("book-hop-10lvl-labelled-x1", 10, 1);
    run_book_hop_labelled("book-hop-10lvl-labelled-x3", 10, 3);
}

/// The book-lane hop on a POLYMARKET core (a roster venue whose capability row declares no trigger kind, so
/// the core holds the stop: deribit's row, where this harness first ran, gained a native reduce-only stop
/// and its stop rested in the registry instead, measuring no arm) that holds an operator's CORE-HELD
/// reduce-only stop on the book's own symbol when `armed`, far from the market so it never fires. The book's symbol is the engine's own, so the routed write and the tick
/// check (`CoreThread::tick_conditionals`, `fire_conditionals_at_quote`) run on every message: this is
/// the one variant that reaches the per-tick conditional check at all (every other book variant runs
/// with nothing armed). `armed == false` is the control: the same core with no stop, which pays the one
/// `tick_conditionals` bool and nothing else.
fn run_book_hop_held_stop(label: &str, armed: bool) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let commands = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine = Engine::new(
        Account::new(1.0, "polymarket", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "polymarket",
        "BTC-PERP",
    );
    let processed_hook = Arc::clone(&processed);
    let commands_hook = Arc::clone(&commands);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        // Counts BOOK messages only (the arming command must not shift the hop pacing below) and the
        // command separately: the hook fires on dequeue, and the core handles one message fully before
        // it dequeues the next, so a book sent after the command was seen is folded after the arm.
        on_dequeued: Some(Box::new(move |msg: &Ingest| match msg {
            Ingest::Book(b) => {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_sequence));
                processed_hook.fetch_add(1, Ordering::Release);
            }
            Ingest::Command(_) => {
                commands_hook.fetch_add(1, Ordering::Release);
            }
            _ => {}
        })),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    if armed {
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            venue: "polymarket".into(),
            symbol: "BTC-PERP".into(),
            side: -1,
            qty: 1.0,
            order_type: vike_model::OrderType::Stop,
            trigger_price: Some(1.0),
            reduce_only: true,
            ..Default::default()
        }))));
        spin_until(&commands, 1, "the arming command");
    }

    let mut book = {
        let mut b = vike_marketdata::L2Book::new(0.01);
        let bids: Vec<vike_marketdata::L2BookLevel> = (0..10)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        let asks: Vec<vike_marketdata::L2BookLevel> = (0..10)
            .map(|i| {
                vike_marketdata::L2BookLevel::new(100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1)
            })
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let t_ns = base.elapsed().as_nanos() as u64;
        Arc::make_mut(&mut book).last_sequence = t_ns;
        ticks
            .book(vike_execution::BookUpdate {
                venue: "polymarket".into(),
                symbol: "BTC-PERP".into(),
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
    stats.report(label, &format!(" levels_per_side=10 armed={armed}"));
    // structural only — this harness prints, it does not gate on timing (see the test doc below)
    assert_eq!(stats.n, N, "every Ingest::Book message must be observed by the hook");
}

/// MEASUREMENT (not a gate): what a RESTING core-held reduce-only stop costs the book lane. Every
/// message of the armed core runs `fire_conditionals_at_quote` (the book walk, the degenerate-bar
/// check, no allocation) where the control, `stop-none`, runs the one `tick_conditionals` bool. Read
/// `stop-held` against `stop-none` of the same session, and `stop-none` against the same label on the
/// base build for the cost of the field itself. Asserts NO ceiling, like the other measurements here.
/// Release, `--ignored`, `--nocapture`, `--exact book_lane_hop_held_stop`, on a quiet box.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_held_stop() {
    run_book_hop_held_stop("book-hop-10lvl-stop-none", false);
    run_book_hop_held_stop("book-hop-10lvl-stop-held", true);
}

/// [`run_book_hop_labelled`]'s TRADE-lane twin (`CoreThread::fan_out_trade`): a trade print on `OTHER`
/// per hop, on a core of `labelled` extra LABELLED engines each admitted `OTHER`. The default engine
/// does not accept `OTHER`, so the routed write is a miss and the print is a fanned-out write to
/// `labelled` boards. `labelled == 0` is the control: one `is_empty()` and nothing else.
///
/// `fan_out_trade` writes the boards and does NOT mark the core for the idle publish (`fan_out_quote`
/// does, since v0.1.79). That is a DECISION: marking it for engines that hold a position in the symbol
/// was built and measured with the `held` variants below (three labelled engines, a position and
/// [`HELD_ORDERS`] resting orders each), and the core-hop p99 of `trade-hop-labelled-held-x3` rose by
/// about 4.6 us (IQR 0.6 to 5.1 us) over a 972 ns base, p50 unmoved: one whole publish of three such
/// engines per print, which lands in the tail whenever the next print arrives during it. The `held`
/// variants stay as the instrument that prices that choice; against a core that marks, read them for
/// the cost, against one that does not (this tree) they show only the fan-out write.
fn run_trade_hop_labelled(label: &str, labelled: usize, held: bool) {
    const N: usize = HOP_SAMPLES;
    // The RT gap, slept BEFORE any setup (`rt_gap`).
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hop_ns = Arc::new(std::sync::Mutex::new(Vec::<u64>::with_capacity(N)));

    let engine_of = |route_key: &str| {
        let mut e = Engine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        e.route_key = route_key.to_string();
        e
    };
    let extras: Vec<_> = (0..labelled)
        .map(|k| {
            let route_key = format!("sim#L{k}");
            let mut e = engine_of(&route_key);
            e.admit_symbol(&vike_execution::SymbolAdmission {
                route_key,
                symbol: "OTHER".into(),
                grid: vike_model::SymbolGrid {
                    tick_size: Some(0.01),
                    lot_size: Some(0.001),
                    ..vike_model::SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                initial_mark_ms: None,
                quote_currency: None,
                quote_rate: None,
                rate_as_of_ms: None,
                initial_margin_requirement: None,
                release: false,
                margin_mode: None,
            });
            if held {
                e.account.positions.insert(
                    ("sim".into(), "OTHER".into(), vike_model::events::PositionSide::Long),
                    vike_execution::PositionEntry {
                        size: 1.0,
                        average_price: 100.0,
                        ..Default::default()
                    },
                );
                e.account.set_mark_from("sim", "OTHER", 100.0, MarkSource::VenueMark, 0);
                for j in 0..HELD_ORDERS {
                    let order_client_id = format!("h{j}");
                    let req = OrderRequest {
                        order_client_id: order_client_id.clone(),
                        venue: "sim".to_string(),
                        symbol: "OTHER".to_string(),
                        side: 1,
                        qty: 1.0,
                        order_type: vike_model::OrderType::Limit,
                        price: Some(99.0),
                        ..OrderRequest::default()
                    };
                    e.registry.insert(order_client_id, vike_model::OrderRecord::new(req));
                }
            }
            (1_000.0, e)
        })
        .collect();
    let processed_hook = Arc::clone(&processed);
    let hop_hook = Arc::clone(&hop_ns);
    let config = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Trade(t) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(t.trade.timestamp as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        ..CoreConfig::default()
    };
    let handle = vike_core::spawn_core_multi(engine_of("sim"), extras, config);
    let ticks = handle.tick_sender();

    let spinning = gap.start_window();
    for i in 0..N as u64 {
        let t_ns = base.elapsed().as_nanos() as i64;
        ticks
            .trade(vike_execution::TradeUpdate {
                venue: "sim".into(),
                symbol: "OTHER".into(),
                trade: vike_marketdata::TickTrade {
                    timestamp: t_ns,
                    local_timestamp: 0,
                    price: 100.0,
                    size: 1.0,
                    is_buyer_maker: false,
                    symbol: "OTHER".into(),
                },
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
    stats.report(label, &format!(" labelled={labelled} held={held}"));
    // structural only — this harness prints, it does not gate on timing
    assert_eq!(stats.n, N, "every Ingest::Trade message must be observed by the hook");
}

/// Resting orders each labelled engine of the `held` trade variants carries beside its position.
const HELD_ORDERS: usize = 8;

/// MEASUREMENT (not a gate): what the labelled-account fan-out costs the TRADE lane. `labelled-x0` is
/// the control, `x1` and `x3` fan every print out to one and three labelled engines that were only
/// admitted the symbol; `held-x1` and `held-x3` do the same to engines that hold a position and
/// [`HELD_ORDERS`] resting orders in it (the shape whose publish cost decided that a trade print does not
/// mark the core: see [`run_trade_hop_labelled`]). Release, `--ignored`, `--nocapture`,
/// `--exact trade_lane_hop_labelled_fanout`, on a quiet box.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn trade_lane_hop_labelled_fanout() {
    run_trade_hop_labelled("trade-hop-labelled-x0", 0, false);
    run_trade_hop_labelled("trade-hop-labelled-x1", 1, false);
    run_trade_hop_labelled("trade-hop-labelled-x3", 3, false);
    run_trade_hop_labelled("trade-hop-labelled-held-x1", 1, true);
    run_trade_hop_labelled("trade-hop-labelled-held-x3", 3, true);
}

/// `mounts` [`BookProbe`] mounts for the publish harnesses, each on its OWN symbol (`PUB<k>`) that no
/// message in those harnesses names: no tick lane ever steps one, so the only work they add per
/// message is the publish side (`crates/vike-core/src/runtime/timers.rs`'s `mount_views`, which builds
/// one row per mount, and `crates/vike-core/src/snapshot/build.rs`'s `build`, which carries them).
fn publish_mounts(mounts: usize) -> Vec<vike_core::Mount> {
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
    fn on_order_book(
        &mut self,
        broker: &mut vike_core::LiveBroker,
        _book: &vike_marketdata::L2Book,
    ) {
        if !self.done {
            self.done = true;
            let market = vike_model::OrderPlacement::Market;
            vike_model::OrderBroker::place(broker, "BTCUSDT", "", 1, 1.0, market, false);
        }
    }
}

/// A standing `levels_per_side` book handed out by `Arc`, never mutated (the burst harness's own).
fn standing_book(levels_per_side: usize) -> Arc<vike_marketdata::L2Book> {
    standing_book_at(levels_per_side, 0.0)
}

/// [`standing_book`] with every price moved up by `shift`: a second book whose mid differs, so a
/// harness that alternates the two moves the engine's quote (and so a mounted position's mark) on
/// every message.
fn standing_book_at(levels_per_side: usize, shift: f64) -> Arc<vike_marketdata::L2Book> {
    let mut b = vike_marketdata::L2Book::new(0.01);
    let bids: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
        .map(|i| {
            vike_marketdata::L2BookLevel::new(shift + 100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)
        })
        .collect();
    let asks: Vec<vike_marketdata::L2BookLevel> = (0..levels_per_side)
        .map(|i| {
            vike_marketdata::L2BookLevel::new(
                shift + 100.01 + i as f64 * 0.01,
                1.0 + i as f64 * 0.1,
            )
        })
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

    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let processed_hook = Arc::clone(&processed);
    let published_hook = Arc::clone(&published);
    let mut mount_list = publish_mounts(mounts).into_iter();
    let config = CoreConfig {
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
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    let book = standing_book(10);
    let symbol = if publishes { "BTCUSDT" } else { "OTHER" };
    let published_before = published.load(Ordering::Acquire);

    let spinning = gap.start_window();
    for round in 0..N as u64 {
        let t0 = Instant::now();
        for _ in 0..BURST {
            ticks
                .book(vike_execution::BookUpdate {
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
            std::mem::size_of::<vike_execution::MountView>()
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

    let engine = Engine::new(
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
        list.push(vike_core::Mount {
            strategy: Box::new(OpenOnce { done: false }),
            ..probe_mount("BTCUSDT", Some("pub-open".to_string()), Vec::new())
        });
        list.into_iter()
    } else {
        publish_mounts(mounts).into_iter()
    };
    let config = CoreConfig {
        repaint: Some(Box::new(move || {
            published_hook.fetch_add(1, Ordering::Release);
        })),
        strategy: mount_list.next(),
        extra_mounts: mount_list.collect(),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    // Two books: the same one twice for the plain labels (nothing moves), a second book one price
    // unit higher for `-moving`, so alternating them moves the quote on every message.
    let books =
        [standing_book(10), if moving { standing_book_at(10, 1.0) } else { standing_book(10) }];
    let send = |i: u64| {
        ticks
            .book(vike_execution::BookUpdate {
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
        let order_client_id = handle
            .snapshot()
            .orders
            .first()
            .expect("the position holder's market order is registered after the warm-up book")
            .order_client_id
            .clone();
        handle
            .event_sender()
            .blocking_send(Event::Fill(FillEvent {
                trade_id: "open-fill".into(),
                order_client_id,
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                last_qty: 1.0,
                last_price: 100.0,
                commission: 0.0,
                commission_asset: String::new().into(),
                liquidity_side: "taker".to_string().into(),
                timestamp: 0,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
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
            std::mem::size_of::<vike_execution::MountView>()
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

/// PERF-PROGRAM MEASUREMENT (not a gate): what does a REAL `order_client_id` cost the core hop?
///
/// The two gate tests above send `order_client_id: String::new()`, and an empty `String` never
/// allocates — so the gate is STRUCTURALLY BLIND to the cost of the order client id, and cannot be used to
/// justify or refute the `String -> CompactString` change (perf audit 2026-07-28, finding #4,
/// deliberately NOT taken in this PR — see the PR body). This pair makes that cost visible.
///
/// What each run measures, given the request/response pacing (one message in flight, so message
/// `i`'s DISPATCH and DROP land inside message `i+1`'s hop — the same accounting
/// [`run_core_hop`] documents):
///
/// - `order-client-id-empty` — today's gate path. No order client id allocation anywhere.
/// - `order-client-id-live` — a 13-byte order client id. As a `String` that is one `malloc` on the SENDER side per
///   message plus one `free` on the FOLD thread when `drain_delivered`'s `Vec<Event>` drops.
/// - `order-client-id-live-v2` — the same hop under a 24-byte v2 leg id ([`LIVE_ORDER_CLIENT_ID_V2`], decision 0270's D8):
///   its delta to `order-client-id-live` is what the longer id costs; D8's fallback is a 10-character `U`.
///
/// READING THE RESULT: the p50 DELTA between the two labels is the whole prize `CompactString`
/// could win (13 bytes is inside its 24-byte inline budget, so the change would take that delta to
/// ~0). A delta in the noise REFUTES the change — it would be several hundred mechanical edit sites
/// across 22 crates for nothing. Run on a QUIET box and read p50 before p99 (a loaded box gives an
/// 11-13 µs p99 with a healthy p50 — that is contention, not signal).
///
///     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture order_client_id_alloc
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn order_client_id_alloc_cost_baseline() {
    run_core_hop_with_order_client_id("order-client-id-empty", None, NO_ORDER_CLIENT_ID);
    run_core_hop_with_order_client_id("order-client-id-live", None, LIVE_ORDER_CLIENT_ID);
    run_core_hop_with_order_client_id("order-client-id-live-v2", None, LIVE_ORDER_CLIENT_ID_V2);
}
