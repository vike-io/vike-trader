//! **The 2026-08-05 Polymarket collapse produces an ALERT — and a family that loses two of its
//! members every five minutes, by design, stays SILENT.**
//!
//! ## The incident
//!
//! On 2026-08-05 a Polymarket `book` family on the CI box wrote, by `local_ts` minute:
//!
//! ```text
//! 04:20 117,724 (6 syms) · 04:21 90,540 (4) · 04:22 86,894 (4) · 04:23 NO ROW · 04:24 NO ROW
//! 04:25 402 (6) · 04:26 NO ROW · 04:27 4 (4) · 04:28 NO ROW · 04:29 NO ROW · 04:30 376 (6)
//! 04:31 78,262 (4) · 04:32 50,854 · 04:33 43,348 · 04:34 30,810
//! ```
//!
//! **Five whole minutes with literally zero rows from any member, while four to six members stayed
//! subscribed throughout** — 782 rows over eight minutes against ~100,000/min. ⚠ The five dark
//! minutes are NOT consecutive (the table above is the authority: the longest fully dark run is
//! 04:23-04:24, two minutes), and prose in three files said they were until it was checked against
//! this fixture. The conclusion survives — a 120 s dark run still contains at least three fully
//! dark 30 s tiles at every phase — and the correction is on
//! `crates/vike-recorder/src/liveness.rs`'s `FAMILY_WINDOW_MS`. Nothing said anything, and nothing
//! in this tree could have:
//!
//! * the RECENCY watchdog could not — every member had produced rows inside the 300 s threshold,
//!   and the family ROTATED at least twice inside the dark span (04:25 carries six distinct
//!   symbols, 04:27 carries four), so the tokens born into the collapse had no history to be stale
//!   against;
//! * the CADENCE watchdog could not, and must not be made to —
//!   `crates/vike-data/src/series_cadence.rs` classifies this lane `Cadence::EventDriven` ("the
//!   market sets the rate, so no expectation is derivable") and `ceiling_per_s` returns `None` for
//!   such a row precisely so that no consumer can invent a number. That refusal is load-bearing;
//! * and its SAME-VENUE sibling tape, which is what licenses a cadence verdict, is provably
//!   unusable here: `kind=trade/venue=polymarket` wrote NO ROW AT ALL between 04:23 and 04:30.
//!
//! ⚠ **The number this incident is usually quoted with is wrong.** "18/min for 19 minutes" is
//! `docs/ops/onfailure-notify.md`'s replay row `2026-08-05 04:25:00  18388  1476721 …` read as a
//! per-minute figure; it is a FIVE-MINUTE bucket against a five-minute baseline, i.e. 3,678/min of
//! 295,344/min — an 80x collapse on the latency box's separate `poly-l2-recorder`, which that page's own
//! prose ("a 99.1 % data collapse") corroborates. This store's own witness of the same UTC window
//! is deeper still: 782 rows over 8 minutes against ~100,000/min, five minutes at exactly zero.
//!
//! ## What is driven
//!
//! `vike_recorder::alerts::watchdog_tick` — the real production tick, the same call the daemon
//! makes, with the same arguments. Both inputs are plain data, so this needs no store, no venue, no
//! network and no clock.
//!
//! ⚠ Every rate below is a NAMED CONSTANT carrying its source, for
//! `crates/vike-recorder/tests/cadence_alert.rs`'s reason: a magic number in a threshold test is a
//! threshold nobody can re-derive.
//!
//! ## The half that is harder than the alert
//!
//! Most of the length here is families that must stay QUIET. A Polymarket family loses two members
//! and gains two every five minutes — about 576 legitimate instrument deaths a day — and one
//! token's own rate runs 484 -> 91,143 -> 647 rows/min across its ~11-minute life (a 188x ramp up
//! and a 141x fall, measured 2026-09-10). A check that reads any of that as a fault is muted inside
//! a week, and then the next collapse is invisible again with everybody believing it is watched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_data::Liveness;
use vike_recorder::Alerting;
use vike_recorder::alerts::{FAMILY_RULE_ID, RecorderAlerts, watchdog_tick};
use vike_recorder::liveness::{
    FAMILY_FREEZE_MAX_MS, FAMILY_RING, FAMILY_WINDOW_MS, MIN_BASELINE_ITEMS, SilenceWatch,
};

// ---- the measured numbers, each with the source it came from -----------------------------------

/// `crates/vike-datahub/src/datahub_cli.rs`'s `DEFAULT_TICK_SECS`, in ms.
const TICK_MS: i64 = 30_000;
/// ...and `DEFAULT_SILENT_SECS`, the recency threshold that read green through the whole incident.
const SILENT_SECS: u64 = 300;

/// The alerting family: `{kind}/{venue}/{family}`, the key
/// `crates/vike-recorder/src/runtime.rs`'s `expected_families` builds, and the store partition an
/// operator goes and looks at (`kind=book/venue=polymarket/group=btc-updown-5m`).
const FAM: &str = "book/polymarket/btc-updown-5m";
/// The out-of-family witness. The deployed recorder profile on the CI box subscribes this beside the
/// Polymarket family, and binance wrote 348-909 trades in EVERY minute of 04:23-04:30 on
/// 2026-08-05 — which is why the licence is held OUTSIDE the family rather than on the
/// instrument's own tape.
const LICENCE_FAM: &str = "trade/binance/BTCUSDT.P";
/// The midpoint of that measured range, per minute.
const LICENCE_ROWS_PER_MIN: u64 = 435;

/// Stored parquet ROWS per ingest ITEM on `book`/polymarket, measured by
/// `uniqExact(symbol_col, local_ts, seq)` over four windows on the CI box: 2.173 / 2.403 / 2.444 on
/// healthy hours and 1.553 in the 2026-09-02 collapse hour. Every distribution in this file is
/// measured in ROWS and every constant in `liveness.rs` is stated in ITEMS, so the conversion is
/// applied here, once, and named — getting it wrong silently is the 400x error
/// `series_cadence.rs`'s own doc warns about. The 1.57x spread makes a row-ratio up to 1.57x
/// DEEPER than the item-ratio on the collapse side, which against an 18.6x healthy margin and
/// ratio-zero incident windows changes no conclusion here.
const ROWS_PER_ITEM: u64 = 24; // tenths — 2.4
/// The family's own measured healthy level, in stored rows per 30 s window: the trailing median the
/// the CI box replay of the 2026-08-05 partition carried into the first dark window.
const HEALTHY_ROWS_PER_WINDOW: u64 = 53_485;

/// How many members stayed subscribed across the dark span.
const MEMBERS: usize = 4;

/// One token's own life, rows/min, measured on the CI box 2026-09-10 across its ~11-minute existence.
/// **The number that kills per-instrument self-reference**: 188x up, then 141x down, twice every
/// five minutes, 576 times a day. An UP token and its DOWN twin produce these counts
/// byte-identically, which is a separate and free per-member detector and is NOT this rule.
const TOKEN_LIFE_ROWS_PER_MIN: [u64; 11] =
    [484, 665, 901, 1_600, 18_902, 65_898, 85_360, 91_143, 67_849, 6_700, 647];

/// The measured 2026-08-05 tape, rows/min from 04:20 to 04:34 inclusive.
const INCIDENT_ROWS_PER_MIN: [u64; 15] =
    [117_724, 90_540, 86_894, 0, 0, 402, 0, 4, 0, 0, 376, 78_262, 50_854, 43_348, 30_810];
/// Index into [`INCIDENT_ROWS_PER_MIN`] of 04:23 — the first fully dark minute.
const FIRST_DARK_MINUTE: usize = 3;

/// `trade`/polymarket's measured trailing-median occupancy per 30 s window (135-146 items), and its
/// measured 2026-09-09 minimum window (2 items, ratio 0.01444). The lane whose `SERIES_CADENCE`
/// row says in terms that "no floor above zero is safe on this row, and declaring one is precisely
/// how a pager gets muted".
const THIN_BASELINE_ITEMS: u64 = 138;
const THIN_MINIMUM_ITEMS: u64 = 2;

/// Status markers per HOUR across a healthy `book`/polymarket family — 48 `GapStart` + 0 `Stale` +
/// 24 `LiveResume` over 28 symbols in one measured hour (2026-09-10 12:00Z), against 3,569,212
/// items. **0.002 % of the family total**, which is what answers `CADENCE_FLOOR_FRACTION`'s
/// marker-inflation concern at this grain: a fully dead family still emitting only markers sits
/// 250x BELOW the floor rather than being lifted over it.
const MARKERS_PER_HOUR: u64 = 72;

/// The `depth`/binance lane the deployed profile records beside the family — the witness that
/// matters for a host-wide stall, because it is one of the only two lanes
/// `crates/vike-data/src/live_rec.rs`'s `stream_status` writes markers into at all (`book` and
/// `depth`; every other stream label returns early, so a `trade` tape emits none).
const DEPTH_FAM: &str = "depth/binance/BTCUSDT.P";
/// Its measured trailing-median occupancy per 30 s window, from `MIN_BASELINE_ITEMS`' own doc
/// (~300 items — never admitted as a SUBJECT, and a perfectly good WITNESS).
const DEPTH_ITEMS_PER_WINDOW: u64 = 300;
/// …and what it writes with nothing inbound at all: one `GapStart` per subscribed symbol, emitted
/// BECAUSE the data stopped. The number the old `licence_items > 0` predicate could not tell from
/// health.
const DEPTH_MARKERS_IN_A_STALL: u64 = 1;

// ---- the harness -------------------------------------------------------------------------------

/// A sink that records what was delivered. Blind to `AlertTargets`, like its two sibling files':
/// the recorder's rules set `in_process: false` because a headless daemon has no toast surface.
#[derive(Clone, Default)]
struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

/// One simulated recorder: the real watch, the real alert mount, and an arrival record advanced
/// tick by tick exactly as `crates/vike-data/src/live_rec.rs`'s `ingest` advances the real one —
/// `rows` monotonic per key, never reset, never removed.
struct Recorder {
    watch: SilenceWatch,
    alerts: RecorderAlerts,
    pager: Pager,
    live: HashMap<String, Liveness>,
    tick_ms: i64,
    ticks: i64,
}

impl Recorder {
    fn new() -> Self {
        Self::with_tick(TICK_MS)
    }

    fn with_tick(tick_ms: i64) -> Self {
        let pager = Pager::default();
        let mount = RecorderAlerts::mount(&Alerting::default(), Vec::new());
        let alerts = mount.with_sink(Box::new(pager.clone()));
        Recorder {
            watch: SilenceWatch::new(),
            alerts,
            pager,
            live: HashMap::new(),
            tick_ms,
            ticks: 0,
        }
    }

    /// Advance one tick: deliver, then run the real production watchdog over the result.
    ///
    /// `families` is `RecorderRuntime::expected_families()`'s shape; `expected` is derived from it,
    /// exactly as the daemon derives both from the same subscription set.
    fn tick(&mut self, families: &[(String, String)], deliveries: &[(&str, u64)]) {
        self.ticks += 1;
        let now = self.ticks * self.tick_ms;
        for (key, items) in deliveries {
            let e = self.live.entry((*key).to_string()).or_insert(Liveness { rows: 0, last_ms: 0 });
            if *items > 0 {
                e.rows += items;
                e.last_ms = now;
            }
        }
        let expected: Vec<String> = families.iter().map(|(k, _)| k.clone()).collect();
        watchdog_tick(
            &mut self.watch,
            &mut self.alerts,
            &expected,
            families,
            &self.live,
            now,
            SILENT_SECS,
        );
    }

    /// Only the FAMILY alerts. The other three rules are different faults with different ids, and a
    /// test counting all four could pass for the wrong reason.
    fn family_alerts(&self) -> Vec<FiredAlert> {
        self.pager
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.rule_id == FAMILY_RULE_ID)
            .cloned()
            .collect()
    }
}

/// `{kind}/{venue}/{symbol}` -> `{kind}/{venue}/{family}` pairs, as `expected_families` returns.
fn members(family: &str, count: usize) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        (0..count).map(|i| (format!("{family}/tok{i}"), family.to_string())).collect();
    // The licence: a series OUTSIDE the judged family, subscribed by SYMBOL rather than by family,
    // so `expected_families` maps it to its OWN series key — the degenerate one-member family, with
    // no special case anywhere in the rule.
    v.push((LICENCE_FAM.to_string(), LICENCE_FAM.to_string()));
    v
}

/// Rows -> ingest items at the measured conversion.
fn items(rows: u64) -> u64 {
    rows * 10 / ROWS_PER_ITEM
}

/// Split `total` items across `count` members, losing nothing to rounding — the family total is
/// what the rule judges, so the split must be exact.
fn share(total: u64, count: usize) -> Vec<u64> {
    let base = total / count as u64;
    let mut out = vec![base; count];
    out[0] += total - base * count as u64;
    out
}

/// One tick's deliveries for a family running at `rows` stored rows in this window, plus the
/// licence tape.
fn window(family: &str, count: usize, rows: u64, licence_items: u64) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = share(items(rows), count)
        .into_iter()
        .enumerate()
        .map(|(i, n)| (format!("{family}/tok{i}"), n))
        .collect();
    out.push((LICENCE_FAM.to_string(), licence_items));
    out
}

fn as_refs(v: &[(String, u64)]) -> Vec<(&str, u64)> {
    v.iter().map(|(k, n)| (k.as_str(), *n)).collect()
}

/// The licence tape's items in one 30 s window.
fn licence_per_window() -> u64 {
    LICENCE_ROWS_PER_MIN / 2
}

/// Prime the ring to FULL at the family's measured healthy level, so the next window is judgeable.
///
/// `FAMILY_RING + 1` ticks, not `FAMILY_RING`: the first tick OPENS the window and closes nothing,
/// exactly as the production clock does.
fn prime(r: &mut Recorder, fam: &[(String, String)], family: &str, count: usize, rows: u64) {
    for _ in 0..FAMILY_RING + 1 {
        let d = window(family, count, rows, licence_per_window());
        r.tick(fam, &as_refs(&d));
    }
}

// ---- 1. THE DEFECT -----------------------------------------------------------------------------

/// **THE ONE THAT WOULD HAVE CAUGHT 2026-08-05.** The measured tape, replayed, fires on the FIRST
/// fully dark window — with zero observed against a five-figure baseline, i.e. infinite margin.
///
/// The the CI box query behind [`INCIDENT_ROWS_PER_MIN`], run read-only:
///
/// ```text
/// clickhouse-local --query "SELECT toStartOfMinute(local_ts) m, count(), uniqExact(symbol_col)
///   FROM file('/var/lib/vike/data/hist/kind=book/venue=polymarket/
///             group=btc-updown-5m/date=2026-08-05/*.parquet')
///   WHERE m BETWEEN '2026-08-05 04:20:00' AND '2026-08-05 04:34:00' GROUP BY m ORDER BY m"
/// ```
#[test]
fn the_2026_08_05_family_collapse_fires_on_the_first_closed_window() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);
    let first_measured_tick = r.ticks;

    // Each measured MINUTE is two 30 s windows. The split is even, which the store's per-minute
    // granularity cannot contradict — and the property the verdict rests on survives any split at
    // any phase. ⚠ That property is NOT "five consecutive fully dark minutes": the five dark
    // minutes in `INCIDENT_ROWS_PER_MIN` are 04:23, 04:24, 04:26, 04:28 and 04:29, and the longest
    // consecutive run is TWO. A 120 s dark run still contains at least three fully dark 30 s tiles
    // at every phase, which is what this test then measures. That is the whole reason the window is
    // 30 s: a 300 s tile at the wrong phase reads 0.175 and 0.157 of baseline here and MISSES this
    // incident entirely.
    for rows in INCIDENT_ROWS_PER_MIN {
        for half in 0..2u64 {
            let this = rows / 2 + if half == 0 { rows % 2 } else { 0 };
            let d = window(FAM, MEMBERS, this, licence_per_window());
            r.tick(&fam, &as_refs(&d));
        }
    }

    let fired = r.family_alerts();
    assert!(!fired.is_empty(), "the 2026-08-05 collapse must produce an alert");

    // The first verdict lands on the window carrying 04:23's first half — one window, ~30 s, after
    // the last healthy one, so detection latency is <= 90 s against 19 minutes of loss.
    let want_tick = first_measured_tick + (FIRST_DARK_MINUTE as i64) * 2 + 1;
    assert_eq!(
        fired[0].ts_ms,
        want_tick * TICK_MS,
        "the first verdict must land on the first fully dark window (04:23), not later"
    );

    let body = &fired[0].body;
    assert!(body.contains(FAM), "the alert must NAME the family — which one is the diagnosis");
    assert!(
        body.contains("produced 0 items"),
        "the observed count is ZERO and the body must say so: {body}"
    );
    assert!(
        body.contains(&format!("{MEMBERS} member")),
        "the member count is what separates a whole family from one rotated-out token: {body}"
    );
    assert!(
        body.contains(&format!("last {FAMILY_RING} windows")),
        "the baseline's provenance belongs in the body — it is LEARNED, not declared: {body}"
    );
    assert!(
        body.contains(LICENCE_FAM),
        "the out-of-family licence is the rest of the diagnosis: without it a dead family and a \
         stalled process read the same: {body}"
    );
    // The baseline is a five-figure count, so the ratio is zero against something real rather than
    // zero against nothing.
    let baseline = items(HEALTHY_ROWS_PER_WINDOW);
    assert!(baseline > MIN_BASELINE_ITEMS, "the priming level must clear the resolution gate");
}

/// **The alert is delivered and is ABSENT from the tick's return value.**
///
/// That return value drives `--exit-on-silence`: a rate verdict must never be able to stop a live
/// recorder, because degrading a tape is bad and turning it into NO tape is worse. Same trade the
/// cadence rule makes, pinned here because it is a one-character change to break it.
#[test]
fn a_family_verdict_is_delivered_and_never_drives_exit_on_silence() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    // One fully dark window, with every member still subscribed and still inside the 300 s recency
    // threshold — so the RECENCY rule has nothing to say and this is the family rule alone.
    r.ticks += 1;
    let now = r.ticks * r.tick_ms;
    let expected: Vec<String> = fam.iter().map(|(k, _)| k.clone()).collect();
    // The licence keeps writing; the family does not.
    let e = r.live.entry(LICENCE_FAM.to_string()).or_insert(Liveness { rows: 0, last_ms: 0 });
    e.rows += licence_per_window();
    e.last_ms = now;
    let silent =
        watchdog_tick(&mut r.watch, &mut r.alerts, &expected, &fam, &r.live, now, SILENT_SECS);

    assert_eq!(r.family_alerts().len(), 1, "the collapse was DELIVERED");
    assert!(
        silent.is_empty(),
        "a family verdict must not reach the value that drives --exit-on-silence: {silent:?}"
    );
}

/// A verdict is unchanged for a family whose `(kind, venue)` has NO `SERIES_CADENCE` row at all.
///
/// **The §7 requirement, as a unit test.** This rule derives nothing from that table — it must not,
/// because the table's `ceiling_per_s` returning `None` for an event-driven row is the one gate
/// between the cadence check and an invented threshold. `crates/vike-ops/tests/` carries the text
/// half (the family path may not NAME `cadence_for_series_key` or `ceiling_per_s`); this is the
/// behavioural half.
#[test]
fn a_family_with_no_cadence_row_is_judged_exactly_the_same() {
    const STRANGER: &str = "book/kraken/some-family";
    let mut r = Recorder::new();
    let fam = members(STRANGER, MEMBERS);
    prime(&mut r, &fam, STRANGER, MEMBERS, HEALTHY_ROWS_PER_WINDOW);
    let d = window(STRANGER, MEMBERS, 0, licence_per_window());
    r.tick(&fam, &as_refs(&d));

    let fired = r.family_alerts();
    assert_eq!(fired.len(), 1, "a family with no cadence row is judged the same: {fired:?}");
    assert!(fired[0].body.contains(STRANGER));
}

// ---- 2. THE ROTATION, WHICH IS THE HARD HALF ---------------------------------------------------

/// **A full hour of the real rotation never fires** — 12 rotations, 24 legitimate instrument
/// deaths, zero verdicts.
///
/// This is the property that lets the rule CLOSE the gap rather than exempt it. It never asks
/// whether a member died, because the subject is the family key: a dying member's tail and its
/// successor's birth land in ONE total. No grace window to tune, no end-of-life seam to plumb
/// through `VenueFeed`, no exemption anybody has to maintain.
///
/// Each member follows [`TOKEN_LIFE_ROWS_PER_MIN`] — the real measured life, a 188x ramp up and a
/// 141x fall — and a new PAIR is born every five minutes while the previous pairs are still alive,
/// which is the deployed profile's actual shape (26-28 instruments alive in a one-hour window).
#[test]
fn a_full_hour_of_rotation_never_fires() {
    let mut r = Recorder::new();
    // The membership at minute `m`: every cohort born at `b` with `m - b` inside a token's life.
    const COHORT_EVERY_MIN: usize = 5;
    const PAIR: usize = 2;
    let life = TOKEN_LIFE_ROWS_PER_MIN.len();

    let alive = |minute: usize| -> Vec<(usize, usize)> {
        (0..=minute)
            .filter(|b| b % COHORT_EVERY_MIN == 0 && minute - b < life)
            .map(|b| (b, minute - b))
            .collect()
    };

    let mut deaths = 0usize;
    let mut prev_live = 0usize;
    for minute in 0..60usize {
        let cohorts = alive(minute);
        // The subscription set as the runtime would report it this minute.
        let mut fam: Vec<(String, String)> = cohorts
            .iter()
            .flat_map(|(b, _)| (0..PAIR).map(move |i| (format!("{FAM}/c{b}t{i}"), FAM.to_string())))
            .collect();
        fam.push((LICENCE_FAM.to_string(), LICENCE_FAM.to_string()));
        let now_live = cohorts.len() * PAIR;
        deaths += prev_live.saturating_sub(now_live.min(prev_live));
        prev_live = now_live;

        for half in 0..2 {
            let mut d: Vec<(String, u64)> = cohorts
                .iter()
                .flat_map(|(b, age)| {
                    let per_min = TOKEN_LIFE_ROWS_PER_MIN[*age];
                    let this = per_min / 2 + if half == 0 { per_min % 2 } else { 0 };
                    (0..PAIR).map(move |i| (format!("{FAM}/c{b}t{i}"), items(this)))
                })
                .collect();
            d.push((LICENCE_FAM.to_string(), licence_per_window()));
            r.tick(&fam, &as_refs(&d));
        }
    }

    assert!(deaths >= 18, "the fixture must actually rotate — {deaths} deaths in an hour");
    let fired = r.family_alerts();
    assert!(
        fired.is_empty(),
        "a family that loses two members every five minutes, by design, must never page: {fired:?}"
    );
}

/// **A single member going quiet is NOT claimed** — a declared residual, pinned so nobody later
/// "improves" it into a flake.
///
/// One member of a healthy four is ~25 % of the family total, so its death moves the ratio to
/// ~0.75 and is invisible to this rule. That is the honest other half of never asking the question:
/// because it cannot ask, it also cannot answer. The free detector for it is the MIRROR-PAIR
/// identity (an UP token and its DOWN twin produce byte-identical minute counts, verified on
/// 2026-09-10), which needs no rate model at all and belongs to its own change.
#[test]
fn a_single_member_death_is_not_claimed() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    for _ in 0..FAMILY_RING {
        let mut d = window(FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW, licence_per_window());
        // tok3 dies; the other three carry on at their own rate.
        let dead = format!("{FAM}/tok3");
        for entry in d.iter_mut() {
            if entry.0 == dead {
                entry.1 = 0;
            }
        }
        r.tick(&fam, &as_refs(&d));
    }
    assert!(
        r.family_alerts().is_empty(),
        "one member of four is ~25 % of the total — this rule cannot and must not claim it"
    );
}

// ---- 3. THE GUARDS THAT KEEP IT OFF THE THIN LANES ---------------------------------------------

/// **The regression test for the fatal an earlier draft carried.** `trade`/polymarket runs at a
/// trailing median of 135-146 items per 30 s window, and its measured 2026-09-09 minimum window is
/// 2 items — a ratio of 0.01444, which FIRES at a 1/50 floor. It must never be judged at all.
///
/// [`MIN_BASELINE_ITEMS`] is what refuses it, and it is a RESOLUTION gate rather than a venue
/// classifier: at a 138-item baseline the alarm level is well under one item, so the rule could
/// only ever fire on ZERO — and zero is an ordinary quiet window on a prediction-market tape. That
/// series' own `SERIES_CADENCE` row says the same thing in its own words.
#[test]
fn a_thin_family_is_never_judged() {
    const THIN: &str = "trade/polymarket/btc-updown-5m";
    let mut r = Recorder::new();
    let fam = members(THIN, MEMBERS);

    for _ in 0..FAMILY_RING + 1 {
        let mut d: Vec<(String, u64)> = share(THIN_BASELINE_ITEMS, MEMBERS)
            .into_iter()
            .enumerate()
            .map(|(i, n)| (format!("{THIN}/tok{i}"), n))
            .collect();
        d.push((LICENCE_FAM.to_string(), licence_per_window()));
        r.tick(&fam, &as_refs(&d));
    }
    // The real measured minimum window, and then a fully dark one for good measure.
    for n in [THIN_MINIMUM_ITEMS, 0] {
        let mut d: Vec<(String, u64)> = share(n, MEMBERS)
            .into_iter()
            .enumerate()
            .map(|(i, x)| (format!("{THIN}/tok{i}"), x))
            .collect();
        d.push((LICENCE_FAM.to_string(), licence_per_window()));
        r.tick(&fam, &as_refs(&d));
    }

    // ⚠ Bound to locals rather than compared as two constants: `clippy::assertions_on_constants`
    // rejects the direct form, and the assertion is the NON-VACUITY of this whole test — a fixture
    // that drifted above the gate would make the emptiness below prove nothing.
    let (fixture, gate) = (THIN_BASELINE_ITEMS, MIN_BASELINE_ITEMS);
    assert!(fixture < gate, "the fixture ({fixture}) must sit under the resolution gate ({gate})");
    assert!(
        r.family_alerts().is_empty(),
        "the lane whose cadence row refuses a floor must be refused a verdict too"
    );
}

/// A cold ring takes NO verdict — mirroring `Cadence::ceiling_per_s` returning `None`. No history,
/// no number, nothing to threshold, and a page from a baseline nobody has is worse than no page.
#[test]
fn a_cold_ring_takes_no_verdict() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    // One window short of full, then a fully dark one.
    for _ in 0..FAMILY_RING {
        let d = window(FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW, licence_per_window());
        r.tick(&fam, &as_refs(&d));
    }
    let d = window(FAM, MEMBERS, 0, licence_per_window());
    r.tick(&fam, &as_refs(&d));
    assert!(
        r.family_alerts().is_empty(),
        "a verdict was taken before the ring was full — the baseline would be a guess"
    );
}

/// **An UNLICENSED window takes no verdict** — the declared blind spot, narrowed and pinned.
///
/// When everything this process records goes quiet in the same window, no verdict is taken:
/// deliberately, because that is a whole-process stall (a documented the CI box failure mode) and the
/// RECENCY watchdog owns it and fires first. The licence moves the proof from the instrument's own
/// sibling tape — provably unusable here, `trade`/polymarket wrote nothing at all 04:23-04:30 — to
/// any series outside the family, which binance satisfied in every minute of every measured
/// incident. It narrows the blind spot from "one instrument's siblings" to "everything this process
/// records". It does not remove it.
#[test]
fn an_unlicensed_window_takes_no_verdict() {
    const OTHER: &str = "book/otherven/btc-updown-5m";
    let mut r = Recorder::new();
    let mut fam = members(FAM, MEMBERS);
    fam.retain(|(k, _)| k.as_str() != LICENCE_FAM);
    fam.extend((0..MEMBERS).map(|i| (format!("{OTHER}/tok{i}"), OTHER.to_string())));

    let both = |rows: u64| -> Vec<(String, u64)> {
        let mut d: Vec<(String, u64)> = share(items(rows), MEMBERS)
            .into_iter()
            .enumerate()
            .map(|(i, n)| (format!("{FAM}/tok{i}"), n))
            .collect();
        d.extend(
            share(items(rows), MEMBERS)
                .into_iter()
                .enumerate()
                .map(|(i, n)| (format!("{OTHER}/tok{i}"), n)),
        );
        d
    };

    for _ in 0..FAMILY_RING + 1 {
        let d = both(HEALTHY_ROWS_PER_WINDOW);
        r.tick(&fam, &as_refs(&d));
    }
    // BOTH families dark in the same window: nothing outside either one produced anything.
    for _ in 0..3 {
        let d = both(0);
        r.tick(&fam, &as_refs(&d));
    }
    assert!(
        r.family_alerts().is_empty(),
        "a whole-process stall is the recency watchdog's fault and this rule must withhold"
    );
}

/// **Marker inflation cannot lift a dead family over the floor**, measured at THIS grain for the
/// first time.
///
/// `crates/vike-data/src/live_rec.rs`'s `stream_status` routes `GapStart`/`Stale`/`LiveResume` into
/// the book lane as ordinary items that bump `Liveness::rows`, so a family that is entirely broken
/// still emits something. At instrument grain that is a coin flip by construction (72 markers/hour
/// over four tokens is one per ~200 s against the 300 s `--silent-secs` default, which is why the
/// recency watchdog is unreliable on this lane). At FAMILY grain it is 0.002 % of the total — about
/// 0.6 items per 30 s window against a five-figure baseline — so a dead family still reads 250x
/// below the floor and fires.
///
/// This rule therefore does NOT depend on the marker-exclusion fix, which is a real standalone
/// defect and belongs to its own change.
#[test]
fn the_marker_floor_cannot_lift_a_dead_family_over_the_floor() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    // 72 markers/hour = 0.6 per 30 s window: one marker every other window, from a different
    // member each time, which is the worst case for a rule that looked at members separately.
    let windows_per_hour = (3_600_000 / FAMILY_WINDOW_MS) as u64;
    let markers = MARKERS_PER_HOUR;
    assert!(
        markers < windows_per_hour,
        "the measured marker rate ({markers}/hour) must be under one per window \
         ({windows_per_hour} windows/hour) or this fixture is not the measured shape"
    );
    for i in 0..10usize {
        let mut d: Vec<(String, u64)> =
            (0..MEMBERS).map(|m| (format!("{FAM}/tok{m}"), 0u64)).collect();
        if i % 2 == 0 {
            d[i % MEMBERS].1 = 1;
        }
        d.push((LICENCE_FAM.to_string(), licence_per_window()));
        r.tick(&fam, &as_refs(&d));
    }
    assert!(
        !r.family_alerts().is_empty(),
        "a family emitting ONLY status markers is broken and must still page"
    );
}

// ---- 4. THE FREEZE, AND ITS EXIT ---------------------------------------------------------------

/// **The freeze, proven necessary rather than argued.** Replay the 2026-09-02 shape — a 3 h 24 m
/// collapse at ~1 % of baseline — and assert the rule is STILL firing in the final hour.
///
/// Without the freeze a naive trailing median sags from ~50,000 toward single digits inside six
/// minutes and the alarm silences itself while the outage continues. That is the difference this
/// test is measuring: the same fixture, with a learning ring, goes quiet after about ten windows.
///
/// ⚠ The gate is the RATIO, not whether the window FIRED — a collapse trickling at 1-10 % never
/// trips the floor and would otherwise teach its own baseline down into silence.
#[test]
fn a_collapsed_window_does_not_teach_the_baseline() {
    /// 2026-09-02's measured outage, 01:31:30 -> 04:55:30 UTC.
    const OUTAGE_SECS: i64 = 12_240;
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    let windows = OUTAGE_SECS / (FAMILY_WINDOW_MS / 1_000);
    // 0.1 % of baseline — under the floor, so it fires, and far under the learn gate, so a naive
    // trailing median would follow it down. A 1 % trickle would be ABOVE the 1/200 floor and is a
    // declared residual of this rule, not something it may claim.
    let trickle = HEALTHY_ROWS_PER_WINDOW / 1_000;
    let before = r.family_alerts().len();
    let mut last_hour_start = 0usize;
    for w in 0..windows {
        let d = window(FAM, MEMBERS, trickle, licence_per_window());
        r.tick(&fam, &as_refs(&d));
        if w == windows - 120 {
            last_hour_start = r.family_alerts().len();
        }
    }
    let fired = r.family_alerts();
    assert!(fired.len() > before, "the outage must page at all");
    assert!(
        fired.len() > last_hour_start,
        "the rule went quiet before the outage ended — the baseline sagged into silence, which is \
         exactly the failure the freeze exists to prevent"
    );
}

/// **The freeze's EXIT.** A permanent LEGITIMATE regime shift must self-heal rather than page
/// forever. Held below the learn gate for longer than [`FAMILY_FREEZE_MAX_MS`], the ring is
/// discarded, relearned from the new regime, and the rule goes quiet.
///
/// Six hours is 1.5x the longest collapse in the store (2026-08-26, 3 h 54 m), so every measured
/// incident is covered end to end and a genuine regime shift costs about six pages.
///
/// ⚠ The relearned regime is necessarily BELOW the resolution gate, and that is not an accident of
/// the fixture: any shift deep enough to keep firing is by definition more than 200x down, so its
/// new baseline cannot clear [`MIN_BASELINE_ITEMS`]. The family becomes a thin lane and is
/// UNJUDGED from then on, which is the right answer — it is a thin lane now.
#[test]
fn a_frozen_ring_relearns_after_six_hours() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    let windows = FAMILY_FREEZE_MAX_MS / FAMILY_WINDOW_MS;
    let shifted = HEALTHY_ROWS_PER_WINDOW / 1_000;
    for _ in 0..windows + 2 {
        let d = window(FAM, MEMBERS, shifted, licence_per_window());
        r.tick(&fam, &as_refs(&d));
    }
    let after_timeout = r.family_alerts().len();
    assert!(after_timeout > 1, "the six frozen hours must have paged hourly: {after_timeout}");

    // …and now four further hours in the NEW regime say nothing at all. A ring that stayed frozen
    // would have paged four more times, which is what makes this an observation rather than a
    // tautology.
    for _ in 0..480 {
        let d = window(FAM, MEMBERS, shifted, licence_per_window());
        r.tick(&fam, &as_refs(&d));
    }
    assert_eq!(
        r.family_alerts().len(),
        after_timeout,
        "a ring frozen past its timeout must relearn and go quiet, not page forever"
    );
}

/// **An episode is NOT closed by a merely non-firing window** — the flapping re-page guard.
///
/// `crates/vike-recorder/src/liveness.rs`'s own doc warns that "a flapping governor can outrun the
/// repeat gate": if a non-firing window closed the episode, the family would drop out of the gate's
/// episode set, forget its last alert, and page again on the very next firing window — every 60 s
/// at a sagged baseline. That is the cry-wolf failure that gets a pager muted, arriving through the
/// gate itself. An episode closes only on a window that TEACHES.
#[test]
fn an_episode_survives_a_non_firing_window() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    // Two hours of alternating: fully dark (fires) / 10 % of baseline (neither fires nor teaches).
    for i in 0..240 {
        let rows = if i % 2 == 0 { 0 } else { HEALTHY_ROWS_PER_WINDOW / 10 };
        let d = window(FAM, MEMBERS, rows, licence_per_window());
        r.tick(&fam, &as_refs(&d));
    }
    let fired = r.family_alerts();
    assert_eq!(
        fired.len(),
        2,
        "two hours of a flapping fault must page twice (one per `repeat_secs`), not once per \
         firing window: {} pages",
        fired.len()
    );
}

// ---- 5. WHAT IT DELIBERATELY DOES NOT CLAIM ----------------------------------------------------

/// **A PARTIAL collapse is not claimed** — pinned so nobody later "improves" a declared residual
/// into a flake.
///
/// This is a catastrophic-collapse detector and nothing else. Measured on the motivating incident
/// itself: 2026-08-05's recovery phase ran 78,262 / 50,854 / 43,348 / 30,810 rows/min against a
/// ~100,000/min baseline — 20-50 % of normal, roughly another million rows shed — and never fires.
/// A 2x, a 5x and the 24x binance depth collapse are all invisible to it. `SERIES_CADENCE`'s
/// DECLARED ceiling is what catches that class, and these two are COMPLEMENTS rather than
/// substitutes.
#[test]
fn a_partial_collapse_is_not_claimed() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    // The measured recovery phase, per minute, against a ~100,000/min family.
    for rows_per_min in [78_262u64, 50_854, 43_348, 30_810] {
        for _ in 0..2 {
            let d = window(FAM, MEMBERS, rows_per_min / 2, licence_per_window());
            r.tick(&fam, &as_refs(&d));
        }
    }
    assert!(
        r.family_alerts().is_empty(),
        "a 2-5x shortfall is a DECLARED residual of this rule, not something it may claim"
    );
}

/// **The window is TIME-defined, not tick-counted.** The identical arrival series driven at a 5 s
/// tick and at the default 30 s tick produces the identical verdict set, because the window closes
/// on the first tick whose span reaches [`FAMILY_WINDOW_MS`] — exactly as `slow_series`' does.
///
/// A tick-counted ring would make "20 windows = 10 minutes" true only at the default, and
/// `--tick-secs` is an operator knob.
#[test]
fn the_window_is_time_defined_not_tick_counted() {
    fn run(tick_ms: i64) -> usize {
        let mut r = Recorder::with_tick(tick_ms);
        let fam = members(FAM, MEMBERS);
        let per_window = FAMILY_WINDOW_MS / tick_ms;
        // The same ARRIVALS in wall-clock terms, split across however many ticks a window holds.
        for _ in 0..(FAMILY_RING as i64 + 1) * per_window {
            let d = window(
                FAM,
                MEMBERS,
                HEALTHY_ROWS_PER_WINDOW / per_window as u64,
                licence_per_window() / per_window as u64,
            );
            r.tick(&fam, &as_refs(&d));
        }
        // TWO windows' worth of dark ticks, not one: a window boundary need not coincide with the
        // start of the dark phase, so the first closing window can be a mixed one at a short tick.
        for _ in 0..2 * per_window {
            let d = window(FAM, MEMBERS, 0, licence_per_window() / per_window as u64);
            r.tick(&fam, &as_refs(&d));
        }
        r.family_alerts().len()
    }
    assert_eq!(run(5_000), run(TICK_MS), "the verdict set must not depend on the tick");
    assert_eq!(run(TICK_MS), 1, "…and it must be a real verdict, not a shared zero");
}

// ---- 6. THE LIFECYCLE: WHEN THE CHECK MAY GO QUIET, AND WHEN IT MAY NOT -------------------------

/// **A flapping collapse pages HOURLY however high its recovery windows reach.**
///
/// [`an_episode_survives_a_non_firing_window`] pins the 0.5-25 % band and cannot reach the rest:
/// its recovery windows are `HEALTHY_ROWS_PER_WINDOW / 10`, deliberately below the learn gate. The
/// band ABOVE that gate was the hole. An episode is closed by any window that TEACHES — anything
/// from a quarter of baseline upward, which is a 4x shortfall and not a recovery — and closing it
/// re-armed the repeat gate, so the next dark window paged at once. The measured trace at the
/// deployed tick: dark -> page; 40 % -> episode closed, timestamp forgotten; dark -> page. **60
/// pages an hour against a `repeat_secs` of 3600**, and it sustained, because only the teaching
/// windows enter the ring, so the baseline settles at the recovery level and the dark windows go on
/// firing forever.
///
/// A socket that reconnects, delivers a full book re-snapshot and dies again on a ~30 s cycle — the
/// Polymarket CLOB instability this whole rule exists for — is exactly that shape. Both ends of the
/// band are driven here: a 40 % recovery (which is not one) and a FULL one (which is).
#[test]
fn a_flapping_collapse_pages_hourly_however_high_its_recovery_windows_reach() {
    fn two_hours_of(recovery_rows: u64) -> usize {
        let mut r = Recorder::new();
        let fam = members(FAM, MEMBERS);
        prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);
        // 240 windows x 30 s = two hours, exactly as `an_episode_survives_a_non_firing_window`.
        for i in 0..240 {
            let rows = if i % 2 == 0 { 0 } else { recovery_rows };
            let d = window(FAM, MEMBERS, rows, licence_per_window());
            r.tick(&fam, &as_refs(&d));
        }
        r.family_alerts().len()
    }

    // 40 % of baseline: above the learn gate, so it TEACHES and closes the episode, and still a
    // 2.5x shortfall. The ring converges on this level and the dark windows go on firing.
    assert_eq!(
        two_hours_of(HEALTHY_ROWS_PER_WINDOW * 2 / 5),
        2,
        "a fault flapping ABOVE the learn gate must page once per `repeat_secs`, not once per \
         dark window"
    );
    // …and a FULL recovery between each dark window, which is the same shape with the strongest
    // possible case for calling each collapse a new episode. It is still one family and still one
    // page an hour; `repeat_secs` means the same thing here as for every other subject in the file.
    assert_eq!(
        two_hours_of(HEALTHY_ROWS_PER_WINDOW),
        2,
        "a full-recovery flap is still one subject and still one page per `repeat_secs`"
    );
}

/// **A family that blinks out of the profile for one tick is judged on its return** — and the blink
/// itself is not a fault this rule may claim.
///
/// `crates/vike-recorder/src/runtime.rs`'s `tick` documents an `Ok(empty)` resolve as a real
/// failure ("a family that resolves to zero symbols is the quieter of the two failures this field
/// exists for"), and unlike an `Err` it does not `continue`: `crates/vike-recorder/src/session.rs`'s
/// `reconcile` unsubscribes every member and `expected_families` yields no pair for that family.
/// One such tick used to discard the ring, the freeze clock and the open episode, leaving the
/// family unjudged for the next ten minutes with nothing in the log to say so — a silent blinding,
/// which is the class this whole effort exists to remove.
#[test]
fn a_family_that_blinks_out_of_the_profile_is_judged_on_its_return() {
    let mut r = Recorder::new();
    let fam = members(FAM, MEMBERS);
    prime(&mut r, &fam, FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW);

    // ONE tick in which the venue's market list came back EMPTY.
    let only_licence: Vec<(String, String)> =
        vec![(LICENCE_FAM.to_string(), LICENCE_FAM.to_string())];
    r.tick(&only_licence, &[(LICENCE_FAM, licence_per_window())]);
    assert!(
        r.family_alerts().is_empty(),
        "a family with no members has no total, so the blink itself is not a verdict: {:?}",
        r.family_alerts()
    );

    // …and it is back on the very next tick, dark. Ten minutes of blindness would show up here as
    // silence.
    let d = window(FAM, MEMBERS, 0, licence_per_window());
    r.tick(&fam, &as_refs(&d));
    assert_eq!(
        r.family_alerts().len(),
        1,
        "a one-tick empty resolve must not blind this rule for a ring length"
    );
}

/// **A host-wide stall seen only through DISCONNECT MARKERS takes no verdict** — and the same dark
/// family pages the moment a witness is genuinely alive.
///
/// ⚠ [`an_unlicensed_window_takes_no_verdict`] drives every other family to EXACTLY zero, and the
/// marker path makes that the unlikely case rather than the normal one:
/// `crates/vike-data/src/live_rec.rs`'s `stream_status` writes `GapStart`/`Stale`/`LiveResume` into
/// the `book` and `depth` lanes as ordinary rows that bump `Liveness::rows`, and those are emitted
/// BECAUSE data stopped. `crates/bridges/binance/src/family/market_feed.rs`'s
/// `health_to_stream_status` is the producer. So in the first window of a whole-host network loss
/// the depth lane writes at least one `GapStart` per symbol while receiving nothing at all, which
/// used to satisfy a licence spelled `licence_items > 0` — and the alert body then asserted "this
/// recorder was still receiving data" on the strength of a disconnect.
///
/// This is the SILENCE half of that fix, driven on NON-ZERO input, which is the only kind that
/// proves anything here.
#[test]
fn a_host_wide_stall_seen_only_through_markers_takes_no_verdict() {
    let mut fam = members(FAM, MEMBERS);
    fam.push((DEPTH_FAM.to_string(), DEPTH_FAM.to_string()));
    let mut r = Recorder::new();
    for _ in 0..FAMILY_RING + 1 {
        let mut d = window(FAM, MEMBERS, HEALTHY_ROWS_PER_WINDOW, licence_per_window());
        d.push((DEPTH_FAM.to_string(), DEPTH_ITEMS_PER_WINDOW));
        r.tick(&fam, &as_refs(&d));
    }

    // The network goes. Every lane falls to its own markers: the family to well under one per
    // window (`MARKERS_PER_HOUR`), the trade tape to nothing at all (it emits none — `stream_status`
    // returns early for every stream but `book` and `depth`), the depth tape to one `GapStart` per
    // symbol.
    for i in 0..4usize {
        let mut d: Vec<(String, u64)> =
            (0..MEMBERS).map(|m| (format!("{FAM}/tok{m}"), 0u64)).collect();
        if i % 2 == 0 {
            d[i % MEMBERS].1 = 1;
        }
        d.push((LICENCE_FAM.to_string(), 0));
        d.push((DEPTH_FAM.to_string(), DEPTH_MARKERS_IN_A_STALL));
        r.tick(&fam, &as_refs(&d));
    }
    assert!(
        r.family_alerts().is_empty(),
        "a disconnect marker is not evidence that this recorder was still receiving data — a \
         whole-process stall is the recency watchdog's fault and this rule withholds: {:?}",
        r.family_alerts()
    );

    // …and the same dark family pages the moment a witness is genuinely writing again, so this is a
    // licence test and not a rule that quietly stopped working.
    let mut d = window(FAM, MEMBERS, 0, licence_per_window());
    d.push((DEPTH_FAM.to_string(), DEPTH_ITEMS_PER_WINDOW));
    r.tick(&fam, &as_refs(&d));
    let fired = r.family_alerts();
    assert_eq!(fired.len(), 1, "a live witness licenses the same dark window: {fired:?}");
    assert!(
        fired[0].body.contains("was still receiving data"),
        "…and only then may the body say so: {}",
        fired[0].body
    );
}
