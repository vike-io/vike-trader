use super::*;

/// A fixed uniform sequence in `[-0.5, 0.5)` (no `rand` here: a 64-bit LCG).
fn noise(seed: u64, n: usize) -> Vec<f64> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect()
}

fn matrix(rows: usize, columns: usize, values: &[f64]) -> Matrix {
    Matrix::from_row_major(rows, columns, values.to_vec()).unwrap()
}

/// A random symmetric `n × n` matrix.
fn symmetric(seed: u64, n: usize) -> Matrix {
    let random = noise(seed, n * n);
    let mut values = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            values[i * n + j] = random[i * n + j];
            values[j * n + i] = random[i * n + j];
        }
    }
    matrix(n, n, &values)
}

fn largest_difference(a: &Matrix, b: &Matrix) -> f64 {
    assert_eq!((a.rows(), a.columns()), (b.rows(), b.columns()));
    a.values().iter().zip(b.values()).fold(0.0, |most, (x, y)| f64::max(most, (x - y).abs()))
}

#[test]
fn a_matrix_is_its_rows_and_its_shape() {
    assert!(Matrix::from_row_major(2, 2, vec![1.0; 3]).is_none());
    assert!(Matrix::from_rows(&[vec![1.0, 2.0], vec![3.0]]).is_none());
    let a = Matrix::from_rows(&[vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]).unwrap();
    assert_eq!((a.rows(), a.columns()), (2, 3));
    assert_eq!(a.get(1, 0), 4.0);
    assert_eq!(a.row(1), &[4.0, 5.0, 6.0]);
    assert_eq!(a.column(2), vec![3.0, 6.0]);
    assert_eq!(a.transpose(), matrix(3, 2, &[1.0, 4.0, 2.0, 5.0, 3.0, 6.0]));
    assert_eq!(Matrix::from_rows(&[]).unwrap().values(), &[] as &[f64]);
}

#[test]
fn the_product_by_hand_and_a_shape_that_does_not_fit() {
    let a = matrix(2, 2, &[1.0, 2.0, 3.0, 4.0]);
    let b = matrix(2, 1, &[5.0, 6.0]);
    assert_eq!(a.multiply(&b).unwrap(), matrix(2, 1, &[17.0, 39.0]));
    assert!(b.multiply(&a).is_none());
}

/// The textbook factor: every step is exact in binary, so the factor is too.
#[test]
fn the_cholesky_factor_by_hand() {
    let a = matrix(3, 3, &[4.0, 12.0, -16.0, 12.0, 37.0, -43.0, -16.0, -43.0, 98.0]);
    let factor = cholesky(&a).unwrap();
    assert_eq!(factor.lower(), &matrix(3, 3, &[2.0, 0.0, 0.0, 6.0, 1.0, 0.0, -8.0, 5.0, 3.0]));
    // `A · [1, 2, 3] = [-20, -43, 192]`.
    let x = factor.solve(&[-20.0, -43.0, 192.0]).unwrap();
    for (got, want) in x.iter().zip([1.0, 2.0, 3.0]) {
        assert!((got - want).abs() < 1e-12, "{x:?}");
    }
    assert!(factor.solve(&[1.0, 2.0]).is_none());
    // Only the lower triangle is read.
    let lower_only = matrix(3, 3, &[4.0, f64::NAN, 0.0, 12.0, 37.0, 1e9, -16.0, -43.0, 98.0]);
    assert_eq!(cholesky(&lower_only).unwrap(), factor);
}

#[test]
fn cholesky_refuses_what_is_not_positive_definite() {
    assert!(cholesky(&matrix(2, 2, &[1.0, 2.0, 2.0, 1.0])).is_none());
    assert!(cholesky(&matrix(2, 2, &[1.0, 1.0, 1.0, 1.0])).is_none());
    assert!(cholesky(&matrix(2, 2, &[0.0, 0.0, 0.0, 1.0])).is_none());
    assert!(cholesky(&matrix(2, 3, &[1.0; 6])).is_none());
    assert!(cholesky(&matrix(2, 2, &[1.0, 0.0, f64::INFINITY, 1.0])).is_none());
}

/// `y = [1, 3, 2, 5, 4]` on `x = 1..5` with an intercept: `Sxx = 10`, `Sxy = 8`, so the slope is
/// `0.8` and the intercept `3 - 2.4 = 0.6`; the residuals `[-0.4, 0.8, -1, 1.2, -0.6]` give
/// `RSS = 3.6` over 3 degrees, `s² = 1.2`, `se(slope)² = s² / Sxx = 0.12` and
/// `se(intercept)² = s² (1/5 + 9/10) = 1.32`.
#[test]
fn the_least_squares_line_by_hand() {
    let design = matrix(5, 2, &[1.0, 1.0, 1.0, 2.0, 1.0, 3.0, 1.0, 4.0, 1.0, 5.0]);
    let fit = least_squares(&design, &[1.0, 3.0, 2.0, 5.0, 4.0]).unwrap();
    let close = |got: f64, want: f64| (got - want).abs() <= 1e-13 * want.abs();
    assert!(close(fit.coefficients[0], 0.6) && close(fit.coefficients[1], 0.8), "{fit:?}");
    assert!(close(fit.residual_sum_of_squares, 3.6), "{fit:?}");
    assert_eq!(fit.degrees_of_freedom, 3);
    assert!(close(fit.residual_variance(), 1.2));
    let errors = fit.standard_errors();
    assert!(close(errors[0], 1.32f64.sqrt()) && close(errors[1], 0.12f64.sqrt()), "{errors:?}");
}

/// A square design is solved exactly and leaves nothing to estimate the error variance from.
#[test]
fn a_square_design_is_an_exact_solve() {
    let fit = least_squares(&matrix(2, 2, &[2.0, 1.0, 1.0, 3.0]), &[3.0, 5.0]).unwrap();
    assert!((fit.coefficients[0] - 0.8).abs() < 1e-14 && (fit.coefficients[1] - 1.4).abs() < 1e-14);
    assert_eq!(fit.degrees_of_freedom, 0);
    assert!(fit.residual_variance().is_nan());
    assert!(fit.standard_errors().iter().all(|e| e.is_nan()));
}

#[test]
fn least_squares_refuses_what_has_no_unique_answer() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0];
    let y = [1.0, 3.0, 2.0, 5.0, 4.0];
    // A column that is twice another, and an all-zero column.
    let collinear: Vec<f64> = x.iter().flat_map(|v| [1.0, *v, 2.0 * v]).collect();
    assert!(least_squares(&matrix(5, 3, &collinear), &y).is_none());
    let zero: Vec<f64> = x.iter().flat_map(|v| [*v, 0.0]).collect();
    assert!(least_squares(&matrix(5, 2, &zero), &y).is_none());
    // More columns than rows, no column, the wrong number of observations, a non-finite entry.
    assert!(least_squares(&matrix(1, 2, &[1.0, 2.0]), &[1.0]).is_none());
    assert!(least_squares(&matrix(5, 0, &[]), &y).is_none());
    assert!(least_squares(&matrix(5, 1, &x), &y[..4]).is_none());
    assert!(least_squares(&matrix(5, 1, &x), &[1.0, 3.0, f64::NAN, 5.0, 4.0]).is_none());
}

/// On random designs: the residual is orthogonal to every column, the coefficients are the normal
/// equations' (solved through [`cholesky`]), and `R` is the Cholesky factor of `XᵀX` up to the
/// sign of each row (`XᵀX = RᵀR`).
#[test]
fn least_squares_agrees_with_the_normal_equations() {
    for (seed, rows, columns) in [(1, 6, 1), (2, 7, 3), (3, 40, 5), (4, 200, 8)] {
        let design = matrix(rows, columns, &noise(seed, rows * columns));
        let y = noise(seed + 100, rows);
        let fit = least_squares(&design, &y).unwrap();
        let fitted = design.multiply(&matrix(columns, 1, &fit.coefficients)).unwrap();
        let residual: Vec<f64> = y.iter().zip(fitted.values()).map(|(a, b)| a - b).collect();
        let normal = design.transpose().multiply(&design).unwrap();
        let moment = design.transpose().multiply(&matrix(rows, 1, &y)).unwrap();
        let orthogonality = design.transpose().multiply(&matrix(rows, 1, &residual)).unwrap();
        assert!(orthogonality.values().iter().all(|v| v.abs() < 1e-12), "{seed}");
        let mut rss = 0.0;
        for r in &residual {
            rss += r * r;
        }
        assert!((rss - fit.residual_sum_of_squares).abs() < 1e-12 * rss, "{seed}");
        let factor = cholesky(&normal).unwrap();
        let through_normal = factor.solve(moment.values()).unwrap();
        for (a, b) in fit.coefficients.iter().zip(&through_normal) {
            assert!((a - b).abs() < 1e-10 * (1.0 + b.abs()), "{seed}: {a} {b}");
        }
        let r = &fit.triangle;
        for k in 0..columns {
            let sign = if r.get(k, k) < 0.0 { -1.0 } else { 1.0 };
            for j in k..columns {
                let l = factor.lower().get(j, k);
                assert!((sign * r.get(k, j) - l).abs() < 1e-10 * (1.0 + l.abs()), "{seed} {k} {j}");
            }
        }
    }
}

/// The second-difference matrix: eigenvalues `2 + √2, 2, 2 - √2` with eigenvectors
/// `(1, -√2, 1) / 2`, `(1, 0, -1) / √2` and `(1, √2, 1) / 2`.
#[test]
fn the_eigen_decomposition_by_hand() {
    let a = matrix(3, 3, &[2.0, -1.0, 0.0, -1.0, 2.0, -1.0, 0.0, -1.0, 2.0]);
    let eigen = symmetric_eigen(&a).unwrap();
    let root = 2f64.sqrt();
    for (got, want) in eigen.values.iter().zip([2.0 + root, 2.0, 2.0 - root]) {
        assert!((got - want).abs() < 1e-14, "{:?}", eigen.values);
    }
    // The sign rule makes the largest-magnitude entry positive: `-√2/2` flips the first vector.
    let first = [-0.5, root / 2.0, -0.5];
    let last = [0.5, root / 2.0, 0.5];
    for r in 0..3 {
        assert!((eigen.vectors.get(r, 0) - first[r]).abs() < 1e-14, "{:?}", eigen.vectors);
        assert!((eigen.vectors.get(r, 2) - last[r]).abs() < 1e-14, "{:?}", eigen.vectors);
    }
    // The middle vector's two largest entries tie in magnitude, so either sign is the rule's.
    assert!(eigen.vectors.get(1, 1).abs() < 1e-14);
    assert!((eigen.vectors.get(0, 1) + eigen.vectors.get(2, 1)).abs() < 1e-14);
    assert!((eigen.vectors.get(0, 1).abs() - 1.0 / root).abs() < 1e-14);
}

#[test]
fn a_diagonal_matrix_is_only_sorted() {
    let eigen =
        symmetric_eigen(&matrix(3, 3, &[1.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 2.0])).unwrap();
    assert_eq!(eigen.values, vec![3.0, 2.0, 1.0]);
    assert_eq!(eigen.vectors, matrix(3, 3, &[0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]));
    let empty = symmetric_eigen(&matrix(0, 0, &[])).unwrap();
    assert!(empty.values.is_empty());
}

/// A rank-one matrix: one eigenvalue carries the whole trace and the others are zero.
#[test]
fn a_rank_one_matrix_has_one_eigenvalue() {
    let eigen = symmetric_eigen(&matrix(3, 3, &[1.0; 9])).unwrap();
    assert!((eigen.values[0] - 3.0).abs() < 1e-14, "{:?}", eigen.values);
    assert!(eigen.values[1..].iter().all(|v| v.abs() < 1e-14), "{:?}", eigen.values);
    let third = 1.0 / 3f64.sqrt();
    assert!(eigen.vectors.column(0).iter().all(|v| (v - third).abs() < 1e-14));
}

/// On random symmetric matrices: `A V = V Λ`, `Vᵀ V = I`, the trace is kept, the values fall, and
/// every column obeys the sign rule.
#[test]
fn the_eigen_decomposition_reconstructs_random_matrices() {
    for n in 1..=12 {
        let a = symmetric(n as u64, n);
        let eigen = symmetric_eigen(&a).unwrap();
        let vectors = &eigen.vectors;
        let mut scaled = vectors.clone();
        for r in 0..n {
            for c in 0..n {
                scaled.values[r * n + c] *= eigen.values[c];
            }
        }
        let applied = a.multiply(vectors).unwrap();
        assert!(largest_difference(&applied, &scaled) < 1e-12, "{n}");
        let gram = vectors.transpose().multiply(vectors).unwrap();
        assert!(largest_difference(&gram, &Matrix::identity(n)) < 1e-13, "{n}");
        let (mut trace, mut total) = (0.0, 0.0);
        for (i, value) in eigen.values.iter().enumerate() {
            trace += a.get(i, i);
            total += value;
        }
        assert!((trace - total).abs() < 1e-13, "{n}");
        assert!(eigen.values.windows(2).all(|w| w[0] >= w[1]), "{n}");
        for c in 0..n {
            let column = vectors.column(c);
            let largest = column.iter().fold(0.0, |most: f64, v| most.max(v.abs()));
            let first = column.iter().find(|v| v.abs() == largest).unwrap();
            assert!(*first > 0.0, "{n} {c}");
        }
    }
}

#[test]
fn symmetric_eigen_reads_the_lower_triangle_and_refuses_what_it_cannot() {
    let lower_only = matrix(2, 2, &[2.0, f64::NAN, 1.0, 2.0]);
    let eigen = symmetric_eigen(&lower_only).unwrap();
    assert!((eigen.values[0] - 3.0).abs() < 1e-15 && (eigen.values[1] - 1.0).abs() < 1e-15);
    assert!(symmetric_eigen(&matrix(2, 2, &[2.0, 0.0, f64::NAN, 2.0])).is_none());
    assert!(symmetric_eigen(&matrix(2, 3, &[1.0; 6])).is_none());
}

/// `x = [1, 2, 3, 4]`, `y = [2, 4, 6, 9]`: the centred values are exact quarters, so
/// `Sxx = 5`, `Syy = 26.75` and `Sxy = 11.5`, each over 3.
#[test]
fn the_covariance_and_correlation_matrices_by_hand() {
    let observations = matrix(4, 2, &[1.0, 2.0, 2.0, 4.0, 3.0, 6.0, 4.0, 9.0]);
    let covariance = covariance_matrix(&observations).unwrap();
    let want = [5.0 / 3.0, 11.5 / 3.0, 11.5 / 3.0, 26.75 / 3.0];
    for (got, want) in covariance.values().iter().zip(want) {
        assert!((got - want).abs() <= 1e-15 * want, "{covariance:?}");
    }
    let correlation = correlation_matrix(&observations).unwrap();
    let r = 11.5 / (5.0f64 * 26.75).sqrt();
    assert_eq!((correlation.get(0, 0), correlation.get(1, 1)), (1.0, 1.0));
    assert!((correlation.get(0, 1) - r).abs() < 1e-15, "{correlation:?} {r}");
    // Exactly symmetric: one product divides both entries.
    assert_eq!(correlation.get(0, 1), correlation.get(1, 0));
    // A line correlates at 1 up to rounding, and the clip keeps it from stepping past 1.
    let line = matrix(3, 2, &[0.1, 1.2, 0.7, 2.4, 1.3, 3.6]);
    let r = correlation_matrix(&line).unwrap().get(0, 1);
    assert!(r <= 1.0 && r > 1.0 - 1e-15, "{r}");
}

#[test]
fn no_covariance_or_correlation_without_an_answer() {
    let constant = matrix(3, 2, &[1.0, 5.0, 2.0, 5.0, 3.0, 5.0]);
    let covariance = covariance_matrix(&constant).unwrap();
    assert_eq!((covariance.get(1, 1), covariance.get(0, 1)), (0.0, 0.0));
    assert!(correlation_matrix(&constant).is_none());
    assert!(covariance_matrix(&matrix(1, 2, &[1.0, 2.0])).is_none());
    assert!(covariance_matrix(&matrix(3, 0, &[])).is_none());
    assert!(covariance_matrix(&matrix(2, 1, &[1.0, f64::NAN])).is_none());
}

/// `[[4, 3], [3, 9]]`: deviations 2 and 3, so the correlation is `3 / 6 = 0.5`, exactly.
#[test]
fn a_covariance_rescaled_to_its_correlation_by_hand() {
    let correlation = correlation_from_covariance(&matrix(2, 2, &[4.0, 3.0, 3.0, 9.0])).unwrap();
    assert_eq!(correlation, matrix(2, 2, &[1.0, 0.5, 0.5, 1.0]));
    assert!(correlation_from_covariance(&matrix(2, 2, &[4.0, 0.0, 0.0, 0.0])).is_none());
    assert!(correlation_from_covariance(&matrix(2, 2, &[4.0, 0.0, 0.0, -1.0])).is_none());
    assert!(correlation_from_covariance(&matrix(2, 2, &[4.0, f64::NAN, 0.0, 1.0])).is_none());
    assert!(correlation_from_covariance(&matrix(1, 2, &[4.0, 1.0])).is_none());
}

/// The dot product and the squared distance by hand, over the common prefix.
#[test]
fn the_dot_product_and_the_squared_distance_by_hand() {
    assert_eq!(dot(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]), 32.0);
    assert_eq!(dot(&[1.0, 2.0], &[3.0]), 3.0);
    assert_eq!(squared_distance(&[1.0, 2.0], &[4.0, 6.0]), 25.0);
    assert_eq!(squared_distance(&[], &[1.0]), 0.0);
}

/// The column means are compensated (a plain fold reads `0` for the first column), and a matrix with
/// no row has none.
#[test]
fn the_column_means_are_compensated() {
    let observations = matrix(3, 2, &[1e16, 1.0, 1.0, 2.0, -1e16, 6.0]);
    assert_eq!(column_means(&observations), vec![1.0 / 3.0, 3.0]);
    let empty = column_means(&matrix(0, 2, &[]));
    assert!(empty.len() == 2 && empty.iter().all(|m| m.is_nan()), "{empty:?}");
}

/// Each entry of the covariance matrix is the covariance of its two columns, to the bit, and the
/// matrix is exactly symmetric.
#[test]
fn the_covariance_matrix_is_the_pairwise_covariance() {
    let observations = matrix(7, 3, &noise(11, 21));
    let covariance_of = covariance_matrix(&observations).unwrap();
    for i in 0..3 {
        for j in 0..3 {
            let want = crate::statistics::covariance(
                &observations.column(i),
                &observations.column(j),
                1.0,
            );
            assert_eq!(covariance_of.get(i, j).to_bits(), want.to_bits(), "({i}, {j})");
            assert_eq!(covariance_of.get(i, j).to_bits(), covariance_of.get(j, i).to_bits());
        }
    }
}

/// Rebuilding with the identity gives the matrix back, exactly symmetric; with `1 / √λ` it is the
/// inverse square root (`S^(-1/2) S S^(-1/2) = I`); with zero it is the zero matrix.
#[test]
fn rebuild_with_maps_the_eigenvalues() {
    let a = symmetric(5, 4);
    let eigen = symmetric_eigen(&a).unwrap();
    let rebuilt = eigen.rebuild_with(|value| value);
    assert!(largest_difference(&rebuilt, &a) < 1e-12, "{rebuilt:?}");
    for i in 0..4 {
        for j in 0..4 {
            assert_eq!(rebuilt.get(i, j).to_bits(), rebuilt.get(j, i).to_bits());
        }
    }
    assert!(eigen.rebuild_with(|_| 0.0).values().iter().all(|v| *v == 0.0));
    let positive = covariance_matrix(&matrix(9, 3, &noise(3, 27))).unwrap();
    let root = symmetric_eigen(&positive).unwrap().rebuild_with(|value| 1.0 / value.sqrt());
    let identity = root.multiply(&positive).unwrap().multiply(&root).unwrap();
    assert!(largest_difference(&identity, &Matrix::identity(3)) < 1e-9, "{identity:?}");
}
