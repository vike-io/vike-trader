//! The parsed shape of a LightGBM `save_model` text file, reduced to what INFERENCE reads.
//!
//! # What is deliberately not here
//!
//! `split_gain`, `leaf_weight`, `leaf_count`, `internal_value`, `internal_weight`, `internal_count`
//! and `feature_infos` are parsed past and dropped. They are training bookkeeping: the prediction
//! path in LightGBM's own `Tree::Decision` reads `split_feature`, `threshold`, `decision_type`,
//! `left_child`, `right_child`, `cat_boundaries` and `cat_threshold`, and nothing else. Keeping a
//! field this crate never reads would invite a future reader to believe it means something here.
//!
//! `shrinkage` is likewise dropped, and that one is a TRAP rather than a tidy-up — see
//! [`crate::infer`]'s `raw_score`.

/// `decision_type` bit 0: this node splits on a CATEGORY set, not on a threshold.
///
/// This bit alone routes a node through the categorical path in LightGBM's `Tree::Decision`, and
/// the two paths do not agree about missing values — see [`crate::infer`].
pub const CATEGORICAL_MASK: u8 = 1;

/// `decision_type` bit 1: a MISSING value goes left. Read by the numerical path ONLY.
pub const DEFAULT_LEFT_MASK: u8 = 2;

/// LightGBM's `kZeroThreshold`: what its `IsZero` counts as zero for `missing_type == Zero`.
///
/// Confirmed against v4.7.0's `include/LightGBM/meta.h`: `const double kZeroThreshold = 1e-35f;` —
/// a FLOAT literal widened to double, which is NOT the same double as a bare `1e-35` (they differ
/// by up to half a float ULP, ~7e-43). `1e-35f32 as f64` reproduces the exact C++ widening.
pub const ZERO_THRESHOLD: f64 = 1e-35f32 as f64;

// ⚠ `MODEL_VERSION` is NOT re-exported here. It lives in [`crate::pins`] with its twin — the two
// pins move together on a LightGBM bump, and that file carries the bump procedure's reasoning. The
// re-export this replaced was a move shim ("every existing `use crate::model::MODEL_VERSION` keeps
// resolving"); the only import it ever kept resolving was `parse.rs`'s, which now names `pins`
// directly. Outside this crate the spelling is the flat `vike_ml::MODEL_VERSION`.

/// What a node treats as a missing value, baked in at training time by `use_missing` /
/// `zero_as_missing`. Neither of those parameters is read again at inference; this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingType {
    /// Nothing is missing. A NaN input is silently remapped to `0.0` before the comparison.
    NoMissing,
    /// A zero (within [`ZERO_THRESHOLD`]) is the missing value.
    Zero,
    /// A NaN is the missing value.
    Nan,
}

/// Unpack the two `missing_type` bits — `(decision_type >> 2) & 3`.
pub fn missing_type(decision_type: u8) -> MissingType {
    match (decision_type >> 2) & 3 {
        1 => MissingType::Zero,
        2 => MissingType::Nan,
        _ => MissingType::NoMissing,
    }
}

/// Does this node split on a category set?
pub fn is_categorical(decision_type: u8) -> bool {
    decision_type & CATEGORICAL_MASK != 0
}

/// Does a missing value go left at this node? Meaningless at a categorical node, which never
/// consults it.
pub fn is_default_left(decision_type: u8) -> bool {
    decision_type & DEFAULT_LEFT_MASK != 0
}

/// LightGBM's `IsZero`: a threshold test, not `== 0.0`.
pub fn is_zero(fval: f64) -> bool {
    (-ZERO_THRESHOLD..=ZERO_THRESHOLD).contains(&fval)
}

/// One boosted tree: parallel arrays of length `num_leaves - 1` for the internal nodes and
/// `num_leaves` for the leaves.
///
/// A child entry `>= 0` is the index of another INTERNAL node; a NEGATIVE entry encodes a leaf as
/// its one's complement (`leaf = !node`), which is why a naive negation is a bug rather than a
/// style choice — see [`crate::infer`].
#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    pub num_leaves: usize,
    /// Feature index per internal node.
    pub split_feature: Vec<u32>,
    /// Numerical node: the threshold. CATEGORICAL node: an INDEX into [`Tree::cat_boundaries`],
    /// not a feature value at all.
    pub threshold: Vec<f64>,
    /// The packed bits: [`CATEGORICAL_MASK`], [`DEFAULT_LEFT_MASK`], [`missing_type`].
    pub decision_type: Vec<u8>,
    pub left_child: Vec<i32>,
    pub right_child: Vec<i32>,
    /// ⚠ ALREADY multiplied by `learning_rate` at training time. Never scale it again.
    pub leaf_value: Vec<f64>,
    /// `num_cat + 1` word offsets into [`Tree::cat_threshold`]. Empty when `num_cat == 0`.
    pub cat_boundaries: Vec<u32>,
    /// Packed category-membership bitsets, 32 categories per `u32` word.
    pub cat_threshold: Vec<u32>,
}

/// The output transform. Only `binary` is implemented; the parser refuses anything else rather
/// than defaulting, because the wrong transform is a plausible-looking number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Objective {
    /// `1 / (1 + exp(-sigmoid * raw))`. `sigmoid` comes from the header's `objective=` line — it
    /// is NOT a per-tree field and NOT always 1.0.
    Binary { sigmoid: f64 },
}

/// A parsed model: the header facts inference needs, and every tree in file order.
///
/// Tree order is load-bearing twice over. The score is their sum, and `boost_from_average`'s
/// initial score is not a header scalar at all — LightGBM FOLDS IT INTO THE FIRST TREE
/// (`GBDT::TrainOneIter` calls `new_tree->AddBias(init_score)` when that tree splits, and
/// materializes a constant tree only when the learner could not split at all), so summing every
/// tree in the file is both necessary and sufficient.
#[derive(Clone, Debug, PartialEq)]
pub struct GbdtModel {
    pub max_feature_idx: usize,
    pub objective: Objective,
    pub feature_names: Vec<String>,
    pub trees: Vec<Tree>,
}

impl GbdtModel {
    /// How wide a feature row must be: `max_feature_idx + 1`.
    pub fn n_features_expected(&self) -> usize {
        self.max_feature_idx + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_numerical_node_is_neither_categorical_nor_default_left() {
        assert!(!is_categorical(0));
        assert!(!is_default_left(0));
        assert_eq!(missing_type(0), MissingType::NoMissing);
    }

    #[test]
    fn bit_zero_is_categorical_and_bit_one_is_default_left() {
        assert!(is_categorical(1));
        assert!(!is_default_left(1));
        assert!(!is_categorical(2));
        assert!(is_default_left(2));
        assert!(is_categorical(3) && is_default_left(3));
    }

    #[test]
    fn the_missing_type_is_the_two_bits_above_them() {
        // (decision_type >> 2) & 3 — 0 None, 1 Zero, 2 NaN.
        assert_eq!(missing_type(1 << 2), MissingType::Zero);
        assert_eq!(missing_type(2 << 2), MissingType::Nan);
        // ...and it is independent of the two low bits.
        assert_eq!(missing_type((2 << 2) | 3), MissingType::Nan);
        assert!(is_categorical((2 << 2) | 3));
    }

    #[test]
    fn is_zero_is_a_threshold_not_an_equality() {
        assert!(is_zero(0.0));
        assert!(is_zero(-0.0));
        assert!(is_zero(1e-40));
        assert!(!is_zero(1e-30));
        assert!(!is_zero(f64::NAN));
    }

    #[test]
    fn the_expected_feature_count_is_one_past_the_max_index() {
        let m = GbdtModel {
            max_feature_idx: 2,
            objective: Objective::Binary { sigmoid: 1.0 },
            feature_names: vec!["a".into(), "b".into(), "c".into()],
            trees: Vec::new(),
        };
        assert_eq!(m.n_features_expected(), 3);
    }
}
