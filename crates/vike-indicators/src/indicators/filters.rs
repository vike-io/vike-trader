//! Filters from the signal-processing literature, each written from its published definition:
//! Ehlers' Gaussian filter (`gaussian_filter`), Ehlers' early-onset trend (`early_onset_trend`, the
//! quotient transform of a normalised roofing filter), the constant-gain g-h tracking filter
//! (`gh_filter`) and the scalar Kalman filter of a random walk (`kalman`). Cores and types live
//! here; the rows are in `registry/overlays.rs` (`gaussian_filter`, `gh_filter`, `kalman`) and
//! `registry/oscillators.rs` (`early_onset_trend`).
//!
//! Sources:
//!
//! - John Ehlers, "Gaussian and Other Low Lag Filters", MESA Software technical paper (c. 2000),
//!   <https://mesasoftware.com/papers/GaussianFilters.pdf>: `beta`, `alpha` and the one- to
//!   four-pole recurrences ("Nuts and Bolts"), and the coefficient tables at its end.
//! - John F. Ehlers, "The Quotient Transform", Technical Analysis of Stocks & Commodities,
//!   August 2014, with the article's code as the magazine's Traders' Tips reproduce it,
//!   <https://traders.com/Documentation/FEEDbk_docs/2014/08/TradersTips.html>.
//! - Eli Brookner, "Tracking and Kalman Filtering Made Easy", John Wiley & Sons, 1998
//!   (ISBN 0-471-18407-1, <https://doi.org/10.1002/0471224197>), section 1.2: the g-h prediction
//!   equations (1.2-11) and the stability triangle (1.2-37).
//! - Greg Welch and Gary Bishop, "An Introduction to the Kalman Filter", Technical Report TR 95-041,
//!   Department of Computer Science, University of North Carolina at Chapel Hill (1995, updated
//!   2006), <https://www.cs.unc.edu/~welch/media/pdf/kalman_intro.pdf>: the discrete filter's time
//!   update (Table 1-1) and measurement update (Table 1-2), and the scalar case of section 3
//!   ("Estimating a Random Constant"), `A = H = 1`, with its `P_0 = 1`.
//!
//! All four are infinite-memory recurrences: the number at bar `i` depends on where the series
//! started, so every one reports `warmup_path_dependent`. The transcendentals are `libm`'s and are
//! taken once, in the constructor; a bar is a handful of multiplies.

use super::pbars;
use crate::fast_e::scalar_indicator;
use std::f64::consts::PI;

// ---- gaussian filter ---------------------------------------------------------

/// Ehlers' `N`-pole Gaussian filter: `N` exponential averages in cascade, written as one
/// recurrence over the filter's own last `N` values.
#[derive(Clone)]
pub(crate) struct GaussianCore {
    /// `alpha^N`, the weight of the input.
    gain: f64,
    /// The weight of the value `k + 1` bars back: `(-1)^k C(N, k + 1) (1 - alpha)^(k + 1)`.
    feedback: [f64; 4],
    poles: usize,
    /// The filter's last values, newest first; 0 before they exist.
    past: [f64; 4],
}

impl GaussianCore {
    pub(crate) fn new(period: f64, poles: usize) -> Self {
        const BINOMIAL: [[f64; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [2.0, 1.0, 0.0, 0.0],
            [3.0, 3.0, 1.0, 0.0],
            [4.0, 6.0, 4.0, 1.0],
        ];
        let poles = poles.clamp(1, 4);
        let n = poles as f64;
        // The paper's corner: the gain is 1/sqrt(2) at `period`, its `1.414^(2/N)` as printed.
        let beta = (1.0 - libm::cos(2.0 * PI / period)) / (libm::pow(1.414, 2.0 / n) - 1.0);
        let alpha = -beta + (beta * beta + 2.0 * beta).sqrt();
        let r = 1.0 - alpha;
        let mut feedback = [0.0; 4];
        for (k, w) in feedback.iter_mut().enumerate().take(poles) {
            let term = BINOMIAL[poles - 1][k] * libm::pow(r, (k + 1) as f64);
            *w = if k % 2 == 0 { term } else { -term };
        }
        Self { gain: libm::pow(alpha, n), feedback, poles, past: [0.0; 4] }
    }

    pub(crate) fn clear(&mut self) {
        self.past = [0.0; 4];
    }

    #[inline]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let mut f = self.gain * x;
        for (w, p) in self.feedback.iter().zip(&self.past).take(self.poles) {
            f += w * p;
        }
        self.past = [f, self.past[0], self.past[1], self.past[2]];
        f
    }
}

scalar_indicator! {
    /// Ehlers' Gaussian filter (`gaussian_filter`): a low-lag smoother of the close, `poles`
    /// exponential averages in cascade. Parameters `period` (the cycle period, in whole bars, at
    /// which the response is down 3 dB; default 20) and `poles` (1 to 4, default 4); one output
    /// line, a value from the first bar.
    ///
    /// Formula (Ehlers, "Gaussian and Other Low Lag Filters", in this module's doc). With
    /// `N = poles`, `beta = (1 - cos(2 pi / period)) / (1.414^(2/N) - 1)` and
    /// `alpha = -beta + sqrt(beta^2 + 2 beta)`, the transfer response is
    /// `alpha^N / (1 - (1 - alpha) z^-1)^N`, which as a recurrence is
    /// `f = alpha^N x + sum over k = 1..N of (-1)^(k+1) C(N, k) (1 - alpha)^k f[k]`; for four poles
    /// `f = alpha^4 x + 4(1-alpha) f[1] - 6(1-alpha)^2 f[2] + 4(1-alpha)^3 f[3] - (1-alpha)^4 f[4]`.
    /// The filter's own earlier values are 0 before they exist, as in the paper's code, so it climbs
    /// from 0 toward the price over its first bars. The paper's tables were computed with
    /// `sqrt(2)` where its formula prints 1.414, so they differ from this filter in the fourth
    /// decimal.
    ///
    /// ⚠ Path dependent: the value carries every bar since the series started, and a `NaN` close
    /// poisons it for good.
    GaussianFilter, "gaussian_filter", GaussianCore, input = value,
    [period = 20.0, poles = 4.0],
    build = GaussianCore::new(period.round(), pbars(poles)),
    lookback = 0,
    path_dependent = true
}

// ---- early onset trend -------------------------------------------------------

/// Ehlers' early-onset trend: a two-pole high pass (100 bars), a two-pole SuperSmoother, a
/// fast-attack slow-decay peak, the ratio of the two and its quotient transform.
#[derive(Clone)]
pub(crate) struct EotCore {
    /// The high pass: `(1 - a/2)^2`, `2 (1 - a)` and `(1 - a)^2`.
    hp_gain: f64,
    hp_1: f64,
    hp_2: f64,
    /// The SuperSmoother: `c1`, `c2`, `c3`.
    ss_1: f64,
    ss_2: f64,
    ss_3: f64,
    k: f64,
    /// `crate::indicators::EarlyOnsetTrend::with_zero_history`.
    pub(crate) zero_history: bool,
    /// The last two closes, newest first, and how many have come (at most 2).
    closes: [f64; 2],
    seen: usize,
    hp: [f64; 2],
    filt: [f64; 2],
    peak: f64,
    x: f64,
}

impl EotCore {
    pub(crate) fn new(lp_period: f64, k: f64) -> Self {
        // The high pass's corner: 0.707 * 360 / 100 degrees.
        let angle = 0.707 * 2.0 * PI / 100.0;
        let a = (libm::cos(angle) + libm::sin(angle) - 1.0) / libm::cos(angle);
        let a1 = libm::exp(-1.414 * PI / lp_period);
        let b1 = 2.0 * a1 * libm::cos(1.414 * PI / lp_period);
        let (ss_2, ss_3) = (b1, -a1 * a1);
        Self {
            hp_gain: (1.0 - a / 2.0) * (1.0 - a / 2.0),
            hp_1: 2.0 * (1.0 - a),
            hp_2: (1.0 - a) * (1.0 - a),
            ss_1: 1.0 - ss_2 - ss_3,
            ss_2,
            ss_3,
            k,
            zero_history: false,
            closes: [0.0; 2],
            seen: 0,
            hp: [0.0; 2],
            filt: [0.0; 2],
            peak: 0.0,
            x: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.closes = [0.0; 2];
        self.seen = 0;
        self.hp = [0.0; 2];
        self.filt = [0.0; 2];
        self.peak = 0.0;
        self.x = 0.0;
    }

    #[inline]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let [c1, c2] = self.closes;
        self.closes = [close, c1];
        if !self.zero_history && self.seen < 2 {
            self.seen += 1;
            return f64::NAN;
        }
        let hp = self.hp_gain * (close - 2.0 * c1 + c2) + self.hp_1 * self.hp[0]
            - self.hp_2 * self.hp[1];
        let filt = self.ss_1 * (hp + self.hp[0]) / 2.0
            + self.ss_2 * self.filt[0]
            + self.ss_3 * self.filt[1];
        self.hp = [hp, self.hp[0]];
        self.filt = [filt, self.filt[0]];
        self.peak *= 0.991;
        if filt.abs() > self.peak {
            self.peak = filt.abs();
        }
        if self.peak != 0.0 {
            self.x = filt / self.peak;
        }
        (self.x + self.k) / (self.k * self.x + 1.0)
    }
}

scalar_indicator! {
    /// Ehlers' early-onset trend (`early_onset_trend`): an oscillator in `[-1, 1]` that turns with
    /// a trend's first bars. Parameters `lp_period` (the SuperSmoother's period in whole bars,
    /// default 30) and `k`
    /// (the quotient transform's constant, `-0.99..=0.99`, default 0.85: near 1 it holds the
    /// ratio up, near -1 it turns it over); one output line, `NaN` for the first 2 bars.
    ///
    /// Formula (Ehlers, "The Quotient Transform", in this module's doc). The high pass keeps
    /// cycles shorter than 100 bars: with `a = (cos w + sin w - 1) / cos w`, `w = 0.707 * 2 pi / 100`,
    /// `HP = (1 - a/2)^2 (close - 2 close[1] + close[2]) + 2(1 - a) HP[1] - (1 - a)^2 HP[2]`. The
    /// SuperSmoother: `a1 = exp(-1.414 pi / lp_period)`, `c2 = 2 a1 cos(1.414 pi / lp_period)`,
    /// `c3 = -a1^2`, `c1 = 1 - c2 - c3`, `Filt = c1 (HP + HP[1]) / 2 + c2 Filt[1] + c3 Filt[2]`.
    /// The peak decays by 0.991 a bar and takes `|Filt|` when that is larger; `X = Filt / Peak`
    /// (kept from the bar before while the peak is 0), and the output is the quotient
    /// `(X + k) / (k X + 1)`. The first value is on the third bar, the one that has two closes before
    /// it; the recurrences' own earlier values are 0 there, as in the article's code.
    ///
    /// ⚠ Path dependent: the value carries every bar since the series started, and a `NaN` close
    /// poisons it for good.
    ///
    /// Option, off by default: [`EarlyOnsetTrend::with_zero_history`].
    EarlyOnsetTrend, "early_onset_trend", EotCore, input = value,
    [lp_period = 30.0, k = 0.85],
    build = EotCore::new(lp_period.round(), k),
    lookback(core) = if core.zero_history { 0 } else { 2 },
    path_dependent = true
}

impl EarlyOnsetTrend {
    /// Option `zero_history`, `false` by default: the closes before the first read as 0, so the
    /// recurrence runs, and answers, from the first bar (its first high pass is the first close
    /// times `(1 - a/2)^2`, an impulse the oscillator then works off). Unset (the registry row's
    /// behaviour), the first two bars are `NaN` and the first value reads two real closes before
    /// it. `reset` and `vectorize` keep it.
    #[inline]
    pub fn with_zero_history(mut self, zero_history: bool) -> Self {
        self.core.zero_history = zero_history;
        self
    }
}

// ---- g-h filter ----------------------------------------------------------------

/// The constant-gain g-h tracking filter in its prediction form (Brookner's (1.2-11), unit time
/// step): position and velocity from the residual against the last prediction.
#[derive(Clone)]
pub(crate) struct GhCore {
    g: f64,
    h: f64,
    /// The last prediction, `NaN` when there is no track.
    est: f64,
    velocity: f64,
}

impl GhCore {
    pub(crate) fn new(g: f64, h: f64) -> Self {
        Self { g, h, est: f64::NAN, velocity: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        self.est = f64::NAN;
        self.velocity = 0.0;
    }

    #[inline]
    pub(crate) fn push(&mut self, y: f64) -> f64 {
        if y.is_nan() {
            self.clear();
            return f64::NAN;
        }
        // A track starts on its first number with no velocity.
        let previous = if self.est.is_nan() { y } else { self.est };
        let residual = y - previous;
        self.velocity += self.h * residual;
        self.est = previous + self.g * residual + self.velocity;
        self.est
    }
}

scalar_indicator! {
    /// The g-h filter (`gh_filter`), the constant-gain form a Kalman filter for a constant-velocity
    /// target settles into: a tracker of the close that carries a velocity, so it follows a steady
    /// trend without the lag of an average. Parameters `g` (the position gain, default 0.36) and `h`
    /// (the velocity gain, default 0.04: the critically damped pair for `theta = 0.8`,
    /// `g = 1 - theta^2`, `h = (1 - theta)^2`); one output line, a value from the first bar.
    ///
    /// Formula (Brookner, "Tracking and Kalman Filtering Made Easy", (1.2-11), in this module's doc),
    /// with a time step of one bar and `x*` the prediction made on the bar before:
    /// `v = v[1] + h (y - x*[1])` and `x* = x*[1] + v + g (y - x*[1])`. The output on a bar is its
    /// prediction for the next. The track starts on the first close with velocity 0, so the first
    /// value is that close. The ranges keep every pair inside Brookner's stability triangle
    /// (1.2-37), `g > 0`, `h > 0`, `4 - 2g - h > 0`. A `NaN` close reads `NaN` and the next number
    /// starts a new track.
    ///
    /// ⚠ Path dependent: the value carries every bar since the track started.
    GhFilter, "gh_filter", GhCore, input = value,
    [g = 0.36, h = 0.04],
    build = GhCore::new(g, h),
    lookback = 0,
    path_dependent = true
}

// ---- kalman filter -------------------------------------------------------------

/// The error variance the filter starts from: Welch and Bishop's `P_0 = 1`.
const KALMAN_INITIAL_VARIANCE: f64 = 1.0;

/// The scalar discrete Kalman filter with `A = H = 1` (a random-walk state seen in noise): the
/// estimate and its error variance.
#[derive(Clone)]
pub(crate) struct KalmanCore {
    q: f64,
    r: f64,
    /// The last estimate, `NaN` before the first number.
    est: f64,
    /// Its error variance.
    var: f64,
}

impl KalmanCore {
    pub(crate) fn new(q: f64, r: f64) -> Self {
        Self { q, r, est: f64::NAN, var: KALMAN_INITIAL_VARIANCE }
    }

    pub(crate) fn clear(&mut self) {
        self.est = f64::NAN;
        self.var = KALMAN_INITIAL_VARIANCE;
    }

    #[inline]
    pub(crate) fn push(&mut self, z: f64) -> f64 {
        // Time update: the state is carried, its variance grows by Q. The first number is its own prior.
        let prior = if self.est.is_nan() { z } else { self.est };
        let prior_var = self.var + self.q;
        if z.is_nan() {
            // No measurement: the time update alone, nothing to correct with.
            if !self.est.is_nan() {
                self.var = prior_var;
            }
            return f64::NAN;
        }
        // Measurement update.
        let gain = prior_var / (prior_var + self.r);
        self.est = prior + gain * (z - prior);
        self.var = (1.0 - gain) * prior_var;
        self.est
    }
}

scalar_indicator! {
    /// The scalar Kalman filter (`kalman`): the best linear estimate of a level that wanders as a
    /// random walk, seen through the close with noise added. Parameters `process_noise` (`Q`, the
    /// variance of the level's step per bar, default 0.001) and `measurement_noise` (`R`, the
    /// variance of the noise on the close, default 0.1); one output line, a value from the first
    /// bar. Only `Q / R` sets where the gain settles; the start reads them against `P_0 = 1`.
    ///
    /// Formula (Welch and Bishop, "An Introduction to the Kalman Filter", in this module's doc), the
    /// model `x = x[1] + w`, `z = x + v`, `w ~ N(0, Q)`, `v ~ N(0, R)`. Time update:
    /// `x- = x[1]`, `P- = P[1] + Q`. Measurement update: `K = P- / (P- + R)`, `x = x- + K (z - x-)`,
    /// `P = (1 - K) P-`. The error variance starts at the report's `P_0 = 1`; the estimate starts on
    /// the first close (the report's example starts it at 0, which a price would take hundreds of
    /// bars to forget), so the first value is that close. The gain settles at `K = P- / (P- + R)`
    /// with `P- = (Q + sqrt(Q^2 + 4 Q R)) / 2`, where the filter is an exponential average with that
    /// weight. A `NaN` close reads `NaN` and is a time update alone: the estimate is kept and its
    /// variance grows by `Q`.
    ///
    /// ⚠ Path dependent: the value carries every bar since the series started.
    Kalman, "kalman", KalmanCore, input = value,
    [process_noise = 0.001, measurement_noise = 0.1],
    build = KalmanCore::new(process_noise, measurement_noise),
    lookback = 0,
    path_dependent = true
}
