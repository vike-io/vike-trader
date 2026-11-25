//! Gatheral's raw SVI ("stochastic volatility inspired") smile: one expiry's total implied
//! variance as a function of forward log-moneyness, its least-squares fit, and the two static
//! arbitrage checks on fitted slices. NEW code, no Python twin, no parity fixture: tested on
//! synthetic smiles with known parameters and hand-worked densities (`svi/tests.rs`).
//!
//! Sources: J. Gatheral, "A parsimonious arbitrage-free implied volatility parameterization with
//! application to the valuation of volatility derivatives", Global Derivatives (2004); J. Gatheral
//! and A. Jacquier, "Arbitrage-free SVI volatility surfaces", Quantitative Finance 14 (2014)
//! 59-71, <https://doi.org/10.1080/14697688.2013.819986> (the density `g` and the calendar
//! condition below are that paper's).
//!
//! - **The slice.** With `k = ln(K / F)` (`crate::log_moneyness` at the forward) and `w = σ_iv²·t`
//!   the total implied variance, raw SVI is
//!   `w(k) = a + b·(ρ·(k − m) + √((k − m)² + σ²))` ([`SviParameters::total_variance`]). It is
//!   ADMISSIBLE ([`SviParameters::is_admissible`]) when `b ≥ 0`, `|ρ| < 1`, `σ > 0` and the
//!   minimum `a + b·σ·√(1 − ρ²)` is not negative (a total variance below zero is no smile).
//! - **The fit** ([`fit_svi`]) minimises the sum of squared errors in total variance over the five
//!   parameters with the workspace's ONE minimiser, `vike_math::minimize::nelder_mead` (no
//!   second optimiser here); an inadmissible point scores `+∞`, so the simplex stays inside. The
//!   problem is first made scale-free (total variance over its mean, log-moneyness centred and
//!   over its half-range, an exact reparameterisation since SVI is linear in `a`, `b` and shifts
//!   and scales with `m`, `σ`), and the simplex is restarted from its best point until a restart
//!   no longer improves it.
//! - **Butterfly arbitrage** (a negative risk-neutral density inside one slice):
//!   `g(k) = (1 − k·w′/(2w))² − (w′²/4)·(1/w + 1/4) + w″/2` must not be negative
//!   ([`SviParameters::butterfly_density`], [`butterfly_arbitrage`]).
//! - **Calendar arbitrage** (a later expiry cheaper than an earlier one): total variance must not
//!   fall with maturity at any `k` ([`calendar_arbitrage`]).
//!
//! Both checks are read on a caller's grid of `k`: a violation between grid points is not seen,
//! so the grid is the caller's resolution. Only `sqrt` is used, which IEEE 754 rounds correctly
//! (the same on every box); no transcendental.

use vike_math::minimize::nelder_mead;

/// The five raw-SVI parameters of one slice (see the module doc's formula).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviParameters {
    /// The level: total variance's vertical shift.
    pub a: f64,
    /// The slope of the wings (`b·(1 ± ρ)` are the two asymptotic slopes).
    pub b: f64,
    /// The skew, in `(−1, 1)`: negative tilts the smile up on the low strikes.
    pub rho: f64,
    /// The horizontal shift: where the smile's vertex sits in log-moneyness.
    pub m: f64,
    /// The vertex's curvature: the smile's smoothness around `m`, positive.
    pub sigma: f64,
}

impl SviParameters {
    /// Total implied variance `w(k)` at forward log-moneyness `k`.
    pub fn total_variance(&self, k: f64) -> f64 {
        let x = k - self.m;
        self.a + self.b * (self.rho * x + (x * x + self.sigma * self.sigma).sqrt())
    }

    /// The implied volatility at `k` of a slice `years` long, `√(w(k) / t)`; `None` when `years`
    /// is not positive or the total variance is negative.
    pub fn implied_vol(&self, k: f64, years: f64) -> Option<f64> {
        let w = self.total_variance(k);
        (years > 0.0 && w >= 0.0).then_some((w / years).sqrt())
    }

    /// Whether the parameters describe a smile (see the module doc); `false` on any NaN.
    pub fn is_admissible(&self) -> bool {
        self.b >= 0.0
            && self.rho.abs() < 1.0
            && self.sigma > 0.0
            && self.a + self.b * self.sigma * (1.0 - self.rho * self.rho).sqrt() >= 0.0
    }

    /// Gatheral and Jacquier's `g(k)`, the risk-neutral density's sign at `k` (up to a positive
    /// factor): negative is butterfly arbitrage.
    pub fn butterfly_density(&self, k: f64) -> f64 {
        let x = k - self.m;
        let root = (x * x + self.sigma * self.sigma).sqrt();
        let w = self.total_variance(k);
        let slope = self.b * (self.rho + x / root);
        let curvature = self.b * self.sigma * self.sigma / (root * root * root);
        let first = 1.0 - k * slope / (2.0 * w);
        first * first - slope * slope / 4.0 * (1.0 / w + 0.25) + curvature / 2.0
    }
}

/// What [`fit_svi`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct SviFit {
    /// The fitted slice, always admissible.
    pub parameters: SviParameters,
    /// The root of the mean squared error in total variance at the fit.
    pub rmse: f64,
    /// Whether the last simplex met the tolerance (`false` means it ran out of evaluations).
    pub converged: bool,
    /// Objective evaluations over every restart.
    pub evaluations: usize,
}

/// The simplex's first step along each scale-free axis.
const STEP: f64 = 0.1;
/// The simplex's stopping tolerance, on the scale-free objective and coordinates.
const TOLERANCE: f64 = 1e-12;
/// The evaluation budget of one simplex run.
const MAX_EVALUATIONS: usize = 50_000;
/// The most restarts after the first run.
const RESTARTS: usize = 8;

/// Fits raw SVI to one expiry's smile, `points` of `(k, w)`: forward log-moneyness and total
/// implied variance (`iv² · years`). `None` with fewer than five points (five parameters), a
/// non-finite or negative value, or fewer than two distinct `k`.
pub fn fit_svi(points: &[(f64, f64)]) -> Option<SviFit> {
    if points.len() < 5 || points.iter().any(|&(k, w)| !k.is_finite() || !w.is_finite() || w < 0.0)
    {
        return None;
    }
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (low, high) = (sorted[0].0, sorted[sorted.len() - 1].0);
    let mut variance_sum = 0.0;
    for &(_, w) in &sorted {
        variance_sum += w;
    }
    let level = variance_sum / sorted.len() as f64;
    if high <= low || level <= 0.0 {
        return None;
    }
    // The scale-free problem: k' = (k − centre) / half, w' = w / level.
    let (centre, half) = (0.5 * (low + high), 0.5 * (high - low));
    let scaled: Vec<(f64, f64)> =
        sorted.iter().map(|&(k, w)| ((k - centre) / half, w / level)).collect();
    let objective = |p: &[f64]| {
        let slice = SviParameters { a: p[0], b: p[1], rho: p[2], m: p[3], sigma: p[4] };
        if !slice.is_admissible() {
            return f64::INFINITY;
        }
        let mut sum = 0.0;
        for &(k, w) in &scaled {
            let error = slice.total_variance(k) - w;
            sum += error * error;
        }
        sum
    };
    let start = starting_point(&scaled);
    let mut best = nelder_mead(objective, &start, STEP, TOLERANCE, MAX_EVALUATIONS);
    let mut evaluations = best.evaluations;
    for _ in 0..RESTARTS {
        let again = nelder_mead(objective, &best.point, STEP, TOLERANCE, MAX_EVALUATIONS);
        evaluations += again.evaluations;
        if again.value >= best.value {
            best.converged = again.converged;
            break;
        }
        best = again;
    }
    if !best.value.is_finite() {
        return None;
    }
    let p = &best.point;
    let parameters = SviParameters {
        a: level * p[0],
        b: level * p[1] / half,
        rho: p[2],
        m: centre + half * p[3],
        sigma: half * p[4],
    };
    let rmse = level * (best.value / scaled.len() as f64).sqrt();
    Some(SviFit { parameters, rmse, converged: best.converged, evaluations })
}

/// A start for the scale-free fit from the smile itself (`points` sorted by `k`): the vertex at the
/// lowest variance, the wings from the chords to each end, a vertex width of a quarter of the
/// half-range, and the level that puts the minimum on the lowest variance.
fn starting_point(points: &[(f64, f64)]) -> [f64; 5] {
    let vertex = points.iter().min_by(|a, b| a.1.total_cmp(&b.1)).copied().unwrap_or((0.0, 1.0));
    let chord = |end: (f64, f64)| {
        if end.0 == vertex.0 { 0.0 } else { (end.1 - vertex.1) / (end.0 - vertex.0) }
    };
    let (left, right) = (chord(points[0]), chord(points[points.len() - 1]));
    let b = (0.5 * (right - left)).max(0.1);
    let rho = ((right + left) / (right - left)).clamp(-0.9, 0.9);
    let rho = if rho.is_finite() { rho } else { 0.0 };
    let sigma = 0.25;
    let a = vertex.1 - b * sigma * (1.0 - rho * rho).sqrt();
    [a, b, rho, vertex.0, sigma]
}

/// The grid points of `grid` (log-moneyness) where `parameters`' density is negative: empty is a
/// slice free of butterfly arbitrage on that grid.
pub fn butterfly_arbitrage(parameters: &SviParameters, grid: &[f64]) -> Vec<f64> {
    grid.iter().copied().filter(|&k| parameters.butterfly_density(k) < 0.0).collect()
}

/// One expiry's fitted slice and its maturity, as [`calendar_arbitrage`] reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviSlice {
    /// Years to expiry.
    pub years: f64,
    /// The slice's parameters, in forward log-moneyness.
    pub parameters: SviParameters,
}

/// Where a later slice's total variance falls below an earlier one's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalendarArbitrage {
    /// The earlier slice's index in the caller's list.
    pub earlier: usize,
    /// The later slice's index in the caller's list.
    pub later: usize,
    /// The grid point where `w_later(k) < w_earlier(k)`.
    pub k: f64,
}

/// Every grid point where total variance falls from one maturity to the next: `slices` in any
/// order are compared in maturity order, each with the next longer one (slices of one maturity are
/// not compared). Empty is a surface free of calendar arbitrage on that grid.
pub fn calendar_arbitrage(slices: &[SviSlice], grid: &[f64]) -> Vec<CalendarArbitrage> {
    let mut order: Vec<usize> = (0..slices.len()).collect();
    order.sort_by(|&i, &j| slices[i].years.total_cmp(&slices[j].years));
    let mut found = Vec::new();
    for pair in order.windows(2) {
        let (earlier, later) = (pair[0], pair[1]);
        if slices[later].years <= slices[earlier].years {
            continue;
        }
        for &k in grid {
            let (before, after) = (
                slices[earlier].parameters.total_variance(k),
                slices[later].parameters.total_variance(k),
            );
            if after < before {
                found.push(CalendarArbitrage { earlier, later, k });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests;
