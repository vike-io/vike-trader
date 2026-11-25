//! Hierarchical Risk Parity (López de Prado, "Building Diversified Portfolios that Outperform Out of
//! Sample", 2016; *Advances in Financial Machine Learning*, 2018, ch. 16) and the correlation
//! clustering it shares with Nested Clustered Optimisation (`crate::nested_clustered`).
//!
//! **The clustering.** A correlation `ρᵢⱼ` becomes the distance `dᵢⱼ = √((1 − ρᵢⱼ) / 2)`, and two
//! assets are as far apart as their whole distance profiles: `d̃ᵢⱼ = √(Σₖ (dₖᵢ − dₖⱼ)²)`, the distance
//! the book's single linkage and its k-means both measure (its snippets 16.1 and *Machine Learning for
//! Asset Managers*' 4.1 hand `d` to them as observations). The tree is single linkage on `d̃`,
//! merging the closest pair of clusters first; the first pair found in index order wins a tie. Each
//! merge lists the cluster with the LOWER id first, a leaf's id its asset index and the `m`-th merge's
//! id `n + m`, which is SciPy's `linkage` convention the book's code reads.
//!
//! **Hierarchical Risk Parity** ([`hierarchical_risk_parity`]): order the assets by the tree, each
//! cluster's leaves its first child's then its second's ([`quasi_diagonal_order`], snippet 16.2), then
//! bisect that list recursively (snippet 16.3): each half's variance at inverse-variance weights,
//! and the budget split `1 − V₀ / (V₀ + V₁)` to the first half. Long-only and fully invested, and it
//! inverts no matrix, so a singular covariance is no obstacle.
//!
//! ⚠ **Not invariant to the asset order, by construction of the published method.** The child order
//! at a merge follows the cluster ids, so relabelling the assets can reverse a subtree and move where
//! a list is cut in half: the same covariance in another order may get other weights. Every other
//! portfolio of this crate is permutation-equivariant (`tests/properties.rs`).
//!
//! **The clusters** ([`clusters_by_silhouette`]): the tree cut into `k` clusters for each `k` from 2
//! to a maximum, keeping the cut whose silhouette scores have the highest mean over standard
//! deviation, the quality *Machine Learning for Asset Managers* ranks its clusterings by (snippet 4.1).
//! The book draws its clusterings from randomly started k-means; this crate cuts one deterministic
//! tree instead, so the same matrix always gives the same clusters.

use vike_math::linear_algebra::{Matrix, correlation_from_covariance, dot, squared_distance};
use vike_math::statistics::{correlation_distance, mean_variance};

use crate::risk_parity::inverse_variance;
use crate::{assets, product, submatrix};

/// The Hierarchical Risk Parity weights of `covariance`.
///
/// `None` when `covariance` is not square, holds a non-finite entry, or has a variance that is not
/// positive (no correlation exists for it).
pub fn hierarchical_risk_parity(covariance: &Matrix) -> Option<Vec<f64>> {
    let n = assets(covariance)?;
    let order = quasi_diagonal_order(&correlation_from_covariance(covariance)?)?;
    let mut weights = vec![1.0; n];
    let mut clusters = vec![order];
    while !clusters.is_empty() {
        clusters = clusters
            .into_iter()
            .filter(|cluster| cluster.len() > 1)
            .flat_map(|cluster| {
                let (first, second) = cluster.split_at(cluster.len() / 2);
                [first.to_vec(), second.to_vec()]
            })
            .collect();
        for pair in clusters.chunks(2) {
            let first = cluster_variance(covariance, &pair[0])?;
            let second = cluster_variance(covariance, &pair[1])?;
            let share = 1.0 - first / (first + second);
            for &i in &pair[0] {
                weights[i] *= share;
            }
            for &i in &pair[1] {
                weights[i] *= 1.0 - share;
            }
        }
    }
    Some(weights)
}

/// The assets in the order of the single-linkage tree's leaves (the order that gathers correlated
/// assets along the diagonal).
///
/// `None` when `correlation` is not square or holds a non-finite entry.
pub fn quasi_diagonal_order(correlation: &Matrix) -> Option<Vec<usize>> {
    let n = assets(correlation)?;
    let merges = single_linkage(&distance_of_distances(correlation), n);
    let mut order = Vec::with_capacity(n);
    let root = 2 * n - 2;
    leaves(root, n, &merges, &mut order);
    Some(order)
}

/// The partition of the assets that the single-linkage tree cut into `k ∈ 2..=max_clusters` clusters
/// gives the highest silhouette quality (mean over population standard deviation; scikit-learn's
/// silhouette, `0` for a lone member). The first `k` wins a tie. Each cluster lists its assets in
/// index order, and the clusters come in the order of their first asset.
///
/// `None` when `correlation` is not square or holds a non-finite entry, and when no `k` is admissible:
/// a silhouette needs `2 ≤ k ≤ n − 1`, so fewer than three assets or a `max_clusters` below 2 have
/// none.
pub fn clusters_by_silhouette(
    correlation: &Matrix,
    max_clusters: usize,
) -> Option<Vec<Vec<usize>>> {
    let n = assets(correlation)?;
    let distances = distance_of_distances(correlation);
    let merges = single_linkage(&distances, n);
    let mut best: Option<(f64, Vec<usize>)> = None;
    for k in 2..=max_clusters.min(n.saturating_sub(1)) {
        let labels = cut(&merges, n, k);
        let quality = silhouette_quality(&distances, &labels, k);
        if best.as_ref().map_or(!quality.is_nan(), |(most, _)| quality > *most) {
            best = Some((quality, labels));
        }
    }
    let (_, labels) = best?;
    Some(clusters_of(&labels))
}

/// The partition `labels` gives (one label per asset): each cluster lists its assets in index order,
/// and the clusters come in the order of their first asset, whatever the labels' own numbers.
pub(crate) fn clusters_of(labels: &[usize]) -> Vec<Vec<usize>> {
    let mut seen: Vec<usize> = Vec::new();
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for (asset, &label) in labels.iter().enumerate() {
        match seen.iter().position(|&s| s == label) {
            Some(index) => clusters[index].push(asset),
            None => {
                seen.push(label);
                clusters.push(vec![asset]);
            }
        }
    }
    clusters
}

/// `d̃`, row-major `n × n`: the Euclidean distance between the columns of `d = √((1 − ρ) / 2)`.
pub(crate) fn distance_of_distances(correlation: &Matrix) -> Vec<f64> {
    let n = correlation.rows();
    // Row `i` of `columns` is column `i` of `d`, so each pair of columns is two contiguous slices.
    let columns: Vec<f64> =
        correlation.transpose().values().iter().map(|&rho| correlation_distance(rho)).collect();
    let column = |i: usize| &columns[i * n..(i + 1) * n];
    let mut out = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..i {
            out[i * n + j] = squared_distance(column(i), column(j)).sqrt();
            out[j * n + i] = out[i * n + j];
        }
    }
    out
}

/// Single linkage over `distances` (row-major `n × n`): the `n − 1` merges in order, each as its two
/// cluster ids, the lower first; the `m`-th merge makes cluster `n + m`.
fn single_linkage(distances: &[f64], n: usize) -> Vec<(usize, usize)> {
    // `slot` holds one live cluster per row of `between`, the single-linkage distances among them.
    let mut slot: Vec<Option<usize>> = (0..n).map(Some).collect();
    let mut between = distances.to_vec();
    let mut merges = Vec::with_capacity(n.saturating_sub(1));
    for m in 0..n.saturating_sub(1) {
        let mut closest: Option<(f64, usize, usize)> = None;
        for a in 0..n {
            for b in a + 1..n {
                if slot[a].is_some()
                    && slot[b].is_some()
                    && closest.is_none_or(|(least, _, _)| between[a * n + b] < least)
                {
                    closest = Some((between[a * n + b], a, b));
                }
            }
        }
        // Two live clusters remain while merges are left, so a pair is always found.
        let Some((_, a, b)) = closest else { break };
        let (Some(left), Some(right)) = (slot[a], slot[b]) else { break };
        merges.push((left.min(right), left.max(right)));
        for c in 0..n {
            let nearer = f64::min(between[a * n + c], between[b * n + c]);
            between[a * n + c] = nearer;
            between[c * n + a] = nearer;
        }
        slot[a] = Some(n + m);
        slot[b] = None;
    }
    merges
}

/// Append cluster `id`'s leaves, its first child's before its second's.
fn leaves(id: usize, n: usize, merges: &[(usize, usize)], out: &mut Vec<usize>) {
    if id < n {
        out.push(id);
    } else {
        let (first, second) = merges[id - n];
        leaves(first, n, merges, out);
        leaves(second, n, merges, out);
    }
}

/// Each asset's cluster after the first `n − k` merges, numbered by first appearance in asset order.
fn cut(merges: &[(usize, usize)], n: usize, k: usize) -> Vec<usize> {
    let mut cluster: Vec<usize> = (0..n).collect();
    for (m, &(left, right)) in merges.iter().take(n - k).enumerate() {
        for id in &mut cluster {
            if *id == left || *id == right {
                *id = n + m;
            }
        }
    }
    let mut seen: Vec<usize> = Vec::with_capacity(k);
    cluster
        .iter()
        .map(|id| {
            seen.iter().position(|s| s == id).unwrap_or_else(|| {
                seen.push(*id);
                seen.len() - 1
            })
        })
        .collect()
}

/// The mean of the assets' silhouettes over their population standard deviation.
pub(crate) fn silhouette_quality(distances: &[f64], labels: &[usize], k: usize) -> f64 {
    let n = labels.len();
    let silhouettes: Vec<f64> = (0..n)
        .map(|i| {
            let mut totals = vec![0.0; k];
            let mut sizes = vec![0usize; k];
            for (j, &label) in labels.iter().enumerate() {
                if j != i {
                    totals[label] += distances[i * n + j];
                    sizes[label] += 1;
                }
            }
            let own = labels[i];
            if sizes[own] == 0 {
                return 0.0;
            }
            let within = totals[own] / sizes[own] as f64;
            let nearest = (0..k)
                .filter(|&c| c != own && sizes[c] > 0)
                .map(|c| totals[c] / sizes[c] as f64)
                .fold(f64::INFINITY, f64::min);
            let larger = f64::max(within, nearest);
            if larger > 0.0 { (nearest - within) / larger } else { 0.0 }
        })
        .collect();
    let (mean, spread) = mean_variance(silhouettes.iter().copied(), 0.0);
    mean / spread.sqrt()
}

/// A cluster's variance at its inverse-variance weights, `wᵀΣw` (López de Prado's `getClusterVar`).
fn cluster_variance(covariance: &Matrix, items: &[usize]) -> Option<f64> {
    let block = submatrix(covariance, items)?;
    let weights = inverse_variance(&block)?;
    Some(dot(&weights, &product(&block, &weights)?))
}

#[cfg(test)]
#[path = "hierarchical_tests.rs"]
mod tests;
