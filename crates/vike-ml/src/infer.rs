//! The tree walker — pure Rust, no native dependency, and the reason a live binary can predict
//! from a LightGBM model without a C++ toolchain.
//!
//! Every routing rule below was read out of LightGBM's `include/LightGBM/tree.h` (`Decision`,
//! `NumericalDecision`, `CategoricalDecision`, `GetLeafByMap`) rather than inferred, because each
//! one fails SILENTLY: the walk terminates whatever you do, at a different leaf, and the
//! probability that comes out looks like a probability.

use crate::error::MlError;
use crate::model::{
    GbdtModel, MissingType, Objective, Tree, is_categorical, is_default_left, is_zero, missing_type,
};

impl Tree {
    /// The feature this node splits on, or `NaN` when the row does not carry it.
    ///
    /// ⚠ `NaN` is NOT what LightGBM itself substitutes for a genuinely absent entry — its own
    /// dense-buffer fill (`Predictor`'s row buffer, `Tree::GetLeafByMap`) uses `0.0`. Choosing
    /// `NaN` here matches LightGBM's behaviour at a `NoMissing`/`Zero` node (which remaps a `NaN`
    /// input right back to `0.0` anyway — see `numerical_decision`) but DIVERGES at a
    /// `missing_type == Nan` node: LightGBM's `0.0` fill is not missing there, while this `NaN` is.
    /// A row that drifted out of sync with the model's width is exactly the case the
    /// `debug_assert!` in [`GbdtModel::raw_score`] exists to catch in test/debug builds; the
    /// CHECKED entry point for production is [`crate::GbdtModel::predict_proba_batch`], which
    /// validates the row width once.
    fn feature_value(&self, node: usize, row: &[f64]) -> f64 {
        row.get(self.split_feature[node] as usize).copied().unwrap_or(f64::NAN)
    }

    /// One node's child, as the raw encoding: `>= 0` is another internal node, `< 0` is a leaf.
    fn decide(&self, node: usize, row: &[f64]) -> i32 {
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
    ///    then compared normally. A walker that instead sent every NaN to the default side
    ///    diverges on every `NoMissing`/`Zero` node — the most common kind in a model trained on
    ///    dense data.
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
    /// `raw_score`.
    ///
    /// ⚠ Every index below is UNCHECKED, deliberately — this is the per-row live path — and it is
    /// total only because [`crate::parse`]'s `check_children` proved the two facts it needs at LOAD
    /// time: every internal child is an index in range, and every child is numbered ABOVE its
    /// parent, so this loop's `node` strictly increases and cannot cycle. A tree that reached here
    /// without that check could panic or spin inside a live prediction. Do not weaken the parser's
    /// side of that bargain.
    pub fn leaf_value_for(&self, row: &[f64]) -> f64 {
        // A constant tree (`num_leaves == 1`, empty split arrays) has nothing to walk. LightGBM
        // emits one when a learner could not split at all — `GBDT::TrainOneIter`'s
        // `AsConstantTree` arm — which is rare but perfectly legal, and reaching into an empty
        // `split_feature` would panic rather than return the leaf.
        if self.split_feature.is_empty() {
            return self.leaf_value[0];
        }
        let mut node: i32 = 0;
        while node >= 0 {
            node = self.decide(node as usize, row);
        }
        self.leaf_value[!node as usize]
    }

    /// LightGBM's `CategoricalDecision` — a DIFFERENT function from the numerical one, and the
    /// place a hand-written walker is most likely to diverge in production.
    ///
    /// What it does NOT do, and this is the whole trap: it never consults `default_left` and never
    /// consults `missing_type`. Those bits live in the same packed byte and only the numerical
    /// path reads them. A `NaN` or a negative category goes RIGHT, unconditionally.
    ///
    /// What it does do:
    ///
    /// 1. `int_fval = fval as i32` — truncation toward zero, matching C++'s `static_cast<int>`,
    ///    NOT rounding. Rust's `as` cast also saturates instead of invoking UB, so an absurd id
    ///    is a right turn rather than a crash.
    /// 2. `cat_idx = threshold[node] as usize` — the node's threshold is an INDEX into
    ///    `cat_boundaries`, not a feature value.
    /// 3. Test bit `int_fval % 32` of word `cat_boundaries[cat_idx] + int_fval / 32`. A word past
    ///    `cat_boundaries[cat_idx + 1]` is implicitly zero — i.e. not in the set — which is how an
    ///    unseen category routes without an out-of-bounds read.
    ///
    /// `max_cat_to_onehot` and `max_cat_threshold` are TRAINING-time search heuristics: whether
    /// LightGBM chose a one-hot-style split or a many-category one, the serialized result is this
    /// same bitset lookup with a different number of bits set. There is no second case to handle.
    fn categorical_decision(&self, node: usize, fval: f64) -> i32 {
        if fval.is_nan() {
            return self.right_child[node];
        }
        let int_fval = fval as i32;
        if int_fval < 0 {
            return self.right_child[node];
        }
        let cat_idx = self.threshold[node] as usize;
        let (Some(&start), Some(&end)) =
            (self.cat_boundaries.get(cat_idx), self.cat_boundaries.get(cat_idx + 1))
        else {
            // A group index the model does not describe. Not reachable from a well-formed model —
            // the parser checks `cat_boundaries.len() == num_cat + 1` — so treat it as "not in the
            // set" rather than panicking inside a live prediction.
            return self.right_child[node];
        };
        let word = int_fval as usize / 32;
        let idx = start as usize + word;
        if idx >= end as usize {
            return self.right_child[node];
        }
        match self.cat_threshold.get(idx) {
            Some(bits) if (bits >> (int_fval as u32 % 32)) & 1 == 1 => self.left_child[node],
            _ => self.right_child[node],
        }
    }
}

impl GbdtModel {
    /// The pre-transform score: the plain sum of every tree's selected `leaf_value`.
    ///
    /// # Three things this deliberately does not do
    ///
    /// * **It does not multiply by `shrinkage`.** LightGBM's `Tree::Shrinkage` multiplies
    ///   `leaf_value_` by the learning rate IN PLACE at training time, and its prediction path
    ///   reads `leaf_value_` directly. The `shrinkage=` field in the text model exists so a
    ///   reloaded model can be shrunk again if boosting RESUMES. Applying it here scales every
    ///   score by `learning_rate` squared — a wrong number that still looks like a score.
    /// * **It does not add a base score.** `boost_from_average` (on by default for `binary`)
    ///   computes an initial log-odds from the label mean, and LightGBM then folds that init score
    ///   INTO THE FIRST TREE: `GBDT::TrainOneIter` calls `new_tree->AddBias(init_score)` when that
    ///   tree splits — adding the score onto every one of its leaf values — and materializes it as
    ///   a constant tree only when the learner could not split at all. Either way it is already
    ///   inside the trees this loop sums, so summing every tree in the file is both necessary and
    ///   sufficient; adding a separate base score double-counts it.
    ///
    ///   ⚠ `Tree::AddBias` also forces `shrinkage_ = 1.0` on that first tree, which is why a real
    ///   model's `Tree=0` carries `shrinkage=1` while every later tree carries the learning rate.
    ///   Nothing here reads that field (see the bullet above), but a reader who does not know this
    ///   will suspect the file.
    /// * **It does not use a compensated sum.** `vike_model::py_sum` is the workspace's Neumaier
    ///   fold and it is MORE accurate than this loop — which is exactly why it is wrong here.
    ///   LightGBM accumulates `score += leaf_value_[~node]` in a plain loop over trees in file
    ///   order, and `tests/train_infer_equality.rs`'s `the_raw_scores_match_bit_for_bit` compares
    ///   this function against LightGBM's own `predict_raw_score=true` output BIT FOR BIT. A
    ///   better sum would fail that gate, correctly.
    pub fn raw_score(&self, row: &[f64]) -> f64 {
        // Free in release, loud in every test and debug run: the single-row API is intentionally
        // UNCHECKED in release (it is the per-tick live path), so a row that silently drifted out
        // of sync with the model's width would otherwise produce a plausible wrong probability —
        // see feature_value's doc for exactly how NaN and LightGBM's own 0.0 fill can disagree.
        debug_assert!(
            row.len() >= self.n_features_expected(),
            "row is {} wide; this model splits on feature {} so it needs at least {}",
            row.len(),
            self.max_feature_idx,
            self.n_features_expected()
        );
        let mut acc = 0.0;
        for tree in &self.trees {
            acc += tree.leaf_value_for(row);
        }
        acc
    }

    /// `predict_proba` for `objective=binary`: `1 / (1 + exp(-sigmoid * raw))`.
    ///
    /// The `sigmoid` is the one parsed from the header's `objective=` line, not an assumed 1.0.
    /// `is_unbalance` and `scale_pos_weight` change the TRAINING gradients only — whatever they
    /// did is already baked into the leaf values, and there is no inference-time case for them.
    ///
    /// ⚠ **The `exp` is `libm::exp`, NOT `f64::exp`, and it is the one place a probability out of
    /// this crate could differ between two boxes.** IEEE 754 requires `+`, `-`, `*`, `/` and `sqrt`
    /// to be correctly rounded and requires NOTHING of `exp`, so an `f64` METHOD call reaches
    /// whichever libm the platform ships — MSVC's CRT on the Windows dev box, glibc on the CI box and
    /// every CI runner — and the two disagree in the last bit, measured by the `exp` sweeps in
    /// `crates/vike-analytics/tests/libm_platform_probe.rs` (whose
    /// `converted_functions_are_platform_invariant` is the gate half of the same file). The `libm`
    /// CRATE is the same source everywhere.
    /// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
    /// verdict, and its Consequences section names this crate among the ones not yet converted;
    /// this site is now that entry's WHOLE share. ⚠ It was one of TWO until `metrics.rs` was
    /// deleted — `binary_logloss` was the other, and it went because it had no caller in this
    /// workspace and its single `vike_model::py_sum` import was the last thing tying this crate to
    /// `vike-model`. So the 0032 ledger row shrank by one site without any conversion happening;
    /// do not read the smaller share as progress against that record.
    ///
    /// ⚠ Everything UNDER the `exp` was already portable and is untouched: [`GbdtModel::raw_score`]
    /// is a plain `+` fold over leaf values, and the walk that selects them is comparisons and
    /// integer bit tests. So the conversion moves the probability by at most an ulp and cannot move
    /// a routing decision — which is exactly why the two gates over this function did not have to
    /// be rebaselined. Both already budget `1e-12` for "one `exp` through two libms", stated in
    /// those words: `crates/vike-ml/tests/train_infer_equality.rs`'s
    /// `the_pure_rust_walker_reproduces_the_scorer_it_replaces` (and its no-categorical sibling
    /// `the_walker_agrees_on_a_model_with_no_categorical_feature_either`) compares against
    /// LightGBM's own `task=predict` — which still calls the PLATFORM's `exp`, so that gap does not
    /// close here, it merely stops depending on which box ran OUR side — and
    /// `crates/vike-ml/tests/real_model_golden.rs`'s
    /// `the_walker_reproduces_lightgbms_own_probabilities_to_a_stated_epsilon` compares against
    /// frozen fixture values captured from it. An ulp is four orders inside that budget, so no
    /// fixture moves. Do not widen either number on account of this change.
    pub fn predict_proba(&self, row: &[f64]) -> f64 {
        match self.objective {
            Objective::Binary { sigmoid } => {
                1.0 / (1.0 + libm::exp(-sigmoid * self.raw_score(row)))
            }
        }
    }

    /// Row-major batch prediction, with the width check the single-row API deliberately skips.
    pub fn predict_proba_batch(
        &self,
        flat_x: &[f64],
        n_features: usize,
    ) -> Result<Vec<f64>, MlError> {
        Ok(self.rows(flat_x, n_features)?.map(|row| self.predict_proba(row)).collect())
    }

    /// The pre-transform twin of [`GbdtModel::predict_proba_batch`] — what the equality gate
    /// compares against LightGBM's own `raw_scores`.
    pub fn raw_scores_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, MlError> {
        Ok(self.rows(flat_x, n_features)?.map(|row| self.raw_score(row)).collect())
    }

    /// Validate a row-major matrix once and hand back its rows.
    fn rows<'a>(
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
                self.max_feature_idx,
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

#[path = "infer_tests.rs"]
#[cfg(test)]
mod infer_tests;
