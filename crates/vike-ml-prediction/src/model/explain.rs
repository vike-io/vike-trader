//! Exact TreeSHAP: one prediction split into one additive contribution per feature, plus the
//! model's expected output (Lundberg, Erion & Lee, *Consistent Individualized Feature Attribution
//! for Tree Ensembles*, arXiv:1802.03888, Algorithm 2).
//!
//! # What is explained, and in which space
//!
//! The RAW score — the sum of leaf values, before the sigmoid or the softmax — exactly as
//! LightGBM's `predict_contrib` explains it. For every output `k` (one for binary and regression,
//! K for multiclass):
//!
//! `phi[k][0] + … + phi[k][n - 1] + expected[k] == raw_score[k]`, up to float rounding.
//!
//! A probability is not additive in the features, so it is not what the numbers sum to; a caller
//! that wants the probability of the explained row asks [`GbdtModel::predict_into`] beside this.
//!
//! # Exact, and LightGBM's own algorithm step for step
//!
//! "Exact" means the Shapley values of the PATH-DEPENDENT value function: a coalition `S` of
//! features is worth the tree's output with every feature outside `S` integrated out by the
//! training-row COVERS (`leaf_count`/`internal_count`: at a split on a feature outside `S` both
//! branches count, weighted by how many training rows took each). No sampling, no background data
//! set, and polynomial time: `O(leaves × depth²)` per tree.
//!
//! Every routine below is LightGBM v4.7.0's `src/io/tree.cpp` with the same operation order:
//! [`extend_path`] its `ExtendPath`, [`unwind_path`] its `UnwindPath`, [`unwound_path_sum`] its
//! `UnwoundPathSum`, `Tree::tree_shap` its `TreeSHAP`, `Tree::expected_value` its `ExpectedValue`,
//! and [`GbdtModel::shap_values_into`] `GBDT::PredictContrib`. The hot branch is the walker's own
//! decision (`Tree::decide`), so a NaN, a missing value or a category set routes exactly as a
//! prediction does. `crates/vike-ml-training/tests/train_infer_equality.rs`'s
//! `the_shap_values_match_lightgbms_own_contributions` compares the result against LightGBM's
//! `predict_contrib=true` on a real model, where the binary is.
//!
//! # The output layout
//!
//! `predict_contrib`'s: one block per output, each `n_features + 1` wide — the contribution of
//! every feature column in order, then the expected value. `n_features` is the MODEL's width
//! ([`GbdtModel::n_features_expected`]); a feature no tree splits on contributes exactly `0.0`.
//!
//! # Refusals
//!
//! A tree whose covers are absent or malformed (the parser keeps them leniently, see
//! [`crate::model::parse`]'s `covers`), hold a zero, or do not add up (an internal node's count
//! is not its two children's sum) is REFUSED with [`MlError::Unsupported`] naming the tree: the
//! zero makes a branch fraction `0 / 0`, and covers that do not add up break the additivity above,
//! so either would be a plausible-looking wrong attribution. LightGBM itself checks neither.

use crate::error::MlError;
use crate::model::{GbdtModel, Tree};

/// One entry of the unique path: LightGBM's `Tree::PathElement`.
#[derive(Clone, Copy, Debug)]
struct PathElement {
    /// The feature this entry splits on; `None` for the root's placeholder (LightGBM's `-1`).
    feature: Option<u32>,
    /// The fraction of the paths WITHOUT this feature that flow down this branch: its cover ratio.
    zero_fraction: f64,
    /// `1` when the row's own value takes this branch, `0` when it does not.
    one_fraction: f64,
    /// The permutation weight of the paths with this entry's index of ones in them. Not tied to
    /// the other three fields, as in LightGBM: [`unwind_path`] moves those and leaves this.
    path_weight: f64,
}

const UNUSED: PathElement =
    PathElement { feature: None, zero_fraction: 0.0, one_fraction: 0.0, path_weight: 0.0 };

/// The recursion's shared state: the row, the contribution block being written, and the unique
/// path buffer every level carves its own region out of.
struct Walk<'a> {
    row: &'a [f64],
    phi: &'a mut [f64],
    path: &'a mut [PathElement],
}

impl GbdtModel {
    /// The SHAP values of `row`, in `predict_contrib`'s layout (module doc): `out` holds
    /// [`crate::model::Objective::output_count`] blocks of `n_features_expected() + 1` numbers.
    ///
    /// Each block sums to that output's raw score. The row width is unchecked in release, as in
    /// [`GbdtModel::raw_scores_into`]; [`GbdtModel::shap_values_batch`] is the checked entry point.
    pub fn shap_values_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), MlError> {
        debug_assert!(
            row.len() >= self.n_features_expected(),
            "row is {} wide; this model splits on feature {} so it needs at least {}",
            row.len(),
            self.max_feature_index,
            self.n_features_expected()
        );
        let n_features = self.n_features_expected();
        let outputs = self.objective.output_count();
        let width = n_features + 1;
        if outputs == 0 || out.len() != outputs * width {
            return Err(MlError::Shape(format!(
                "this model explains {outputs} output(s) of {width} numbers each per row; the \
                 output buffer holds {}",
                out.len()
            )));
        }
        let mut depth = 0;
        for (index, tree) in self.trees.iter().enumerate() {
            depth = depth.max(tree.explainable_depth(index, n_features)?);
        }
        // LightGBM's `max_path_len * (max_path_len + 1) / 2` with `max_path_len = depth + 1`: every
        // level of the recursion copies its parent's path into a fresh region of this buffer.
        let mut path = vec![UNUSED; (depth + 1) * (depth + 2) / 2];
        out.fill(0.0);
        for (index, tree) in self.trees.iter().enumerate() {
            let block = &mut out[(index % outputs) * width..][..width];
            tree.contributions_into(row, block, &mut path);
        }
        Ok(())
    }

    /// [`GbdtModel::shap_values_into`] over a row-major matrix, with the width check the
    /// single-row call skips: one `predict_contrib` line per row, rows concatenated.
    pub fn shap_values_batch(
        &self,
        flat_x: &[f64],
        n_features: usize,
    ) -> Result<Vec<f64>, MlError> {
        let width = self.objective.output_count() * (self.n_features_expected() + 1);
        let mut out = Vec::with_capacity(flat_x.len() / n_features.max(1) * width);
        for row in self.rows(flat_x, n_features)? {
            let start = out.len();
            out.resize(start + width, 0.0);
            self.shap_values_into(row, &mut out[start..])?;
        }
        Ok(out)
    }
}

impl Tree {
    /// How many training rows reached `node`: LightGBM's `data_count`. Valid only after
    /// [`Tree::explainable_depth`] accepted the tree.
    fn cover(&self, node: i32) -> u32 {
        if node >= 0 { self.internal_count[node as usize] } else { self.leaf_count[!node as usize] }
    }

    /// The depth of this tree's deepest leaf (root to leaf, in edges), once its covers are proven
    /// usable — the module doc's refusals, each naming tree `index`.
    fn explainable_depth(&self, index: usize, n_features: usize) -> Result<usize, MlError> {
        if self.num_leaves == 1 {
            return Ok(0);
        }
        let refuse = |what: String| {
            MlError::Unsupported(format!("tree {index} cannot be explained by TreeSHAP: {what}"))
        };
        let n_internal = self.num_leaves - 1;
        if self.leaf_count.len() != self.num_leaves || self.internal_count.len() != n_internal {
            return Err(refuse(format!(
                "its `leaf_count`/`internal_count` covers are missing or malformed (it needs \
                 {} and {n_internal} counts) — the weights that integrate an absent feature out",
                self.num_leaves
            )));
        }
        if let Some(leaf) = self.leaf_count.iter().position(|&count| count == 0) {
            return Err(refuse(format!(
                "leaf {leaf} has a zero cover, so the branch fraction leading to it is 0 / 0"
            )));
        }
        // `check_children` proved every internal child is numbered above its parent, so one
        // forward pass sees each node's depth before its children need it.
        let mut depth = vec![0usize; n_internal];
        let mut deepest = 0;
        for node in 0..n_internal {
            if self.split_feature[node] as usize >= n_features {
                return Err(refuse(format!(
                    "node {node} splits on feature {} but the model declares {n_features}",
                    self.split_feature[node]
                )));
            }
            let mut children = 0u64;
            for child in [self.left_child[node], self.right_child[node]] {
                children += u64::from(self.cover(child));
                if child >= 0 {
                    depth[child as usize] = depth[node] + 1;
                } else {
                    deepest = deepest.max(depth[node] + 1);
                }
            }
            let count = self.internal_count[node];
            if count == 0 || children != u64::from(count) {
                return Err(refuse(format!(
                    "internal node {node} covers {count} rows but its two children cover \
                     {children}; covers that do not add up break the additivity of the attribution"
                )));
            }
        }
        Ok(deepest)
    }

    /// LightGBM's `Tree::ExpectedValue`: the leaf values averaged by cover — the tree's output
    /// when no feature is known.
    fn expected_value(&self) -> f64 {
        if self.num_leaves == 1 {
            return self.leaf_value[0];
        }
        let total = f64::from(self.internal_count[0]);
        let mut value = 0.0;
        for (&count, &leaf) in self.leaf_count.iter().zip(&self.leaf_value) {
            value += (f64::from(count) / total) * leaf;
        }
        value
    }

    /// LightGBM's `Tree::PredictContrib`: the expected value into the block's last entry, then the
    /// recursion's contributions into the rest.
    fn contributions_into(&self, row: &[f64], block: &mut [f64], path: &mut [PathElement]) {
        let bias = block.len() - 1;
        block[bias] += self.expected_value();
        if self.num_leaves > 1 {
            let root = PathElement {
                feature: None,
                zero_fraction: 1.0,
                one_fraction: 1.0,
                path_weight: 0.0,
            };
            self.tree_shap(&mut Walk { row, phi: block, path }, 0, 0, 0, root);
        }
    }

    /// LightGBM's `Tree::TreeSHAP`. The node's region of the path buffer starts at
    /// `parent_start + unique_depth` and first receives a copy of the parent's `unique_depth`
    /// entries; `incoming` is the entry this node's branch extends it with.
    fn tree_shap(
        &self,
        walk: &mut Walk<'_>,
        node: i32,
        parent_start: usize,
        mut unique_depth: usize,
        incoming: PathElement,
    ) {
        let start = parent_start + unique_depth;
        if unique_depth > 0 {
            walk.path.copy_within(parent_start..parent_start + unique_depth, start);
        }
        extend_path(&mut walk.path[start..], unique_depth, incoming);

        if node < 0 {
            let leaf = self.leaf_value[!node as usize];
            let unique = &walk.path[start..];
            for (i, element) in unique.iter().enumerate().take(unique_depth + 1).skip(1) {
                let weight = unwound_path_sum(unique, unique_depth, i);
                // Every entry past the root's placeholder names a feature.
                if let Some(feature) = element.feature {
                    walk.phi[feature as usize] +=
                        weight * (element.one_fraction - element.zero_fraction) * leaf;
                }
            }
            return;
        }

        let at = node as usize;
        let hot = self.decide(at, walk.row);
        let cold =
            if hot == self.left_child[at] { self.right_child[at] } else { self.left_child[at] };
        let w = f64::from(self.cover(node));
        let hot_zero_fraction = f64::from(self.cover(hot)) / w;
        let cold_zero_fraction = f64::from(self.cover(cold)) / w;
        let mut incoming_zero_fraction = 1.0;
        let mut incoming_one_fraction = 1.0;

        // A feature already on the path is split again: undo its earlier extension and redo it
        // here, so a feature counts once per path however often the tree tests it.
        let feature = self.split_feature[at];
        if let Some(path_index) =
            (0..=unique_depth).find(|&i| walk.path[start + i].feature == Some(feature))
        {
            incoming_zero_fraction = walk.path[start + path_index].zero_fraction;
            incoming_one_fraction = walk.path[start + path_index].one_fraction;
            unwind_path(&mut walk.path[start..], unique_depth, path_index);
            unique_depth -= 1;
        }

        let hot_entry = PathElement {
            feature: Some(feature),
            zero_fraction: hot_zero_fraction * incoming_zero_fraction,
            one_fraction: incoming_one_fraction,
            path_weight: 0.0,
        };
        self.tree_shap(walk, hot, start, unique_depth + 1, hot_entry);
        let cold_entry = PathElement {
            feature: Some(feature),
            zero_fraction: cold_zero_fraction * incoming_zero_fraction,
            one_fraction: 0.0,
            path_weight: 0.0,
        };
        self.tree_shap(walk, cold, start, unique_depth + 1, cold_entry);
    }
}

/// LightGBM's `Tree::ExtendPath`: append `incoming` at `unique_depth` and re-weigh every shorter
/// permutation for one more feature on the path.
fn extend_path(unique: &mut [PathElement], unique_depth: usize, incoming: PathElement) {
    unique[unique_depth] =
        PathElement { path_weight: if unique_depth == 0 { 1.0 } else { 0.0 }, ..incoming };
    let denominator = (unique_depth + 1) as f64;
    for i in (0..unique_depth).rev() {
        unique[i + 1].path_weight +=
            incoming.one_fraction * unique[i].path_weight * (i + 1) as f64 / denominator;
        unique[i].path_weight =
            incoming.zero_fraction * unique[i].path_weight * (unique_depth - i) as f64
                / denominator;
    }
}

/// LightGBM's `Tree::UnwindPath`: undo [`extend_path`] for the entry at `path_index`, then close
/// the gap it leaves (the weights stay where they are; only the three entry fields move).
fn unwind_path(unique: &mut [PathElement], unique_depth: usize, path_index: usize) {
    let one_fraction = unique[path_index].one_fraction;
    let zero_fraction = unique[path_index].zero_fraction;
    let mut next_one_portion = unique[unique_depth].path_weight;
    let denominator = (unique_depth + 1) as f64;
    for i in (0..unique_depth).rev() {
        if one_fraction != 0.0 {
            let tmp = unique[i].path_weight;
            unique[i].path_weight =
                next_one_portion * denominator / ((i + 1) as f64 * one_fraction);
            next_one_portion = tmp
                - unique[i].path_weight * zero_fraction * (unique_depth - i) as f64 / denominator;
        } else {
            unique[i].path_weight =
                (unique[i].path_weight * denominator) / (zero_fraction * (unique_depth - i) as f64);
        }
    }
    for i in path_index..unique_depth {
        let next = unique[i + 1];
        unique[i] = PathElement { path_weight: unique[i].path_weight, ..next };
    }
}

/// LightGBM's `Tree::UnwoundPathSum`: the total permutation weight of the path with the entry at
/// `path_index` unwound, without unwinding it.
fn unwound_path_sum(unique: &[PathElement], unique_depth: usize, path_index: usize) -> f64 {
    let one_fraction = unique[path_index].one_fraction;
    let zero_fraction = unique[path_index].zero_fraction;
    let mut next_one_portion = unique[unique_depth].path_weight;
    let denominator = (unique_depth + 1) as f64;
    let mut total = 0.0;
    for i in (0..unique_depth).rev() {
        if one_fraction != 0.0 {
            let tmp = next_one_portion * denominator / ((i + 1) as f64 * one_fraction);
            total += tmp;
            next_one_portion = unique[i].path_weight
                - tmp * zero_fraction * ((unique_depth - i) as f64 / denominator);
        } else {
            total +=
                (unique[i].path_weight / zero_fraction) / ((unique_depth - i) as f64 / denominator);
        }
    }
    total
}

#[path = "explain_tests.rs"]
#[cfg(test)]
mod explain_tests;
