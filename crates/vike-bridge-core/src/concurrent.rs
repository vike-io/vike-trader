//! Bounded CONCURRENCY for a paged REST backfill, with the venue's rate budget still enforced **in
//! aggregate**.
//!
//! A sequential pager cannot space requests closer than one round trip, so where the pacer's target
//! gap is narrower (binance spot: 50 ms against ~280 ms) its delay is already floored and no delay
//! tuning reaches the budget asked for. The only lever is more than one request in flight.
//!
//! ## The safety argument
//! **Concurrency here does not raise the target rate; it makes the existing target reachable.**
//! One [`crate::pacer::Pacer`] is shared by every lane behind [`LaneGate`], which hands out
//! DISPATCH SLOTS spaced [`crate::pacer::Pacer::target_gap`] apart, so the venue sees the same rate
//! at 6 lanes as at 1. The lane COUNT comes from [`crate::pacer::Pacer::suggested_lanes`], never a
//! constant, and is `1` for a venue whose budget was not discovered. [`MAX_LANES`] bounds threads,
//! not rate.
//!
//! ## Splitting the WINDOW, never the page grid
//! [`split_range`] cuts `[start_ms, end_ms]` into contiguous, disjoint, inclusive spans and each
//! lane runs the venue's OWN unmodified pager over its span. Precomputing page boundaries would
//! assume a fixed row grid, and for the END-ANCHORED venues (bybit/deribit page BACKWARD from what
//! the last page returned) a wrongly computed window silently TRUNCATES. Every pager is already
//! correct for an arbitrary `[start, end]`, forward or backward.
//!
//! ## Failure policy: the whole fetch fails
//! [`run_lanes`] returns the FIRST error by span order and discards every lane's rows: no partial
//! result, no retry at this layer (each venue's page fetch owns a bounded 429/418 retry). A kline
//! backfill commits under a key naming the WHOLE window
//! (`{venue}:{symbol}:{interval}:{start}-{end}`), so a partial result reported as success would
//! mark the window ingested and its missing rows would never be fetched. A failure also STOPS the
//! other lanes: [`Lanes::aborted`] flips, each lane checks it between pages, and in-flight requests
//! drain (blocking `ureq` has no cancel), so a failure costs at most one round trip per lane.
//!
//! ## No async, no `rayon`
//! `std::thread::scope` + one `Mutex` is the whole machinery, matching the blocking bridge layer
//! (`ureq`, `tungstenite`). `rayon` is a CPU pool sized to core count; these lanes are
//! socket-blocked and their count comes from a latency ratio, so it is the wrong size and shape.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::pacer::Pacer;

/// Ceiling on lanes: a bound on OS THREADS and open sockets, **not on rate** ([`LaneGate`] admits
/// one request per target gap however many threads wait). It exists because
/// [`crate::pacer::Pacer::suggested_lanes`] is a RATIO, and a degenerate one (a 30 s round trip
/// against the 1 ms delay floor) would ask for thirty thousand threads.
///
/// 8 covers the widest real ratio (binance spot, `ceil(280 ms / 50 ms)` = 6) at a socket count
/// per-IP connection limits tolerate. A caller running lanes must size its HTTP pool to match
/// (ureq defaults to 3 idle connections per host): `http::blocking_agent_for_lanes`, named not
/// linked because `http` rides the `full` feature and this module does not.
pub const MAX_LANES: usize = 8;

/// Cut `[start_ms, end_ms]` (inclusive, as every kline fetcher takes it) into at most `lanes`
/// contiguous, disjoint, inclusive spans that together cover it exactly.
///
/// Properties callers depend on (the tests pin them):
/// * **disjoint**: span `i` ends at `span[i+1].0 - 1`, so a bar lands in exactly one span and the
///   concatenation needs no cross-span dedup;
/// * **covering**: `spans.first().0 == start_ms` and `spans.last().1 == end_ms`;
/// * **ascending**: span `i` is entirely older than span `i+1`, so concatenating ascending lane
///   outputs is ascending.
///
/// `lanes <= 1`, or a window too narrow to split, yields ONE span equal to the input (the caller's
/// single-window call, unchanged). `end_ms < start_ms` yields NO spans. `lanes` above [`MAX_LANES`]
/// is CLAMPED: an unclamped `usize::MAX` would ask for an allocation that aborts the process.
pub fn split_range(start_ms: i64, end_ms: i64, lanes: usize) -> Vec<(i64, i64)> {
    if end_ms < start_ms {
        return Vec::new();
    }
    // i128: `end - start` can exceed i64 at extreme bounds, and a wrapped span would overlap or
    // invert.
    let width = end_ms as i128 - start_ms as i128 + 1;
    let lanes = lanes.clamp(1, MAX_LANES) as i128;
    if lanes <= 1 || width <= lanes {
        return vec![(start_ms, end_ms)];
    }
    let each = width / lanes; // >= 1, since width > lanes
    let mut spans = Vec::with_capacity(lanes as usize);
    let mut cursor = start_ms as i128;
    for i in 0..lanes {
        // The LAST span absorbs the remainder and is pinned to `end_ms`, so the cover holds.
        let span_end = if i == lanes - 1 { end_ms as i128 } else { cursor + each - 1 };
        spans.push((cursor as i64, span_end as i64));
        cursor = span_end + 1;
    }
    spans
}

/// The shared state one [`LaneGate`] guards, one type under one lock.
struct GateState {
    /// The ONE pacer every lane observes into and paces against: N per-lane pacers would each spend
    /// the whole budget.
    pacer: Pacer,
    /// The instant the NEXT request may be dispatched. `None` before the first admission, which
    /// goes immediately, as the sequential pager's first page does.
    next_slot: Option<Instant>,
}

/// The aggregate admission gate: hands out dispatch slots spaced [`crate::pacer::Pacer::target_gap`]
/// apart, to however many lanes ask, which is what makes concurrency rate-NEUTRAL. Reserving a slot
/// and sleeping to it are two steps: the lock is released before the sleep, so a waiting lane never
/// blocks the next reservation.
///
/// ## The weight estimate is MEASURED sequentially, then FROZEN
/// ⚠ Lane readings go to [`crate::pacer::Pacer::observe_absolute`], never
/// [`crate::pacer::Pacer::observe`]: with several requests in flight a delta cannot be attributed
/// to one, and it inflates `per_request` (so this gate's own gap) by about the lane count; that
/// method's doc carries the mechanism and the accepted residual. `per_request` keeps what the
/// SEQUENTIAL probe phase measured, which is why the caller must probe TWO pages before splitting:
/// one reading is not a delta, and leaves the pacer's pessimistic seed in place.
///
/// The guards that actually prevent a 429/418, the caller's `weight_soft_limit` and
/// [`crate::pacer::Pacer::should_cool_down`], compare an ABSOLUTE reading against an absolute
/// threshold, so they work unchanged; [`LaneGate::complete`] applies them to every lane at once.
pub struct LaneGate {
    inner: Mutex<GateState>,
}

impl LaneGate {
    /// Take ownership of the pacer the lanes will share. Build it from the venue's own discovery and
    /// seed/observe it BEFORE handing it over: a gate is for the concurrent phase only.
    pub fn new(pacer: Pacer) -> Self {
        LaneGate { inner: Mutex::new(GateState { pacer, next_slot: None }) }
    }

    /// Lock, tolerating poisoning: one lane's panic (which `std::thread::scope` already propagates)
    /// must not become every lane's `unwrap` panic. Sound because the guarded state is two plain
    /// values with no invariant a panic can break halfway.
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserve the next dispatch slot and BLOCK until it arrives; call immediately before each
    /// request, from any lane. The slot moves one target gap per admission, so K admissions span
    /// `K * target_gap` however they are spread across lanes. The first is immediate.
    pub fn admit(&self) {
        let slot = {
            let mut g = self.lock();
            let gap = g.pacer.target_gap();
            let now = Instant::now();
            // `max(now)`: an idle gate must not bank unused slots into a burst; it resumes from now.
            let slot = g.next_slot.map_or(now, |s| s.max(now));
            g.next_slot = Some(slot + gap);
            slot
        }; // lock released BEFORE the sleep — a waiting lane never holds up the queue behind it.
        let wait = slot.saturating_duration_since(Instant::now());
        if wait > Duration::ZERO {
            std::thread::sleep(wait);
        }
    }

    /// Report one COMPLETED request: its wall clock and, where the venue sends one, its cumulative
    /// used-weight counter. The concurrent twin of the sequential `observe_request` + cooldown pair,
    /// in one call because the cooldown must move the SHARED schedule, not be slept per lane.
    ///
    /// The cooldown is `next_slot = max(next_slot, now + cooldown)`, NOT `next_slot + cooldown`:
    /// every lane completing above the threshold would otherwise stack its own pause.
    ///
    /// ⚠ The counter goes to [`crate::pacer::Pacer::observe_absolute`] (this type's doc); the
    /// DURATION is fed normally, since a lane times its own request unambiguously.
    pub fn complete(
        &self,
        used_weight: Option<u64>,
        elapsed: Duration,
        weight_soft_limit: u64,
        weight_cooldown: Duration,
    ) {
        let mut g = self.lock();
        // `None` for the weight: the counter follows separately, absolutely, without inference.
        g.pacer.observe_request(None, elapsed);
        if let Some(w) = used_weight {
            g.pacer.observe_absolute(w);
        }
        // The sequential pager's two guards, both on an ABSOLUTE reading: the hand-set soft limit
        // and the pacer's "this window's share is spent".
        if used_weight.is_some_and(|w| w >= weight_soft_limit) || g.pacer.should_cool_down() {
            let floor = Instant::now() + weight_cooldown;
            g.next_slot = Some(g.next_slot.map_or(floor, |s| s.max(floor)));
        }
    }

    /// A snapshot of the shared pacer, to log the pace or persist the run's measurement.
    pub fn pacer(&self) -> Pacer {
        self.lock().pacer.clone()
    }
}

/// The lane-shared control flag handed to each lane body: the lane side of the failure policy
/// (module doc).
pub struct Lanes {
    abort: AtomicBool,
}

impl Lanes {
    /// Has another lane already failed? Check between pages, never mid-request (a blocking `ureq`
    /// call cannot be cancelled).
    ///
    /// `Relaxed` suffices: the flag guards nothing but itself (a stale `false` costs one more page),
    /// and the failing lane's error reaches the caller through the scope join, a full sync point.
    pub fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
}

/// Run one `body` per span, concurrently, and return their rows concatenated **in span order**.
///
/// `body` receives the shared [`Lanes`] flag and its own inclusive `(start_ms, end_ms)`. With
/// [`split_range`]'s spans and ascending per-window pagers the concatenation is already ascending.
///
/// **Any lane error fails the whole call** and discards every lane's rows (module doc). The error
/// returned is the LOWEST failing span's, so a broken window reports the same message whichever
/// lane noticed first.
///
/// A single span spawns no thread: `body` runs inline, so a one-lane run IS the sequential path.
pub fn run_lanes<T, F>(spans: &[(i64, i64)], body: F) -> Result<Vec<T>, String>
where
    T: Send,
    F: Fn(&Lanes, i64, i64) -> Result<Vec<T>, String> + Sync,
{
    let lanes = Lanes { abort: AtomicBool::new(false) };
    match spans {
        [] => return Ok(Vec::new()),
        // ONE span: no thread, the caller's own thread runs the body.
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
                        // Flip BEFORE returning, so siblings stop at their next page boundary.
                        lanes.abort.store(true, Ordering::Relaxed);
                    }
                    out
                })
            })
            .collect();
        // A lane PANIC is re-raised with its payload, never turned into an `Err`: a pager bug must
        // not read as a venue that returned a 500.
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
