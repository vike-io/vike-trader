//! **A series runs far below its declared cadence and an ALERT is produced — and every healthy
//! series measured on the real box stays SILENT.**
//!
//! ## The failure, and why the second half of that sentence is the harder one
//!
//! A binance perp depth lane recorded at 4 % of its true rate for FORTY DAYS. Nothing noticed,
//! because the only thing watching measured RECENCY: the broken lane produced a row every ~4.3 s
//! against a 300 s threshold, so every tick read healthy while a backtest read a book that
//! teleported every four seconds.
//!
//! Catching that is easy in isolation and worthless in isolation. A rate check that fires on a
//! quiet market is worse than no check at all — an operator mutes it inside a week and the next
//! forty-day collapse is invisible again, with the extra cost that everybody believes it is
//! watched. So most of the length below is series that must stay QUIET, each driven at a
//! distribution measured on the CI box rather than at a number somebody found reasonable:
//!
//! * the binance trade tape's real burstiness — a per-second p50 of 4 against a max of 2,056, with
//!   356 of 3,600 seconds carrying no trade at all;
//! * a polymarket family ROTATING every five minutes, instruments living ~600 s and dying;
//! * a polymarket token trading at ~0.25/s, which legitimately sees nothing for minutes;
//! * and the one that kills the obvious design — the market's THINNEST actively-traded binance
//!   pairs, measured at 0.0067 updates/s, sixty times slower than the broken lane.
//!
//! ## What is driven
//!
//! `vike_recorder::alerts::watchdog_tick` — the real production tick, the same call the daemon
//! binary makes, with the same arguments. Both inputs are plain data (`expected` is
//! `RecorderRuntime::expected_series()`, `live` is `RecorderHandle::liveness()`), so this needs no
//! store, no venue, no network and no clock: `now_ms` is a parameter all the way down.
//!
//! ⚠ Every rate below is a NAMED CONSTANT carrying its source. A magic number in a threshold test
//! is a threshold nobody can re-derive, and this repository has already paid 98 CI attempts for one
//! of those.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_data::Liveness;
use vike_recorder::Alerting;
use vike_recorder::alerts::{RecorderAlerts, SLOW_RULE_ID, watchdog_tick};
use vike_recorder::liveness::{CADENCE_WINDOW_MS, SilenceWatch};

// ---- the measured numbers, each with the source it came from -----------------------------------

/// `crates/vike-datahub/src/datahub_cli.rs`'s `DEFAULT_TICK_SECS`, in ms — the loop this drives.
const TICK_MS: i64 = 30_000;
/// The FAMILY rule's input, EMPTY on purpose: this file judges CADENCE, and an empty slice judges
/// no family — `crates/vike-recorder/tests/family_collapse_alert.rs` is where that rule is driven.
const NO_FAMILIES: &[(String, String)] = &[];
/// ...and `DEFAULT_SILENT_SECS`, the recency threshold that read green for forty days.
const SILENT_SECS: u64 = 300;
/// Ticks in one cadence window: 900_000 / 30_000 = 30.
const WINDOW_TICKS: usize = (CADENCE_WINDOW_MS / TICK_MS) as usize;

/// The BROKEN binance depth lane, items/s — §12.2 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` measured 0.41-0.43
/// updates/s across four one-hour windows on 2026-09-08 UTC.
const BROKEN_DEPTH_PER_S: f64 = 0.42;
/// The same lane as a RECURRENCE would present TODAY. Since 2026-09-10
/// `crates/vike-data/src/live_rec.rs`'s `stream_status` routes depth `GapStart`/`Stale`/
/// `LiveResume` into the depth lane as ordinary events, so they reach `ingest` and bump
/// `Liveness::rows`: ~20,000 reconnect cycles a day is ~0.46 marker items/s on top of the data.
/// **Marker inflation moves a BROKEN lane UP toward the floor**, which is the side the margin has
/// to be spent on — so this, not 0.42, is the number the threshold has to clear.
const BROKEN_DEPTH_WITH_MARKERS_PER_S: f64 = 0.88;
/// The HEALTHY binance depth lane — 295 applied diffs of 309 frames in 30 s, sampled live on the
/// box 2026-09-10 (`crates/bridges/binance/src/family/depth.rs`).
const HEALTHY_DEPTH_PER_S: f64 = 9.8;
/// The binance depth subscription's declared ceiling: `@depth@100ms`, so 10 publishes/s
/// (`crates/bridges/binance/src/family/market_feed.rs`'s `depth_ws_url` and `publish_book`).
const DEPTH_CEILING_PER_S: f64 = 10.0;
/// binance BTCUSDT.P trades/s — the WORST of §12.3's four measured hourly means, so the weakest
/// governor the real tape has been observed to be.
const TAPE_WORST_HOUR_PER_S: f64 = 20.30;

/// A polymarket instrument's book WHILE ALIVE (§12.3, per instrument — NOT the group total, which
/// is 26-28x larger and the wrong scale for a liveness key).
const POLY_BOOK_ALIVE_PER_S: f64 = 180.0;
/// A polymarket instrument's own trade tape (§12.6).
const POLY_TRADE_PER_S: f64 = 0.25;
/// How long a polymarket up/down instrument lives before the family rotates (§12.3), in ticks.
const POLY_LIFETIME_TICKS: usize = 20;

/// The market's THINNEST actively-traded pairs, measured 2026-07-11 across the binance/bybit/okx
/// keyless depth feeds and recorded on
/// `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_FRESHNESS_THRESHOLD`: gaps of
/// 44-47 s, "a near-dead pair updated only twice in 5 min".
const THIN_DEPTH_PER_S: f64 = 2.0 / 300.0;
/// A plausible tape for such a pair. ⚠ UNMEASURED — the 2026-07-11 sweep recorded the depth side
/// only. It is used here to show the governor SILENCES the lane, and the argument does not depend
/// on the value: any tape below the ceiling produces no verdict at all, which
/// `a_thin_pair_is_silenced_even_when_its_tape_is_just_under_the_ceiling` pins at the boundary.
const THIN_TAPE_PER_S: f64 = 0.05;

// ---- the harness -------------------------------------------------------------------------------

/// A sink that records what was delivered. Deliberately blind to `AlertTargets`, like
/// `tests/silence_alert.rs`'s: the recorder's rules set `in_process: false` because a headless
/// daemon has no toast surface, so an `InProcessSink` would legitimately see nothing.
#[derive(Clone, Default)]
struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

/// One simulated recorder: the real watch, the real alert mount, and an arrival record advanced
/// tick by tick exactly as `crates/vike-data/src/live_rec.rs`'s `ingest` advances the real one.
struct Recorder {
    watch: SilenceWatch,
    alerts: RecorderAlerts,
    pager: Pager,
    live: HashMap<String, Liveness>,
    now_ms: i64,
}

impl Recorder {
    fn new() -> Self {
        let pager = Pager::default();
        let mount = RecorderAlerts::mount(&Alerting::default(), Vec::new());
        let alerts = mount.with_sink(Box::new(pager.clone()));
        Recorder { watch: SilenceWatch::new(), alerts, pager, live: HashMap::new(), now_ms: 1_000 }
    }

    /// Deliver rows without judging — the half `tick` and the off-switch case share.
    fn deliver(&mut self, deliveries: &[(&str, u64)]) {
        self.now_ms += TICK_MS;
        for (key, items) in deliveries {
            if *items == 0 {
                continue;
            }
            let e = self.live.entry((*key).to_string());
            let e = e.or_insert(Liveness { rows: 0, last_ms: 0 });
            e.rows += items;
            e.last_ms = self.now_ms;
        }
    }

    /// Advance one 30 s tick: deliver, then run the real production watchdog over the result.
    fn tick(&mut self, expected: &[String], deliveries: &[(&str, u64)]) {
        self.deliver(deliveries);
        let now = self.now_ms;
        watchdog_tick(
            &mut self.watch,
            &mut self.alerts,
            expected,
            NO_FAMILIES,
            &self.live,
            now,
            SILENT_SECS,
        );
    }

    /// Only the CADENCE alerts. The recency and feed rules are different faults with different
    /// ids, and a test counting all three could pass for the wrong reason.
    fn slow_alerts(&self) -> Vec<FiredAlert> {
        self.pager.0.lock().unwrap().iter().filter(|a| a.rule_id == SLOW_RULE_ID).cloned().collect()
    }
}

fn keys(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Items delivered in tick `i` at a flat `per_s`, via an exact cumulative floor so a fractional
/// rate loses nothing to rounding over a long run.
fn flat(per_s: f64, i: usize) -> u64 {
    let upto = |n: usize| (per_s * (TICK_MS as f64 / 1_000.0) * n as f64).floor() as u64;
    upto(i + 1) - upto(i)
}

// ---- 1. THE DEFECT -----------------------------------------------------------------------------

const DEPTH: &str = "depth/binance/BTCUSDT.P";
const TAPE: &str = "trade/binance/BTCUSDT.P";

/// **THE ONE THAT MATTERS.** The measured broken cadence, sustained beside the measured tape, is
/// alerted — with both rates in the body.
///
/// This is #1749's lane exactly: 0.42 items/s against a subscription that declares 10/s, while the
/// SAME instrument's trade tape ran at 20.30/s. Forty days of that produced no signal of any kind.
#[test]
fn the_measured_broken_binance_depth_lane_alerts() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..WINDOW_TICKS + 1 {
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i)), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }

    let fired = r.slow_alerts();
    assert_eq!(fired.len(), 1, "expected exactly one cadence alert, got {fired:?}");
    let body = &fired[0].body;
    assert!(body.contains(DEPTH), "the alert must NAME the series — which one is the diagnosis");
    assert!(body.contains("0.42"), "the observed rate belongs in the body: {body}");
    assert!(body.contains("10.00"), "the expected rate belongs in the body: {body}");
    assert!(
        body.contains(TAPE) && body.contains("20.3"),
        "the governor and its rate belong in the body — without them an operator cannot tell a \
         broken lane from a dead market: {body}"
    );
}

/// **The regression the 2026-09-10 marker change created.** The same lane as it would present
/// TODAY: `stream_status` now writes depth `GapStart`/`LiveResume` into the depth series, so a
/// reconnect loop inflates the very counter the check reads — roughly doubling it.
///
/// At `CADENCE_FLOOR_FRACTION = 0.20` the floor is 2.0/s and the inflated broken rate is 0.88/s:
/// still 2.2x clear. At 0.10 the floor would be 1.0/s and the margin 1.08x — a check the next
/// reconnect loop could walk straight past. This test is why the constant is 0.20.
#[test]
fn it_still_alerts_once_the_status_markers_are_counted_too() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..WINDOW_TICKS + 1 {
        let broken = flat(BROKEN_DEPTH_WITH_MARKERS_PER_S, i);
        let d = [(DEPTH, broken), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }
    let fired = r.slow_alerts();
    assert_eq!(fired.len(), 1, "marker inflation must not hide a 4 % lane");
    // The margin this test defends, read out of the alert the production path produced rather
    // than recomputed here from the same constants that produced it.
    let body = &fired[0].body;
    assert!(
        body.contains("0.88") && body.contains("10.00"),
        "the inflated rate and the ceiling it was judged against both belong in the body: {body}"
    );
}

/// A persistent fault pages ONCE an hour, not once per window.
///
/// The repeat gate is the reason this alert can exist at all — a pager that fires every fifteen
/// minutes for forty days is a pager that gets muted, which is where the whole failure started.
/// ⚠ It is also the thing a naive reuse of the silence gate breaks: the slow set is EMPTY on the
/// twenty-nine ticks between windows, and a gate that read that as a recovery would re-page on
/// every completed window forever.
#[test]
fn a_persistently_slow_series_pages_once_per_repeat_window_not_once_per_cadence_window() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    // Two hours: seven completed cadence windows against a 3,600 s default repeat.
    for i in 0..240 {
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i)), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }
    let fired = r.slow_alerts();
    assert_eq!(
        fired.len(),
        2,
        "two hours of a persistent fault should page twice (one per `repeat_secs`), not once per \
         completed window: {fired:?}"
    );
}

/// A recorder in its FIRST window says nothing — there is no rate to measure yet.
#[test]
fn a_series_in_its_first_window_gets_no_verdict() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..WINDOW_TICKS {
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i)), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(
        r.slow_alerts().is_empty(),
        "a verdict was taken before a full window closed — the rate would be measured over an \
         arbitrary fraction of one"
    );
}

// ---- 2. THE HEALTHY SERIES, at their measured distributions -------------------------------------

/// The FIXED binance depth lane, at the rate measured on the box after #1749: quiet.
#[test]
fn the_repaired_binance_depth_lane_stays_quiet_for_an_hour() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..120 {
        let d = [(DEPTH, flat(HEALTHY_DEPTH_PER_S, i)), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(r.slow_alerts().is_empty(), "a healthy lane must never page");
}

/// One hour of the binance trade tape's measured per-second SHAPE, with the depth lane modelled
/// from it — quiet.
///
/// ## What the reconstruction is, exactly
///
/// §12.3 publishes five order statistics for W1 (p50 4, p90 55, p99 233, p99.9 938, max 2,056) plus
/// 356 seconds of 3,600 carrying no trade. Five points do not determine a 3,600-point distribution,
/// so [`tape_seconds`] places each published statistic at its exact nearest-rank index and
/// interpolates between them; [`the_reconstructed_tape_reproduces_its_published_statistics`]
/// asserts every one of those anchors. So what is replayed here has the measured SHAPE — the
/// zero-seconds, the 500x spread, the burst tail — even though interpolating between the anchors
/// makes its TOTAL higher than the published 73,063.
///
/// ## …and the depth model is deliberately pessimistic
///
/// Depth is modelled as `min(ceiling, trades that second)`: the book is credited with changing ONLY
/// when a trade happened. That is a floor, not a model — most book changes are order placements and
/// cancellations, which move the top of book with no print at all — so a quiet verdict here is a
/// stronger result than the real lane needs.
#[test]
fn the_binance_tape_replayed_at_its_measured_burstiness_stays_quiet() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    let secs = tape_seconds();
    let ceiling = DEPTH_CEILING_PER_S as u64;
    for t in 0..120usize {
        let (mut tape, mut depth) = (0u64, 0u64);
        for s in 0..30usize {
            let v = secs[t * 30 + s];
            tape += v;
            depth += v.min(ceiling);
        }
        let d = [(DEPTH, depth), (TAPE, tape)];
        r.tick(&expected, &d);
    }
    assert!(
        r.slow_alerts().is_empty(),
        "the measured tape's own burstiness raised a cadence alert — the check would be muted \
         within a week"
    );
}

/// A polymarket family that ROTATES: instruments join, live ~600 s, and die. Never a verdict.
///
/// Two things could go wrong and neither may: a rotating instrument's end of life could read as a
/// collapse (26-28 false alerts an hour), and its keys could accumulate in the watch forever. The
/// first is what this asserts; the second is structural — the watch forgets any key that leaves
/// `expected`.
#[test]
fn a_rotating_polymarket_family_never_pages() {
    let mut r = Recorder::new();
    for t in 0..120usize {
        let window = t / POLY_LIFETIME_TICKS;
        let book = format!("book/polymarket/0xtok{window}");
        let trade = format!("trade/polymarket/0xtok{window}");
        let quote = format!("quote/polymarket/0xtok{window}");
        let expected = vec![book.clone(), trade.clone(), quote.clone()];
        let age = t % POLY_LIFETIME_TICKS;
        let d = [
            (book.as_str(), flat(POLY_BOOK_ALIVE_PER_S, age)),
            (trade.as_str(), flat(POLY_TRADE_PER_S, age)),
            // The derived L1 top, at the order the book's own top-of-book churn produces.
            (quote.as_str(), flat(POLY_BOOK_ALIVE_PER_S / 50.0, age)),
        ];
        r.tick(&expected, &d);
    }
    assert!(
        r.slow_alerts().is_empty(),
        "a five-minute rotation must not read as a rate collapse — 26-28 instruments an hour is \
         26-28 pages an hour"
    );
}

/// A polymarket token trading at its measured ~0.25/s — which legitimately means zero trades for
/// minutes at a time — never pages.
///
/// It cannot, and the reason is structural rather than a tuned threshold: `trade` is declared
/// `EventDriven` in `crates/vike-data/src/series_cadence.rs`'s `SERIES_CADENCE`, so its cadence
/// yields no ceiling and there is nothing to compare against. This test exists so a future edit
/// turning that row into a number has to argue with a red test first.
#[test]
fn a_polymarket_token_trading_at_a_quarter_a_second_never_pages() {
    let mut r = Recorder::new();
    let trade = "trade/polymarket/0xquiet";
    let book = "book/polymarket/0xquiet";
    let expected = keys(&[trade, book]);
    for i in 0..120 {
        let d = [(trade, flat(POLY_TRADE_PER_S, i)), (book, flat(POLY_BOOK_ALIVE_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(r.slow_alerts().is_empty(), "an event-driven lane has no floor and must never page");
}

// ---- 3. THE CASE THAT KILLS THE OBVIOUS DESIGN --------------------------------------------------

/// **The thin pair.** A binance depth lane at 0.0067 updates/s — SIXTY TIMES SLOWER than the broken
/// BTCUSDT.P lane, and completely healthy — never pages.
///
/// This is why the check is not "compare against the declared ceiling". A `*USDT.P` family glob
/// resolves hundreds of these, and an absolute floor sized to catch 0.42/s would page on every one
/// of them, forever, from the first window. The governor is what saves it: the instrument's own
/// tape is quieter than the sampler, so there is no honest expectation and no verdict is taken.
#[test]
fn the_thinnest_measured_binance_pair_never_pages() {
    let mut r = Recorder::new();
    let depth = "depth/binance/THINUSDT.P";
    let tape = "trade/binance/THINUSDT.P";
    let expected = keys(&[depth, tape]);
    for i in 0..120 {
        let d = [(depth, flat(THIN_DEPTH_PER_S, i)), (tape, flat(THIN_TAPE_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(
        r.slow_alerts().is_empty(),
        "a near-dead pair was reported as a broken lane — this is the false alarm that would have \
         made the whole check unusable"
    );

    // The premise, DEMONSTRATED rather than asserted from the same constants the check used: the
    // identical healthy thin lane, moved next to a busy tape, is exactly what a naive absolute
    // floor would page on. The governor is the only thing between the two verdicts.
    //
    // ⚠ This half found a real defect. At 0.0067/s the thin lane's FIRST row lands four ticks
    // after its tape's, which under a per-key window put the two permanently out of phase: their
    // windows never closed on the same tick, the governor's rate was never available when the lane
    // was judged, and the lane was NEVER JUDGED — silently. `SilenceWatch::slow_series` now runs
    // one window clock for the whole watch, and this is the case that proves it.
    let mut naive = Recorder::new();
    for i in 0..120 {
        let d = [(depth, flat(THIN_DEPTH_PER_S, i)), (tape, flat(TAPE_WORST_HOUR_PER_S, i))];
        naive.tick(&expected, &d);
    }
    assert!(
        !naive.slow_alerts().is_empty(),
        "this rate is supposed to sit UNDER the floor — if it no longer does, the thin-pair test \
         above proves nothing and the fraction needs re-deriving"
    );
}

/// **The phase-drift regression, isolated.** A lane whose first row lands LATER than its
/// governor's is still judged.
///
/// The window clock is shared by the whole watch precisely so this works. Under a per-key window it
/// did not, and the failure mode was the worst one available: not a wrong verdict but NO verdict,
/// forever, with nothing in any log to say the check had quietly stopped running.
#[test]
fn a_lane_whose_first_row_arrives_after_its_governors_is_still_judged() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..120 {
        // The depth lane stays completely silent for the first five ticks, then runs at the
        // measured broken rate. Its tape is busy from the first tick.
        let depth_items = if i < 5 { 0 } else { flat(BROKEN_DEPTH_PER_S, i - 5) };
        let d = [(DEPTH, depth_items), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(
        !r.slow_alerts().is_empty(),
        "a lane that started late was never judged — the window clock has gone per-key again, and \
         the check silently stops running for any series whose first row is off-phase"
    );
}

/// ...and the boundary: a tape just under the ceiling still produces no verdict, however dead the
/// depth lane looks — while one print/s over it, the SAME depth data pages.
///
/// The gate is `governor >= ceiling`, a hard edge on purpose: a soft one would need a second
/// calibrated constant, and nobody has the data to calibrate it.
#[test]
fn a_thin_pair_is_silenced_even_when_its_tape_is_just_under_the_ceiling() {
    let depth = "depth/binance/EDGEUSDT.P";
    let tape = "trade/binance/EDGEUSDT.P";
    let expected = keys(&[depth, tape]);

    let mut under = Recorder::new();
    for i in 0..60 {
        let d = [(depth, flat(BROKEN_DEPTH_PER_S, i)), (tape, flat(DEPTH_CEILING_PER_S - 0.1, i))];
        under.tick(&expected, &d);
    }
    assert!(
        under.slow_alerts().is_empty(),
        "the governor gate is `>= ceiling` and must hold at it"
    );

    let mut over = Recorder::new();
    for i in 0..60 {
        let d = [(depth, flat(BROKEN_DEPTH_PER_S, i)), (tape, flat(DEPTH_CEILING_PER_S + 1.0, i))];
        over.tick(&expected, &d);
    }
    assert!(!over.slow_alerts().is_empty(), "past the gate, the same lane must be judged");
}

/// A depth lane with NO sibling tape subscribed gets no verdict — a profile can legitimately record
/// depth alone, and guessing an expectation for it is precisely what this design refuses to do.
#[test]
fn a_depth_lane_with_no_sibling_tape_gets_no_verdict() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH]);
    for i in 0..120 {
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i))];
        r.tick(&expected, &d);
    }
    assert!(r.slow_alerts().is_empty(), "no governor, no verdict");
}

/// **The DECLARED BLIND SPOT, pinned so it stays deliberate.** If the tape dies WITH the book, the
/// governor falls with it and nothing pages here.
///
/// That is a division of labour rather than a hole: a venue outage takes both lanes down, and the
/// RECENCY watchdog owns a total death — it fires at 300 s, a third of a cadence window. What is
/// genuinely uncovered is a PROPORTIONAL partial collapse of both lanes on one symbol, which
/// nothing in this tree can currently see.
#[test]
fn both_lanes_collapsing_together_is_the_silence_watchdogs_job_not_this_ones() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..120 {
        // The tape collapses in the same proportion — 4 % of its measured rate.
        let collapsed_tape = flat(TAPE_WORST_HOUR_PER_S * 0.04, i);
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i)), (TAPE, collapsed_tape)];
        r.tick(&expected, &d);
    }
    assert!(
        r.slow_alerts().is_empty(),
        "if this ever starts firing, the design changed — update `SilenceWatch::slow_series`'s \
         declared residual, which currently says this case is uncovered"
    );
}

// ---- 4. the off switch --------------------------------------------------------------------------

/// `--silent-secs 0` silences BOTH judgements, not just the one it is named after.
///
/// An operator who switched this alarm off must get silence rather than a quieter alarm — the rule
/// `crates/vike-ops/tests/kill_switch_gate.rs` already holds for the recency half, inherited here
/// by making the rate check share its guard instead of growing a second flag.
#[test]
fn the_existing_off_switch_silences_the_rate_check_too() {
    let mut r = Recorder::new();
    let expected = keys(&[DEPTH, TAPE]);
    for i in 0..120 {
        let d = [(DEPTH, flat(BROKEN_DEPTH_PER_S, i)), (TAPE, flat(TAPE_WORST_HOUR_PER_S, i))];
        r.deliver(&d);
        let now = r.now_ms;
        watchdog_tick(&mut r.watch, &mut r.alerts, &expected, NO_FAMILIES, &r.live, now, 0);
    }
    assert!(r.slow_alerts().is_empty(), "`--silent-secs 0` must switch off the whole watchdog");
}

// ---- the reconstructed tape, and its own proof --------------------------------------------------

/// One hour of the binance BTCUSDT.P trade tape, second by second, reconstructed from §12.3's W1
/// order statistics — see [`the_binance_tape_replayed_at_its_measured_burstiness_stays_quiet`] for
/// what the reconstruction does and does not claim.
///
/// The sorted population is spread across the hour with a stride coprime to 3,600, so no window is
/// an artefact of the reconstruction's own ordering — without that the first quarter-hour would be
/// nothing but the zero-seconds and the test would prove nothing.
fn tape_seconds() -> Vec<u64> {
    let ramp = |i: usize, lo_i: usize, hi_i: usize, lo_v: u64, hi_v: u64| -> u64 {
        lo_v + (hi_v - lo_v) * (i - lo_i) as u64 / (hi_i - lo_i) as u64
    };
    let sorted: Vec<u64> = (0..3600usize)
        .map(|i| match i {
            // 356 of 3,600 seconds carry NO trade (§12.3, W1).
            0..=355 => 0,
            // …then each published order statistic at its exact nearest-rank index:
            // p50 = 4, p90 = 55, p99 = 233, p99.9 = 938, max = 2,056.
            356..=1799 => ramp(i, 356, 1799, 1, 4),
            1800..=3239 => ramp(i, 1800, 3239, 4, 55),
            3240..=3563 => ramp(i, 3240, 3563, 55, 233),
            3564..=3595 => ramp(i, 3564, 3595, 233, 938),
            _ => ramp(i, 3596, 3599, 938, 2056),
        })
        .collect();
    let mut out = vec![0u64; 3600];
    for (i, v) in sorted.into_iter().enumerate() {
        out[(i * 997) % 3600] = v;
    }
    out
}

/// The reconstruction reproduces every statistic §12.3 published for W1 — otherwise the test above
/// replays a shape nobody measured while claiming it did.
#[test]
fn the_reconstructed_tape_reproduces_its_published_statistics() {
    let mut v = tape_seconds();
    assert_eq!(v.len(), 3600);
    assert_eq!(v.iter().filter(|&&x| x == 0).count(), 356, "seconds carrying no trade");
    v.sort_unstable();
    // Nearest-rank, as `crates/vike-data/examples/store_rates.rs` computes them.
    assert_eq!(v[1799], 4, "p50");
    assert_eq!(v[3239], 55, "p90");
    assert_eq!(v[3563], 233, "p99");
    assert_eq!(v[3595], 938, "p99.9");
    assert_eq!(v[3599], 2056, "max");
    // ⚠ The TOTAL is NOT the published 73,063: interpolating between five order statistics
    // overshoots a convex distribution. It is asserted only as a bound, and it errs BUSIER, which
    // opens the governor gate more readily — the conservative direction for a false-alarm test.
    let total: u64 = v.iter().sum();
    assert!(total > 73_063, "the reconstruction is busier than the real tape: {total}");
}
