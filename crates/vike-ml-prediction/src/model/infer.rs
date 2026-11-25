//! The tree walker — pure Rust, no native dependency, and the reason a live binary can predict
//! from a LightGBM model without a C++ toolchain.
//!
//! Every routing rule below was read out of LightGBM's `include/LightGBM/tree.h` (`Decision`,
//! `NumericalDecision`, `CategoricalDecision`, `GetLeafByMap`) rather than inferred, because each
//! one fails SILENTLY: the walk terminates whatever you do, at a different leaf, and the
//! probability that comes out looks like a probability.

use vike_math::special_functions::{logistic, softmax};

use crate::error::MlError;
use crate::model::{
    GbdtModel, MissingType, Objective, Tree, is_categorical, is_default_left, is_zero, missing_type,
};

impl Tree {
    /// The feature this node splits on, or `NaN` when the row does not carry it.
    ///
    /// ⚠ `NaN` is NOT what LightGBM itself substitutes for an absent entry — its dense-buffer fill
    /// (`Predictor`'s row buffer, `Tree::GetLeafByMap`) uses `0.0`. The two agree at a
    /// `NoMissing`/`Zero` node (which remaps `NaN` to `0.0` — see `numerical_decision`) but
    /// DIVERGE at a `missing_type == Nan` node. A too-narrow row is what the `debug_assert!` in
    /// [`GbdtModel::raw_scores_into`] catches in test/debug builds; the CHECKED entry points are
    /// the batch methods ([`crate::GbdtModel::predict_proba_batch`], `predict_batch`).
    fn feature_value(&self, node: usize, row: &[f64]) -> f64 {
        row.get(self.split_feature[node] as usize).copied().unwrap_or(f64::NAN)
    }

    /// One node's child, as the raw encoding: `>= 0` is another internal node, `< 0` is a leaf.
    /// [`crate::model::explain`]'s TreeSHAP takes its HOT branch from here, as LightGBM's
    /// `Tree::TreeSHAP` takes it from `Decision`, so an explanation follows the walk exactly.
    pub(crate) fn decide(&self, node: usize, row: &[f64]) -> i32 {
        let fval = self.feature_value(node, row);
        if is_categorical(self.decision_type[node]) {
            self.categorical_decision(node, fval)
        } else {
            self.numerical_decision(node, fval)
        }
    }

    /// LightGBM's `NumericalDecision`, verbatim in behaviour.
    ///
    /// Three rules, in this order, and the order is the whole thing:
    ///
    /// 1. A `NaN` at a node whose `missing_type` is NOT `NaN` is silently remapped to `0.0` and
    ///    then compared normally — NOT sent to the default side, which would diverge on every
    ///    `NoMissing`/`Zero` node.
    /// 2. A value that IS this node's missing value goes to the `default_left` side.
    /// 3. Otherwise `fval <= threshold` goes LEFT. It is `<=`, not `<`.
    fn numerical_decision(&self, node: usize, mut fval: f64) -> i32 {
        let dt = self.decision_type[node];
        let mt = missing_type(dt);
        if fval.is_nan() && mt != MissingType::Nan {
            fval = 0.0;
        }
        let is_missing =
            (mt == MissingType::Zero && is_zero(fval)) || (mt == MissingType::Nan && fval.is_nan());
        if is_missing {
            return if is_default_left(dt) {
                self.left_child[node]
            } else {
                self.right_child[node]
            };
        }
        if fval <= self.threshold[node] { self.left_child[node] } else { self.right_child[node] }
    }

    /// Walk this tree and return the `leaf_value` the row reaches.
    ///
    /// ⚠ The leaf index is the ONE'S COMPLEMENT of the terminal node value (`leaf = !node`), which
    /// is LightGBM's own `return ~node;`. A naive negation is off by one for every leaf and maps
    /// leaf 0 onto node 0 — the root — so the bug is a wrong answer, not a crash.
    ///
    /// ⚠ The returned value is ALREADY scaled by `learning_rate`; see [`crate::GbdtModel`]'s
    /// `raw_scores_into`.
    ///
    /// ⚠ Every index below is UNCHECKED, deliberately — this is the per-row live path — and it is
    /// total only because [`crate::model::parse`]'s `check_children` proved at LOAD time that every
    /// internal child is in range and numbered ABOVE its parent, so `node` strictly increases and
    /// cannot cycle. Do not weaken the parser's side of that bargain.
    pub fn leaf_value_for(&self, row: &[f64]) -> f64 {
        // A constant tree (`num_leaves == 1`, empty split arrays; `GBDT::TrainOneIter`'s
        // `AsConstantTree` arm) is legal and has nothing to walk.
        if self.split_feature.is_empty() {
            return self.leaf_value[0];
        }
        let mut node: i32 = 0;
        while node >= 0 {
            node = self.decide(node as usize, row);
        }
        self.leaf_value[!node as usize]
    }

    /// LightGBM's `CategoricalDecision` — a DIFFERENT function from the numerical one.
    ///
    /// ⚠ It never consults `default_left` or `missing_type` (same packed byte; only the numerical
    /// path reads them). A `NaN` or a negative category goes RIGHT, unconditionally.
    ///
    /// What it does do:
    ///
    /// 1. `int_fval = fval as i32` — truncation toward zero, matching C++'s `static_cast<int>`,
    ///    NOT rounding. Rust's `as` cast also saturates instead of invoking UB, so an absurd id
    ///    is a right turn rather than a crash.
    /// 2. `cat_index = threshold[node] as usize` — the node's threshold is an INDEX into
    ///    `cat_boundaries`, not a feature value.
    /// 3. Test bit `int_fval % 32` of word `cat_boundaries[cat_index] + int_fval / 32`. A word past
    ///    `cat_boundaries[cat_index + 1]` is implicitly zero — i.e. not in the set — which is how an
    ///    unseen category routes without an out-of-bounds read.
    ///
    /// `max_cat_to_onehot` and `max_cat_threshold` are TRAINING-time heuristics: a one-hot-style
    /// split serializes as this same bitset lookup. There is no second case to handle.
    fn categorical_decision(&self, node: usize, fval: f64) -> i32 {
        if fval.is_nan() {
            return self.right_child[node];
        }
        let int_fval = fval as i32;
        if int_fval < 0 {
            return self.right_child[node];
        }
        let cat_index = self.threshold[node] as usize;
        let (Some(&start), Some(&end)) =
            (self.cat_boundaries.get(cat_index), self.cat_boundaries.get(cat_index + 1))
        else {
            // Unreachable from a well-formed model (the parser checks `cat_boundaries.len() ==
            // category_count + 1`): "not in the set" rather than a panic in a live prediction.
            return self.right_child[node];
        };
        let word = int_fval as usize / 32;
        let index = start as usize + word;
        if index >= end as usize {
            return self.right_child[node];
        }
        match self.cat_threshold.get(index) {
            Some(bits) if (bits >> (int_fval as u32 % 32)) & 1 == 1 => self.left_child[node],
            _ => self.right_child[node],
        }
    }
}

impl GbdtModel {
    /// The pre-transform score of a SINGLE-OUTPUT model (binary, regression): the plain sum of
    /// every tree's selected `leaf_value`. A multiclass model has K scores, not one, so this is
    /// `NaN` for it — never a sum across classes; use [`GbdtModel::raw_scores_into`].
    pub fn raw_score(&self, row: &[f64]) -> f64 {
        let mut out = [0.0];
        match self.raw_scores_into(row, &mut out) {
            Ok(()) => out[0],
            Err(_) => f64::NAN,
        }
    }

    /// LightGBM's `GBDT::PredictRaw`: one pre-transform score per output into `out`, whose length
    /// must be [`Objective::output_count`]. Tree `t` adds to output `t % K` — the file holds K
    /// trees per iteration, class-minor — so each output is the plain sum of its own trees in file
    /// order, exactly LightGBM's `output[k] += models_[i * K + k]->Predict(..)`.
    ///
    /// # Three things this deliberately does not do
    ///
    /// * **It does not multiply by `shrinkage`.** LightGBM's `Tree::Shrinkage` scales
    ///   `leaf_value_` IN PLACE at training time; the `shrinkage=` field exists only so boosting
    ///   can RESUME. Applying it here scales every score by `learning_rate` squared.
    /// * **It does not add a base score.** `boost_from_average`'s init score is folded INTO THE
    ///   FIRST TREE (`GBDT::TrainOneIter`'s `new_tree->AddBias(init_score)`, or a constant tree
    ///   when the learner could not split), so it is already inside the trees this loop sums;
    ///   adding it again double-counts. (`AddBias` also forces `shrinkage_ = 1.0`, which is why a
    ///   real model's `Tree=0` carries `shrinkage=1`.)
    /// * **It does not use a compensated sum.** `vike_math::summation::compensated_sum` is MORE accurate than this
    ///   loop, which is exactly why it is wrong here: LightGBM accumulates
    ///   `score += leaf_value_[~node]` in a plain loop in file order, and
    ///   `tests/train_infer_equality.rs`'s `the_raw_scores_match_bit_for_bit` compares BIT FOR BIT.
    pub fn raw_scores_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), MlError> {
        // The row width is UNCHECKED in release (the per-tick live path); a too-narrow row would
        // give a plausible wrong probability (`feature_value`'s doc), so debug builds shout.
        debug_assert!(
            row.len() >= self.n_features_expected(),
            "row is {} wide; this model splits on feature {} so it needs at least {}",
            row.len(),
            self.max_feature_index,
            self.n_features_expected()
        );
        let outputs = self.objective.output_count();
        if outputs == 0 || out.len() != outputs {
            return Err(MlError::Shape(format!(
                "this model predicts {outputs} value(s) per row; the output buffer holds {}",
                out.len()
            )));
        }
        out.fill(0.0);
        for (index, tree) in self.trees.iter().enumerate() {
            out[index % outputs] += tree.leaf_value_for(row);
        }
        Ok(())
    }

    /// LightGBM's `GBDT::Predict`: [`GbdtModel::raw_scores_into`], then the objective's
    /// `ConvertOutput` — binary the sigmoid [`GbdtModel::predict_proba`] documents, regression
    /// nothing (the raw score IS the prediction), multiclass the softmax over the K scores. Both
    /// `exp`s are `libm::exp` (decision 0032). The sigmoid is the workspace's one
    /// `vike_math::special_functions::logistic`: its `-x` then `exp` is bit-identical to
    /// `exp(-sigmoid * raw)` (IEEE 754 rounds `-(a·b)` and `(-a)·b` the same), and the platform
    /// probe's hash row pins it.
    pub fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), MlError> {
        self.raw_scores_into(row, out)?;
        match self.objective {
            Objective::Binary { sigmoid } => out[0] = logistic(sigmoid * out[0]),
            Objective::Regression => {}
            Objective::Multiclass { .. } => softmax(out),
        }
        Ok(())
    }

    /// `predict_proba` for `objective=binary`: `1 / (1 + exp(-sigmoid * raw))`.
    ///
    /// The `sigmoid` is the one parsed from the header's `objective=` line, not an assumed 1.0.
    /// `is_unbalance` and `scale_pos_weight` change the TRAINING gradients only — whatever they
    /// did is already baked into the leaf values.
    ///
    /// ⚠ **The `exp` is `libm::exp`, NOT `f64::exp`**, because it is the one place a probability
    /// out of this crate could differ between two boxes: the `f64` method reaches the platform's
    /// libm (MSVC on the dev box, glibc on CI) and the two disagree in the last bit
    /// (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`; pinned
    /// by `crates/vike-ml-prediction/tests/libm_platform_probe.rs`). Everything under the `exp` — a plain `+`
    /// fold, comparisons, integer bit tests — is portable already.
    ///
    /// ⚠ Both gates over this function budget `1e-12` for "one `exp` through two libms":
    /// `crates/vike-ml-training/tests/train_infer_equality.rs`'s
    /// `the_pure_rust_walker_reproduces_the_scorer_it_replaces` (LightGBM's own `task=predict` still
    /// calls the PLATFORM's `exp`) and `crates/vike-ml-prediction/tests/real_model_golden.rs`'s
    /// `the_walker_reproduces_lightgbms_own_probabilities_to_a_stated_epsilon`. Do not widen either.
    ///
    /// A regression or multiclass model has no single probability, so this is `NaN` for it rather
    /// than a plausible number: use [`GbdtModel::predict_into`], which also holds the one sigmoid
    /// this delegates to.
    pub fn predict_proba(&self, row: &[f64]) -> f64 {
        let mut out = [f64::NAN];
        match self.objective {
            Objective::Binary { .. } if self.predict_into(row, &mut out).is_ok() => out[0],
            _ => f64::NAN,
        }
    }

    /// Row-major batch prediction, with the width check the single-row API deliberately skips.
    /// A non-binary model is refused: [`GbdtModel::predict_batch`] is its batch.
    pub fn predict_proba_batch(
        &self,
        flat_x: &[f64],
        n_features: usize,
    ) -> Result<Vec<f64>, MlError> {
        if !matches!(self.objective, Objective::Binary { .. }) {
            return Err(MlError::Unsupported(format!(
                "objective {:?} has no single probability per row; use `predict_batch`",
                self.objective
            )));
        }
        Ok(self.rows(flat_x, n_features)?.map(|row| self.predict_proba(row)).collect())
    }

    /// [`GbdtModel::predict_into`] over a row-major matrix: [`Objective::output_count`] values per
    /// row, row-major — LightGBM's own `task=predict` output, one line per row.
    pub fn predict_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, MlError> {
        self.batch(flat_x, n_features, Self::predict_into)
    }

    /// The pre-transform twin of [`GbdtModel::predict_batch`] (one value per row for a
    /// single-output model, K for multiclass) — what the equality gate compares against
    /// LightGBM's own `raw_scores`.
    pub fn raw_scores_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, MlError> {
        self.batch(flat_x, n_features, Self::raw_scores_into)
    }

    fn batch(&self, flat_x: &[f64], n_features: usize, each: PerRow) -> Result<Vec<f64>, MlError> {
        let rows = self.rows(flat_x, n_features)?;
        let outputs = self.objective.output_count();
        let mut out = Vec::with_capacity(flat_x.len() / n_features * outputs);
        for row in rows {
            let start = out.len();
            out.resize(start + outputs, 0.0);
            each(self, row, &mut out[start..])?;
        }
        Ok(out)
    }

    /// Validate a row-major matrix once and hand back its rows. Also the checked entry of
    /// [`GbdtModel::shap_values_batch`].
    pub(crate) fn rows<'a>(
        &self,
        flat_x: &'a [f64],
        n_features: usize,
    ) -> Result<impl Iterator<Item = &'a [f64]>, MlError> {
        if n_features == 0 {
            return Err(MlError::Shape("n_features must be > 0".into()));
        }
        if n_features < self.n_features_expected() {
            return Err(MlError::Shape(format!(
                "rows are {n_features} wide; the model splits on feature {} so it needs {}",
                self.max_feature_index,
                self.n_features_expected()
            )));
        }
        if !flat_x.len().is_multiple_of(n_features) {
            return Err(MlError::Shape(format!(
                "{} values is not a whole number of {n_features}-wide rows",
                flat_x.len()
            )));
        }
        Ok(flat_x.chunks_exact(n_features))
    }
}

/// One row's answer into its slice of a batch: [`GbdtModel::predict_into`] or
/// [`GbdtModel::raw_scores_into`].
type PerRow = fn(&GbdtModel, &[f64], &mut [f64]) -> Result<(), MlError>;

#[path = "infer_tests.rs"]
#[cfg(test)]
mod infer_tests;
