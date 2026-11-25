//! Features read off a model of the series, each written from its published definition, bar by bar
//! and causal: GARCH(1,1) conditional volatility (`garch_volatility`), the forward filter of a
//! two-state Gaussian hidden Markov model (`hmm_regime`), the dominant cycle period of a rolling
//! discrete Fourier transform (`dominant_cycle`), the rolling Haar wavelet detail energy
//! (`haar_detail_energy`) and the rolling augmented Dickey-Fuller statistic of an autoregression of
//! the log close (`adf`). Cores and types live here; the rows are in `registry/models.rs`.
//!
//! The model parameters are INPUTS: nothing here fits the GARCH or HMM model. A fit is research-side
//! (a study estimates GARCH or HMM parameters on history, `vike-analytics`' `garch` and `hmm`), and the
//! fitted numbers are passed to these indicators, which then run the same filter in a backtest and
//! live: the filter steps are `vike_math::garch`'s and `vike_math::hidden_markov`'s, ONE copy for the row
//! and the fit. `adf`'s regression is `vike_math::stationarity_tests`'s `augmented_dickey_fuller`, the one copy of it.
//!
//! Sources:
//!
//! - Tim Bollerslev, "Generalized Autoregressive Conditional Heteroskedasticity", Journal of
//!   Econometrics 31 (1986) 307-327, <https://doi.org/10.1016/0304-4076(86)90063-1>: the GARCH(1,1)
//!   variance equation and its stationarity condition `alpha + beta < 1`.
//! - J.P. Morgan/Reuters, "RiskMetrics - Technical Document", 4th ed., 1996: the zero-mean
//!   exponentially weighted variance `v = lambda v[1] + (1 - lambda) r^2`, started at the first
//!   squared return (`crate::realized_vol`'s `Estimator::Ewma`), which is GARCH(1,1) with
//!   `omega = 0`, `alpha = 1 - lambda`, `beta = lambda`.
//! - James D. Hamilton, "A New Approach to the Economic Analysis of Nonstationary Time Series and
//!   the Business Cycle", Econometrica 57 (1989) 357-384, <https://doi.org/10.2307/1912559>: the
//!   filtered regime probability of a Markov-switching model.
//! - Lawrence R. Rabiner, "A Tutorial on Hidden Markov Models and Selected Applications in Speech
//!   Recognition", Proceedings of the IEEE 77 (1989) 257-286, <https://doi.org/10.1109/5.18626>:
//!   the forward variable (section III.A, problem 1) and its scaling (section V.A), here normalised
//!   on every observation.
//! - Gerald Goertzel, "An Algorithm for the Evaluation of Finite Trigonometric Series", The
//!   American Mathematical Monthly 65 (1958) 34-35, <https://doi.org/10.2307/2310304>: the
//!   second-order recurrence that gives one term of a discrete Fourier transform.
//! - Julius O. Smith III, "Spectral Audio Signal Processing", W3K Publishing, 2011, "Quadratic
//!   Interpolation of Spectral Peaks": the vertex of the parabola through a peak and its two
//!   neighbours, `p = (a - c) / (2 (a - 2b + c))`.
//! - Donald B. Percival and Andrew T. Walden, "Wavelet Methods for Time Series Analysis", Cambridge
//!   University Press, 2000, chapter 4: the Haar discrete wavelet transform and the decomposition
//!   of a series' energy by scale.
//! - Said E. Said and David A. Dickey, "Testing for unit roots in autoregressive-moving average models
//!   of unknown order", Biometrika 71(3) (1984) 599-607, <https://doi.org/10.1093/biomet/71.3.599>:
//!   the augmented Dickey-Fuller regression; James G. MacKinnon, "Critical Values for Cointegration
//!   Tests", Queen's Economics Department Working Paper 1227 (2010): the 5% critical value with a
//!   constant, about -2.86.
//!
//! # Input rule
//!
//! `garch_volatility`, `hmm_regime` and `haar_detail_energy` read the log return
//! `r = ln(close / close[1])` through the realized-volatility family's own return step
//! (`crate::fast_j`'s `next_log_return`, the rule `crate::realized_vol` states): a price is usable
//! when it is finite and above 0, and a return exists only when this bar's close and the IMMEDIATELY
//! previous bar's close are both usable (so one bad close removes two returns). The two recursive
//! ones (`garch_volatility`, `hmm_regime`) answer `NaN` on a bar with no return and keep their state
//! as it was; `haar_detail_energy` answers `NaN` while its window holds a missing return.
//! `dominant_cycle` reads the closes themselves, detrends them by the regression the `linearreg`
//! rows fold (`crate::fast_l`'s `ols_ordered`), and answers `NaN` while its window holds a value that
//! is not finite. `adf` reads the log close (`libm::log` of a usable close, `NaN` otherwise) and
//! answers `NaN` while its window holds a `NaN`.
//!
//! Every logarithm, exponential and cosine is `libm`'s, never the platform's, so a value is the same
//! number on every box (decision 0032); `sqrt` is IEEE 754's correctly rounded one. Streaming equals
//! batch by construction: `vectorize` folds the same `push` over the bars.

use super::pbars;
use crate::fast_e::scalar_indicator;
use crate::fast_j::next_log_return;
use crate::fast_l::ols_ordered;
use std::collections::VecDeque;
use std::f64::consts::PI;
use vike_math::rolling::is_constant_window;
use vike_math::{garch, hidden_markov, stationarity_tests};

// ---- GARCH(1,1) ----------------------------------------------------------------------------

/// GARCH(1,1) over log returns: the variance forecast for the next return.
#[derive(Clone)]
pub(crate) struct GarchCore {
    omega: f64,
    alpha: f64,
    beta: f64,
    /// The previous bar's close when it was usable, else `NaN` (`next_log_return` keeps it).
    previous: f64,
    /// The variance forecast for the next return, `NaN` before the first return.
    variance: f64,
}

impl GarchCore {
    pub(crate) fn new(omega: f64, alpha: f64, beta: f64) -> Self {
        Self { omega, alpha, beta, previous: f64::NAN, variance: f64::NAN }
    }

    pub(crate) fn clear(&mut self) {
        self.previous = f64::NAN;
        self.variance = f64::NAN;
    }

    /// `omega + alpha r^2 + beta sigma^2`, with `sigma^2` this return's own forecast: the last one,
    /// or for the first return the unconditional variance (`alpha + beta < 1`) or else `r^2`. The
    /// step and the start are `vike_math::garch`'s, the copy its fit (`vike_math::garch::fit`) runs too.
    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let Some(r) = next_log_return(&mut self.previous, close) else {
            return f64::NAN;
        };
        let (omega, alpha, beta) = (self.omega, self.alpha, self.beta);
        let prior = if self.variance.is_nan() {
            garch::starting_variance(omega, alpha, beta, r)
        } else {
            self.variance
        };
        self.variance = garch::next_variance(omega, alpha, beta, r, prior);
        self.variance.sqrt()
    }
}

scalar_indicator! {
    /// GARCH(1,1) conditional volatility (`garch_volatility`): the forecast standard deviation of
    /// the NEXT bar's log return, made on this bar. Parameters `omega` (the constant, default
    /// 0.000001), `alpha` (the weight of the last squared return, default 0.1) and `beta` (the
    /// weight of the last forecast, default 0.85), each the model's own and none fitted here; one
    /// output line, a per-period volatility as a fraction (never annualized:
    /// [`crate::realized_vol::annualize`] takes a periods-per-year), `NaN` on the first bar.
    ///
    /// Formula (Bollerslev 1986, in this module's doc), zero-mean returns `r = ln(close / close[1])`:
    /// `sigma^2[t+1] = omega + alpha r[t]^2 + beta sigma^2[t]`, and the value on bar `t` is
    /// `sqrt(sigma^2[t+1])`. The forecast for the first return, `sigma^2[1]`, is the unconditional
    /// variance `omega / (1 - alpha - beta)` when `alpha + beta < 1`; otherwise (an integrated or
    /// explosive model, which has none) it is `r[1]^2`, RiskMetrics' start, so `omega = 0`,
    /// `alpha = 1 - lambda`, `beta = lambda` is, to the rounding of its first step, the `rvol_ewma`
    /// row with decay `lambda`. The ranges let `alpha + beta` exceed 1; the variance then grows
    /// without bound, as the model says.
    ///
    /// A bar with no return (the input rule in this module's doc) reads `NaN` and leaves the
    /// forecast as it was.
    ///
    /// ⚠ Path dependent: the value carries every return since the series started.
    GarchVolatility, "garch_volatility", GarchCore, input = value,
    [omega = 0.000001, alpha = 0.1, beta = 0.85],
    build = GarchCore::new(omega, alpha, beta),
    lookback = 1,
    path_dependent = true
}

// ---- two-state Gaussian HMM forward filter ---------------------------------------------------

/// The forward filter of a two-state hidden Markov model whose states emit Gaussian log returns. The
/// step and the start are `vike_math::hidden_markov`'s, the copy the Baum-Welch fit (`vike_math::hidden_markov::fit`) runs
/// too.
#[derive(Clone)]
pub(crate) struct HmmCore {
    model: hidden_markov::TwoStateGaussian,
    /// The chain's stationary probability of state 1: the belief before the first return.
    start: f64,
    /// The previous bar's close when it was usable, else `NaN` (`next_log_return` keeps it).
    previous: f64,
    /// The filtered probability of state 1 given every return so far.
    probability: f64,
}

impl HmmCore {
    pub(crate) fn new(mean: [f64; 2], sigma: [f64; 2], stay: [f64; 2]) -> Self {
        let start = hidden_markov::stationary(stay);
        Self {
            model: hidden_markov::TwoStateGaussian::new(mean, sigma, stay),
            start,
            previous: f64::NAN,
            probability: start,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.previous = f64::NAN;
        self.probability = self.start;
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let Some(r) = next_log_return(&mut self.previous, close) else {
            return f64::NAN;
        };
        self.probability = self.model.step(self.probability, r).filtered;
        self.probability
    }
}

scalar_indicator! {
    /// The regime probability of a two-state Gaussian hidden Markov model (`hmm_regime`): the
    /// probability that the market is in state 1 on this bar, given every log return so far (the
    /// Hamilton filter). Parameters, each the model's own and none fitted here: `mean_0`,
    /// `sigma_0` and `stay_0` (state 0's mean and standard deviation of the log return per bar and
    /// its probability of staying from one bar to the next; defaults 0, 0.01 and 0.98) and
    /// `mean_1`, `sigma_1` and `stay_1` (state 1's; defaults 0, 0.03 and 0.95: a calm state and a
    /// turbulent one); one output line in `[0, 1]`, `NaN` on the first bar.
    ///
    /// Formula (Hamilton 1989; Rabiner 1989's forward variable, normalised on every bar; both in
    /// this module's doc). The belief starts at the chain's stationary law,
    /// `P(state 1) = (1 - stay_0) / (2 - stay_0 - stay_1)` (one half when both stays are 1). On each
    /// return `r = ln(close / close[1])` it is predicted one step,
    /// `q1 = (1 - p) (1 - stay_0) + p stay_1`, `q0 = 1 - q1`, then weighed by each state's Gaussian
    /// density `N(r; mean_i, sigma_i)` and normalised: `p = q1 f1 / (q0 f0 + q1 f1)`. The densities
    /// are taken as logarithms and scaled by the larger before `exp`, so a return far out in both
    /// tails still answers; when that leaves no weight at all (the likelier state was already ruled
    /// out by a stay of 1), the belief is the prediction.
    ///
    /// A bar with no return (the input rule in this module's doc) reads `NaN` and leaves the belief
    /// as it was. Six parameters are more than a Rhai script can pass, so the row is called from Rust.
    ///
    /// ⚠ Path dependent: the value carries every return since the series started.
    HmmRegime, "hmm_regime", HmmCore, input = value,
    [mean_0 = 0.0, sigma_0 = 0.01, stay_0 = 0.98, mean_1 = 0.0, sigma_1 = 0.03, stay_1 = 0.95],
    build = HmmCore::new([mean_0, mean_1], [sigma_0, sigma_1], [stay_0, stay_1]),
    lookback = 1,
    path_dependent = true
}

// ---- dominant cycle ------------------------------------------------------------------------

/// The period of the largest spectral power of the last `length` closes, linearly detrended.
#[derive(Clone)]
pub(crate) struct CycleCore {
    pub(crate) length: usize,
    /// The candidate periods, shortest first.
    periods: Vec<f64>,
    /// Goertzel's `2 cos(2 pi / period)` per candidate.
    coefficients: Vec<f64>,
    window: VecDeque<f64>,
    /// Scratch: the detrended window and each candidate's power.
    detrended: Vec<f64>,
    powers: Vec<f64>,
}

impl CycleCore {
    pub(crate) fn new(length: usize, min_period: usize, max_period: usize) -> Self {
        // A period of 2 is the shortest a sampled series carries; none can be longer than the window.
        let low = min_period.clamp(2, length.max(2));
        let high = max_period.clamp(low, length.max(low));
        let periods: Vec<f64> = (low..=high).map(|p| p as f64).collect();
        let coefficients = periods.iter().map(|p| 2.0 * libm::cos(2.0 * PI / p)).collect();
        Self {
            length,
            powers: vec![0.0; periods.len()],
            periods,
            coefficients,
            window: VecDeque::with_capacity(length),
            detrended: Vec::with_capacity(length),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.window.clear();
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        if self.length == 0 {
            return f64::NAN;
        }
        if self.window.len() == self.length {
            self.window.pop_front();
        }
        self.window.push_back(close);
        if self.window.len() < self.length {
            return f64::NAN;
        }
        self.dominant_period()
    }

    /// Detrend the full window by its least-squares line, take each candidate's power by Goertzel,
    /// and place the peak by the parabola through it and its neighbours.
    fn dominant_period(&mut self) -> f64 {
        let window = self.window.make_contiguous();
        // No cycle in a window that does not move, and none to measure through a missing value.
        if is_constant_window(window) || window.iter().any(|v| !v.is_finite()) {
            return f64::NAN;
        }
        // The `linearreg` rows' least-squares line, fitted to the differences from the oldest close.
        let (slope, intercept) = ols_ordered(window, 0);
        let first = window[0];
        self.detrended.clear();
        self.detrended.extend(
            window.iter().enumerate().map(|(i, &v)| (v - first) - (intercept + slope * i as f64)),
        );
        for (power, &c) in self.powers.iter_mut().zip(&self.coefficients) {
            *power = vike_math::time_series::goertzel_power(&self.detrended, c);
        }
        // The first largest power: every one before it is strictly smaller.
        let powers = &self.powers;
        let k =
            (1..powers.len()).fold(0, |best, j| if powers[j] > powers[best] { j } else { best });
        let period = self.periods[k];
        if k == 0 || k + 1 == self.powers.len() {
            return period;
        }
        let (before, peak, after) = (powers[k - 1], powers[k], powers[k + 1]);
        let curvature = before - 2.0 * peak + after;
        // Below 0 whenever the powers are numbers (`before < peak`, `after <= peak`).
        if curvature < 0.0 { period + 0.5 * (before - after) / curvature } else { period }
    }
}

scalar_indicator! {
    /// The dominant cycle (`dominant_cycle`): the period, in bars, of the strongest cycle in the last
    /// `length` closes, from their discrete Fourier transform. Parameters `length` (the window, in
    /// whole bars, default 64), `min_period` and `max_period` (the shortest and longest period
    /// searched, in whole bars, defaults 8 and 48; both are held inside `2..=length`, and a
    /// `max_period` below `min_period` searches `min_period` alone); one output line, a period in
    /// bars, `NaN` for the first `length - 1` bars.
    ///
    /// Formula. The window `x[0..n]` (oldest first) is detrended by its least-squares line, the one
    /// the `linearreg` row fits: `y[i] = (x[i] - x[0]) - (a + b i)`, with `a` and `b` fitted to the
    /// differences `x[i] - x[0]`. Each whole period `P` from `min_period` to `max_period` gets the
    /// power of its frequency `1/P`, the squared modulus of the Fourier sum
    /// `sum over i of y[i] (cos(2 pi i / P) - sqrt(-1) sin(2 pi i / P))`, by Goertzel's recurrence
    /// (Goertzel 1958, in this module's doc): `s = y + 2 cos(2 pi / P) s[1] - s[2]` over the window,
    /// then `power = s[1]^2 + s[2]^2 - 2 cos(2 pi / P) s[1] s[2]`. The largest power (the shortest
    /// period on a tie) is placed between its neighbours by the parabola through the three (Smith,
    /// "Quadratic Interpolation of Spectral Peaks"): `P + (before - after) / (2 (before - 2 peak +
    /// after))`, with `before`, `peak` and `after` the powers at `P - 1`, `P` and `P + 1`. At either
    /// end of the range the period is that end.
    ///
    /// A window of bit-equal closes has no cycle and reads `NaN`, as does a window holding a value
    /// that is not finite.
    DominantCycle, "dominant_cycle", CycleCore, input = value,
    [length = 64.0, min_period = 8.0, max_period = 48.0],
    build = CycleCore::new(pbars(length), pbars(min_period), pbars(max_period)),
    lookback(core) = core.length.saturating_sub(1),
    path_dependent = false
}

// ---- Haar wavelet detail energy ------------------------------------------------------------

/// The deepest level whose block (`2^level` returns) a window can hold: `RING_CAP`'s.
const HAAR_MAX_LEVEL: usize = 16;

/// The energy of the level-`level` Haar detail coefficients of the last `window` log returns.
#[derive(Clone)]
pub(crate) struct HaarCore {
    /// `2^level`: the returns one detail coefficient spans.
    block: usize,
    /// The returns read: the most whole blocks that fit in `length`, and at least one.
    pub(crate) window: usize,
    /// The previous bar's close when it was usable, else `NaN` (`next_log_return` keeps it).
    previous: f64,
    /// The last `window` returns, oldest first; `NaN` where a bar had none.
    returns: VecDeque<f64>,
}

impl HaarCore {
    pub(crate) fn new(length: usize, level: usize) -> Self {
        let block = 1 << level.clamp(1, HAAR_MAX_LEVEL);
        let window = (length / block).max(1) * block;
        Self { block, window, previous: f64::NAN, returns: VecDeque::with_capacity(window) }
    }

    pub(crate) fn clear(&mut self) {
        self.previous = f64::NAN;
        self.returns.clear();
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let r = next_log_return(&mut self.previous, close).unwrap_or(f64::NAN);
        if self.returns.len() == self.window {
            self.returns.pop_front();
        }
        self.returns.push_back(r);
        if self.returns.len() < self.window {
            return f64::NAN;
        }
        vike_math::time_series::haar_detail_energy(self.returns.make_contiguous(), self.block)
    }
}

// The Haar detail energy of the window is `vike_math::time_series::haar_detail_energy` (decision 0294).

scalar_indicator! {
    /// The Haar wavelet detail energy (`haar_detail_energy`): how much of the last returns' energy
    /// sits at one time scale, the variation between neighbouring blocks of `2^(level - 1)` bars.
    /// Parameters `length` (the returns read, in whole bars, default 64) and `level` (the scale,
    /// 1 to 8, default 1: bar against bar); one output line, an energy in squared log return,
    /// `NaN` for the first `window` bars.
    ///
    /// Formula (Percival and Walden, chapter 4, in this module's doc). Blocks of `2^level` log
    /// returns `r = ln(close / close[1])` tile the window back from the newest return; the window is
    /// the most whole blocks that fit in `length`, and one block when none does, so it is
    /// `window = max(1, floor(length / 2^level)) 2^level` returns. The orthonormal Haar detail
    /// coefficient of a block is `d = (sum of its older half - sum of its newer half) / 2^(level/2)`,
    /// and the value is `sum of d^2` over the window's blocks. Summed over the levels `1..=J` of a
    /// `2^J`-return window, plus the energy of its mean, `(sum r)^2 / 2^J`, it is the window's
    /// `sum r^2` (Parseval). A window of identical returns has energy 0.
    ///
    /// A window holding a bar with no return (the input rule in this module's doc) reads `NaN`.
    HaarDetailEnergy, "haar_detail_energy", HaarCore, input = value,
    [length = 64.0, level = 1.0],
    build = HaarCore::new(pbars(length), pbars(level)),
    lookback(core) = core.window,
    path_dependent = false
}

// ---- augmented Dickey-Fuller ---------------------------------------------------------------

/// The augmented Dickey-Fuller statistic of the last `period` log closes.
#[derive(Clone)]
pub(crate) struct AdfCore {
    pub(crate) period: usize,
    lags: usize,
    /// The last `period` log closes, oldest first; `NaN` where a close was not usable.
    levels: VecDeque<f64>,
}

impl AdfCore {
    pub(crate) fn new(period: usize, lags: usize) -> Self {
        Self { period, lags, levels: VecDeque::with_capacity(period) }
    }

    pub(crate) fn clear(&mut self) {
        self.levels.clear();
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        if self.period == 0 {
            return f64::NAN;
        }
        if self.levels.len() == self.period {
            self.levels.pop_front();
        }
        self.levels.push_back(vike_math::time_series::log_of_positive(close));
        if self.levels.len() < self.period {
            return f64::NAN;
        }
        stationarity_tests::augmented_dickey_fuller(
            self.levels.make_contiguous(),
            self.lags,
            stationarity_tests::Deterministic::Constant,
        )
    }
}

scalar_indicator! {
    /// The rolling augmented Dickey-Fuller statistic (`adf`): the t-statistic of the unit-root test on
    /// the last `period` log closes, with `lags` lagged differences and a constant. The null is a
    /// unit root (a random walk), so a strongly NEGATIVE value rejects it: the window keeps returning
    /// to a level. The asymptotic 5% critical value with a constant, about -2.86 (MacKinnon), is the
    /// row's band. Parameters `period` (closes in the window, default 100) and `lags` (lagged
    /// differences, default 1; at most 8, so the shortest window keeps a residual degree of
    /// freedom); one output line, `NaN` for the first `period - 1` bars.
    ///
    /// Formula (Said and Dickey 1984): with `y` the window's log closes, the ordinary least squares
    /// fit of `Δy_t = α + β y_(t-1) + γ_1 Δy_(t-1) + … + γ_p Δy_(t-p) + ε_t` over every `t` the window
    /// holds `p` earlier differences for, and the value is `β̂ / se(β̂)`. The regression is
    /// `vike_math::stationarity_tests`'s `augmented_dickey_fuller`, the one copy of it, which a strategy file or a study can also
    /// call on a window of its own (and the supremum ADF calls window by window).
    ///
    /// `NaN` for a window of identical closes, one holding a close that is not finite and above
    /// zero, or a design the least-squares fit finds rank deficient.
    Adf, "adf", AdfCore, input = value,
    [period = 100.0, lags = 1.0],
    build = AdfCore::new(pbars(period), pbars(lags)),
    lookback(core) = core.period.saturating_sub(1),
    path_dependent = false
}

#[cfg(test)]
mod tests {
    use super::{Adf, DominantCycle, GarchVolatility, HaarDetailEnergy, HmmRegime};
    use crate::Indicator;
    use vike_marketdata::Bar;

    /// A raw `with_params` can read an infinity, a `NaN`, a negative or a zero: no constructor may
    /// panic, ask the allocator for the address space, or wrap a length.
    #[test]
    fn with_params_degenerate_never_panics() {
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let x = 100.0 + f64::from(i % 7);
                Bar::ohlcv(i64::from(i) * 60_000, x, x + 1.0, x - 1.0, x, 10.0)
            })
            .collect();
        macro_rules! probe {
            ($($ty:ty),* $(,)?) => {$(
                for raw in [
                    &[0.0f64][..],
                    &[0.0; 6][..],
                    &[f64::NAN; 6][..],
                    &[-1e9; 6][..],
                    &[f64::INFINITY, f64::NEG_INFINITY, 0.0, 1.0, 0.0, 1.0][..],
                    &[f64::INFINITY; 6][..],
                ] {
                    let mut indicator = <$ty>::with_params(raw);
                    assert!(indicator.lookback() <= 1 << 16, "{} lookback at {raw:?}", indicator.name());
                    for b in &bars {
                        let _ = indicator.on_bar(b);
                    }
                    let _ = indicator.vectorize(&bars);
                }
            )*};
        }
        probe!(GarchVolatility, HmmRegime, DominantCycle, HaarDetailEnergy, Adf);
    }
}
