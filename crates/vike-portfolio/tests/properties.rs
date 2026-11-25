//! Property tests of `vike-portfolio` over random covariances: a fully invested portfolio sums to one,
//! a long-only one holds no short, the long-only solves meet their optimality (KKT) conditions, risk
//! parity equalises the risk contributions, every portfolio but Hierarchical Risk Parity follows its
//! assets when their order changes (its tree breaks ties by asset index, as the published method
//! does: `vike_portfolio::hierarchical`'s module doc), denoising leaves a valid correlation matrix, and
//! the bet size is bounded, monotone in the probability, and discretised to its step.
//!
//! A market is `4n` random returns of `n ≤ 6` assets mixed by a random `n × n` matrix (so the assets
//! correlate), their sample covariance plus `10⁻⁴` on the diagonal (so it is positive definite and
//! not absurdly conditioned), random expected excess returns, and a reordering (a rotation, perhaps
//! reversed).

use proptest::collection::vec;
use proptest::prelude::*;
use vike_math::linear_algebra::{
    Matrix, correlation_from_covariance, covariance_matrix, dot, symmetric_eigen,
};
use vike_portfolio::bet_size::{bet_size, discretise_bet_size};
use vike_portfolio::denoise::{denoise_correlation, detone};
use vike_portfolio::hierarchical::{clusters_by_silhouette, hierarchical_risk_parity};
use vike_portfolio::kelly::kelly_weights;
use vike_portfolio::mean_variance::{
    long_only_maximum_sharpe, long_only_minimum_variance, maximum_sharpe, minimum_variance,
};
use vike_portfolio::nested_clustered::nested_clustered_optimisation;
use vike_portfolio::risk_parity::{
    inverse_variance, inverse_volatility, risk_contributions, risk_parity,
};

#[derive(Debug)]
struct Market {
    covariance: Matrix,
    returns: Vec<f64>,
    /// New position `i` holds old asset `order[i]`.
    order: Vec<usize>,
}

fn markets() -> impl Strategy<Value = Market> {
    (2usize..=6)
        .prop_flat_map(|n| {
            (
                vec(-0.05f64..0.05, 4 * n * n),
                vec(-1.0f64..1.0, n * n),
                vec(-0.02f64..0.03, n),
                0..n,
                any::<bool>(),
            )
        })
        .prop_map(|(draws, mixing, returns, rotation, reverse)| {
            let n = returns.len();
            let draws = Matrix::from_row_major(4 * n, n, draws).unwrap();
            let mixing = Matrix::from_row_major(n, n, mixing).unwrap();
            let sample = covariance_matrix(&draws.multiply(&mixing).unwrap()).unwrap();
            let mut values = sample.values().to_vec();
            for i in 0..n {
                values[i * n + i] += 1e-4;
            }
            let mut order: Vec<usize> = (0..n).map(|i| (i + rotation) % n).collect();
            if reverse {
                order.reverse();
            }
            Market { covariance: Matrix::from_row_major(n, n, values).unwrap(), returns, order }
        })
}

fn permuted(matrix: &Matrix, order: &[usize]) -> Matrix {
    let values = order.iter().flat_map(|&i| order.iter().map(move |&j| matrix.get(i, j))).collect();
    Matrix::from_row_major(order.len(), order.len(), values).unwrap()
}

fn reordered(values: &[f64], order: &[usize]) -> Vec<f64> {
    order.iter().map(|&i| values[i]).collect()
}

fn total(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |t, v| t + v)
}

/// The largest magnitude, the scale a tolerance is taken of (no `vike-math` function computes it).
fn largest(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |m: f64, v| m.max(v.abs()))
}

/// `matrix · vector`, through the one matrix product (`vector` as a column).
fn times(matrix: &Matrix, vector: &[f64]) -> Vec<f64> {
    let column = Matrix::from_row_major(vector.len(), 1, vector.to_vec()).unwrap();
    matrix.multiply(&column).unwrap().values().to_vec()
}

/// The portfolio variance `wᵀ Σ w`, the one `dot` of [`times`] (`vike-math` has no quadratic form).
fn variance(covariance: &Matrix, weights: &[f64]) -> f64 {
    dot(weights, &times(covariance, weights))
}

fn close_all(what: &str, got: &[f64], want: &[f64], tolerance: f64) -> Result<(), TestCaseError> {
    prop_assert_eq!(got.len(), want.len());
    let scale = largest(want).max(1.0);
    for (g, w) in got.iter().zip(want) {
        prop_assert!((g - w).abs() <= tolerance * scale, "{what}: {got:?} vs {want:?}");
    }
    Ok(())
}

fn fully_invested(what: &str, weights: &[f64]) -> Result<(), TestCaseError> {
    let error = (total(weights) - 1.0).abs();
    prop_assert!(error <= 1e-12 * largest(weights).max(1.0), "{what} sums to {}", total(weights));
    Ok(())
}

fn long(what: &str, weights: &[f64]) -> Result<(), TestCaseError> {
    fully_invested(what, weights)?;
    prop_assert!(weights.iter().all(|w| *w >= 0.0), "{what} holds a short: {weights:?}");
    Ok(())
}

/// `after`, computed on the reordered market, is `before` reordered.
fn follows(
    what: &str,
    before: &[f64],
    after: &[f64],
    order: &[usize],
    tolerance: f64,
) -> Result<(), TestCaseError> {
    close_all(what, after, &reordered(before, order), tolerance)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn fully_invested_portfolios_sum_to_one_and_long_ones_hold_no_short(market in markets()) {
        let (covariance, returns) = (&market.covariance, &market.returns);
        fully_invested("minimum_variance", &minimum_variance(covariance).unwrap())?;
        if let Some(weights) = maximum_sharpe(covariance, returns) {
            fully_invested("maximum_sharpe", &weights)?;
        }
        long("long_only_minimum_variance", &long_only_minimum_variance(covariance).unwrap())?;
        let some_positive = returns.iter().any(|r| *r > 0.0);
        let long_sharpe = long_only_maximum_sharpe(covariance, returns);
        prop_assert_eq!(long_sharpe.is_some(), some_positive);
        if let Some(weights) = long_sharpe {
            long("long_only_maximum_sharpe", &weights)?;
        }
        long("inverse_volatility", &inverse_volatility(covariance).unwrap())?;
        long("inverse_variance", &inverse_variance(covariance).unwrap())?;
        long("risk_parity", &risk_parity(covariance).unwrap())?;
        long("hierarchical_risk_parity", &hierarchical_risk_parity(covariance).unwrap())?;
        let n = returns.len();
        let correlation = correlation_from_covariance(covariance).unwrap();
        let clusters = clusters_by_silhouette(&correlation, n)
            .unwrap_or_else(|| (0..n).map(|i| vec![i]).collect());
        let nested = nested_clustered_optimisation(covariance, None, &clusters).unwrap();
        fully_invested("nested_clustered_optimisation", &nested)?;
    }

    /// Long-only minimum variance: every held asset's marginal variance `(Σw)ᵢ` equals the portfolio
    /// variance and no empty one's is below it. Long-only maximum Sharpe: `(Σw)ᵢ = c μᵢ` where held
    /// and `≥ c μᵢ` where not, `c = wᵀΣw / μᵀw`. Both beat every feasible single asset and the equal
    /// weights, and neither beats its unconstrained twin.
    #[test]
    fn the_long_only_solves_meet_their_optimality_conditions(market in markets()) {
        let (covariance, returns) = (&market.covariance, &market.returns);
        let n = returns.len();
        let weights = long_only_minimum_variance(covariance).unwrap();
        let marginal = times(covariance, &weights);
        let level = variance(covariance, &weights);
        let tolerance = 1e-7 * largest(&marginal);
        for (w, m) in weights.iter().zip(&marginal) {
            prop_assert!(*m >= level - tolerance, "{marginal:?} vs {level}");
            if *w > 1e-6 {
                prop_assert!((m - level).abs() <= tolerance, "{marginal:?} vs {level} at {weights:?}");
            }
        }
        let unconstrained = minimum_variance(covariance).unwrap();
        prop_assert!(level >= variance(covariance, &unconstrained) * (1.0 - 1e-12));
        prop_assert!(level <= variance(covariance, &vec![1.0 / n as f64; n]) * (1.0 + 1e-9));
        for i in 0..n {
            prop_assert!(level <= covariance.get(i, i) * (1.0 + 1e-9));
        }

        let Some(weights) = long_only_maximum_sharpe(covariance, returns) else {
            return Ok(());
        };
        let marginal = times(covariance, &weights);
        let excess = dot(&weights, returns);
        prop_assert!(excess > 0.0);
        let slope = variance(covariance, &weights) / excess;
        let tolerance = 1e-7 * largest(&marginal);
        for ((w, m), r) in weights.iter().zip(&marginal).zip(returns) {
            prop_assert!(*m >= slope * r - tolerance, "{marginal:?} vs {slope} · {returns:?}");
            if *w > 1e-6 {
                prop_assert!((m - slope * r).abs() <= tolerance, "{marginal:?} at {weights:?}");
            }
        }
        let sharpe = |w: &[f64]| dot(w, returns) / variance(covariance, w).sqrt();
        let best = sharpe(&weights);
        for i in (0..n).filter(|&i| returns[i] > 0.0) {
            let mut single = vec![0.0; n];
            single[i] = 1.0;
            prop_assert!(best >= sharpe(&single) * (1.0 - 1e-9));
        }
        if let Some(unconstrained) = maximum_sharpe(covariance, returns) {
            prop_assert!(best <= sharpe(&unconstrained) * (1.0 + 1e-9));
        }
    }

    #[test]
    fn risk_parity_equalises_the_risk_contributions(market in markets()) {
        let weights = risk_parity(&market.covariance).unwrap();
        let contributions = risk_contributions(&market.covariance, &weights).unwrap();
        let share = total(&contributions) / contributions.len() as f64;
        for c in &contributions {
            prop_assert!((c - share).abs() <= 1e-9 * share, "{contributions:?}");
        }
    }

    #[test]
    fn the_weights_follow_their_assets_when_the_order_changes(market in markets()) {
        let (covariance, returns, order) = (&market.covariance, &market.returns, &market.order);
        let moved = permuted(covariance, order);
        let moved_returns = reordered(returns, order);
        let closed = 1e-9;
        follows("minimum_variance", &minimum_variance(covariance).unwrap(),
            &minimum_variance(&moved).unwrap(), order, closed)?;
        // Both or neither, but for a `1ᵀΣ⁻¹μ` so near zero that rounding picks its sign.
        if let (Some(before), Some(after)) =
            (maximum_sharpe(covariance, returns), maximum_sharpe(&moved, &moved_returns))
        {
            follows("maximum_sharpe", &before, &after, order, closed)?;
        }
        follows("kelly_weights", &kelly_weights(covariance, returns, 0.5).unwrap(),
            &kelly_weights(&moved, &moved_returns, 0.5).unwrap(), order, closed)?;
        follows("inverse_volatility", &inverse_volatility(covariance).unwrap(),
            &inverse_volatility(&moved).unwrap(), order, closed)?;
        follows("inverse_variance", &inverse_variance(covariance).unwrap(),
            &inverse_variance(&moved).unwrap(), order, closed)?;
        follows("risk_parity", &risk_parity(covariance).unwrap(),
            &risk_parity(&moved).unwrap(), order, closed)?;
        follows("long_only_minimum_variance", &long_only_minimum_variance(covariance).unwrap(),
            &long_only_minimum_variance(&moved).unwrap(), order, 1e-7)?;
        if let Some(before) = long_only_maximum_sharpe(covariance, returns) {
            follows("long_only_maximum_sharpe", &before,
                &long_only_maximum_sharpe(&moved, &moved_returns).unwrap(), order, 1e-7)?;
        }
        // The same clusters, renamed: old asset `order[i]` is new asset `i`.
        let n = order.len();
        let mut new_index = vec![0; n];
        for (i, &old) in order.iter().enumerate() {
            new_index[old] = i;
        }
        let assets: Vec<usize> = (0..n).collect();
        let clusters: Vec<Vec<usize>> = assets.chunks(2).map(<[usize]>::to_vec).collect();
        let renamed: Vec<Vec<usize>> =
            clusters.iter().map(|c| c.iter().map(|&a| new_index[a]).collect()).collect();
        follows("nested_clustered_optimisation",
            &nested_clustered_optimisation(covariance, None, &clusters).unwrap(),
            &nested_clustered_optimisation(&moved, None, &renamed).unwrap(), order, closed)?;
        if let (Some(before), Some(after)) = (
            nested_clustered_optimisation(covariance, Some(returns), &clusters),
            nested_clustered_optimisation(&moved, Some(&moved_returns), &renamed),
        ) {
            follows("nested_clustered_optimisation (Sharpe)", &before, &after, order, closed)?;
        }
        let correlation = correlation_from_covariance(covariance).unwrap();
        let denoised = denoise_correlation(&correlation, 1).unwrap();
        let moved_denoised = denoise_correlation(&permuted(&correlation, order), 1).unwrap();
        close_all("denoise_correlation", moved_denoised.values(),
            permuted(&denoised, order).values(), closed)?;
    }

    /// Denoised, a correlation matrix is still one: exactly symmetric, a unit diagonal, entries in
    /// `[−1, 1]`, no negative eigenvalue. Detoned, it keeps the unit diagonal.
    #[test]
    fn denoising_leaves_a_correlation_matrix(market in markets(), keep in 0usize..=6) {
        let correlation = correlation_from_covariance(&market.covariance).unwrap();
        let n = correlation.rows();
        let denoised = denoise_correlation(&correlation, keep.min(n)).unwrap();
        prop_assert_eq!(&denoised, &denoised.transpose());
        for i in 0..n {
            prop_assert_eq!(denoised.get(i, i), 1.0);
        }
        prop_assert!(denoised.values().iter().all(|v| (-1.0..=1.0).contains(v)));
        let spectrum = symmetric_eigen(&denoised).unwrap().values;
        prop_assert!(spectrum.iter().all(|v| *v >= -1e-12), "{spectrum:?}");
        if let Some(detoned) = detone(&correlation, 1) {
            for i in 0..n {
                prop_assert_eq!(detoned.get(i, i), 1.0);
            }
        }
    }

    #[test]
    fn the_bet_size_is_bounded_monotone_and_discretised(
        first in 0.0f64..=1.0,
        second in 0.0f64..=1.0,
        classes in 2usize..=5,
        step in 0.01f64..=1.0,
    ) {
        let (low, high) = if first <= second { (first, second) } else { (second, first) };
        let (small, large) = (bet_size(low, classes).unwrap(), bet_size(high, classes).unwrap());
        prop_assert!((-1.0..=1.0).contains(&small) && (-1.0..=1.0).contains(&large));
        prop_assert!(small <= large + 1e-15, "{low} → {small}, {high} → {large}");
        let discrete = discretise_bet_size(large, step).unwrap();
        prop_assert!((-1.0..=1.0).contains(&discrete));
        let steps = discrete / step;
        prop_assert!(discrete.abs() == 1.0 || (steps - steps.round()).abs() < 1e-9, "{discrete} / {step}");
        prop_assert!(discrete.abs() == 1.0 || (discrete - large).abs() <= step / 2.0 + 1e-12);
    }
}
