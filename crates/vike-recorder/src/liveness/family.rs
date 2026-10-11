//! The FAMILY rule: a rotating family whose arrival total collapsed against its own history.

use std::collections::{BTreeMap, HashMap, VecDeque};

use vike_data::Liveness;

use super::{SilenceWatch, due_now};

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
/// not either, and correctly: `vike_data::store::series_cadence` classifies that lane
/// `Cadence::EventDriven` — the market sets the rate — so `ceiling_per_s` returns `None` and there
/// is nothing to judge against. That refusal is right and this rule does not touch it.
///
/// Every field is a COUNT or a name; there is deliberately no rate anywhere on this type. See
/// [`SilenceWatch::family_collapse`] for the whole rule and what it cannot see.
///
/// [`Silent`]: super::Silent
/// [`Slow`]: super::slow::Slow
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
///
/// [`CADENCE_WINDOW_MS`]: super::slow::CADENCE_WINDOW_MS
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
///
/// [`CADENCE_FLOOR_FRACTION`]: super::slow::CADENCE_FLOOR_FRACTION
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
pub(super) struct MemberAnchor {
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
///
/// [`judge`]: super::slow::judge
fn median(ring: &VecDeque<u64>) -> u64 {
    if ring.is_empty() {
        return 0;
    }
    let mut v: Vec<u64> = ring.iter().copied().collect();
    v.sort_unstable();
    v[v.len() / 2]
}

impl SilenceWatch {
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
    ///    mirrors `vike_data::store::series_cadence`'s `ceiling_per_s` returning `None`;
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
    ///   self-reference, and `crates/vike-data/src/store/series_cadence.rs`'s binance `depth` row states
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
    ///
    /// [`FeedResolve`]: super::resolve_watch::FeedResolve
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
                // The `None` arm is EXACT, not an approximation: `crates/vike-data/src/rec/live_rec.rs`'s
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
            // MARKERS `crates/vike-data/src/rec/live_rec.rs`'s `stream_status` writes as ordinary rows
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

/// The FAMILY rule's MECHANISM, at the grain the integration test cannot reach.
///
/// `crates/vike-recorder/tests/family_collapse_alert.rs` drives the real `watchdog_tick` over the
/// real incident; these drive [`SilenceWatch::family_collapse`] directly, with a tick SMALLER than
/// the window, because the accumulator's three interesting moments — a member born mid-window, a
/// member dying mid-window, and a rotation that does both at once — are only visible from inside a
/// window.
#[path = "family_tests.rs"]
#[cfg(test)]
mod family_tests;
