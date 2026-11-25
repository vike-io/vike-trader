//! Does a factor (a feature, a model score) order the forward returns? The rank Information
//! Coefficient per period and the mean forward return per factor quantile.
//!
//! Grinold and Kahn's IC, measured the way factor research reads it (Spearman's rank correlation
//! across the assets of one period), and the quantile spread beside it.
//!
//! # The contract
//!
//! - A PANEL is `periods × assets`: `factor[p][a]` is asset `a`'s factor value at period `p`, known
//!   at that period, and `forward_returns[p][a]` is the return that follows it. The caller aligns
//!   them; nothing here shifts a series. Rows of one period must have equal lengths.
//! - Per period, only the assets where BOTH values are finite take part (a NaN is "no value").
//! - Ties take the AVERAGE rank (two values tied for ranks 3 and 4 both rank 3.5), Spearman's usual
//!   convention, computed exactly as doubled integer ranks.

use vike_math::statistics::{doubled_average_ranks, pearson};

/// The rank IC of each period: Spearman's correlation between the factor and the forward return
/// across the assets with both values finite. NaN for a period with fewer than two such assets or
/// with every factor (or every return) tied, where a correlation is undefined: a NaN is "no
/// reading", never a zero that would read as "no skill".
///
/// # Panics
///
/// When the two panels have different period counts, or a period's two rows differ in length.
pub fn rank_information_coefficient(factor: &[Vec<f64>], forward_returns: &[Vec<f64>]) -> Vec<f64> {
    assert_eq!(factor.len(), forward_returns.len(), "one return row per factor row");
    factor
        .iter()
        .zip(forward_returns)
        .map(|(factor_row, return_row)| {
            let (factor_values, return_values) = finite_pairs(factor_row, return_row);
            let factor_ranks = doubled_average_ranks(&factor_values);
            let return_ranks = doubled_average_ranks(&return_values);
            if factor_values.len() < 2 || all_equal(&factor_ranks) || all_equal(&return_ranks) {
                return f64::NAN;
            }
            let as_f64 =
                |ranks: Vec<usize>| ranks.into_iter().map(|r| r as f64).collect::<Vec<_>>();
            // Pearson is scale-free, so the doubled ranks give Spearman's coefficient exactly.
            pearson(&as_f64(factor_ranks), &as_f64(return_ranks))
        })
        .collect()
}

/// The mean forward return of each factor quantile, per period: `out[p][b]` for bucket
/// `b in 0..quantiles`, bucket 0 holding the LOWEST factor values. NaN for an empty bucket.
///
/// Among a period's `m` assets with both values finite, the asset of average rank `r` (1-based)
/// falls in bucket `ceil(r × quantiles / m) - 1`: equal counts when `quantiles` divides `m`, the
/// remainder toward the top otherwise, and tied factor values always share a bucket.
///
/// # Panics
///
/// As [`rank_information_coefficient`], and when `quantiles` is 0.
pub fn quantile_returns(
    factor: &[Vec<f64>],
    forward_returns: &[Vec<f64>],
    quantiles: usize,
) -> Vec<Vec<f64>> {
    assert!(quantiles > 0, "at least one quantile");
    assert_eq!(factor.len(), forward_returns.len(), "one return row per factor row");
    factor
        .iter()
        .zip(forward_returns)
        .map(|(factor_row, return_row)| {
            let (factor_values, return_values) = finite_pairs(factor_row, return_row);
            let doubled_count = 2 * factor_values.len();
            let mut sums = vec![0.0; quantiles];
            let mut counts = vec![0_usize; quantiles];
            for (doubled_rank, value) in
                doubled_average_ranks(&factor_values).into_iter().zip(return_values)
            {
                // ceil(r q / m) - 1 = ceil(2r q / 2m) - 1, in integers.
                let bucket = (doubled_rank * quantiles).div_ceil(doubled_count) - 1;
                sums[bucket] += value;
                counts[bucket] += 1;
            }
            sums.iter()
                .zip(&counts)
                .map(|(&sum, &count)| if count == 0 { f64::NAN } else { sum / count as f64 })
                .collect()
        })
        .collect()
}

/// The `(factor, return)` values of one period where both are finite, in asset order.
fn finite_pairs(factor_row: &[f64], return_row: &[f64]) -> (Vec<f64>, Vec<f64>) {
    assert_eq!(factor_row.len(), return_row.len(), "one return per asset");
    factor_row
        .iter()
        .zip(return_row)
        .filter(|(f, r)| f.is_finite() && r.is_finite())
        .map(|(&f, &r)| (f, r))
        .unzip()
}

fn all_equal(ranks: &[usize]) -> bool {
    ranks.windows(2).all(|pair| pair[0] == pair[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_ic_hand_cases() {
        let factor = vec![
            vec![1.0, 2.0, 3.0, 4.0],
            vec![1.0, 1.0, 2.0, f64::NAN],
            vec![1.0, f64::NAN],
            vec![5.0, 5.0, 5.0],
        ];
        let returns = vec![
            vec![0.1, 0.3, 0.2, 0.4],
            vec![0.0, 0.1, 0.2, 0.3],
            vec![0.1, 0.2],
            vec![0.1, 0.2, 0.3],
        ];
        let ic = rank_information_coefficient(&factor, &returns);
        // ranks (1,2,3,4) vs (1,3,2,4): 1 - 6·2 / (4·15) = 0.8.
        assert!((ic[0] - 0.8).abs() < 1e-15);
        // ranks (1.5,1.5,3) vs (1,2,3): cov 1.5, variances 1.5 and 2 -> √3 / 2.
        assert!((ic[1] - 3.0_f64.sqrt() / 2.0).abs() < 1e-15);
        assert!(ic[2].is_nan(), "one finite pair");
        assert!(ic[3].is_nan(), "every factor tied");
    }

    #[test]
    fn quantile_returns_hand_cases() {
        let factor = vec![vec![4.0, 1.0, 3.0, 2.0]];
        let returns = vec![vec![0.75, 0.25, 0.5, 0.125]];
        // q=2: ranks 4,1,3,2 -> buckets 1,0,1,0.
        assert_eq!(quantile_returns(&factor, &returns, 2), vec![vec![0.1875, 0.625]]);
        // q=3: ceil(3r/4) - 1 -> ranks 1,2 -> 0,1; ranks 3,4 -> 2.
        assert_eq!(quantile_returns(&factor, &returns, 3), vec![vec![0.25, 0.125, 0.625]]);
        // more buckets than assets leaves some empty.
        let wide = quantile_returns(&[vec![1.0, 2.0]], &[vec![0.5, 0.25]], 4);
        assert_eq!((wide[0][1], wide[0][3]), (0.5, 0.25));
        assert!(wide[0][0].is_nan() && wide[0][2].is_nan());
    }
}
