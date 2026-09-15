//! R5 latency harness — the plan's "p99 core-hop < 10 µs" gate, plus the snapshot-build
//! cost measured separately (provably off the per-event path).
//!
//! The three gate tests — `baseline`, `journal` (append cost isolated, no cadence snapshot) and
//! `journal-snap` (the production journal config, snapshots included) — assert the whole TAIL, not
//! just p99. See [`P99_BUDGET_NS`] and the block of budgets around it for why, and for the
//! reference measurements every ceiling is derived from.
//!
//! ⚠ The two journal variants no longer share the baseline's 10 µs p99 bar (their test NAMES still
//! say `under_10us`, and are kept only so a CI log greps the same across the change): that literal
//! was never derived for them and `main` itself measured 9 809 ns against it. They gate on
//! [`JOURNAL_P99_NS`] — read THE 2026-08-05 CALIBRATION block below before touching either.
//!
//! Run manually in release (timing-sensitive, skipped in CI's debug run):
//!     cargo test -p vike-core --release --test runtime_latency -- --ignored --nocapture
//!
//! # GATES vs MEASUREMENTS — this file holds both, and only three of them are gates
//!
//! The three tests named above ASSERT ceilings. Everything else here MEASURES and asserts nothing
//! about timing, and that split is deliberate rather than unfinished work. Calibrating a ceiling
//! honestly on this box is expensive — [`JOURNAL_P99_NS`] took 98 CI attempts to derive, and the
//! block below is what that cost bought — while an UNCALIBRATED ceiling is a flaky gate, which is
//! worse than no gate at all because people learn to re-run it rather than read it. A measured,
//! persisted, trendable series catches drift for an afternoon's work and leaves the ceiling as a
//! later decision, taken from a baseline instead of from a guess. **Adding a measurement variant
//! here does not mean a budget is coming; adding a ceiling means somebody derived one.**
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
//!     shows up as a loss COUNTER — `crates/vike-data/src/live_rec.rs`'s `RecorderHandle`, whose
//!     `dropped` (rows refused at a full channel) and disjoint `discarded` (rows lost writing) are
//!     the two numbers that move — not as a slow flush. A latency series over it would read like
//!     coverage of a hazard it structurally cannot see. If it is ever measured, it belongs next to
//!     the store in `vike-data`, not in a binary that runs on the reserved cores.
//!
//! ## The persistence contract, and why a new variant needs no workflow change
//!
//! [`HopStats::report`] writes ONE `LATENCY-GATE` line per variant to the stderr HANDLE.
//! `.github/workflows/ci.yml`'s latency step parses every such line out of each attempt's log and
//! appends one JSON row per (run, attempt, variant) to `/home/the CI user/.vike-ci-metrics/latency-series.jsonl` on
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
//! about to go idle (`crates/vike-core/src/runtime/mod.rs`'s `publish_guarded`, reached under
//! `if self.dirty` immediately before `blocking_recv`), and this file's request/response pacing
//! makes the core idle between every single message — so `CoreSnapshot::build` runs once per
//! message, and it walks the whole order registry. A harness that submits N orders under N distinct
//! client-order-ids therefore costs O(N²), and its tail measures `IndexMap` growth rather than the
//! lane it claims to. [`run_submit_hop`] pins the registry by REUSING one coid for exactly that
//! reason.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use vike_core::{CoreConfig, CoreSnapshot, JournalConfig, ReconBlock, spawn_core};
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, Ingest, OrderIntent, PriceCfg,
    RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

/// The `LATENCY-GATE` line's own format gate — see that module's doc for what silently breaks
/// without it. `#[path]`, because cargo auto-discovers test binaries from `tests/*.rs` and
/// `tests/*/main.rs` only, so a file one directory down is a plain module rather than a second
/// binary (`crates/bridges/ctrader/tests/` is the in-tree precedent for the layout).
#[path = "common/latency_line.rs"]
mod latency_line;

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
// TAIL BUDGETS
//
// Until this file changed, both gates asserted `p99 < 10_000` and NOTHING else — while already
// computing p50 / p99.9 / max / `hops>100µs` purely in order to PRINT them. That made the gate
// blind to its own tail in the most literal way: the reference run below went green with a p99.9
// of 37 741 ns (3.8x over the budget the gate's own name states) and one 141.86 ms core hop.
// For a market maker the tail is the number that costs money; p99 is the number that looks good.
//
// Both blind spots became VISIBLE only once [`HopStats::report`] started writing its summary
// straight to the stderr HANDLE: libtest captures test output on a PASS, so until then every figure
// here existed exclusively inside an assertion's panic message — i.e. exclusively when the gate was
// already red.
//
// ⚠ This block used to say "once #915 added `--nocapture` to the CI step", and that is FALSE in
// both halves: no workflow in this repo has ever passed `--nocapture` (`ci.yml`'s latency step is
// `$RUN "$BIN" --ignored --test-threads=1`, and `grep -rn nocapture .github/` finds only prose),
// and the fix was never a runner flag. ci.yml's own series block names that same false premise as
// one it expects people to keep re-deriving; the stderr-handle write in [`HopStats::report`] is
// what made the numbers legible on a PASS, deliberately, so that they do not depend on how the
// step is invoked.
//
// THE REFERENCE MEASUREMENT that every constant below is derived from — GitHub Actions run
// 30689963620, the latency box's `the latency runner` runner, `SCHED_FIFO 50` + `taskset -c 28-31`, quiesced
// (`box quiesced after 35s (load1=12.92)`), passed on attempt 1, 100 000 hops per variant:
//
//     variant    p50      p99        p99.9       max              hops>100µs
//     baseline   271 ns   551 ns     2 104 ns    25 709 ns        0
//     journal    932 ns   4 689 ns   37 741 ns   141 863 526 ns   4
//
// FOUR MORE POINTS from this file's own green runs, deliberately spanning a 5x load range so the
// ceilings are sized on a SPREAD rather than on any single quiet row. All four passed on attempt
// 1, so none of these is a retry survivor:
//
//     run          load1   variant    p50      p99        p99.9       max             hops>100µs
//     30694578143   5.25   baseline   290 ns   541 ns      2 645 ns   21 240 ns        0
//     30694578143   5.25   journal    932 ns  3 637 ns     36 929 ns  66 336 719 ns    1
//     30694882148  11.87   baseline   291 ns   531 ns      5 941 ns   26 079 ns        0
//     30694882148  11.87   journal    962 ns  6 291 ns     42 580 ns  49 560 812 ns   17
//     30695229811  27.71   baseline   281 ns   581 ns      7 764 ns   28 253 ns        0
//     30695229811  27.71   journal    962 ns  4 088 ns     41 798 ns  53 584 398 ns    2
//     30696024454  23.40   baseline   451 ns   732 ns      4 087 ns   37 150 ns        0
//     30696024454  23.40   journal    982 ns  5 650 ns     52 619 ns  62 109 191 ns   11
//
// WHAT VARIES AND WHAT DOES NOT, across all five:
//   * p50 and p99 are rock solid (271–451 / 531–732 baseline; 932–982 / 3 637–6 291 journal).
//   * journal p99.9 is the most reproducible signal of the journaling debt — but "reproducible"
//     has loosened as observations accumulated, and it is now the number with the LEAST margin.
//     See the WORST-GREEN DRIFT note below and `JOURNAL_P999_NS`.
//   * BASELINE p99.9 is the one that tracks load — but NOT monotonically, and an earlier revision
//     of this comment claimed it did on the strength of three points that happened to be ordered
//     (2 645 -> 5 941 -> 7 764 ns at load1 5.25 -> 11.87 -> 27.71). Two later runs refute it:
//     3 898 ns at load1 19.35 and 4 087 ns at 23.40, both BELOW the 5 941 ns measured at 11.87.
//     Load raises the CEILING of this figure, not its value; treat the trend as an envelope.
//     27.71 is already about the latency box's documented IDLE load (ci.yml: "the latency box idles at load ~29").
//   * the journal MAX swings ~3x on identical work — 141.9 / 66.3 / 49.6 / 53.6 / 62.1 ms. It is
//     one `msync` of the same 16 MiB, so this is I/O weather, not a change in behavior.
//   * `hops>100µs` is the LEAST predictable: 4 / 1 / 17 / 2 / 11, and NOT monotone in load — the
//     17 came at load 11.87, the 2 at 27.71. See `JOURNAL_HOPS_OVER_100US`.
//   * BASELINE max has a new worst of 37 150 ns (was 34 595) — still 54x under its 2 ms ceiling,
//     and still two orders of magnitude below the un-shielded regime that ceiling is placed to
//     detect. Noted for the record, not a concern.
//
// ⚠ WORST-GREEN DRIFT — the one trend that has NOT settled. The worst journal p99.9 measured in a
// GREEN run has risen on each of the last three observations: 42 580 -> 47 970 -> 52 619 ns. The
// 250 000 ns ratchet still holds, but its real margin has fallen 5.9x -> 5.2x -> 4.75x, and this
// is the figure `JOURNAL_P999_NS`'s own doc names as the one to watch. It is not yet a flake — but
// if a green run reports p99.9 above ~60 000 ns, stop treating the ratchet as settled and re-read
// the RATCHET block: the honest response is to fix the sync policy, NOT to raise the ratchet,
// because raising it is exactly the "record today's debt" move that this constant already made
// once and is meant to be lowered from, not repeated.
//
// ⚠ UPDATE (#932) — that instruction was FOLLOWED, twice, and it worked. The ratchet was not
// raised; the sync policy was fixed (#929, then #932), and the ratchet came DOWN 250 000 ->
// 100 000 ns. The 60 000 ns tripwire above stands unchanged and now sits BELOW the ceiling rather
// than 4.75x under it, so it fires as a re-read signal well before the gate does. Post-fix p99.9
// measures 35 357-39 955 ns across eight reps — see THE 2026-08-01 RE-MEASUREMENT below.
//
// THE RED-REGIME OBSERVATION, run 30695570400 — the one that actually validates the SIZING RULE
// below, because it is the only one where the box was hot enough to fail. Attempt 1 landed at
// load1 38.54 and attempt 2 (after the retry) at 19.35:
//
//     attempt  load1   variant    p50       p99        p99.9        max             hops>100µs
//     1        38.54   baseline    391 ns   1 262 ns     7 344 ns   34 595 ns          0
//     1        38.54   journal   1 743 ns  22 853 ns   153 279 ns   51 072 848 ns    241
//     2        19.35   baseline    281 ns     562 ns     3 898 ns   27 792 ns          0
//     2        19.35   journal     982 ns   6 362 ns    47 970 ns   48 036 028 ns     16
//
// Read it as three separate confirmations:
//   1. THE BASELINE CEILINGS HOLD AT load1 38.54 — the highest load ever measured here, well above
//      the latency box's ~29 idle. All four baseline assertions passed, p99.9 at 7 344 ns against a 50 000
//      ceiling. That is the answer to "will the new strict half flake", measured rather than
//      argued.
//   2. Attempt 1 went red on `p99 = 22 853 ns` — the PRE-EXISTING plan gate, not anything added
//      here. `JOURNAL_HOPS_OVER_100US` would also have failed (241), which is precisely the
//      already-red regime the SIZING RULE says costs nothing; the journal p99.9 of 153 279 ns
//      would still have PASSED its 250 000 ratchet even there.
//   3. The 3x retry did its job: attempt 2 passed everything. Its journal p99.9 of 47 970 ns was
//      the worst green-regime value on record when this was written — SUPERSEDED since by
//      52 619 ns (run 30696024454, in the table above), which is why the margin quoted on
//      `JOURNAL_P999_NS` is 4.75x and not the 5.2x this point originally claimed.
//
// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE 2026-08-01 RE-MEASUREMENT (#932) — the reference for every `JOURNAL_*` constant BELOW, and
// the reason all three came down. Everything above this line predates #929 and is kept as history.
//
// Two changes landed between that history and these numbers, and they are the whole story:
//   * #929 moved the per-chunk `msync` and the roll seal off the fold thread onto a syncer thread.
//     That is what removed the 141.9 / 66.3 / 49.6 / 53.6 / 62.1 ms maxima the ratchets recorded.
//   * #932 (this change) moved the THIRD one — `CoreThread::write_snap`'s blocking
//     `CommandJournal::flush()`, taken once per `snapshot_every` records — off it as well, and
//     added the `journal-snap` variant that can SEE it. The old two variants both pin
//     `snapshot_every = u64::MAX`, so no cadence snapshot has ever fired inside a measured hop and
//     the gate was configured differently from production in exactly the dimension that mattered.
//
// Measured on the latency box by hand, the same way ci.yml measures — cores 28-31, `sudo chrt -f 50`,
// `--test-threads=1`, 100 000 hops per variant. The four BEFORE reps at load1 4.7-7.7 and the four
// tabulated AFTER reps at load1 17.0 — so the after-column comes from the BUSIER box of the two and
// the improvement below is understated, not flattered:
//
//     rep  variant        p50       p99        p99.9       max            hops>100µs
//     BEFORE (#932's fix not applied — write_snap still calls the blocking flush)
//     1    journal         1 002 ns   5 249 ns   38 613 ns      77 696 ns       0
//     1    journal-snap    1 183 ns   6 112 ns  153 899 ns  14 123 277 ns     102
//     2    journal         1 011 ns   5 070 ns   35 367 ns      70 893 ns       0
//     2    journal-snap    1 183 ns   4 990 ns   82 596 ns  10 726 410 ns      97
//     3    journal         1 082 ns   6 272 ns   42 900 ns     176 773 ns      13
//     3    journal-snap    1 183 ns   6 162 ns  107 332 ns  16 732 602 ns     101
//     4    journal         1 032 ns   4 168 ns   37 390 ns      85 331 ns       0
//     4    journal-snap    1 222 ns  10 059 ns  324 241 ns  14 823 225 ns     120
//     AFTER (write_snap posts a watermark to the syncer instead)
//     1    journal           1 012 ns  5 069 ns  35 998 ns  105 489 ns   1
//     1    journal-snap        972 ns  5 059 ns  36 369 ns  196 591 ns   2
//     2    journal             962 ns  4 398 ns  35 357 ns   96 231 ns   0
//     2    journal-snap        962 ns  4 709 ns  35 918 ns   72 046 ns   0
//     3    journal             942 ns  4 358 ns  35 597 ns   95 159 ns   0
//     3    journal-snap        952 ns  5 781 ns  39 955 ns  229 462 ns   6
//     4    journal             972 ns  4 739 ns  37 671 ns   97 874 ns   0
//     4    journal-snap        982 ns  5 440 ns  38 463 ns  155 593 ns   9
//
// EIGHT MORE AFTER-REPS were then taken against the LOWERED ceilings below, deliberately spanning
// three load bands, so the constants are sized on TWELVE post-fix reps (= 24 journal-variant
// measurements) rather than on the four tabulated above. Stated as ranges over all twelve:
//
//     variant       p50            p99              p99.9          max                 hops>100µs
//     journal        942-1 012 ns  3 937-10 089 ns  34 785-50 936     78 778-  456 140     0-28
//     journal-snap   932-1 012 ns  4 298- 5 951 ns  35 918-39 955     72 046-4 276 748     0- 9
//
// ⚠ FOUR OUTLIERS in that set, and every one of them binds a constant below — do not average them
// away:
//   * `journal-snap max = 4 276 748 ns` at load1 15.0, in a rep whose p99 was a healthy 4 558 ns
//     and which had just 2 hops over 100 µs — one lone spike in a clean run. That is the
//     **~4 ms timer-tick `park()` wakeup**, the historical flake signature of this gate (recorded
//     maxima 4.000814 / 4.000218 / 4.004 / 4.016 ms, p50 healthy throughout). It is quantized, not
//     scaling — and it is what actually sets [`JOURNAL_MAX_NS`]'s headroom (2.3x), NOT the 456 µs
//     worst of everything else (22x).
//   * `journal hops>100µs = 28` at load1 15.0 — in a rep that was itself RED on the pre-existing
//     `p99 < 10 µs` plan gate (10 089 ns), i.e. an already-failing attempt. The worst count from a
//     fully GREEN post-fix rep is 20, which is what [`JOURNAL_HOPS_OVER_100US`]'s 2.5x is measured
//     against.
//   * `journal hops>100µs = 17` at load1 7.9 — reproducing, exactly, the worst count this file had
//     ever recorded in a green run (17 at load1 11.87, pre-#929). Two independent samplings of the
//     jitter floor landing on the same number is why that ceiling is 50 and not 20.
//   * `journal p99.9 = 50 936 ns` — same p99-red rep; the worst from a green rep is 44 675 ns.
//
// AND THE RUN THAT MATTERS MOST — the first pass of this gate through CI ITSELF, on the latency box's own
// `the latency runner` runner, with ci.yml's quiesce preflight and retry loop rather than a hand-typed
// `chrt`. Run 30706529680, attempt 1, 7/7 green, `box quiesced after 10s`, load1 9.33:
//
//     variant       p50       p99       p99.9      max         hops>100µs
//     baseline        301 ns    632 ns    4 759 ns   28 194 ns     0
//     journal       1 083 ns  6 031 ns   54 463 ns  120 207 ns     2
//     journal-snap  1 012 ns  5 270 ns   42 230 ns  120 137 ns     3
//
// ⚠ Note `journal p99.9 = 54 463 ns` — HIGHER than any figure quoted anywhere above, including the
// 52 619 ns this file had called the worst green value on record. Two things follow, and neither of
// them is "raise the ceiling": (a) the honest headroom on `JOURNAL_P999_NS` is **1.8x**, not the
// 2.2x twelve hand-run reps suggested — CI's own environment is the one that counts; (b) that
// ceiling cannot flake on its own regardless, because at `JOURNAL_HOPS_OVER_100US` = 50 it is
// arithmetically implied (see its doc). If a future run pushes p99.9 past 100 µs, the hop ceiling
// will have gone first, and THAT is the number to read.
//
// AND THE ONE MEASUREMENT THAT EXERCISES THIS GATE *AS A GATE* — every table above compares two
// builds and reads the numbers; none of them answers "does the assertion actually fire on the
// defect?". Taken 2026-08-01 on the same box, independently of the four BEFORE reps: ONE clone, ONE
// binary path, the fix reverted in place by a one-line edit (`j.queue_sync()` ->
// `j.flush().expect("journal flush")`), rebuilt, and three reps run each way back to back.
// Deliberately WITHOUT the quiet-box protocol — no `chrt`, no `taskset`, load1 3.9 — to check the
// signature survives ordinary scheduling rather than only the reserved cores:
//
//     rep  variant        p50        p99         p99.9        max            hops>100µs  verdict
//     FIX REVERTED
//     1    journal-snap   1 603 ns   13 175 ns   299 474 ns   13 887 934 ns     197      RED
//     2    journal-snap   1 432 ns    6 662 ns   130 496 ns   14 558 328 ns     107      RED
//     3    journal-snap   1 553 ns   12 123 ns   251 914 ns   18 158 369 ns     145      RED
//     FIX APPLIED (minutes earlier, same binary path)
//     1    journal-snap     962 ns    4 438 ns    37 861 ns      182 955 ns       1      green
//     2    journal-snap     962 ns    4 469 ns    37 050 ns      247 536 ns       2      green
//     3    journal-snap     991 ns    6 021 ns    44 905 ns      101 521 ns       1      green
//
// `journal-snap` is RED 3/3 reverted and green 3/3 applied, so the variant is a real regression
// detector and not decoration. All THREE lowered ceilings fired on all three reverted reps
// (max 13.9-18.2 ms vs 10 ms; hops 107-197 vs 50; p99.9 130-299 µs vs 100 µs) — which also puts
// three more independent observations under [`JOURNAL_MAX_NS`], the constant whose 2.3x headroom is
// the weakest of the three: the smallest defect max on record across all seven pre-fix reps is now
// 10.7 ms, still above the 10 ms ceiling, and the nearest real timer-park is 4.28 ms.
//
// TWO honest notes on this band, neither of which softens the result. The hop counts run HIGHER than
// the quiet-box 97-120 because unpinned scheduling stacks its own jitter on top of the ~97 snapshot
// stalls — the snapshot signature is a FLOOR, not a ceiling. And rep 1's `journal` variant went red
// too (hops 125), which is NOT this defect — that variant pins `snapshot_every = u64::MAX` and fires
// no cadence snap — but the box: its p50 read 1 743 ns against the 942-1 072 ns the other reps
// measured, which is exactly the "p50 doubling means the box, not the code" tell the triage notes
// above name. Read p50 first on any red.
//
// AND ONE DELIBERATELY-LOADED BAND, taken while a sibling `cargo test -p vike-core` build was
// resident on the box (load1 46.0) — exactly the regime ci.yml's quiesce preflight exists to avoid.
// Both reps failed, and the SIZING RULE below is why that is not a reason to loosen anything:
//
//     rep  variant        p50        p99         p99.9       max            hops>100µs
//     1    journal        1 834 ns    9 969 ns   98 506 ns     271 812 ns      92
//     1    journal-snap   1 833 ns   10 379 ns   93 386 ns   2 386 757 ns      83
//     2    journal        1 904 ns   10 580 ns  104 086 ns     268 466 ns     115
//     2    journal-snap   1 903 ns   15 990 ns  107 393 ns  18 295 928 ns     132
//
// Read it as the confirmation it is: in that band BOTH journal variants are already red on the
// PRE-EXISTING `p99 < 10 µs` plan gate (and the baseline failed too, on `hops>100µs = 4`), so the
// lowered ceilings add no flake surface there — they redden a run that was going to be red anyway.
// It also shows the p50 doubling (0.95 -> 1.9 µs) that is the tell for "this is the box, not the
// code": read p50 FIRST on any red, exactly as the triage notes say.
//
// READ IT AS FOUR FACTS:
//   1. `hops>100µs` is the signature, and it is ARITHMETIC rather than weather: the run takes
//      100 000 / 1024 = 97 cadence snapshots, and the before-column reads 102 / 97 / 101 / 120.
//      One hop over 100 µs per snapshot, near-exactly. After, over twelve reps: 0-9, i.e. at or
//      BELOW the `journal` variant's own jitter floor (0-28).
//   2. The worst hop was 10.7-16.7 ms — ~1 400x the 10 µs the gate's own name states — and it is
//      one `msync` of a 64 MiB mapping carrying ~287 KB of dirty data (1024 records x ~287 B).
//      After: 0.072-0.229 ms across eleven of the twelve reps, and a single 4.28 ms timer-park in
//      the twelfth. So the improvement on the worst is 73x measured against real work, and ~4x even
//      if you charge the fix for an OS park it did not cause.
//   3. It reached the PLAN GATE, not just the tail: rep 4's before-run measured
//      `journal-snap p99 = 10 059 ns`, a genuine `p99 < 10 µs` failure caused solely by running
//      the production `snapshot_every`. After, `journal-snap` p99 never exceeded 5 951 ns in any of
//      the twelve — while the older `journal` variant breached 10 µs once (10 089 ns), which is the
//      pre-existing marginality documented in the flake notes, not something #932 introduced.
//   4. p50 improved too (1 183-1 222 -> 932-1 012 ns) despite the busier box. The blocking flush
//      also msync'd the WHOLE mapping without advancing the syncer's own `synced` cursor, so the
//      next chunk request re-covered ground the fold thread had already paid for.
//
// AND ONE NULL RESULT WORTH KEEPING: after the fix, `journal` and `journal-snap` are the same
// distribution over all twelve reps — p50 0.94-1.01 vs 0.93-1.01 µs, p99.9 34.8-50.9 vs
// 35.9-40.0 µs — and if anything the SNAPSHOTTING variant is the TIGHTER of the two on p99, p99.9
// and hop count, its one 4.28 ms park notwithstanding. That is what licenses gating both with ONE
// set of constants, and it is the property that breaks the moment a blocking call returns to the
// snapshot path.
//
// ⚠ TWO CORRECTIONS FROM CI's SECOND RUN (30707685535, load1 16.8-17.8 throughout — a materially
// busier box than the 9.33 of the first). Recorded because both weaken claims made above, and this
// file's rule is that the measurement wins:
//
//   * That NULL RESULT does not survive as stated. `journal-snap` measured **p99 = 10 470 ns** on
//     that run's attempt 2 — so "never exceeded 5 951 ns" is true only of the twelve HAND-RUN reps,
//     and "the tighter of the two" is not a property to rely on. Both variants have now breached the
//     10 µs plan gate once each in CI. What still holds is the weaker claim that actually licenses
//     the shared constants: after the fix the two are the SAME distribution, neither reliably
//     tighter. Note the p50 on that attempt was a healthy 992 ns, so the file's own "p50 doubling
//     means the box" tell did NOT fire — load1 16.8 and hops 31 (vs 1 on the passing attempt) are
//     what indict the box there, not p50.
//   * The run needed ALL THREE attempts (1 and 2 red, 3 green), not the attempt-1 pass the first two
//     runs got. Both failures were on the PRE-EXISTING `P99_BUDGET_NS` 10 µs gate — assertion order
//     puts it first, so no lowered ratchet is implicated in either. But one honest caveat: attempt
//     1's `journal` rep also carried `hops>100µs = 80`, which WOULD have failed
//     [`JOURNAL_HOPS_OVER_100US`] = 50 had it reached that assert. That is the SIZING RULE below
//     working as designed rather than a counter-example — that rep was already red on the
//     pre-existing gate (p99 12 324 ns) with the p50 tell showing (1 463 ns vs 942-1 072 ns) — but
//     it is the first CI sighting of a lowered ceiling being reachable, and it is the number to
//     watch. If this gate starts costing attempts on GREEN-p99 runs, raise
//     `JOURNAL_HOPS_OVER_100US` first.
// ─────────────────────────────────────────────────────────────────────────────────────────────
//
// SIZING RULE. the latency box is shared (ClickHouse, a live hl-node, the CI build fleet) and shielding does
// NOT make it deterministic — the CI step runs three attempts for precisely that reason, and its
// quiesce preflight caps its wait and then measures anyway. So a new ceiling is only worth adding
// if it survives the runs that pass TODAY; a gate that flakes is worse than no gate. Note the
// asymmetry that follows from that, because it decides how much headroom each half gets:
//
//   * BASELINE — `p99 < 10 µs` passes even on a loaded box (ci.yml's own A/B: baseline p99 is
//     2.5–7.9 µs under SCHED_FIFO with sibling builds resident). So a new baseline tail ceiling
//     CAN redden a run that is green today, and these ceilings are sized against that LOADED
//     regime, not against the quiet reference row above.
//   * JOURNAL — `p99 < 10 µs` is ALREADY red in the FULLY loaded regime (15–38 µs, 0/3 attempts,
//     per the same A/B), so a journal tail ceiling adds no new flake surface there. But there is a
//     MIDDLE regime and it is not hypothetical: run 30694882148 measured journal `p99 = 6 291 ns`
//     (green) alongside 17 hops over 100 µs. Journal ceilings are sized for that middle, not for
//     the quiet row alone — which is what moved `JOURNAL_HOPS_OVER_100US` off its first value.
// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE 2026-08-05 CALIBRATION — why the journal variants stopped sharing the baseline's p99 bar.
//
// Every ceiling above was DERIVED from measurements. The p99 bar was not: `10_000` is the R5 plan
// literal, written when this file gated one variant, carried verbatim into `P99_BUDGET_NS` by #921,
// and then applied to two journal variants that had never been measured against it. This block is
// the measurement nobody had taken, and [`JOURNAL_P99_NS`] is what it produced.
//
// THE DATA — not a hand-run bench: every `latency` job CI itself ran on the latency box's `the latency runner`
// runner across the 250 most recent `ci.yml` runs, 2026-08-04 13:41Z - 2026-08-05 18:24Z (~29 h).
// **98 measured attempts**, all post-#932, each printing one `LATENCY-GATE` line per variant.
// RE-DERIVE IT (shell only, ~5 min; every figure below comes straight out of this):
//
//     gh run list --workflow=ci.yml --limit 250 --json databaseId --jq '.[].databaseId' |
//     while read -r r; do
//       j=$(gh api "repos/vike-io/vike-trader-private/actions/runs/$r/jobs" \
//             --jq '.jobs[] | select(.name=="latency") | .id')
//       [ -n "$j" ] && gh api "repos/vike-io/vike-trader-private/actions/jobs/$j/logs" |
//         grep -E 'LATENCY-GATE variant=(baseline|journal|journal-snap) '
//     done
//
// An attempt is GREEN here iff EVERY ceiling in this file passes on it — computed from the printed
// figures, not read off the job conclusion, so a retry survivor cannot be miscounted as healthy.
// **80 GREEN / 18 RED.**
//
// WHAT THE 98 ATTEMPTS SAY, taking each attempt's WORST journal-variant p99 (the figure the gate
// actually asserts on), in ns — the whole distribution, `|` marking the old 10 000 ns bar:
//
//     4188 4208 4278 4378 4388 4388 4469 4478 4479 4498 4518 4569 4579 4589 4609 4619 4619 4619
//     4619 4629 4719 4729 4789 4810 4859 4959 5029 5039 5060 5159 5170 5230 5240 5250 5300 5370
//     5510 5530 5580 5741 5781 5851 5851 5911 6011 6011 6101 6122 6172 6181 6462 6512 6612 6633
//     6652 6763 6783 6793 6823 6903 6923 6933 7144 7153 7203 7304 7314 7394 7635 7644 7885 8186
//     8205 8235 8296 8546 8566 8646 8686 8706 8897 9047 9377 9378 9659 9758 9809 9928 | 10059
//     10711 10921 10990 11271 11802 13565 15219 15629 18174
//
//   * worst GREEN = **9 809 ns** — 98.1% of the bar. That is **1.9% headroom on code containing no
//     PR under test** (run 31004929871, `push: main`, 2026-08-05 12:16Z, attempt 1, released by the
//     quiesce preflight, every other ceiling green).
//   * second worst GREEN = 9 758 ns, also `push: main` (run 31014508613, 14:18Z). Not one unlucky
//     sample: **13 of the 80 GREEN attempts (16%) landed within 20% of the bar.**
//   * a gate whose noise floor reaches 98% of its ceiling cannot distinguish a regression from
//     weather — any single red it produces is, by construction, inside the distribution of runs
//     that pass.
//   * **3 of the 98 attempts were red SOLELY on this literal** (10 711 / 10 921 / 10 990 ns, all
//     `journal-snap`; runs 30990576982 / 30944193736 / 30926215593, each attempt 1). Every one had
//     a healthy baseline (321-411 ns) and a `journal` sibling at 3.7-4.0 µs with ZERO hops over
//     100 µs. One variant spiked; nothing regressed. Those three are what [`JOURNAL_P99_NS`] turns
//     green — and NOTHING ELSE does: all 15 remaining reds carry a `hops>100µs` and/or p99.9 cause
//     that stands on its own.
//   * ⚠ IN PARTICULAR it does not paper over the two runs that failed 3/3 (30968437855 and
//     30968479806, created 2026-08-05 02:07/02:08Z — the pair whose logs literally read "latency
//     gate failed 3x, every attempt on a quiesced box — treat as a real regression, not jitter",
//     at load1 52-88). All six of those attempts stay RED under the new ceiling: they carried
//     121-205 hops over 100 µs against a ceiling of 50, and p99.9 104-144 µs against 100 µs. Their
//     misreading was never the p99 bar; it was the verdict string, and ci.yml is where that is
//     fixed.
//   * for contrast, the SAME attempts put the BASELINE variant's p99 at 331-1 272 ns in 74 of the
//     80 green ones against that same 10 000 ns bar — i.e. 8-30x under, where the journal variants
//     sit at 1.0-2.4x under. One literal was never going to be a calibrated ceiling for both, and
//     two PRs (#1087, #1090) were investigated at length as regressions before anyone checked what
//     `main` itself measures.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The plan gate itself, unchanged since R5: p99 core-hop < 10 µs — **for the BASELINE variant
/// only**, since the 2026-08-05 calibration above. 18x the baseline's measured 551 ns, and 8-30x
/// what the baseline actually measures in CI today (331-1 272 ns in 74 of 80 green attempts).
///
/// ⚠ It used to gate all three variants. It was never DERIVED for the journal ones — a literal
/// copied from the R5 plan, not a measurement — which is what [`JOURNAL_P99_NS`] fixes. Keeping it
/// here is deliberate: on the journal-free path it is a genuine 10-30x-headroom ceiling, and
/// tightening it is a separate question this change does not touch.
const P99_BUDGET_NS: u64 = 10_000;

/// The journal variants' p99 ceiling — **2.0x the worst GREEN observation** (9 809 ns) from the
/// 98-attempt calibration above.
///
/// SIZED FROM GREEN OBSERVATIONS ONLY, exactly like its three `JOURNAL_*` siblings, and for the
/// same reason: a figure from a RED attempt is evidence of how badly this gate can be disturbed,
/// never of how the code behaves when healthy. The 10 059-18 174 ns readings in the calibration
/// set and the historical 10 470 ns from run 30707685535 attempt 2 are therefore NOT inputs here —
/// they are sanity checks below, not the derivation.
///
/// THE MARGIN, and why 2.0x. This file's other three journal ceilings are 1.8x / 2.3x / 2.5x above
/// their own worst green observations ([`JOURNAL_P999_NS`] / [`JOURNAL_MAX_NS`] /
/// [`JOURNAL_HOPS_OVER_100US`]), so 2.0x is the middle of the family rather than a new precedent.
/// 9 809 x 2 = 19 618, rounded to 20 000 for legibility.
///
/// WHAT IT STILL CATCHES, all measured, none argued:
///   * the hot-box regime that made this gate red for real: 22 853 ns at load1 38.54 (run
///     30695570400 attempt 1) still fails, as does most of the 15-38 µs fully-loaded A/B band
///     ci.yml documents.
///   * the #932 defect — a blocking `msync` back on the fold — is caught 3/3 by the three siblings
///     (p99.9 130-299 µs vs 100 µs; max 13.9-18.2 ms vs 10 ms; hops 107-197 vs 50) in the
///     fix-reverted A/B above. ⚠ Note the p99 assert caught it in only 1 of those 3 reps even at
///     10 000 ns (6 662 / 12 123 / 13 175 ns): **p99 was never the discriminator for the journal
///     variants**, so raising it costs this gate no detection power it demonstrably had.
///   * and it stops SHADOWING the ceilings that do the work: `assert_hop_budget` asserts p99 FIRST,
///     so a journal red reported the LEAST informative number in the file. 15 of the 18 reds in the
///     calibration set carry a `hops>100µs` and/or p99.9 cause — read those first, and now the
///     failure message names one of them.
///
/// WHAT IT GIVES UP, stated plainly: a regression that adds under ~10 µs to the journal p99 while
/// leaving p99.9, max and the hop count alone now passes. The 10 000 ns bar did not actually detect
/// that class either — with green observations at 98.1% of it, a single red there is
/// indistinguishable from the noise floor — so this trades a detection nobody could act on for a
/// verdict that means something. Buying real p99 detection power back means LOWERING THE NOISE (the
/// residual per-append serde_json + mapped-page memcpy documented in the `JOURNAL_*` block), then
/// re-deriving this constant downward. Same discipline as the ratchets: the fix is the response, not
/// the ceiling.
///
/// ⚠ TRIPWIRE, in the style of [`JOURNAL_P999_NS`]'s: **if a GREEN run reports a journal-variant
/// p99 above ~12 000 ns, the noise floor has moved again** — re-run the harvest above and re-derive
/// from the new green distribution rather than nudging this literal. Nudging is precisely how the
/// 10 000 ns bar reached the journal variants in the first place: written for one variant, copied
/// onto a second by `bbe2dbd8` (2026-07-10), folded into a shared constant by #921 (2026-08-01),
/// re-used by a third variant in #932 — four steps, no measurement at any of them.
const JOURNAL_P99_NS: u64 = 20_000;

/// Baseline p99.9 ceiling — 5x [`P99_BUDGET_NS`], 6.4x the WORST of the five measurements
/// (2 104 / 2 645 / 5 941 / 7 764 / 4 087 ns).
///
/// Sized to clear the loaded-box regime (where baseline p99 alone reaches 7.9 µs, so p99.9 is
/// necessarily some multiple of that) while still being ~6x TIGHTER than the journal variant's
/// p99.9 — i.e. tight enough that the class of stall this file exists to expose would fail here if
/// it ever showed up with journaling OFF.
///
/// ⚠ This is the TIGHTEST of the four baseline ceilings and the one most likely to flake first: it
/// is the only baseline figure here whose ENVELOPE widens with load. Not its value, though — an
/// earlier revision of this doc read the first three points (2 645 -> 5 941 -> 7 764 ns at load1
/// 5.25 -> 11.87 -> 27.71) as a monotone rise, and two later runs refute that: 3 898 ns at load1
/// 19.35 and 4 087 ns at 23.40, both below the 5 941 ns seen at 11.87. Load buys VARIANCE, so the
/// figure to size against is the envelope's top, which is what the 6.4x above is measured from.
///
/// Two facts mitigate it, both measured rather than argued: 27.71 is already about the latency box's
/// documented IDLE load, and the ONE observation from a genuinely hot box — load1 38.54, hot
/// enough that the journal half went red on the pre-existing p99 gate — still put this at
/// 7 344 ns, i.e. 6.8x under. Every green observation also came on ATTEMPT 1, before the 3x
/// retry did any work.
///
/// If it does flake, the honest fix is 100 000 — but note that at 100 000 it becomes strictly
/// IMPLIED by [`BASELINE_HOPS_OVER_100US`] (at most 2 hops over 100 µs forces the 100th-worst hop
/// below 100 µs) and stops carrying information of its own. Prefer keeping it here and reading a
/// red run as a real question.
const BASELINE_P999_NS: u64 = 50_000;

/// Baseline max ceiling — 77x the measured 25 709 ns.
///
/// Placed deliberately BETWEEN the two scheduling regimes ci.yml's own A/B measured: SCHED_FIFO +
/// taskset holds the baseline max to 0.07–0.23 ms even with sibling the latency box builds resident, while
/// WITHOUT FIFO the same variant maxes at 2.7–3.4 ms. So 2 ms passes ~9x over the worst SHIELDED
/// observation, and goes red if the shielding silently stops applying — the CI step's
/// `chrt unavailable → taskset only` fallback branch is a real path that nothing reports today,
/// and a measurement taken through it is not comparable to the numbers above.
const BASELINE_MAX_NS: u64 = 2_000_000;

/// Baseline `hops > 100 µs` budget, out of [`HOP_SAMPLES`]. Measured **0**, and 0 is the TARGET.
///
/// It is not asserted as `== 0` because that is a threshold with zero headroom by construction,
/// and the evidence says the shielded-but-loaded regime produces one: ci.yml records a baseline
/// max of 0.07–0.23 ms with sibling builds on the box, which IS a hop over 100 µs, in every rep of
/// that regime. The quiesce preflight that avoids it caps its wait at 420 s and then explicitly
/// measures anyway ("still busy … — measuring anyway, expect jitter"), so the regime is reachable
/// by design rather than by accident.
///
/// **TIGHTEN TO 0** once a run at load 65+ is on record passing with 0 — the same confound #915
/// flagged for the `system.slice AllowedCPUs=0-27` fence, whose effect is still unseparated from
/// "the box happened to be quiet".
const BASELINE_HOPS_OVER_100US: usize = 2;

// ── JOURNAL VARIANTS: A RATCHET, NOT A BUDGET ───────────────────────────────────────────────
//
// The three `JOURNAL_*` constants below record TODAY'S measured tail so that it stops being
// invisible. They are NOT a statement that this tail is acceptable — the tightest of them is still
// 5x the p99 budget this gate's own name states — and they are meant to be LOWERED as the journal's
// I/O policy improves. **All three move together, or none does**: each catches a different shape of
// the same defect (see each constant's doc), and lowering one alone just relocates a regression into
// the two that stayed loose.
//
// ⚠ [`JOURNAL_P99_NS`] joined them on 2026-08-05 and takes the same discipline (derived from GREEN
// observations, lowered when the noise is fixed rather than nudged when it bites) — but it is NOT a
// fourth member of the move-together set. The three below all catch ONE defect shape from three
// angles; the p99 ceiling separates the journal path's ordinary cost from the baseline's, and its
// derivation stands on its own calibration block above.
//
// THEY HAVE NOW BEEN LOWERED ONCE, and by a lot — #932, from the numbers in the re-measurement block
// above:
//
//     constant                   was            now           why it could move
//     JOURNAL_P999_NS            250 000 ns     100 000 ns    #929 + #932
//     JOURNAL_MAX_NS             500 000 000    10 000 000    #929 removed the 141 ms chunk msync;
//                                                             #932 the 10.7-16.7 ms snap msync
//     JOURNAL_HOPS_OVER_100US    100            50            the ~97/run snapshot stalls are gone
//
// WHAT THE OLD VALUES RECORDED, so a reader can tell what was fixed from what was merely re-sized:
// `journal.rs`'s `write_framed` used to call `sync_completed_region()` — a BLOCKING `msync` — inline
// on the fold path, one per `sync_chunk_bytes`. On this harness's 256 MiB segment that is a 16 MiB
// chunk, the run's ~28 MB of WAL crosses it exactly once, and that ONE crossing was the 141.86 ms
// core hop at `max_at_hop=58518`. #929 moved it (and `roll`'s whole-segment seal) to a syncer thread
// the appender never joins. #932 then found the third: `write_snap`'s `flush()`, ~97 times per run
// at the production `snapshot_every`, worth 10.7-16.7 ms each.
//
// The `wal_bytes_min=` / `snap_every=` / `snaps_expected=` figures on each journal summary line exist
// to keep that arithmetic checkable straight from the CI log rather than inferred, on every run.
//
// ⚠ WHAT IS LEFT, and why these are not baseline-tight. Even with every `msync` off the thread, the
// journal variants sit ~3.5x above the baseline on p50 (~0.97 µs vs ~0.28 µs) and ~9x on p99.9
// (~37 µs vs ~4 µs). That residue is the append itself — one `serde_json` serialization plus a
// memcpy into a mapped page, and first-touch write faults on fresh segment pages — NOT a blocking
// syscall. The measured breakdown lives in the latency-gate flake notes: append p50 210 ns / p99
// 3.1 µs measured directly against `CommandJournal`, with 3-9 appends per 100 000 over 100 µs from
// sparse-segment faults, which `reserve_blocks` now largely removes at `open` (but deliberately NOT
// at `roll`, which runs on the append path).
//
// Journaling is OFF by default (no `VIKE_JOURNAL_DIR`, no run-profile journal sink ⇒ no WAL), so
// this cost is opt-in today; `vike-tradehub` on the production box is the caller that opts in.

/// Journal p99.9 ratchet — **1.8x** the highest post-#932 measurement, which came from the FIRST
/// pass of this gate through CI itself (54 463 ns; run 30706529680, green, 7/7, load1 9.33). The
/// twelve hand-run reps that preceded it spanned only 34 785-50 936 ns and would have suggested a
/// comfier 2.2x — CI's environment is the one that counts, so 1.8x is the number stated here. For
/// scale, 54 463 ns is itself ABOVE the 52 619 ns this file had recorded as the worst-ever green
/// value, in the pre-#929 table.
///
/// Lowered 250 000 -> 100 000 by #932. It is the most reproducible of the three journal numbers and
/// the one this file has always told readers to watch — its green-regime spread widened 42% over
/// six observations before #929, from 36 929 to 52 619 ns, and the correct response to a breach was
/// never to raise it but to take a blocking call off the fold. Twice now that has been the actual
/// fix (#929, #932).
///
/// ⚠ **HONEST CAVEAT: at [`JOURNAL_HOPS_OVER_100US`] = 50 this ceiling is arithmetically IMPLIED and
/// no longer binds.** `p999` is the 100th-largest of 100 000 hops, so "at most 50 hops exceed
/// 100 µs" already forces the 100th-largest below 100 µs. It is kept, at a value that comes down
/// with its siblings, for three reasons and not because it is doing work: it is the figure quoted in
/// the failure message, it is the one trended run-to-run from the `LATENCY-GATE` line, and it
/// becomes binding again the instant `JOURNAL_HOPS_OVER_100US` is raised back over 100. Making it
/// bind TODAY would mean going below ~54 463 ns — the worst green run on record, and a CI one at
/// that — which is how you buy a flaky gate. The same implication is what makes the 1.8x above
/// survivable: this assertion cannot fail before the hop ceiling does.
const JOURNAL_P999_NS: u64 = 100_000;

/// Journal max ratchet — **22x** the worst post-#932 measurement of real work (456 140 ns) but only
/// **2.3x** the worst measurement of any kind (4 276 748 ns, a timer-tick `park()` — see below).
/// The 2.3x is the number that matters; the 22x is what the constant would be worth if OS parks did
/// not exist.
///
/// Lowered 500 000 000 -> 10 000 000 by #932, a 50x cut. The old value sat ~350x above reality once
/// #929 landed, which made it useless as a gate: it existed to record a 141.86 ms chunk-`msync` hop
/// that no longer happens.
///
/// 10 ms rather than something tighter, sized from THREE numbers that are all measurements:
///   * it must clear the **~4 ms timer-tick `park()` wakeup**, and this is the BINDING constraint,
///     not a theoretical one — a post-fix `journal-snap` rep measured `max = 4 276 748 ns` in an
///     otherwise clean run (p99 4 558 ns, 2 hops over 100 µs). It is the historical flake signature
///     of this gate (recorded maxima 4.000814 / 4.000218 / 4.004 / 4.016 ms with a healthy p50) and
///     it is tick-QUANTIZED, so it does not scale with load: 10 ms leaves room for one park plus
///     everything else, and even two stacked parks stay under. Every other post-fix measurement is
///     ≤ 456 µs, so absent a park the real headroom is 22x.
///   * it must also clear the un-shielded regime's 3.5 ms journal max, so that ci.yml's
///     `chrt unavailable -> taskset only` fallback does not redden this — detecting THAT is
///     [`BASELINE_MAX_NS`]'s job, deliberately, and duplicating it here would only make two ceilings
///     fail for one cause.
///   * it must still catch the defect it was lowered for. Every one of the four pre-#932 reps
///     measured a max ABOVE it (10.7 / 14.1 / 14.8 / 16.7 ms), so a re-introduced blocking flush in
///     `write_snap` reddens this ceiling on its own, 4/4. ⚠ But note the gap between the defect
///     (10.7 ms at its smallest) and the park (4.3 ms) is only ~2.5x, so this ceiling is the
///     WEAKEST of the three discriminators; [`JOURNAL_HOPS_OVER_100US`] is the one to read first on
///     a red run.
///
/// What it can and cannot catch: it is a depth sentinel only. A change that halves the depth of a
/// stall and DOUBLES its frequency sails under here while nothing improves — that is what
/// [`JOURNAL_HOPS_OVER_100US`] is for. Lower BOTH together, or neither.
const JOURNAL_MAX_NS: u64 = 10_000_000;

/// Journal `hops > 100 µs` ratchet, out of [`HOP_SAMPLES`] — **2.5x** the worst count from a GREEN
/// post-#932 rep (20, on the `journal` variant; `journal-snap`'s own worst over twelve reps is 9,
/// and the one 28 came from a rep already red on the plan gate) and 2.9x the worst count ever seen
/// in a green run of this file before #929 (17, at load1 11.87 — a number a post-fix rep then
/// reproduced exactly).
///
/// Lowered 100 -> 50 by #932, and **this is the constant that actually binds** now that
/// [`JOURNAL_P999_NS`] is implied by it (see that doc). It is the one ceiling that catches the
/// defect #932 fixed as ARITHMETIC rather than as weather: a run journaling at the production
/// `snapshot_every = 1024` takes 100 000 / 1024 = 97 cadence snapshots, so a blocking flush inside
/// `write_snap` puts ~97 hops over 100 µs into the measurement — the four pre-fix reps read
/// 102 / 97 / 101 / 120, i.e. `snaps_expected` plus jitter, every time. 50 sits ~2x below that
/// signature and ~2.5x above the worst green observation, which is as clean a separation as this
/// gate's history offers. (A tighter 20 would have been a mistake: post-fix reps measured 17 and
/// 20 on the `journal` variant with p99 comfortably green.)
///
/// ⚠ Do not read the count as pure arithmetic in general, though — the pre-#929 history refutes
/// that. A first draft of the old constant sat at 32 on exactly that theory and the third reference
/// run measured 17 hops over 100 µs from a run crossing ONE chunk boundary, i.e. 16 of them were
/// load jitter; the fourth then reported just 2 at a much HIGHER load1 of 27.71. Jitter dominates
/// the count whenever no per-event stall is present, and it is not monotone in load. What #932
/// changes is that there is now a KNOWN arithmetic contribution (one per snapshot) sitting well
/// above that jitter floor, which is what makes a 2x separation trustworthy.
///
/// The fully-loaded regime ci.yml documents (251 / 410 / 367) blows through this — but `p99` was
/// 15-38 µs in those runs, i.e. already red on the plan gate, so this adds no flake surface there:
/// run 30695570400's attempt 1 measured 241 at load1 38.54, on an attempt that had ALREADY failed on
/// `p99 = 22 853 ns`. The MIDDLE regime is the real risk, and it is why this is 50 and not 20: the
/// 17-hop run had `p99 = 6 291 ns`, comfortably green.
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
    fn from_hops(label: &str, hops_seq: Vec<u64>) -> Self {
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
        Self { n, p50: pct(0.50), p99: pct(0.99), p999: pct(0.999), max, max_idx, over_100us }
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
        let Self { n, p50, p99, p999, max, max_idx, over_100us } = *self;
        let line = format!(
            "LATENCY-GATE variant={label} n={n} p50_ns={p50} p99_ns={p99} p999_ns={p999} \
             max_ns={max} max_at_hop={max_idx} hops_over_100us={over_100us}{extra}\n"
        );
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
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        sender.blocking_send(Event::Fill(fill_with_coid(ts, coid))).unwrap();
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
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

    let stats = HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap());
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
        file: vike_core::journal::JournalFileConfig {
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
/// than the 10 µs this function's NAME states (see the calibration block): after the snap write
/// leaves the fold path the two variants measure the same shape of work, and holding them to one set
/// of numbers is what makes a re-introduced blocking snap flush fail here.
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

/// `CoreSnapshot::build`'s own cost at a given ORDER COUNT — the publish half of the core hop.
///
/// # ⚠ The harness this replaced encoded a premise its own crate refutes
///
/// It was called `snapshot_build_measured_off_path` and its ceiling carried the comment "the build
/// runs coalesced (~16ms), never per-event". **That is false, and this crate documents it as false
/// in two other places.** `crates/vike-core/src/runtime/mod.rs`'s `note_event` states it outright
/// ("#887 deferred rendering to publish, on the premise that … **That premise is false.**"), and
/// `crates/vike-core/CLAUDE.md` makes it one of the two facts its latency-gate section exists to
/// correct: the runtime ALSO publishes whenever the core is about to go idle, ungated by
/// `snapshot_interval`, so on a venue whose events arrive sporadically it publishes PER EVENT. The
/// same premise, held once before, cost #887 a 4.2x tail regression that #896 had to undo.
///
/// So this is not an off-path curiosity: on the request/response pacing every harness in this file
/// uses, `CoreSnapshot::build` runs once per message and its cost is INSIDE the next hop the gates
/// measure. Which is also why the old harness's numbers never reached anybody — it reported a MEAN
/// through `println!`, and libtest's capture swallows stdout on a PASS unless the runner passes
/// `--nocapture`, which the CI step does not. Nothing landed in the durable series either: that file
/// held ten variants and `snapshot-build` was not among them.
///
/// # What varies, and what deliberately does not
///
/// ONE axis: the order count. `build` walks the whole registry (`crates/vike-core/src/snapshot.rs`'s
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
    // harness measures what `CoreSnapshot::build` actually does — clone a refcount per entry.
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
    for seq in 0..SNAPSHOT_BUILDS as u32 {
        drop(last.take()); // the predecessor's free, kept OUT of this sample's window
        let t0 = base.elapsed().as_nanos() as u64;
        let snap = CoreSnapshot::build(
            u64::from(seq),
            &engine,
            &[],
            10_000.0,
            PriceCfg::default(),
            vike_exec::MarginCallConfig::default().mm_requirement,
            &recent,
            &bars,
            &[],
            &None,
            0,
            0,
            ReconBlock::default(),
            &[],
        );
        let t1 = base.elapsed().as_nanos() as u64;
        samples.push(t1.saturating_sub(t0));
        last = Some(snap);
    }

    let stats = HopStats::from_hops(label, samples);
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
    // `crates/vike-core/tests/runtime_smoke.rs`'s `fifo_order_preserved_across_idle_transitions`
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

    for i in 0..N {
        let mut req = template.clone();
        // Stamped AFTER the clone, so the template copy is not charged to the lane; the `Box` that
        // follows IS inside the window, because `OrderIntent::Submit(Box<OrderRequest>)` is the real
        // write contract and a producer genuinely pays for it.
        req.ts = base.elapsed().as_nanos() as i64;
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
        spin_until(&submitted, (resting + i + 1) as u64, "the measured submit");
    }
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, Arc::try_unwrap(hops).unwrap().into_inner().unwrap());
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

/// The book-lane hop WITHOUT a mounted strategy — the original harness, kept as the control.
fn run_book_hop(label: &str, levels_per_side: usize) {
    run_book_hop_inner(label, levels_per_side, None)
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
    run_book_hop_inner(label, levels_per_side, Some(symbols))
}

fn run_book_hop_inner(
    label: &str,
    levels_per_side: usize,
    mount_symbols: Option<Vec<vike_core::MountLeg>>,
) {
    const N: usize = HOP_SAMPLES;
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
    let cfg = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            if let Ingest::Book(b) = msg {
                let now_ns = base.elapsed().as_nanos() as u64;
                hop_hook.lock().unwrap().push(now_ns.saturating_sub(b.book.last_seq));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        // Mounted on the SAME (venue, symbol) the producer below publishes, so every book
        // message drives the strategy lane and builds a ctx.
        strategy: mount_symbols.map(|symbols| vike_core::StrategyMount {
            account: None,
            symbols,
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "OTHER".into(),
            interval: "1m".into(),
            strategy: Box::new(BookProbe),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let ticks = handle.tick_sender();

    // The ONE standing book, exactly as a live pump holds it: `levels_per_side` populated levels
    // per side (a venue's full standing depth), folded in place and handed out by `Arc`.
    let mut book = {
        let mut b = vike_model::L2Book::new(0.01);
        let bids: Vec<(f64, f64)> =
            (0..levels_per_side).map(|i| (100.0 - i as f64 * 0.01, 1.0 + i as f64 * 0.1)).collect();
        let asks: Vec<(f64, f64)> = (0..levels_per_side)
            .map(|i| (100.01 + i as f64 * 0.01, 1.0 + i as f64 * 0.1))
            .collect();
        b.apply_snapshot(1, &bids, &asks);
        Arc::new(b)
    };

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
    handle.shutdown_and_join();

    let stats = HopStats::from_hops(label, Arc::try_unwrap(hop_ns).unwrap().into_inner().unwrap());
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
/// filtered to `book_lane_hop_mounted`, on a quiet box.
#[test]
#[ignore = "release-only latency harness (see module doc)"]
fn book_lane_hop_mounted() {
    run_book_hop("book-hop-10lvl-unmounted", 10);
    run_book_hop_mounted("book-hop-10lvl-mounted", 10, &[]);
    run_book_hop_mounted("book-hop-10lvl-mounted-multi", 10, &["SECOND"]);
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
