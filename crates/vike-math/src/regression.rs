//! Regressions: the slope and the Ornstein–Uhlenbeck half-life read off window moments, the
//! least-squares slope of a set of points, the slope t-value of an ordinary-least-squares fit, and the
//! two-state Kalman-filter regression step.
//!
//! The moment functions take the sums (or the sums over a divisor shared by both) a streaming
//! indicator core already holds; the core keeps its window and its state, and the closing formula
//! is this one `#[inline]` call (decision 0294, verdict 4).

use crate::summation::compensated_sum;

/// The least-squares slope `covariance / variance` of a response on its regressor, from the window's
/// moments; `NaN` where the regressor is constant (`variance == 0`). One regression, many inputs: the
/// `beta` row's returns on returns and `kyle_lambda`'s price change on signed volume.
#[inline(always)]
pub fn slope_of_moments(covariance: f64, variance: f64) -> f64 {
    if variance != 0.0 { covariance / variance } else { f64::NAN }
}

/// The Ornstein–Uhlenbeck half-life `-ln 2 / lambda` of a spread, `lambda = covariance / variance`
/// the slope of its change on its lagged level; `NaN` unless the window reverts (`lambda < 0`) and the
/// lagged level varies (`variance != 0`).
#[inline(always)]
pub fn half_life_of_slope(covariance: f64, variance: f64) -> f64 {
    if variance != 0.0 {
        let lambda = covariance / variance;
        if lambda < 0.0 {
            return -std::f64::consts::LN_2 / lambda;
        }
    }
    f64::NAN
}

/// The least-squares slope of `y` on `x` over `(x, y)` points, two passes of plain folds in index
/// order; `NaN` for fewer than two points or points sharing one `x`.
pub fn least_squares_slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return f64::NAN;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in points {
        let dx = x - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    if sxx > 0.0 { sxy / sxx } else { f64::NAN }
}

/// The slope t-value of the ordinary-least-squares fit of `ys` on `xs` (one length) with an intercept,
/// `slope / sqrt(SSE / (n - 2) / Σ(x - x̄)²)`, every sum compensated; `0.0` where it is undefined (fewer
/// than three points, no spread in `xs`, or a zero residual variance: an exact line). A different
/// function from [`least_squares_slope`] (plain folds, the slope alone, `NaN` when undefined).
///
/// ⚠ Its `0.0` for "no answer" is this function's documented contract, carried verbatim from
/// `vike-analytics`' `metrics` module: `metrics::k_ratio` fits log equity with it and `labels`' trend
/// scanning fits each window. Its operation order is pinned through `k_ratio`'s platform-probe row.
pub fn slope_t_value(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean_x = crate::statistics::mean(xs.iter().copied());
    let mean_y = crate::statistics::mean(ys.iter().copied());
    let ss_xx = crate::statistics::centred_sum_of_squares(xs.iter().copied(), mean_x);
    if ss_xx == 0.0 {
        return 0.0;
    }
    let ss_xy = compensated_sum((0..n).map(|i| (xs[i] - mean_x) * (ys[i] - mean_y)));
    let slope = ss_xy / ss_xx;
    let sse = compensated_sum((0..n).map(|i| {
        let y_hat = mean_y + slope * (xs[i] - mean_x);
        crate::powers::square(ys[i] - y_hat)
    }));
    if n < 3 {
        return 0.0;
    }
    let s2 = sse / (nf - 2.0);
    if s2 <= 0.0 {
        return 0.0;
    }
    let se_slope = (s2 / ss_xx).sqrt();
    if se_slope == 0.0 {
        return 0.0;
    }
    slope / se_slope
}

/// A time-varying regression `observation = beta · regressor + alpha + noise` by a two-state Kalman
/// filter on `x = [beta, alpha]`, a random-walk state. One step is the few operations below, in this
/// order; a step whose innovation variance is not finite or is zero holds the state and answers
/// `NaN`.
#[derive(Clone, Debug, PartialEq)]
pub struct KalmanRegression {
    /// The state's random-walk variance per step, `delta / (1 - delta)` (0 when `delta == 1`).
    process_noise: f64,
    /// The observation noise variance.
    observation_noise: f64,
    beta: f64,
    alpha: f64,
    covariance_beta_beta: f64,
    covariance_beta_alpha: f64,
    covariance_alpha_beta: f64,
    covariance_alpha_alpha: f64,
}

impl KalmanRegression {
    /// A filter at `beta = alpha = 0` with no state covariance, from the forgetting `delta` and the
    /// observation noise variance.
    #[inline]
    pub fn new(delta: f64, observation_noise: f64) -> Self {
        let denominator = 1.0 - delta;
        let process_noise = if denominator != 0.0 { delta / denominator } else { 0.0 };
        KalmanRegression {
            process_noise,
            observation_noise,
            beta: 0.0,
            alpha: 0.0,
            covariance_beta_beta: 0.0,
            covariance_beta_alpha: 0.0,
            covariance_alpha_beta: 0.0,
            covariance_alpha_alpha: 0.0,
        }
    }

    /// One step on `(observation, regressor)`: the updated `beta`, or `NaN` (state held) when the
    /// innovation variance is not finite or is zero.
    #[inline(always)]
    pub fn push(&mut self, observation: f64, regressor: f64) -> f64 {
        let b = regressor;
        // R = P + Vw·I
        let (r00, r01, r10, r11) = (
            self.covariance_beta_beta + self.process_noise,
            self.covariance_beta_alpha,
            self.covariance_alpha_beta,
            self.covariance_alpha_alpha + self.process_noise,
        );
        // F = [regressor, 1]
        let yhat = b * self.beta + self.alpha;
        // R·Fᵀ, then the scalar innovation variance Q.
        let rf0 = r00 * b + r01;
        let rf1 = r10 * b + r11;
        let q = b * rf0 + rf1 + self.observation_noise;
        if !q.is_finite() || q == 0.0 {
            return f64::NAN; // no usable gain: hold state, this step is NaN
        }
        let e = observation - yhat;
        let (k0, k1) = (rf0 / q, rf1 / q);
        self.beta += k0 * e;
        self.alpha += k1 * e;
        // P = R − K·(F·R); R is symmetric so F·R == (R·Fᵀ)ᵀ == [rf0, rf1].
        self.covariance_beta_beta = r00 - k0 * rf0;
        self.covariance_beta_alpha = r01 - k0 * rf1;
        self.covariance_alpha_beta = r10 - k1 * rf0;
        self.covariance_alpha_alpha = r11 - k1 * rf1;
        self.beta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moment_closings_and_their_guards() {
        assert_eq!(slope_of_moments(3.0, 2.0), 1.5);
        assert!(slope_of_moments(3.0, 0.0).is_nan());
        assert_eq!(half_life_of_slope(-1.0, 2.0), -std::f64::consts::LN_2 / -0.5);
        assert!(half_life_of_slope(1.0, 2.0).is_nan(), "a trending window has no half-life");
        assert!(half_life_of_slope(-1.0, 0.0).is_nan());
    }

    #[test]
    fn least_squares_slope_by_hand() {
        assert_eq!(least_squares_slope(&[(0.0, 1.0), (1.0, 3.0), (2.0, 5.0)]), 2.0);
        assert!(least_squares_slope(&[(1.0, 1.0)]).is_nan());
        assert!(least_squares_slope(&[(1.0, 1.0), (1.0, 2.0)]).is_nan());
    }

    /// A noiseless exact line is learnt, and a zero innovation variance holds the state.
    #[test]
    fn the_kalman_regression_learns_a_line_and_holds_on_no_gain() {
        let mut filter = KalmanRegression::new(0.5, 1e-9);
        let mut beta = 0.0;
        for i in 0..200 {
            let x = 1.0 + (i % 7) as f64;
            beta = filter.push(2.0 * x + 0.5, x);
        }
        assert!((beta - 2.0).abs() < 1e-3, "{beta}");
        let mut frozen = KalmanRegression::new(0.0, 0.0);
        assert!(frozen.push(1.0, 0.0).is_nan());
    }
}
