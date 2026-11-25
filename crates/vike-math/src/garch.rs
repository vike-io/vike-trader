//! GARCH(1,1): the one copy of the variance recursion `sigma^2[t+1] = omega + alpha r[t]^2 + beta
//! sigma^2[t]` and of its start ([`starting_variance`], [`next_variance`]), and its fit to log returns by
//! maximum likelihood ([`fit`]) (Tim Bollerslev, "Generalized Autoregressive Conditional
//! Heteroskedasticity", Journal of Econometrics 31 (1986) 307-327,
//! <https://doi.org/10.1016/0304-4076(86)90063-1>).
//!
//! The `garch_volatility` indicator row (`vike_indicators::indicators::GarchVolatility`) runs the step
//! bar by bar and the fit runs it over a whole series, so the row runs exactly the filter the fit
//! estimated: a study fits the parameters on history here and hands them to the row, which runs the
//! same variance filter in a backtest and live. The step is only `+ - * /`, each correctly rounded by
//! IEEE 754, so a value is the same bits on every box.
//!
//! The fit's model: zero-mean log returns `r[t] = sigma[t] z[t]` with `z` standard normal. The first
//! return's variance is the unconditional `omega / (1 - alpha - beta)` (for a model with
//! `alpha + beta >= 1`, which has none, it is `r[1]^2`): the row's own start, so
//! [`GarchParameters::variances`] is, bit for bit, the square of the row's values. The Gaussian
//! log-likelihood is `-1/2 sum over t of (ln(2 pi) + ln sigma^2[t] + r[t]^2 / sigma^2[t])`.
//!
//! [`fit`] maximises it under `omega > 0`, `alpha >= 0`, `beta >= 0` and `alpha + beta < 1`
//! (covariance stationarity) by searching an unconstrained `(u, v, w)` with `omega = exp(u)`,
//! `alpha + beta = 1 / (1 + exp(-v))` and `alpha / (alpha + beta) = 1 / (1 + exp(-w))`, through
//! [`crate::minimize::nelder_mead`] (run twice, the second from the first's answer). Every logarithm
//! and exponential is `libm`'s; the logistic and its inverse are
//! [`crate::special_functions::logistic`]/[`crate::special_functions::logit`].

use crate::minimize::nelder_mead;
use crate::special_functions::{logistic, logit};

/// The variance forecast of the FIRST return: the unconditional variance
/// `omega / (1 - (alpha + beta))` when `alpha + beta < 1`; otherwise (an integrated or explosive model,
/// which has none) the first return's square, RiskMetrics' start.
#[inline]
pub fn starting_variance(omega: f64, alpha: f64, beta: f64, first_return: f64) -> f64 {
    if alpha + beta < 1.0 { omega / (1.0 - (alpha + beta)) } else { first_return * first_return }
}

/// The variance forecast of the next return, after return `r` whose own forecast was `variance`:
/// `omega + alpha r^2 + beta variance`.
#[inline]
pub fn next_variance(omega: f64, alpha: f64, beta: f64, r: f64, variance: f64) -> f64 {
    omega + alpha * (r * r) + beta * variance
}

/// Fewer returns cannot pin three parameters.
const MIN_RETURNS: usize = 10;
/// The minimiser's tolerance on the mean negative log-likelihood and on the search coordinates.
const TOLERANCE: f64 = 1e-10;
/// The minimiser's budget per run.
const MAX_EVALUATIONS: usize = 5_000;

/// The three numbers of a GARCH(1,1) model, in the order the `garch_volatility` row takes them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GarchParameters {
    pub omega: f64,
    pub alpha: f64,
    pub beta: f64,
}

impl GarchParameters {
    /// The variance forecast of every return, then the forecast after the last: `returns.len() + 1`
    /// values (none for no returns). Entry `t` is `sigma^2` of return `t`; the row's value on the bar
    /// of return `t` is the square root of entry `t + 1`.
    pub fn variances(&self, returns: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(returns.len() + 1);
        let Some(&first) = returns.first() else {
            return out;
        };
        let Self { omega, alpha, beta } = *self;
        let mut variance = starting_variance(omega, alpha, beta, first);
        out.push(variance);
        for &r in returns {
            variance = next_variance(omega, alpha, beta, r, variance);
            out.push(variance);
        }
        out
    }

    /// The Gaussian log-likelihood of `returns` under this model; `-inf` when a variance is not a
    /// positive number.
    pub fn log_likelihood(&self, returns: &[f64]) -> f64 {
        let mut total = 0.0;
        for (&r, &variance) in returns.iter().zip(&self.variances(returns)) {
            if !(variance > 0.0 && variance.is_finite()) {
                return f64::NEG_INFINITY;
            }
            total += libm::log(variance) + r * r / variance;
        }
        -0.5 * (returns.len() as f64 * libm::log(std::f64::consts::TAU) + total)
    }
}

/// What [`fit`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct GarchFit {
    pub parameters: GarchParameters,
    /// The log-likelihood at `parameters`.
    pub log_likelihood: f64,
    /// Whether the minimiser met its tolerance (`false` means its budget ran out first).
    pub converged: bool,
}

/// The search coordinates `(u, v, w)` as a model (see the module doc).
fn parameters_at(x: &[f64]) -> GarchParameters {
    let persistence = logistic(x[1]);
    let share = logistic(x[2]);
    GarchParameters {
        omega: libm::exp(x[0]),
        alpha: persistence * share,
        beta: persistence * (1.0 - share),
    }
}

/// Fit GARCH(1,1) to zero-mean log `returns` by maximum likelihood under the stationarity constraint.
/// `None` for fewer than 10 returns, a return that is not finite, or returns that are all 0. The
/// search starts at `alpha = 0.05`, `beta = 0.90` and the `omega` that makes the unconditional
/// variance the returns' mean square.
pub fn fit(returns: &[f64]) -> Option<GarchFit> {
    if returns.len() < MIN_RETURNS || returns.iter().any(|r| !r.is_finite()) {
        return None;
    }
    let n = returns.len() as f64;
    let mean_square = returns.iter().fold(0.0, |s, &r| s + r * r) / n;
    if mean_square <= 0.0 {
        return None;
    }
    let objective = |x: &[f64]| -parameters_at(x).log_likelihood(returns) / n;
    let start = [libm::log(mean_square * 0.05), logit(0.95), logit(0.05 / 0.95)];
    let first = nelder_mead(&objective, &start, 0.5, TOLERANCE, MAX_EVALUATIONS);
    let second = nelder_mead(&objective, &first.point, 0.1, TOLERANCE, MAX_EVALUATIONS);
    let parameters = parameters_at(&second.point);
    Some(GarchFit {
        parameters,
        log_likelihood: parameters.log_likelihood(returns),
        converged: second.converged,
    })
}

#[cfg(test)]
#[path = "garch_tests.rs"]
mod tests;
