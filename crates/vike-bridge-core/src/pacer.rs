//! [`Pacer`]: pace a paged REST backfill against a DISCOVERED weight budget and the venue's
//! OBSERVED consumption, so neither the budget nor the per-request weight is hardcoded.
//!
//! ## The contract
//! The pacer is fed the venue's cumulative used-weight counter (Binance/Aster's
//! `x-mbx-used-weight-1m` header, or any venue with the same convention) via [`Pacer::observe`], and
//! answers how long to sleep before the next request ([`Pacer::next_delay`]) and whether usage has
//! crossed the target share of the budget ([`Pacer::should_cool_down`]). It is **pure**: no
//! sleeping, no clock, no I/O; the CALLER owns the `thread::sleep`.
//!
//! The budget comes from the venue (`crate::rate_discovery`) and the per-request weight from
//! successive counter deltas, so a venue that re-prices an endpoint (2 -> 5) self-corrects on the
//! next observation instead of over-spending into a 418 IP ban.
//!
//! ## The counter reset
//! The counter is cumulative WITHIN the venue's interval window and RESETS when the window rolls.
//! **A backwards step is a window reset, never negative weight**: an unchecked `used - last` on
//! `u64` panics in debug and wraps to ~1.8e19 in release, stalling the backfill at
//! `MAX_DELAY_SECS`. On a reset the pacer re-baselines and KEEPS the previous estimate: after a
//! roll it cannot tell its own request from another client on the same IP.
//!
//! ## The gap is `sleep + request`, never `sleep` alone
//! A sequential pager's real spacing is `sleep + request_time`, so sleeping the whole target
//! interval under-spends by whatever a request costs (binance fapi at a 40 % target delivered ~23 %
//! with ~280 ms round trips). [`Pacer::observe_request`] feeds the measured wall clock in and
//! [`Pacer::next_delay`] SUBTRACTS it, floored at `MIN_DELAY_SECS`; with nothing measured the
//! delay is the plain arithmetic. The request-time estimate is an EWMA, unlike the latest-wins
//! weight, because it is subtracted (`REQUEST_TIME_ALPHA`).
//!
//! ## Seeding from a previous run ([`Pacer::seed`] / [`Pacer::measured`])
//! [`Pacer::measured`] hands out the run's observation (`None` unless something was timed, so a run
//! that measured nothing cannot poison a stored record) and [`Pacer::seed`] takes one back in. This
//! module does no I/O: persistence belongs to a caller in a binary-reachable layer. Three rules
//! make seeding safe:
//! 1. **A seed is a PRIOR, never a measurement.** It lives in its own field and is dropped whole on
//!    the first live [`Pacer::observe_request`]; blending would let a stale number keep pulling the
//!    sleep down after the truth is known.
//! 2. **A seed never applies over a live observation.** [`Pacer::seed`] is a no-op once anything
//!    has been observed.
//! 3. **A seed measured against a DIFFERENT budget is refused.** The budget is the host's identity
//!    (binance spot 6000/min prices `/klines` at weight 2, fapi 2400/min at 5), so a stored record
//!    can move the SLEEP, never the budget.
//!
//! With nothing seeded every delay and ETA is the unseeded arithmetic (the `fallback`/unseeded tests
//! pin it).
//!
//! ## Discovery failure still MEASURES
//! [`Pacer::fallback`] (no published budget: bybit, okx, deribit) is a fixed delay that never cools
//! down, but a request can always be timed, so [`Pacer::observe_request`], [`Pacer::measured`] and
//! [`Pacer::eta`] work in both modes. Only [`Pacer::next_delay`] branches: in `Fixed` it answers the
//! caller's constant whatever was observed, because a delay derived from a bare stopwatch, with no
//! ceiling to pace against, is a number nothing could check.
//!
//! ## Reaching the target needs CONCURRENCY
//! A sequential pager cannot space requests closer than one round trip, so when the target gap is
//! narrower the delay floors at `MIN_DELAY_SECS` and the pager under-spends.
//! [`Pacer::suggested_lanes`] derives the in-flight count, a concurrent caller paces on
//! [`Pacer::target_gap`], and [`crate::concurrent`] enforces the aggregate.

use std::time::Duration;

use crate::rate_discovery::WeightBudget;

// Utilization bounds are `vike_model::rate_limits`' (it validates operator input against them);
// never redefine them here, or the accepted range and the applied clamp drift apart.
use vike_model::rate_limits::{MAX_UTILIZATION, MIN_UTILIZATION, PaceSample};

/// A NaN utilization is a caller bug; resolve it in the SAFE direction (slowest) rather than
/// propagating NaN into a `Duration::from_secs_f64`, which panics.
const NAN_UTILIZATION: f64 = MIN_UTILIZATION;

/// Per-request weight assumed before the first counter delta.
///
/// PESSIMISTIC (binance fapi klines, the most expensive endpoint measured, weight 5), not 1, because
/// the failure modes are asymmetric: too high on a cheap endpoint costs a slow first page or two;
/// too low on an expensive one runs the opening burst up to 5x over budget into an HTTP 418 IP ban
/// that also takes down the live recorder sharing the egress.
const SEED_WEIGHT: f64 = 5.0;

/// Weight floor for the pace divisor. A request cannot cost less than one unit, and a `0` divisor
/// would mean "infinite requests per minute" — a zero-delay hammer.
const MIN_WEIGHT: f64 = 1.0;

/// EWMA weight of the NEWEST observed request duration. Not latest-wins like the per-request weight
/// ([`Pacer::observe`]), because the two are used in opposite directions: the weight MULTIPLIES the
/// gap (a high outlier slows the next request, the safe direction), the duration is SUBTRACTED from
/// it (a high outlier, e.g. a folded-in 429 `Retry-After` sleep, would floor the next sleep). A
/// quarter weight caps one outlier's pull at 25 % and still converges on a real step change within
/// a handful of pages.
const REQUEST_TIME_ALPHA: f64 = 0.25;

/// Delay floor. Even a huge budget against a weight-1 endpoint must not spin the pager into a
/// busy-loop against the venue.
const MIN_DELAY_SECS: f64 = 0.001;

/// Delay ceiling: past a minute per request the venue's window has rolled and slowing gains
/// nothing. Also keeps an absurd inferred weight from becoming an un-representable `Duration`.
const MAX_DELAY_SECS: f64 = 60.0;

/// What the pacer is pacing against: a venue-declared budget or a fixed delay. There is no third
/// state and no "half-discovered" budget with a guessed limit.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// A venue-declared budget, reduced to the two numbers pacing needs.
    Discovered {
        /// Target spend, weight per MINUTE, normalized so "600 per 10s" and "3600 per 60s" pace
        /// identically.
        per_minute_target: f64,
        /// Target spend WITHIN the venue's own interval window: the unit the cumulative counter is
        /// denominated in, so the only one `should_cool_down` may compare against.
        window_target: f64,
        /// The venue's UNSCALED published budget per minute (before `utilization`). Steers
        /// nothing: it is the identity a persisted [`PaceSample`] is matched against
        /// ([`Pacer::seed`]'s rule 3). Stored, not re-derived from `per_minute_target /
        /// utilization`, which would go through two roundings and a clamp.
        budget_per_minute: u64,
    },
    /// Discovery failed: one fixed inter-request delay, no target. Requests are still timed and
    /// reported (module doc).
    Fixed(Duration),
}

/// A pure pacing calculator over a venue's used-weight counter. Cheap to clone; holds no handle,
/// clock or socket. One pacer belongs to one paging loop.
#[derive(Debug, Clone)]
pub struct Pacer {
    mode: Mode,
    /// Last counter value seen, the delta baseline. `None` until the first [`Pacer::observe`].
    last_used: Option<u64>,
    /// The latest counter value: this window's budget already spent, by us and by anything else
    /// sharing the IP.
    observed: u64,
    /// Inferred cost of one request, in weight units: `SEED_WEIGHT`, then each positive delta.
    per_request: f64,
    /// EWMA of request wall-clock, in SECONDS (`REQUEST_TIME_ALPHA`). `None` until the first
    /// [`Pacer::observe_request`], and load-bearing: it gives an un-instrumented caller the plain
    /// delays and makes [`Pacer::eta`] refuse to answer.
    observed_request_secs: Option<f64>,
    /// A request time seeded from a previous run ([`Pacer::seed`]), in seconds. A SEPARATE field so
    /// the first live observation drops it whole (module doc's rule 1); `None` makes every delay and
    /// ETA the unseeded one.
    seeded_request_secs: Option<f64>,
    /// How many requests were timed; reported by [`Pacer::measured`], steers nothing.
    samples: u64,
    /// FLOOR of this run's used-weight deltas. Persisted instead of `per_request` because a shared
    /// per-IP counter overstates our cost; never used for the live delay.
    min_delta: Option<f64>,
}

impl Pacer {
    /// Pace against a venue-declared budget, targeting `utilization` (0.0..=1.0) of it.
    ///
    /// `utilization` is CLAMPED into [`MIN_UTILIZATION`]..=[`MAX_UTILIZATION`] (NaN resolves to the
    /// floor), so a caller cannot produce a divide-by-zero, a zero-delay hammer, or a target above
    /// the venue's real ceiling.
    pub fn discovered(budget: WeightBudget, utilization: f64) -> Self {
        let util = clamp_utilization(utilization);
        Pacer {
            mode: Mode::Discovered {
                // `.max(1.0)`: a degenerate discovered budget (limit 0) must not become a zero
                // divisor / an always-true cooldown. One weight unit is the smallest sane target.
                per_minute_target: (budget.per_minute() as f64 * util).max(1.0),
                window_target: (budget.limit as f64 * util).max(1.0),
                budget_per_minute: budget.per_minute(),
            },
            last_used: None,
            observed: 0,
            per_request: SEED_WEIGHT,
            observed_request_secs: None,
            seeded_request_secs: None,
            min_delta: None,
            samples: 0,
        }
    }

    /// Discovery failed: [`Pacer::next_delay`] is exactly `page_delay` forever and
    /// [`Pacer::should_cool_down`] always `false` (no budget, no target). It still measures, so feed
    /// it [`Pacer::observe_request`] (module doc).
    pub fn fallback(page_delay: Duration) -> Self {
        Pacer {
            mode: Mode::Fixed(page_delay),
            last_used: None,
            observed: 0,
            per_request: SEED_WEIGHT,
            observed_request_secs: None,
            seeded_request_secs: None,
            min_delta: None,
            samples: 0,
        }
    }

    /// Is this pacer steering a DISCOVERED budget rather than answering a fixed `page_delay`? A pace
    /// report should stamp it (the same delay is a budget fraction or a constant); it is not a
    /// reason to withhold the report.
    pub fn is_discovered(&self) -> bool {
        matches!(self.mode, Mode::Discovered { .. })
    }

    /// The inferred cost of one request in weight units: `SEED_WEIGHT` until two observations
    /// have produced a counter delta ([`Pacer::observe`]). DIAGNOSTICS only (a pace report prints
    /// it); nothing outside steers on it.
    pub fn per_request_weight(&self) -> f64 {
        self.per_request
    }

    /// The venue's UNSCALED published budget in weight/minute, or `None` in fallback mode: the
    /// number a persisted record is matched against, not a permission. Unscaled so two runs at
    /// different utilizations, still the same host, can share a measured pace.
    pub fn budget_per_minute(&self) -> Option<u64> {
        match self.mode {
            Mode::Discovered { budget_per_minute, .. } => Some(budget_per_minute),
            Mode::Fixed(_) => None,
        }
    }

    /// This run's own pace observation, for a caller that persists it: `None` unless a request was
    /// TIMED ([`Pacer::observe_request`]), so a run that measured nothing can never overwrite a good
    /// stored record. A fallback pager reports too, with `budget_per_min: None`, which keeps its
    /// record from ever seeding a discovered run ([`Pacer::seed`]'s rule 3).
    ///
    /// ⚠ `per_request_weight` means something only where the venue publishes a counter. On
    /// bybit/okx/deribit it is the un-inferred `SEED_WEIGHT`, carried because
    /// [`PaceSample::is_usable`] requires a positive weight and inert because a `None` budget
    /// confines the record to another `Fixed` pacer; read `request_ms` there.
    ///
    /// `request_ms` rounds the EWMA to whole milliseconds and saturates rather than wrapping.
    pub fn measured(&self) -> Option<PaceSample> {
        let secs = self.observed_request_secs?;
        let ms = secs * 1_000.0;
        let request_ms =
            if ms.is_finite() && ms >= 0.0 { ms.round().min(u64::MAX as f64) } else { 0.0 };
        Some(PaceSample {
            request_ms: request_ms as u64,
            // FLOOR, not the latest: see `observe`. A polluted delta must not be written down.
            per_request_weight: self.min_delta.unwrap_or(self.per_request),
            budget_per_min: self.budget_per_minute(),
            samples: self.samples,
        })
    }

    /// Seed this pacer from a PREVIOUS run's [`PaceSample`]; returns whether it was applied.
    ///
    /// REFUSED (`false`, nothing changed) once anything has been observed (module doc's rule 2), for
    /// a sample [`PaceSample::is_usable`] rejects (a `0` weight is a zero-delay hammer, a NaN panics
    /// `Duration::from_secs_f64`), for a `budget_per_min` that is not this pacer's (rule 3, `None`
    /// included), and for a request time that is non-finite or at/above `MAX_DELAY_SECS`. On
    /// acceptance the weight lands in the estimate and the request time in the prior field (rule 1).
    pub fn seed(&mut self, sample: &PaceSample) -> bool {
        if self.observed_request_secs.is_some() || self.last_used.is_some() || self.samples > 0 {
            return false;
        }
        if !sample.is_usable() || sample.budget_per_min != self.budget_per_minute() {
            return false;
        }
        let secs = sample.request_secs();
        if !secs.is_finite() || secs <= 0.0 || secs >= MAX_DELAY_SECS {
            return false;
        }
        self.per_request = sample.per_request_weight.max(MIN_WEIGHT);
        self.seeded_request_secs = Some(secs);
        true
    }

    /// The request-time estimate: the live EWMA, else a seeded prior, else nothing. ONE resolution,
    /// so `next_delay` and `eta` cannot disagree about what has been measured.
    fn request_secs_estimate(&self) -> Option<f64> {
        self.observed_request_secs.or(self.seeded_request_secs)
    }

    /// Feed the venue's cumulative used-weight counter (e.g. `x-mbx-used-weight-1m`). Records in
    /// both modes and updates the inferred per-request weight:
    /// * `used_weight > last`: the delta BECOMES the estimate. Latest-wins, not an average: a paging
    ///   loop issues identical requests, and a one-off heavier one slows only the next request.
    /// * `used_weight == last`: no information; keep the estimate rather than infer a free request.
    /// * `used_weight < last`: the window ROLLED. Re-baseline, keep the estimate, never subtract.
    ///
    /// Sequential callers only: with several requests in flight use [`Pacer::observe_absolute`].
    ///
    /// ## Latest-wins is right for PACING, wrong for PERSISTING
    /// The counter is per-IP, so a delta is `our request + whatever else spent in the gap`: it can
    /// overstate our cost, never understate it. For pacing that errs slow and self-corrects, and a
    /// genuine re-price must widen the gap at once
    /// (`a_repriced_endpoint_self_corrects_on_the_next_observation`), which a minimum would miss.
    /// For persisting it does not: on a shared egress a weight-5 page read 25 and the next run paced
    /// 25x too slow. So [`Pacer::measured`] reports the FLOOR of this run's deltas.
    pub fn observe(&mut self, used_weight: u64) {
        if let Some(last) = self.last_used
            && used_weight > last
        {
            let delta = (used_weight - last) as f64;
            self.per_request = delta;
            // The floor is for PERSISTENCE only, never the live delay.
            self.min_delta = Some(self.min_delta.map_or(delta, |m: f64| m.min(delta)));
        }
        // `used_weight <= last`: no movement or a window reset; keep the estimate, never subtract.
        self.last_used = Some(used_weight);
        self.observed = used_weight;
    }

    /// Record the venue's counter WITHOUT inferring a per-request weight: the CONCURRENT twin of
    /// [`Pacer::observe`], and the one [`crate::concurrent::LaneGate`] calls.
    ///
    /// ⚠ **With several requests in flight a reading must come HERE, never to the delta path.** A
    /// delta then cannot be attributed to one request: the server samples the counter as it handles
    /// each of ours, so two readings span roughly N requests, and out-of-order responses make
    /// `observe` re-baseline on backward steps so the surviving forward steps span more. The bias is
    /// one-directional: `per_request` inflates by roughly the lane count, [`Pacer::target_gap`]
    /// widens with it, N lanes each pace N times slower, and a large enough delta saturates the gap
    /// at `MAX_DELAY_SECS`, a hang.
    ///
    /// The reading still lands in `observed`, so [`Pacer::should_cool_down`] and the caller's
    /// `weight_soft_limit` (the guards that actually prevent a 429/418, comparing absolute readings
    /// to absolute thresholds) work unchanged; `per_request` and `min_delta` stay where the
    /// sequential phase measured them.
    ///
    /// ⚠ Accepted residual: a mid-flight re-price is caught only coarsely, by the cooldown (binance
    /// spot 2 -> 5 would spend the full ceiling, cross the window target within ~24 s, then pause
    /// every lane). Do not switch the live estimate to `min_delta` instead: a floor misses a genuine
    /// re-price and paces over budget into a 418.
    pub fn observe_absolute(&mut self, used_weight: u64) {
        // `last_used` advances too: it is the delta BASELINE, and leaving it behind would make a
        // later `observe` subtract across the whole concurrent phase at once.
        self.last_used = Some(used_weight);
        self.observed = used_weight;
    }

    /// Feed one COMPLETED request: its wall-clock time and, when the venue sent one, its used-weight
    /// counter (forwarded to [`Pacer::observe`], so a measuring caller calls only this).
    ///
    /// The real gap is `next_delay() + elapsed`; `elapsed` is the half the pacer cannot see (module
    /// doc). The estimate is an EWMA (`REQUEST_TIME_ALPHA`); the first sample seeds it outright.
    ///
    /// ⚠ Measure the REQUEST, not the retry loop: a duration that swallowed a 429 `Retry-After`
    /// sleep shortens the next sleep. Bounded (the EWMA caps one sample's pull, the delay floors at
    /// `MIN_DELAY_SECS`) but avoidable.
    pub fn observe_request(&mut self, used_weight: Option<u64>, elapsed: Duration) {
        // A `Duration` is finite and non-negative; an absurd one (`Duration::MAX`) is handled where
        // it matters: `next_delay` clamps and `eta` saturates.
        let secs = elapsed.as_secs_f64();
        self.observed_request_secs = Some(match self.observed_request_secs {
            Some(prev) => prev * (1.0 - REQUEST_TIME_ALPHA) + secs * REQUEST_TIME_ALPHA,
            None => secs,
        });
        // A live measurement DROPS the seeded prior whole (module doc's rule 1), cleared here, at
        // the one site that could violate it.
        self.seeded_request_secs = None;
        self.samples = self.samples.saturating_add(1);
        if let Some(w) = used_weight {
            self.observe(w);
        }
    }

    /// How long to sleep before the NEXT request.
    ///
    /// Discovered: the target GAP `60s * per_request / per_minute_target` minus the measured request
    /// time (`0.0` with nothing measured), clamped into `MIN_DELAY_SECS`..=`MAX_DELAY_SECS` so a
    /// request slower than the gap floors the delay instead of going negative, and any counter value
    /// stays a representable `Duration`.
    ///
    /// Fixed: the caller's `page_delay`, EXACTLY, whatever was observed (module doc).
    pub fn next_delay(&self) -> Duration {
        match self.mode {
            Mode::Discovered { per_minute_target, .. } => {
                let per_req = self.per_request.max(MIN_WEIGHT);
                // Both operands are finite and >= 1, so the result stays finite; the clamp makes
                // `from_secs_f64` total (it panics on NaN/negative/overflow).
                let target_gap = 60.0 * per_req / per_minute_target;
                let secs = (target_gap - self.request_secs_estimate().unwrap_or(0.0))
                    .clamp(MIN_DELAY_SECS, MAX_DELAY_SECS);
                Duration::from_secs_f64(secs)
            }
            Mode::Fixed(page_delay) => page_delay,
        }
    }

    /// The target inter-request GAP: the whole spacing the venue should see, BEFORE any request
    /// time is subtracted.
    ///
    /// A CONCURRENT pager ([`crate::concurrent::LaneGate`]) paces on this, never on
    /// [`Pacer::next_delay`]: its round trips overlap, so subtracting one would spend the difference
    /// twice over.
    ///
    /// ⚠ In `Mode::Fixed` this is the caller's `page_delay`, but a fixed pager's real spacing is
    /// `page_delay + request`, so a shared gate driven off it would RAISE the rate; that is why
    /// [`Pacer::suggested_lanes`] hands out one lane there.
    ///
    /// Clamped like `next_delay`, but after the divide here and before the subtraction there: the
    /// two are deliberately not expressed in terms of each other, so this cannot move a sleep.
    pub fn target_gap(&self) -> Duration {
        match self.mode {
            Mode::Discovered { per_minute_target, .. } => {
                let per_req = self.per_request.max(MIN_WEIGHT);
                let secs =
                    (60.0 * per_req / per_minute_target).clamp(MIN_DELAY_SECS, MAX_DELAY_SECS);
                Duration::from_secs_f64(secs)
            }
            Mode::Fixed(page_delay) => page_delay,
        }
    }

    /// How many requests must be IN FLIGHT to reach [`Pacer::target_gap`], capped at `max`:
    /// `ceil(request_time / target_gap)`, because a sequential pager cannot space requests closer
    /// than one round trip. When the gap is the wider of the two the budget binds and this answers
    /// `1` (binance at 0.40 with ~280 ms round trips: fapi's 312 ms gap gives 1 lane, spot's 50 ms
    /// gap gives 6).
    ///
    /// ⚠ **Lane advice reads `per_request`, which needs TWO sequential readings or a seeded
    /// [`PaceSample`].** [`Pacer::observe`] infers from a delta, so after one reading the estimate
    /// is still the pessimistic `SEED_WEIGHT` and the lane count is wrong (spot reads as a 125 ms
    /// gap and 3 lanes, pacing the run at 40 % of the 40 % asked for). Nothing here can enforce it;
    /// the caller's side is `crates/bridges/binance/src/family/klines.rs`'s `PROBE_PAGES`. ⚠ In
    /// `Mode::Fixed` the delay a pacer reports is not the pager's real spacing
    /// ([`Pacer::target_gap`]).
    ///
    /// Returns `1`, never more, as a refusal when: nothing is measured (no live sample, no seed);
    /// the mode is `Fixed` (no published ceiling for an aggregate rate to be a fraction of); or the
    /// ratio is non-finite or non-positive. `max` bounds THREADS, not rate
    /// ([`crate::concurrent::MAX_LANES`]): the rate is `1 / target_gap` whatever this returns.
    pub fn suggested_lanes(&self, max: usize) -> usize {
        if !self.is_discovered() {
            return 1;
        }
        let Some(request_secs) = self.request_secs_estimate() else {
            return 1;
        };
        let gap = self.target_gap().as_secs_f64();
        if !request_secs.is_finite() || request_secs <= gap || gap <= 0.0 {
            return 1;
        }
        let want = (request_secs / gap).ceil();
        if !want.is_finite() || want <= 1.0 {
            return 1;
        }
        // Bounded before the cast, so an absurd ratio saturates at `max` instead of wrapping.
        let capped = want.min(max.max(1) as f64) as usize;
        capped.clamp(1, max.max(1))
    }

    /// How long `remaining_requests` more requests take at the CURRENT pace:
    /// `remaining * (request_time + next_delay())`, the full cycle the venue sees.
    ///
    /// `None` while nothing is measured or seeded: an ETA from the sleep alone under-counts every
    /// round trip, and a wrong ETA is worse than none. A seeded prior answers from the first page.
    /// Answers in BOTH modes (a report, not a target). [`remaining_pages`] is the multiplicand a
    /// paged backfill hands it. Saturates at `Duration::MAX` rather than panicking.
    pub fn eta(&self, remaining_requests: u64) -> Option<Duration> {
        let request_secs = self.request_secs_estimate()?;
        let cycle_secs = request_secs + self.next_delay().as_secs_f64();
        let total = remaining_requests as f64 * cycle_secs;
        Some(Duration::try_from_secs_f64(total).unwrap_or(Duration::MAX))
    }

    /// True when observed usage has crossed the target: the caller should cool down.
    ///
    /// Compares the latest counter value against `window_target` (the counter's own interval unit).
    /// Crossing is not an error: this window's share is spent (possibly by another client on the
    /// IP); once the window rolls the next [`Pacer::observe`] sees the reset and this clears.
    pub fn should_cool_down(&self) -> bool {
        match self.mode {
            Mode::Discovered { window_target, .. } => self.observed as f64 >= window_target,
            // No discovered budget ⇒ no target ⇒ nothing to cross.
            Mode::Fixed(_) => false,
        }
    }
}

/// Estimated pages left across a `span_ms`-wide window at `rows_per_page` rows per response
/// ([`Pacer::eta`]'s multiplicand).
///
/// `None` when the interval does not parse ([`vike_model::time::interval_ms`]) or a page spans
/// nothing; `0` is a real answer; a negative span is `0`, never a wrap. A progress hint, not a
/// schedule: a venue serves no rows for a gap in its own history, so a backfill can finish early.
///
/// `rows_per_page` is a PARAMETER because the pagers disagree (binance/aster/bybit 1000, okx 100)
/// and deribit's ~5001 cap is undocumented, so its pager passes what its first page returned.
pub fn remaining_pages(span_ms: i64, interval: &str, rows_per_page: usize) -> Option<u64> {
    let rows = i64::try_from(rows_per_page).ok()?;
    let page_span = vike_model::time::interval_ms(interval)?.checked_mul(rows)?;
    if page_span <= 0 {
        return None;
    }
    Some((span_ms.max(0) / page_span) as u64)
}

/// Clamp a caller-supplied utilization into the sane band, NaN to the floor. A free function so the
/// law is testable without building a budget.
fn clamp_utilization(utilization: f64) -> f64 {
    if utilization.is_nan() {
        return NAN_UTILIZATION;
    }
    utilization.clamp(MIN_UTILIZATION, MAX_UTILIZATION)
}

#[path = "pacer_tests.rs"]
#[cfg(test)]
mod pacer_tests;
