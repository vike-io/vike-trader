use super::*;

fn matrix(rows: usize, columns: usize, values: &[f64]) -> Matrix {
    Matrix::from_row_major(rows, columns, values.to_vec()).unwrap()
}

/// Four points, two tight pairs. The mean is `(5, 5.5)`; the farthest points from it are `(0, 0)` and
/// `(10, 11)` at the same squared distance 55.25 (the first index wins), so the centres start at `(0, 0)`
/// and then at the point farthest from it, `(10, 11)`. The first pass puts `(0, 1)` with `(0, 0)` and
/// `(10, 10)` with `(10, 11)`; the centres move to `(0, 0.5)` and `(10, 10.5)`; the second pass changes
/// nothing. Each point is 0.5 from its centre, so the inertia is `4 · 0.25 = 1`.
#[test]
fn two_tight_pairs_by_hand() {
    let points = matrix(4, 2, &[0.0, 0.0, 0.0, 1.0, 10.0, 10.0, 10.0, 11.0]);
    let clustering = kmeans(&points, 2).unwrap();
    assert_eq!(clustering.labels, vec![0, 0, 1, 1]);
    assert_eq!(clustering.centres, matrix(2, 2, &[0.0, 0.5, 10.0, 10.5]));
    assert_eq!(clustering.inertia, 1.0);
    assert_eq!(clustering.iterations, 2);
}

/// One cluster is the whole set around its mean: `[0, 1, 5]` has mean 2 and inertia `4 + 1 + 9 = 14`.
/// As many clusters as points leave nothing to explain: each point is its own centre.
#[test]
fn one_cluster_and_as_many_clusters_as_points() {
    let points = matrix(3, 1, &[0.0, 1.0, 5.0]);
    let one = kmeans(&points, 1).unwrap();
    assert_eq!(one.labels, vec![0, 0, 0]);
    assert_eq!(one.centres, matrix(1, 1, &[2.0]));
    assert_eq!(one.inertia, 14.0);
    let all = kmeans(&points, 3).unwrap();
    assert_eq!(all.inertia, 0.0);
    let mut labels = all.labels.clone();
    labels.sort_unstable();
    assert_eq!(labels, vec![0, 1, 2]);
}

#[test]
fn no_answer_without_points_or_with_an_impossible_count() {
    let points = matrix(3, 1, &[0.0, 1.0, 5.0]);
    assert!(kmeans(&points, 0).is_none());
    assert!(kmeans(&points, 4).is_none());
    assert!(kmeans(&matrix(3, 1, &[0.0, f64::NAN, 5.0]), 2).is_none());
    assert!(kmeans(&Matrix::from_row_major(0, 0, Vec::new()).unwrap(), 1).is_none());
}

/// Equal points make a centre with no points of its own; it stays and the answer still exists.
#[test]
fn identical_points_leave_a_centre_empty_without_failing() {
    let points = matrix(3, 1, &[2.0, 2.0, 2.0]);
    let clustering = kmeans(&points, 2).unwrap();
    assert_eq!(clustering.inertia, 0.0);
    assert_eq!(clustering.labels, vec![0, 0, 0]);
}

/// The hierarchical module's two pairs (`ρ₀₂ = 0.9`, `ρ₁₃ = 0.8`, nothing across): the silhouette quality
/// of the two-cluster cut is about 12 (silhouettes 0.73 and 0.62), against about 1 for three clusters
/// (two lone assets score 0), so k-means on the rows of `d` returns the two pairs, as the tree does.
#[test]
fn clusters_two_correlated_pairs() {
    let correlation = matrix(
        4,
        4,
        &[1.0, 0.0, 0.9, 0.0, 0.0, 1.0, 0.0, 0.8, 0.9, 0.0, 1.0, 0.0, 0.0, 0.8, 0.0, 1.0],
    );
    assert_eq!(kmeans_clusters(&correlation, 3).unwrap(), vec![vec![0, 2], vec![1, 3]]);
    // The same answer however the assets are numbered: relabel 0, 1, 2, 3 as 3, 2, 1, 0.
    let reversed = matrix(
        4,
        4,
        &[1.0, 0.0, 0.8, 0.0, 0.0, 1.0, 0.0, 0.9, 0.8, 0.0, 1.0, 0.0, 0.0, 0.9, 0.0, 1.0],
    );
    assert_eq!(kmeans_clusters(&reversed, 3).unwrap(), vec![vec![0, 2], vec![1, 3]]);
}

#[test]
fn no_clusters_without_an_admissible_count() {
    let pair = matrix(2, 2, &[1.0, 0.5, 0.5, 1.0]);
    assert!(kmeans_clusters(&pair, 5).is_none(), "a silhouette needs three assets");
    let three = matrix(3, 3, &[1.0, 0.9, 0.0, 0.9, 1.0, 0.0, 0.0, 0.0, 1.0]);
    assert!(kmeans_clusters(&three, 1).is_none());
    assert!(kmeans_clusters(&matrix(2, 2, &[1.0, f64::NAN, 0.0, 1.0]), 3).is_none());
}
