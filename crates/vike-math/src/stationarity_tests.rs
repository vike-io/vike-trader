//! The augmented Dickey–Fuller (ADF) unit-root statistic over one window of LEVELS: the one copy of
//! that regression in the workspace, for a rolling statistic and for the supremum ADF (SADF) alike.
//!
//! [`augmented_dickey_fuller`] fits, by ordinary least squares,
//!
//! `Δy_t = [α] [+ δ t] + β y_(t-1) + γ_1 Δy_(t-1) + … + γ_p Δy_(t-p) + ε_t`
//!
//! over every `t` from `p + 1` to the last index, and returns the t-statistic of `β`, `β̂ / se(β̂)`
//! (Said and Dickey, "Testing for unit roots in autoregressive-moving average models of unknown
//! order", Biometrika 71(3), 1984; the regression as in Hamilton, "Time Series Analysis", 1994,
//! section 17.7). A unit root (`β = 0`) gives values near zero or above; a strongly negative value
//! rejects it. The asymptotic 5% critical values are about `-2.86` with a constant and `-3.41` with a
//! constant and a trend (MacKinnon, "Critical values for cointegration tests", 1991/2010).
//!
//! How it is computed: the design `[[1], [t], Δy_(t-1) … Δy_(t-p), y_(t-1)]` and `Δy_t` go to
//! [`crate::linear_algebra::least_squares`], the workspace's one least-squares fit. It factors the
//! design by Householder QR, so the near-collinear "constant beside a price level" pair costs its
//! condition number once rather than squared as in the normal equations, and with `y_(t-1)` LAST its
//! standard error is `s / |R_qq|`, no inverse. Every fold there is a naive left fold in row order, so
//! a value is the same bits on every box; there is no transcendental (`sqrt` is correctly rounded by
//! IEEE 754).
//!
//! `NaN`, never a number, when the answer does not exist: a non-finite level in the window, a
//! window whose levels are all the same float, fewer than one residual degree of freedom
//! (`len - 1 - lags <= columns`), or a design the QR factor finds rank deficient.

use crate::linear_algebra::{Matrix, least_squares};

/// The deterministic terms of the test regression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deterministic {
    /// No intercept and no trend: the test regression of a series that is already centred, such as
    /// the residual of a cointegrating regression with an intercept (the Engle–Granger test, whose
    /// critical values assume this regression: MacKinnon, "Critical values for cointegration
    /// tests", 2010).
    Absent,
    /// An intercept: the alternative is a series stationary around a constant level.
    Constant,
    /// An intercept and a linear time trend: the alternative is stationary around a trend.
    ConstantAndTrend,
}

/// The ADF t-statistic of `levels` (oldest first) with `lags` lagged differences and the given
/// deterministic terms. See the module doc for the regression and for when the answer is `NaN`.
///
/// A caller testing a price usually passes LOG prices: the statistic does not move under
/// `a · y + b`, but it does under a log.
pub fn augmented_dickey_fuller(levels: &[f64], lags: usize, deterministic: Deterministic) -> f64 {
    let constant = deterministic != Deterministic::Absent;
    let trend = deterministic == Deterministic::ConstantAndTrend;
    // The regressors: [the constant], [the trend], the lags, y_(t-1) LAST.
    let columns = usize::from(constant) + usize::from(trend) + lags + 1;
    let first = lags + 1;
    let rows = levels.len().saturating_sub(first);
    // `rows - columns` residual degrees of freedom.
    if rows <= columns {
        return f64::NAN;
    }
    if levels.iter().any(|v| !v.is_finite()) || levels.iter().all(|v| *v == levels[0]) {
        return f64::NAN;
    }
    let mut design = Vec::with_capacity(rows * columns);
    let mut differences = Vec::with_capacity(rows);
    for t in first..levels.len() {
        if constant {
            design.push(1.0);
        }
        if trend {
            design.push(t as f64);
        }
        for j in 1..=lags {
            design.push(levels[t - j] - levels[t - j - 1]);
        }
        design.push(levels[t - 1]);
        differences.push(levels[t] - levels[t - 1]);
    }
    let fit = Matrix::from_row_major(rows, columns, design)
        .and_then(|design| least_squares(&design, &differences));
    let Some(fit) = fit else {
        return f64::NAN;
    };
    let standard_error = fit.standard_errors()[columns - 1];
    if standard_error.is_nan() || standard_error <= 0.0 {
        return f64::NAN;
    }
    fit.coefficients[columns - 1] / standard_error
}

/// The supremum augmented Dickey–Fuller statistic (SADF; López de Prado, *Advances in Financial Machine
/// Learning* ch. 17, snippets 17.1 to 17.4, after Phillips, Wu and Yu 2011): explosive-bubble detection.
/// Every window is fitted by [`augmented_dickey_fuller`], so this function only chooses the windows and
/// takes the supremum.
///
/// SADF at the LAST bar of `levels` (oldest first; pass log prices for a price series):
/// `sup over s of ADF(levels[s..])` with `s` running over `0..=levels.len() - 1 - min_sample_length`,
/// so the shortest window holds `min_sample_length + 1` levels. That is snippet 17.1's
/// `startPoints = range(0, y.shape[0] + lags - minSL + 1)`, whose regression rows `y[start:]` are the
/// rows of `levels[start..]`. A large positive value is evidence of an explosive (bubble) regime; a
/// unit-root series stays near or below zero.
///
/// A window the regression cannot answer (too few rows for `lags` and the deterministic terms, a
/// constant stretch, a non-finite level) is skipped. NaN when no window answers, `levels` included
/// shorter than `min_sample_length + 1`.
///
/// The SADF SERIES is this at each end bar: `(0..n).map(|t| supremum_augmented_dickey_fuller(
/// &levels[..=t], ...))`. Each call fits one regression per start, so a series costs about `n³ / 6`
/// row passes: evaluate it on the end bars a study needs rather than on every bar of a long history.
pub fn supremum_augmented_dickey_fuller(
    levels: &[f64],
    min_sample_length: usize,
    lags: usize,
    deterministic: Deterministic,
) -> f64 {
    let Some(last_start) = levels.len().checked_sub(min_sample_length + 1) else {
        return f64::NAN;
    };
    // `f64::max` keeps the number when one side is NaN, so an unanswered window is skipped.
    (0..=last_start)
        .map(|start| augmented_dickey_fuller(&levels[start..], lags, deterministic))
        .fold(f64::NAN, f64::max)
}

/// The KPSS level-stationarity statistic of `levels` with `lags` Bartlett-weighted autocovariances in
/// the long-run variance ([`crate::time_series::bartlett_long_run_variance`]); `NaN` for a constant
/// window (no variance to test) and for a `NaN` inside. `residuals` is the caller's scratch, reused.
/// Kwiatkowski, Phillips, Schmidt and Shin, "Testing the null hypothesis of stationarity against the
/// alternative of a unit root", Journal of Econometrics 54 (1992): `η = T⁻² Σ S_t² / s²(l)`, their (13).
/// A LARGE value rejects stationarity (their 5% critical value is 0.463).
#[inline]
pub fn kpss_statistic(levels: &[f64], lags: usize, residuals: &mut Vec<f64>) -> f64 {
    if crate::rolling::is_constant_window(levels) {
        return f64::NAN;
    }
    let t = levels.len() as f64;
    let mut mean = 0.0;
    for v in levels {
        mean += v;
    }
    mean /= t;
    residuals.clear();
    residuals.extend(levels.iter().map(|v| v - mean));
    let mut partial = 0.0;
    let mut partial_squares = 0.0;
    let mut squares = 0.0;
    for e in residuals.iter() {
        partial += e;
        partial_squares += crate::powers::square(partial);
        squares += crate::powers::square(*e);
    }
    let long_run =
        crate::time_series::bartlett_long_run_variance_of_squares(residuals, lags, squares);
    if long_run.is_nan() || long_run <= 0.0 {
        return f64::NAN;
    }
    partial_squares / (t * t * long_run)
}

#[cfg(test)]
#[path = "stationarity_tests_tests.rs"]
mod tests;
