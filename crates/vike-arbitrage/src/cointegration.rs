//! Cointegration tests: whether a combination of non-stationary series is stationary, which is what makes a
//! pair's spread worth trading back to its mean.
//!
//! **Engle–Granger** ([`engle_granger`]; Engle and Granger, "Co-integration and error correction:
//! representation, estimation, and testing", Econometrica 55(2), 1987). Regress one series on the others
//! and an intercept by least squares, then test the residual for a unit root with the augmented
//! Dickey–Fuller statistic WITHOUT a deterministic term (the residual has mean zero by construction). The
//! null is "no cointegration", so a statistic BELOW the critical value rejects it; the values are
//! MacKinnon's ([`critical_values`]), NOT the Dickey–Fuller ones, because the residual was
//! fitted to look stationary. The test is NOT symmetric: which series is the dependent one changes the
//! statistic, so a pair screen tries both ([`crate::pair_discovery::cointegrated_pairs`]). It finds one
//! relation among any number of series.
//!
//! **Johansen** ([`johansen`]; Johansen, "Statistical analysis of cointegration vectors", Journal of
//! Economic Dynamics and Control 12, 1988, and "Estimation and hypothesis testing of cointegration vectors in
//! Gaussian vector autoregressive models", Econometrica 59, 1991). The vector error-correction model
//! `ΔX_t = Π X_(t-1) + Γ_1 ΔX_(t-1) + … + Γ_k ΔX_(t-k) + deterministic + ε_t` has `Π` of rank `r`, the
//! number of independent cointegrating relations. Concentrate the lagged differences and the unrestricted
//! deterministic terms out of `ΔX_t` and `X_(t-1)` (least squares; residuals `R₀` and `R₁`), form the moment
//! matrices `S_ij = R_iᵀ R_j / T`, and the eigenvalues `λ₁ ≥ λ₂ ≥ …` of `S₁₁⁻¹ S₁₀ S₀₀⁻¹ S₀₁` are the squared
//! canonical correlations between the two. The trace statistic of `r` is `−T Σ_(i>r) ln(1 − λ_i)` and the
//! maximal-eigenvalue statistic `−T ln(1 − λ_(r+1))`; the null is "rank at most `r`", rejected by a
//! statistic ABOVE the critical value. The eigenvectors give the cointegrating vectors.
//!
//! The eigenproblem is made symmetric before it is solved, so the workspace's symmetric eigen-decomposition
//! (`vike_math::linear_algebra::symmetric_eigen`) does it: with `C = S₀₀^(-1/2) S₀₁ S₁₁^(-1/2)` (the
//! inverse square roots come from that same decomposition), `CᵀC` has the same eigenvalues and its
//! eigenvectors `v` give `β = S₁₁^(-1/2) v`, normalised `βᵀ S₁₁ β = I`.
//!
//! **What the numbers are NOT.** The critical values are asymptotic (`critical_values`); both tests
//! over-reject in a short sample and when the errors are heavy-tailed, and a screen over many pairs finds
//! some by chance (`crate::pair_discovery`'s module doc). Every regression is `vike_math::linear_algebra`'s
//! and the Dickey–Fuller statistic `vike_math::stationarity_tests::augmented_dickey_fuller`'s; nothing is fitted twice.
//!
//! Every fold is a naive left fold in index order; the only transcendental is `libm::log1p`.

use vike_math::linear_algebra::{Matrix, least_squares, symmetric_eigen};
use vike_math::stationarity_tests::{self, Deterministic};

pub mod critical_values;

use critical_values::{
    CriticalValues, JohansenStatistic, Significance, engle_granger_critical_values,
    johansen_critical_values,
};

/// The most series [`johansen`] and [`engle_granger`] accept: the published tables stop there.
pub const MAXIMUM_SERIES: usize = 12;

/// The least-squares coefficients of `observations` on `design`, and the residual
/// `observations − design · coefficients`.
pub(crate) fn fitted_residuals(
    design: &Matrix,
    observations: &[f64],
) -> Option<(Vec<f64>, Vec<f64>)> {
    let fit = least_squares(design, observations)?;
    let residuals = (0..design.rows())
        .map(|t| {
            let mut fitted = 0.0;
            for (b, x) in fit.coefficients.iter().zip(design.row(t)) {
                fitted += b * x;
            }
            observations[t] - fitted
        })
        .collect();
    Some((fit.coefficients, residuals))
}

/// An Engle–Granger test, from [`engle_granger`].
#[derive(Clone, Debug, PartialEq)]
pub struct EngleGranger {
    /// The cointegrating regression's coefficients: the intercept, then one per regressor column.
    pub coefficients: Vec<f64>,
    /// The regression's residuals, one per observation: the spread of the pair when there is one regressor.
    pub residuals: Vec<f64>,
    /// The Dickey–Fuller t-statistic of the residual; BELOW a critical value rejects "no cointegration".
    pub statistic: f64,
    /// The lagged differences the Dickey–Fuller regression used.
    pub lags: usize,
    /// The observations in that regression, which the critical values are for.
    pub observations: usize,
    /// MacKinnon's critical values for this many series and observations.
    pub critical_values: CriticalValues,
}

impl EngleGranger {
    /// Whether the test rejects "no cointegration" at `level`.
    pub fn cointegrated(&self, level: Significance) -> bool {
        self.statistic < self.critical_values.at(level)
    }
}

/// The Engle–Granger test of `dependent` against the columns of `regressors` (one row per observation, at
/// most `MAXIMUM_SERIES - 1` columns), with `lags` lagged differences in the residual's Dickey–Fuller
/// regression. Pass LOG prices for prices.
///
/// `None` when the shapes disagree, a value is not finite, the regression has no unique answer (a
/// regressor that never moves, or one that repeats another), the residual's statistic does not exist
/// (see `vike_math::stationarity_tests::augmented_dickey_fuller`) or the series count is outside the critical-value table.
pub fn engle_granger(dependent: &[f64], regressors: &Matrix, lags: usize) -> Option<EngleGranger> {
    let (n, k) = (dependent.len(), regressors.columns());
    if k == 0 || regressors.rows() != n || dependent.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut design = Vec::with_capacity(n * (k + 1));
    for row in regressors.values().chunks_exact(k) {
        design.push(1.0);
        design.extend_from_slice(row);
    }
    let design = Matrix::from_row_major(n, k + 1, design)?;
    let (coefficients, residuals) = fitted_residuals(&design, dependent)?;
    let statistic =
        stationarity_tests::augmented_dickey_fuller(&residuals, lags, Deterministic::Absent);
    if statistic.is_nan() {
        return None;
    }
    let observations = n.checked_sub(1 + lags)?;
    let critical_values = engle_granger_critical_values(k + 1, observations)?;
    Some(EngleGranger { coefficients, residuals, statistic, lags, observations, critical_values })
}

/// The deterministic terms of the Johansen model, which decide the critical values (the three of Osterwald-Lenum's
/// five that a price pair uses; the two trend cases are not built). In every case the lagged differences
/// are concentrated out; the constant is the only term that differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JohansenCase {
    /// Case 0: no deterministic term. The series are centred already: the model of a stationary spread
    /// around zero, as for the log of a ratio of two prices known to be near one.
    NoConstant,
    /// Case 1*: a constant that lives INSIDE the cointegrating relation only (the series have no
    /// drift, the spread has a nonzero mean). The relation's vector has one more entry, that constant.
    RestrictedConstant,
    /// Case 1: an unrestricted constant: the series may drift, the spread has a nonzero mean. The usual
    /// choice for prices, and the one a `statsmodels` `det_order = 0` runs.
    UnrestrictedConstant,
}

/// A Johansen test, from [`johansen`]. Every vector is indexed by `r`, the rank under the null, `0` to
/// `p - 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Johansen {
    pub case: JohansenCase,
    /// The rows the regressions used: the series' length less one less the lagged differences.
    pub observations: usize,
    /// The `p` largest squared canonical correlations, in `[0, 1)`, largest first.
    pub eigenvalues: Vec<f64>,
    /// `−T Σ_(i>r) ln(1 − λ_i)`; ABOVE its critical value rejects "rank at most `r`".
    pub trace_statistics: Vec<f64>,
    /// `−T ln(1 − λ_(r+1))`; ABOVE its critical value rejects "rank `r`" for "rank `r + 1`".
    pub maximum_eigenvalue_statistics: Vec<f64>,
    /// The asymptotic critical values of each trace statistic.
    pub trace_critical_values: Vec<CriticalValues>,
    /// The asymptotic critical values of each maximal-eigenvalue statistic.
    pub maximum_eigenvalue_critical_values: Vec<CriticalValues>,
    /// One cointegrating vector per COLUMN, in the order of [`Self::eigenvalues`], `βᵀ S₁₁ β = I`. `p`
    /// rows, or `p + 1` in the restricted-constant case (the last is the constant's).
    pub cointegrating_vectors: Matrix,
}

impl Johansen {
    /// The number of cointegrating relations the sequence of tests finds at `level`: the first `r` (from
    /// zero) whose null is NOT rejected; the number of series when every one is (the series are then
    /// stationary themselves).
    pub fn rank(&self, statistic: JohansenStatistic, level: Significance) -> usize {
        let (statistics, critical) = match statistic {
            JohansenStatistic::Trace => (&self.trace_statistics, &self.trace_critical_values),
            JohansenStatistic::MaximumEigenvalue => {
                (&self.maximum_eigenvalue_statistics, &self.maximum_eigenvalue_critical_values)
            }
        };
        for (r, (value, values)) in statistics.iter().zip(critical).enumerate() {
            if *value <= values.at(level) {
                return r;
            }
        }
        statistics.len()
    }

    /// Cointegrating vector `vector` scaled so its first entry is `1`, the way a spread
    /// `X₀ − (β₁/β₀)X₁ − …` is written; `None` when `vector` is out of range or its first entry is zero.
    pub fn spread_weights(&self, vector: usize) -> Option<Vec<f64>> {
        if vector >= self.cointegrating_vectors.columns() {
            return None;
        }
        let column = self.cointegrating_vectors.column(vector);
        let first = column[0];
        (first != 0.0).then(|| column.iter().map(|w| w / first).collect())
    }
}

/// `S^(-1/2)` of a symmetric positive-definite matrix, from its eigen-decomposition `V Λ Vᵀ` as
/// `V Λ^(-1/2) Vᵀ` (`vike_math::linear_algebra::SymmetricEigen::rebuild_with`, so it is exactly
/// symmetric); `None` when an eigenvalue is not above `1e-12` of the largest (it is singular).
fn inverse_square_root(s: &Matrix) -> Option<Matrix> {
    let eigen = symmetric_eigen(s)?;
    let largest = *eigen.values.first()?;
    if largest.is_nan() || largest <= 0.0 {
        return None;
    }
    if eigen.values.iter().any(|value| value.is_nan() || *value <= largest * 1e-12) {
        return None;
    }
    Some(eigen.rebuild_with(|value| 1.0 / value.sqrt()))
}

/// `aᵀ b / rows`: the moment matrix of two residual matrices with the same rows.
fn moment(a: &Matrix, b: &Matrix) -> Option<Matrix> {
    let product = a.transpose().multiply(b)?;
    let rows = a.rows() as f64;
    Matrix::from_row_major(
        product.rows(),
        product.columns(),
        product.values().iter().map(|v| v / rows).collect(),
    )
}

/// What is left of each column of `target` after least squares on `regressors`; `target` itself when
/// there is no regressor.
fn concentrate_out(target: &Matrix, regressors: Option<&Matrix>) -> Option<Matrix> {
    let Some(regressors) = regressors else {
        return Some(target.clone());
    };
    let (rows, columns) = (target.rows(), target.columns());
    let mut values = vec![0.0; rows * columns];
    for c in 0..columns {
        let (_, residuals) = fitted_residuals(regressors, &target.column(c))?;
        for (t, residual) in residuals.into_iter().enumerate() {
            values[t * columns + c] = residual;
        }
    }
    Matrix::from_row_major(rows, columns, values)
}

/// The Johansen cointegration tests of the series in the columns of `levels` (one row per observation,
/// `1 ..= MAXIMUM_SERIES` columns; pass LOG prices for prices) with `lagged_differences` lagged
/// differences in the model (the VAR order less one) and the deterministic terms of `case`.
///
/// `None` when a value is not finite, there are too few rows for the regressions to leave a positive-definite
/// moment matrix (more than the regressors and the series), a regression or a moment matrix is singular
/// (a series that is constant, a series that repeats another, an exact linear relation among them
/// in levels AND differences), or the series count is outside the critical-value table.
pub fn johansen(
    levels: &Matrix,
    lagged_differences: usize,
    case: JohansenCase,
) -> Option<Johansen> {
    let (total, p) = (levels.rows(), levels.columns());
    if p == 0 || p > MAXIMUM_SERIES || levels.values().iter().any(|v| !v.is_finite()) {
        return None;
    }
    let first = lagged_differences + 1;
    let observations = total.checked_sub(first)?;
    let unrestricted_constant = case == JohansenCase::UnrestrictedConstant;
    let restricted_constant = case == JohansenCase::RestrictedConstant;
    let width = usize::from(unrestricted_constant) + lagged_differences * p;
    let lagged_columns = p + usize::from(restricted_constant);
    if observations <= width + lagged_columns {
        return None;
    }
    let difference = |t: usize, i: usize| levels.get(t, i) - levels.get(t - 1, i);
    let mut differences = Vec::with_capacity(observations * p);
    let mut lagged = Vec::with_capacity(observations * lagged_columns);
    let mut concentrated = Vec::with_capacity(observations * width);
    for t in first..total {
        for i in 0..p {
            differences.push(difference(t, i));
            lagged.push(levels.get(t - 1, i));
        }
        if restricted_constant {
            lagged.push(1.0);
        }
        if unrestricted_constant {
            concentrated.push(1.0);
        }
        for j in 1..=lagged_differences {
            for i in 0..p {
                concentrated.push(difference(t - j, i));
            }
        }
    }
    let differences = Matrix::from_row_major(observations, p, differences)?;
    let lagged = Matrix::from_row_major(observations, lagged_columns, lagged)?;
    let regressors = if width > 0 {
        Some(Matrix::from_row_major(observations, width, concentrated)?)
    } else {
        None
    };
    let r0 = concentrate_out(&differences, regressors.as_ref())?;
    let r1 = concentrate_out(&lagged, regressors.as_ref())?;
    let (s00, s11, s01) = (moment(&r0, &r0)?, moment(&r1, &r1)?, moment(&r0, &r1)?);
    let (root00, root11) = (inverse_square_root(&s00)?, inverse_square_root(&s11)?);
    let canonical = root00.multiply(&s01)?.multiply(&root11)?;
    let eigen = symmetric_eigen(&canonical.transpose().multiply(&canonical)?)?;

    let count = observations as f64;
    // A canonical correlation of exactly one is an exact relation: its statistic is huge, not infinite.
    let terms: Vec<f64> = eigen.values[..p]
        .iter()
        .map(|&lambda| -count * libm::log1p(-lambda.clamp(0.0, 1.0 - f64::EPSILON)))
        .collect();
    let eigenvalues: Vec<f64> = eigen.values[..p].iter().map(|&l| l.max(0.0)).collect();
    let mut trace_statistics = Vec::with_capacity(p);
    let mut trace_critical_values = Vec::with_capacity(p);
    let mut maximum_eigenvalue_critical_values = Vec::with_capacity(p);
    for r in 0..p {
        let mut total_terms = 0.0;
        for term in &terms[r..] {
            total_terms += term;
        }
        trace_statistics.push(total_terms);
        trace_critical_values.push(johansen_critical_values(
            case,
            JohansenStatistic::Trace,
            p - r,
        )?);
        maximum_eigenvalue_critical_values.push(johansen_critical_values(
            case,
            JohansenStatistic::MaximumEigenvalue,
            p - r,
        )?);
    }
    let mut leading = Vec::with_capacity(lagged_columns * p);
    for row in eigen.vectors.values().chunks_exact(lagged_columns) {
        leading.extend_from_slice(&row[..p]);
    }
    let cointegrating_vectors =
        root11.multiply(&Matrix::from_row_major(lagged_columns, p, leading)?)?;
    Some(Johansen {
        case,
        observations,
        eigenvalues,
        trace_statistics,
        maximum_eigenvalue_statistics: terms,
        trace_critical_values,
        maximum_eigenvalue_critical_values,
        cointegrating_vectors,
    })
}
