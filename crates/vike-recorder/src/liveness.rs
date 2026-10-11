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
//! `crates/vike-data/src/store/series_cadence.rs`'s `SERIES_CADENCE`; how a floor is derived from it, and
//! why the sibling trade tape has a vote, is on [`SilenceWatch::slow_series`].
//!
//! Pure and clock-injected: `now_ms` is a parameter, so the tests own time.
//!
//! [`ResolveWatch`]: resolve_watch::ResolveWatch

pub mod family;
pub mod resolve_watch;
pub mod slow;

use std::collections::{HashMap, HashSet, VecDeque};

use vike_data::Liveness;

use family::MemberAnchor;

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
    ///
    /// [`FAMILY_ABSENCE_GRACE_MS`]: family::FAMILY_ABSENCE_GRACE_MS
    family_absent_since: HashMap<String, i64>,
    /// Items accumulated so far in the OPEN window, per family key. Zeroed when a window closes.
    family_items: HashMap<String, u64>,
    /// The last [`FAMILY_RING`] completed windows' item counts, per family — the learned baseline.
    ///
    /// [`FAMILY_RING`]: family::FAMILY_RING
    family_ring: HashMap<String, VecDeque<u64>>,
    /// When a family's ring stopped learning, per family — the freeze, and the clock
    /// [`FAMILY_FREEZE_MAX_MS`] is measured against. Absent while the family is teaching normally.
    ///
    /// [`FAMILY_FREEZE_MAX_MS`]: family::FAMILY_FREEZE_MAX_MS
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
    ///
    /// [`FAMILY_WINDOW_MS`]: family::FAMILY_WINDOW_MS
    /// [`CADENCE_WINDOW_MS`]: slow::CADENCE_WINDOW_MS
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
}

#[path = "liveness_tests.rs"]
#[cfg(test)]
mod liveness_tests;
