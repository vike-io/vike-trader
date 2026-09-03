//! Bounded CONCURRENCY for a paged REST backfill, with the venue's rate budget still enforced **in
//! aggregate**.
//!
//! ## The problem, MEASURED
//! A sequential pager cannot space requests closer together than one request takes. MEASURED on
//! the CI box 2026-08-04, a pooled-agent binance kline page costs ~280 ms, so ~92 % of a multi-minute
//! backfill's wall clock is one thread waiting on a socket. Delay tuning cannot touch that: on
//! binance SPOT the pacer's own target gap is 50 ms and [`crate::pacer::Pacer::next_delay`] is
//! ALREADY floored — the operator asked for 40 % of a 6000 weight/min budget and gets ~18 % of it,
//! with no knob left to turn. The only lever is more than one request in flight.
//!
//! ## The whole safety argument, in one sentence
//! **Concurrency here does not raise the target rate — it is what makes the existing target
//! reachable.** One [`crate::pacer::Pacer`] is shared by every lane behind [`LaneGate`], which hands
//! out DISPATCH SLOTS spaced exactly [`crate::pacer::Pacer::target_gap`] apart. The venue therefore
//! sees the same request rate at 6 lanes as at 1; what changes is only that the rate is now
//! *achieved*, because a lane waiting on a socket is no longer also the thing holding the schedule.
//! The lane COUNT is likewise derived from the pacer
//! ([`crate::pacer::Pacer::suggested_lanes`] = `ceil(request_time / target_gap)`), never from a
//! constant — and it is `1` for any venue whose budget was not discovered, because there is no
//! published ceiling there for an aggregate to be a fraction of.
//!
//! [`MAX_LANES`] bounds THREADS, not rate. Raising it cannot raise consumption: the gate still
//! admits one request per target gap. It exists so an absurd ratio (a 30-second round trip against a
//! 1 ms gap) cannot spawn thirty thousand OS threads and thirty thousand sockets.
//!
//! ## Splitting the WINDOW, never the page grid
//! [`split_range`] cuts `[start_ms, end_ms]` into contiguous, disjoint, inclusive spans and each lane
//! runs the venue's OWN unmodified pager over its span. That is deliberately not "precompute every
//! page boundary": a page grid assumes the venue serves rows on a fixed grid and page-size cap, and
//! for the END-ANCHORED venues (bybit/deribit page BACKWARD from what the last page returned — see
//! #1030/#1039) a wrongly-computed window silently TRUNCATES, which is the exact bug those PRs fixed.
//! A span split has no such assumption: every one of these pagers is already correct for an arbitrary
//! caller-supplied `[start, end]`, so running it on a sub-window is the same code answering a
//! narrower question, forward or backward.
//!
//! ## Failure policy: the whole fetch fails
//! [`run_lanes`] returns the FIRST error by span order and discards every lane's rows. It does not
//! return a partial result, and it does not retry a failed span at this layer (each venue's page
//! fetch already owns a bounded 429/418 `Retry-After` retry). The reason is the caller: a kline
//! backfill writes under a commit key naming the WHOLE window
//! (`{venue}:{symbol}:{interval}:{start}-{end}`), so a partial result reported as success marks that
//! window permanently ingested and the missing rows are never fetched again. "A partial backfill
//! reported as success" is a defect class this repo has already hit; a lane that fails must fail the
//! call exactly as a page that fails fails the sequential loop.
//!
//! A failure also STOPS the other lanes rather than letting them run the window out:
//! [`Lanes::aborted`] flips, every lane checks it between pages, and the in-flight requests drain.
//! Nothing is cancelled mid-request (blocking `ureq` has no cancel), so the cost of a failure is
//! bounded by one round trip per lane.
//!
//! ## What this module does NOT do
//! No async runtime, no `rayon`, no new dependency. `std::thread::scope` + one `Mutex` is the whole
//! machinery, matching the workspace's deliberately blocking bridge layer (`ureq`, `tungstenite`) and
//! the existing scoped-thread precedent in `vike_backfill`'s `events_api_backfill` bin. `rayon` IS a
//! workspace dep, but it is a CPU work-stealing pool sized to core count: these lanes are
//! socket-blocked, not CPU-bound, and their count comes from a latency ratio rather than from
//! `available_parallelism`, so a rayon pool would be both the wrong size and the wrong shape (and
//! would add a dep edge to every bridge crate).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::pacer::Pacer;

/// Ceiling on lanes — a bound on OS THREADS and open sockets, **not on rate**.
///
/// The rate is `1 / target_gap` whatever this is: [`LaneGate`] admits one request per target gap no
/// matter how many threads are waiting, so raising this number cannot raise consumption by a single
/// weight unit. It exists because [`crate::pacer::Pacer::suggested_lanes`] is a RATIO, and a
/// degenerate one (a 30-second round trip against the 1 ms delay floor) would otherwise ask for
/// thirty thousand threads.
///
/// 8 covers every measured shape with headroom — the widest real ratio today is binance SPOT's
/// `ceil(280 ms / 50 ms)` = 6 — while staying at a socket count a venue's per-IP connection limits
/// are comfortable with. A caller running lanes must size its HTTP pool to match (ureq defaults to
/// 3 idle connections per host); `http::blocking_agent_for_lanes` is that builder. It is named
/// rather than linked here because `http` rides the `full` feature and this module does not.
pub const MAX_LANES: usize = 8;

/// Cut `[start_ms, end_ms]` (inclusive, as every kline fetcher takes it) into at most `lanes`
/// contiguous, disjoint, inclusive spans that together cover it exactly.
///
/// Properties every caller depends on, and the tests pin:
/// * **disjoint** — span `i` ends at `span[i+1].0 - 1`, so a bar's timestamp lands in exactly one
///   span and the concatenated result needs no cross-span dedup to be correct;
/// * **covering** — `spans.first().0 == start_ms` and `spans.last().1 == end_ms`, so no sliver of the
///   window is silently dropped between two lanes;
/// * **ascending** — span `i` is entirely older than span `i+1`, so concatenating each lane's already
///   ascending output is itself ascending and the merge is a no-op sort.
///
/// `lanes <= 1`, or a window too narrow to split, yields ONE span equal to the input — which is what
/// makes a one-lane run the caller's existing single-window call, unchanged. `end_ms < start_ms` is
/// an empty window and yields NO spans, so a caller never touches the venue for it. `lanes` above
/// [`MAX_LANES`] is CLAMPED to it rather than honoured: this is a `pub` helper, the module's bound on
/// lanes is [`MAX_LANES`], and an unclamped `usize::MAX` would ask for an allocation that aborts the
/// process. Clamping cannot change the rate — [`LaneGate`] admits one request per target gap however
/// many spans exist.
pub fn split_range(start_ms: i64, end_ms: i64, lanes: usize) -> Vec<(i64, i64)> {
    if end_ms < start_ms {
        return Vec::new();
    }
    // `width` as i128: `end - start` can exceed i64 for extreme bounds, and the whole point of this
    // function is that it never produces a wrapped (and therefore overlapping or inverted) span.
    let width = end_ms as i128 - start_ms as i128 + 1;
    let lanes = lanes.clamp(1, MAX_LANES) as i128;
    if lanes <= 1 || width <= lanes {
        return vec![(start_ms, end_ms)];
    }
    let each = width / lanes; // >= 1, since width > lanes
    let mut spans = Vec::with_capacity(lanes as usize);
    let mut cursor = start_ms as i128;
    for i in 0..lanes {
        // The LAST span absorbs the division remainder and is pinned to `end_ms` exactly, so the
        // cover property holds without any rounding argument.
        let span_end = if i == lanes - 1 { end_ms as i128 } else { cursor + each - 1 };
        spans.push((cursor as i64, span_end as i64));
        cursor = span_end + 1;
    }
    spans
}

/// The shared state one [`LaneGate`] guards. Split out so the lock's whole contents are one type and
/// nothing can be read without the other.
struct GateState {
    /// The ONE pacer every lane observes into and paces against. Shared rather than per-lane
    /// precisely so the budget is enforced in AGGREGATE: N pacers each targeting the venue's budget
    /// would spend N times it.
    pacer: Pacer,
    /// The instant the NEXT request may be dispatched. `None` before the first admission (the first
    /// request goes immediately, exactly as the sequential pager's first page does).
    next_slot: Option<Instant>,
}

/// The aggregate admission gate: hands out dispatch slots spaced [`crate::pacer::Pacer::target_gap`]
/// apart, to however many lanes ask.
///
/// This is the thing that makes concurrency rate-NEUTRAL. A per-lane pacer would let each lane spend
/// the whole budget; a shared *slot reservation* lets the lanes between them spend it once. Reserving
/// the slot and sleeping to it are deliberately two steps — the lock is released before the sleep, so
/// a lane waiting its turn never blocks another lane from taking the slot after it.
///
/// ## The weight estimate is MEASURED sequentially and then FROZEN
/// ⚠ An earlier draft of this doc argued the opposite — that feeding every lane's counter reading
/// into [`crate::pacer::Pacer::observe`] was fine because "the MEAN delta is the true per-request
/// weight in steady state". That is wrong, and the error is one-directional. Two effects compound:
/// a reading is sampled at the SERVER, so a delta between two of our readings covers however many of
/// our requests the venue handled in between (roughly the lane count); and responses arrive OUT OF
/// ORDER, so the values we read are a shuffle of a monotone sequence, whose positive excursions sum
/// to MORE than its net increase because `observe` drops the backward steps as window rolls. The
/// mean of the surviving deltas is therefore not `w` — it is inflated by about the lane count, which
/// widens [`crate::pacer::Pacer::target_gap`] by the same factor and hands back exactly the
/// throughput the lanes were added to gain. Past a large enough delta it saturates at the pacer's
/// `MAX_DELAY_SECS` ceiling, i.e. a stall.
///
/// So the gate uses [`crate::pacer::Pacer::observe_absolute`]: the reading lands in the counter, and
/// nothing is inferred from it. `per_request` keeps the value the SEQUENTIAL probe phase measured
/// with exactly one request outstanding — the only condition under which a delta means anything.
/// That is also why the caller must probe TWO pages before splitting: one reading is not a delta, so
/// a single probe leaves `per_request` at the pacer's pessimistic seed.
///
/// The hard guards are untouched, and they are the ones that actually prevent a 429/418 — the
/// venue-agnostic `weight_soft_limit` and [`crate::pacer::Pacer::should_cool_down`], both consulted
/// in [`LaneGate::complete`] and both comparing an ABSOLUTE reading against an absolute threshold,
/// attributing nothing to any request. They pause EVERY lane at once by pushing the shared slot
/// forward. `observe_absolute`'s doc carries the residual this trade accepts (a mid-flight re-price
/// is caught by the cooldown rather than by the delta).
pub struct LaneGate {
    inner: Mutex<GateState>,
}

impl LaneGate {
    /// Take ownership of the pacer the lanes will share. Build it from the venue's own discovery, and
    /// seed/observe it BEFORE handing it over — a gate is for the concurrent phase only.
    pub fn new(pacer: Pacer) -> Self {
        LaneGate { inner: Mutex::new(GateState { pacer, next_slot: None }) }
    }

    /// Lock, tolerating poisoning.
    ///
    /// A lane that panics mid-request would otherwise poison the mutex and turn one lane's bug into
    /// every lane's `unwrap` panic — noise on top of a failure that `std::thread::scope` already
    /// propagates on its own. The guarded state is two plain values with no invariant a panic can
    /// break halfway (a `Pacer` is arithmetic; `next_slot` is an `Instant`), so recovering it is
    /// sound rather than merely convenient.
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserve the next dispatch slot and BLOCK until it arrives. Call immediately before each
    /// request, from any lane.
    ///
    /// The reservation is what bounds the aggregate: the slot moves forward by one target gap per
    /// admission, so K admissions span `K * target_gap` no matter how they are distributed across
    /// lanes. The first admission is immediate (`next_slot` is `None`), matching the sequential
    /// pager, which also never sleeps before its first page.
    pub fn admit(&self) {
        let slot = {
            let mut g = self.lock();
            let gap = g.pacer.target_gap();
            let now = Instant::now();
            // `max(now)`: a gate that idled (every lane was busy for longer than the gap) must not
            // bank up the unused slots and release a burst — it resumes from now, one gap at a time.
            let slot = g.next_slot.map_or(now, |s| s.max(now));
            g.next_slot = Some(slot + gap);
            slot
        }; // lock released BEFORE the sleep — a waiting lane never holds up the queue behind it.
        let wait = slot.saturating_duration_since(Instant::now());
        if wait > Duration::ZERO {
            std::thread::sleep(wait);
        }
    }

    /// Report one COMPLETED request: its wall clock and (where the venue sends one) its cumulative
    /// used-weight counter. The concurrent twin of the sequential pager's
    /// `pacer.observe_request(..)` + proactive-cooldown pair, folded into one call because under
    /// concurrency the cooldown must be applied to the SHARED schedule, not slept per lane.
    ///
    /// The cooldown is `next_slot = max(next_slot, now + cooldown)` — deliberately NOT
    /// `next_slot + cooldown`. Every lane completing while the counter sits above the threshold would
    /// otherwise stack a cooldown each, multiplying a 10-second pause into a minute; this shape says
    /// "no request before `cooldown` from now" once, however many lanes say it.
    ///
    /// ⚠ The counter goes to [`crate::pacer::Pacer::observe_absolute`], **not**
    /// [`crate::pacer::Pacer::observe`]: with several requests outstanding a delta cannot be
    /// attributed to one of them, and feeding it in inflates `per_request` — and therefore the gate's
    /// own gap — by about the lane count. See this type's doc. The DURATION is fed normally; a lane
    /// times its own request, which is unambiguous however many are in flight.
    pub fn complete(
        &self,
        used_weight: Option<u64>,
        elapsed: Duration,
        weight_soft_limit: u64,
        weight_cooldown: Duration,
    ) {
        let mut g = self.lock();
        // `None` for the weight: the duration EWMA and the sample count are what this call feeds the
        // pacer. The counter follows separately, absolutely, and without inference.
        g.pacer.observe_request(None, elapsed);
        if let Some(w) = used_weight {
            g.pacer.observe_absolute(w);
        }
        // The two independent guards the sequential pager already applies, unchanged in meaning: the
        // venue-agnostic hand-set soft limit, and the pacer's own "this window's share is spent".
        // Both read an ABSOLUTE counter value, so neither depends on attributing a delta to a lane —
        // which is exactly why they still work under the freeze above, and why they are the guards
        // that actually stop a 429.
        if used_weight.is_some_and(|w| w >= weight_soft_limit) || g.pacer.should_cool_down() {
            let floor = Instant::now() + weight_cooldown;
            g.next_slot = Some(g.next_slot.map_or(floor, |s| s.max(floor)));
        }
    }

    /// A snapshot of the shared pacer, for a caller that wants to log the pace or persist the run's
    /// measurement. Cheap — `Pacer` is plain arithmetic state.
    pub fn pacer(&self) -> Pacer {
        self.lock().pacer.clone()
    }
}

/// The lane-shared control flag handed to each lane body.
///
/// One method today, and it is the whole failure policy on the lane side: a lane checks
/// [`Lanes::aborted`] between pages and returns early once another lane has failed, so a failed
/// backfill costs at most one more round trip per lane instead of running every remaining span out.
pub struct Lanes {
    abort: AtomicBool,
}

impl Lanes {
    /// Has another lane already failed? Check between pages; do not check mid-request (a blocking
    /// `ureq` call cannot be cancelled, and pretending otherwise would only hide the real cost).
    ///
    /// `Relaxed` is the right ordering: this flag guards nothing but itself. A lane that reads a
    /// stale `false` merely fetches one more page, which is the same page it would have fetched had
    /// the failure landed a microsecond later — and the failing lane's error reaches the caller
    /// through the scope join, which is a full synchronisation point.
    pub fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
}

/// Run one `body` per span, concurrently, and return their rows concatenated **in span order**.
///
/// `body` receives the shared [`Lanes`] flag and its own inclusive `(start_ms, end_ms)`. Because
/// [`split_range`]'s spans are ascending and disjoint, and every venue pager returns its own window
/// ascending, the concatenation is already ascending — a caller that additionally sorts and dedups
/// (as `walk_backward_pages` does) is buying insurance, not correcting an ordering.
///
/// **Any lane error fails the whole call** and discards every lane's rows — see the module doc's
/// failure policy. The error returned is the one from the LOWEST span index that failed, so a given
/// broken window reports the same message whichever lane happened to notice first: a concurrent
/// backfill must not be non-deterministic about WHY it failed.
///
/// A single span (the `lanes <= 1` case) still spawns no thread — it runs `body` inline on the
/// caller's thread, which is what makes a one-lane run indistinguishable from the sequential path
/// rather than merely equivalent to it.
pub fn run_lanes<T, F>(spans: &[(i64, i64)], body: F) -> Result<Vec<T>, String>
where
    T: Send,
    F: Fn(&Lanes, i64, i64) -> Result<Vec<T>, String> + Sync,
{
    let lanes = Lanes { abort: AtomicBool::new(false) };
    match spans {
        [] => return Ok(Vec::new()),
        // ONE span ⇒ no thread, no channel, no join: the caller's own thread runs the same body over
        // the same window it would have run sequentially.
        [(start, end)] => return body(&lanes, *start, *end),
        _ => {}
    }

    let results: Vec<Result<Vec<T>, String>> = std::thread::scope(|scope| {
        let lanes = &lanes;
        let body = &body;
        let handles: Vec<_> = spans
            .iter()
            .map(|&(start, end)| {
                scope.spawn(move || {
                    let out = body(lanes, start, end);
                    if out.is_err() {
                        // Flip BEFORE returning, so sibling lanes stop at their next page boundary
                        // rather than after the whole scope has joined.
                        lanes.abort.store(true, Ordering::Relaxed);
                    }
                    out
                })
            })
            .collect();
        // A lane PANIC is re-raised on this thread with its original payload, deliberately not
        // swallowed into an `Err`: a panic in a pager is a bug, and converting it to a fetch error
        // would make it indistinguishable from a venue that returned a 500.
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|payload| std::panic::resume_unwind(payload)))
            .collect()
    });

    let mut out = Vec::new();
    for r in results {
        // First error by SPAN index wins — `results` is in span order because `handles` is.
        out.push(r?);
    }
    Ok(out.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_discovery::WeightBudget;

    /// binance SPOT, the shape concurrency exists for: 6000 weight/min, weight-2 pages, the 0.40
    /// default ⇒ a 50 ms target gap against a ~280 ms round trip.
    fn spot_pacer() -> Pacer {
        let mut p = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
        p.observe_request(Some(100), Duration::from_millis(280));
        p.observe_request(Some(102), Duration::from_millis(280)); // weight 2
        p
    }

    // ----- split_range --------------------------------------------------------------------------

    #[test]
    fn spans_are_disjoint_ascending_and_cover_the_window_exactly() {
        for lanes in 1..=8usize {
            for (start, end) in [(0i64, 999i64), (1_700_000_000_000, 1_700_086_400_000), (-50, 50)]
            {
                let spans = split_range(start, end, lanes);
                assert!(!spans.is_empty(), "lanes={lanes} {start}..{end}");
                assert_eq!(spans.first().unwrap().0, start, "the cover starts at start_ms");
                assert_eq!(spans.last().unwrap().1, end, "...and ends at end_ms");
                for w in spans.windows(2) {
                    assert_eq!(w[1].0, w[0].1 + 1, "disjoint AND contiguous: {spans:?}");
                }
                for (s, e) in &spans {
                    assert!(s <= e, "no inverted span: {spans:?}");
                }
                assert!(spans.len() <= lanes);
            }
        }
    }

    /// The one-lane case IS the caller's own window — which is what lets the lane path dispatch to
    /// the sequential one without a second code path.
    #[test]
    fn one_lane_is_the_whole_window_unchanged() {
        assert_eq!(split_range(10, 20, 1), vec![(10, 20)]);
        assert_eq!(split_range(10, 20, 0), vec![(10, 20)], "zero lanes is one lane, never none");
        // A window too narrow to split does not produce empty or inverted spans.
        assert_eq!(split_range(10, 10, 8), vec![(10, 10)]);
        assert_eq!(split_range(10, 13, 8), vec![(10, 13)]);
    }

    #[test]
    fn an_empty_window_yields_no_spans_so_the_venue_is_never_touched() {
        assert!(split_range(20, 10, 4).is_empty());
        assert!(split_range(i64::MAX, i64::MIN, 8).is_empty());
    }

    /// An absurd lane count is CLAMPED, not honoured — a `pub` helper must not be talked into an
    /// allocation that aborts the process. The clamp changes no rate: the gate still admits one
    /// request per target gap however many spans came back.
    #[test]
    fn an_absurd_lane_count_clamps_instead_of_allocating() {
        let spans = split_range(i64::MIN, i64::MAX, usize::MAX);
        assert_eq!(spans.len(), MAX_LANES);
        assert_eq!(spans.first().unwrap().0, i64::MIN);
        assert_eq!(spans.last().unwrap().1, i64::MAX);
        assert_eq!(split_range(0, 999, 1_000_000).len(), MAX_LANES);
    }

    /// Extreme bounds must not wrap into an overlapping or inverted span — the arithmetic is i128
    /// for exactly this.
    #[test]
    fn a_full_width_window_does_not_wrap() {
        let spans = split_range(i64::MIN, i64::MAX, 4);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans.first().unwrap().0, i64::MIN);
        assert_eq!(spans.last().unwrap().1, i64::MAX);
        for w in spans.windows(2) {
            assert_eq!(w[1].0, w[0].1 + 1);
            assert!(w[0].0 < w[0].1);
        }
    }

    /// The remainder lands in the LAST span, and the cover is exact rather than approximately right.
    #[test]
    fn the_division_remainder_lands_in_the_final_span() {
        // 10 units over 3 lanes: 3 + 3 + 4.
        assert_eq!(split_range(0, 9, 3), vec![(0, 2), (3, 5), (6, 9)]);
    }

    // ----- LaneGate: the aggregate rate ---------------------------------------------------------

    /// The pacer the gate is handed answers the operator's target, and it is the number the
    /// aggregate must not exceed: 40 % of binance spot's published 6000 weight/min, at the measured
    /// weight-2 page cost, is a 50 ms gap ⇒ 2400 weight/min.
    #[test]
    fn the_gate_paces_on_the_target_gap_not_on_the_floored_sleep() {
        let p = spot_pacer();
        let gap = p.target_gap().as_secs_f64();
        assert!((gap - 0.050).abs() < 1e-6, "{gap}");
        // The SLEEP the sequential pager would take is floored — which is the whole problem: it
        // cannot express a 50 ms spacing when the request alone costs 280 ms.
        assert!(p.next_delay() <= Duration::from_millis(1), "the sequential sleep is floored");
        let per_min = (60.0 / gap) * 2.0;
        assert!((per_min - 2400.0).abs() < 1.0, "{per_min} weight/min is 40% of 6000");
        assert!(per_min <= 6000.0, "and stays under the venue's published ceiling");
    }

    /// THE budget property, and the reason a shared gate exists at all: K admissions occupy
    /// `K * target_gap` of wall clock **however many lanes take them**. A per-lane pacer would let
    /// each of L lanes spend the whole budget; the shared reservation lets them spend it once.
    ///
    /// Asserted as a LOWER bound on elapsed time (the direction that cannot flake: a loaded box makes
    /// it slower, never faster), and separately on the reservation schedule, which is exact.
    #[test]
    fn admissions_occupy_one_target_gap_each_whatever_the_lane_count() {
        // A small gap keeps the test quick while staying far above scheduler noise: 20 ms x 24
        // admissions = ~460 ms of mandated spacing.
        let gap = Duration::from_millis(20);
        const ADMITS_PER_LANE: usize = 6;

        for lanes in [1usize, 2, 4] {
            let gate = LaneGate::new(Pacer::fallback(gap)); // `Fixed`'s target gap IS `gap`
            let spans = split_range(0, 10_000, lanes);
            assert_eq!(spans.len(), lanes);
            let started = Instant::now();
            run_lanes(&spans, |_, s, _| {
                for _ in 0..ADMITS_PER_LANE {
                    gate.admit();
                }
                Ok(vec![s])
            })
            .unwrap();
            let elapsed = started.elapsed();
            let admissions = lanes * ADMITS_PER_LANE;
            // The first admission is free (no schedule yet), so N admissions mandate N-1 gaps.
            let floor = gap * (admissions as u32 - 1);
            assert!(
                elapsed >= floor,
                "lanes={lanes}: {admissions} admissions took {elapsed:?}, under the {floor:?} the \
                 aggregate budget mandates — concurrency multiplied through the gate"
            );
        }
    }

    /// A gate that IDLED must not bank the unused slots and release a burst — it resumes from now.
    #[test]
    fn an_idle_gate_resumes_from_now_instead_of_bursting() {
        let gate = LaneGate::new(spot_pacer());
        {
            // Pretend the gate's schedule is far in the past (every lane was busy for a long time).
            // `checked_sub`, not `-`: `Instant` is the MONOTONIC clock, which on Linux starts at
            // boot, so a bare subtraction panics on a machine up for less than a minute.
            let mut g = gate.lock();
            g.next_slot = Some(Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(
                // No such instant on a very young clock — `now` still exercises the same branch.
                Instant::now,
            ));
        }
        let before = Instant::now();
        gate.admit(); // must return immediately, and re-anchor the schedule
        let after = gate.lock().next_slot.unwrap();
        assert!(before.elapsed() < Duration::from_millis(50), "a past slot must not sleep");
        assert!(
            after >= before,
            "the schedule must re-anchor to now, or the next 1200 slots are all in the past"
        );
        assert!(after <= Instant::now() + Duration::from_millis(60));
    }

    /// The proactive cooldown applies ONCE to the shared schedule, however many lanes report it —
    /// `max(next_slot, now + cooldown)`, never `next_slot + cooldown` per lane.
    #[test]
    fn a_cooldown_pauses_every_lane_once_and_does_not_stack() {
        let cooldown = Duration::from_secs(10);
        let gate = LaneGate::new(spot_pacer());
        let before = Instant::now();
        // Six lanes all complete a page while the counter sits above the soft limit.
        for _ in 0..6 {
            gate.complete(Some(5_000), Duration::from_millis(280), 2_000, cooldown);
        }
        let slot = gate.lock().next_slot.expect("a cooldown sets the schedule");
        let pushed = slot - before;
        assert!(pushed >= cooldown, "the pause must be at least one cooldown: {pushed:?}");
        assert!(
            pushed < cooldown * 2,
            "six lanes must not stack six cooldowns into a minute: {pushed:?}"
        );
    }

    /// THE inflation gate. With several requests outstanding a counter delta is not one request's
    /// cost, and feeding it to the pacer's latest-wins estimator widens the SHARED gap by roughly the
    /// lane count — handing back exactly the throughput the lanes were added to gain.
    ///
    /// FAILS on the pre-fix gate, which called `Pacer::observe`: the first reading here would infer
    /// a 12-weight request (six lanes x weight 2) and stretch the gap from 50 ms to 300 ms — 6x, the
    /// lane count, precisely cancelling six lanes.
    #[test]
    fn interleaved_completions_never_move_the_gates_gap() {
        let gate = LaneGate::new(spot_pacer());
        let sequential_gap = gate.pacer().target_gap();
        assert!((sequential_gap.as_secs_f64() - 0.050).abs() < 1e-6, "{sequential_gap:?}");

        // The shape a shared, server-sampled counter actually produces under six lanes: each
        // reading has advanced by ~six requests' worth, and one arrival in four is out of order
        // (sampled earlier, delivered later) — the backward step `Pacer::observe` reads as a window
        // roll, which makes the NEXT surviving delta span even more requests.
        let mut counter = 200u64;
        for i in 0..24 {
            counter += 12; // 6 lanes x the real weight-2 page cost
            let read = if i % 4 == 3 { counter - 7 } else { counter };
            gate.complete(Some(read), Duration::from_millis(280), 4_800, Duration::from_secs(10));
        }
        // Deliberately below BOTH guards (soft limit 4800, window share 2400), so this test is about
        // the gap and nothing else.
        assert!(counter < 2_400);

        assert_eq!(
            gate.pacer().target_gap(),
            sequential_gap,
            "an unattributable delta must not re-pace the shared gate"
        );
        assert_eq!(
            gate.pacer().per_request_weight(),
            2.0,
            "the SEQUENTIAL probe's measurement is what the gate paces on"
        );
    }

    /// ...and the freeze must not disarm the brake. The counter's ABSOLUTE value is still recorded,
    /// so the guard that actually prevents a 429 fires exactly as before — this is the half of
    /// `observe_absolute` that is kept, and it is the load-bearing half.
    ///
    /// FAILS on the pre-fix gate too, on the last assertion: `observe` would have inferred a
    /// 2398-weight request from the same reading.
    #[test]
    fn the_frozen_weight_leaves_the_absolute_guards_armed() {
        let gate = LaneGate::new(spot_pacer());
        gate.complete(Some(2_500), Duration::from_millis(280), 4_800, Duration::from_secs(10));
        assert!(gate.pacer().should_cool_down(), "2500 is past the 2400 window share");
        assert!(gate.lock().next_slot.is_some(), "...and every lane is paused for it");
        assert_eq!(
            gate.pacer().per_request_weight(),
            2.0,
            "without that reading re-pacing the gate"
        );
    }

    /// Below both guards, a completion moves no schedule — the gate must not invent a pause.
    #[test]
    fn a_completion_under_both_guards_does_not_push_the_schedule() {
        let gate = LaneGate::new(spot_pacer());
        gate.complete(Some(10), Duration::from_millis(280), 2_000, Duration::from_secs(10));
        assert!(gate.lock().next_slot.is_none(), "no guard fired ⇒ no slot pushed");
        // ...and the measurement DID land, so the run can still persist its pace.
        assert!(gate.pacer().measured().is_some());
    }

    // ----- run_lanes: ordering, dedup, failure ---------------------------------------------------

    /// One span runs INLINE — no thread — so a one-lane run is the sequential path, not a
    /// coincidentally-equal one.
    #[test]
    fn a_single_span_runs_on_the_callers_own_thread() {
        let caller = std::thread::current().id();
        let seen = Mutex::new(None);
        let out = run_lanes(&[(0i64, 100i64)], |_, s, e| {
            *seen.lock().unwrap() = Some(std::thread::current().id());
            Ok(vec![(s, e)])
        })
        .unwrap();
        assert_eq!(out, vec![(0, 100)]);
        assert_eq!(seen.into_inner().unwrap(), Some(caller));
    }

    #[test]
    fn rows_come_back_in_span_order_across_lanes() {
        let spans = split_range(0, 999, 4);
        // Each lane emits its span's start; concatenation must be ascending by span, not by
        // completion order (lane 3 sleeps longest and still lands last).
        let out = run_lanes(&spans, |_, s, _| {
            std::thread::sleep(Duration::from_millis((999 - s) as u64 / 100));
            Ok(vec![s])
        })
        .unwrap();
        assert_eq!(out, spans.iter().map(|s| s.0).collect::<Vec<_>>());
        assert!(out.windows(2).all(|w| w[0] < w[1]), "ascending: {out:?}");
    }

    #[test]
    fn an_empty_span_list_never_calls_the_body() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let out: Vec<i64> = run_lanes(&[], |_, _, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![1i64])
        })
        .unwrap();
        assert!(out.is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// THE failure policy: one lane failing fails the WHOLE call. A partial `Ok` here would be
    /// written under a commit key naming the entire window and the missing rows would never be
    /// fetched again.
    #[test]
    fn one_failed_lane_fails_the_whole_fetch_and_returns_no_rows() {
        let spans = split_range(0, 999, 4);
        let err = run_lanes(&spans, |_, s, _| {
            if s == spans[2].0 {
                Err("lane 2 blew up".to_string())
            } else {
                Ok(vec![s])
            }
        })
        .unwrap_err();
        assert_eq!(err, "lane 2 blew up", "the failure is reported, never silently shortened");
    }

    /// ...and the reported error is DETERMINISTIC — the lowest failing span index, not whichever
    /// thread happened to finish first.
    #[test]
    fn the_reported_error_is_the_lowest_failing_span_not_the_fastest() {
        let spans = split_range(0, 999, 4);
        for _ in 0..8 {
            let err = run_lanes(&spans, |_, s, _| {
                if s == spans[1].0 {
                    std::thread::sleep(Duration::from_millis(20)); // finish LAST
                    Err("span 1".to_string())
                } else if s == spans[3].0 {
                    Err("span 3".to_string()) // finish FIRST
                } else {
                    Ok(vec![s])
                }
            })
            .unwrap_err();
            assert_eq!(err, "span 1");
        }
    }

    /// A failure STOPS the siblings rather than letting them run the window out — bounded by one
    /// page per lane, since a blocking request cannot be cancelled mid-flight.
    #[test]
    fn a_failure_aborts_the_remaining_lanes() {
        let spans = split_range(0, 9_999, 4);
        let pages = std::sync::atomic::AtomicUsize::new(0);
        let _ = run_lanes(&spans, |lanes, s, _| {
            if s == spans[0].0 {
                return Err("fail immediately".to_string());
            }
            // A long "pager": without the abort check this would run 1000 pages per lane.
            for _ in 0..1_000 {
                if lanes.aborted() {
                    return Ok(vec![s]);
                }
                pages.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(200));
            }
            Ok(vec![s])
        });
        assert!(
            pages.load(Ordering::Relaxed) < 3_000,
            "the siblings must stop early, not run all 3000 pages"
        );
    }
}
