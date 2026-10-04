//! The observed MAKER-vs-REFERENCE basis — an event-time EWMA of `mid_A / mid_B − 1`.
//!
//! # ⚠ DIAGNOSTIC AND HALT BAND ONLY — this value is NEVER a term in a quote price
//!
//! The obvious use for a basis estimate is to rebase the reference touch before pricing. That is a
//! trap, and [`super::pricing::passive_clamp`]'s doc carries the full argument: rebasing destroys
//! the hedge identity (a fill that "looks" hedged is a guaranteed loss net of fees), and flooring
//! the rebase back to restore the identity produces a one-sided accumulator quoting off an estimate
//! whose error between the fill and the hedge is unbounded. The passive clamp needs no estimate at
//! all — venue A's own book is the ground truth about where A trades.
//!
//! So this estimator has exactly two jobs, both of which a POINT estimate is honest enough for:
//!
//! 1. **publish**, so an operator can size `min_profitability` against basis VOLATILITY rather than
//!    a point level — that volatility, not the level, is the real risk between the A-fill and the
//!    B-hedge;
//! 2. **[`BasisEwma::out_of_band`]**, which catches a decoupled pair, a wrong-instrument mount and
//!    a lying feed with ONE number, turning "this pair has no two-sided edge" into an
//!    operator-visible halt instead of a silent one-sided accumulation.
//!
//! # Three deliberate disciplines
//!
//! - **PAIRED observations only.** A fold needs BOTH venues' mids, both sane. Folding one venue's
//!   move against the other's stale last price would measure feed latency, not basis.
//! - **EVENT-TIME half-life**, never wall clock — so the estimate is deterministic under test and
//!   identical under replay, the same rule the rest of this crate's windows follow.
//! - **NEVER fed by own fills.** Own fills are the ADVERSELY-SELECTED sample: the maker is filled
//!   precisely when the basis moved against it, so folding them biases the estimate toward the side
//!   already being picked off. The estimator sees market touches only.
//!
//! House style: naïve f64 folds (no `mul_add`), pure, in-file `#[cfg(test)]`.

/// Event-time EWMA of the multiplicative basis `mid_A / mid_B − 1`.
///
/// Cold (`value == None`) until the first paired observation. Inert — and permanently cold — when
/// `halflife_ms <= 0`, which is what makes a maker that never configured a band cost nothing.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BasisEwma {
    /// The current estimate, or `None` while cold.
    value: Option<f64>,
    /// EVENT ts of the last folded observation.
    last_ts: i64,
}

impl BasisEwma {
    /// Fold ONE paired observation. `mid_a` is the MAKER venue's mid, `mid_b` the REFERENCE
    /// venue's, `ts` the event time of the later of the two touches.
    ///
    /// The raw observation is `mid_a / mid_b − 1`, clamped to `±clamp` (when `clamp > 0`) so ONE
    /// garbage touch cannot move the estimate arbitrarily far — the estimator's whole job is to
    /// detect a persistent regime, and a single outlier is the opposite of that.
    ///
    /// The decay weight is `0.5^(dt / halflife)` in EVENT milliseconds, so one half-life exactly
    /// halves the weight of everything before it. A non-positive `dt` (a same-ts or out-of-order
    /// tick) folds at full weight rather than being dropped — an out-of-order tick is still a real
    /// observation, and dropping it would make the estimate depend on arrival order.
    ///
    /// A no-op when `halflife_ms <= 0` (the estimator is disabled), when either mid is not finite
    /// and strictly positive, or when the ratio comes out non-finite.
    pub(crate) fn observe(
        &mut self,
        mid_a: f64,
        mid_b: f64,
        ts: i64,
        halflife_ms: i64,
        clamp: f64,
    ) {
        if halflife_ms <= 0 {
            return;
        }
        if !(mid_a.is_finite() && mid_a > 0.0 && mid_b.is_finite() && mid_b > 0.0) {
            return;
        }
        let mut obs = mid_a / mid_b - 1.0;
        if !obs.is_finite() {
            return;
        }
        if clamp > 0.0 {
            obs = obs.clamp(-clamp, clamp);
        }
        self.value = Some(match self.value {
            None => obs,
            Some(prev) => {
                let dt = (ts - self.last_ts) as f64;
                if dt <= 0.0 {
                    obs
                } else {
                    // 0.5^(dt/halflife): the weight the PREVIOUS estimate keeps.
                    let w = libm::pow(0.5, dt / halflife_ms as f64);
                    w * prev + (1.0 - w) * obs
                }
            }
        });
        self.last_ts = ts;
    }

    /// The current estimate as a FRACTION (e.g. `-0.001` = venue A trades 10 bp below venue B), or
    /// `None` while cold. Published for the operator; never consumed by a price.
    pub(crate) fn value(&self) -> Option<f64> {
        self.value
    }

    /// `true` when the estimate has warmed AND its magnitude exceeds `max_bps` basis points — the
    /// halt band. `max_bps <= 0` disables it, and a COLD estimator never trips (an absent estimate
    /// is not evidence of a wide basis; the freshness bounds cover "I can't see anything").
    pub(crate) fn out_of_band(&self, max_bps: f64) -> bool {
        if max_bps <= 0.0 {
            return false;
        }
        self.value.is_some_and(|v| v.abs() * 10_000.0 > max_bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One half-life exactly halves the previous estimate's weight — pinned bit-for-bit, because
    /// the band's calibration depends on it.
    #[test]
    fn one_halflife_halves_the_previous_weight() {
        let mut b = BasisEwma::default();
        // seed at +1% (100.0 vs 99.0... use exact ratios so the arithmetic is checkable)
        b.observe(101.0, 100.0, 0, 1_000, 0.0);
        assert_eq!(b.value().unwrap().to_bits(), (101.0_f64 / 100.0 - 1.0).to_bits());
        // exactly one half-life later, a 0% observation ⇒ half the seed remains
        b.observe(100.0, 100.0, 1_000, 1_000, 0.0);
        let seed = 101.0_f64 / 100.0 - 1.0;
        assert_eq!(
            b.value().unwrap().to_bits(),
            (0.5 * seed + 0.5 * 0.0).to_bits(),
            "one half-life leaves exactly half the seed"
        );
    }

    /// DISABLED means COLD, not merely quiet: with `halflife_ms <= 0` the estimator never warms, so
    /// nothing downstream can read a stale or fabricated value out of it.
    #[test]
    fn a_disabled_estimator_never_warms() {
        let mut b = BasisEwma::default();
        for ts in [0, 1_000, 2_000] {
            b.observe(101.0, 100.0, ts, 0, 0.05);
        }
        assert_eq!(b.value(), None, "halflife <= 0 ⇒ permanently cold");
        assert!(!b.out_of_band(1.0), "and a cold estimator never trips the band");
    }

    /// A single garbage touch cannot move the estimate arbitrarily far — the clamp bounds ONE
    /// observation, which is what keeps the band a regime detector rather than an outlier alarm.
    #[test]
    fn a_runaway_observation_is_clamped() {
        let mut b = BasisEwma::default();
        b.observe(1_000_000.0, 100.0, 0, 1_000, 0.05); // a 999_900% "basis"
        assert_eq!(b.value().unwrap().to_bits(), 0.05_f64.to_bits(), "clamped to ±5%");
        let mut unclamped = BasisEwma::default();
        unclamped.observe(1_000_000.0, 100.0, 0, 1_000, 0.0);
        assert!(unclamped.value().unwrap() > 1.0, "clamp <= 0 genuinely disables the clamp");
    }

    /// The band trips on magnitude in BOTH directions, and never while cold or disabled.
    #[test]
    fn the_band_trips_on_magnitude_in_both_directions() {
        let mut below = BasisEwma::default();
        below.observe(99.0, 100.0, 0, 1_000, 0.0); // −100 bp
        assert!(below.out_of_band(50.0), "−100 bp is outside a 50 bp band");
        assert!(!below.out_of_band(150.0), "and inside a 150 bp one");
        assert!(!below.out_of_band(0.0), "max_bps <= 0 disables the band");
        let mut above = BasisEwma::default();
        above.observe(101.0, 100.0, 0, 1_000, 0.0); // +100 bp
        assert!(above.out_of_band(50.0), "the band is on |basis|, not a signed threshold");
        assert!(!BasisEwma::default().out_of_band(1.0), "a cold estimator never trips");
    }

    /// Insane inputs fold NOTHING rather than poisoning the estimate — the same
    /// unknowable-is-never-free discipline `passive_clamp` follows.
    #[test]
    fn insane_mids_fold_nothing() {
        let mut b = BasisEwma::default();
        for (a, m) in [(0.0, 100.0), (100.0, 0.0), (f64::NAN, 100.0), (100.0, f64::INFINITY)] {
            b.observe(a, m, 0, 1_000, 0.05);
            assert_eq!(b.value(), None, "({a}, {m}) must not warm the estimator");
        }
    }
}
