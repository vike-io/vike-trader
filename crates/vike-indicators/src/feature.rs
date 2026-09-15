//! The STUDY↔STRATEGY seam: one feature definition, evaluated both ways over one numeric column.
//!
//! # The failure this exists to make unrepresentable
//!
//! A study trains on a MATRIX — a column per feature, a row per bar, computed in one batch pass.
//! A live strategy computing that same feature per bar has to arrive at the same number for the
//! same timestamp, or the model receives inputs it never saw. Nothing errors; the edge simply
//! degrades. It is the same hazard `crates/vike-analytics/src/signal.rs` states for the OTHER half
//! of the handoff — "the offline research converts a concatenated out-of-sample probability series,
//! while a live strategy converts one probability per bar. If those two ever disagree, a forward
//! test silently stops testing what the research measured" — and `hysteresis_step`/`hysteresis`
//! answers it the only way that cannot rot: the batch form is a FOLD of the streaming one, so there
//! is a single arithmetic and disagreement is not expressible.
//!
//! This module is that answer for the FEATURE side, taken in the other direction. Here the batch
//! form is the reference (it is what the study trains on, and it is what `crate::window` already
//! ships), so [`FeatureStream`] is defined as the batch kernel re-run over a bounded retained tail.
//! There is still exactly ONE arithmetic — a [`ColumnFeature`] implementor writes `vectorize` and
//! gets the streaming form for free, and cannot write a second kernel because there is nowhere to
//! put one.
//!
//! # What is left to get wrong, and where it is gated
//!
//! Exactly one number: [`ColumnFeature::reach`], how many trailing values the streaming form must
//! retain. Declare it too small and the streamed value is computed from a truncated history —
//! silently, past a threshold no short test reaches. That is not a hypothetical shape in this
//! crate: `crates/vike-indicators/tests/parity.rs`'s
//! `every_trimmed_indicator_is_truncation_invariant` exists because six indicators shipped with
//! exactly that defect, and its doc records that they were green because the gate ran 400 bars
//! while the trim first fired at 512.
//!
//! So the reach is a DECLARATION with a gate on it, the same shape as [`crate::WindowReach`] and
//! `keep_for`. The gate is `crate::test_support` (the `test-support` feature), and it is a LIBRARY
//! function rather than a test file on purpose: the study whose features these are does not live in
//! this repository, so a `tests/*.rs` here could never run on the definition that matters.
//!
//! # ⚠ This module holds no feature DEFINITION belonging to any study
//!
//! [`WindowFeature`] wraps the six kernels `crate::window` already owns, and those are general by
//! construction — that module's own doc says the registered `zscore`/`var` indicators and the
//! research features "are presets of the same code". A study's own features — which columns, which
//! windows, which lags, what they are called — are the author's, live in their own tree, and reach
//! this seam by implementing [`ColumnFeature`].

use std::collections::VecDeque;

use crate::window::{self, WindowSpec};

/// A feature definition over ONE numeric column, evaluable in batch and — through
/// [`FeatureStream`] — one value at a time.
///
/// # Contract
///
/// * `vectorize` returns exactly one value per input row, and the value at row `i` reads only rows
///   `i + 1 - reach() ..= i`. Warm-up rows are `f64::NAN`; nothing looks forward.
/// * `reach` is that bound, in rows, INCLUDING the current row. Over-declaring is safe and merely
///   wasteful; under-declaring returns a different number from the streaming path and is the whole
///   defect this seam exists to prevent.
///
/// There is deliberately no `on_value` method to override. The streaming form is PROVIDED, so an
/// implementor has no second kernel to write and no second kernel to get wrong — which is the
/// property [`crate::Indicator`] buys by convention (`on_bar` must match `vectorize`) and this
/// trait buys by construction.
///
/// # Why this is not [`crate::Indicator`]
///
/// `Indicator` streams a `vike_model::Bar` — OHLCV plus a timestamp. A model feature is a rolling
/// statistic over an ARBITRARY numeric column: a funding rate, an open-interest series, a venue
/// spread, a per-cohort aggregate. Pushing one of those through a `Bar` means inventing five fields
/// so that one can be read. `Indicator` also has no `lag`/`ddof`/`clip`, which
/// [`WindowSpec::point_in_time`] exists precisely to carry, and its registry is a fixed roster of
/// named indicators that a user's own feature is not in.
pub trait ColumnFeature {
    /// Batch: the whole column at once, one value per row. This is the REFERENCE — it is the
    /// number a study trains on.
    fn vectorize(&self, x: &[f64]) -> Vec<f64>;

    /// How many trailing rows, INCLUDING the current one, the value at a row can read.
    ///
    /// A [`FeatureStream`] retains exactly this many values, so this number is the difference
    /// between a streamed feature that equals the trained one and a streamed feature that does not.
    fn reach(&self) -> usize;
}

impl<F: ColumnFeature + ?Sized> ColumnFeature for &F {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        (**self).vectorize(x)
    }
    fn reach(&self) -> usize {
        (**self).reach()
    }
}

impl<F: ColumnFeature + ?Sized> ColumnFeature for Box<F> {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        (**self).vectorize(x)
    }
    fn reach(&self) -> usize {
        (**self).reach()
    }
}

/// The streaming form of any [`ColumnFeature`]: push one value, get that row's feature.
///
/// # How it cannot disagree with the batch form
///
/// It re-runs the feature's OWN `vectorize` over the last [`ColumnFeature::reach`] values and takes
/// the final element. This is `hist_indicator!`'s `stream_tail` idiom, which this crate already
/// uses for the majority of its registered indicators, applied to the column seam — and it is what
/// makes bit-identity a property of the construction rather than of a test that has to keep
/// noticing. The only claim under test is that `reach` is truthful.
///
/// Before the buffer is full it holds the entire prefix `x[0 ..= i]`, which is exactly what the
/// batch form can read at row `i` when `i < reach - 1`. So the warm-up rows agree for the same
/// reason the steady state does.
///
/// # One instrument per stream
///
/// The buffer is a rolling window over whatever it is fed, with no notion of an instrument. A
/// caller with several instruments needs one stream each, or [`FeatureStream::reset`] at the
/// boundary — the identical discipline `vike_analytics::signal::hysteresis` states for its own
/// carried state, and the one `crate::window::per_group` enforces on the batch side. The
/// `assert_stream_matches_batch_per_group` half of the harness gates it.
#[derive(Clone, Debug)]
pub struct FeatureStream<F> {
    feature: F,
    reach: usize,
    buf: VecDeque<f64>,
}

impl<F: ColumnFeature> FeatureStream<F> {
    /// A stream over `feature`, retaining [`ColumnFeature::reach`] values.
    ///
    /// ⚠ A declared reach of `0` is raised to `1`. A feature still emits one value per row even if
    /// it reads none of them, and `vectorize(&[])` has no last element to return; a zero-capacity
    /// buffer would therefore be a panic rather than a diagnosis. It is not silently forgiven
    /// either — `crate::test_support`'s harness refuses a reach of `0` by name.
    pub fn new(feature: F) -> Self {
        let reach = feature.reach().max(1);
        Self { feature, reach, buf: VecDeque::with_capacity(reach) }
    }

    /// Feed the next row's raw value; get that row's feature value.
    pub fn push(&mut self, v: f64) -> f64 {
        if self.buf.len() == self.reach {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
        let tail = self.buf.make_contiguous();
        let out = self.feature.vectorize(tail);
        // A short or long answer is a broken CONTRACT, not awkward data — the distinction
        // `crate::window::WindowError` draws. Taking `out.last()` regardless would hand this bar an
        // OLDER row's feature value, which is precisely the silent-wrong-number failure this seam
        // exists to remove, so it is refused loudly instead. vike-indicators is not the vike-core
        // hot path (see the crate doc), so an assertion per row is affordable here.
        assert_eq!(
            out.len(),
            tail.len(),
            "ColumnFeature::vectorize returned {} values for {} rows — the contract is one value \
             per row. Answering the last element of a short vector would return an EARLIER row's \
             feature as this row's, with no error anywhere.",
            out.len(),
            tail.len()
        );
        out[out.len() - 1]
    }

    /// Drop all retained history — use at an instrument boundary, never mid-series.
    pub fn reset(&mut self) {
        self.buf.clear();
    }

    /// The retained-history bound this stream was built with (`reach().max(1)`).
    pub fn reach(&self) -> usize {
        self.reach
    }

    /// How many values are currently retained. Below [`FeatureStream::reach`] the feature is still
    /// warming up and its value is whatever the batch form answers for a short prefix.
    pub fn retained(&self) -> usize {
        self.buf.len()
    }

    /// The feature being streamed.
    pub fn feature(&self) -> &F {
        &self.feature
    }
}

/// Which statistic [`WindowFeature`] takes over its window — one variant per `crate::window`
/// kernel, and no arithmetic of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowStat {
    /// [`window::rolling_mean`].
    Mean,
    /// [`window::rolling_var`].
    Var,
    /// [`window::rolling_std`].
    Std,
    /// [`window::rolling_rank_pct`].
    RankPct,
    /// [`window::rolling_median`].
    Median,
    /// [`window::zscore`], optionally clipped to `±clip`.
    ZScore {
        /// Passed straight through; see [`window::zscore`] for why a bad value answers rather than
        /// panics.
        clip: Option<f64>,
    },
}

impl WindowStat {
    /// A stable short name, for a harness label or a column name.
    pub fn name(&self) -> &'static str {
        match self {
            WindowStat::Mean => "mean",
            WindowStat::Var => "var",
            WindowStat::Std => "std",
            WindowStat::RankPct => "rank_pct",
            WindowStat::Median => "median",
            WindowStat::ZScore { .. } => "zscore",
        }
    }
}

/// The general in-tree [`ColumnFeature`]: one `crate::window` kernel under one [`WindowSpec`].
///
/// This is a THIN pairing and deliberately so — it adds a reach declaration to kernels that already
/// exist, and no arithmetic. Its value is that `crate::window`'s six kernels shipped in BATCH form
/// only, so a live strategy wanting the same number per bar had nothing to call and could only
/// re-derive it, which is the divergence this module exists to remove.
///
/// Both of `WindowSpec`'s shipped presets are covered by construction: `WindowSpec::indicator`
/// (window ends at the current row, population variance) and `WindowSpec::point_in_time` (window
/// EXCLUDES the current row, sample variance) — the second being the preset that module names as
/// the one "the research features use".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowFeature {
    /// Which kernel.
    pub stat: WindowStat,
    /// Over which window.
    pub spec: WindowSpec,
}

impl WindowFeature {
    /// Pair a kernel with a window.
    pub fn new(stat: WindowStat, spec: WindowSpec) -> Self {
        Self { stat, spec }
    }

    /// `stat` over [`WindowSpec::indicator`]`(period)` — the chart/indicator preset.
    pub fn indicator(stat: WindowStat, period: usize) -> Self {
        Self::new(stat, WindowSpec::indicator(period))
    }

    /// `stat` over [`WindowSpec::point_in_time`]`(period)` — the model-feature preset, whose window
    /// excludes the current row.
    pub fn point_in_time(stat: WindowStat, period: usize) -> Self {
        Self::new(stat, WindowSpec::point_in_time(period))
    }
}

impl ColumnFeature for WindowFeature {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        match self.stat {
            WindowStat::Mean => window::rolling_mean(x, self.spec),
            WindowStat::Var => window::rolling_var(x, self.spec),
            WindowStat::Std => window::rolling_std(x, self.spec),
            WindowStat::RankPct => window::rolling_rank_pct(x, self.spec),
            WindowStat::Median => window::rolling_median(x, self.spec),
            WindowStat::ZScore { clip } => window::zscore(x, self.spec, clip),
        }
    }

    /// `period + lag`, and it is EXACT rather than padded.
    ///
    /// Every kernel in `crate::window` reads `window_at(x, i, s)` — the `period` values ending
    /// `lag` rows back — plus, for [`WindowStat::ZScore`] alone, `x[i]` itself. The deepest row
    /// that reaches is `i - lag + 1 - period`, which is `period + lag - 1` rows back, so
    /// `period + lag` values including the current one is exactly sufficient and one fewer is not.
    /// `crate::test_support`'s tightness assertion is what keeps that "and one fewer is not" a
    /// measured fact rather than a claim in a comment.
    fn reach(&self) -> usize {
        self.spec.period.saturating_add(self.spec.lag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam's smallest possible statement: a stream over a hand-checked series.
    #[test]
    fn a_streamed_mean_is_the_batch_mean_row_by_row() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let f = WindowFeature::indicator(WindowStat::Mean, 3);
        let batch = f.vectorize(&x);
        let mut s = FeatureStream::new(f);
        let streamed: Vec<f64> = x.iter().map(|&v| s.push(v)).collect();
        assert!(batch[0].is_nan() && batch[1].is_nan(), "a 3-window warms at index 2");
        assert!(streamed[0].is_nan() && streamed[1].is_nan(), "...and so does the stream");
        assert_eq!(streamed[2].to_bits(), batch[2].to_bits());
        assert_eq!(streamed[4].to_bits(), batch[4].to_bits());
        assert_eq!(streamed[4], 4.0);
    }

    /// The buffer is BOUNDED — this is what makes it a stream rather than an accumulating copy of
    /// the whole series, and a `reach()` that grew with the input would pass every parity
    /// assertion while streaming nothing.
    #[test]
    fn the_retained_history_stops_growing_at_the_declared_reach() {
        let f = WindowFeature::point_in_time(WindowStat::Std, 4);
        assert_eq!(f.reach(), 5, "period 4 + lag 1");
        let mut s = FeatureStream::new(f);
        for i in 0..500 {
            s.push(i as f64);
        }
        assert_eq!(s.retained(), 5, "the buffer must not grow with the series");
    }

    /// `reset` returns the stream to construction, which is what an instrument boundary needs.
    #[test]
    fn reset_makes_the_next_row_a_fresh_warm_up() {
        let f = WindowFeature::indicator(WindowStat::Mean, 3);
        let mut s = FeatureStream::new(f);
        for v in [10.0, 20.0, 30.0] {
            s.push(v);
        }
        assert_eq!(s.push(40.0), 30.0);
        s.reset();
        assert_eq!(s.retained(), 0);
        assert!(s.push(1.0).is_nan(), "after a reset the window is warming again");
    }

    /// A reach of `0` cannot be represented as a buffer, so the stream raises it — and the harness
    /// refuses it separately rather than letting the raise pass for a declaration.
    #[test]
    fn a_zero_reach_is_raised_to_one_rather_than_panicking() {
        struct Nothing;
        impl ColumnFeature for Nothing {
            fn vectorize(&self, x: &[f64]) -> Vec<f64> {
                vec![0.0; x.len()]
            }
            fn reach(&self) -> usize {
                0
            }
        }
        let mut s = FeatureStream::new(Nothing);
        assert_eq!(s.reach(), 1);
        assert_eq!(s.push(7.0), 0.0);
    }

    /// The blanket impls exist so a harness can take `&dyn ColumnFeature` and a caller can hold a
    /// boxed one; without them every signature would have to be generic over ownership.
    #[test]
    fn a_boxed_and_a_borrowed_feature_are_both_streamable() {
        let owned: Box<dyn ColumnFeature> = Box::new(WindowFeature::indicator(WindowStat::Mean, 2));
        assert_eq!(owned.reach(), 2);
        let mut boxed = FeatureStream::new(owned);
        boxed.push(1.0);
        assert_eq!(boxed.push(3.0), 2.0);

        let f = WindowFeature::indicator(WindowStat::Mean, 2);
        let dynref: &dyn ColumnFeature = &f;
        let mut borrowed = FeatureStream::new(dynref);
        borrowed.push(1.0);
        assert_eq!(borrowed.push(3.0), 2.0);
    }

    /// A feature that answers the wrong number of rows is refused rather than silently returning
    /// an earlier row's value.
    #[test]
    #[should_panic(expected = "one value per row")]
    fn a_feature_that_answers_the_wrong_row_count_is_refused() {
        struct Short;
        impl ColumnFeature for Short {
            fn vectorize(&self, x: &[f64]) -> Vec<f64> {
                vec![0.0; x.len().saturating_sub(1)]
            }
            fn reach(&self) -> usize {
                4
            }
        }
        FeatureStream::new(Short).push(1.0);
    }
}
