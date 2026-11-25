//! Dense linear algebra over `f64`: the workspace's ONE home for it (decision 0141), so a
//! regression, a covariance matrix or an eigen-decomposition is never written twice.
//!
//! - [`Matrix`] — a dense row-major matrix with the few operations the rest need (transpose,
//!   product).
//! - [`least_squares`] — ordinary least squares by Householder QR, with the residual sum of squares
//!   and the coefficients' standard errors ([`LeastSquares`]). The augmented Dickey–Fuller
//!   regression (`crate::stationarity_tests::augmented_dickey_fuller`) is fitted by it.
//! - [`cholesky`] — `A = L Lᵀ` of a symmetric positive-definite matrix, and its solve ([`Cholesky`]).
//! - [`symmetric_eigen`] — the eigenvalues and unit eigenvectors of a symmetric matrix by the cyclic
//!   Jacobi method ([`SymmetricEigen`]): largest eigenvalue first, each vector's sign fixed;
//!   [`SymmetricEigen::rebuild_with`] — `V · diag(f(λ)) · Vᵀ`, the one rebuild from eigenpairs.
//! - [`covariance_matrix`] / [`correlation_matrix`] — of observations in rows by variables in
//!   columns; [`correlation_from_covariance`] — the one rescaling of a covariance to a correlation,
//!   which [`correlation_matrix`] goes through; [`column_means`].
//! - [`dot`] and [`squared_distance`] — of two vectors.
//!
//! **Sized for research, not for a solver library**: dense, `O(n³)`, no blocking and no threads, which
//! is enough for a few hundred dimensions (an eigen-decomposition of a 300 × 300 covariance is about
//! ten Jacobi sweeps of `5 · 10⁷` multiply-adds each).
//!
//! **Deterministic**: no transcendental at all (`sqrt` is correctly rounded by IEEE 754, so it is
//! the same bits on every box; decision 0032), no `mul_add`, and every fold in index order: the
//! statistics ([`column_means`], [`covariance_matrix`]) are compensated sums
//! (`crate::statistics::covariance`'s), every other fold a naive left fold. The same input gives the
//! same bits on every platform.
//!
//! **No answer is `None`, never a number**: a shape that does not fit, a non-finite entry, a
//! rank-deficient design, a matrix that is not positive definite. [`cholesky`] and
//! [`symmetric_eigen`] read the LOWER triangle only (LAPACK's `uplo = 'L'`), so a matrix that is
//! symmetric only up to rounding still has one answer.

use crate::statistics::{correlation_of_moments, covariance};
use crate::summation::compensated_sum;

/// The most cyclic Jacobi sweeps [`symmetric_eigen`] runs before it gives up. It converges
/// quadratically, in well under twenty sweeps for any matrix this workspace builds; reaching this
/// bound means the input was not a matrix the method can finish on, and the answer is `None`.
const MAXIMUM_JACOBI_SWEEPS: usize = 64;

/// A dense matrix of `f64`, stored row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    rows: usize,
    columns: usize,
    values: Vec<f64>,
}

impl Matrix {
    /// A `rows × columns` matrix from its entries row after row; `None` unless there are exactly
    /// `rows · columns` of them.
    pub fn from_row_major(rows: usize, columns: usize, values: Vec<f64>) -> Option<Self> {
        (rows.checked_mul(columns) == Some(values.len())).then_some(Self { rows, columns, values })
    }

    /// A matrix from one `Vec` per row; `None` when the rows differ in length. No rows is the
    /// `0 × 0` matrix.
    pub fn from_rows(rows: &[Vec<f64>]) -> Option<Self> {
        let columns = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|row| row.len() != columns) {
            return None;
        }
        Some(Self { rows: rows.len(), columns, values: rows.concat() })
    }

    fn identity(size: usize) -> Self {
        let mut values = vec![0.0; size * size];
        for i in 0..size {
            values[i * size + i] = 1.0;
        }
        Self { rows: size, columns: size, values }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    /// The entry at `row`, `column`.
    ///
    /// # Panics
    ///
    /// When either index is out of range.
    pub fn get(&self, row: usize, column: usize) -> f64 {
        assert!(
            row < self.rows && column < self.columns,
            "({row}, {column}) is outside the matrix"
        );
        self.values[row * self.columns + column]
    }

    /// Row `row`'s entries.
    ///
    /// # Panics
    ///
    /// When `row` is out of range.
    pub fn row(&self, row: usize) -> &[f64] {
        &self.values[row * self.columns..(row + 1) * self.columns]
    }

    /// Column `column`'s entries, copied.
    ///
    /// # Panics
    ///
    /// When `column` is out of range.
    pub fn column(&self, column: usize) -> Vec<f64> {
        assert!(column < self.columns, "column {column} is outside the matrix");
        self.values.iter().skip(column).step_by(self.columns).copied().collect()
    }

    /// Every entry, row after row.
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    pub fn transpose(&self) -> Matrix {
        let mut values = Vec::with_capacity(self.values.len());
        for j in 0..self.columns {
            for i in 0..self.rows {
                values.push(self.values[i * self.columns + j]);
            }
        }
        Matrix { rows: self.columns, columns: self.rows, values }
    }

    /// The product `self · other`; `None` unless `self.columns() == other.rows()`.
    pub fn multiply(&self, other: &Matrix) -> Option<Matrix> {
        if self.columns != other.rows {
            return None;
        }
        let mut values = Vec::with_capacity(self.rows * other.columns);
        for i in 0..self.rows {
            for j in 0..other.columns {
                let mut total = 0.0;
                for k in 0..self.columns {
                    total +=
                        self.values[i * self.columns + k] * other.values[k * other.columns + j];
                }
                values.push(total);
            }
        }
        Some(Matrix { rows: self.rows, columns: other.columns, values })
    }
}

/// An ordinary-least-squares fit, from [`least_squares`].
#[derive(Clone, Debug, PartialEq)]
pub struct LeastSquares {
    /// One per design column, in the design's column order.
    pub coefficients: Vec<f64>,
    /// `Σ (y − X b)²`.
    pub residual_sum_of_squares: f64,
    /// Rows minus columns: what the residual variance is divided by.
    pub degrees_of_freedom: usize,
    /// The `R` of `X = Q R`, columns × columns, upper triangular: `XᵀX = RᵀR`.
    triangle: Matrix,
}

impl LeastSquares {
    /// `s² = RSS / (rows − columns)`, the unbiased estimate of the error variance; `NaN` when the
    /// fit has no degree of freedom left (a square design).
    pub fn residual_variance(&self) -> f64 {
        if self.degrees_of_freedom == 0 {
            return f64::NAN;
        }
        self.residual_sum_of_squares / self.degrees_of_freedom as f64
    }

    /// Each coefficient's standard error, `sqrt(s² · ((XᵀX)⁻¹)_jj)`, through
    /// `(XᵀX)⁻¹ = R⁻¹ R⁻ᵀ`, so the normal equations are never formed. `NaN` where
    /// [`Self::residual_variance`] is. The LAST coefficient's is `s / |R_qq|`: a caller that needs
    /// one standard error orders that regressor last.
    pub fn standard_errors(&self) -> Vec<f64> {
        let n = self.triangle.columns;
        let r = &self.triangle.values;
        // `R⁻¹`, upper triangular, one column at a time by back substitution.
        let mut inverse = vec![0.0; n * n];
        for c in 0..n {
            inverse[c * n + c] = 1.0 / r[c * n + c];
            for i in (0..c).rev() {
                let mut v = 0.0;
                for k in i + 1..=c {
                    v -= r[i * n + k] * inverse[k * n + c];
                }
                inverse[i * n + c] = v / r[i * n + i];
            }
        }
        let variance = self.residual_variance();
        (0..n)
            .map(|j| {
                let mut squares = 0.0;
                for c in j..n {
                    squares += inverse[j * n + c] * inverse[j * n + c];
                }
                (variance * squares).sqrt()
            })
            .collect()
    }
}

/// The ordinary-least-squares fit of `observations` on the columns of `design` (one row per
/// observation; put a column of ones in the design for an intercept), by Householder QR: the design
/// is never squared into the normal equations, so a near-collinear pair (a constant beside a price
/// level) costs its condition number once, not twice.
///
/// `None` when there is no unique answer: `observations.len()` is not `design.rows()`, the design
/// has no column or fewer rows than columns, an entry is not finite, or the design is rank deficient
/// (a column whose part outside the span of the columns before it is at most
/// `rows · ε` of its own length). A square design is solved exactly and keeps no degree of freedom.
pub fn least_squares(design: &Matrix, observations: &[f64]) -> Option<LeastSquares> {
    let (m, n) = (design.rows, design.columns);
    if n == 0 || m < n || observations.len() != m {
        return None;
    }
    if design.values.iter().chain(observations).any(|v| !v.is_finite()) {
        return None;
    }
    let mut a = design.values.clone();
    let mut b = observations.to_vec();
    // Each column's own length: the yardstick of its rank test.
    let lengths: Vec<f64> = (0..n)
        .map(|j| {
            let mut squares = 0.0;
            for i in 0..m {
                squares += a[i * n + j] * a[i * n + j];
            }
            squares.sqrt()
        })
        .collect();
    let tolerance = m as f64 * f64::EPSILON;
    for k in 0..n {
        let mut squares = 0.0;
        for i in k..m {
            squares += a[i * n + k] * a[i * n + k];
        }
        let length = squares.sqrt();
        if length <= tolerance * lengths[k] {
            return None;
        }
        // The reflection `H = I − 2 v vᵀ / vᵀv` with `v = x − α e₁` maps the column `x` to `α e₁`.
        // `α` takes the sign opposite to `x`'s head so `v`'s head adds magnitudes, and
        // `vᵀv = −2 α v_head` turns `H y` into `y + v (vᵀy) / (α v_head)`. The tail of `v` is the
        // column's own tail, read in place below the diagonal.
        let head = a[k * n + k];
        let alpha = if head > 0.0 { -length } else { length };
        let v_head = head - alpha;
        let denominator = alpha * v_head;
        a[k * n + k] = alpha;
        for j in k + 1..n {
            let mut dot = v_head * a[k * n + j];
            for i in k + 1..m {
                dot += a[i * n + k] * a[i * n + j];
            }
            let factor = dot / denominator;
            a[k * n + j] += factor * v_head;
            for i in k + 1..m {
                a[i * n + j] += factor * a[i * n + k];
            }
        }
        let mut dot = v_head * b[k];
        for i in k + 1..m {
            dot += a[i * n + k] * b[i];
        }
        let factor = dot / denominator;
        b[k] += factor * v_head;
        for i in k + 1..m {
            b[i] += factor * a[i * n + k];
        }
    }
    // `R b = (Qᵀy)[..n]` by back substitution; the rest of `Qᵀy` is the rotated residual.
    let mut triangle = vec![0.0; n * n];
    for i in 0..n {
        triangle[i * n + i..(i + 1) * n].copy_from_slice(&a[i * n + i..(i + 1) * n]);
    }
    let mut coefficients = b[..n].to_vec();
    for i in (0..n).rev() {
        let mut v = coefficients[i];
        for k in i + 1..n {
            v -= triangle[i * n + k] * coefficients[k];
        }
        coefficients[i] = v / triangle[i * n + i];
    }
    let mut residual_sum_of_squares = 0.0;
    for value in &b[n..] {
        residual_sum_of_squares += value * value;
    }
    Some(LeastSquares {
        coefficients,
        residual_sum_of_squares,
        degrees_of_freedom: m - n,
        triangle: Matrix { rows: n, columns: n, values: triangle },
    })
}

/// The Cholesky factor of a symmetric positive-definite matrix, from [`cholesky`].
#[derive(Clone, Debug, PartialEq)]
pub struct Cholesky {
    lower: Matrix,
}

impl Cholesky {
    /// `L` of `A = L Lᵀ`: lower triangular, with a positive diagonal.
    pub fn lower(&self) -> &Matrix {
        &self.lower
    }

    /// The `x` of `A x = b`, by forward substitution through `L` and back substitution through
    /// `Lᵀ`; `None` when `b` is not `A`'s size.
    pub fn solve(&self, b: &[f64]) -> Option<Vec<f64>> {
        let n = self.lower.rows;
        if b.len() != n {
            return None;
        }
        let l = &self.lower.values;
        let mut x = b.to_vec();
        for i in 0..n {
            let mut v = x[i];
            for k in 0..i {
                v -= l[i * n + k] * x[k];
            }
            x[i] = v / l[i * n + i];
        }
        for i in (0..n).rev() {
            let mut v = x[i];
            for k in i + 1..n {
                v -= l[k * n + i] * x[k];
            }
            x[i] = v / l[i * n + i];
        }
        Some(x)
    }
}

/// `A = L Lᵀ` (Cholesky–Banachiewicz, row by row), reading `a`'s lower triangle only.
///
/// `None` when `a` is not square, its lower triangle holds a non-finite entry, or it is not
/// positive definite (a pivot at or below zero).
pub fn cholesky(a: &Matrix) -> Option<Cholesky> {
    let n = a.rows;
    if a.columns != n {
        return None;
    }
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut v = a.values[i * n + j];
            if !v.is_finite() {
                return None;
            }
            for k in 0..j {
                v -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if v.is_nan() || v <= 0.0 {
                    return None;
                }
                l[i * n + i] = v.sqrt();
            } else {
                l[i * n + j] = v / l[j * n + j];
            }
        }
    }
    Some(Cholesky { lower: Matrix { rows: n, columns: n, values: l } })
}

/// The eigen-decomposition `A = V Λ Vᵀ` of a symmetric matrix, from [`symmetric_eigen`].
#[derive(Clone, Debug, PartialEq)]
pub struct SymmetricEigen {
    /// The eigenvalues, largest first; equal ones keep the order the Jacobi sweeps left them in.
    pub values: Vec<f64>,
    /// One unit eigenvector per COLUMN, in the order of [`Self::values`]. Each column's sign is
    /// fixed so its largest-magnitude entry is positive (the first such entry on an exact tie).
    pub vectors: Matrix,
}

impl SymmetricEigen {
    /// `V · diag(f(λ)) · Vᵀ = Σₖ f(λₖ) vₖ vₖᵀ`: the matrix rebuilt from these eigenpairs with each
    /// eigenvalue mapped through `f` (`|λ| λ` the matrix itself, `|λ| 1.0 / λ.sqrt()` its inverse
    /// square root). Component after component in [`Self::values`]' order, each lower-triangle entry
    /// adds `(f(λₖ) · vᵢ) · vⱼ`; a component with `f(λₖ) = 0` adds nothing. The lower triangle is
    /// mirrored, so the result is exactly symmetric. A refusal (a singular `λ`) is the caller's, before
    /// the call.
    pub fn rebuild_with(&self, f: impl Fn(f64) -> f64) -> Matrix {
        let n = self.vectors.rows;
        let mut out = vec![0.0; n * n];
        for (k, &value) in self.values.iter().enumerate() {
            let weight = f(value);
            if weight == 0.0 {
                continue;
            }
            let v = self.vectors.column(k);
            for i in 0..n {
                for j in 0..=i {
                    out[i * n + j] += weight * v[i] * v[j];
                }
            }
        }
        for i in 0..n {
            for j in 0..i {
                out[j * n + i] = out[i * n + j];
            }
        }
        Matrix { rows: n, columns: n, values: out }
    }
}

/// The eigenvalues and orthonormal eigenvectors of the symmetric matrix whose lower triangle is
/// `a`'s, by the cyclic Jacobi method: sweep every off-diagonal pair `(p, q)` in row order and
/// rotate it to zero (the rotation of Numerical Recipes' `jacobi`, 3rd ed. section 11.1), until a
/// whole sweep finds every off-diagonal entry negligible beside its two diagonal entries,
/// `|a_pq| <= ε · sqrt(|a_pp · a_qq|)` (Demmel and Veselić's relative test, which also gives the
/// small eigenvalues to high relative accuracy).
///
/// `None` when `a` is not square, its lower triangle holds a non-finite entry, or
/// `MAXIMUM_JACOBI_SWEEPS` sweeps did not finish.
pub fn symmetric_eigen(a: &Matrix) -> Option<SymmetricEigen> {
    let n = a.rows;
    if a.columns != n {
        return None;
    }
    let mut s = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let v = a.values[i * n + j];
            if !v.is_finite() {
                return None;
            }
            s[i * n + j] = v;
            s[j * n + i] = v;
        }
    }
    let mut vectors = Matrix::identity(n).values;
    let mut finished = false;
    for _ in 0..MAXIMUM_JACOBI_SWEEPS {
        let mut rotated = false;
        for p in 0..n {
            for q in p + 1..n {
                let apq = s[p * n + q];
                let (app, aqq) = (s[p * n + p], s[q * n + q]);
                let magnitude = apq.abs();
                if magnitude < f64::MIN_POSITIVE
                    || magnitude <= f64::EPSILON * (app * aqq).abs().sqrt()
                {
                    continue;
                }
                rotated = true;
                // `t = tan φ`, the smaller root of `t² + 2θt − 1 = 0`; a huge `θ` overflows
                // `θ²` to infinity and gives `t = 0`, which only zeroes a negligible `a_pq`.
                let theta = (aqq - app) / (2.0 * apq);
                let t = 1.0 / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta < 0.0 { -t } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let sine = t * c;
                s[p * n + p] = app - t * apq;
                s[q * n + q] = aqq + t * apq;
                s[p * n + q] = 0.0;
                s[q * n + p] = 0.0;
                for r in 0..n {
                    if r == p || r == q {
                        continue;
                    }
                    let (arp, arq) = (s[r * n + p], s[r * n + q]);
                    let new_p = c * arp - sine * arq;
                    let new_q = sine * arp + c * arq;
                    s[r * n + p] = new_p;
                    s[p * n + r] = new_p;
                    s[r * n + q] = new_q;
                    s[q * n + r] = new_q;
                }
                for r in 0..n {
                    let (vrp, vrq) = (vectors[r * n + p], vectors[r * n + q]);
                    vectors[r * n + p] = c * vrp - sine * vrq;
                    vectors[r * n + q] = sine * vrp + c * vrq;
                }
            }
        }
        if !rotated {
            finished = true;
            break;
        }
    }
    if !finished {
        return None;
    }
    // Largest first; `sort_by` is stable, so equal eigenvalues keep their sweep order.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| s[j * n + j].total_cmp(&s[i * n + i]));
    let values = order.iter().map(|&i| s[i * n + i]).collect();
    let mut sorted = vec![0.0; n * n];
    for (column, &source) in order.iter().enumerate() {
        let mut pivot = 0;
        for r in 1..n {
            if vectors[r * n + source].abs() > vectors[pivot * n + source].abs() {
                pivot = r;
            }
        }
        let sign = if vectors[pivot * n + source] < 0.0 { -1.0 } else { 1.0 };
        for r in 0..n {
            sorted[r * n + column] = sign * vectors[r * n + source];
        }
    }
    Some(SymmetricEigen { values, vectors: Matrix { rows: n, columns: n, values: sorted } })
}

/// Each column's mean, a compensated sum ([`compensated_sum`]) in row order over the row count; `NaN`
/// for every column of a matrix with no row.
pub fn column_means(observations: &Matrix) -> Vec<f64> {
    let (rows, columns) = (observations.rows as f64, observations.columns);
    (0..columns)
        .map(|k| {
            compensated_sum(observations.values.iter().skip(k).step_by(columns).copied()) / rows
        })
        .collect()
}

/// `Σ aᵢ bᵢ` over the common prefix, a naive left fold from `0.0` in index order.
pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |total, (x, y)| total + x * y)
}

/// The squared Euclidean distance `Σ (aᵢ − bᵢ)²` over the common prefix, a naive left fold from `0.0`
/// in index order.
pub fn squared_distance(a: &[f64], b: &[f64]) -> f64 {
    let mut total = 0.0;
    for (x, y) in a.iter().zip(b) {
        total += (x - y) * (x - y);
    }
    total
}

/// The sample covariance matrix of `observations` (one row per observation, one column per
/// variable), divisor `rows − 1`: entry `(i, j)` is `crate::statistics::covariance` of columns `i`
/// and `j`, every sum compensated (its column means are [`column_means`]' bits). The lower triangle
/// is computed and mirrored, so the matrix is exactly symmetric.
///
/// `None` with fewer than two rows, no column, or a non-finite entry.
pub fn covariance_matrix(observations: &Matrix) -> Option<Matrix> {
    let (m, n) = (observations.rows, observations.columns);
    if m < 2 || n == 0 || observations.values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let columns: Vec<Vec<f64>> = (0..n).map(|j| observations.column(j)).collect();
    let mut values = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let v = covariance(&columns[i], &columns[j], 1.0);
            values[i * n + j] = v;
            values[j * n + i] = v;
        }
    }
    Some(Matrix { rows: n, columns: n, values })
}

/// The Pearson correlation matrix of `observations` (one row per observation, one column per
/// variable): [`correlation_from_covariance`] of [`covariance_matrix`].
///
/// `None` where [`covariance_matrix`] is, and when a variable does not vary (its correlations do
/// not exist).
pub fn correlation_matrix(observations: &Matrix) -> Option<Matrix> {
    correlation_from_covariance(&covariance_matrix(observations)?)
}

/// A covariance matrix rescaled to a correlation matrix: each entry through the one Pearson closing
/// formula, `crate::statistics::correlation_of_moments(c_ij, c_ii, c_jj) = c_ij / √(c_ii · c_jj)` (a
/// product under the root, so a symmetric input gives an exactly symmetric matrix), the diagonal
/// exactly `1` and every entry clipped to `[-1, 1]` against rounding (López de Prado's `cov2corr`).
/// The product of two variances overflows only near `1e154` each, which no price or return series has.
///
/// `None` when `covariance` is not square, holds a non-finite entry, or has a diagonal entry that
/// is not positive (a variable that does not vary has no correlation).
pub fn correlation_from_covariance(covariance: &Matrix) -> Option<Matrix> {
    let n = covariance.columns;
    if covariance.rows != n || covariance.values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut matrix = covariance.clone();
    let variances: Vec<f64> = (0..n).map(|i| matrix.values[i * n + i]).collect();
    if variances.iter().any(|v| *v <= 0.0) {
        return None;
    }
    for i in 0..n {
        for j in 0..n {
            let v = &mut matrix.values[i * n + j];
            *v = if i == j {
                1.0
            } else {
                correlation_of_moments(*v, variances[i], variances[j]).clamp(-1.0, 1.0)
            };
        }
    }
    Some(matrix)
}

#[cfg(test)]
#[path = "linear_algebra_tests.rs"]
mod tests;
