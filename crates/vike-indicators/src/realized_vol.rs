//! Realized volatility: one family of estimators over [`Bar`]s, per period by contract, annualized
//! only by an explicit call that names the sampling frequency.
//!
//! [`Estimator`] picks the estimator, [`RealizedVol`] streams it one bar at a time, and
//! [`Estimator::series`] computes it over a whole history. [`annualize`] and [`deannualize`]
//! convert between per-period and annual volatility. The same estimators are in the registry as
//! the `rvol_*` indicators (`rvol_ewma`, `rvol_close_to_close`, `rvol_parkinson`,
//! `rvol_garman_klass`, `rvol_rogers_satchell`, `rvol_yang_zhang`), which emit per-period values
//! only.
//!
//! # The estimators ([`Estimator`])
//!
//! Every value is a per-period VOLATILITY (a standard deviation of log price changes per bar), the
//! square root of the variance named below. `n` is the window, `O/H/L/C` the bar's prices and
//! `C₋₁` the previous bar's close.
//!
//! - [`Estimator::Ewma`] — RiskMetrics (J.P. Morgan/Reuters, *RiskMetrics — Technical Document*,
//!   4th ed., 1996), zero mean, reads `C₋₁, C`: `v₁ = r₁²`, `vₜ = λ·vₜ₋₁ + (1 − λ)·rₜ²`,
//!   `r = ln(C/C₋₁)`.
//! - [`Estimator::CloseToClose`] — reads `C₋₁, C`: the SAMPLE variance of the last `n` log
//!   returns, demeaned, `Σ(r − r̄)² / (n − 1)`.
//! - [`Estimator::Parkinson`] — Parkinson (1980), reads `H, L`:
//!   `(1 / (4·ln 2)) · (1/n)·Σ ln(H/L)²`.
//! - [`Estimator::GarmanKlass`] — Garman & Klass (1980), reads `O, H, L, C`:
//!   `(1/n)·Σ [½·ln(H/L)² − (2·ln 2 − 1)·ln(C/O)²]`.
//! - [`Estimator::RogersSatchell`] — Rogers & Satchell (1991), reads `O, H, L, C`:
//!   `(1/n)·Σ [ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)]`.
//! - [`Estimator::YangZhang`] — Yang & Zhang (2000), reads `C₋₁, O, H, L, C`:
//!   `σ²_o + k·σ²_c + (1 − k)·σ²_rs` with `k = 0.34 / (1.34 + (n + 1)/(n − 1))`, where `σ²_o` is
//!   the sample variance of the overnight returns `ln(O/C₋₁)`, `σ²_c` that of the open-to-close
//!   returns `ln(C/O)`, and `σ²_rs` the Rogers–Satchell mean, all over the same `n` bars.
//!
//! Parkinson and Garman–Klass assume zero drift and no opening gap; Rogers–Satchell is
//! independent of drift; Yang–Zhang is built to be independent of both drift and the opening gap.
//!
//! # How this relates to `hvol`
//!
//! The `hvol` indicator folds the same log returns over the same window of defined returns, but it
//! is the POPULATION standard deviation (divides by `period`, not `period − 1`), multiplied by
//! `√ann` and by `100` (an annualized PERCENT), with `ann` a parameter defaulting to 365.
//! [`Estimator::CloseToClose`] is the SAMPLE standard deviation, per period, as a fraction. In real
//! arithmetic `hvol = close_to_close · √((n − 1)/n) · √ann · 100`; in f64 the two are different
//! folds and are not bit-equal.
//!
//! # Annualization is a separate, explicit step
//!
//! [`annualize`] / [`deannualize`] take a required `periods_per_year`; nothing in this module
//! assumes a year. [`DAYS_PER_YEAR`] (365) is a named CONVENTION for calendar-day daily bars (crypto
//! trades every day) and is never applied for you: an equity caller passes 252, an hourly crypto
//! caller passes `365.0 * 24.0`, and the registry rows (`rvol_*`) emit per-period values only.
//!
//! # Input rules
//!
//! - A price is USABLE when it is finite and `> 0`.
//! - A per-bar term EXISTS only when every price that estimator reads is usable on that bar — and,
//!   for the two that read `C₋₁`, on the IMMEDIATELY previous bar. A return never spans a bad bar:
//!   one unusable close removes two close-to-close returns.
//! - A bar whose term does not exist emits `NaN` and leaves the estimator's state as it was; the
//!   windowed estimators fold the last `n` EXISTING terms (so after a gap a window spans more than
//!   `n` bars), and emit `NaN` until `n` exist.
//! - The first bar has no `C₋₁`: `Ewma`, `CloseToClose` and `YangZhang` are `NaN` there.
//! - OHLC ordering (`L ≤ O, C ≤ H`) is NOT checked. A bar that violates it can make a
//!   Garman–Klass or Rogers–Satchell term negative and, on a window dominated by such bars, the
//!   variance negative, whose square root is `NaN` — reported, not clamped.
//!
//! # Constant windows
//!
//! The two DEMEANED folds — [`Estimator::CloseToClose`] and Yang–Zhang's `σ²_o`/`σ²_c` — return
//! exactly `0.0` when every value in the window is bit-equal, so a series compounding at a fixed
//! rate has zero close-to-close volatility rather than a rounding residual. The other estimators
//! subtract no mean, so there is nothing to cancel: a bar that did not move has `ln(1) = 0`
//! exactly, and every term is exactly `0`.
//!
//! # Streaming equals batch by construction
//!
//! [`Estimator::series`] is [`RealizedVol::update`] folded over the bars, so there is one
//! arithmetic and the streaming and batch paths cannot differ. The windowed estimators keep the
//! last `n` existing terms in a ring with running sums (O(1) a bar; a variance on sums shifted by
//! the window's oldest value, rebuilt from the ring every `ceil(512 / n)` laps), so a value can
//! differ from a fold of the window from scratch in the last bits, by the rounding noise every
//! running sum has. `sqrt` is `f64::sqrt` (IEEE 754 rounds it correctly), and every logarithm is
//! `libm::log` rather than `f64::ln`, so the numbers are the same on every platform.
//
// ---- Engineering notes: not rendered on docs.rs. ----
// NEW code with no Python twin (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`:
// vike is the reference), so no parity fixture governs it. Its oracle is the hand-unrolled folds in
// `crates/vike-indicators/tests/realized_vol.rs`, which rebuild each estimator from the same
// primitives and compare EXACT bits. The streaming core started as `vike-options`' `vol.rs`
// (`EwmaVol`, `RollingVol`, `annualize`, `deannualize`, `DAYS_PER_YEAR`, `RISKMETRICS_LAMBDA`),
// which had no consumer outside that crate; it lives here now because this crate is the home of
// series maths, and `vike-options` drops its copies.
//
// `ln 2` is [`std::f64::consts::LN_2`], the correctly rounded constant, so no estimator takes a
// logarithm of a constant at run time.
//
// How this relates to `hvol` — it is NOT the same quantity, and `hvol` is untouched:
// `crates/vike-indicators/src/indicators/streams_window.rs`'s `Hvol` folds the same log returns
// over the same DEFINED-return window, but it is the POPULATION standard deviation (divides by
// `period`, not `period − 1`), multiplied by `√ann` and by `100` (an annualized PERCENT), with `ann`
// a registry parameter defaulting to 365. [`Estimator::CloseToClose`] is the SAMPLE standard
// deviation, per period, as a fraction. In real arithmetic
// `hvol = close_to_close · √((n − 1)/n) · √ann · 100`; in f64 the two are different folds and are
// NOT bit-equal, so neither reuses the other's kernel. `hvol` and its parity rows stay exactly as
// they were.
//
// Input rules — the same as `hvol`'s, stated here so they are not inferred:
// - (`batch_hvol` checks `> 0` only, so a `+inf` close passes there and poisons its window; this
//   family refuses it.)
// - (The `vike-options` original skipped a bad tick and took the next return against the last good
//   price; that is the behaviour that changed in the move.) One unusable close removes two
//   close-to-close returns, exactly as in `batch_hvol`; the windowed estimators' compaction is the
//   same one `batch_hvol` does.
//
// The constant-window rule (the crate page's), per estimator: the helper is
// `crates/vike-indicators/src/window.rs`'s `is_constant_window`. Each site is declared in
// `crates/vike-indicators/tests/parity/variance_fold_gate.rs`'s `VARIANCE_FOLD_SITES`.
//
// Streaming equals batch by CONSTRUCTION: `tests/parity`'s `on_bar == vectorize` rows for `rvol_*`
// are therefore true by construction rather than evidence. Every logarithm is `libm::log`, never
// `f64::ln`
// (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`).

use vike_marketdata::Bar;

use crate::Indicator;
use crate::fast_j::{
    CloseToCloseCore, EwmaVolCore, GarmanKlassCore, ParkinsonCore, RogersSatchellCore,
    YangZhangCore,
};

/// The RiskMetrics EWMA decay `lambda` for daily data (0.94; RiskMetrics Technical Document,
/// 1996).
pub const RISKMETRICS_LAMBDA: f64 = 0.94;

/// Calendar days per year — a CONVENTION for daily bars of a market that trades every day (crypto).
/// It is never applied implicitly: pass it to [`annualize`] when it is the right basis, and pass
/// the right basis when it is not (252 for equity trading days, `365.0 * 24.0` for hourly crypto
/// bars).
//
// Matches `vike-options`' 365-day expiry basis.
pub const DAYS_PER_YEAR: f64 = 365.0;

/// Per-period volatility → annual volatility: `vol · √periods_per_year`.
///
/// `periods_per_year` is how many bars of the series' own resolution make a year. There is no
/// default; a non-positive or non-finite basis yields `NaN` or `0`, never a guess.
pub fn annualize(vol_per_period: f64, periods_per_year: f64) -> f64 {
    vol_per_period * periods_per_year.sqrt()
}

/// Annual volatility → per-period volatility: the inverse of [`annualize`].
pub fn deannualize(vol_annual: f64, periods_per_year: f64) -> f64 {
    vol_annual / periods_per_year.sqrt()
}

/// Which realized-volatility estimator, with its one parameter. See the module doc for each
/// formula and its source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Estimator {
    /// RiskMetrics exponentially weighted, zero-mean. `lambda` must lie in `(0, 1)`;
    /// [`RISKMETRICS_LAMBDA`] is the daily-data default.
    Ewma { lambda: f64 },
    /// Sample standard deviation of the last `window` log close-to-close returns (`window >= 2`).
    CloseToClose { window: usize },
    /// Parkinson high–low range estimator over `window` bars (`window >= 2`).
    Parkinson { window: usize },
    /// Garman–Klass OHLC estimator over `window` bars (`window >= 2`).
    GarmanKlass { window: usize },
    /// Rogers–Satchell drift-independent OHLC estimator over `window` bars (`window >= 2`).
    RogersSatchell { window: usize },
    /// Yang–Zhang gap- and drift-robust estimator over `window` bars (`window >= 2`).
    YangZhang { window: usize },
}

impl Estimator {
    /// The registry key of this estimator's indicator row (`vike_indicators::make(name)` builds it
    /// at the registry's default parameter).
    pub fn name(self) -> &'static str {
        match self {
            Estimator::Ewma { .. } => "rvol_ewma",
            Estimator::CloseToClose { .. } => "rvol_close_to_close",
            Estimator::Parkinson { .. } => "rvol_parkinson",
            Estimator::GarmanKlass { .. } => "rvol_garman_klass",
            Estimator::RogersSatchell { .. } => "rvol_rogers_satchell",
            Estimator::YangZhang { .. } => "rvol_yang_zhang",
        }
    }

    /// The index of the first bar at which [`Estimator::series`] is not `NaN` when every bar is
    /// usable: one past the first bar for the estimators that read the previous close, `window - 1`
    /// for the single-bar range estimators. Bad bars push it later (see the module doc).
    pub fn lookback(self) -> usize {
        match self {
            Estimator::Ewma { .. } => 1,
            Estimator::CloseToClose { window } | Estimator::YangZhang { window } => window,
            Estimator::Parkinson { window }
            | Estimator::GarmanKlass { window }
            | Estimator::RogersSatchell { window } => window.saturating_sub(1),
        }
    }

    /// The per-period volatility at every bar — what [`RealizedVol::update_ohlc`] returns folded
    /// over `bars`, so the batch and the stream are one arithmetic. Panics on an invalid parameter
    /// (see [`RealizedVol::new`]).
    #[inline]
    pub fn series(self, bars: &[Bar]) -> Vec<f64> {
        let mut core = RealizedVol::new(self).core;
        // Filled first, then written by index: the page faults of a fresh buffer land in the fill,
        // not inside the recurrence.
        let mut out = vec![f64::NAN; bars.len()];
        match &mut core {
            Core::Ewma(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.close);
                }
            }
            Core::CloseToClose(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.close);
                }
            }
            Core::Parkinson(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.high, b.low);
                }
            }
            Core::GarmanKlass(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.open, b.high, b.low, b.close);
                }
            }
            Core::RogersSatchell(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.open, b.high, b.low, b.close);
                }
            }
            Core::YangZhang(c) => {
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = c.push(b.open, b.high, b.low, b.close);
                }
            }
        }
        out
    }
}

/// A streaming realized-volatility estimator: feed one [`Bar`] (or one bar's prices) at a time, read
/// the per-period volatility. Bounded state — the last `window` existing terms, or one running
/// variance for [`Estimator::Ewma`].
#[derive(Debug, Clone)]
pub struct RealizedVol {
    estimator: Estimator,
    /// The estimator's fast core (`crate::fast_j`): a ring of the last `window` existing terms with
    /// running sums, or the EWMA's one variance, and the previous usable close the estimators that
    /// read it keep.
    core: Core,
    /// The last [`RealizedVol::update`] output.
    last: f64,
}

/// One core per [`Estimator`] variant.
#[derive(Debug, Clone)]
enum Core {
    Ewma(EwmaVolCore),
    CloseToClose(CloseToCloseCore),
    Parkinson(ParkinsonCore),
    GarmanKlass(GarmanKlassCore),
    RogersSatchell(RogersSatchellCore),
    YangZhang(YangZhangCore),
}

impl RealizedVol {
    /// A fresh estimator. Panics on a configuration error (not bad data): a `lambda` outside
    /// `(0, 1)` or a `window` below 2 — a sample variance of one value does not exist.
    ///
    /// The window's ring is allocated up front, `window` values of 8 bytes each (times three for
    /// [`Estimator::YangZhang`]).
    pub fn new(estimator: Estimator) -> Self {
        let core = match estimator {
            Estimator::Ewma { lambda } => {
                assert!(
                    lambda > 0.0 && lambda < 1.0,
                    "EWMA lambda must be in (0, 1), got {lambda}"
                );
                Core::Ewma(EwmaVolCore::new(lambda))
            }
            Estimator::CloseToClose { window } => {
                Core::CloseToClose(CloseToCloseCore::new(checked_window(window)))
            }
            Estimator::Parkinson { window } => {
                Core::Parkinson(ParkinsonCore::new(checked_window(window)))
            }
            Estimator::GarmanKlass { window } => {
                Core::GarmanKlass(GarmanKlassCore::new(checked_window(window)))
            }
            Estimator::RogersSatchell { window } => {
                Core::RogersSatchell(RogersSatchellCore::new(checked_window(window)))
            }
            Estimator::YangZhang { window } => {
                Core::YangZhang(YangZhangCore::new(checked_window(window)))
            }
        };
        Self { estimator, core, last: f64::NAN }
    }

    /// The estimator this instance folds.
    pub fn estimator(&self) -> Estimator {
        self.estimator
    }

    /// Fold one bar and return this bar's per-period volatility — `NaN` while warming up or when
    /// this bar's term does not exist (module doc, "Input rules").
    #[inline]
    pub fn update(&mut self, bar: &Bar) -> f64 {
        self.update_ohlc(bar.open, bar.high, bar.low, bar.close)
    }

    /// [`RealizedVol::update`] for a bar's four prices as plain numbers, no [`Bar`] needed; the same
    /// answer for a bar with those prices. Each estimator reads a subset and ignores the rest:
    /// [`Estimator::Ewma`] and [`Estimator::CloseToClose`] the close, [`Estimator::Parkinson`] the
    /// high and low, the other three all four.
    #[inline]
    pub fn update_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        let out = match &mut self.core {
            Core::Ewma(c) => c.push(close),
            Core::CloseToClose(c) => c.push(close),
            Core::Parkinson(c) => c.push(high, low),
            Core::GarmanKlass(c) => c.push(open, high, low, close),
            Core::RogersSatchell(c) => c.push(open, high, low, close),
            Core::YangZhang(c) => c.push(open, high, low, close),
        };
        self.last = out;
        out
    }

    /// The last [`RealizedVol::update`] output, `None` when it was `NaN` (warming up, or a bar
    /// whose term did not exist).
    pub fn value(&self) -> Option<f64> {
        (!self.last.is_nan()).then_some(self.last)
    }

    /// [`RealizedVol::value`] through [`annualize`] at the caller's `periods_per_year`.
    pub fn annualized(&self, periods_per_year: f64) -> Option<f64> {
        self.value().map(|v| annualize(v, periods_per_year))
    }

    /// Back to the state [`RealizedVol::new`] produced, keeping the estimator.
    pub fn reset(&mut self) {
        match &mut self.core {
            Core::Ewma(c) => c.clear(),
            Core::CloseToClose(c) => c.clear(),
            Core::Parkinson(c) => c.clear(),
            Core::GarmanKlass(c) => c.clear(),
            Core::RogersSatchell(c) => c.clear(),
            Core::YangZhang(c) => c.clear(),
        }
        self.last = f64::NAN;
    }
}

/// A window the estimators accept: 2 or more (a sample variance of one value does not exist).
fn checked_window(window: usize) -> usize {
    assert!(window >= 2, "realized-vol window must be >= 2, got {window}");
    window
}

// =============================== the registry rows ================================

/// Registry-row ids for [`RvolRow`] — one per [`Estimator`] variant.
pub(crate) const RVOL_EWMA: u8 = 0;
pub(crate) const RVOL_CLOSE_TO_CLOSE: u8 = 1;
pub(crate) const RVOL_PARKINSON: u8 = 2;
pub(crate) const RVOL_GARMAN_KLASS: u8 = 3;
pub(crate) const RVOL_ROGERS_SATCHELL: u8 = 4;
pub(crate) const RVOL_YANG_ZHANG: u8 = 5;

/// The windowed rows' default window, shared with their `ParamSpec` so `make()` and
/// `make_with(defaults)` cannot disagree.
pub(crate) const DEFAULT_WINDOW: f64 = 20.0;

/// One registry row of the family: a [`RealizedVol`] whose estimator is picked by `KIND` and whose
/// one parameter is read positionally, the shape `registry.rs`'s `ind!` constructs.
#[derive(Clone)]
pub(crate) struct RvolRow<const KIND: u8>(RealizedVol);

impl<const KIND: u8> RvolRow<KIND> {
    /// `p[0]` is `lambda` for the EWMA row and `window` for the rest. The registry coerces it into
    /// the `ParamSpec` range first; a raw out-of-range value (the un-coerced path) falls back to
    /// the default rather than panicking, and a window is rounded the way every period here is.
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let x = p.first().copied();
        let window = || crate::indicators::pbars(x.unwrap_or(DEFAULT_WINDOW)).max(2);
        let estimator = match KIND {
            RVOL_EWMA => Estimator::Ewma {
                lambda: x.filter(|l| *l > 0.0 && *l < 1.0).unwrap_or(RISKMETRICS_LAMBDA),
            },
            RVOL_CLOSE_TO_CLOSE => Estimator::CloseToClose { window: window() },
            RVOL_PARKINSON => Estimator::Parkinson { window: window() },
            RVOL_GARMAN_KLASS => Estimator::GarmanKlass { window: window() },
            RVOL_ROGERS_SATCHELL => Estimator::RogersSatchell { window: window() },
            _ => Estimator::YangZhang { window: window() },
        };
        Self(RealizedVol::new(estimator))
    }
}

impl<const KIND: u8> Default for RvolRow<KIND> {
    fn default() -> Self {
        Self::with_params(&[])
    }
}

impl<const KIND: u8> Indicator for RvolRow<KIND> {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.0.update(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        vec![self.0.estimator.series(bars)]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.0.last]
    }
    fn reset(&mut self) {
        self.0.reset();
    }
    fn name(&self) -> &str {
        self.0.estimator.name()
    }
    fn lookback(&self) -> usize {
        self.0.estimator.lookback()
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
