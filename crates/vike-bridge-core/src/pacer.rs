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
//! does no I/O and holds no clock, so a caller in a BINARY-reachable layer owns any file. (The one
//! that did — `vike-backfill`'s pace file — was deleted with the one-shot kline programs by
//! docs/decisions/0094.)
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
//! shape. The measurement is now taken and reported (and was persisted too, until
//! docs/decisions/0094 deleted `vike-backfill`'s pace file) — and the sleep is STILL exactly
//! `page_delay`. Acting on the number is a separate change with its own
//! measurement, not a side effect of being able to see it.

use std::time::Duration;

use crate::rate_discovery::WeightBudget;

// Utilization bounds are NOT redefined here — `vike_model::rate_limits` owns them, validates
// operator input against them, and is what a GUI slider reads. A second copy in this crate meant
// vike-model accepted `1.0` as legal while the pacer silently clamped to `0.95`: an operator
// setting 100% got 95% with nothing telling them so. One definition, or the two drift.
use vike_model::rate_limits::{MAX_UTILIZATION, MIN_UTILIZATION, PaceSample};

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
        if let Some(last) = self.last_used
            && used_weight > last
        {
            let delta = (used_weight - last) as f64;
            self.per_request = delta;
            // The floor is for PERSISTENCE only — never for the live delay. See above.
            self.min_delta = Some(self.min_delta.map_or(delta, |m: f64| m.min(delta)));
        }
        // `used_weight <= last` is either "no movement" or a window reset — both keep the
        // estimate, and neither subtracts.
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

#[path = "pacer_tests.rs"]
#[cfg(test)]
mod pacer_tests;
