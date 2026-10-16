//! Technical-analysis indicators and candlestick patterns for Rust, with one streaming path and one
//! batch path that give the same numbers.
//!
//! Every indicator implements [`Indicator`]: `on_bar` takes one bar at a time (no look-ahead, for
//! live use) and `vectorize` computes a whole history at once (for charts and research). For every
//! indicator, `on_bar` folded over a series equals `vectorize` on that series **bit for bit**, and
//! tests enforce it, so a backtest and a live run see the same values. Transcendental functions come
//! from the `libm` crate rather than the platform's, so the numbers are also the same on Windows and
//! Linux. The README lists the indicators by category; [`registry::registry`] lists them at run time
//! with their parameters, output lines and render hints.
//!
//! There are three ways in, all backed by the same code:
//!
//! - **Typed**: [`indicators`] names every indicator type (`indicators::Rsi`), `new(..)` takes its
//!   parameters and returns a [`ParamError`] for one outside the registry's range, `next(&bar)`
//!   returns an `f64` (or an array, one entry per output line) and `Default` gives the registry
//!   defaults.
//! - **By name**, when the indicator is chosen at run time: [`make`], [`make_with`] (which clamps
//!   parameters instead of refusing them) and [`compute`] (a whole history, each output line named).
//! - **The registry**: [`registry::registry`] and [`registry::get`] describe every indicator,
//!   [`lookback`] and [`lookback_full`] say how many bars it needs to warm up.
//!
//! # Quick start
//!
//! Typed, one bar at a time (an out-of-range parameter is an error, not a silent clamp):
//!
//! ```
//! use vike_indicators::indicators::Rsi;
//! use vike_marketdata::Bar;
//!
//! # fn main() -> Result<(), vike_indicators::ParamError> {
//! let mut rsi = Rsi::new(14.0)?;
//! let mut last = f64::NAN;
//! for i in 0..30 {
//!     let close = 100.0 + (i as f64).sin() * 3.0;
//!     last = rsi.next(&Bar::ohlcv(i * 60_000, close, close, close, close, 1.0));
//! }
//! assert!(last.is_finite()); // `NaN` until the indicator has warmed up
//! assert!(Rsi::new(0.0).is_err());
//! # Ok(()) }
//! ```
//!
//! By name, over a whole history, with each output line named:
//!
//! ```
//! use vike_marketdata::Bar;
//!
//! let bars: Vec<Bar> = (0..60)
//!     .map(|i| {
//!         let c = 100.0 + (i as f64 * 0.3).sin() * 5.0;
//!         Bar::ohlcv(i * 60_000, c, c + 1.0, c - 1.0, c, 10.0)
//!     })
//!     .collect();
//! let lines = vike_indicators::compute("macd", &[], &bars).expect("a registry name");
//! assert_eq!(lines.iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["macd", "signal", "hist"]);
//! assert_eq!(lines[0].1.len(), bars.len());
//! ```
//!
//! # Conventions
//!
//! - Warm-up positions are `f64::NAN`; [`Indicator::lookback`] reports the first index with a value.
//! - Three indicators read future bars (Ichimoku's chikou line, `zigzag`, `williams_fractal`) and are
//!   flagged [`registry::IndicatorMeta::batch_only`]: `vectorize` is authoritative for them.
//! - Pair (benchmark) indicators live in [`pairs`], realized volatility in [`realized_vol`], and the
//!   validated asset-by-time panel and rolling features for research in [`frame`], [`rolling`],
//!   [`window`] and [`feature`].
//! - This is a `0.x` crate: the API can change between minor versions.
//
// ---- Engineering notes: not rendered on docs.rs. They record how the crate came to be and the
// ---- rules its tests hold; the public contract is the documentation above.
// vike-indicators — the Qt-free indicator framework. One [`Indicator`] trait with
// a streaming `on_bar` (the live/event-engine path — one bar at a time, no
// lookahead) and a batch `vectorize` (the vector-engine/chart path) that are ONE
// source of truth: for every indicator, `on_bar` folded over a series equals
// `vectorize` on that series, bit-for-bit (gated in `tests/parity.rs`). Faithful
// f64 port of the whole vike-trader-app `core/indicators/*.py` catalog — the
// **171** single-series indicators across 8 categories (overlap/momentum/
// volatility/volume/statistics/structure/price/pattern; see [`fn@registry`]) plus
// the **8** 2-series (benchmark) indicators behind the separate [`pairs`] seam
// — plus **6** net-new single-series rows with no Python twin, the `rvol_*`
// realized-volatility family of [`realized_vol`], for **203** single-series and
// 211 in all. Depends on vike-marketdata — the market-data vocabulary, from which this crate
// names `Bar` and nothing else — plus `libm` for the transcendentals; see the crate manifest for
// why a PLATFORM libm must not be used here. ⚠ This line said "Depends on vike-model" until
// 2026-09-19 and had been stale since
// `docs/decisions/0067-the-market-data-vocabulary-leaves-the-model.md` moved `Bar` down.
//
//
// ⚠ **"Faithful port" here means PORTED FROM, not bit-identical to CPython, and the difference is
// worth stating because `fixtures/README.md` carries a rule this crate does not follow.** That
// rule says a ported site writing Python's builtin `sum()` over floats must use
// `vike_model::py_sum`, CPython's Neumaier-compensated fold. This crate uses naive left folds
// throughout — the CONTRACT below says so outright, and it is a choice rather than an oversight:
//
// * `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` retired the oracle. vike
//   IS the reference now, so "does this match CPython" is no longer a requirement anywhere.
// * Nothing compares this crate to Python. `tests/parity.rs` holds `on_bar` against `vectorize` —
//   this crate's own two paths — and there is no oracle fixture for it under `fixtures/`.
// * It could not follow the rule if it wanted to: `py_sum` lives in `vike-model`, and this crate's
//   only dependency is `vike-marketdata`. Reaching it would undo 0067's whole point.
//
// So the naive folds stay, and the word "faithful" below is about PROVENANCE. If a future change
// ever compares these numbers to CPython, that comparison is what reopens the question — not this
// paragraph.
//
// CONTRACT:
// - `vectorize` is the reference kernel (a faithful line-for-line port of the
//   Python function). Warm-up positions emit `f64::NAN`; fold order (naive left
//   folds) is load-bearing. ⚠ A bounded-window kernel folds its window and
//   carries NOTHING across bars — see [`WindowReach`], which is the property
//   `hist_indicator!`'s history drain depends on and which the sliding
//   accumulators this crate used to run (`sum += c[i] - c[i-n]`, `run_sum`/
//   `run_sum2` pairs) silently violated.
// - `on_bar` MUST match `vectorize` bit-for-bit. Simple recurrences use bounded
//   O(1)/O(window) state; the rest stream by history-recompute (`stream_tail`),
//   which is correct-by-construction for causal indicators (vike-indicators is
//   NOT the vike-core hot path).
// - Three indicators read future bars (ichimoku's chikou, zigzag,
//   williams_fractal) and are flagged [`registry::IndicatorMeta::batch_only`]:
//   `vectorize` is authoritative, `on_bar` is a causal best-effort, and the
//   `on_bar == vectorize` gate skips them.
// - Never widen a float tolerance to make a test pass — compare via `f64::to_bits`
//   and treat both-NaN as equal.
// - [`Indicator::lookback`] reports warm-up depth (the first non-NaN `vectorize`
//   index) so a caller can size its seed history by name+params — via
//   [`registry::lookback`] / [`IndicatorMeta::lookback`], and
//   [`pairs::pair_lookback`] for the pair seam. It is a CONSERVATIVE lower bound:
//   134 of the 203 single-series indicators (and the pair indicators that declare one) carry a real
//   param-derived override flagged [`Indicator::lookback_exact`] and gated `==`;
//   the rest (the 63 candlestick patterns, whose signal series is data-dependent
//   and never NaN, plus the batch-only/structure series) keep the `0` default and
//   are gated `>=`. `tests/lookback.rs` is that gate and prints measured values so
//   coverage can be ratcheted.
// - A SECOND seam sits beside the indicator one: [`feature::ColumnFeature`], the same
//   streaming-equals-batch contract over a raw numeric COLUMN rather than over a `Bar`. It exists
//   for the study→strategy handoff — a model feature computed one row at a time by a live
//   strategy must equal the matrix column its model trained on — and it buys the property by
//   CONSTRUCTION rather than by convention: an implementor writes `vectorize` only, and
//   [`feature::FeatureStream`] IS that kernel over a bounded tail, so there is no second
//   arithmetic to drift. The one declaration left to get wrong is
//   [`feature::ColumnFeature::reach`], and `test_support` (the `test-support` feature) is the
//   harness that gates it — for this crate's own features in `tests/feature_parity.rs`, and for a
// - A THIRD seam sits above both: [`frame::Frame`], the validated `(asset, timestamp)` PANEL, and
//   [`rolling`], the blocks that apply a [`feature::ColumnFeature`] to every column of one. The
//   panel is what [`window::per_group`] needs and could not previously be handed: an index sorted
//   by `(asset, ts)` and strictly increasing inside an asset, from which the per-asset row ranges
//   are DERIVED — so a window reaching across an asset boundary is unrepresentable rather than
//   discouraged. Its timestamps are opaque integers and its grid ([`frame::Frame::on_grid`]) is a
//   caller-named cadence in the caller's own unit, so this crate names no time base of its own.
// - [`Indicator::lookback_full`] is the ALL-LINES twin
//   (the number to size seed history with on multi-line indicators), and
//   [`Indicator::warmup_path_dependent`] flags the cumulative/path-dependent series
//   whose truthful `lookback() == 0` does NOT mean "warm".

// Items in the private `indicators` tree are `pub(crate)`: this lint keeps a new `pub` there from
// reading as API (the only route to the concrete types is `registry::make` / `make_with`).
#![warn(unreachable_pub)]

use vike_marketdata::Bar;

mod fast_a;
mod fast_b;
mod fast_c;
mod fast_d1;
mod fast_d2;
mod fast_e;
mod fast_f;
mod fast_g;
mod fast_h;
mod fast_i;
mod fast_j;
pub mod feature;
pub mod frame;
pub mod indicators;
mod math;
pub mod pairs;
pub mod realized_vol;
pub mod registry;
#[allow(dead_code, clippy::allow_attributes)] // the steppers adopt these one family at a time
mod roll;
pub mod rolling;
pub mod series;
mod series_a;
mod series_b;
mod series_c;
mod series_d1;
mod series_d2;
mod series_e;
mod series_f;
mod series_g;
mod series_h;
mod series_i;
mod series_j;
mod typed;
pub mod window;

// The STUDY↔STRATEGY parity harness. Behind `test-support` so a shipped build compiles NONE of it —
// the `vike-ml`/`vike-data` model. It is a LIBRARY module rather than a `tests/` file because the
// feature definitions it exists to check do not live in this repository: per
// `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` (R1) a study's own
// source is the author's and sits in `user_data/`, so a test binary here could never run on it.
// `crates/vike-indicators/tests/feature_parity.rs` is one caller; the study is another.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

// ⚠ The kernel types are public through `indicators` — `vike_indicators::indicators::Rsi` — and
// NOT re-exported at the crate root. Until the first crates.io release they were reached only
// through [`registry`] (`make`/`make_with` hand back a `Box<dyn Indicator>`), because a crate-root
// `pub use` copied identifier-for-identifier from `indicators/mod.rs` stood here with ZERO users
// and named only the original `base.rs` set. A published crate has users who name the type
// (`Rsi::new(14.0)?`, `macd.next(&bar)`), which is the caller that line was waiting for; the
// namespace is `indicators::` so 177 type names do not crowd the crate root, and `indicators/mod.rs`
// is the ONE `pub use` per family, so every type still has exactly one path. The registry stays the
// door for a name chosen at run time.
// ⚠ No crate-root re-export of [`feature`]'s types, deliberately, and the precedent is
// [`window::WindowSpec`] rather than [`Indicator`]: a module that carries its own vocabulary is
// named through its module (`vike_indicators::feature::ColumnFeature`), which is how the one
// existing out-of-crate consumer of `window` spells it. Add one here only when a caller needs the
// shorter path — and say which caller.
pub use pairs::{PairIndicator, PairMeta, pair_lookback, pair_make, pair_make_with, pair_registry};
pub use registry::{
    Category, IndicatorMeta, OutSpec, OutputStyle, ParamSpec, RenderKind, UserFactory, coerce,
    compute, get, lookback, lookback_full, make, make_with, registry,
};
pub use typed::ParamError;

/// How far back an indicator's batch value at bar `i` can reach.
///
/// An indicator that streams by recomputing its batch kernel over a retained tail of bars
/// (see [`Indicator::trims_history`]) uses this to decide how many bars it must keep so that the
/// streamed values equal the batch values exactly. A kernel that folds a bounded window and
/// carries nothing across bars is a pure function of that window; a kernel with an
/// exponential or Wilder recurrence keeps a decaying contribution from every earlier bar and
/// needs a retention sized against that decay instead. [`keep_for`] turns a value of this type
/// into a bar count.
//
// ⚠ **The classification is STRUCTURAL, and it must be.**
// `crates/vike-indicators/src/indicators/classify.rs`'s `window_reach` is the authority and carries
// the per-name evidence.
// `parity/past_the_trim.rs`'s `every_trimmed_indicator_is_truncation_invariant` is a BACKSTOP, not the
// authority: it was MEASURED that `stochrsi` looks invariant from 30 retained bars on that
// gate's own 4,000-bar series while its Wilder `rsi_vals` sub-kernel needs ~493 bars on every
// data shape tried — so a wrong `Finite` declaration for it would be greenlit. That is the
// `declaration-pinning-tests-dont-gate` failure mode this repo has already hit; the cure is that
// the declaration cites the kernel, not the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowReach {
    /// The value at bar `i` is a fold over a fixed count of consecutive bars ending at `i`, and
    /// nothing is carried between bars. Retention needs the window plus a small slack.
    Finite,
    /// An infinite impulse response (exponential or Wilder smoothing) whose smoothing period is
    /// known. Retention is bounded by the decay of that period (a multiple of it, see
    /// [`SMOOTHED_FACTOR`]) rather than by a blanket multiple of the warm-up depth.
    //
    // ⚠ The distinction is worth a variant because `lookback_full` is the WRONG quantity to scale
    // an IIR bound by, and it is wrong in the expensive direction. `relative_volatility` declares
    // `lookback_full = 2p - 2`, so at its default `p = 14` the blanket rule retains
    // `64 * 26 + 1 = 1665` bars — while the decay it actually needs is over `p`, measured stable
    // from **305** bars (analytic EMA bound `18.4 * 14 = 258`). `stochrsi` is the same shape: 1857
    // retained against a Wilder `rsi_vals` sub-kernel measured to need ~493.
    //
    // The factor is [`SMOOTHED_FACTOR`], sized for WILDER (`alpha = 1/n`, the slower decay) so one
    // number is safe for both families — see [`keep_for`].
    //
    // ⚠ NOT for a kernel whose reach is unbounded for a NON-decay reason. `hvol` and `kst` fold
    // over `smooth_defined`'s DEFINED values, so a single non-positive close creates an interior
    // gap and the window spans arbitrarily many bars. No decay bound helps that, and they keep
    // [`WindowReach::Smoothed`].
    SmoothedOver(usize),
    /// The value carries an infinite impulse response whose period is not named, or reaches a
    /// variable distance back (for example a window over values with the gaps removed can span
    /// arbitrarily many bars when the source has interior gaps). Retention is the conservative
    /// [`KEEP_FACTOR`] multiple of the warm-up depth. This is the default: over-retaining costs
    /// memory and streaming time, under-retaining would return a different number.
    Smoothed,
}

/// Multiple of the full warm-up depth that a [`WindowReach::Smoothed`] indicator retains
/// (see [`keep_for`]); [`KEEP_FLOOR`] is the minimum retained length.
//
// ("`hist_indicator!`'s retained-history multiple for the [`WindowReach::Smoothed`] family, and its
// floor — promoted out of the macro so a TEST can compute the same `keep` the drain does.
//
// A gate that hard-coded these would drift the moment the macro changed; a gate that reads them
// asks the real question. `crates/vike-indicators/src/indicators/macros.rs` is where they are
// USED, and its comment carries the derivation (`F*n*alpha > 53*ln2` — Wilder sets the bound).")
pub const KEEP_FACTOR: usize = 64;
/// Minimum number of bars a [`WindowReach::Smoothed`] or [`WindowReach::SmoothedOver`] indicator
/// retains. See [`KEEP_FACTOR`].
pub const KEEP_FLOOR: usize = 256;

/// The multiple of its smoothing period that a [`WindowReach::SmoothedOver`] indicator retains.
///
/// `37` covers both Wilder smoothing (`alpha = 1/n`, which needs more than about 36.7) and
/// exponential smoothing (`alpha = 2/(n+1)`, which needs more than about 18.4).
//
// `37`, from the Wilder bound `F * n * alpha > 53*ln2` with `alpha = 1/n` (`F > 36.7`). EMA
// (`alpha = 2/(n+1)`) needs only `> 18.4`, so one number covers both families. Measured margin at
// the two rows that use it: `37 * 14 = 518` retained against `relative_volatility`'s 305 and
// `stochrsi`'s ~493.
pub const SMOOTHED_FACTOR: usize = 37;

/// Extra bars retained beyond a [`WindowReach::Finite`] indicator's own warm-up depth.
//
// A finite kernel needs `lookback_full + 1` bars and not one more, so this is pure margin against
// a `lookback_full` that under-reports by a bar or two (`tr[i]` reading `c[i-1]`, `dpo`'s
// displacement, `eom`'s NaN at index 0). It is NOT margin against an unbounded reach — a kernel
// that can reach an unbounded distance is [`WindowReach::Smoothed`] by definition, and no slack
// is the right amount of slack for "unbounded".
pub const FINITE_SLACK: usize = 32;
/// Minimum number of bars a [`WindowReach::Finite`] indicator retains. See [`FINITE_SLACK`].
//
// The floor matters for the paramless/shallow kernels (`ac`'s
// `lookback_full` is a constant 37, `trima` at `period = 1` is 0) where the slack alone would be
// a very small buffer to amortise the drain over.
pub const FINITE_FLOOR: usize = 64;

/// The number of bars a streaming indicator retains, for an indicator whose last output line
/// warms at `lookback_full` and whose batch kernel reaches back as far as `reach` says.
///
/// An indicator that streams by recomputing over a retained tail drops old bars once its buffer
/// reaches `2 * keep`, so the retained tail is always in `keep ..= 2*keep - 1` bars.
//
// ⚠ **Callers pass the indicator's OWN `reach`, never a reconstructed policy.** `parity.rs` and
// the drain must compute the same number, so both read [`Indicator::window_reach`]; a gate that
// re-derived which family an indicator belongs to would be testing its own copy of the answer.
pub fn keep_for(lookback_full: usize, reach: WindowReach) -> usize {
    match reach {
        // The decay bound, over the SMOOTHING period rather than the warm-up depth. Sized for
        // Wilder (`alpha = 1/n`, so `F * n * alpha > 53*ln2` gives `F > 36.7`) because that is the
        // slower of the two families and one safe number beats two that must be kept straight.
        WindowReach::SmoothedOver(period) => period
            .saturating_mul(SMOOTHED_FACTOR)
            .saturating_add(1)
            .max(KEEP_FLOOR)
            // ⚠ Never MORE than the blanket rule would have retained: this variant exists to
            // shrink retention, and a deep `lookback_full` with a shallow smoothing period must not
            // make it grow.
            .min(lookback_full.saturating_mul(KEEP_FACTOR).saturating_add(1).max(KEEP_FLOOR)),
        WindowReach::Smoothed => {
            lookback_full.saturating_mul(KEEP_FACTOR).saturating_add(1).max(KEEP_FLOOR)
        }
        WindowReach::Finite => {
            lookback_full.saturating_add(FINITE_SLACK).saturating_add(1).max(FINITE_FLOOR)
        }
    }
}

/// Object-safe state clone for boxed indicators.
///
/// Lets a caller evaluate a speculative bar (for example a still-forming live bar, which a
/// streaming fold cannot un-see) on a throwaway copy, without advancing the original instance.
/// Implemented for every `Clone` indicator; `Indicator: BoxedClone` makes cloneability part of
/// the contract.
pub trait BoxedClone {
    /// An independent copy of the full streaming state as a fresh boxed trait
    /// object. Feeding the copy never affects the original (and vice versa).
    fn clone_box(&self) -> Box<dyn Indicator>;
}

impl<T: Indicator + Clone + 'static> BoxedClone for T {
    fn clone_box(&self) -> Box<dyn Indicator> {
        Box::new(self.clone())
    }
}

/// A technical indicator with a streaming path and a batch path that give the same numbers.
///
/// [`Indicator::on_bar`] feeds one bar at a time; [`Indicator::vectorize`] computes a whole
/// history at once. Folding `on_bar` over a series equals `vectorize` on that series bit for bit
/// (except for the batch-only indicators, see [`registry::IndicatorMeta::batch_only`]).
/// Concrete types are in [`indicators`]; a name chosen at run time goes through [`make`] or
/// [`make_with`].
pub trait Indicator: BoxedClone + Send + Sync {
    /// Streaming: feed the next bar, advance the internal state, and return this bar's
    /// output(s), one value per output line (for example three for MACD). Warm-up bars return
    /// `f64::NAN`. It never looks ahead.
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64>;

    /// Batch: compute the whole series at once, one `Vec<f64>` per output line, each as long as
    /// `bars`. Warm-up positions are `f64::NAN`. This is the reference that `on_bar` reproduces.
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>>;

    /// The most recent `on_bar` output without advancing (same shape as `on_bar`).
    fn value(&self) -> Vec<f64>;

    /// Clear all streaming state back to construction.
    fn reset(&mut self);

    /// The registry key (e.g. "macd").
    fn name(&self) -> &str;

    /// Warm-up depth: the index of the first bar at which `vectorize` produces a non-NaN value on
    /// **any** output line. `lookback() == 0` means some line is defined from the very first bar.
    ///
    /// **This is not a seed size for a multi-line indicator.** On indicators whose lines warm at
    /// different indices (`adx`'s namesake line trails its `+DI`/`-DI` by `period - 1`; `macd`'s
    /// signal and histogram trail the macd line; `stochastic`'s %D trails %K) reading
    /// `cols[i][lookback()]` yields NaN on the later lines. To size a seed history so that every
    /// line is valid, use [`Indicator::lookback_full`], which is `>= lookback()`.
    ///
    /// The value is a conservative lower bound: an indicator that overrides it returns the exact
    /// index, and the default returns `0`, which is a lower bound for every indicator. Seeding at
    /// least `lookback()` bars is therefore always safe, but a non-NaN value at exactly that
    /// index is only guaranteed when [`Indicator::lookback_exact`] is `true`. Data-dependent
    /// indicators (candlestick patterns, pivot and structure series) have no derivable warm-up
    /// and keep the `0` default.
    fn lookback(&self) -> usize {
        0
    }

    /// Full warm-up depth: the index of the first bar at which **every** output line of
    /// `vectorize` is non-NaN. This is the number to use to size a seed history, since it makes
    /// `cols[line][lookback_full()]` valid for all lines. Always `>= lookback()`; equal for
    /// single-line indicators and for multi-line ones whose lines warm together.
    ///
    /// Exactness follows [`Indicator::lookback_exact`]. The default forwards to
    /// [`Indicator::lookback`], which is correct for every single-line indicator.
    fn lookback_full(&self) -> usize {
        self.lookback()
    }

    /// `true` when this indicator streams by recomputing its batch kernel over a bounded tail
    /// of recent bars, rather than by an incremental update. Such an indicator keeps about
    /// [`keep_for`]`(lookback_full, window_reach)` bars. A hand-written incremental indicator
    /// (for example `Sma` or `Obv`) folds from the first bar and never truncates, so this is
    /// `false` for it.
    //
    // A hand-written incremental impl (`Sma`, `Obv`, `Psar`, …) folds from bar 0 and never
    // truncates, so the truncation-invariance question does not arise for it. This flag is what
    // lets `parity/past_the_trim.rs`'s `every_trimmed_indicator_is_truncation_invariant` ask ONLY the
    // indicators the drain can actually reach, rather than a hand-kept name list — the shape of
    // list that was wrong in #1183 and is the reason that gate exists.
    fn trims_history(&self) -> bool {
        false
    }

    /// How far back this indicator's batch kernel can reach, see [`WindowReach`]. [`keep_for`]
    /// uses it to size the retained tail of an indicator for which
    /// [`Indicator::trims_history`] is `true`.
    ///
    /// The default is the conservative [`WindowReach::Smoothed`], which retains the large
    /// multiple; an incremental indicator never truncates, so the value is unused for it.
    fn window_reach(&self) -> WindowReach {
        WindowReach::Smoothed
    }

    /// `true` when the indicator's value depends on all history since it was started rather than
    /// on a bounded window, so a small `lookback()` does not mean it is warm. These are the
    /// cumulative and path-dependent series, including `psar`, `vwap`, `mcginley`, `obv`, `ad`,
    /// `adosc`, `asi`, `nvi`, `pvi`, `pvt`, `net_volume`, `kama`, `vidya`, `frama`, `evwma`,
    /// `hilo_activator`, `atr_trailing_stop`, `williams_ad`, `anchored_vwap` and `heikin_ashi`.
    /// Most emit a number from the first bar, but it depends on where the series started and
    /// differs from what a chart holding the full history shows (`vwap` accumulates only from
    /// the start of the UTC day as seen, and `psar` has to assume an initial trend). Seed as much
    /// history as is available for these (for `vwap`, at least back to the UTC-day boundary).
    //
    // ⚠ The membership is
    // `crates/vike-indicators/src/indicators/classify.rs`'s `is_path_dependent` and NOT this sentence,
    // which named eight of them while the set held eleven — `net_volume`, `asi` and `adosc` were
    // missing, and `asi`/`adosc` joined in #1183 precisely because a name list was wrong.
    // Their [`Indicator::lookback`] is a
    // truthful `0` (they emit a number from bar one) but that number is NOT the
    // same number a chart holding full history shows: `Psar` fabricates its initial
    // trend, `Vwap` accumulates only from the current UTC day *as seen*, and the
    // cumulative sums start from whatever bar the mount began at.
    //
    // A caller that seeds `lookback()` bars for one of these is NOT warm — seed as
    // much history as is available (for `vwap`, at least back to the UTC-day
    // boundary). Gated by name in `tests/lookback.rs`.
    fn warmup_path_dependent(&self) -> bool {
        false
    }

    /// `true` when [`Indicator::lookback`] is a real, parameter-derived value equal to the exact
    /// first non-NaN index; `false` when it is the conservative `0` default. It lets a caller
    /// tell "no warm-up" from "warm-up unknown".
    fn lookback_exact(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod send_bound {
    use super::*;
    #[test]
    fn boxed_indicator_is_send() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Box<dyn Indicator>>(); // registry make_with returns this
        assert_sync::<Box<dyn Indicator>>(); // both halves of the trait's `Send + Sync` bound
        let ind = make_with("sma", &[3.0]).unwrap();
        std::thread::spawn(move || {
            let _ = ind;
        })
        .join()
        .unwrap(); // moves across threads
    }
}
