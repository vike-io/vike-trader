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

#[path = "concurrent_tests.rs"]
#[cfg(test)]
mod concurrent_tests;
