//! The silence watchdog: which subscribed series have stopped receiving rows.
//!
//! ## The failure this exists for
//!
//! A venue feed that is subscribed and CONNECTED but receiving nothing is invisible from every
//! other vantage point in this daemon. `subscribe_*` returned `Ok`, so the reconcile report says
//! `started=N failed=0`. The socket is `ESTABLISHED`. No error is ever raised, because nothing
//! failed — the venue simply stops sending. The loss counters stay at zero: there are no rows to
//! lose. And the store stops growing, which nothing was watching.
//!
//! the CI box's recorder ran **95 minutes** in exactly that state, writing nothing for binance while
//! logging a clean startup and zero warnings. It was noticed by accident, days later, by reading
//! row counts by hand. That is the gap this closes: rows ARRIVING is the only signal that means
//! "this series is really recording", so it is compared against a threshold every tick.
//!
//! ## Why "expected" comes from the caller
//!
//! [`vike_data::RecorderHandle::liveness`] only knows about series that have received at least one
//! row — so on its own it can never report the worst case, a series that **never started**. The
//! runtime knows what it subscribed, so it passes that set in and the two are diffed here.
//!
//! ## Two watches, one shape
//!
//! [`SilenceWatch`] watches SERIES — rows arriving on something already subscribed. [`ResolveWatch`]
//! watches FEEDS — a venue that has never produced a symbol to subscribe in the first place. They
//! are separate because the first is structurally BLIND to the second: `expected` comes from what
//! the runtime subscribed, and a venue that never resolved subscribed nothing, so the series watch
//! iterates an empty set and reports nothing at 2 seconds or at 2 days. That blindness is why a
//! misconfigured Polymarket proxy recorded nothing for a whole run while every watchdog stayed
//! green.
//!
//! ## …and a THIRD question, over the same arrival record
//!
//! [`SilenceWatch::slow_series`] answers "is this series receiving ENOUGH?", which recency cannot.
//! A binance perp depth lane ran at **4 % of its declared cadence for forty days** and never went
//! 30 s without a row, so every check above read healthy for the whole of it. It lives on
//! [`SilenceWatch`] rather than in a watch of its own because it needs the identical bookkeeping —
//! the same key, the same subscription grace, the same forgetting of a departed series — and a
//! second copy would drift on one of the three. The expectation it judges against is declared in
//! `crates/vike-data/src/series_cadence.rs`'s `SERIES_CADENCE`; how a floor is derived from it, and
//! why the sibling trade tape has a vote, is on [`SilenceWatch::slow_series`].
//!
//! Pure and clock-injected: `now_ms` is a parameter, so the tests own time.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use vike_data::Liveness;

/// One series that is not receiving rows, and which kind of not-receiving it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Silent {
    /// `"{kind}/{venue}/{symbol}"` — the key [`vike_data::RecorderHandle::liveness`] uses.
    pub series: String,
    /// `Some(ms)` = it received rows and then stopped this long ago; `None` = it has NEVER
    /// received one. Distinct on purpose: the first is usually a venue-side stream death, the
    /// second is usually a wrong stream name or an unsupported subscription that reported success.
    pub silent_for_ms: Option<i64>,
    /// Rows this series has received in total — `0` exactly when `silent_for_ms` is `None`.
    pub rows: u64,
}

/// One series that IS receiving rows and is receiving far too few of them.
///
/// The fault [`Silent`] is structurally blind to: a binance perp depth lane ran at 4 % of its
/// declared cadence for forty days and never went 30 s without a row, so every recency check read
/// healthy while a backtest read a book that teleported every 4.3 seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Slow {
    /// `"{kind}/{venue}/{symbol}"`, as in [`Silent::series`].
    pub series: String,
    /// Ingest items per second measured over the completed window.
    pub observed_per_s: f64,
    /// The rate this series should be running at — see [`SilenceWatch::slow_series`] for how it is
    /// derived, and why it is not simply the subscription's ceiling.
    pub expected_per_s: f64,
    /// [`CADENCE_FLOOR_FRACTION`] of [`Self::expected_per_s`] — what `observed_per_s` came in
    /// under.
    pub floor_per_s: f64,
    /// The SIBLING series whose activity licensed the verdict, and its own rate. Carried in the
    /// alert body because "the tape was busy and the book was not" is the whole diagnosis, and
    /// without it an operator cannot tell a broken lane from a dead market.
    pub governor: String,
    pub governor_per_s: f64,
    /// The measured span of the window, and the items counted in it — the raw evidence, so a
    /// reader never has to trust the division.
    pub window_ms: i64,
    pub items_in_window: u64,
}

/// How long a cadence verdict is taken over.
///
/// **Fifteen minutes, and every shorter candidate was rejected for a measured reason.** The binance
/// trade tape has a per-second p50 of 4 against a max of 2,056 and 356–581 of every 3,600 seconds
/// carrying no trade at all, so any window measured in ticks reads ordinary quiet as a fault. It is
/// also 3x `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_RESEED_INTERVAL` (300 s),
/// which forces a reconnect and a REST re-seed on a HEALTHY lane — a 300 s window would beat 1:1
/// with that and some windows would contain a whole dip. ⚠ That constant belongs to the BRIDGE and
/// not to this crate, which an earlier draft of this paragraph got wrong: bybit's and okx's own
/// `market_feed.rs` each carry a separate copy at the same value, so a venue could change its
/// re-seed cadence without anything here noticing. The 3x is sized against binance's, because
/// binance is the only venue this check judges today.
///
/// And it is 3x the 300 s silence default, which keeps the two judgements from racing: anything the
/// RECENCY watchdog catches, it catches first.
///
/// The cost is stated rather than hidden: the fault this exists for is only visible fifteen minutes
/// after a recorder starts, and it ran for forty days.
pub const CADENCE_WINDOW_MS: i64 = 900_000;

/// The fraction of a series' expected rate below which it is judged SLOW.
///
/// ⚠ **A JUDGEMENT with a measured separation under it, not a derivation** — the same shape
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.7 uses for
/// `MD_LAPSE_BUDGET`, and it is stated that way so nobody re-derives a number that was never
/// derived.
///
/// What IS measured is the gap the fraction has to sit inside, for the one lane there is data for:
///
/// * BROKEN — 0.41–0.43 items/s (§12.2), and up to about **0.9/s** once the §B status markers are
///   counted, because `crates/vike-data/src/live_rec.rs`'s `stream_status` has routed depth
///   `GapStart`/`Stale`/`LiveResume` through the depth lane since 2026-09-10 and they reach
///   `ingest` like any other item. Marker inflation moves a BROKEN lane UP toward the floor and
///   leaves a healthy one alone, so it is the side the margin has to be spent on.
/// * HEALTHY — **9.8 applied diffs/s** (295 applied of 309 frames in 30 s, sampled live on
///   2026-09-10; `crates/bridges/binance/src/family/depth.rs` carries it).
///
/// At `0.20` of a 10/s ceiling the floor is 2.0/s: **2.2x above the marker-inflated broken rate and
/// 4.9x below the healthy one.** `0.10` would leave the broken side 1.08x — inside the noise the
/// markers alone can produce, i.e. a check that the next reconnect loop could walk straight past.
pub const CADENCE_FLOOR_FRACTION: f64 = 0.20;

/// One FAMILY whose total arrival count collapsed against its OWN recent history.
///
/// The fault both [`Silent`] and [`Slow`] are structurally blind to on an EVENT-DRIVEN lane, and
/// the one that motivated the whole watchdog effort: on 2026-08-05 a Polymarket `book` family went
/// from ~100,000 rows/min to 782 rows over EIGHT MINUTES across every one of its four to six
/// still-subscribed members — FIVE of those minutes at literally zero rows (04:23, 04:24, 04:26,
/// 04:28, 04:29), the longest consecutive dark run being TWO — and nothing said anything.
///
/// ⚠ **This read "FIVE CONSECUTIVE MINUTES" until it was checked against the tape.** The five dark
/// minutes are real and measured, but they are not adjacent: 04:25 carries 402 rows and 04:27
/// carries 4. `crates/vike-recorder/tests/family_collapse_alert.rs`'s `INCIDENT_ROWS_PER_MIN` is
/// the fixture, byte-identical to the store partition it was read from, and it disproves the
/// stronger claim on its own. Every conclusion drawn from the incident survives — see
/// [`FAMILY_WINDOW_MS`], where the phase argument is now made against the 120 s run it actually
/// has — and the margin behind the window size is 2.5x smaller than the old sentence asserted.
/// This is the same class of restated-figure error the "18/min" correction on
/// `crates/vike-recorder/src/alerts.rs` is about, arriving in the commit that made it.
///
/// [`Silent`] could not: the members
/// alive at 04:25 and 04:27 had produced rows inside the 300 s recency threshold, and the family
/// rotated twice inside the dark span so the tokens born into it were brand new. [`Slow`] could
/// not either, and correctly: `vike_data::series_cadence` classifies that lane
/// `Cadence::EventDriven` — the market sets the rate — so `ceiling_per_s` returns `None` and there
/// is nothing to judge against. That refusal is right and this rule does not touch it.
///
/// Every field is a COUNT or a name; there is deliberately no rate anywhere on this type. See
/// [`SilenceWatch::family_collapse`] for the whole rule and what it cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyCollapse {
    /// `{kind}/{venue}/{family}` — the subject, and the only one with continuous existence across
    /// a rotation. `crates/vike-recorder/src/runtime.rs`'s `expected_families` builds it.
    pub family: String,
    /// Ingest items summed across every member of the family in the completed window.
    pub observed_items: u64,
    /// The family's own rolling median over [`FAMILY_RING`] completed windows — a LEARNED
    /// baseline, never a declared cadence, which is why nothing here is expressed per second.
    pub baseline_items: u64,
    /// The measured span of the window, so a reader never has to trust the comparison.
    pub window_ms: i64,
    /// How many member series keys the family held when the window CLOSED. Not necessarily how
    /// many contributed: a member that departed mid-window has its final tail counted and is then
    /// gone from this number, which is the honest reading of "how big is this family now".
    pub members: usize,
    /// How many completed windows [`Self::baseline_items`] was the median of.
    pub ring_windows: usize,
    /// The out-of-family series group that LICENSED this verdict, and what it produced in the same
    /// window on the same clock. Carried because it is the entire diagnosis: "this family produced
    /// nothing while that one produced N" separates a dead family from a stalled process, and the
    /// rule refuses to take a verdict without one.
    pub licence: String,
    pub licence_items: u64,
}

/// How long a FAMILY verdict is taken over.
///
/// ⚠ **Thirty seconds, DERIVED FROM THE SHORTEST MEASURED EVENT — not from the tick, and not from
/// [`CADENCE_WINDOW_MS`].** The shortest collapse in the store is 2026-09-04's, four minutes
/// (14:55-14:59Z), and at least SEVEN 30 s tiles fall entirely inside a 240 s span at ANY phase
/// (eight only when the two happen to align — stated as seven because a phase is not something the
/// rule gets to choose).
///
/// The arithmetic that kills every longer candidate is on the motivating incident. 2026-08-05's
/// dark span is 8 minutes inside a family running ~100,000 rows/min, and a 300 s tile at the wrong
/// phase reads 87,296 rows for [04:22,04:27) and 78,642 for [04:27,04:32) against a ~500,000
/// baseline — 0.175 and 0.157 of it, nowhere near any floor, and **the incident is missed
/// entirely**. [`CADENCE_WINDOW_MS`] dilutes it further still.
///
/// ⚠ **The phase argument is made against the 120 s run the tape actually has, not a 300 s one.**
/// 2026-08-05's five dark minutes are NOT consecutive (see [`FamilyCollapse`]); its longest fully
/// dark run is 04:23-04:24, two minutes. A dark interval of 120 s contains at least THREE fully
/// dark 30 s tiles at every phase (four when aligned), so there is no phase at which this window
/// fails to see it — with a margin of three tiles rather than the ten the "five consecutive
/// minutes" sentence implied. The 30 s choice is unchanged; only its stated headroom is.
///
/// ⚠ **TIME-defined, never tick-counted.** The window closes on the first tick whose span reaches
/// it, exactly as [`SilenceWatch::slow_series`]' does, because `--tick-secs` is an operator knob:
/// at the default 30 s a window is one tick, at 5 s it is six, and at 60 s it degenerates to 60 s
/// and detection latency doubles to ~180 s. That degradation is declared and is SAFE — a longer
/// window carries a larger count and therefore a TIGHTER healthy floor, never a looser one.
pub const FAMILY_WINDOW_MS: i64 = 30_000;

/// How many completed windows the family baseline is the median of — ten minutes at the default
/// tick.
///
/// **Both bounds are measured.** LOWER: it must span at least two Polymarket rotation periods
/// (2 x 300 s) or the median is dominated by one rotation phase. UPPER: it must track the measured
/// 1.7x diurnal swing in the group rate (§12.3 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` measured 624-1,046
/// updates/s across four windows), which a ten-minute median does and a multi-hour mean does not.
///
/// MEDIAN rather than mean, so one rotation-boundary window or one burst cannot move the baseline.
pub const FAMILY_RING: usize = 20;

/// The fraction of a family's own rolling baseline below which it is judged COLLAPSED.
///
/// ⚠ **A JUDGEMENT WITH A MEASURED VOID UNDER IT** — the same shape [`CADENCE_FLOOR_FRACTION`]'s
/// doc declares, and stated that way so nobody re-derives a number that was never derived.
///
/// THE VOID, replayed over the real store on the CI box (read-only `clickhouse-local` over the
/// `kind=book`/polymarket, `kind=trade`/polymarket and `kind=trade`/binance partitions): across
/// 23,154 judged healthy windows on 13 lane-days the WORST healthy ratio is 0.0932, every incident
/// onset window is 0.000000, and the fire count on eight clean days is ZERO at every threshold from
/// 0.05 down to 0.001. **No healthy window anywhere in that corpus lies between 0.001 and 0.05 of
/// its own trailing median**, so this constant sits in the middle of a measured 50x band inside
/// which the verdict set does not change.
///
/// MARGINS: 18.6x below the worst healthy book window; 64x below the worst guarded binance-tape
/// window; 250x ABOVE the status-marker floor a fully dead family still emits (measured at 0.002 %
/// of a healthy hour's total, so marker inflation cannot lift a broken family over this line).
///
/// WHY NOT 1/50, which an earlier draft carried: it FIRES on the `trade`/polymarket lane
/// (2026-09-09, measured minimum ratio 0.01444) and leaves only 4.7x on the alerting lane. WHY NOT
/// 1/500: it still works, but retains less of each episode (the 2026-09-02 replay drops from 350
/// firing windows to 235) for no gain in a band where nothing healthy lives.
///
/// ⚠ **Two limits on that evidence, declared rather than implied.** It is measured from what the
/// recorder WROTE, not from the wire, so a sink-side loss and a venue-side loss are indistinguishable
/// in it. And the false-alarm floor rests on one family in one store over eight days: it is a
/// sample, not a proof, and a regime this store has not seen could go under 0.0932. The 50x void is
/// what buys the margin against that, not the sample size.
pub const FAMILY_FLOOR_FRACTION: f64 = 0.005;

/// The smallest baseline a family may be judged against at all.
///
/// **A RESOLUTION gate, not a venue classifier**, and it is what keeps this rule off the lane whose
/// `SERIES_CADENCE` row says in terms that "no floor above zero is safe on this row, and declaring
/// one is precisely how a pager gets muted". A ratio can only be thresholded where the alarm level
/// is a number of items a healthy family cannot reach by being quiet: at a 140-item baseline the
/// [`FAMILY_FLOOR_FRACTION`] level is 0.7 items, so the rule could only ever fire on ZERO — and
/// zero is an ORDINARY quiet window on a prediction-market trade tape.
///
/// MEASURED trailing-median occupancy on the deployed profile, per 30 s window: `book`/polymarket
/// 16,000-47,000 items (always admitted, >= 3.2x clear); `trade`/binance typically 340-2,082 with
/// busy-window peaks around 11,000 (admitted in 0.1-6 % of windows, worst ratio there 0.322);
/// `trade`/polymarket 135-146 (never admitted, 34x below); `depth`/binance ~300 (never admitted,
/// and it does not need this rule — it is the one lane [`SilenceWatch::slow_series`] already covers
/// with a DECLARED ceiling).
///
/// ⚠ The two distributions OVERLAP — binance's busiest windows exceed the book lane's quietest
/// baseline — so this is deliberately a per-WINDOW gate and not a per-lane allowlist. The 276
/// windows in which the binance tape crosses it were measured and are safe by 64x. It also
/// suppressed 120 windows of garbage verdicts on 2026-09-01, where the ring had bootstrapped
/// INSIDE a collapse and learned baselines of 20, 53, 66 and 105 rows.
pub const MIN_BASELINE_ITEMS: u64 = 5_000;

/// A window at or above this fraction of the baseline TEACHES the ring; one below it does not.
///
/// **The freeze**, and it is proven necessary rather than argued: replayed on the real 2026-08-05
/// partition a naive trailing median sags from ~50,000 toward single digits inside six minutes and
/// SILENCES ITS OWN ALARM. The 2026-09-02 replay is the positive proof — 350 firing windows held
/// end to end across a 3 h 24 m outage.
///
/// ⚠ It gates on the RATIO, not on whether the window FIRED, which is the strict improvement over
/// freezing only what fired: a collapse trickling at 1-10 % of baseline never trips
/// [`FAMILY_FLOOR_FRACTION`] and would otherwise sag its own baseline into silence.
///
/// DERIVED: it sits above the entire measured healthy minimum band (worst 0.0932, ordinary quiet
/// dips bottoming at 0.09-0.16) so a genuinely depressed window never teaches, and far below the
/// healthy median so the ring keeps learning. MEASURED COST: it excludes 1.2-6.3 % of healthy
/// windows from learning, biasing the median up by at most about one rank in twenty — irrelevant
/// against an 18.6x margin. The longest consecutive healthy exclusion run measured is six windows.
pub const FAMILY_LEARN_MIN_RATIO: f64 = 0.25;

/// How long the ring may stay frozen before it is discarded and relearned from scratch.
///
/// **The freeze's EXIT.** Without one, a permanent LEGITIMATE regime shift — a venue halving its
/// publish rate, a profile narrowed to fewer members — locks the ring at the old regime and pages
/// forever with no self-heal.
///
/// DERIVED FROM THE LONGEST MEASURED COLLAPSE: 2026-08-26 is 14,040 s (3 h 54 m, the confirmed
/// Polymarket scheduled CLOB maintenance window) and 2026-09-02 is 12,240 s (3 h 24 m), so six
/// hours is 1.5x the longest event in the store — every measured incident is covered end to end,
/// and a permanent regime shift self-heals after six hours having paged about six times under the
/// hourly repeat gate. MEASURED SAFETY: the longest consecutive below-learn-gate run on a healthy
/// day is six windows (three minutes), 120x below this timeout, so it can never trigger on a
/// healthy lane.
pub const FAMILY_FREEZE_MAX_MS: i64 = 21_600_000;

/// How long a family's learned state survives the family being ABSENT from the runtime's view.
///
/// ⚠ **Without this the state died on the FIRST tick a family did not appear, and that is a
/// transient the runtime documents as a real failure rather than a hypothetical.**
/// `crates/vike-recorder/src/runtime.rs`'s `tick` guards `last_nonempty_ms` with
/// `if !desired.is_empty()` and says in terms that "a family that resolves to zero symbols is the
/// quieter of the two failures this field exists for" — an `Ok(empty)` market-list response, which
/// unlike an `Err` does NOT `continue`. `crates/vike-recorder/src/session.rs`'s `reconcile` then
/// unsubscribes every member, `expected_families` yields no pair for that family, and one tick of
/// that used to discard a full ring, a freeze clock and an open episode. The family then needed
/// [`FAMILY_RING`] further closed windows — ten minutes at the default tick — before it could be
/// judged again, SILENTLY, and an in-flight episode re-paged as a fresh one. An empty market-list
/// response during a venue-side outage is adjacent to the very incident class this rule targets.
///
/// **DERIVED, not tuned: one ring length.** A ring is a statement about the last
/// [`FAMILY_RING`] x [`FAMILY_WINDOW_MS`] of a family's life, so once a family has been absent for
/// longer than the span its own ring covers, every entry in that ring is older than the window the
/// ring claims to describe and it is discarded — the same argument [`FAMILY_RING`] already carries,
/// read from the other end. Shorter would reintroduce the blind window this closes; longer would
/// judge a returning family against a regime its own baseline no longer claims to cover.
/// `crates/vike-recorder/src/membership.rs`'s `RETIRE_GRACE` is the same idiom one layer down.
pub const FAMILY_ABSENCE_GRACE_MS: i64 = FAMILY_RING as i64 * FAMILY_WINDOW_MS;

/// The series in `expected` that are not receiving rows: never-started ones, plus those whose last
/// row is older than `threshold_ms`.
///
/// `expected` is what the runtime believes it subscribed; `live` is
/// [`vike_data::RecorderHandle::liveness`]. A key present in `live` but absent from `expected` is
/// IGNORED rather than reported — it is a series this recorder is no longer subscribed to (a
/// rotated-out Polymarket token, say), and its silence is correct, not a fault.
///
/// Output is sorted by series name so a log line is stable between ticks.
pub fn silent_series(
    expected: &[String],
    live: &HashMap<String, Liveness>,
    now_ms: i64,
    threshold_ms: i64,
) -> Vec<Silent> {
    let mut out: Vec<Silent> = expected
        .iter()
        .filter_map(|series| match live.get(series) {
            None => Some(Silent { series: series.clone(), silent_for_ms: None, rows: 0 }),
            Some(l) => {
                let age = now_ms - l.last_ms;
                (age > threshold_ms).then(|| Silent {
                    series: series.clone(),
                    silent_for_ms: Some(age),
                    rows: l.rows,
                })
            }
        })
        .collect();
    out.sort_by(|a, b| a.series.cmp(&b.series));
    out
}

/// [`silent_series`] plus the one piece of state it needs: when each series was first subscribed.
///
/// **Why the pure function is not enough.** A series that has never received a row is silent by
/// definition the instant it is subscribed — so reporting it immediately would fire on every
/// startup and on every Polymarket token rotation, which is how a warning column becomes noise
/// nobody reads. A never-started series is only a FAULT once it has had the same grace the
/// stopped-series case gets, so this remembers when each key first appeared and withholds the
/// report until `threshold_ms` has passed since then.
///
/// Series that disappear from `expected` are forgotten, so a rotated-out token cannot make the map
/// grow without bound on a long-running daemon.
///
/// It also owns the PER-SERIES notification gate ([`alertable`](Self::alertable)) — a different
/// question from how often a silent series is LOGGED, and one the alerting rule cannot answer.
/// It ALSO owns the CADENCE judgement ([`slow_series`](Self::slow_series)) — a second question
/// over the same arrival record, deliberately not a second struct. Both need the identical
/// bookkeeping: the same `{kind}/{venue}/{symbol}` key, the same "has this been subscribed long
/// enough to judge" grace, and the same forgetting of a series that leaves `expected` so a rotating
/// Polymarket family cannot grow a map without bound. A separate watch would reimplement all three
/// and drift on one of them.
#[derive(Debug, Default)]
pub struct SilenceWatch {
    first_seen: HashMap<String, i64>,
    /// when each series last produced an ALERT — the per-series repeat gate. Dropped for a series
    /// the moment it stops being silent, so a fresh episode pages immediately.
    last_alerted: HashMap<String, i64>,
    /// The open cadence window per series: `(anchored_at_ms, rows_at_anchor)`. Re-anchored the tick
    /// a window completes, so windows tile rather than slide — a slide would re-judge the same
    /// deficit every tick and turn one fault into thirty.
    anchors: HashMap<String, (i64, u64)>,
    /// When the CURRENT cadence window opened — one clock for the whole watch, so a lane and its
    /// governor are always measured over the same span. `None` until the first judgement call.
    /// See [`slow_series`](Self::slow_series) for the phase-drift defect that forced it.
    window_start_ms: Option<i64>,
    /// [`last_alerted`](Self::last_alerted)'s twin for the cadence rule. SEPARATE on purpose: a
    /// series that pages for silence and later pages for slowness is two different faults with two
    /// different fixes, and one shared map would suppress the second.
    slow_alerted: HashMap<String, i64>,
    /// The series whose most recently COMPLETED window came in under its floor — i.e. the ones
    /// still in a slow EPISODE, which is not the same as the ones with a verdict this tick. See
    /// [`due_now`] for why the distinction is load-bearing rather than bookkeeping.
    slow_now: HashSet<String>,

    // ---- the FAMILY rule's state. See [`SilenceWatch::family_collapse`]. --------------------
    /// Per MEMBER series key: the family it was last seen in, and its last-seen `Liveness::rows` —
    /// the anchor a per-key DELTA is taken from.
    ///
    /// ⚠ This is the whole repair of the obvious wrong design: a family total must be a SUM OF
    /// PER-KEY DELTAS and never a delta of a sum of counters, or a rotation (two members leaving
    /// with their whole lifetime counters while two join at zero) reads zero-or-negative on a
    /// HEALTHY family, every window, forever. [`slow_series`](Self::slow_series)' `anchors` is the
    /// same idiom and exists for the same reason.
    ///
    /// The FAMILY rides along so a member that has already left `families` can still have its final
    /// delta credited to the family it was recorded under — see the departed pass in
    /// [`family_collapse`](Self::family_collapse).
    family_tick_rows: HashMap<String, MemberAnchor>,
    /// When a family the per-family maps still hold was first seen ABSENT from the runtime's view,
    /// per family. Cleared the moment it reappears; see [`FAMILY_ABSENCE_GRACE_MS`], which is the
    /// whole of why this field exists.
    family_absent_since: HashMap<String, i64>,
    /// Items accumulated so far in the OPEN window, per family key. Zeroed when a window closes.
    family_items: HashMap<String, u64>,
    /// The last [`FAMILY_RING`] completed windows' item counts, per family — the learned baseline.
    family_ring: HashMap<String, VecDeque<u64>>,
    /// When a family's ring stopped learning, per family — the freeze, and the clock
    /// [`FAMILY_FREEZE_MAX_MS`] is measured against. Absent while the family is teaching normally.
    family_frozen_ms: HashMap<String, i64>,
    /// Families whose most recently COMPLETED window was a verdict — the EPISODE set, and
    /// [`slow_now`](Self::slow_now)'s twin. ⚠ A merely NON-FIRING window does not remove a family
    /// from it; only a window that TEACHES does. See [`family_collapse`](Self::family_collapse).
    family_now: HashSet<String>,
    /// [`slow_alerted`](Self::slow_alerted)'s twin for the family rule, on its own map for the same
    /// reason: a family that collapses and a series inside it that goes stale are two faults, and
    /// one shared map would suppress the second.
    family_alerted: HashMap<String, i64>,
    /// When the CURRENT family window opened — a SECOND shared clock, because this rule's window is
    /// [`FAMILY_WINDOW_MS`] and [`slow_series`](Self::slow_series)' is [`CADENCE_WINDOW_MS`]. The
    /// property that matters (a subject and its governor measured over the SAME span) is preserved
    /// because a family total and its licence share THIS clock.
    family_window_start_ms: Option<i64>,
}

/// The per-SERIES repeat gate, shared by both judgements so they cannot drift.
///
/// `in_episode` is the set still IN the fault; a name that drops out of it forgets its last alert,
/// so the next episode pages immediately. `candidates` is the set with something to say THIS tick;
/// a name in it re-pages only every `repeat_ms`, and `repeat_ms == 0` pages once per episode and
/// never again while it lasts.
///
/// ⚠ **The two sets are separate arguments because the two judgements disagree about them, and
/// collapsing them silently breaks the rate one.** A silent series is in `silent_series`' output on
/// EVERY tick of an outage, so for that caller the two sets are identical. A slow series is only in
/// `slow_series`' output on the tick its WINDOW COMPLETES — one tick in thirty — so a shared set
/// would read the twenty-nine quiet ticks in between as a recovery, drop the gate, and page again
/// every fifteen minutes forever. That is the pager-fatigue failure the gate exists to prevent,
/// arriving through the gate itself.
fn due_now(
    last: &mut HashMap<String, i64>,
    in_episode: &[String],
    candidates: &[String],
    now_ms: i64,
    repeat_ms: i64,
) -> Vec<String> {
    // Recovery FIRST: a series no longer in an episode must not hold a timestamp that would
    // suppress the next genuine one.
    last.retain(|k, _| in_episode.iter().any(|a| a == k));

    // A plain loop, not a filter+map chain: the two closures would capture `last` shared and
    // mutable at once, which does not borrow-check.
    let mut due = Vec::new();
    for name in candidates {
        let fire = match last.get(name) {
            None => true, // first tick of this episode
            Some(prev) => repeat_ms > 0 && now_ms.saturating_sub(*prev) >= repeat_ms,
        };
        if fire {
            last.insert(name.clone(), now_ms);
            due.push(name.clone());
        }
    }
    due
}

/// One completed window's verdict for one series — `None` for "not slow" AND for "not judgeable",
/// which [`SilenceWatch::slow_series`] deliberately treats identically.
///
/// A free function rather than a method so it borrows nothing of the watch: the caller is midway
/// through mutating the episode set, and threading `&mut self` through the judgement would make the
/// borrow checker the reason the code is shaped the way it is.
fn judge(
    key: &str,
    observed: f64,
    items: u64,
    window_ms: i64,
    rates: &HashMap<&str, (f64, u64)>,
) -> Option<Slow> {
    let row = vike_data::series_cadence::cadence_for_series_key(key)?;
    // `None` for every class but a sampled lane with a DECLARED interval. That is the one gate
    // between this check and an invented threshold — see the cadence table's module doc.
    let ceiling = row.cadence.ceiling_per_s()?;
    let governor = governor_key(key)?;
    let &(governor_per_s, _) = rates.get(governor.as_str())?;
    // The licence: the instrument's own tape has to be busier than the sampler before a
    // ceiling-rate publish is an honest expectation.
    if governor_per_s < ceiling {
        return None;
    }
    let floor_per_s = CADENCE_FLOOR_FRACTION * ceiling;
    if observed >= floor_per_s {
        return None;
    }
    Some(Slow {
        series: key.to_string(),
        observed_per_s: observed,
        expected_per_s: ceiling,
        floor_per_s,
        governor,
        governor_per_s,
        window_ms,
        items_in_window: items,
    })
}

/// One MEMBER's anchor for the family accumulator — the per-key counter a delta is taken from.
///
/// ⚠ **`departed_ms` is the half that is easy to leave out, and leaving it out over-counts.** The
/// departed pass used to credit a member's final tail and then FORGET its anchor outright. A member
/// that is absent for one tick and back the next — a one-tick empty resolve, see
/// [`FAMILY_ABSENCE_GRACE_MS`] — then hits `family_collapse`'s first-sight arm, where `delta` is the
/// member's WHOLE `Liveness::rows` counter, and books a Polymarket token's entire ~600 s lifetime
/// into the window it returned in. That inflated window then TEACHES the ring. A 20-window median
/// absorbs one such entry, which is why this was a quiet defect rather than a loud one, and why it
/// is fixed beside the grace rather than left as a second thing to remember: keeping the ring alive
/// across a blink while letting the blink poison it would be half a repair.
///
/// So a departed member keeps its anchor, UPDATED to the counter its tail was taken from, and is
/// pruned only after [`FAMILY_ABSENCE_GRACE_MS`] — which is also the memory bound, since `live`
/// never removes a key and ~576 Polymarket tokens die a day.
#[derive(Debug)]
struct MemberAnchor {
    /// The family this member was last recorded under, so its final tail lands in the right total
    /// even after it has left `families`.
    family: String,
    /// Its `Liveness::rows` at the last accounting.
    rows: u64,
    /// `Some(first tick it was absent)` while it is gone; `None` while it is present.
    departed_ms: Option<i64>,
}

/// The MEDIAN of a family's rolling window — its learned baseline.
///
/// Median rather than mean so one rotation-boundary window or one burst cannot move the baseline,
/// which is the whole reason the ring can be as short as [`FAMILY_RING`]. On an even-length ring it
/// takes the UPPER middle element (`len / 2` after sorting) rather than averaging the two: the
/// quantity is a COUNT of items, an average would invent a value the family never produced, and the
/// difference between the two middles is noise against an 18.6x margin. Deterministic, so a log
/// line is reproducible.
///
/// A free function rather than a method for [`judge`]'s reason — the caller is midway through
/// mutating the ring it is reading.
fn median(ring: &VecDeque<u64>) -> u64 {
    if ring.is_empty() {
        return 0;
    }
    let mut v: Vec<u64> = ring.iter().copied().collect();
    v.sort_unstable();
    v[v.len() / 2]
}

/// The SIBLING trade tape of a series key — `depth/binance/BTCUSDT.P` -> `trade/binance/BTCUSDT.P`.
///
/// `None` when the key is already a trade lane (a tape cannot govern itself) or is not a three-part
/// series key.
fn governor_key(series: &str) -> Option<String> {
    let mut parts = series.splitn(3, '/');
    let kind = parts.next()?;
    let venue = parts.next()?;
    let symbol = parts.next()?;
    (kind != "trade").then(|| format!("trade/{venue}/{symbol}"))
}

impl SilenceWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The silent series worth reporting this tick. Same arguments as [`silent_series`], plus the
    /// grace described above.
    pub fn check(
        &mut self,
        expected: &[String],
        live: &HashMap<String, Liveness>,
        now_ms: i64,
        threshold_ms: i64,
    ) -> Vec<Silent> {
        for s in expected {
            self.first_seen.entry(s.clone()).or_insert(now_ms);
        }
        self.first_seen.retain(|k, _| expected.iter().any(|e| e == k));

        silent_series(expected, live, now_ms, threshold_ms)
            .into_iter()
            .filter(|s| match s.silent_for_ms {
                // Already had rows: the age check in `silent_series` is the whole judgment.
                Some(_) => true,
                // Never had one: only a fault once it has been subscribed long enough to have had
                // a fair chance.
                None => self
                    .first_seen
                    .get(&s.series)
                    .is_some_and(|first| now_ms - first > threshold_ms),
            })
            .collect()
    }

    /// Which of `silent` should raise an ALERT right now — [`check`](Self::check)'s output, gated
    /// per SERIES so a page is not the same event as a log line.
    ///
    /// **Why the gate lives here and not on the rule.** `AlertRule::cooldown_ms` is per RULE: with
    /// one rule covering every series (which is the useful shape — a recorder's series set rotates,
    /// so per-series rules are unwritable), the first silent series would consume the cooldown and
    /// the other five would be swallowed. the CI box's watchdog fired for SIX series in one episode;
    /// paging about one of them and silently dropping the rest is worse than not paging at all,
    /// because it reads as a single-series fault.
    ///
    /// `repeat_ms = 0` pages ONCE per silence EPISODE: a series that drops out of `silent` — it
    /// recovered, or it rotated out and is no longer expected — forgets its last alert, so the next
    /// episode pages immediately. A positive `repeat_ms` re-pages a still-silent series that often,
    /// which is what an operator wants for an outage that outlives one shift.
    ///
    /// Clock-injected like the rest of this module, so a test owns time.
    pub fn alertable(&mut self, silent: &[Silent], now_ms: i64, repeat_ms: i64) -> Vec<Silent> {
        let names: Vec<String> = silent.iter().map(|s| s.series.clone()).collect();
        // Both sets are the same one here: a silent series is reported on EVERY tick of its
        // outage, so "in the episode" and "has a verdict this tick" cannot differ.
        let due = due_now(&mut self.last_alerted, &names, &names, now_ms, repeat_ms);
        silent.iter().filter(|s| due.iter().any(|d| d == &s.series)).cloned().collect()
    }

    /// **The series running far below the cadence their subscription declares** — the rate half of
    /// the watchdog, and the fault [`check`](Self::check) cannot see.
    ///
    /// Same inputs as [`check`](Self::check) plus the window, so the caller passes nothing new.
    ///
    /// Windows TILE on one clock shared by every series (they do not slide, and they are not
    /// per-key): a verdict is taken only on the tick a window closes, and only for series that were
    /// present when it opened. Sliding would re-judge the same deficit every tick and turn one
    /// fault into thirty; per-key windows would let a lane and its governor phase-drift apart until
    /// the lane was never judged at all — see the comment on the window clock inside.
    ///
    /// # How an expectation is derived, and why it is not just the ceiling
    ///
    /// `vike_data::series_cadence` declares a CEILING for a sampled lane — binance depth subscribes
    /// `@depth@100ms`, so at most 10 publishes/s. It deliberately declares no FLOOR, because a
    /// sampled stream publishes only when the underlying CHANGED, and how often that happens is a
    /// property of the instrument. `crates/bridges/binance/src/family/market_feed.rs`'s
    /// `DEPTH_FRESHNESS_THRESHOLD` carries the measurement that kills the naive design: the
    /// market's thinnest actively-traded pairs gapped 44–47 s — "a near-dead pair updated only
    /// twice in 5 min", i.e. **0.0067 updates/s, sixty times slower than the lane this exists to
    /// catch**. An absolute floor sized to catch 0.42/s pages forever on every thin symbol a family
    /// glob resolves, and a pager that fires forever is a pager that gets muted.
    ///
    /// So the ceiling is a hypothesis and the SIBLING TRADE TAPE is the licence to test it:
    ///
    /// > A sampled lane is judged only while its own instrument's tape ran at or above the sampling
    /// > ceiling for the whole window. Then, and only then, the expectation IS the ceiling.
    ///
    /// Every trade changes the book, so a tape at ≥ 10 prints/s means the book had something to
    /// report in most 100 ms buckets and a ceiling-rate publish is the honest expectation. Below
    /// that, no verdict at all — which is exactly the thin-pair case, the quiet-market case, the
    /// venue-outage case (the tape dies with the book, and the RECENCY watchdog owns a total death)
    /// and the no-tape-subscribed case, all silenced by ONE rule rather than four exemptions.
    ///
    /// ⚠ **The residual, declared rather than implied.** The tape-changes-the-book argument holds
    /// on average and is violated by CLUSTERING: a tape whose prints all land inside a few hundred
    /// 100 ms buckets conflates many trades into few diffs. The fifteen-minute window and the 5x
    /// gap between the floor and a healthy rate absorb the clustering the one measured tape
    /// actually shows (84–99 % of its seconds carry a print), but a hypothetical instrument
    /// trading ≥ 9,000 times in fifteen minutes and moving its book fewer than 1,800 times would
    /// raise a false alarm. Nothing in the store shows that shape; it is not measured either way.
    ///
    /// # What it costs to be wrong in the other direction
    ///
    /// A lane whose tape is quiet goes unjudged, so this catches nothing on a thin symbol. That is
    /// the deliberate trade: the failure it exists for was on the busiest binance perp there is,
    /// and a rate collapse on an instrument nobody trades is indistinguishable from an instrument
    /// nobody trades.
    ///
    /// # ⚠ Two more accepted costs, declared because neither is obvious from the code
    ///
    /// **A lane that stops DEAD beside a busy tape pages TWICE.** Pass 1 below skips a key absent
    /// from `live` — never received a row — precisely so it does not page beside
    /// [`check`](Self::check), but a key that received rows and then STOPPED is still in `live`,
    /// still anchored, and measures 0 items/s. So it raises `recorder-series-stale` at
    /// `--silent-secs` and `recorder-series-slow` at the next window close: one fault, two rule
    /// ids, about ten minutes apart. Left as is rather than suppressed. Suppressing would mean
    /// consulting the recency verdict from here, and a suppression bug in the ONLY check for a
    /// rate collapse costs more than a duplicate page — while the second alert is not redundant,
    /// because it is the one that names the governor and so says the instrument was still trading.
    ///
    /// **A flapping governor can outrun the repeat gate.** An instrument whose tape hovers around
    /// the ceiling alternates judged / not-judged, and a `None` verdict CLOSES the slow episode
    /// (see the `None` arm below), which drops the series from `slow_alerted` and re-arms it. So a
    /// genuinely broken lane on a marginal instrument can page once per window rather than once
    /// per `repeat_secs`. This is the same trade the `None` arm argues for and it is the one path
    /// that can exceed the repeat budget; it cannot affect a HEALTHY lane, which is never judged
    /// slow in the first place.
    pub fn slow_series(
        &mut self,
        expected: &[String],
        live: &HashMap<String, Liveness>,
        now_ms: i64,
        window_ms: i64,
    ) -> Vec<Slow> {
        if window_ms <= 0 {
            return Vec::new();
        }
        self.anchors.retain(|k, _| expected.iter().any(|e| e == k));
        self.slow_now.retain(|k| expected.iter().any(|e| e == k));

        // ⚠ ONE window clock for the whole watch, not one per series — and this is a CORRECTNESS
        // property, not tidiness. A per-key window starts when that key's first row arrives, so a
        // lane whose first row lands on a different tick from its governor's is permanently
        // phase-shifted: their windows never close on the same tick, the governor's rate is never
        // available when the lane is judged, and the lane is therefore NEVER JUDGED — silently,
        // forever. That is the failure this whole feature exists to remove, reappearing inside it.
        // Caught by `crates/vike-recorder/tests/cadence_alert.rs`'s
        // `the_thinnest_measured_binance_pair_never_pages`, whose depth lane is quiet enough that
        // its first row lands four ticks after its tape's.
        let start = *self.window_start_ms.get_or_insert(now_ms);
        let span = now_ms.saturating_sub(start);
        let closing = span >= window_ms;
        // A wall-clock STEP makes `span` meaningless — `Liveness::last_ms` and `now_ms` both read
        // `vike_model::clock::now_ms`, so an NTP correction lands between the anchor and here.
        // Discard the window and re-anchor rather than divide by a number nobody measured.
        let stepped = span > window_ms.saturating_mul(4);

        // Pass 1 — measure every series that has been present since this window OPENED, and
        // re-anchor everything when it closes. EVERY expected series is measured, not just the
        // sampled ones: a sampled lane's verdict needs its governor's rate over the SAME window.
        let mut rates: HashMap<&str, (f64, u64)> = HashMap::new();
        for key in expected {
            // Absent from `live` = never received a row. That is `check`'s fault to report, and
            // reporting it here as "0 items/s" would page twice for one thing.
            let Some(l) = live.get(key) else {
                self.anchors.remove(key);
                continue;
            };
            let Some((at_ms, rows)) = self.anchors.get(key).copied() else {
                self.anchors.insert(key.clone(), (now_ms, l.rows));
                continue;
            };
            if !closing {
                continue;
            }
            // `at_ms > start` = this series joined MID-window, so its count covers less than the
            // window and its rate would read low. Skip it and let the re-anchor below put it on
            // the shared boundary, where it is judged from the next window on.
            if at_ms <= start && !stepped {
                let items = l.rows.saturating_sub(rows);
                rates.insert(key.as_str(), (items as f64 * 1_000.0 / span as f64, items));
            }
            self.anchors.insert(key.clone(), (now_ms, l.rows));
        }

        if !closing {
            return Vec::new();
        }
        self.window_start_ms = Some(now_ms);

        // Pass 2 — judge the sampled lanes whose governor was measured over this same window.
        let mut out: Vec<Slow> = Vec::new();
        for key in expected {
            // A closed window is the only thing that can START or END an episode, so a series that
            // was not measured leaves `slow_now` exactly as it was.
            let Some(&(observed, items)) = rates.get(key.as_str()) else { continue };
            match judge(key, observed, items, span, &rates) {
                Some(slow) => {
                    self.slow_now.insert(key.clone());
                    out.push(slow);
                }
                // Not slow, OR not judgeable at all — and the two must behave the SAME here: a
                // lane whose tape goes quiet after a slow episode has no verdict either way, and
                // holding its episode open would suppress the alert for its next real one.
                None => {
                    self.slow_now.remove(key);
                }
            }
        }
        out.sort_by(|a, b| a.series.cmp(&b.series));
        out
    }

    /// Which of `slow` should raise an ALERT right now — [`alertable`](Self::alertable)'s twin, on
    /// its own map so the two faults cannot suppress each other.
    pub fn slow_alertable(&mut self, slow: &[Slow], now_ms: i64, repeat_ms: i64) -> Vec<Slow> {
        // ⚠ The two sets differ here, and that is the whole point — see [`due_now`]. `slow_now` is
        // who is still IN a slow episode (it persists between windows); `slow` is only who
        // completed a window THIS tick, which is one tick in thirty.
        let in_episode: Vec<String> = self.slow_now.iter().cloned().collect();
        let names: Vec<String> = slow.iter().map(|s| s.series.clone()).collect();
        let due = due_now(&mut self.slow_alerted, &in_episode, &names, now_ms, repeat_ms);
        slow.iter().filter(|s| due.iter().any(|d| d == &s.series)).cloned().collect()
    }

    /// **The families whose total arrival count collapsed against their OWN recent history** — the
    /// rate half of the watchdog on a lane where no rate can be DECLARED, and the one that closes
    /// the 2026-08-05 Polymarket incident.
    ///
    /// `families` is `crate::runtime::RecorderRuntime::expected_families()`: one
    /// `(series key, family key)` pair per live subscription, resolved by the RUNTIME and passed as
    /// plain data so this module stays the pure judgement layer (the same reason [`FeedResolve`] is
    /// not a `crate::runtime::FeedTick`). `live` is the same arrival record every other judgement
    /// here reads.
    ///
    /// # Why the subject is a FAMILY
    ///
    /// A per-INSTRUMENT rate cannot work on this lane and that is measured, not argued. A
    /// Polymarket token's own rate across its ~600 s life runs 484 -> 91,143 -> 647 rows/min
    /// (2026-09-10, one token, its whole life) — a 188x ramp up and a 141x fall, happening to two
    /// tokens every five minutes, 576 times a day. The fall is the NORMAL end of every token's
    /// life, indistinguishable from a collapse by rate alone. Worse for the motivating incident
    /// specifically: the family rotated at least twice INSIDE the 2026-08-05 dark span, so the
    /// tokens born into it had no healthy history to fall from at all.
    ///
    /// So the question is never asked. The family key is the one subject with CONTINUOUS EXISTENCE
    /// across a rotation: a dying member's tail and its successor's birth land in one total, and
    /// the ~576 legitimate deaths a day are invisible BY CONSTRUCTION — not by a tuned grace
    /// window, not by an end-of-life seam plumbed through `crate::runtime::VenueFeed`, not by an
    /// exemption anybody has to maintain. Replayed over eight healthy days (22,878 judged windows,
    /// ~4,600 legitimate deaths) it produced ZERO verdicts, and the worst family-total ratio
    /// anywhere in that corpus is 0.0932.
    ///
    /// # The rule
    ///
    /// Each [`FAMILY_WINDOW_MS`] window, every member's per-key ITEM DELTA is summed into its
    /// family and that total is judged against the family's own FROZEN rolling median
    /// ([`FAMILY_RING`] windows). A verdict is taken only when ALL of:
    ///
    /// 1. the ring is FULL — no history, no number, nothing to threshold. The cold-start refusal
    ///    mirrors `vike_data::series_cadence`'s `ceiling_per_s` returning `None`;
    /// 2. the baseline clears [`MIN_BASELINE_ITEMS`] — a resolution gate, so the alarm level is a
    ///    number of items a healthy family cannot reach by being quiet;
    /// 3. some series OUTSIDE this family produced items in the SAME window (the licence), unless
    ///    this process records only one family and there is nothing outside to ask;
    /// 4. the total is under [`FAMILY_FLOOR_FRACTION`] of the baseline.
    ///
    /// …and the window then TEACHES the ring only if it reached [`FAMILY_LEARN_MIN_RATIO`] of the
    /// baseline, so a sustained collapse cannot sag its own expectation into silence. A ring held
    /// below that gate for [`FAMILY_FREEZE_MAX_MS`] is discarded and relearned, which is how a
    /// permanent legitimate regime shift self-heals instead of paging forever.
    ///
    /// ⚠ **An EPISODE closes only on a window that TEACHES, never on a merely non-firing one.**
    /// [`due_now`]'s own doc warns that "a flapping governor can outrun the repeat gate"; here a
    /// collapse trickling at 10 % of baseline neither fires nor teaches, and treating that as a
    /// recovery would re-arm the gate and page again on the very next dark window — every 30
    /// seconds, which is precisely the cry-wolf failure that gets a pager muted.
    ///
    /// ⚠ **That covers the 0.5-25 % band and NOT the 25-100 % one, and this paragraph used to be
    /// read as covering both.** `taught` is true from a quarter of baseline upward, so a fault
    /// alternating dark / 40 % closes an episode on every other window — a 4x shortfall being
    /// treated as a recovery — and until it was fixed each such window forgot the repeat gate's
    /// timestamp and the next dark window paged immediately. The guard that actually holds the
    /// whole band is in [`family_alertable`](Self::family_alertable), where the gate's memory
    /// outlives the episode by `repeat_ms`; read its note for the trace and for what that costs.
    /// The episode rule above is kept because it is still the cheaper of the two and because it is
    /// what keeps a trickling collapse in one episode for the LOG as well as for the pager.
    ///
    /// # ⚠ What this CANNOT see, declared rather than implied
    ///
    /// * **A fault already present when the baseline was learned.** The single fundamental limit of
    ///   self-reference, and `crates/vike-data/src/series_cadence.rs`'s binance `depth` row states
    ///   it as a rule: never seed an expectation from a series' own history. That lane ran at 4 %
    ///   of its DECLARED cadence for forty days; this ring would have learned the broken rate as
    ///   normal inside ten minutes and said nothing for all forty. [`slow_series`](Self::slow_series)
    ///   catches exactly that and this cannot. **They are COMPLEMENTS, not substitutes.**
    /// * **Any shortfall shallower than ~200x.** This is a catastrophic-collapse detector and
    ///   nothing else. Measured on the motivating incident itself: 2026-08-05's recovery phase ran
    ///   at 20-50 % of normal for fourteen minutes, shedding roughly another million rows, and
    ///   never fires.
    /// * **A decay slower than the ring.** Anything degrading more slowly than a ten-minute
    ///   trailing median is followed down by its own expectation. The learn gate narrows this and
    ///   does not close it: a decay that stays above a quarter per window is absorbed.
    /// * **One member of a healthy family.** One member is ~25 % of the total, so a single member
    ///   dying moves the ratio to ~0.75 and never fires. The complement is the MIRROR-PAIR identity
    ///   (an UP token and its DOWN twin produce byte-identical minute counts, verified 2026-09-10),
    ///   which needs no rate model at all and is not folded in here.
    /// * **WHOSE fault it is.** The alert says this family stopped while another kept writing, and
    ///   nothing more. It cannot tell a dead socket from a venue-side CLOB halt.
    /// * **The first [`FAMILY_RING`] windows after a restart.** Nothing is persisted across one,
    ///   deliberately — a stale baseline applied to a changed profile is worse than no baseline.
    /// * **Any family whose baseline is under [`MIN_BASELINE_ITEMS`]**, by construction. Those
    ///   lanes are UNJUDGED, not covered.
    /// * **CORRECTNESS — only VOLUME.** A family producing the right volume of duplicated, stale or
    ///   wrong-symbol rows is perfectly healthy to this rule.
    /// * **A whole-process stall**, where the licence is withheld and no verdict is taken at all:
    ///   that is the recency watchdog's fault and it fires first. ⚠ This was a CLAIM the code did
    ///   not keep until the licence stopped being a `> 0` test: stream-health markers are ordinary
    ///   rows on the two L2 lanes, they are emitted BECAUSE data stopped, and one `GapStart` per
    ///   binance symbol was enough to license a verdict during a host-wide network loss. The
    ///   licence now requires a witness that is materially alive on its own baseline — see the
    ///   licence note in the body. ⚠ On a recorder subscribing ONLY
    ///   Polymarket the sole out-of-family series is that venue's trade lane, which died in the
    ///   same minutes on 2026-08-05 — so such a recorder would take no verdict either. The blind
    ///   spot moves from "one instrument's siblings" to "everything this process records". It does
    ///   not vanish.
    /// * **A symbol id RE-USED more than [`FAMILY_ABSENCE_GRACE_MS`] after it was pruned.** A
    ///   departed member keeps its anchor for one grace window and is then dropped; were the same
    ///   id re-subscribed after that, its first window would over-count by that symbol's whole
    ///   prior lifetime. Polymarket token ids are unique per window and binance symbols never leave
    ///   the set, so this cannot fire on any deployed profile — but it is an assumption about
    ///   venues, declared rather than assumed away. ⚠ The grace narrows this: a member that merely
    ///   BLINKS (an `Ok(empty)` resolve for one tick) used to land in exactly this arm and book a
    ///   whole token lifetime into the window it returned in.
    /// * **A collapse continuing past [`FAMILY_FREEZE_MAX_MS`] stops paging, PERMANENTLY.** The
    ///   freeze exit discards the ring and relearns it from the collapsed traffic; any shift deep
    ///   enough to have kept firing is by definition more than 200x down, so the relearned median
    ///   cannot clear [`MIN_BASELINE_ITEMS`] and the family is UNJUDGED from then on rather than
    ///   temporarily quiet. That is the intended self-heal for a permanent legitimate regime shift
    ///   — the family IS a thin lane now — and it is indistinguishable from an outage still in
    ///   progress. The longest measured collapse (2026-08-26, 3 h 54 m) fits inside six hours,
    ///   which is the argument for the constant and belongs beside this consequence.
    ///   `crates/vike-recorder/tests/family_collapse_alert.rs`'s `a_frozen_ring_relearns_after_six_hours`
    ///   is the pin.
    /// * **A family absent for longer than [`FAMILY_ABSENCE_GRACE_MS`]**, which discards its ring
    ///   and leaves it unjudged for [`FAMILY_RING`] windows after it returns. Shorter absences are
    ///   covered; see that constant for why the two are the same number.
    /// * **A witness whose own ring was just discarded.** The licence requires a busier family to
    ///   have reached [`FAMILY_LEARN_MIN_RATIO`] of its own baseline, and a family with no baseline
    ///   yet (a cold ring, or one cleared by its own freeze exit) satisfies that trivially. A
    ///   network loss lasting past a witness's six-hour freeze exit could therefore license again
    ///   on markers alone. Every measured incident is two orders of magnitude short of that, and
    ///   the recency watchdog owns a six-hour total stall long before.
    ///
    /// # ⚠ A wall-clock step does not need a guard here, and that is worth stating
    ///
    /// [`slow_series`](Self::slow_series) divides by its span and so must discard a stepped window.
    /// This one compares COUNTS, so a forward step only closes a window after fewer TICKS than
    /// usual. At the default tick (equal to the window) that costs nothing at all; at the worst
    /// configurable tick it leaves a window holding 1/30th of its arrivals, i.e. a ratio of 0.033 —
    /// still 6.7x above the floor. A backward step would hold a window open indefinitely, so that
    /// one IS handled: the clock re-anchors and the window is dropped.
    pub fn family_collapse(
        &mut self,
        families: &[(String, String)],
        live: &HashMap<String, Liveness>,
        now_ms: i64,
        window_ms: i64,
    ) -> Vec<FamilyCollapse> {
        if window_ms <= 0 {
            return Vec::new();
        }

        // PASS 1 — every member's own DELTA, summed into its family.
        //
        // ⚠ A SUM OF PER-KEY DELTAS, never a delta of a sum of counters. Differencing a summed
        // counter across a CHANGING key set reads zero-or-negative on a HEALTHY rotating family,
        // every window, forever: two departing tokens take their whole lifetime counters out of
        // the sum while two joiners start at zero. `slow_series`' `anchors` is the same idiom.
        let mut members: BTreeMap<String, usize> = BTreeMap::new();
        for (key, fam) in families {
            *members.entry(fam.clone()).or_insert(0) += 1;
            let mut delta = 0;
            if let Some(l) = live.get(key) {
                // The `None` arm is EXACT, not an approximation: `crates/vike-data/src/live_rec.rs`'s
                // `ingest` inserts `Liveness { rows: 0, .. }` on first sight and the counter is
                // per-process, so a member first seen with N items produced all N in THIS window.
                // Taking 0 instead would shed a whole birth's arrivals at every rotation.
                delta = match self.family_tick_rows.get(key) {
                    Some(a) => l.rows.saturating_sub(a.rows),
                    // Absent from `live` entirely = never received a row. That is `check`'s fault
                    // to report and it contributes nothing here.
                    None => l.rows,
                };
                // A returning member is anchored, not re-counted: `departed_ms` goes back to
                // `None` and the delta above was taken from the anchor its departure left behind.
                self.family_tick_rows.insert(
                    key.clone(),
                    MemberAnchor { family: fam.clone(), rows: l.rows, departed_ms: None },
                );
            }
            *self.family_items.entry(fam.clone()).or_insert(0) += delta;
        }

        // PASS 2 — the DEPARTED. A token leaves `families` the tick the runtime unsubscribes it,
        // which is before the rows it produced since the previous tick have been accounted. Its
        // final delta is collected ONCE, against the family it was recorded under.
        //
        // ⚠ Its anchor is KEPT, re-anchored at the counter the tail was taken from, and pruned only
        // after `FAMILY_ABSENCE_GRACE_MS` — see `MemberAnchor::departed_ms` for the over-count that
        // forgetting it immediately produced on a member that blinked and came back. The prune is
        // still the memory bound: `live` never removes a key and ~576 Polymarket tokens die a day,
        // so only the handful inside one grace window are held.
        let departed: Vec<String> = self
            .family_tick_rows
            .keys()
            .filter(|k| !families.iter().any(|(key, _)| key == *k))
            .cloned()
            .collect();
        for key in departed {
            let Some(anchor) = self.family_tick_rows.get_mut(&key) else { continue };
            // The tail is credited ONCE — on the first tick of the absence, which is the only one
            // where the anchor is still behind the counter. Later ticks find `saturating_sub` at
            // zero anyway, but the explicit `is_none` says so rather than relying on that.
            if anchor.departed_ms.is_none() {
                anchor.departed_ms = Some(now_ms);
                if let Some(l) = live.get(&key) {
                    let tail = l.rows.saturating_sub(anchor.rows);
                    anchor.rows = l.rows;
                    if tail > 0 {
                        *self.family_items.entry(anchor.family.clone()).or_insert(0) += tail;
                    }
                }
            }
        }
        self.family_tick_rows.retain(|_, a| {
            a.departed_ms.is_none_or(|since| now_ms.saturating_sub(since) < FAMILY_ABSENCE_GRACE_MS)
        });

        // …and every per-family map follows the live family set, so a profile change or a venue
        // removal cannot leave a ring, a freeze or an episode behind to be judged against later —
        // but NOT on the first tick a family is missing.
        //
        // ⚠ `family_items` is the exception and drops IMMEDIATELY, deliberately: it is the OPEN
        // window's accumulator, and a family with no members this tick has no honest total for a
        // window that closes now. Judging a partial accumulation is the one shape of this fix that
        // could manufacture a verdict, so the grace is granted to the LEARNED state (the ring, the
        // freeze clock, the episode) and refused to the in-flight count. A family that blinks
        // therefore takes no verdict for that window and resumes with its baseline intact, which is
        // the difference between "unjudged for one window" and "blind for ten minutes".
        self.family_items.retain(|f, _| members.contains_key(f));
        // A family that is BACK forgets it was ever away, so a second blink gets a whole fresh
        // grace rather than resuming somebody else's clock.
        self.family_absent_since.retain(|f, _| !members.contains_key(f));
        let mut expired: Vec<String> = Vec::new();
        for fam in self.family_ring.keys() {
            if members.contains_key(fam) {
                continue;
            }
            let since = *self.family_absent_since.entry(fam.clone()).or_insert(now_ms);
            if now_ms.saturating_sub(since) >= FAMILY_ABSENCE_GRACE_MS {
                expired.push(fam.clone());
            }
        }
        for fam in expired {
            self.family_absent_since.remove(&fam);
            self.family_ring.remove(&fam);
            self.family_frozen_ms.remove(&fam);
            self.family_now.remove(&fam);
        }

        // ⚠ ONE window clock for the whole watch, and a SECOND one beside `slow_series`': that
        // rule's window is `CADENCE_WINDOW_MS` and this one's is `FAMILY_WINDOW_MS`. The property
        // a shared clock exists to defend — a subject and its governor measured over the SAME
        // span — is preserved, because a family total and its licence share THIS one.
        let start = *self.family_window_start_ms.get_or_insert(now_ms);
        let span = now_ms.saturating_sub(start);
        if span < 0 {
            // A wall-clock step BACKWARDS would otherwise hold this window open until the clock
            // caught up. Re-anchor and drop it; see the step note on this method.
            self.family_window_start_ms = Some(now_ms);
            return Vec::new();
        }
        if span < window_ms {
            return Vec::new();
        }
        self.family_window_start_ms = Some(now_ms);
        let closed = std::mem::take(&mut self.family_items);

        let family_count = closed.len();

        // PASS A — every family's own ring, freeze and learn verdict, BEFORE any licence is
        // chosen. The order is load-bearing rather than tidy: a family cannot be asked to vouch
        // for another until its own window has been judged against its own baseline, and the
        // licence below is exactly that question. The ring update is unaffected by the split — it
        // gates on the RATIO and never on whether a window fired, as `FAMILY_LEARN_MIN_RATIO`'s
        // doc says — so this pass is the same bookkeeping it always was, just hoisted.
        let mut judged: HashMap<String, (usize, u64, bool)> = HashMap::new();
        for (fam, &items) in &closed {
            // THE FREEZE'S EXIT, evaluated BEFORE the verdict: a ring held below the learn gate
            // this long describes a regime that no longer exists, so it is discarded and this
            // window takes no verdict at all — the cold-start refusal, reached from the other side.
            if self
                .family_frozen_ms
                .get(fam)
                .is_some_and(|since| now_ms.saturating_sub(*since) >= FAMILY_FREEZE_MAX_MS)
            {
                self.family_frozen_ms.remove(fam);
                self.family_ring.entry(fam.clone()).or_default().clear();
            }

            let (ring_windows, baseline, taught) = {
                let ring = self.family_ring.entry(fam.clone()).or_default();
                let ring_windows = ring.len();
                let baseline = if ring_windows >= FAMILY_RING { median(ring) } else { 0 };
                // A ring that is not yet full, or a baseline of zero, always teaches: there is no
                // ratio to gate on, and refusing to learn would leave it empty forever.
                let taught =
                    baseline == 0 || items as f64 >= FAMILY_LEARN_MIN_RATIO * baseline as f64;
                if taught {
                    if ring.len() >= FAMILY_RING {
                        ring.pop_front();
                    }
                    ring.push_back(items);
                }
                (ring_windows, baseline, taught)
            };

            if taught {
                self.family_frozen_ms.remove(fam);
            } else {
                self.family_frozen_ms.entry(fam.clone()).or_insert(now_ms);
            }
            judged.insert(fam.clone(), (ring_windows, baseline, taught));
        }

        // PASS B — the licence, and the verdict it gates.
        let mut out = Vec::new();
        for (fam, &items) in &closed {
            let (ring_windows, baseline, taught) = judged[fam];

            // The licence: the busiest series group outside this family that is itself MATERIALLY
            // ALIVE this window, on this same clock. Ties break on the name so a log line is stable
            // between ticks.
            //
            // ⚠ **"Alive" is not "non-zero", and the difference is a whole failure mode.** This
            // read `licence_items > 0` over raw `Liveness::rows`, which counts the stream-health
            // MARKERS `crates/vike-data/src/live_rec.rs`'s `stream_status` writes as ordinary rows
            // — `GapStart`/`Stale`/`LiveResume`, emitted BECAUSE data stopped. A whole-host network
            // loss therefore licensed itself: binance's depth lane emits at least one `GapStart` per
            // symbol with nothing inbound at all, the Polymarket book family falls to its own
            // handful of markers (measured at ~0.6 items per window, far under its floor), and the
            // first dark window of a PROCESS-wide outage paged for one family with a body asserting
            // "this recorder was still receiving data". That is the proportional-collapse blind spot
            // this rule DECLARES it withholds on, contradicted by the code that declares it.
            //
            // The repair costs no new constant: a witness must have reached `FAMILY_LEARN_MIN_RATIO`
            // of its OWN baseline — the same gate that already decides whether a window is healthy
            // enough to teach, computed for every family in pass A above. A marker-only window is
            // far below it, so a dead process can no longer vouch for itself, while the measured
            // healthy case is unaffected: binance wrote 348-909 trades in every minute of
            // 04:23-04:30 on 2026-08-05, which is its ordinary level and teaches.
            // `licence_items > 0` is KEPT beside it rather than replaced, because a witness whose
            // own ring is not yet full has `baseline == 0` and so teaches trivially.
            let (licence, licence_items) = closed
                .iter()
                .filter(|(f, _)| f != &fam)
                .filter(|(f, n)| **n > 0 && judged.get(*f).is_some_and(|j| j.2))
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(f, n)| (f.clone(), *n))
                .unwrap_or_default();

            // The licence is WAIVED when this process records one family and nothing else: there
            // is no outside to ask, and a rule that could never fire on a single-family recorder
            // would be a rule nobody could deploy. A total death there is the recency watchdog's.
            let licensed = family_count < 2 || licence_items > 0;
            let verdict = ring_windows >= FAMILY_RING
                && baseline >= MIN_BASELINE_ITEMS
                && licensed
                && (items as f64) < FAMILY_FLOOR_FRACTION * baseline as f64;

            if verdict {
                self.family_now.insert(fam.clone());
                out.push(FamilyCollapse {
                    family: fam.clone(),
                    observed_items: items,
                    baseline_items: baseline,
                    window_ms: span,
                    members: members.get(fam).copied().unwrap_or(0),
                    ring_windows,
                    licence,
                    licence_items,
                });
            } else if taught {
                // ⚠ Only a window that TEACHES closes an episode — see the flapping note on this
                // method. A window that neither fired nor taught leaves the episode exactly as it
                // was, which is what keeps the repeat gate armed through a trickling collapse.
                //
                // ⚠ This is no longer the ONLY thing holding the gate, and it must not be read as
                // such: `taught` is true from a quarter of baseline upward, so a fault flapping
                // between dark and 40 % closes an episode here on every other window. What stops
                // that re-paging is `family_alertable`, where the repeat gate's memory now outlives
                // the episode by `repeat_ms`. Read the two together.
                self.family_now.remove(fam);
            }
        }
        // Sorted by family, like every other judgement here, so a log line is stable between ticks
        // — `closed` is a `HashMap` and its iteration order is not.
        out.sort_by(|a, b| a.family.cmp(&b.family));
        out
    }

    /// Which of `collapses` should raise an ALERT right now — [`slow_alertable`](Self::slow_alertable)'s
    /// twin, keyed on the FAMILY and on its own map so the three faults cannot suppress each other.
    pub fn family_alertable(
        &mut self,
        collapses: &[FamilyCollapse],
        now_ms: i64,
        repeat_ms: i64,
    ) -> Vec<FamilyCollapse> {
        // ⚠ The two sets differ here for [`due_now`]'s documented reason, and this rule needs the
        // distinction MORE than the cadence one does: a family verdict is taken only on the tick a
        // window CLOSES, and — unlike the cadence rule — a merely non-firing window does not close
        // the episode. See [`family_collapse`](Self::family_collapse)'s episode note.
        //
        // ⚠ **`in_episode` is WIDER than the episode set, and that is the fix for a re-page every
        // 60 s.** `due_now`'s first act is to forget the last-alert timestamp of any name not in
        // this slice, so whatever closes an episode also RE-ARMS the repeat gate. The episode is
        // closed by any window that TEACHES, i.e. by anything from a quarter of baseline upward —
        // which is a 4x shortfall, not a recovery. A socket that reconnects, delivers a burst and
        // dies again on a ~30 s cycle (the Polymarket CLOB instability this rule exists for) then
        // alternates dark / 40 %, and each 40 % window dropped the timestamp so the next dark
        // window paged immediately: 60 pages an hour against a `repeat_secs` of 3600, sustaining
        // indefinitely because only the teaching windows enter the ring so the baseline settles at
        // the recovery level. That is the cry-wolf failure the whole gate exists to prevent,
        // arriving through the gate itself.
        //
        // So the gate's memory OUTLIVES the episode by `repeat_ms`: a family is treated as still in
        // one while its last alert is younger than the repeat interval. It can only ever SUPPRESS a
        // page, never produce one, so none of the rule's false-alarm properties are touched. The
        // cost is declared rather than hidden: a family that genuinely recovers and genuinely
        // collapses AGAIN inside `repeat_secs` is one page, not two — which is precisely what
        // `repeat_secs` means for every other subject in this file, and the operator was paged for
        // that family inside the hour either way.
        //
        // ⚠ `repeat_ms == 0` ("page once per episode, never again while it lasts") is deliberately
        // unchanged: `now - t < 0` is false for every `t`, so a closed episode forgets its
        // timestamp exactly as before and a genuinely new episode pages. Widening that case would
        // change the meaning of the setting rather than fix a defect.
        let in_episode: Vec<String> = self
            .family_now
            .iter()
            .cloned()
            .chain(
                self.family_alerted
                    .iter()
                    .filter(|(_, at)| now_ms.saturating_sub(**at) < repeat_ms)
                    .map(|(fam, _)| fam.clone()),
            )
            .collect();
        let names: Vec<String> = collapses.iter().map(|c| c.family.clone()).collect();
        let due = due_now(&mut self.family_alerted, &in_episode, &names, now_ms, repeat_ms);
        collapses.iter().filter(|c| due.iter().any(|d| d == &c.family)).cloned().collect()
    }
}

/// One feed's resolve state, as [`ResolveWatch`] sees it — [`crate::runtime::FeedTick`] narrowed to
/// the two things the judgment needs.
///
/// Deliberately NOT the `FeedTick` itself: this module is the pure judgment layer and takes plain
/// data, exactly as [`silent_series`] takes `expected`/`live` rather than a runtime handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedResolve {
    pub venue: String,
    /// See [`crate::runtime::FeedTick::last_nonempty_ms`]. `None` = this venue has never produced a
    /// symbol since startup.
    pub last_nonempty_ms: Option<i64>,
}

/// [`SilenceWatch`]'s FEED-level twin: which venues have NEVER resolved a symbol since startup.
///
/// ## The gap it closes
///
/// [`SilenceWatch`] can only report a series the runtime believes it SUBSCRIBED
/// (`RecorderRuntime::expected_series`). A venue whose resolve fails has zero subscriptions, so
/// that set is empty, so [`silent_series`] iterates nothing and returns nothing — at 2 seconds or
/// at 2 days. **The silence watchdog watches SERIES; nothing watched FEEDS.** That is why a
/// misconfigured Polymarket proxy produced one `warn!` a tick, forever, and no alert: measured on
/// the CI box, `could not resolve the desired set — subscriptions unchanged … Connection refused`.
///
/// ## What it does NOT report, deliberately
///
/// A venue that resolved before and is failing NOW (`last_nonempty_ms: Some(_)`). That is a retry —
/// a Gamma blip, a DNS wobble — the daemon leaves its live books alone on purpose
/// (`crate::runtime`'s module doc), and the per-tick `warn!` is the right and unchanged reaction.
/// Escalating it would page for every transient the daemon already handles correctly.
///
/// Same three properties as its sibling, for the same reasons: a per-VENUE `first_seen` grace (a
/// startup resolve is allowed one threshold to succeed, or every start pages), a per-VENUE repeat
/// gate with a recovery re-arm (a five-venue outage must page five times, not once — see
/// [`SilenceWatch::alertable`]), and departed-venue forgetting. Pure and clock-injected.
#[derive(Debug, Default)]
pub struct ResolveWatch {
    first_seen: HashMap<String, i64>,
    last_alerted: HashMap<String, i64>,
}

impl ResolveWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The venues that have never resolved a symbol and have had `grace_ms` to do it. Sorted, so a
    /// log line is stable between ticks.
    pub fn check(&mut self, feeds: &[FeedResolve], now_ms: i64, grace_ms: i64) -> Vec<String> {
        for f in feeds {
            self.first_seen.entry(f.venue.clone()).or_insert(now_ms);
        }
        self.first_seen.retain(|k, _| feeds.iter().any(|f| &f.venue == k));

        let mut out: Vec<String> = feeds
            .iter()
            .filter(|f| f.last_nonempty_ms.is_none())
            .filter(|f| {
                self.first_seen.get(&f.venue).is_some_and(|first| now_ms - first > grace_ms)
            })
            .map(|f| f.venue.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Which of `unresolved` should ALERT right now — the per-VENUE repeat gate, with a recovery
    /// re-arm. Verbatim [`SilenceWatch::alertable`]'s contract, keyed on venue instead of series,
    /// and it is NOT `AlertRule::cooldown_ms` for that method's documented reason: one rule covers
    /// every venue, so a per-rule cooldown would page for one venue of a three-venue outage and
    /// swallow the rest, which reads as a single-venue fault.
    pub fn alertable(&mut self, unresolved: &[String], now_ms: i64, repeat_ms: i64) -> Vec<String> {
        self.last_alerted.retain(|k, _| unresolved.iter().any(|v| v == k));

        let mut due = Vec::new();
        for venue in unresolved {
            let fire = match self.last_alerted.get(venue) {
                None => true,
                Some(last) => repeat_ms > 0 && now_ms.saturating_sub(*last) >= repeat_ms,
            };
            if fire {
                self.last_alerted.insert(venue.clone(), now_ms);
                due.push(venue.clone());
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
        pairs
            .iter()
            .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
            .collect()
    }
    pub(super) fn expect(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flowing_series_is_not_reported() {
        let got = silent_series(
            &expect(&["trade/binance/BTC"]),
            &live(&[("trade/binance/BTC", 9, 990)]),
            1_000,
            60_000,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    /// **The 95-minute case.** It received rows, then the venue stream went quiet — the loss
    /// counters stay zero and nothing errors, so this is the only place it shows.
    #[test]
    fn a_series_that_stopped_is_reported_with_its_age() {
        let got = silent_series(
            &expect(&["trade/binance/BTC"]),
            &live(&[("trade/binance/BTC", 6_000, 1_000)]),
            1_000 + 95 * 60_000,
            60_000,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, Some(95 * 60_000));
        assert_eq!(got[0].rows, 6_000, "it DID receive rows once — that is the diagnosis");
    }

    /// **The worse case**, and the one the handle alone can never report: a series with no entry at
    /// all. `RecorderHandle::liveness` only knows series that received something, so "never
    /// started" is only visible by diffing against what was subscribed.
    #[test]
    fn a_series_that_never_started_is_reported_distinctly() {
        let got = silent_series(&expect(&["depth/binance/BTC"]), &live(&[]), 5_000, 60_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, None, "never-started is not 'stale for 5s'");
        assert_eq!(got[0].rows, 0);
    }

    /// A series still inside the threshold is quiet, not silent — a 5-minute-window family can go
    /// a minute between rows without being a fault.
    #[test]
    fn a_pause_shorter_than_the_threshold_is_not_a_fault() {
        let got = silent_series(
            &expect(&["trade/poly/T"]),
            &live(&[("trade/poly/T", 3, 0)]),
            59_000,
            60_000,
        );
        assert!(got.is_empty());
    }

    /// A series the recorder is no longer subscribed to is IGNORED — a rotated-out Polymarket
    /// token stops receiving rows by design, and reporting it would bury the real faults.
    #[test]
    fn an_unsubscribed_series_is_not_reported() {
        let got = silent_series(
            &expect(&["trade/poly/NEW"]),
            &live(&[("trade/poly/OLD", 500, 0), ("trade/poly/NEW", 1, 9_000)]),
            10_000,
            60_000,
        );
        assert!(got.is_empty(), "only OLD is stale, and OLD is no longer expected: {got:?}");
    }

    #[test]
    fn output_is_sorted_so_a_log_line_is_stable() {
        let got = silent_series(&expect(&["b/v/s", "a/v/s", "c/v/s"]), &live(&[]), 0, 0);
        let names: Vec<&str> = got.iter().map(|s| s.series.as_str()).collect();
        assert_eq!(names, vec!["a/v/s", "b/v/s", "c/v/s"]);
    }
}

#[cfg(test)]
mod watch_tests {
    use super::tests::{expect, live};
    use super::*;

    /// **The false positive that would make this unreadable.** A just-subscribed series has
    /// received nothing BY DEFINITION; reporting it immediately fires on every startup and every
    /// Polymarket token rotation.
    #[test]
    fn a_just_subscribed_series_is_not_reported_yet() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        assert!(w.check(&e, &live(&[]), 0, 60_000).is_empty(), "t=0");
        assert!(w.check(&e, &live(&[]), 59_000, 60_000).is_empty(), "still inside the grace");
    }

    /// …but once it HAS had its grace and still never received a row, it is a real fault — a wrong
    /// stream name the venue accepted anyway.
    #[test]
    fn a_series_that_never_starts_is_reported_after_the_grace() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        let _ = w.check(&e, &live(&[]), 0, 60_000);
        let got = w.check(&e, &live(&[]), 61_000, 60_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, None);
    }

    /// The grace applies ONLY to never-started series. One that received rows and then stopped is
    /// judged on its own age, so a feed dying right after startup is still caught immediately.
    #[test]
    fn the_grace_does_not_delay_a_series_that_stopped() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        let got = w.check(&e, &live(&[("trade/binance/BTC", 10, 0)]), 61_000, 60_000);
        assert_eq!(got.len(), 1, "first tick ever, and it is already reported: {got:?}");
        assert_eq!(got[0].silent_for_ms, Some(61_000));
    }

    /// A rotated-out token is forgotten, so the map cannot grow without bound on a daemon that
    /// rotates every 5 minutes forever.
    #[test]
    fn departed_series_are_forgotten() {
        let mut w = SilenceWatch::new();
        let _ = w.check(&expect(&["trade/poly/OLD"]), &live(&[]), 0, 60_000);
        let _ = w.check(&expect(&["trade/poly/NEW"]), &live(&[]), 1_000, 60_000);
        assert_eq!(w.first_seen.len(), 1);
        assert!(w.first_seen.contains_key("trade/poly/NEW"));
    }

    /// A re-subscribed token gets a FRESH grace rather than inheriting the old one — it is a new
    /// subscription to a new stream, and judging it by when its predecessor appeared would report
    /// it instantly.
    #[test]
    fn a_returning_series_gets_a_fresh_grace() {
        let mut w = SilenceWatch::new();
        let a = expect(&["trade/poly/A"]);
        let _ = w.check(&a, &live(&[]), 0, 60_000);
        let _ = w.check(&expect(&["trade/poly/B"]), &live(&[]), 10_000, 60_000);
        assert!(w.check(&a, &live(&[]), 70_000, 60_000).is_empty(), "re-subscribed at t=70_000");
    }
}

/// The PER-SERIES notification gate: how often a silent series may PAGE, as opposed to how often it
/// is logged (every tick, unchanged).
#[cfg(test)]
mod alertable_tests {
    use super::*;

    fn silent(names: &[&str]) -> Vec<Silent> {
        names
            .iter()
            .map(|n| Silent { series: n.to_string(), silent_for_ms: Some(600_000), rows: 7 })
            .collect()
    }

    /// **The reason this gate is not `AlertRule::cooldown_ms`.** the CI box's watchdog fired for SIX
    /// series in one episode; a per-RULE cooldown would page for one of them and swallow five,
    /// which reads to an operator as a single-series fault.
    #[test]
    fn every_series_of_one_episode_alerts_not_just_the_first() {
        let mut w = SilenceWatch::new();
        let got = w.alertable(&silent(&["a/v/s", "b/v/s", "c/v/s"]), 1_000, 3_600_000);
        assert_eq!(got.len(), 3, "all three, on one tick, under one rule: {got:?}");
    }

    /// A still-silent series does not re-page every tick — the watchdog runs every 30s and an
    /// outage lasts hours.
    #[test]
    fn a_still_silent_series_does_not_repage_within_the_repeat_window() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1, "first tick pages");
        assert!(w.alertable(&s, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
        assert!(w.alertable(&s, 3_599_999, 3_600_000).is_empty(), "just inside the window");
        assert_eq!(w.alertable(&s, 3_600_000, 3_600_000).len(), 1, "the window elapsed → re-pages");
    }

    /// `repeat_ms = 0` means ONCE per episode: never re-page while it stays silent.
    #[test]
    fn a_zero_repeat_pages_once_per_episode_and_never_again_while_it_lasts() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 0).len(), 1);
        assert!(w.alertable(&s, 86_400_000, 0).is_empty(), "a day later, still one episode");
    }

    /// …but a NEW episode pages immediately, however the last one ended. Without the recovery
    /// sweep, a series that came back and died again would be silently suppressed for a whole
    /// repeat window — the failure this whole file exists to make impossible.
    #[test]
    fn recovery_rearms_so_the_next_episode_pages_at_once() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1);
        // it recovered: this tick's silent set no longer names it.
        assert!(w.alertable(&[], 1_000, 3_600_000).is_empty());
        // …and it dies again, well inside the repeat window.
        assert_eq!(w.alertable(&s, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
    }

    /// A rotated-out Polymarket token leaves the silent set forever; its bookkeeping must go with
    /// it or the map grows without bound on a daemon that rotates every 5 minutes.
    #[test]
    fn departed_series_do_not_accumulate() {
        let mut w = SilenceWatch::new();
        let _ = w.alertable(&silent(&["trade/poly/OLD"]), 0, 0);
        let _ = w.alertable(&silent(&["trade/poly/NEW"]), 1_000, 0);
        assert_eq!(w.last_alerted.len(), 1);
        assert!(w.last_alerted.contains_key("trade/poly/NEW"));
    }
}

/// The FEED-level watch: never-resolved vs resolved-then-failed, its grace, and its per-venue pager
/// gate.
#[cfg(test)]
mod resolve_watch_tests {
    use super::*;

    fn never(venue: &str) -> FeedResolve {
        FeedResolve { venue: venue.into(), last_nonempty_ms: None }
    }
    fn worked_at(venue: &str, ms: i64) -> FeedResolve {
        FeedResolve { venue: venue.into(), last_nonempty_ms: Some(ms) }
    }

    /// **The false positive that would make this unusable.** A feed is unresolved BY DEFINITION on
    /// the tick it is mounted; reporting it at once fires on every startup of every box.
    #[test]
    fn a_just_mounted_venue_is_not_reported_yet() {
        let mut w = ResolveWatch::new();
        assert!(w.check(&[never("polymarket")], 0, 300_000).is_empty(), "t=0");
        assert!(w.check(&[never("polymarket")], 299_000, 300_000).is_empty(), "inside the grace");
    }

    /// **The measured failure.** Past the grace and it has still never produced a symbol: a proxy
    /// nothing is listening behind, a family name that matches no market. It will not fix itself.
    #[test]
    fn a_venue_that_never_resolves_is_reported_after_the_grace() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[never("polymarket")], 0, 300_000);
        assert_eq!(w.check(&[never("polymarket")], 301_000, 300_000), vec!["polymarket"]);
    }

    /// ⚠ **The retry case, which must NEVER escalate.** `crate::runtime`'s module doc is why: a
    /// resolution failure changes nothing and is retried next tick, and paging for a Gamma blip is
    /// how a pager gets muted.
    #[test]
    fn a_venue_that_resolved_once_is_never_reported_however_long_it_has_been_failing() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[worked_at("polymarket", 1_000)], 1_000, 300_000);
        assert!(
            w.check(&[worked_at("polymarket", 1_000)], 86_400_000, 300_000).is_empty(),
            "a day of failing resolves, after ONE success, is still a retry — not a page"
        );
    }

    /// A three-venue box with one dead venue must name exactly that one, and name it.
    #[test]
    fn only_the_never_resolved_venues_are_reported_and_the_output_is_sorted() {
        let mut w = ResolveWatch::new();
        let feeds =
            [never("polymarket"), worked_at("binance", 500), never("aster"), worked_at("okx", 500)];
        let _ = w.check(&feeds, 0, 300_000);
        assert_eq!(w.check(&feeds, 301_000, 300_000), vec!["aster", "polymarket"]);
    }

    /// A venue dropped from the profile is forgotten, so a long-running daemon's map cannot grow
    /// without bound — and a re-added venue gets a FRESH grace rather than inheriting the old one.
    #[test]
    fn departed_venues_are_forgotten_and_a_returning_one_gets_a_fresh_grace() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[never("polymarket")], 0, 300_000);
        let _ = w.check(&[never("binance")], 1_000, 300_000);
        assert_eq!(w.first_seen.len(), 1);
        assert!(w.first_seen.contains_key("binance"));
        // polymarket comes back at t=1_000's successor: it must get its whole grace again.
        assert!(w.check(&[never("polymarket")], 2_000, 300_000).is_empty());
        assert!(w.check(&[never("polymarket")], 301_000, 300_000).is_empty(), "grace from t=2_000");
        assert_eq!(w.check(&[never("polymarket")], 303_000, 300_000), vec!["polymarket"]);
    }

    /// Every venue of one episode pages — the reason this gate is not `AlertRule::cooldown_ms`.
    #[test]
    fn every_unresolved_venue_of_one_episode_alerts_not_just_the_first() {
        let mut w = ResolveWatch::new();
        let all = ["aster".to_string(), "binance".to_string(), "polymarket".to_string()];
        assert_eq!(w.alertable(&all, 1_000, 3_600_000).len(), 3, "all three, on one tick");
    }

    /// A still-unresolved venue does not re-page every tick — the daemon ticks every 30 s.
    #[test]
    fn a_still_unresolved_venue_does_not_repage_within_the_repeat_window() {
        let mut w = ResolveWatch::new();
        let one = ["polymarket".to_string()];
        assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1, "first tick pages");
        assert!(w.alertable(&one, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
        assert_eq!(w.alertable(&one, 3_600_000, 3_600_000).len(), 1, "the window elapsed");
    }

    /// …and a recovery re-arms, so the NEXT episode pages at once rather than waiting out a window.
    #[test]
    fn a_recovered_venue_rearms_and_a_fresh_episode_pages_immediately() {
        let mut w = ResolveWatch::new();
        let one = ["polymarket".to_string()];
        assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1);
        assert!(w.alertable(&[], 1_000, 3_600_000).is_empty(), "it resolved: nothing to page");
        assert_eq!(w.alertable(&one, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
        assert_eq!(w.last_alerted.len(), 1, "…and the map does not accumulate");
    }
}

/// The FAMILY rule's MECHANISM, at the grain the integration test cannot reach.
///
/// `crates/vike-recorder/tests/family_collapse_alert.rs` drives the real `watchdog_tick` over the
/// real incident; these drive [`SilenceWatch::family_collapse`] directly, with a tick SMALLER than
/// the window, because the accumulator's three interesting moments — a member born mid-window, a
/// member dying mid-window, and a rotation that does both at once — are only visible from inside a
/// window.
#[cfg(test)]
mod family_tests {
    use super::tests::live;
    use super::*;

    const FAM: &str = "book/polymarket/btc-updown-5m";
    const OUT: &str = "trade/binance/BTCUSDT.P";
    /// Half a window, so every fixture below has an inside and an edge.
    const HALF: i64 = FAMILY_WINDOW_MS / 2;
    /// A baseline comfortably over [`MIN_BASELINE_ITEMS`], so the fixtures are judged rather than
    /// refused for want of resolution.
    const BASELINE: u64 = 20_000;

    fn fam(keys: &[&str]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            keys.iter().map(|k| (k.to_string(), FAM.to_string())).collect();
        // The licence, which every verdict needs: a series OUTSIDE the judged family.
        v.push((OUT.to_string(), OUT.to_string()));
        v
    }

    /// A running arrival record — `rows` monotonic per key, never reset and never removed, exactly
    /// as `crates/vike-data/src/live_rec.rs`'s `ingest` maintains the real one.
    #[derive(Default)]
    struct Tape {
        rows: HashMap<String, u64>,
        now: i64,
    }

    impl Tape {
        fn add(&mut self, key: &str, items: u64) {
            *self.rows.entry(key.to_string()).or_insert(0) += items;
        }
        fn live(&self) -> HashMap<String, Liveness> {
            live(&self.rows.iter().map(|(k, r)| (k.as_str(), *r, self.now)).collect::<Vec<_>>())
        }
    }

    /// Prime `watch`'s ring to FULL at [`BASELINE`], one window per tick, and return the tape.
    fn primed(watch: &mut SilenceWatch, keys: &[&str]) -> Tape {
        let f = fam(keys);
        let mut tape = Tape::default();
        for _ in 0..FAMILY_RING + 1 {
            tape.now += FAMILY_WINDOW_MS;
            for k in keys {
                tape.add(k, BASELINE / keys.len() as u64);
            }
            tape.add(OUT, 200);
            watch.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        }
        tape
    }

    /// **A member born MID-WINDOW loses no items**, and the `None`-anchor arm is EXACT rather than
    /// an approximation.
    ///
    /// `ingest` inserts `Liveness { rows: 0, last_ms: 0 }` on first sight and the counter is
    /// per-process, so a member first seen with `rows = N` has produced exactly N items in this
    /// window and nothing earlier. Taking `N` in full is therefore right; taking `0` would silently
    /// shed a whole birth's worth of a rotation's arrivals, every rotation, forever.
    #[test]
    fn a_member_born_mid_window_loses_no_items() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);

        // The judged window: A is silent, and NEWBORN arrives mid-window with 7 items already on
        // its counter. Two half-ticks, so the window closes on the second.
        let f = fam(&["A", "NEWBORN"]);
        tape.now += HALF;
        tape.add("NEWBORN", 7);
        tape.add(OUT, 100);
        assert!(w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

        tape.now += HALF;
        tape.add(OUT, 100);
        let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1, "the window collapsed and must be reported: {got:?}");
        assert_eq!(
            got[0].observed_items, 7,
            "a member born mid-window must contribute every item it has produced"
        );
        assert_eq!(got[0].members, 2, "both members are in the family this window");
    }

    /// **A member DEPARTING mid-window contributes its tail**, and the family total never goes
    /// backwards.
    ///
    /// A token leaves `expected_families` the tick the runtime unsubscribes it — before the rows it
    /// produced since the previous tick have been accounted. Collecting a departed key's final
    /// delta against its REMEMBERED family, once, on the tick it disappears, is what keeps a
    /// rotation's arithmetic exact; dropping it would shed one tick of each dying token's tail
    /// (~1 % of a window on the deployed profile) in the direction of a FALSE POSITIVE.
    #[test]
    fn a_member_departing_mid_window_contributes_its_tail() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A", "LEAVING"]);

        // LEAVING produces 11 more items and is then unsubscribed. A produces nothing.
        tape.now += HALF;
        tape.add("LEAVING", 11);
        tape.add(OUT, 100);
        let f_after = fam(&["A"]);
        assert!(w.family_collapse(&f_after, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

        tape.now += HALF;
        tape.add(OUT, 100);
        let got = w.family_collapse(&f_after, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].observed_items, 11,
            "a departing member's final delta belongs to the family it was recorded under"
        );
    }

    /// **THE REGRESSION TEST FOR THE OBVIOUS WRONG DESIGN.** The family total is a SUM OF PER-KEY
    /// DELTAS, never a delta of a sum of counters.
    ///
    /// Differencing a summed counter across a CHANGING key set reads zero-or-negative on a HEALTHY
    /// rotating family, every window, forever: two departing tokens take their entire lifetime
    /// counters out of the sum while two joiners start at zero, and the Polymarket rotation period
    /// (300 s) sits inside any window long enough to be worth measuring. `slow_series`' `anchors`
    /// idiom is the repair and it was already in this file.
    #[test]
    fn the_family_total_is_a_sum_of_per_key_deltas_not_a_delta_of_a_sum() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["OLD_UP", "OLD_DOWN"]);
        // The two outgoing members carry large cumulative counters by now.
        let summed_before: u64 = ["OLD_UP", "OLD_DOWN"].iter().map(|k| tape.rows[*k]).sum();
        assert!(summed_before > BASELINE, "the fixture must have real history to lose");

        // The rotation: both old members leave with a last 500 items each, both new ones join at
        // zero and produce 1,500 each. A delta-of-a-sum would read
        // (3,000 + 1,000) - summed_before, i.e. deeply NEGATIVE, and saturate to 0.
        let f_new = fam(&["NEW_UP", "NEW_DOWN"]);
        tape.now += HALF;
        tape.add("OLD_UP", 500);
        tape.add("OLD_DOWN", 500);
        tape.add("NEW_UP", 700);
        tape.add("NEW_DOWN", 700);
        tape.add(OUT, 100);
        assert!(w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());

        tape.now += HALF;
        tape.add("NEW_UP", 800);
        tape.add("NEW_DOWN", 800);
        tape.add(OUT, 100);
        let got = w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        // 500 + 500 (the departing tails) + 1,500 + 1,500 (the newborns) = 4,000, which is 20 % of
        // BASELINE — a healthy rotation dip, NOT a collapse, so nothing fires and the arithmetic is
        // read out of the next window instead.
        assert!(got.is_empty(), "a healthy rotation must not be reported as a collapse: {got:?}");

        // Now a genuinely dark window, whose verdict carries the running total as evidence.
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&f_new, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].observed_items, 0, "and a dark window is zero, never a wrapped negative");
    }

    /// A rotation in which the family total holds STEADY is judged steady — the property the whole
    /// design rests on, at the mechanism level. Two members leave and two join inside one window,
    /// and the total is unchanged, so no verdict is taken and the ring learns the window normally.
    #[test]
    fn a_rotation_that_holds_the_total_steady_is_invisible() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["OLD_UP", "OLD_DOWN"]);

        for r in 0..6 {
            let old = (format!("R{r}_UP"), format!("R{r}_DOWN"));
            let f = fam(&[&old.0, &old.1]);
            tape.now += FAMILY_WINDOW_MS;
            tape.add(&old.0, BASELINE / 2);
            tape.add(&old.1, BASELINE / 2);
            tape.add(OUT, 200);
            let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
            assert!(got.is_empty(), "rotation {r} was reported as a collapse: {got:?}");
        }
    }

    /// A window with NO out-of-family witness takes no verdict, even though the family is dark —
    /// the whole-process stall, which the recency watchdog owns and which fires first.
    #[test]
    fn a_window_with_no_out_of_family_activity_takes_no_verdict() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);
        tape.now += FAMILY_WINDOW_MS;
        // Nothing at all arrives — not the family, not the licence.
        let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert!(got.is_empty(), "a stalled process must not be reported as a dead family: {got:?}");
    }

    /// A family whose baseline is under [`MIN_BASELINE_ITEMS`] is never judged, whatever it does.
    /// The resolution gate, at the mechanism level.
    #[test]
    fn a_baseline_below_the_resolution_gate_is_never_judged() {
        let mut w = SilenceWatch::new();
        let f = fam(&["A"]);
        let mut tape = Tape::default();
        for _ in 0..FAMILY_RING + 4 {
            tape.now += FAMILY_WINDOW_MS;
            tape.add("A", MIN_BASELINE_ITEMS / 40);
            tape.add(OUT, 200);
            assert!(w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty());
        }
        for _ in 0..3 {
            tape.now += FAMILY_WINDOW_MS;
            tape.add(OUT, 200);
            let got = w.family_collapse(&f, &tape.live(), tape.now, FAMILY_WINDOW_MS);
            assert!(got.is_empty(), "a thin family must be refused a verdict: {got:?}");
        }
    }

    /// `expected_families` as it reads on a tick where the family resolved to ZERO symbols — only
    /// the out-of-family witness is left. `crates/vike-recorder/src/runtime.rs`'s `tick` documents
    /// that state as a real failure ("a family that resolves to zero symbols is the quieter of the
    /// two failures this field exists for"), and unlike an `Err` it does not `continue`.
    fn only_out() -> Vec<(String, String)> {
        vec![(OUT.to_string(), OUT.to_string())]
    }

    /// **A family that blinks out of the profile for ONE tick keeps its baseline.**
    ///
    /// The state used to die on the first tick a family was absent, so an `Ok(empty)` market-list
    /// response — one tick — discarded a full ring and the family was UNJUDGED for the next
    /// [`FAMILY_RING`] windows, silently. Ten minutes blind, with nothing in the log to say so, on
    /// a failure adjacent to the very incident class this rule targets.
    #[test]
    fn a_family_absent_for_one_tick_keeps_its_baseline() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);

        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        assert!(
            w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS).is_empty(),
            "a family with no members this window has no honest total, so it takes no verdict"
        );

        // …and it is back on the very next tick, dark.
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1, "the blink must not have discarded the ring: {got:?}");
        assert_eq!(got[0].ring_windows, FAMILY_RING, "…and the ring is still the one it learned");
        assert_eq!(got[0].baseline_items, BASELINE, "…against the baseline it already had");
    }

    /// **A member that blinks out contributes its DELTA on return, never its whole lifetime.**
    ///
    /// The departed pass used to forget a member's anchor outright, so a member absent for one tick
    /// hit the first-sight arm on its return and booked its entire `Liveness::rows` counter — a
    /// Polymarket token's whole ~600 s life — into that one window, which then TAUGHT the ring.
    /// Fixed beside the grace above rather than after it: keeping a ring alive across a blink while
    /// letting the blink poison it would be half a repair.
    #[test]
    fn a_member_that_blinks_out_contributes_its_delta_not_its_lifetime_on_return() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);
        let lifetime = tape.rows["A"];
        assert!(lifetime > BASELINE * 10, "the fixture must have a lifetime worth over-counting");

        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let _ = w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS);

        // A is back and has produced NOTHING since. With its anchor kept, that window is zero and
        // fires; with the anchor forgotten it would read `lifetime` and be the busiest window the
        // family ever had.
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1, "a dark window after a blink is still a dark window: {got:?}");
        assert_eq!(got[0].observed_items, 0, "a returning member is re-anchored, not re-counted");
    }

    /// **…and an absence longer than the ring's own span DOES discard it** — the grace is one ring
    /// length precisely because every entry in a ring older than that describes a window the ring
    /// no longer claims to cover. See [`FAMILY_ABSENCE_GRACE_MS`].
    ///
    /// ⚠ **The returning member is a NEW token, and that is what makes this test mean anything.**
    /// A first draft brought the SAME member back, and it passed whether or not the expiry fired:
    /// by then that member's anchor had been pruned on the same clock, so its return booked its
    /// whole lifetime, the window read as enormous rather than dark, and no verdict was taken for a
    /// reason that had nothing to do with the ring. Mutating the expiry to `i64::MAX` left it green
    /// — a vacuous test, caught only by mutating the thing it claimed to pin. A fresh token is also
    /// the realistic fixture: ten minutes is two Polymarket rotation periods, so the family that
    /// comes back does not hold the members that left.
    #[test]
    fn a_family_absent_past_the_grace_relearns_from_scratch() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);

        for _ in 0..FAMILY_ABSENCE_GRACE_MS / FAMILY_WINDOW_MS + 1 {
            tape.now += FAMILY_WINDOW_MS;
            tape.add(OUT, 200);
            let _ = w.family_collapse(&only_out(), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        }

        // The family is back, with a token born during the absence, and dark. A kept ring would
        // judge that against the regime of ten minutes ago and fire.
        let back = fam(&["B"]);
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert!(
            got.is_empty(),
            "a ring older than the span it claims to describe must be discarded, not re-used: \
             {got:?}"
        );

        // …and it RELEARNS rather than being permanently dead: a full ring at the new regime, then
        // a dark window, and it judges again — against the baseline it just learned.
        for _ in 0..FAMILY_RING {
            tape.now += FAMILY_WINDOW_MS;
            tape.add("B", BASELINE / 2);
            tape.add(OUT, 200);
            let _ = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        }
        tape.now += FAMILY_WINDOW_MS;
        tape.add(OUT, 200);
        let got = w.family_collapse(&back, &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1, "a relearned family must be judged again: {got:?}");
        assert_eq!(
            got[0].baseline_items,
            BASELINE / 2,
            "…against the regime it learned on its return, not the one it left"
        );
    }

    /// **A witness reduced to disconnect MARKERS cannot licence a verdict** — the whole-process
    /// stall this rule declares it withholds on, at the mechanism level.
    ///
    /// The licence was `licence_items > 0` over raw `Liveness::rows`, which counts the
    /// `GapStart`/`Stale`/`LiveResume` rows `crates/vike-data/src/live_rec.rs`'s `stream_status`
    /// writes — rows emitted BECAUSE data stopped. One marker from one other lane therefore
    /// licensed a verdict during a host-wide network loss, and the alert body asserted "this
    /// recorder was still receiving data" on the strength of a disconnect.
    #[test]
    fn a_witness_reduced_to_markers_cannot_licence_a_verdict() {
        let mut w = SilenceWatch::new();
        let mut tape = primed(&mut w, &["A"]);

        // The host loses the network: the family falls to its own handful of markers and the
        // witness to a couple of its own. Non-zero on both sides, which is the case the old
        // `> 0` predicate could not tell from health.
        tape.now += FAMILY_WINDOW_MS;
        tape.add("A", 1);
        tape.add(OUT, 2);
        let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert!(got.is_empty(), "a stalled process must not vouch for itself: {got:?}");

        // …and the SAME dark family fires the moment the witness is genuinely writing again, so
        // this is a licence test rather than a rule that stopped working.
        tape.now += FAMILY_WINDOW_MS;
        tape.add("A", 1);
        tape.add(OUT, 200);
        let got = w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, FAMILY_WINDOW_MS);
        assert_eq!(got.len(), 1, "a live witness licenses the same window: {got:?}");
        assert_eq!(got[0].licence, OUT);
    }

    /// A non-positive window is inert, matching [`SilenceWatch::slow_series`]' own guard.
    #[test]
    fn a_non_positive_window_judges_nothing() {
        let mut w = SilenceWatch::new();
        let mut tape = Tape { now: 1_000, ..Default::default() };
        tape.add("A", 5);
        assert!(w.family_collapse(&fam(&["A"]), &tape.live(), tape.now, 0).is_empty());
    }
}
