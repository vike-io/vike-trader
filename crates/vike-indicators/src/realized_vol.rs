//! Realized volatility — ONE estimator family over [`Bar`]s, per-period by contract, annualized
//! only by an explicit call that names the sampling frequency.
//!
//! NEW code with no Python twin (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`:
//! vike is the reference), so no parity fixture governs it. Its oracle is the hand-unrolled folds in
//! `crates/vike-indicators/tests/realized_vol.rs`, which rebuild each estimator from the same
//! primitives and compare EXACT bits. The streaming core started as `vike-options`' `vol.rs`
//! (`EwmaVol`, `RollingVol`, `annualize`, `deannualize`, `DAYS_PER_YEAR`, `RISKMETRICS_LAMBDA`),
//! which had no consumer outside that crate; it lives here now because this crate is the home of
//! series maths, and `vike-options` drops its copies.
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
//! `ln 2` is [`std::f64::consts::LN_2`], the correctly rounded constant, so no estimator takes a
//! logarithm of a constant at run time.
//!
//! # How this relates to `hvol` — it is NOT the same quantity, and `hvol` is untouched
//!
//! `crates/vike-indicators/src/indicators/volatility.rs`'s `batch_hvol` folds the same log returns
//! over the same DEFINED-return window, but it is the POPULATION standard deviation (divides by
//! `period`, not `period − 1`), multiplied by `√ann` and by `100` (an annualized PERCENT), with `ann`
//! a registry parameter defaulting to 365. [`Estimator::CloseToClose`] is the SAMPLE standard
//! deviation, per period, as a fraction. In real arithmetic
//! `hvol = close_to_close · √((n − 1)/n) · √ann · 100`; in f64 the two are different folds and are
//! NOT bit-equal, so neither reuses the other's kernel. `hvol` and its parity rows stay exactly as
//! they were.
//!
//! # Annualization is a separate, explicit step
//!
//! [`annualize`] / [`deannualize`] take a REQUIRED `periods_per_year`; nothing in this module
//! assumes a year. [`DAYS_PER_YEAR`] (365) is a named CONVENTION for calendar-day daily bars (crypto
//! trades every day) and is never applied for you: an equity caller passes 252, an hourly crypto
//! caller passes `365.0 * 24.0`, and the registry rows (`rvol_*`) emit per-period values only.
//!
//! # Input rules — the same as `hvol`'s, stated here so they are not inferred
//!
//! - A price is USABLE when it is finite and `> 0`. (`batch_hvol` checks `> 0` only, so a `+inf`
//!   close passes there and poisons its window; this family refuses it.)
//! - A per-bar term EXISTS only when every price that estimator reads is usable on that bar — and,
//!   for the two that read `C₋₁`, on the IMMEDIATELY previous bar. A return never spans a bad bar:
//!   one unusable close removes two close-to-close returns, exactly as in `batch_hvol`. (The
//!   `vike-options` original skipped a bad tick and took the next return against the last good
//!   price; that is the behaviour that changed in the move.)
//! - A bar whose term does not exist emits `NaN` and leaves the estimator's state as it was; the
//!   windowed estimators fold the last `n` EXISTING terms (so after a gap a window spans more than
//!   `n` bars — the same compaction `batch_hvol` does), and emit `NaN` until `n` exist.
//! - The first bar has no `C₋₁`: `Ewma`, `CloseToClose` and `YangZhang` are `NaN` there.
//! - OHLC ordering (`L ≤ O, C ≤ H`) is NOT checked. A bar that violates it can make a
//!   Garman–Klass or Rogers–Satchell term negative and, on a window dominated by such bars, the
//!   variance negative, whose square root is `NaN` — reported, not clamped.
//!
//! # The constant-window rule (the crate page's), per estimator
//!
//! The two DEMEANED folds — [`Estimator::CloseToClose`] and Yang–Zhang's `σ²_o`/`σ²_c` — go
//! through one helper that returns exactly `0.0` when the window's values are bit-equal
//! (`crates/vike-indicators/src/window.rs`'s `is_constant_window`), so a series compounding at a fixed rate is zero
//! close-to-close volatility rather than a rounding residual. The others subtract no mean, so there
//! is nothing to cancel: a bar that did not move has `ln(1) = 0` exactly, every term is exactly `0`,
//! and a repeated non-zero term is the estimator's true non-zero value. Each site is declared in
//! `crates/vike-indicators/tests/parity/variance_fold_gate.rs`'s `VARIANCE_FOLD_SITES`.
//!
//! # Streaming equals batch by CONSTRUCTION
//!
//! [`Estimator::series`] IS [`RealizedVol::update`] folded over the bars, so there is one
//! arithmetic and nothing to drift; `tests/parity`'s `on_bar == vectorize` rows for `rvol_*` are
//! therefore true by construction rather than evidence. Folds are naive left folds over a window
//! held oldest-first, and `sqrt` is `f64::sqrt` (IEEE 754 rounds it correctly, so it is the same
//! number on every box); every logarithm is `libm::log`, never `f64::ln`
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`).

use std::f64::consts::LN_2;

use vike_marketdata::Bar;

use crate::Indicator;
use crate::math::sq;
use crate::window::is_constant_window;

/// The RiskMetrics EWMA decay for DAILY data (RiskMetrics Technical Document, 1996).
pub const RISKMETRICS_LAMBDA: f64 = 0.94;

/// Calendar days per year — a CONVENTION for daily bars of a market that trades every day (crypto),
/// matching `vike-options`' 365-day expiry basis. It is never applied implicitly: pass it to
/// [`annualize`] when it is the right basis, and pass the right basis when it is not (252 for
/// equity trading days, `365.0 * 24.0` for hourly crypto bars).
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

    /// The per-period volatility at every bar — [`RealizedVol::update`] folded over `bars`, so the
    /// batch and the stream are one arithmetic. Panics on an invalid parameter (see
    /// [`RealizedVol::new`]).
    pub fn series(self, bars: &[Bar]) -> Vec<f64> {
        let mut rv = RealizedVol::new(self);
        bars.iter().map(|b| rv.update(b)).collect()
    }
}

/// A streaming realized-volatility estimator: feed one [`Bar`] at a time, read the per-period
/// volatility. Bounded state — the last `window` existing terms, or one running variance for
/// [`Estimator::Ewma`].
#[derive(Debug, Clone)]
pub struct RealizedVol {
    estimator: Estimator,
    /// The previous bar's close when it was usable; `None` after an unusable one, so a return never
    /// spans a bad bar.
    prev_close: Option<f64>,
    /// The windowed estimators' per-bar terms, oldest first, at most `window` long. `xs` carries
    /// the one term most estimators have (and Yang–Zhang's overnight return), `ys` Yang–Zhang's
    /// open-to-close return, `zs` its Rogers–Satchell term.
    xs: Vec<f64>,
    ys: Vec<f64>,
    zs: Vec<f64>,
    /// [`Estimator::Ewma`]'s running variance, `None` until the first return.
    variance: Option<f64>,
    /// The last [`RealizedVol::update`] output.
    last: f64,
}

impl RealizedVol {
    /// A fresh estimator. Panics on a configuration error (not bad data): a `lambda` outside
    /// `(0, 1)` or a `window` below 2 — a sample variance of one value does not exist.
    pub fn new(estimator: Estimator) -> Self {
        match estimator {
            Estimator::Ewma { lambda } => {
                assert!(lambda > 0.0 && lambda < 1.0, "EWMA lambda must be in (0, 1), got {lambda}")
            }
            Estimator::CloseToClose { window }
            | Estimator::Parkinson { window }
            | Estimator::GarmanKlass { window }
            | Estimator::RogersSatchell { window }
            | Estimator::YangZhang { window } => {
                assert!(window >= 2, "realized-vol window must be >= 2, got {window}")
            }
        }
        Self {
            estimator,
            prev_close: None,
            xs: Vec::new(),
            ys: Vec::new(),
            zs: Vec::new(),
            variance: None,
            last: f64::NAN,
        }
    }

    /// The estimator this instance folds.
    pub fn estimator(&self) -> Estimator {
        self.estimator
    }

    /// Fold one bar and return this bar's per-period volatility — `NaN` while warming up or when
    /// this bar's term does not exist (module doc, "Input rules").
    pub fn update(&mut self, bar: &Bar) -> f64 {
        let prev = self.prev_close;
        self.prev_close = usable(bar.close).then_some(bar.close);
        let out = match self.estimator {
            Estimator::Ewma { lambda } => match log_return(prev, bar.close) {
                Some(r) => {
                    let v = ewma_step(self.variance, r, lambda);
                    self.variance = Some(v);
                    v.sqrt()
                }
                None => f64::NAN,
            },
            Estimator::CloseToClose { window } => match log_return(prev, bar.close) {
                Some(r) => {
                    push_bounded(&mut self.xs, r, window);
                    if self.xs.len() == window { sample_var(&self.xs).sqrt() } else { f64::NAN }
                }
                None => f64::NAN,
            },
            Estimator::Parkinson { window } => {
                self.fold_mean(parkinson_term(bar), window, 1.0 / (4.0 * LN_2))
            }
            Estimator::GarmanKlass { window } => {
                self.fold_mean(garman_klass_term(bar), window, 1.0)
            }
            Estimator::RogersSatchell { window } => {
                self.fold_mean(rogers_satchell_term(bar), window, 1.0)
            }
            Estimator::YangZhang { window } => match yang_zhang_terms(prev, bar) {
                Some((overnight, open_close, rs)) => {
                    push_bounded(&mut self.xs, overnight, window);
                    push_bounded(&mut self.ys, open_close, window);
                    push_bounded(&mut self.zs, rs, window);
                    if self.xs.len() == window {
                        let n = window as f64;
                        let k = 0.34 / (1.34 + (n + 1.0) / (n - 1.0));
                        let var = sample_var(&self.xs)
                            + k * sample_var(&self.ys)
                            + (1.0 - k) * mean(&self.zs);
                        var.sqrt()
                    } else {
                        f64::NAN
                    }
                }
                None => f64::NAN,
            },
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
        *self = Self::new(self.estimator);
    }

    /// Shared tail of the three single-bar range estimators: push the term when it exists and,
    /// once `window` exist, return `√(scale · mean)`.
    fn fold_mean(&mut self, term: Option<f64>, window: usize, scale: f64) -> f64 {
        match term {
            Some(t) => {
                push_bounded(&mut self.xs, t, window);
                if self.xs.len() == window { (scale * mean(&self.xs)).sqrt() } else { f64::NAN }
            }
            None => f64::NAN,
        }
    }
}

// ================================= per-bar terms ==================================

/// Finite and strictly positive — the only prices a logarithm is taken of.
fn usable(p: f64) -> bool {
    p.is_finite() && p > 0.0
}

/// `ln(close / prev)`, when both exist and are usable.
fn log_return(prev: Option<f64>, close: f64) -> Option<f64> {
    let prev = prev?;
    usable(close).then(|| libm::log(close / prev))
}

/// One RiskMetrics step: the first return SEEDS the variance at `r²` (a zero seed would understate
/// volatility for many periods); every later one folds `λ·v + (1 − λ)·r²`. `(1.0 - lambda)` is
/// spelled as the recursion states it — in f64 `1.0 - 0.94` is not the literal `0.06`.
fn ewma_step(variance: Option<f64>, r: f64, lambda: f64) -> f64 {
    let r2 = sq(r);
    match variance {
        None => r2,
        Some(v) => lambda * v + (1.0 - lambda) * r2,
    }
}

/// Parkinson's per-bar term `ln(H/L)²`.
fn parkinson_term(bar: &Bar) -> Option<f64> {
    (usable(bar.high) && usable(bar.low)).then(|| sq(libm::log(bar.high / bar.low)))
}

/// `2·ln 2 − 1`, Garman–Klass's weight on the squared open-to-close return.
const GARMAN_KLASS_CLOSE_WEIGHT: f64 = 2.0 * LN_2 - 1.0;

/// Garman–Klass's per-bar term `½·ln(H/L)² − (2·ln 2 − 1)·ln(C/O)²`.
fn garman_klass_term(bar: &Bar) -> Option<f64> {
    let (hl, co) = ohlc_logs(bar).map(|l| (l.high_low, l.close_open))?;
    Some(0.5 * sq(hl) - GARMAN_KLASS_CLOSE_WEIGHT * sq(co))
}

/// Rogers–Satchell's per-bar term `ln(H/C)·ln(H/O) + ln(L/C)·ln(L/O)`.
fn rogers_satchell_term(bar: &Bar) -> Option<f64> {
    ohlc_logs(bar).map(|l| l.rogers_satchell)
}

/// Yang–Zhang's three per-bar terms: the overnight return `ln(O/C₋₁)`, the open-to-close return
/// `ln(C/O)` and the Rogers–Satchell term.
fn yang_zhang_terms(prev: Option<f64>, bar: &Bar) -> Option<(f64, f64, f64)> {
    let prev = prev?;
    let l = ohlc_logs(bar)?;
    Some((libm::log(bar.open / prev), l.close_open, l.rogers_satchell))
}

/// The log ratios an OHLC estimator reads from one bar.
struct OhlcLogs {
    high_low: f64,
    close_open: f64,
    rogers_satchell: f64,
}

/// [`OhlcLogs`] for a bar whose four prices are all usable.
fn ohlc_logs(bar: &Bar) -> Option<OhlcLogs> {
    let Bar { open: o, high: h, low: l, close: c, .. } = *bar;
    if !(usable(o) && usable(h) && usable(l) && usable(c)) {
        return None;
    }
    let (hc, ho) = (libm::log(h / c), libm::log(h / o));
    let (lc, lo) = (libm::log(l / c), libm::log(l / o));
    Some(OhlcLogs {
        high_low: libm::log(h / l),
        close_open: libm::log(c / o),
        rogers_satchell: hc * ho + lc * lo,
    })
}

// ================================= window folds ===================================

/// Append `x`, dropping the oldest value once the window holds `window`.
fn push_bounded(w: &mut Vec<f64>, x: f64, window: usize) {
    if w.len() == window {
        w.remove(0);
    }
    w.push(x);
}

/// Arithmetic mean, a naive left fold (net-new Rust with no Python twin, so no `py_sum`).
fn mean(w: &[f64]) -> f64 {
    let mut s = 0.0;
    for &x in w {
        s += x;
    }
    s / w.len() as f64
}

/// Two-pass SAMPLE variance (`n − 1`). A window of bit-equal values is exactly `0.0` without
/// folding — the crate's constant-window rule, through its one spelling.
fn sample_var(w: &[f64]) -> f64 {
    if is_constant_window(w) {
        return 0.0;
    }
    let m = mean(w);
    let mut ss = 0.0;
    for &x in w {
        ss += sq(x - m);
    }
    ss / (w.len() as f64 - 1.0)
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
