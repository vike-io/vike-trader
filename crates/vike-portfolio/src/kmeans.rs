//! k-means clustering (Lloyd, "Least squares quantization in PCM", 1982), the second way this crate
//! groups assets beside the single-linkage tree of [`crate::hierarchical`]: the one k-means in the
//! workspace, with the book's way of choosing `k` by the silhouette quality (López de Prado, *Machine
//! Learning for Asset Managers*, 2020, snippet 4.1, `clusterKMeansBase`).
//!
//! **Deterministic**, as every function of this crate is: no random start. The first centre is the
//! point farthest from the mean of all of them and every next centre the point farthest from the
//! centres chosen so far (the farthest-first traversal, Gonzalez 1985; the first index wins a tie),
//! so the same points always give the same clusters. A randomly started k-means, as scikit-learn
//! runs it, finds other local optima on other seeds; this one finds one, and
//! [`kmeans_clusters`] cannot be reseeded to look for a better partition. A caller that wants the
//! restarts supplies its own starts: there is no randomness to supply them from here.
//!
//! **Lloyd's iteration** alternates assigning each point to its nearest centre (the lowest index wins a
//! tie) and moving each centre to the mean of its points, until no assignment changes or
//! [`MAXIMUM_ITERATIONS`] passes have run. A centre whose points all left stays where it is.
//!
//! [`kmeans_clusters`] clusters ASSETS from a correlation matrix the way the book does: each asset's row
//! of `d = √((1 − ρ) / 2)` is its observation, and `k` is the count from 2 to `max_clusters` with the
//! highest silhouette quality ([`crate::hierarchical::silhouette_quality`], the quality
//! [`crate::hierarchical::clusters_by_silhouette`] ranks its cuts by).
//!
//! Every fold runs in index order, a naive left fold but the starting mean (compensated:
//! `vike_math::linear_algebra::column_means`), and the only transcendental is `sqrt`, so a result
//! is the same bits on every box.

use vike_math::linear_algebra::{Matrix, column_means, squared_distance};
use vike_math::statistics::correlation_distance;

use crate::assets;
use crate::hierarchical::{clusters_of, distance_of_distances, silhouette_quality};

/// The most Lloyd passes one clustering runs. Lloyd's iteration never revisits an assignment, so it
/// stops far sooner on any points this workspace holds; reaching the bound still returns the
/// assignment the last pass made.
pub const MAXIMUM_ITERATIONS: usize = 300;

/// A k-means clustering, from [`kmeans`].
#[derive(Clone, Debug, PartialEq)]
pub struct KMeans {
    /// The cluster of each point, in `0..k`.
    pub labels: Vec<usize>,
    /// `k × dimensions`: row `c` is cluster `c`'s centre.
    pub centres: Matrix,
    /// The sum over the points of the squared distance to their centre.
    pub inertia: f64,
    /// The Lloyd passes that ran.
    pub iterations: usize,
}

/// The index of the centre nearest `point` among the rows of `centres` (`dimensions` wide); the
/// lowest index wins a tie.
fn nearest(point: &[f64], centres: &[f64], dimensions: usize) -> usize {
    let mut best = (f64::INFINITY, 0);
    for (c, centre) in centres.chunks_exact(dimensions).enumerate() {
        let distance = squared_distance(point, centre);
        if distance < best.0 {
            best = (distance, c);
        }
    }
    best.1
}

/// The `k` starting centres by the farthest-first traversal: row indices in the order chosen.
fn starting_centres(points: &Matrix, k: usize) -> Vec<usize> {
    let n = points.rows();
    let mean = column_means(points);
    // Each point's squared distance to the nearest centre chosen so far; the mean stands in for the
    // first round, so the first centre is the point farthest from the mean.
    let mut reach: Vec<f64> = (0..n).map(|i| squared_distance(points.row(i), &mean)).collect();
    let mut chosen = Vec::with_capacity(k);
    while chosen.len() < k {
        let mut farthest = (-1.0, 0);
        for (i, &distance) in reach.iter().enumerate() {
            if distance > farthest.0 {
                farthest = (distance, i);
            }
        }
        chosen.push(farthest.1);
        for (i, distance) in reach.iter_mut().enumerate() {
            let to_new = squared_distance(points.row(i), points.row(farthest.1));
            if chosen.len() == 1 || to_new < *distance {
                *distance = to_new;
            }
        }
    }
    chosen
}

/// Cluster the rows of `points` into `k` clusters by Lloyd's iteration from the farthest-first
/// centres (see the module doc).
///
/// `None` when there are no points, `k` is zero or above the number of points, or an entry is not
/// finite.
pub fn kmeans(points: &Matrix, k: usize) -> Option<KMeans> {
    let (n, dimensions) = (points.rows(), points.columns());
    if n == 0
        || dimensions == 0
        || k == 0
        || k > n
        || points.values().iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let mut centres: Vec<f64> = Vec::with_capacity(k * dimensions);
    for index in starting_centres(points, k) {
        centres.extend_from_slice(points.row(index));
    }
    let mut labels = vec![usize::MAX; n];
    let mut iterations = 0;
    for pass in 1..=MAXIMUM_ITERATIONS {
        iterations = pass;
        let mut moved = false;
        for (i, label) in labels.iter_mut().enumerate() {
            let c = nearest(points.row(i), &centres, dimensions);
            if *label != c {
                *label = c;
                moved = true;
            }
        }
        if !moved {
            break;
        }
        let mut sums = vec![0.0; k * dimensions];
        let mut counts = vec![0usize; k];
        for (i, &label) in labels.iter().enumerate() {
            counts[label] += 1;
            for (s, v) in
                sums[label * dimensions..(label + 1) * dimensions].iter_mut().zip(points.row(i))
            {
                *s += v;
            }
        }
        for (c, &count) in counts.iter().enumerate() {
            if count > 0 {
                for d in 0..dimensions {
                    centres[c * dimensions + d] = sums[c * dimensions + d] / count as f64;
                }
            }
        }
    }
    let mut inertia = 0.0;
    for (i, &label) in labels.iter().enumerate() {
        inertia +=
            squared_distance(points.row(i), &centres[label * dimensions..(label + 1) * dimensions]);
    }
    Some(KMeans {
        labels,
        centres: Matrix::from_row_major(k, dimensions, centres)?,
        inertia,
        iterations,
    })
}

/// The partition of the assets that k-means into `k ∈ 2..=max_clusters` clusters (on each asset's
/// row of `d = √((1 − ρ) / 2)`) gives the highest silhouette quality; the first `k` wins a tie. Each
/// cluster lists its assets in index order, and the clusters come in the order of their first asset,
/// as [`crate::hierarchical::clusters_by_silhouette`] returns them.
///
/// `None` when `correlation` is not square or holds a non-finite entry, and when no `k` is
/// admissible: a silhouette needs `2 ≤ k ≤ n − 1`.
pub fn kmeans_clusters(correlation: &Matrix, max_clusters: usize) -> Option<Vec<Vec<usize>>> {
    let n = assets(correlation)?;
    let distances = distance_of_distances(correlation);
    let observations: Vec<f64> =
        correlation.values().iter().map(|&rho| correlation_distance(rho)).collect();
    let observations = Matrix::from_row_major(n, n, observations)?;
    let mut best: Option<(f64, Vec<usize>)> = None;
    for k in 2..=max_clusters.min(n.saturating_sub(1)) {
        let labels = kmeans(&observations, k)?.labels;
        let quality = silhouette_quality(&distances, &labels, k);
        if best.as_ref().map_or(!quality.is_nan(), |(most, _)| quality > *most) {
            best = Some((quality, labels));
        }
    }
    let (_, labels) = best?;
    Some(clusters_of(&labels))
}

#[cfg(test)]
#[path = "kmeans_tests.rs"]
mod tests;
