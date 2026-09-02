//! [`Pacer`] — pace a paged REST backfill against a **discovered** weight budget and the venue's
//! **observed** consumption, so neither the budget nor the per-request weight is ever hardcoded.
//!
//! ## The contract
//! A pacer is fed the venue's own cumulative used-weight counter (Binance/Aster's
//! `x-mbx-used-weight-1m` header, and any venue that ships the same convention) via [`Pacer::observe`],
//! and answers two questions: how long to sleep before the next request ([`Pacer::next_delay`]) and
//! whether observed usage has already crossed the target share of the budget
//! ([`Pacer::should_cool_down`]). It is **pure**: no sleeping, no clock, no I/O. `observe` records,
//! `next_delay` computes; the CALLER owns the `thread::sleep`. That is what makes the whole pacing
//! policy testable in microseconds instead of behind a live backfill.
//!
//! ## Why this replaces the hardcoded pair
//! Today each venue carries a hand-measured `page_delay` + `weight_soft_limit` pair, and both halves
//! of that pair encode facts the venue already publishes:
//!   * the **budget** — `REQUEST_WEIGHT`/`MINUTE` from `exchangeInfo` (binance spot 6000, fapi 2400)
//!     — which the soft limit restates as a constant that can silently sit ABOVE the real ceiling
//!     (`weight_soft_limit: 5000` on the 2400-limit fapi host was unreachable dead code, "safety by
//!     accident" — see `vike_binance::data`'s `BINANCE_KLINE_PERP` doc);
//!   * the **per-request weight** — 2 for spot `/klines?limit=1000`, 5 for the fapi twin — which the
//!     `page_delay` encodes only implicitly, as a delay someone divided out by hand once, per venue,
//!     per host, and which is wrong the moment the venue re-prices an endpoint.
//!
//! Pacing against a discovered budget and an INFERRED weight removes both hand-measurements: the
//! budget arrives from the venue (rate discovery), and the weight falls out of successive counter
//! deltas. A venue that re-prices `/klines` from 2 to 5 self-corrects on the very next observation
//! instead of quietly tripling consumption until a 418 IP-ban.
//!
//! ## Inferring the per-request weight (and surviving the counter reset)
//! The counter is CUMULATIVE WITHIN the venue's interval window and RESETS when that window rolls.
//! So a delta is only meaningful when it does not go backwards, and **a backwards step is a window
//! reset, never negative weight** — the single arithmetic trap this module exists to get right (an
//! unchecked `used - last` on `u64` panics in debug and wraps to ~1.8e19 in release, which would
//! then be read as a per-request weight and stall the backfill for the [`MAX_DELAY_SECS`] ceiling).
//! On a reset we re-baseline and KEEP the previous estimate rather than inferring from the new
//! counter value: after a roll we cannot tell how much of it is our own request versus another
//! client sharing the IP.
//!
//! ## The gap is `sleep + request`, never `sleep` alone
//! A pacer that answers the whole target interval as a SLEEP misses its target by whatever a
//! request costs, because the caller's real spacing is `sleep + request_time`. MEASURED on the CI box
//! (2026-08-04, 24 months of `BTCUSDT.P`): a 2400/min fapi budget at 40 % should spend
//! 960 weight/min, and the run left `x-mbx-used-weight-1m` at **556** — ~23 %, not 40 % — because
//! each page's ~280 ms round trip through the pooled agent dominated the sleep. So
//! [`Pacer::observe_request`] feeds the measured wall clock back in and [`Pacer::next_delay`]
//! SUBTRACTS it from the target interval (floored at [`MIN_DELAY_SECS`], so it can never go
//! negative or become a zero-delay hammer). Nothing is measured until the caller reports one —
//! before that first observation the delay is exactly what it always was.
//!
//! The request-time estimate is an **EWMA**, deliberately unlike the latest-wins `per_request`
//! weight: this one is SUBTRACTED, so an outlier shortens the next sleep, and the two therefore
//! need opposite smoothing (see [`REQUEST_TIME_ALPHA`]).
//!
//! ## A measurement can OUTLIVE the process ([`Pacer::seed`] / [`Pacer::measured`])
//! Everything above is re-derived from zero every run: the weight starts at [`SEED_WEIGHT`], the
//! request time at "unmeasured", and the process then throws both away at exit. So run N+1 opens
//! with the same pessimistic constants run N did — on binance SPOT, where a page costs weight 2, the
//! [`SEED_WEIGHT`] of 5 paces the opening pages 2.5x slower than the venue permits, every run,
//! forever.
//!
//! [`Pacer::measured`] hands the run's own observation out (`None` unless something was actually
//! timed — a run that measured nothing reports nothing, so it can never poison a stored record), and
//! [`Pacer::seed`] takes one back in. The persistence itself is emphatically NOT here: this module
//! does no I/O and holds no clock, so a caller (`vike_backfill::pace_book`, a BINARY-reachable
//! layer) owns the file.
//!
//! **Three rules make seeding safe**, and each exists because the unsafe version is a real failure:
//! 1. **A seed is a PRIOR, never a measurement.** It lands in its own field and is dropped whole on
//!    the first live [`Pacer::observe_request`], which then seeds the EWMA outright exactly as it
//!    always did. Blending a live sample against a stored one would let a stale number keep pulling
//!    the sleep down for pages after the truth was known.
//! 2. **A seed never applies over a live observation.** [`Pacer::seed`] is a no-op once anything has
//!    been observed — a caller cannot accidentally overwrite the run's own truth with last week's.
//! 3. **A seed measured against a DIFFERENT budget is refused.** The budget is the host's identity
//!    here (binance spot publishes 6000/min and prices `/klines` at weight 2; fapi publishes 2400
//!    and prices it at 5), so a record whose `budget_per_min` is not this pacer's is describing
//!    another endpoint and is discarded whole. This is also what keeps the LIMIT discovery's: a
//!    persisted record can move the SLEEP, never the budget it is a fraction of.
//!
//! With nothing seeded every field, every delay and every ETA is exactly what it was before this
//! existed — that is the additive contract, and the `fallback`/unseeded tests below pin it.
//!
//! ## Discovery failure is a first-class mode — and it still MEASURES
//! [`Pacer::fallback`] is the honest "discovery did not answer" shape: a fixed inter-request delay,
//! byte-identical to today's `page_delay` behavior. It never cools down, because with no discovered
//! budget there is no target to cross — the caller's own soft-limit guard remains the discipline.
//!
//! What it does NOT do is change its own delay — and that is the ONLY thing it does not do. Two
//! independent questions were coupled once and are now separate:
//!   * ***what is the budget?*** — DISCOVERED (binance, aster publish `REQUEST_WEIGHT`) or unknown
//!     (bybit, okx, deribit publish none). This is the question `Fixed` cannot answer.
//!   * ***how fast is a request?*** — **always measurable, on every venue.** Not publishing a budget
//!     never prevented us from timing our own round trips.
//!
//! So [`Pacer::observe_request`] records in BOTH modes, [`Pacer::measured`] reports in both, and
//! [`Pacer::eta`] answers in both. Only [`Pacer::next_delay`] branches, and in `Fixed` it answers
//! the caller's constant no matter what was observed. That asymmetry IS the safety property: with no
//! ceiling to pace against, deriving a delay from a bare stopwatch would be precisely the hardcoded
//! guess this module exists to remove — a number nothing could check.
//!
//! ## Reaching the target needs CONCURRENCY, and the pacer decides how much
//! A sequential pager cannot space requests closer together than one request TAKES. So when the
//! target gap is narrower than the round trip, [`Pacer::next_delay`] floors at [`MIN_DELAY_SECS`]
//! and the pager silently under-spends the operator's utilization with nothing left to tune.
//! MEASURED (the CI box, 2026-08-04, ~280 ms per pooled-agent page) the two binance hosts sit on opposite
//! sides of that line: fapi's 312 ms target gap is WIDER than the round trip (already at target, one
//! request at a time), while spot's is **50 ms** — so a sequential spot backfill delivers ~18 % of
//! the budget it was told to spend.
//!
//! [`Pacer::suggested_lanes`] answers that, and answers it from the MEASUREMENT rather than from a
//! constant: `ceil(request_time / target_gap)` is exactly the number of in-flight requests needed to
//! keep the pipe full AT the target — never above it, because a concurrent caller paces on
//! [`Pacer::target_gap`] (the whole spacing) instead of `next_delay` (the spacing minus a round trip
//! that no longer serialises). See [`crate::concurrent`] for the gate that enforces the aggregate.
//!
//! It returns `1` for a `Mode::Fixed` pacer no matter what is measured, and that refusal is the
//! safety property: a `page_delay` is a hand-chosen constant, not a fraction of a published ceiling,
//! so N of them in parallel raises the venue-facing rate against a number nobody machine-checked.
//!
//! MEASURED on the CI box 2026-08-04: okx spot/perp average **476–486 ms per page**, against a hardcoded
//! `PAGE_DELAY` of 200 ms. The constant is not even the dominant term of the gap the venue sees, and
//! nobody had ever checked, because nothing on that path was timed. bybit and deribit are the same
//! shape. The measurement is now taken, reported, and persisted (`vike_backfill::pace_book`) — and
//! the sleep is STILL exactly `page_delay`. Acting on the number is a separate change with its own
//! measurement, not a side effect of being able to see it.

use std::time::Duration;

use crate::rate_discovery::WeightBudget;

// Utilization bounds are NOT redefined here — `vike_model::rate_limits` owns them, validates
// operator input against them, and is what a GUI slider reads. A second copy in this crate meant
// vike-model accepted `1.0` as legal while the pacer silently clamped to `0.95`: an operator
// setting 100% got 95% with nothing telling them so. One definition, or the two drift.
use vike_model::rate_limits::{PaceSample, MAX_UTILIZATION, MIN_UTILIZATION};

/// A NaN utilization is a caller bug; resolve it in the SAFE direction (slowest) rather than
/// propagating NaN into a `Duration::from_secs_f64`, which panics.
const NAN_UTILIZATION: f64 = MIN_UTILIZATION;

/// Per-request weight assumed before the FIRST observation, when nothing has been measured yet.
///
/// Seeded PESSIMISTIC (the most expensive klines endpoint we have measured: binance fapi at
/// weight 5) rather than at 1. The failure modes are not symmetric, and that asymmetry decides it:
///
/// * seed too HIGH on a cheap endpoint -> the first page or two are slower than necessary, and the
///   first `observe` corrects it. Cost: milliseconds, once per run.
/// * seed too LOW on an expensive endpoint -> the opening burst runs up to 5x over budget before
///   any correction lands. Cost: HTTP 418 and an IP ban that also takes down the live recorder
///   sharing this egress.
///
/// An earlier draft seeded 1.0, reasoning that "seeding high makes discovery indistinguishable
/// from a stall". True, and irrelevant: a slow start is observable and recoverable, a ban is
/// neither.
const SEED_WEIGHT: f64 = 5.0;

/// Weight floor for the pace divisor. A request cannot cost less than one unit, and a `0` divisor
/// would mean "infinite requests per minute" — a zero-delay hammer.
const MIN_WEIGHT: f64 = 1.0;

/// EWMA weight given to the NEWEST observed request duration (the remaining 0.75 keeps the running
/// estimate). Deliberately NOT the latest-wins rule [`Pacer::observe`] uses for the per-request
/// WEIGHT, because the two numbers are used in opposite directions:
///
/// * the weight estimate is a MULTIPLIER on the gap — a one-off high sample slows the next request,
///   which is the safe direction, so taking it verbatim costs nothing;
/// * the duration estimate is SUBTRACTED from the gap — a one-off high sample (a TCP retransmit, a
///   venue hiccup, a 429 `Retry-After` sleep folded into the caller's timing) would shorten the
///   next sleep, which is the UNSAFE direction. Latest-wins here would let a single 5x outlier
///   floor the delay outright.
///
/// A quarter-weight EWMA caps one outlier's pull at 25 % and decays it over the next few pages,
/// while still converging on a genuine step change (a slower route, a different host) inside a
/// handful of requests — the pager runs hundreds of them, so convergence speed is not scarce.
const REQUEST_TIME_ALPHA: f64 = 0.25;

/// Delay floor. Even a huge budget against a weight-1 endpoint must not spin the pager into a
/// busy-loop against the venue.
const MIN_DELAY_SECS: f64 = 0.001;

/// Delay ceiling. Past one whole minute per request there is nothing left to gain by slowing
/// further — the venue's own window has rolled — and this bound is also what keeps a bogus
/// (absurdly large) inferred weight from converting into an un-representable `Duration`.
const MAX_DELAY_SECS: f64 = 60.0;

/// What the pacer is pacing against. Discovery either answered (a venue-declared budget) or it did
/// not (a fixed delay); there is no third state, and no "half-discovered" budget with a guessed limit.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// A venue-declared budget, reduced to the two numbers pacing actually needs.
    Discovered {
        /// Target spend rate, weight per MINUTE — the budget normalized across intervals so a
        /// venue publishing "600 per 10s" and one publishing "3600 per 60s" pace identically.
        per_minute_target: f64,
        /// Target spend WITHIN the venue's own interval window — the number the cumulative counter
        /// is denominated in, and therefore the only one `should_cool_down` may compare against.
        /// Distinct from `per_minute_target` for any interval that is not 60s.
        window_target: f64,
        /// The venue's UNSCALED published budget per minute — `budget.per_minute()`, before
        /// `utilization` is applied. Steers nothing: it is carried purely as the identity a
        /// persisted [`PaceSample`] is matched against ([`Pacer::seed`]'s rule 3) and reported back
        /// out by [`Pacer::measured`], so a stored record can be recognised as belonging to another
        /// host. Keeping it here rather than re-deriving from `per_minute_target / utilization`
        /// avoids reconstructing a venue fact through two roundings and a clamp.
        budget_per_minute: u64,
    },
    /// Discovery failed — the pre-discovery behavior: one fixed inter-request delay, no target.
    ///
    /// The delay is fixed; the MEASUREMENT is not. Requests are still timed, reported and used for
    /// the ETA in this mode (module doc) — what a missing budget removes is the ceiling to steer
    /// against, not the stopwatch.
    Fixed(Duration),
}

/// A pure pacing calculator over a venue's used-weight counter. Cheap to clone; holds no handle,
/// no clock and no socket — one pacer belongs to one paging loop.
#[derive(Debug, Clone)]
pub struct Pacer {
    mode: Mode,
    /// Last counter value seen, for delta inference. `None` until the first [`Pacer::observe`].
    last_used: Option<u64>,
    /// Current in-window usage — the latest counter value, which IS "how much of this window's
    /// budget is already spent" (by us and by anything else sharing the IP).
    observed: u64,
    /// Inferred cost of one request, in weight units. Seeded at [`SEED_WEIGHT`] and replaced by
    /// each positive counter delta.
    per_request: f64,
    /// EWMA of observed request wall-clock, in SECONDS ([`REQUEST_TIME_ALPHA`]). `None` until the
    /// first [`Pacer::observe_request`] — and that `None` is load-bearing, not laziness: it is what
    /// makes an un-instrumented caller (and every caller before this existed) get exactly the
    /// delays it used to, and what makes [`Pacer::eta`] refuse to answer with nothing measured.
    observed_request_secs: Option<f64>,
    /// A PERSISTED request time from a previous run ([`Pacer::seed`]), in seconds. Deliberately a
    /// SEPARATE field from `observed_request_secs` rather than pre-loading that one: this is a prior
    /// that the first live observation drops WHOLE, so a stored number can never blend into — and
    /// keep pulling on — the run's own measurement. `None` (the default, and the state after any
    /// live observation) makes every delay and every ETA byte-identical to the unseeded pacer.
    seeded_request_secs: Option<f64>,
    /// How many requests [`Pacer::observe_request`] has timed. Reported by [`Pacer::measured`] so a
    /// stored record carries how much evidence is behind it; steers nothing in this module.
    samples: u64,
    /// FLOOR of the used-weight deltas seen this run. Persisted instead of `per_request`: the
    /// counter is shared per-IP, so a delta overstates our cost, and the minimum is the closest
    /// estimate of what OUR request alone costs. Never used for the live delay.
    min_delta: Option<f64>,
}

impl Pacer {
    /// Pace against a venue-declared budget, targeting `utilization` (0.0..=1.0) of it.
    ///
    /// `utilization` is CLAMPED into [`MIN_UTILIZATION`]..=[`MAX_UTILIZATION`] (NaN resolves to the
    /// floor) — a caller cannot produce a divide-by-zero, a zero-delay hammer, or a target above the
    /// venue's real ceiling. Example, the live fapi shape: a 2400/min budget at `0.4` targets
    /// 960 weight/min, which at the measured weight-5 `/klines` cost is 192 requests/min.
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

    /// Discovery failed — fall back to a fixed inter-request delay.
    ///
    /// [`Pacer::next_delay`] returns exactly `page_delay` forever and [`Pacer::should_cool_down`] is
    /// always `false`: with no discovered budget there is no target to cross, so inventing one from
    /// a bare counter value — or from a bare stopwatch — would be the same hardcoded guess this
    /// module removes.
    ///
    /// It is nonetheless a MEASURING pacer, and a caller should feed it: [`Pacer::observe_request`]
    /// records, [`Pacer::measured`] reports a persistable [`PaceSample`], and [`Pacer::eta`] answers
    /// from the first timed page. That is the whole point of pacing a no-budget venue through this
    /// type rather than a bare `thread::sleep(PAGE_DELAY)` — the delay is identical either way, but
    /// only one of the two can tell an operator what a page actually costs.
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

    /// Is this pacer steering a DISCOVERED budget (rather than answering a fixed `page_delay`)?
    ///
    /// The one thing a caller cannot infer from [`Pacer::next_delay`] alone, and it needs to, because
    /// the same delay means two different things: a fraction of the venue's own published budget, or
    /// the caller's hardcoded constant. A pager that reports its pace should STAMP the answer on that
    /// report — it is what tells a reader whether `next_delay` was derived or dictated.
    ///
    /// It is deliberately NOT a reason to withhold the report. That is what it used to be, on the
    /// reasoning that a fallback pager had "nothing to report" — which conflated the unknown budget
    /// with the perfectly measurable request time (see the module doc).
    pub fn is_discovered(&self) -> bool {
        matches!(self.mode, Mode::Discovered { .. })
    }

    /// The currently inferred cost of one request in weight units — [`SEED_WEIGHT`] until two
    /// observations have produced a counter delta (see [`Pacer::observe`]).
    ///
    /// Exposed for DIAGNOSTICS only: it is the number a pager's one-line pace report has to print
    /// for that report to be checkable against the venue's published budget. Nothing steers on it
    /// from outside — the pacer already folded it into [`Pacer::next_delay`].
    pub fn per_request_weight(&self) -> f64 {
        self.per_request
    }

    /// The venue's UNSCALED published budget in weight/minute, or `None` in fallback mode.
    ///
    /// The number a persisted record is matched against — NOT a permission. It is deliberately the
    /// venue's own published figure and not `per_minute_target` (which already has `utilization`
    /// folded in): two runs at different operator utilizations are still the same host, and must
    /// still be able to share a measured pace.
    pub fn budget_per_minute(&self) -> Option<u64> {
        match self.mode {
            Mode::Discovered { budget_per_minute, .. } => Some(budget_per_minute),
            Mode::Fixed(_) => None,
        }
    }

    /// This run's own pace observation, for a caller that persists it — `None` unless a request was
    /// actually TIMED ([`Pacer::observe_request`]).
    ///
    /// That `None` is the point: a run that measured nothing (an un-instrumented caller, a backfill
    /// whose window was already ingested and fetched one empty page) reports nothing, so it can
    /// never overwrite a good stored record with a fabricated one. A FALLBACK-mode pager is not in
    /// that list — it times its pages like any other and reports them, with `budget_per_min: None`,
    /// which is also what stops its record ever seeding a discovered run (see [`Pacer::seed`]'s
    /// rule 3).
    ///
    /// ⚠ **`per_request_weight` is only meaningful where the venue publishes a counter.** It may
    /// still be the un-inferred [`SEED_WEIGHT`] when the venue never moved one — honest on binance
    /// (it is what this run actually paced against) and *inert but meaningless* on bybit/okx/deribit,
    /// which have no weight concept at all and whose `Fixed` pacer never reads the field. It is
    /// carried because [`PaceSample::is_usable`] requires a positive weight; the field an operator
    /// should read on those venues is `request_ms`. Nothing can act on the stale number: a `None`
    /// budget confines the record to another `Fixed` pacer, where the weight steers nothing.
    ///
    /// `request_ms` rounds the EWMA to whole milliseconds (the unit the persisted file speaks) and
    /// saturates rather than wrapping, so an absurd `Duration` cannot come back as a tiny one.
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

    /// Seed this pacer from a PREVIOUS run's [`PaceSample`] — the whole point of persisting one.
    ///
    /// Returns whether the seed was applied, so a caller can log which of the two it got rather than
    /// guessing. It is REFUSED (returning `false`, changing nothing) in four cases, and every one of
    /// them is a real failure mode rather than defensive decoration:
    /// * **anything has already been observed** — a seed is a prior for a run that has not started
    ///   measuring, never an override of the run's own truth;
    /// * **the sample is unusable** ([`PaceSample::is_usable`]: a NaN/zero/negative weight, an
    ///   untimed request, zero samples) — a `0` weight is a zero-delay hammer and a NaN panics
    ///   `Duration::from_secs_f64`;
    /// * **its `budget_per_min` is not this pacer's** ([`Pacer::budget_per_minute`], `None`
    ///   included) — the budget is the host's identity, and a spot record seeding a perp pager would
    ///   pace it at 2.5x the target. This is also the rule that keeps the LIMIT discovery's: nothing
    ///   a stored file says can widen the budget, only the sleep inside it;
    /// * **the request time is not a usable duration** — non-finite, or at/above [`MAX_DELAY_SECS`],
    ///   where it would be subtracted into the floor and mean nothing anyway.
    ///
    /// On acceptance the weight lands directly in the estimate (the first counter delta overwrites
    /// it latest-wins, exactly as an in-run observation does) while the request time lands in the
    /// SEPARATE prior field the first live observation drops whole — see the module doc's rule 1.
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

    /// The request-time estimate the pacing arithmetic runs on: this run's live EWMA if there is
    /// one, else a seeded prior, else nothing. ONE resolution, so `next_delay` and `eta` can never
    /// disagree about what has been measured.
    fn request_secs_estimate(&self) -> Option<f64> {
        self.observed_request_secs.or(self.seeded_request_secs)
    }

    /// Feed the venue's own cumulative used-weight counter (e.g. `x-mbx-used-weight-1m`).
    ///
    /// Records unconditionally (in both modes, so the bookkeeping has no mode branch), and updates
    /// the inferred per-request weight from the delta:
    ///   * `used_weight > last` — a real consumption delta; it BECOMES the estimate. Latest-wins,
    ///     not an average: a paging loop issues identical requests, so successive deltas agree, and
    ///     a one-off heavier request correctly slows only the request that follows it (the safe
    ///     direction) before the next normal delta restores the estimate.
    ///   * `used_weight == last` — the venue did not move the counter; no information, keep the
    ///     estimate rather than inferring a free request.
    ///   * `used_weight < last` — the interval window ROLLED. Re-baseline, keep the estimate, and
    ///     never compute `used - last` (see the module doc: that subtraction is the trap).
    ///
    /// ## Latest-wins is right for PACING, and wrong for PERSISTING
    ///
    /// The counter is per-IP and shared with everything else on this egress, so a delta is
    /// `our request + whatever else spent in the gap` — it can OVERSTATE our cost but never
    /// understate it. For live pacing that error is in the SAFE direction (too slow, never too
    /// fast) and self-corrects on the next clean delta, which is exactly what
    /// `a_repriced_endpoint_self_corrects_on_the_next_observation` pins: a genuine 2 -> 5 re-price
    /// must widen the gap immediately, and taking a minimum here would MISS that and pace over
    /// budget into a 418.
    ///
    /// What must not inherit the over-estimate is the value written to disk. MEASURED 2026-08-04
    /// on the CI box, where `vike-recorder` also hits binance: a fapi klines page costing weight 5
    /// inferred 25, and persisting that made the next run pace 25x too slow (52 ms -> 1312 ms).
    /// So [`Pacer::measured`] reports the FLOOR of the deltas seen this run — the best estimate of
    /// what one of OUR requests costs — while `per_request` keeps the latest for pacing.
    pub fn observe(&mut self, used_weight: u64) {
        if let Some(last) = self.last_used {
            if used_weight > last {
                let delta = (used_weight - last) as f64;
                self.per_request = delta;
                // The floor is for PERSISTENCE only — never for the live delay. See above.
                self.min_delta = Some(self.min_delta.map_or(delta, |m: f64| m.min(delta)));
            }
            // `used_weight <= last` is either "no movement" or a window reset — both keep the
            // estimate, and neither subtracts.
        }
        self.last_used = Some(used_weight);
        self.observed = used_weight;
    }

    /// Record the venue's counter WITHOUT inferring a per-request weight from it — the CONCURRENT
    /// twin of [`Pacer::observe`], and the reason [`crate::concurrent::LaneGate`] does not simply
    /// call that one.
    ///
    /// ## Why a delta is unattributable while several requests are in flight
    /// [`Pacer::observe`]'s inference is `delta = this reading - the last one`, and it is sound for
    /// exactly one reason: with ONE request outstanding, the only thing that moved the counter
    /// between two readings was that one request. With N outstanding the premise is gone, in two
    /// compounding ways:
    /// * a reading is sampled at the SERVER when it processes our request, so the delta between two
    ///   of our readings covers however many of our requests the venue handled in between —
    ///   roughly N;
    /// * responses arrive OUT OF ORDER, so the values we read are a shuffle of a monotone sequence.
    ///   `observe` reads a backward step as a window roll and re-baselines on it, which makes the
    ///   surviving forward steps span even more requests. The positive excursions of a shuffled
    ///   monotone sequence sum to more than its net increase, so the bias is systematic and
    ///   one-directional: **`per_request` inflates by roughly the lane count.**
    ///
    /// An inflated `per_request` widens [`Pacer::target_gap`] proportionally, and a concurrent gate
    /// paces on exactly that — so N lanes would each be paced N times slower and the aggregate would
    /// land back at the sequential rate or below it. Past a large enough delta the gap saturates at
    /// [`MAX_DELAY_SECS`] outright (the stall
    /// `an_unseeded_pacer_is_unchanged_in_every_observable_way` pins after its deliberate counter
    /// spike). That is not a lost optimisation; it is a hang.
    ///
    /// ## What is kept, and what it costs
    /// The ABSOLUTE reading still lands in `observed`, so [`Pacer::should_cool_down`] and the
    /// caller's own `weight_soft_limit` guard keep working unchanged — and those two are the guards
    /// that actually prevent a 429/418, because each compares an absolute reading against an
    /// absolute threshold and attributes nothing to a request. `per_request` (and `min_delta`, which
    /// is only ever persisted) are left exactly where the SEQUENTIAL phase measured them.
    ///
    /// ⚠ The accepted residual: a venue that RE-PRICES the endpoint mid-flight is no longer caught
    /// by the delta — it is caught by [`Pacer::should_cool_down`], coarsely. At the worst credible
    /// re-price (binance spot 2 -> 5) a pager still pacing on the stale 2 would spend the venue's
    /// full published ceiling instead of the targeted 40 % of it, the counter would cross the window
    /// target inside the first ~24 s, and every lane would pause for the caller's cooldown. Under
    /// the ceiling, but with no margin left — a brake, not a target. The alternative was to keep
    /// inferring from a delta that cannot be interpreted, which stalls the pager on ordinary traffic
    /// rather than on a re-price.
    ///
    /// **Deliberately NOT `min_delta`.** That floor is documented in [`Pacer::observe`] as being for
    /// PERSISTENCE only, never for the live delay, because a floor would MISS a genuine re-price and
    /// pace over budget into a 418. That decision stands and is unchanged: the fix here is to stop
    /// feeding the estimator input it cannot interpret, not to swap it for one whose stated failure
    /// mode is worse than the one being fixed.
    pub fn observe_absolute(&mut self, used_weight: u64) {
        // `last_used` advances too: it is the delta BASELINE, and leaving it at the sequential
        // phase's last reading would make any later `observe` subtract across the whole concurrent
        // phase in one go — the exact over-estimate this method exists to avoid.
        self.last_used = Some(used_weight);
        self.observed = used_weight;
    }

    /// Feed one COMPLETED request: how long it actually took on the wire, and (when the venue sent
    /// one) its used-weight counter — which is forwarded verbatim to [`Pacer::observe`], so a caller
    /// that measures never has to call both.
    ///
    /// `elapsed` is the caller's own wall clock around the request, and it is the half of the pacing
    /// arithmetic the pacer cannot see: the real inter-request gap is `next_delay() + elapsed`, and
    /// a pacer told only about the sleep systematically UNDER-shoots its utilization target by
    /// whatever the round trip costs (see the module doc's measured 23 %-instead-of-40 %). The
    /// estimate is an EWMA ([`REQUEST_TIME_ALPHA`]); the FIRST sample seeds it outright, since
    /// blending against a fabricated prior would only slow convergence.
    ///
    /// ⚠ Measure the REQUEST, not the retry loop, where the caller can tell them apart. A duration
    /// that swallowed a 429 `Retry-After` sleep is not a request time, and it inflates the estimate
    /// in the direction that shortens the next sleep. The damage is bounded — the EWMA caps one
    /// sample's pull and the delay floors at [`MIN_DELAY_SECS`], so the worst case is the target
    /// interval's own width, not a hammer — but it is noise the caller can simply not introduce.
    pub fn observe_request(&mut self, used_weight: Option<u64>, elapsed: Duration) {
        // A `Duration` is non-negative and finite by construction, so there is no NaN/negative
        // sanitation to do here; an ABSURD one (`Duration::MAX`) stays finite and is handled where
        // it matters — `next_delay` clamps and `eta` saturates, neither panics.
        let secs = elapsed.as_secs_f64();
        self.observed_request_secs = Some(match self.observed_request_secs {
            Some(prev) => prev * (1.0 - REQUEST_TIME_ALPHA) + secs * REQUEST_TIME_ALPHA,
            None => secs,
        });
        // A live measurement DROPS any persisted prior whole rather than blending it in — see the
        // module doc's rule 1. Clearing it here (instead of just letting `observed` win the `or`)
        // is what makes that law visible at the one site that can violate it.
        self.seeded_request_secs = None;
        self.samples = self.samples.saturating_add(1);
        if let Some(w) = used_weight {
            self.observe(w);
        }
    }

    /// How long to sleep before the NEXT request.
    ///
    /// Discovered: spending `per_minute_target` weight per minute at `per_request` weight per
    /// request permits `per_minute_target / per_request` requests per minute, so the target GAP is
    /// `60s * per_request / per_minute_target` — of which the request itself already consumes
    /// whatever [`Pacer::observe_request`] measured, so only the remainder is slept. Bounded into
    /// [`MIN_DELAY_SECS`]..=[`MAX_DELAY_SECS`], which is what makes a request slower than the whole
    /// target interval floor the delay instead of computing a negative one, and what keeps the
    /// value representable as a `Duration` whatever the counter reported. With nothing observed the
    /// subtrahend is `0.0`, i.e. exactly the pre-measurement arithmetic.
    ///
    /// Fixed: the caller's `page_delay`, EXACTLY, whatever has been observed. This is the one method
    /// that branches on the mode, and the branch is deliberate — see the module doc. A `Fixed` pacer
    /// measures and reports; it does not steer, because there is no discovered ceiling for a derived
    /// delay to be a fraction OF, and a delay derived from a stopwatch alone would be un-checkable.
    pub fn next_delay(&self) -> Duration {
        match self.mode {
            Mode::Discovered { per_minute_target, .. } => {
                let per_req = self.per_request.max(MIN_WEIGHT);
                // Both operands are finite and >= 1 by construction, so the quotient is finite and
                // positive; subtracting a finite, non-negative observation keeps it finite, and the
                // clamp then makes `from_secs_f64` total (it panics on NaN/negative/overflow).
                let target_gap = 60.0 * per_req / per_minute_target;
                let secs = (target_gap - self.request_secs_estimate().unwrap_or(0.0))
                    .clamp(MIN_DELAY_SECS, MAX_DELAY_SECS);
                Duration::from_secs_f64(secs)
            }
            Mode::Fixed(page_delay) => page_delay,
        }
    }

    /// The target inter-request GAP — the whole spacing the venue should see between two successive
    /// requests, BEFORE any request time is subtracted from it.
    ///
    /// [`Pacer::next_delay`] is this minus the measured round trip, because a SEQUENTIAL pager's
    /// spacing is `sleep + request`. A CONCURRENT pager's is not: with several requests in flight the
    /// round trips overlap, and the spacing is set by whatever admits the requests. Such a caller
    /// (see [`crate::concurrent::LaneGate`]) must therefore pace on this number and NOT on
    /// `next_delay`, or it would subtract a round trip that no longer serialises and spend the
    /// difference twice over.
    ///
    /// ⚠ In `Mode::Fixed` this is the caller's `page_delay`, which is NOT what that mode's pager
    /// spaces its requests by today — a fixed pager's real spacing is `page_delay + request`. So
    /// driving a shared gate off this value at a no-budget venue would be a RATE INCREASE dressed as
    /// a refactor, which is exactly why [`Pacer::suggested_lanes`] refuses to hand out more than one
    /// lane there. The accessor answers honestly in both modes; the safety lives at the lane count.
    ///
    /// Clamped into [`MIN_DELAY_SECS`]..=[`MAX_DELAY_SECS`] like `next_delay`, so an absurd inferred
    /// weight cannot produce an un-representable `Duration`. (The clamp is applied AFTER the divide
    /// here and BEFORE the subtraction there — the two are deliberately not re-expressed in terms of
    /// each other, so this accessor cannot move a single existing sleep.)
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

    /// How many requests must be IN FLIGHT for a paged backfill to actually reach
    /// [`Pacer::target_gap`] — i.e. the derived degree of concurrency, capped at `max`.
    ///
    /// `ceil(request_time / target_gap)`, because a sequential pager's floor on spacing is the round
    /// trip itself: it cannot issue requests closer together than one request takes, no matter how
    /// small the sleep. When the target gap is the wider of the two the venue's BUDGET is the binding
    /// constraint, one request at a time already meets it, and this answers `1`.
    ///
    /// MEASURED (the CI box, 2026-08-04) — the two binance hosts land on opposite sides of that line:
    /// * **fapi**: 2400 weight/min at the 0.40 default and weight-5 pages ⇒ a 312 ms target gap
    ///   against a ~280 ms round trip ⇒ **1 lane**. Perp backfills are already at target; concurrency
    ///   buys them nothing until `utilization` is raised past ~0.45, where they cross the line.
    /// * **spot**: 6000 weight/min at the same 0.40 and weight-2 pages ⇒ a **50 ms** target gap
    ///   against the same round trip ⇒ **6 lanes**. Sequentially, spot backfills spend ~18 % of the
    ///   budget the operator asked for, and no delay tuning can close that — the sleep is already
    ///   floored.
    ///
    /// ⚠ **"weight-2 pages" is a MEASURED cost, and a caller that has not measured twice does not
    /// have it.** This method reads whatever `per_request` currently is, and [`Pacer::observe`] needs
    /// a DELTA — so after a single reading the estimate is still [`SEED_WEIGHT`] (5), which on spot
    /// reads the same host as a 125 ms gap and **3 lanes**, and paces the whole run at 40 % of the
    /// 40 % that was asked for. The number above is the one a caller gets after TWO sequential
    /// readings, or from a [`PaceSample`] seed. `vike_binance::family::klines`'s `PROBE_PAGES` is
    /// that rule made concrete on the caller's side; nothing here can enforce it.
    ///
    /// Returns `1` — never more — in three cases, each a refusal rather than a default:
    /// * **nothing measured** (no live observation, no seed): the ratio's numerator is unknown, and
    ///   guessing it is how a lane count becomes a hardcoded number.
    /// * **`Mode::Fixed`**: a venue that publishes no budget has no ceiling for an aggregate rate to
    ///   be a fraction OF. Concurrency there raises the venue-facing rate against a number nobody
    ///   machine-checked — the same un-checkable guess this module exists to remove (see
    ///   [`Pacer::target_gap`]).
    /// * **a non-finite or non-positive ratio**: absurd input resolves to the slow direction.
    ///
    /// `max` is a caller-side bound on THREADS, not on rate — see
    /// [`crate::concurrent::MAX_LANES`]. The rate is `1 / target_gap` whatever this returns.
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
        // `want` is finite and > 1 here; the clamp bounds it before the cast, so an absurd ratio
        // saturates at `max` instead of wrapping through `as usize`.
        let capped = want.min(max.max(1) as f64) as usize;
        capped.clamp(1, max.max(1))
    }

    /// How long `remaining_requests` more requests will take at the CURRENT pace, or `None` while
    /// nothing has been measured.
    ///
    /// `None` is the honest answer while nothing has been measured AND nothing was seeded
    /// ([`Pacer::seed`]): an ETA built from the sleep alone is exactly the arithmetic that read 40 %
    /// where the venue metered 23 %, and a wrong ETA on a 24-month backfill is worse than no ETA. A
    /// seeded prior IS a measurement — last run's — so it answers from the first page, which is the
    /// visible payoff of persisting one. After that it is
    /// `remaining * (observed_request_time + next_delay())` — the full per-request cycle, which is
    /// the only spacing the venue ever sees.
    ///
    /// Answers in BOTH modes. It is a report, not a target, so a discovered budget is irrelevant to
    /// it: in `Fixed` the cycle is `measured request + the caller's constant`, which is exactly the
    /// spacing a fixed-delay pager runs at. [`remaining_pages`] is the multiplicand a paged backfill
    /// hands it.
    ///
    /// Saturates at `Duration::MAX` rather than panicking: `remaining_requests` is a caller estimate
    /// and the product of two unbounded numbers is not required to be representable.
    pub fn eta(&self, remaining_requests: u64) -> Option<Duration> {
        let request_secs = self.request_secs_estimate()?;
        let cycle_secs = request_secs + self.next_delay().as_secs_f64();
        let total = remaining_requests as f64 * cycle_secs;
        Some(Duration::try_from_secs_f64(total).unwrap_or(Duration::MAX))
    }

    /// True when observed usage has crossed the target — caller should cool down.
    ///
    /// Compares the latest counter value against `window_target`, the target expressed in the
    /// venue's OWN interval, because that is the unit the cumulative counter is denominated in.
    /// Crossing is not an error: it means this window's share is spent (possibly by another client
    /// on the same IP), and the caller should idle until the window rolls — at which point the next
    /// [`Pacer::observe`] sees the reset counter and this goes `false` again on its own.
    pub fn should_cool_down(&self) -> bool {
        match self.mode {
            Mode::Discovered { window_target, .. } => self.observed as f64 >= window_target,
            // No discovered budget ⇒ no target ⇒ nothing to cross.
            Mode::Fixed(_) => false,
        }
    }
}

/// Estimated pages still to fetch across a `span_ms`-wide window at `rows_per_page` rows per
/// response — [`Pacer::eta`]'s multiplicand, and the only unknown in it.
///
/// `None` when the interval string does not parse ([`vike_model::time::interval_ms`]) or one page
/// spans nothing, because an ETA is a courtesy and a wrong one is worse than none — `0` (the window
/// fits in the page just fetched) is a real answer and stays one. A negative span (a cursor already
/// past the end) is `0`, never a wrap.
///
/// Approximate BY CONSTRUCTION even when it parses — the venue serves no rows for a gap in its own
/// history (a delisted week, a maintenance window), so a real backfill can finish in fewer pages
/// than the window's width implies. It is a progress hint, not a schedule.
///
/// `rows_per_page` is a PARAMETER because the four kline pagers disagree about it and one of them
/// cannot name a constant at all: binance/aster pass their documented 1000-row cap, bybit 1000,
/// okx 100 — while deribit's ~5001 cap is undocumented and deliberately never hardcoded (see
/// `vike_deribit::data`), so its pager passes the row count the first page actually returned. A
/// venue-side change to a cap can therefore not silently corrupt the estimate.
pub fn remaining_pages(span_ms: i64, interval: &str, rows_per_page: usize) -> Option<u64> {
    let rows = i64::try_from(rows_per_page).ok()?;
    let page_span = vike_model::time::interval_ms(interval)?.checked_mul(rows)?;
    if page_span <= 0 {
        return None;
    }
    Some((span_ms.max(0) / page_span) as u64)
}

/// Clamp a caller-supplied utilization into the sane band, resolving NaN conservatively. Free
/// function (not a method) so the clamping law is testable without building a budget.
fn clamp_utilization(utilization: f64) -> f64 {
    if utilization.is_nan() {
        return NAN_UTILIZATION;
    }
    utilization.clamp(MIN_UTILIZATION, MAX_UTILIZATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live fapi shape (`vike_binance::data`'s `BINANCE_KLINE_PERP`): `REQUEST_WEIGHT`/`MINUTE`
    /// = 2400, measured `/klines?limit=1000` cost = 5.
    fn fapi_budget() -> WeightBudget {
        WeightBudget { limit: 2400, interval_secs: 60 }
    }

    /// Weight actually spent per minute if the caller sleeps `delay` between requests, each costing
    /// `per_request`. This is the quantity the pacer is steering, so asserting on IT (rather than on
    /// a delay constant) is what proves the pacing rather than the arithmetic.
    fn weight_per_minute(delay: Duration, per_request: f64) -> f64 {
        (60.0 / delay.as_secs_f64()) * per_request
    }

    /// Weight spent per minute END TO END — the gap the VENUE sees, `sleep + request`, which is the
    /// quantity the utilization target is a fraction of. [`weight_per_minute`] is the same figure
    /// with the request time assumed free, i.e. the arithmetic that read 40 % where the venue
    /// metered 23 %.
    fn end_to_end_weight_per_minute(delay: Duration, request: Duration, per_request: f64) -> f64 {
        (60.0 / (delay.as_secs_f64() + request.as_secs_f64())) * per_request
    }

    /// The pacer's current request-time estimate, read back through the PUBLIC surface: `eta(1)` is
    /// `estimate + next_delay()` by definition, so subtracting the delay recovers it. Reading the
    /// private field would be easier and would prove less — this also pins that `eta` is built on
    /// the full cycle and not on the sleep alone.
    fn estimate_secs(p: &Pacer) -> f64 {
        p.eta(1).expect("estimate_secs needs an observation").as_secs_f64()
            - p.next_delay().as_secs_f64()
    }

    fn assert_close(got: f64, want: f64, tol: f64) {
        assert!((got - want).abs() <= tol, "expected ~{want}, got {got}");
    }

    #[test]
    fn discovered_budget_paces_to_the_targeted_share() {
        // 2400/min at 40% ⇒ 960 weight/min, whatever the per-request weight turns out to be.
        let mut p = Pacer::discovered(fapi_budget(), 0.4);

        // Before any observation the seed weight (1) is assumed: 960 requests/min.
        assert_close(weight_per_minute(p.next_delay(), SEED_WEIGHT), 960.0, 0.5);

        // Once the real weight-5 cost is observed the pace holds the SAME weight/min at 1/5 the
        // request rate — the whole point: the target is weight, not requests.
        p.observe(100);
        p.observe(105);
        assert_close(weight_per_minute(p.next_delay(), 5.0), 960.0, 0.5);
        assert_close(p.next_delay().as_secs_f64(), 0.3125, 0.001); // 60 * 5 / 960
    }

    #[test]
    fn per_minute_normalization_makes_sub_minute_intervals_pace_alike() {
        // "600 per 10s" and "3600 per 60s" are the same budget; only the counter's denomination
        // differs, which is exactly why cooldown uses `limit` and pacing uses `per_minute()`.
        let mut short = Pacer::discovered(WeightBudget { limit: 600, interval_secs: 10 }, 0.5);
        let mut long = Pacer::discovered(WeightBudget { limit: 3600, interval_secs: 60 }, 0.5);
        assert_eq!(short.next_delay(), long.next_delay(), "same weight/min ⇒ same pace");

        // ...but the in-window cooldown targets differ (300 vs 1800), because the venue's counter
        // resets every 10s in one case and every 60s in the other.
        short.observe(300);
        long.observe(300);
        assert!(short.should_cool_down(), "300 of a 600/10s budget at 50% is the whole share");
        assert!(!long.should_cool_down(), "300 is nowhere near 1800 of the 3600/60s budget");
    }

    #[test]
    fn observe_infers_the_per_request_weight_from_the_delta() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert_eq!(p.per_request, SEED_WEIGHT, "seeded until the counter speaks");
        p.observe(100);
        assert_eq!(p.per_request, SEED_WEIGHT, "one sample is not a delta");
        p.observe(105);
        assert_eq!(p.per_request, 5.0, "a weight-5 delta infers 5 — never hardcoded");
    }

    #[test]
    fn a_repriced_endpoint_self_corrects_on_the_next_observation() {
        // The failure the module exists to prevent: the venue re-prices /klines 2 -> 5 and the
        // pager must slow down by itself instead of tripling consumption into a 418.
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(10);
        p.observe(12); // weight 2
        let cheap = p.next_delay();
        p.observe(17); // the SAME endpoint now costs 5
        let dear = p.next_delay();
        assert!(dear > cheap, "a heavier request must widen the gap, not keep the old pace");
        // ...and still lands on the same targeted weight/min, which is the invariant.
        assert_close(weight_per_minute(cheap, 2.0), 960.0, 0.5);
        assert_close(weight_per_minute(dear, 5.0), 960.0, 0.5);
    }

    #[test]
    fn an_unmoved_counter_keeps_the_estimate() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(100);
        p.observe(105);
        let paced = p.next_delay();
        p.observe(105); // venue did not move the counter — no information
        assert_eq!(p.per_request, 5.0, "a zero delta must not be read as a free request");
        assert_eq!(p.next_delay(), paced);
    }

    #[test]
    fn a_counter_reset_is_a_new_window_not_negative_weight() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(2000);
        p.observe(2005);
        assert_eq!(p.per_request, 5.0);
        assert!(p.should_cool_down(), "2005 is past the 960 share");

        p.observe(3); // the interval rolled: 3 < 2005
        assert_eq!(p.per_request, 5.0, "a reset must not overwrite the estimate");
        assert!(p.per_request.is_sign_positive() && p.per_request.is_finite());
        assert!(!p.should_cool_down(), "a fresh window has spent almost nothing");
        // The delay is still the honest weight-5 pace — no wrapped u64 turned into a 60s stall.
        assert_close(p.next_delay().as_secs_f64(), 0.3125, 0.001);

        // ...and inference resumes from the NEW baseline.
        p.observe(9);
        assert_eq!(p.per_request, 6.0);
    }

    #[test]
    fn should_cool_down_fires_on_crossing_the_target_and_not_before() {
        // `0.5` (not the `0.4` the pacing tests use) is deliberate: it is exactly representable, so
        // `2400 * 0.5` is exactly 1200.0 and the AT-the-target assertion below tests the boundary
        // rather than the last ulp of a rounded product. The pacing tests assert with a tolerance
        // precisely because they cannot make that claim.
        let mut p = Pacer::discovered(fapi_budget(), 0.5); // window target = 1200
        assert!(!p.should_cool_down(), "nothing observed yet");
        p.observe(1199);
        assert!(!p.should_cool_down(), "one unit short is not crossed");
        p.observe(1200);
        assert!(p.should_cool_down(), "at the target is crossed");
        p.observe(2399);
        assert!(p.should_cool_down(), "and stays crossed while the window holds");
    }

    #[test]
    fn fallback_always_returns_its_fixed_delay() {
        let mut p = Pacer::fallback(Duration::from_millis(150));
        assert_eq!(p.next_delay(), Duration::from_millis(150));
        assert!(!p.should_cool_down(), "no discovered budget ⇒ no target to cross");
        // Observations are recorded but change nothing: without a budget there is no rate to steer.
        p.observe(100);
        p.observe(6000);
        assert_eq!(p.next_delay(), Duration::from_millis(150));
        assert!(!p.should_cool_down());
        p.observe(1); // reset, too
        assert_eq!(p.next_delay(), Duration::from_millis(150));
    }

    #[test]
    fn zero_utilization_clamps_instead_of_dividing_by_zero() {
        let p = Pacer::discovered(fapi_budget(), 0.0);
        let d = p.next_delay();
        assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO, "no NaN, no zero delay: {d:?}");
        // Clamped to the floor. DERIVED from the constant, not hardcoded: this expectation was
        // `24.0` (a 1% floor) until the bounds were unified onto vike-model's operator-facing
        // range, and a literal would simply have gone stale rather than saying so.
        let floor_wpm = fapi_budget().per_minute() as f64 * MIN_UTILIZATION;
        assert_close(weight_per_minute(d, SEED_WEIGHT), floor_wpm, 0.5);
    }

    #[test]
    fn oversized_utilization_clamps_below_the_full_budget() {
        let p = Pacer::discovered(fapi_budget(), 5.0);
        // MAX of 2400, NOT 500% — a 5x over-budget hammer is exactly the 418 this prevents.
        // Never 100%: the used-weight counter is per-IP and shared with everything else on this
        // egress (the live recorder included), so the last slice is headroom we do not own.
        let ceiling_wpm = fapi_budget().per_minute() as f64 * MAX_UTILIZATION;
        assert_close(weight_per_minute(p.next_delay(), SEED_WEIGHT), ceiling_wpm, 1.0);
        assert!(p.next_delay() >= Duration::from_secs_f64(MIN_DELAY_SECS));
    }

    #[test]
    fn negative_and_nan_utilization_resolve_to_the_conservative_floor() {
        assert_eq!(clamp_utilization(-1.0), MIN_UTILIZATION);
        assert_eq!(clamp_utilization(f64::NAN), MIN_UTILIZATION);
        assert_eq!(clamp_utilization(f64::INFINITY), MAX_UTILIZATION);
        assert_eq!(clamp_utilization(f64::NEG_INFINITY), MIN_UTILIZATION);
        // A sane value passes through untouched.
        assert_eq!(clamp_utilization(0.4), 0.4);
        // And every one of them still yields a usable delay rather than a panic.
        for u in [-1.0, 0.0, f64::NAN, f64::INFINITY, 5.0, 0.4] {
            let d = Pacer::discovered(fapi_budget(), u).next_delay();
            assert!(d > Duration::ZERO && d <= Duration::from_secs_f64(MAX_DELAY_SECS));
        }
    }

    #[test]
    fn a_degenerate_budget_neither_divides_by_zero_nor_cools_down_instantly() {
        // Discovery that answered with a zero limit is a venue/parse bug, not a licence to panic.
        let mut p = Pacer::discovered(WeightBudget { limit: 0, interval_secs: 60 }, 0.5);
        let d = p.next_delay();
        assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO);
        assert!(!p.should_cool_down(), "zero observed is not yet past the 1-unit floor target");
        p.observe(1);
        assert!(p.should_cool_down(), "and the floor target is then immediately crossed");
    }

    #[test]
    fn an_absurd_inferred_weight_is_bounded_not_unrepresentable() {
        // A counter that jumps by a preposterous amount (venue glitch / shared-IP burst attributed
        // to us) must saturate at the ceiling — `Duration::from_secs_f64` panics on overflow.
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(0);
        p.observe(u64::MAX);
        assert_eq!(p.next_delay(), Duration::from_secs_f64(MAX_DELAY_SECS));
    }

    #[test]
    fn a_huge_budget_is_floored_not_a_busy_loop() {
        let p = Pacer::discovered(WeightBudget { limit: u64::MAX, interval_secs: 1 }, 0.9);
        assert_eq!(p.next_delay(), Duration::from_secs_f64(MIN_DELAY_SECS));
    }

    // ----- request-time compensation ------------------------------------------------------------

    /// THE measured case, reproduced as arithmetic (the CI box, 2026-08-04, 24 months of `BTCUSDT.P`):
    /// a 2400/min fapi budget at 40 % with weight-5 pages and a ~280 ms round trip must spend its
    /// 960 weight/min **end to end** — not the ~506 the sleep-only pacer actually delivered, which
    /// metered as 556 (~23 % of the ceiling) instead of the promised 40 %.
    #[test]
    fn the_measured_fapi_shape_hits_its_target_end_to_end() {
        let rtt = Duration::from_millis(280);
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe_request(Some(100), rtt);
        p.observe_request(Some(105), rtt); // weight 5, and the estimate is still exactly 280ms

        let d = p.next_delay();
        // 60 * 5 / 960 = 312.5ms of TARGET GAP, of which the request already spent 280.
        assert_close(d.as_secs_f64(), 0.0325, 1e-6);
        assert_close(end_to_end_weight_per_minute(d, rtt, 5.0), 960.0, 1.0);
        assert_close(60.0 / (d.as_secs_f64() + rtt.as_secs_f64()), 192.0, 0.2); // requests/min

        // The regression this exists to prevent: sleeping the WHOLE 312.5ms would have spaced
        // requests 592.5ms apart — ~506 weight/min, roughly half the target.
        let sleep_only = Duration::from_secs_f64(0.3125);
        assert!(
            end_to_end_weight_per_minute(sleep_only, rtt, 5.0) < 550.0,
            "the sleep-only pace must be the SLOW one, or this test proves nothing"
        );
    }

    #[test]
    fn next_delay_shrinks_by_the_observed_request_time() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(100);
        p.observe(105);
        let unmeasured = p.next_delay(); // 312.5ms — the pre-measurement arithmetic
        assert_close(unmeasured.as_secs_f64(), 0.3125, 1e-6);

        p.observe_request(None, Duration::from_millis(100));
        let measured = p.next_delay();
        assert_close(measured.as_secs_f64(), 0.2125, 1e-6);
        assert!(measured < unmeasured);
        // ...and the END-TO-END pace is unchanged by the correction — that is the whole point.
        assert_close(
            end_to_end_weight_per_minute(measured, Duration::from_millis(100), 5.0),
            960.0,
            1.0,
        );
    }

    #[test]
    fn a_request_slower_than_the_target_interval_floors_the_delay() {
        // 5s per request against a 312.5ms target gap: there is no negative sleep, no panic, and no
        // zero-delay hammer — just the floor. (The venue is metering us below target either way;
        // the pacer cannot make a slow request faster.)
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe_request(Some(100), Duration::from_secs(5));
        p.observe_request(Some(105), Duration::from_secs(5));
        let d = p.next_delay();
        assert_eq!(d, Duration::from_secs_f64(MIN_DELAY_SECS));
        assert!(d > Duration::ZERO && d.as_secs_f64().is_finite());
    }

    #[test]
    fn an_absurd_request_time_neither_panics_nor_goes_negative() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe_request(None, Duration::MAX);
        assert_eq!(p.next_delay(), Duration::from_secs_f64(MIN_DELAY_SECS));
        // And the ETA saturates rather than overflowing `Duration::from_secs_f64` into a panic.
        assert_eq!(p.eta(u64::MAX), Some(Duration::MAX));
    }

    #[test]
    fn the_request_time_estimate_is_smoothed_not_latest_wins() {
        // A budget whose target gap is a whole 2s, so the delay does not floor and the smoothing is
        // visible in `next_delay` itself: 300/min at 50% = 150 weight/min, 60 * 5 / 150 = 2s.
        let mut p = Pacer::discovered(WeightBudget { limit: 300, interval_secs: 60 }, 0.5);
        p.observe(10);
        p.observe(15); // weight 5
        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(280));
        }
        assert_close(estimate_secs(&p), 0.280, 1e-6);
        assert_close(p.next_delay().as_secs_f64(), 1.720, 1e-6);

        // ONE 5x-slow sample (1.4s). Latest-wins would make the estimate 1.4 and the delay 0.6.
        p.observe_request(None, Duration::from_millis(1400));
        let est = estimate_secs(&p);
        assert_close(est, 0.560, 1e-6); // 0.28*0.75 + 1.4*0.25
        assert!(est < 1.4 * 0.5, "one outlier must not become the estimate: {est}");
        assert!(p.next_delay() > Duration::from_millis(1300), "and must not gut the sleep");

        // ...and it decays back toward the truth as normal pages resume.
        for _ in 0..6 {
            p.observe_request(None, Duration::from_millis(280));
        }
        assert!(estimate_secs(&p) < 0.35, "the outlier's pull must fade, not persist");

        // A genuine STEP change still converges — smoothing must not mean deafness.
        for _ in 0..20 {
            p.observe_request(None, Duration::from_millis(900));
        }
        assert_close(estimate_secs(&p), 0.900, 0.01);
    }

    #[test]
    fn eta_is_none_until_a_request_is_observed_then_scales_linearly() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert_eq!(p.eta(1_000), None, "nothing measured ⇒ no ETA, not a guess from the sleep");
        p.observe(100);
        p.observe(105);
        assert_eq!(p.eta(1_000), None, "a WEIGHT observation is not a duration observation");

        let rtt = Duration::from_millis(280);
        p.observe_request(Some(110), rtt); // weight 5 again, estimate seeded at 280ms

        // One full cycle is the target gap itself: 280ms of request + 32.5ms of sleep.
        assert_close(p.eta(1).unwrap().as_secs_f64(), 0.3125, 1e-6);
        assert_close(p.eta(1_000).unwrap().as_secs_f64(), 312.5, 1e-3);
        assert_close(p.eta(2_000).unwrap().as_secs_f64(), 625.0, 1e-3);
        assert_eq!(p.eta(0), Some(Duration::ZERO));

        // The shape the pager reports: 24 months of 1m bars ≈ 1051 pages of 1000.
        let eta = p.eta(1_051).unwrap();
        assert!(
            eta > Duration::from_secs(320) && eta < Duration::from_secs(340),
            "expected ~5m28s for the measured shape, got {eta:?}"
        );
    }

    #[test]
    fn observe_request_is_observe_plus_a_duration() {
        // Same weight sequence, zero-length requests ⇒ byte-identical to the weight-only path. The
        // forwarding contract, so a measuring caller never has to call both.
        let mut weight_only = Pacer::discovered(fapi_budget(), 0.4);
        let mut measured = Pacer::discovered(fapi_budget(), 0.4);
        for w in [100, 105, 110, 2_000] {
            weight_only.observe(w);
            measured.observe_request(Some(w), Duration::ZERO);
        }
        assert_eq!(weight_only.next_delay(), measured.next_delay());
        assert_eq!(weight_only.should_cool_down(), measured.should_cool_down());
        assert_eq!(weight_only.per_request_weight(), measured.per_request_weight());

        // A `None` weight records ONLY the duration — no phantom counter movement, so the weight
        // estimate stays where the venue last put it.
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(10);
        p.observe(12); // weight 2
        p.observe_request(None, Duration::from_millis(280));
        p.observe_request(None, Duration::from_millis(280));
        assert_eq!(p.per_request_weight(), 2.0, "a duration must not be read as a weight delta");
        assert_eq!(p.last_used, Some(12), "nor move the delta baseline");
    }

    /// "Entirely" scopes to the DELAY, which is the only thing `Fixed` withholds — the ETA below
    /// answers from the very same observation. See `fixed_mode_measures_and_reports_but_never_steers`
    /// for the full contract.
    #[test]
    fn fixed_mode_ignores_observed_request_times_entirely() {
        let mut p = Pacer::fallback(Duration::from_millis(150));
        assert!(!p.is_discovered());
        p.observe_request(Some(100), Duration::from_secs(5));
        assert_eq!(p.next_delay(), Duration::from_millis(150), "a fixed delay is a fixed delay");
        p.observe_request(None, Duration::from_millis(1));
        assert_eq!(p.next_delay(), Duration::from_millis(150));
        assert!(!p.should_cool_down());

        // The ETA is still answerable — it is a measurement, not a target — and it counts the
        // fixed sleep plus the measured request, like everywhere else.
        let est = estimate_secs(&p);
        assert_close(p.eta(10).unwrap().as_secs_f64(), 10.0 * (est + 0.150), 1e-6);
    }

    /// THE contract this mode exists to state, on the MEASURED okx shape (the CI box, 2026-08-04: spot
    /// and perp both average 476–486 ms per page against a hardcoded 200 ms `PAGE_DELAY`). A `Fixed`
    /// pacer RECORDS, REPORTS and answers an ETA — and sleeps exactly its configured delay while
    /// doing it.
    ///
    /// The `next_delay` half is the load-bearing one: it is what makes measuring a no-budget venue
    /// SAFE rather than a pacing change wearing a measurement's clothes.
    #[test]
    fn fixed_mode_measures_and_reports_but_never_steers() {
        let page_delay = Duration::from_millis(200);
        let mut p = Pacer::fallback(page_delay);
        assert!(!p.is_discovered());
        assert_eq!(p.measured(), None, "nothing timed yet ⇒ nothing to report");
        assert_eq!(p.eta(1_000), None, "and no ETA invented out of the sleep alone");
        assert_eq!(p.next_delay(), page_delay);

        // Four pages at the measured okx cost. No weight header — this venue publishes no counter.
        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(480));
        }

        // (1) It RECORDS: `measured()` hands out a persistable sample, with NO budget attached —
        // which is what confines it to another fallback pacer (`seed`'s rule 3).
        let m = p.measured().expect("a fixed pager times its pages like any other");
        assert_eq!(m.request_ms, 480, "the EWMA of four identical samples is the sample");
        assert_eq!(m.samples, 4);
        assert_eq!(m.budget_per_min, None, "nothing was discovered, so nothing is claimed");
        assert!(m.is_usable(), "and it is good enough for the pace book to store");

        // (2) It REPORTS an ETA over the FULL cycle — `sleep + request`, the only spacing the venue
        // sees. This is the number the ~480ms measurement makes available and the 200ms constant
        // alone never could: 1000 pages is ~11m20s, not the ~3m20s the sleep implies.
        assert_close(p.eta(1_000).unwrap().as_secs_f64(), 1_000.0 * (0.480 + 0.200), 1e-6);
        assert_eq!(p.eta(0), Some(Duration::ZERO));

        // (3) It does NOT steer. Whatever is observed — including the absurd — the sleep is the
        // caller's constant, to the nanosecond.
        assert_eq!(p.next_delay(), page_delay);
        for absurd in
            [Duration::ZERO, Duration::from_micros(1), Duration::from_secs(30), Duration::MAX]
        {
            p.observe_request(Some(10_000), absurd);
            assert_eq!(p.next_delay(), page_delay, "{absurd:?} moved the fixed delay");
            assert!(!p.should_cool_down(), "no budget ⇒ nothing to cross, whatever the counter");
        }
    }

    /// The cross-run payoff for a no-budget venue: a stored sample answers the ETA on page ZERO
    /// (where an unseeded fallback pacer must refuse), and STILL cannot speed the sleep up. A prior
    /// is evidence about cost, never permission to go faster.
    #[test]
    fn a_seeded_fixed_pacer_answers_from_page_zero_and_still_sleeps_its_constant() {
        let page_delay = Duration::from_millis(200);
        let mut p = Pacer::fallback(page_delay);
        assert_eq!(p.eta(1_000), None);
        assert!(p.seed(&PaceSample {
            request_ms: 480,
            per_request_weight: 1.0,
            budget_per_min: None,
            samples: 12,
        }));
        assert_close(p.eta(1_000).unwrap().as_secs_f64(), 1_000.0 * (0.480 + 0.200), 1e-6);
        assert_eq!(p.next_delay(), page_delay, "a prior must not shorten a fixed delay");
        // ...and the prior is not a measurement: a run that then times nothing reports nothing.
        assert_eq!(p.measured(), None);
    }

    #[test]
    fn is_discovered_distinguishes_the_two_modes() {
        assert!(Pacer::discovered(fapi_budget(), 0.4).is_discovered());
        assert!(!Pacer::fallback(Duration::from_millis(150)).is_discovered());
    }

    #[test]
    fn per_request_weight_reports_the_seed_then_the_inferred_cost() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert_eq!(p.per_request_weight(), SEED_WEIGHT, "seeded until the counter speaks");
        p.observe(100);
        p.observe(102);
        assert_eq!(p.per_request_weight(), 2.0);
    }

    // ----- the ETA's multiplicand ---------------------------------------------------------------

    /// The page count is the window's width over ONE page's span, `None` when the interval is not a
    /// vike interval string at all (an ETA is a courtesy — a wrong one is worse than none). Pinned
    /// across the four pagers' differing page sizes, since that is why it is a parameter.
    #[test]
    fn remaining_pages_divides_the_window_by_one_pages_span() {
        // 24 months of 1m bars — the measured binance shape. 730d = 63_072_000_000ms.
        let two_years = 730 * 86_400_000i64;
        assert_eq!(remaining_pages(two_years, "1m", 1_000), Some(1_051));
        // The SAME window costs 10x the pages on okx, whose history-candles cap is 100.
        assert_eq!(remaining_pages(two_years, "1m", 100), Some(10_512));
        // ...and a fifth of binance's on deribit, whose observed page is ~5001 rows.
        assert_eq!(remaining_pages(two_years, "1m", 5_001), Some(210));
        // Coarser bars ⇒ far fewer pages for the same window.
        assert_eq!(remaining_pages(two_years, "1h", 1_000), Some(17));
        assert_eq!(remaining_pages(two_years, "1d", 1_000), Some(0), "730 days fits in one page");
        // A window narrower than one page has nothing left after the page just fetched.
        assert_eq!(remaining_pages(1_000, "1m", 1_000), Some(0));
        // A NEGATIVE span (a cursor already past the end) is zero, never a wrap.
        assert_eq!(remaining_pages(-two_years, "1m", 1_000), Some(0));
        assert_eq!(remaining_pages(i64::MIN, "1m", 1_000), Some(0));
        // Unparseable / absurd intervals decline to guess instead of dividing by zero.
        assert_eq!(remaining_pages(two_years, "", 1_000), None);
        assert_eq!(remaining_pages(two_years, "1y", 1_000), None);
        assert_eq!(remaining_pages(two_years, "0m", 1_000), None);
        // A zero page size is "no page advances the cursor" — decline, never divide by zero.
        assert_eq!(remaining_pages(two_years, "1m", 0), None);
        // A page span that overflows i64 declines rather than wrapping into a tiny span and
        // reporting a preposterous page count.
        assert_eq!(remaining_pages(i64::MAX, "200000000d", 1_000), None);
        assert_eq!(remaining_pages(i64::MAX, "1m", usize::MAX), None);
    }

    /// The two halves compose into the line a paged backfill actually prints: pages x cycle.
    #[test]
    fn remaining_pages_feeds_the_eta_of_a_fixed_pager() {
        let mut p = Pacer::fallback(Duration::from_millis(200));
        p.observe_request(None, Duration::from_millis(480));
        // One day of 1m candles on okx's 100-row pages = 14 pages, at ~680ms per cycle.
        let pages = remaining_pages(86_400_000, "1m", 100).expect("a parseable interval");
        assert_eq!(pages, 14);
        assert_close(p.eta(pages).unwrap().as_secs_f64(), 14.0 * 0.680, 1e-6);
    }

    // ----- persisted pace: seed in, measurement out ----------------------------------------------

    /// A stored sample shaped like the MEASURED fapi run: weight-5 pages, ~280 ms round trip,
    /// against the 2400/min budget `fapi_budget()` publishes.
    fn fapi_sample(request_ms: u64, weight: f64) -> PaceSample {
        PaceSample {
            request_ms,
            per_request_weight: weight,
            budget_per_min: Some(2400),
            samples: 8,
        }
    }

    /// The payoff: run N+1's FIRST page is already paced at run N's measurement, instead of sleeping
    /// the whole target gap against the pessimistic `SEED_WEIGHT`. Same numbers the live-measured
    /// test above lands on, but with ZERO observations this run.
    #[test]
    fn a_seed_paces_the_very_first_page_at_last_runs_measurement() {
        let mut unseeded = Pacer::discovered(fapi_budget(), 0.4);
        let mut seeded = Pacer::discovered(fapi_budget(), 0.4);
        assert!(seeded.seed(&fapi_sample(280, 5.0)), "a matching sample applies");

        // Unseeded: SEED_WEIGHT (5) and no request time ⇒ the full 312.5ms target gap slept.
        assert_close(unseeded.next_delay().as_secs_f64(), 0.3125, 1e-6);
        // Seeded: the same weight-5 gap MINUS the 280ms the request is known to cost.
        assert_close(seeded.next_delay().as_secs_f64(), 0.0325, 1e-6);
        assert_close(
            end_to_end_weight_per_minute(seeded.next_delay(), Duration::from_millis(280), 5.0),
            960.0,
            1.0,
        );
        // ...and the ETA answers from page ZERO, where the unseeded pacer must refuse.
        assert_eq!(unseeded.eta(1_051), None, "nothing measured and nothing seeded ⇒ no ETA");
        let eta = seeded.eta(1_051).expect("a seeded pacer can answer");
        assert!(eta > Duration::from_secs(300) && eta < Duration::from_secs(360), "{eta:?}");

        // The seed is a PRIOR: the first live observation drops it whole and seeds the EWMA
        // outright, so from then on the two pacers agree given the same live samples.
        for p in [&mut unseeded, &mut seeded] {
            p.observe_request(Some(100), Duration::from_millis(400));
            p.observe_request(Some(105), Duration::from_millis(400));
        }
        assert_eq!(
            seeded.next_delay(),
            unseeded.next_delay(),
            "a prior must not outlive the truth"
        );
        assert_close(estimate_secs(&seeded), 0.400, 1e-6);
    }

    /// The cheap-endpoint case the constant costs every run: binance SPOT prices `/klines` at
    /// weight 2, but `SEED_WEIGHT` is 5, so an unseeded pager opens 2.5x slower than the venue
    /// permits. A stored weight fixes exactly that — and still paces UNDER the published ceiling.
    #[test]
    fn a_seeded_weight_replaces_the_pessimistic_constant() {
        let spot = WeightBudget { limit: 6000, interval_secs: 60 };
        let mut p = Pacer::discovered(spot, 0.4);
        assert_eq!(p.per_request_weight(), SEED_WEIGHT);
        assert!(p.seed(&PaceSample {
            request_ms: 120,
            per_request_weight: 2.0,
            budget_per_min: Some(6000),
            samples: 40,
        }));
        assert_eq!(p.per_request_weight(), 2.0, "the stored cost, not the constant");
        // 6000/min at 40% = 2400 weight/min; at weight 2 that is a 50ms gap, of which the request
        // is known to spend 120ms ⇒ the sleep floors, and the END-TO-END pace stays under budget.
        assert!(
            end_to_end_weight_per_minute(p.next_delay(), Duration::from_millis(120), 2.0) <= 6000.0,
            "a seed must never pace above the venue's published ceiling"
        );
        // The counter still overrules the seed on the first real delta — latest-wins, unchanged.
        p.observe(10);
        p.observe(13);
        assert_eq!(p.per_request_weight(), 3.0);
    }

    /// Rule 3 — the budget is the HOST's identity. A spot record must never seed a perp pager, or
    /// the weight-2 estimate would pace the weight-5 endpoint at 2.5x its target.
    #[test]
    fn a_seed_from_a_different_budget_is_refused_whole() {
        let mut perp = Pacer::discovered(fapi_budget(), 0.4); // 2400/min
        let spot_record = PaceSample {
            request_ms: 120,
            per_request_weight: 2.0,
            budget_per_min: Some(6000),
            samples: 40,
        };
        assert!(!perp.seed(&spot_record), "another host's record must not apply");
        assert_eq!(perp.per_request_weight(), SEED_WEIGHT, "and must leave NOTHING behind");
        assert_close(perp.next_delay().as_secs_f64(), 0.3125, 1e-6);
        assert_eq!(perp.eta(10), None);

        // `None` is a distinct budget, in both directions: a fallback-mode record cannot seed a
        // discovered pacer, and a discovered record cannot seed a fallback one.
        assert!(!perp.seed(&PaceSample { budget_per_min: None, ..fapi_sample(280, 5.0) }));
        let mut fixed = Pacer::fallback(Duration::from_millis(150));
        assert!(!fixed.seed(&fapi_sample(280, 5.0)));
        assert!(
            fixed.seed(&PaceSample { budget_per_min: None, ..fapi_sample(280, 5.0) }),
            "a fallback record matches a fallback pacer"
        );
        // ...and even then a fixed delay is a fixed delay — the seed cannot speed it up.
        assert_eq!(fixed.next_delay(), Duration::from_millis(150));
    }

    /// Rule 2 plus the sanitation: a seed never lands over live evidence, and never lands at all if
    /// it is the kind of number that would hang or hammer the pager.
    #[test]
    fn a_seed_is_refused_over_live_evidence_and_over_nonsense() {
        // Over live evidence — a weight observation counts, not only a timed request.
        let mut observed = Pacer::discovered(fapi_budget(), 0.4);
        observed.observe(100);
        assert!(!observed.seed(&fapi_sample(280, 5.0)), "a counter reading is evidence");
        let mut timed = Pacer::discovered(fapi_budget(), 0.4);
        timed.observe_request(None, Duration::from_millis(400));
        assert!(!timed.seed(&fapi_sample(280, 5.0)), "a timed request is evidence");
        assert_close(estimate_secs(&timed), 0.400, 1e-6);

        // Nonsense: every one of these leaves the pacer exactly as it was built.
        for bad in [
            PaceSample { samples: 0, ..fapi_sample(280, 5.0) },
            PaceSample { request_ms: 0, ..fapi_sample(280, 5.0) },
            fapi_sample(280, 0.0),
            fapi_sample(280, -1.0),
            fapi_sample(280, f64::NAN),
            fapi_sample(280, f64::INFINITY),
            fapi_sample((MAX_DELAY_SECS as u64) * 1_000, 5.0), // a request longer than the ceiling
            fapi_sample(u64::MAX, 5.0),
            PaceSample::default(),
        ] {
            let mut p = Pacer::discovered(fapi_budget(), 0.4);
            assert!(!p.seed(&bad), "{bad:?} must be refused");
            assert_eq!(p.per_request_weight(), SEED_WEIGHT);
            assert_eq!(p.eta(10), None, "{bad:?} must leave no prior behind");
            let d = p.next_delay();
            assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO, "{bad:?} -> {d:?}");
        }
    }

    /// The report side: `None` until something is TIMED, so a run that measured nothing can never
    /// overwrite a stored record — and once timed it carries the budget it was measured against.
    #[test]
    fn measured_reports_only_what_was_actually_timed() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert_eq!(p.measured(), None, "nothing timed ⇒ nothing to persist");
        p.observe(100);
        p.observe(105);
        assert_eq!(p.measured(), None, "a WEIGHT observation is not a duration observation");

        p.observe_request(Some(110), Duration::from_millis(280));
        let m = p.measured().expect("a timed request is reportable");
        assert_eq!(m.request_ms, 280);
        assert_eq!(m.per_request_weight, 5.0);
        assert_eq!(
            m.budget_per_min,
            Some(2400),
            "the budget it was measured against travels with it"
        );
        assert_eq!(m.samples, 1);
        assert!(m.is_usable());

        // Samples accumulate with the EWMA, and the rounding is to whole milliseconds.
        for _ in 0..3 {
            p.observe_request(None, Duration::from_millis(280));
        }
        assert_eq!(p.measured().unwrap().samples, 4);
        assert_eq!(p.measured().unwrap().request_ms, 280);

        // A SEEDED pacer that then measures nothing reports nothing — the prior is not evidence,
        // so a no-op run cannot re-stamp last week's numbers as fresh.
        let mut seeded = Pacer::discovered(fapi_budget(), 0.4);
        assert!(seeded.seed(&fapi_sample(280, 5.0)));
        assert_eq!(seeded.measured(), None, "a prior is not a measurement");

        // Fallback mode still reports (the weight is real, inferred from the header deltas) — with
        // no budget, which is what stops it seeding a discovered run later.
        let mut fixed = Pacer::fallback(Duration::from_millis(150));
        fixed.observe_request(Some(10), Duration::from_millis(90));
        fixed.observe_request(Some(12), Duration::from_millis(90));
        let f = fixed.measured().expect("a fallback pager still times its requests");
        assert_eq!(f.budget_per_min, None);
        assert_eq!(f.per_request_weight, 2.0);
        assert_eq!(f.samples, 2);
    }

    /// The ADDITIVE contract, stated as one test: with no seed, a pacer is bit-for-bit the pacer it
    /// was before any of this existed — same delays, same cooldowns, same ETAs, across the whole
    /// observation sequence the measured tests use.
    #[test]
    fn an_unseeded_pacer_is_unchanged_in_every_observable_way() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert_eq!(p.eta(1_000), None);
        assert_close(p.next_delay().as_secs_f64(), 60.0 * SEED_WEIGHT / 960.0, 1e-9);
        p.observe(100);
        p.observe(105);
        assert_close(p.next_delay().as_secs_f64(), 0.3125, 1e-6);
        assert!(!p.should_cool_down());
        p.observe_request(Some(110), Duration::from_millis(280));
        assert_close(p.next_delay().as_secs_f64(), 0.0325, 1e-6);
        assert_close(p.eta(1).unwrap().as_secs_f64(), 0.3125, 1e-6);
        // The target gap is the WHOLE spacing the sleep-only pacer already produced — `next_delay`
        // plus the round trip it now subtracts. Asserted HERE, before the spike below, because it
        // is a function of the inferred per-request weight and the spike deliberately destroys it.
        assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);

        p.observe(2_000);
        assert!(p.should_cool_down());
        // The new accessors are pure reads — none of them steers anything.
        assert_eq!(p.budget_per_minute(), Some(2400));
        assert!(p.measured().is_some());
        assert_eq!(Pacer::fallback(Duration::from_millis(150)).budget_per_minute(), None);
        // ...and the spike's own consequence, which is NOT a regression: a 1_890-weight delta means
        // the budget affords one request every ~118s, so the gap saturates at the MAX_DELAY_SECS
        // ceiling the module doc describes as the stall. A concurrent pager spacing on this figure
        // therefore stalls exactly as the sequential one does — it cannot outrun a blown budget.
        assert_close(p.target_gap().as_secs_f64(), MAX_DELAY_SECS, 1e-9);
        // One lane until something is measured: the ratio has no numerator yet.
        assert_eq!(Pacer::discovered(fapi_budget(), 0.4).suggested_lanes(8), 1);
    }

    // ----- the derived lane count ---------------------------------------------------------------

    /// `target_gap` is the WHOLE spacing; `next_delay` is that minus the measured round trip. The
    /// distinction is the entire reason a concurrent pager may not pace on `next_delay`: with
    /// requests overlapping, subtracting a round trip that no longer serialises would spend the
    /// difference a second time.
    #[test]
    fn target_gap_is_next_delay_plus_the_measured_request() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(100);
        p.observe(105); // weight 5 ⇒ 60 * 5 / 960 = 312.5ms
        assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
        assert_eq!(p.target_gap(), p.next_delay(), "nothing measured ⇒ nothing subtracted");

        p.observe_request(None, Duration::from_millis(280));
        assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
        assert_close(p.next_delay().as_secs_f64(), 0.0325, 1e-6);
        // The identity that makes the two interchangeable for the caller that knows which it wants.
        assert_close(
            p.next_delay().as_secs_f64() + estimate_secs(&p),
            p.target_gap().as_secs_f64(),
            1e-6,
        );
    }

    /// The MEASURED shapes, which land on opposite sides of the line — and that opposition is the
    /// finding, not the arithmetic: binance PERP is already at its target sequentially, binance SPOT
    /// is at ~18 % of its target and no sleep tuning can fix it (the sleep is floored at 1ms).
    #[test]
    fn lanes_are_derived_from_the_measured_round_trip_not_a_constant() {
        let rtt = Duration::from_millis(280);

        // fapi: 2400/min at 40%, weight-5 pages ⇒ 312.5ms gap > 280ms rtt ⇒ ONE lane.
        let mut perp = Pacer::discovered(fapi_budget(), 0.4);
        perp.observe_request(Some(100), rtt);
        perp.observe_request(Some(105), rtt);
        assert_eq!(perp.suggested_lanes(8), 1, "the budget is the binding constraint on fapi");

        // spot: 6000/min at 40%, weight-2 pages ⇒ 50ms gap ⇒ ceil(280/50) = 6 lanes.
        let mut spot = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
        spot.observe_request(Some(100), rtt);
        spot.observe_request(Some(102), rtt);
        assert_close(spot.target_gap().as_secs_f64(), 0.050, 1e-6);
        assert_eq!(spot.suggested_lanes(8), 6);
        // ...and the CAP is a thread bound, not a rate bound: capping it does not change the gap.
        assert_eq!(spot.suggested_lanes(4), 4);
        assert_eq!(spot.suggested_lanes(1), 1);
        assert_eq!(spot.suggested_lanes(0), 1, "a zero cap is one lane, never zero");
        assert_close(spot.target_gap().as_secs_f64(), 0.050, 1e-6);
    }

    /// `observe_absolute` records the counter and infers NOTHING from it — the concurrent reading
    /// that `observe` would have mis-read as one request's cost.
    ///
    /// FAILS if the concurrent path is wired to `observe`: the 105 -> 500 step would be read as a
    /// 395-weight request and widen the target gap from 312 ms to ~24 s, which is the systematic
    /// lane-count inflation `observe_absolute`'s doc describes (and, past a larger delta, the
    /// `MAX_DELAY_SECS` stall the byte-identity test pins).
    #[test]
    fn observe_absolute_records_the_counter_and_infers_nothing() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(100);
        p.observe(105); // the SEQUENTIAL measurement: exactly one request outstanding ⇒ weight 5
        let sequential_gap = p.target_gap();
        assert_close(sequential_gap.as_secs_f64(), 0.3125, 1e-6);

        p.observe_absolute(500);
        assert_eq!(p.per_request_weight(), 5.0, "a concurrent reading is not a per-request cost");
        assert_eq!(p.target_gap(), sequential_gap, "...so it must not re-pace anything");
        assert!(!p.should_cool_down(), "500 is under the 960 window share");

        // The ABSOLUTE guard is exactly what the freeze keeps — it is the one that stops a 429.
        p.observe_absolute(1_000);
        assert!(p.should_cool_down(), "the counter crossing the share still fires, unattributed");
        assert_eq!(p.per_request_weight(), 5.0, "and STILL nothing is inferred");
        assert_eq!(p.target_gap(), sequential_gap);

        // The persisted floor is untouched too, so a concurrent run cannot write a polluted weight
        // into the pace book for the next run to open on.
        p.observe_request(None, Duration::from_millis(280));
        assert_eq!(p.measured().unwrap().per_request_weight, 5.0);
    }

    /// A window ROLL under `observe_absolute` re-baselines like everywhere else — and, because
    /// nothing is inferred, it cannot produce the wrapped subtraction the module doc calls the trap.
    #[test]
    fn observe_absolute_survives_a_counter_reset() {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        p.observe(100);
        p.observe(105);
        p.observe_absolute(2_000);
        assert!(p.should_cool_down());
        p.observe_absolute(3); // the interval rolled
        assert!(!p.should_cool_down(), "a fresh window has spent almost nothing");
        assert_eq!(p.per_request_weight(), 5.0);
        assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
        // ...and a later `observe` measures from the NEW baseline, not across the whole gap.
        p.observe(8);
        assert_eq!(p.per_request_weight(), 5.0, "8 - 3 = 5, not 8 - 105");
    }

    /// The three refusals. Each is a real failure mode, not defensive decoration — and the `Fixed`
    /// one is the whole reason bybit/okx/deribit stay sequential: their `page_delay` is a hand-chosen
    /// constant, not a fraction of a published ceiling, so N of them in parallel is an unmeasured
    /// rate increase against a venue that publishes no budget at all.
    #[test]
    fn lanes_refuse_without_a_measurement_a_budget_or_a_sane_ratio() {
        // (1) Nothing measured — a lane count needs a numerator.
        let mut p = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
        assert_eq!(p.suggested_lanes(8), 1);
        p.observe(100);
        p.observe(102);
        assert_eq!(p.suggested_lanes(8), 1, "a WEIGHT observation is not a duration observation");

        // (2) No discovered budget — however slow the venue is measured to be.
        let mut fixed = Pacer::fallback(Duration::from_millis(200));
        fixed.observe_request(None, Duration::from_millis(480)); // the measured okx page
        assert_eq!(fixed.target_gap(), Duration::from_millis(200), "the accessor answers honestly");
        assert_eq!(
            fixed.suggested_lanes(8),
            1,
            "...and the lane count still refuses: no ceiling ⇒ no aggregate to be a fraction of"
        );

        // (3) Absurd ratios saturate at the cap rather than wrapping through `as usize`.
        let mut huge = Pacer::discovered(WeightBudget { limit: u64::MAX, interval_secs: 1 }, 0.9);
        huge.observe_request(None, Duration::from_secs(30));
        assert_eq!(huge.suggested_lanes(8), 8);
        let mut absurd = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
        absurd.observe_request(None, Duration::MAX);
        assert_eq!(absurd.suggested_lanes(8), 8);

        // A SEEDED pacer answers from page zero — the prior IS last run's measurement.
        let mut seeded = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
        assert!(seeded.seed(&PaceSample {
            request_ms: 280,
            per_request_weight: 2.0,
            budget_per_min: Some(6000),
            samples: 40,
        }));
        assert_eq!(seeded.suggested_lanes(8), 6);
    }
}
