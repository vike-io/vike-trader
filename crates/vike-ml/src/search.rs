//! The parameter-point vocabulary: the FIVE named axes a caller searches, the free-form name/value
//! point that applies onto [`crate::train::params::GbdtParams`], and the argmin that picks a winner.
//!
//! ⚠ No enumeration lives here. An exhaustive grid (`GridSpace`, `search_grid`) was deleted for
//! having no caller — the consumer enumerates its own five-axis space and scores each point with a
//! walk-forward mini-backtest — because dead code that LOOKS like the thing to reach for is read
//! as a recommendation.
//!
//! # TWO point types, named by ROLE
//!
//! * [`GridPoint`] is the five-field struct a GRID is made of: fixed axes, `Copy` and
//!   const-constructible, so a caller can write its whole search space as a `const` and enumerate
//!   it with nested loops.
//! * [`ParamPoint`] is the free-form bag — a set of parameter assignments; nothing enumerates it.
//!   [`crate::train::params::GbdtParams::with_point`] is its verb.
//!
//! # The bag is a TYPE argument, not a convenience
//!
//! [`crate::train::params::GbdtParams::apply`] refuses a name no parameter matches and an integer that
//! does not fit its field, so a swept knob that was never connected is an error rather than a
//! winner that means nothing — and a free-form parameter editor can offer any name and be told by
//! the library that it is not a parameter. [`GridPoint::as_param_point`] routes the five fixed
//! axes through exactly the same rules.
//!
//! # Children
//!
//! [`grid`] runs a parameter grid over a fixed set of datasets; [`tpe`] is the ask/tell Bayesian
//! sampler, here so a study host can reach it without a backtester.

pub mod grid;
pub mod tpe;

use std::fmt;

/// One value on one axis. Deliberately three concrete kinds rather than a string: an axis of
/// `"7"` and `"15"` would compare and format as text, and a typo would be a silent no-op.
///
/// `Text` is never applicable to a [`crate::train::params::GbdtParams`] field: every parameter
/// `GbdtParams::apply` knows how to set is numeric, so a `Text` value there is always an error.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Text(String),
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Text(v) => write!(f, "{v}"),
        }
    }
}

/// A set of parameter assignments by LightGBM's own names, in the caller's declaration order.
///
/// The free-form twin of [`GridPoint`]: any name, checked when it is APPLIED rather than when it
/// is written.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamPoint {
    pub values: Vec<(String, ParamValue)>,
}

impl ParamPoint {
    pub fn get(&self, name: &str) -> Option<&ParamValue> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

/// The five hyperparameter axes a tree search moves, as one `Copy`, const-constructible point.
///
/// `max_depth` is signed because LightGBM spells "unlimited" as `-1`; a `u32` would make that
/// spelling unrepresentable rather than merely unused.
///
/// ⚠ **The NON-searched parameters are deliberately NOT here**: everything a caller fixes once
/// belongs in the base [`crate::train::params::GbdtParams`] it applies this onto. A copy of a fixed
/// parameter here would give a DROPPED mapping a plausible value to fail silently with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridPoint {
    pub num_leaves: u32,
    pub max_depth: i32,
    pub min_data_in_leaf: u32,
    pub learning_rate: f64,
    pub feature_fraction: f64,
}

/// The point a caller falls back to when a search produced no winner — too little data to split,
/// or every candidate refused.
///
/// Conservative relative to LightGBM's own defaults (`31 / -1 / 20 / 0.1 / 1.0`): a shallower,
/// depth-bounded tree at a lower learning rate on a sampled feature set. ⚠ A FALLBACK, not a
/// recommendation: its job is to be a fixed, reviewable point rather than a good one.
pub const DEFAULT_POINT: GridPoint = GridPoint {
    num_leaves: 15,
    max_depth: 4,
    min_data_in_leaf: 20,
    learning_rate: 0.05,
    feature_fraction: 0.7,
};

impl GridPoint {
    /// The five typed axes as a [`ParamPoint`], under LightGBM's own parameter names.
    ///
    /// Going through the bag rather than assigning [`crate::train::params::GbdtParams`] fields directly
    /// is deliberate: `apply`'s refusals make a renamed axis an error, not a knob never connected.
    pub fn as_param_point(&self) -> ParamPoint {
        ParamPoint {
            values: vec![
                ("num_leaves".to_string(), ParamValue::Int(i64::from(self.num_leaves))),
                ("max_depth".to_string(), ParamValue::Int(i64::from(self.max_depth))),
                ("min_data_in_leaf".to_string(), ParamValue::Int(i64::from(self.min_data_in_leaf))),
                ("learning_rate".to_string(), ParamValue::Float(self.learning_rate)),
                ("feature_fraction".to_string(), ParamValue::Float(self.feature_fraction)),
            ],
        }
    }
}

/// The argmin by `(score, index)`, or `None` when the minimum is not a finite score.
///
/// Lower is better, so a caller ranking on something where higher is better negates first.
/// `+INFINITY` is the "unusable" marker: it loses to every finite score, and when EVERY candidate
/// is unusable the answer is `None`.
///
/// ⚠ **The unusable marker must be `+INFINITY`, NOT NaN or `-INFINITY` — this function cannot
/// rescue a caller that passes one.** Either one at a LOW index becomes the minimum under the
/// ordering below, `filter` drops it, and the WHOLE answer is `None`. A caller folds both into
/// `INFINITY` where it scores; `a_nan_or_a_negative_infinity_takes_the_whole_answer_down` pins it.
///
/// ⚠ **The explicit index tiebreak is load-bearing.** `min_by_key`, a sort or rayon's `min_by`
/// promise no first-of-equals, so comparing `(score, index)` makes first-wins a property of the
/// COMPARISON rather than of the iterator adapter. The `partial_cmp` `None` arm is `Equal`, not a
/// panic, so a NaN is decided by the index rather than aborted.
pub fn best_index(scores: &[f64]) -> Option<usize> {
    scores
        .iter()
        .enumerate()
        .min_by(|(ia, a), (ib, b)| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal).then(ia.cmp(ib))
        })
        .filter(|(_, s)| s.is_finite())
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::params::GbdtParams;
    use std::assert_matches;

    fn point() -> ParamPoint {
        ParamPoint {
            values: vec![
                ("num_leaves".to_string(), ParamValue::Int(15)),
                ("learning_rate".to_string(), ParamValue::Float(0.1)),
            ],
        }
    }

    #[test]
    fn a_point_answers_by_axis_name_and_not_by_position() {
        let p = point();
        assert_eq!(p.get("learning_rate"), Some(&ParamValue::Float(0.1)));
        assert_eq!(p.get("num_leaves"), Some(&ParamValue::Int(15)));
        assert_eq!(p.get("no_such_axis"), None, "an absent axis is None, never the first value");
    }

    #[test]
    fn a_point_applies_onto_params_by_lightgbms_own_parameter_names() {
        let params = GbdtParams::default().with_point(&point()).unwrap();
        assert_eq!(params.num_leaves, 15);
        assert_eq!(params.learning_rate, 0.1);
        assert_eq!(params.objective, "binary", "untouched fields survive");
    }

    #[test]
    fn an_axis_name_no_parameter_matches_is_an_error_not_a_silent_no_op() {
        let mut params = GbdtParams::default();
        assert!(params.apply("no_such_param", &ParamValue::Int(1)).is_err());
    }

    #[test]
    fn an_axis_value_of_the_wrong_kind_is_an_error() {
        let mut params = GbdtParams::default();
        assert!(params.apply("num_leaves", &ParamValue::Text("many".into())).is_err());
    }

    #[test]
    fn an_out_of_range_integer_is_refused_rather_than_silently_wrapped() {
        // `-1 as u32` is 4_294_967_295, not an error.
        let mut params = GbdtParams::default();
        assert!(params.apply("num_leaves", &ParamValue::Int(-1)).is_err());
    }

    /// A caller puts the rendering in a report, so it must not read `Int(15)`.
    #[test]
    fn a_value_displays_as_the_value_and_not_as_its_variant() {
        assert_eq!(ParamValue::Int(15).to_string(), "15");
        assert_eq!(ParamValue::Float(0.03).to_string(), "0.03");
        assert_eq!(ParamValue::Text("gbdt".into()).to_string(), "gbdt");
    }

    /// Every axis differs from `GbdtParams::default()` (`31 / -1 / 20 / 0.1 / 1.0`): a value that
    /// COINCIDES with a library default lets a dropped mapping pass the assertions below.
    fn typed() -> GridPoint {
        GridPoint {
            num_leaves: 7,
            max_depth: 3,
            min_data_in_leaf: 10,
            learning_rate: 0.03,
            feature_fraction: 0.5,
        }
    }

    /// What the field-by-field test below CANNOT see: a SWAP of two same-typed axes reads
    /// correctly from both sides at once. This reads the bag directly.
    #[test]
    fn no_axis_is_mapped_onto_another_axis_name() {
        let bag = typed().as_param_point();
        let names: Vec<&str> = bag.values.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "num_leaves",
                "max_depth",
                "min_data_in_leaf",
                "learning_rate",
                "feature_fraction"
            ]
        );
        assert_eq!(bag.get("num_leaves"), Some(&ParamValue::Int(7)));
        assert_eq!(bag.get("max_depth"), Some(&ParamValue::Int(3)));
        assert_eq!(bag.get("min_data_in_leaf"), Some(&ParamValue::Int(10)));
        assert_eq!(bag.get("learning_rate"), Some(&ParamValue::Float(0.03)));
        assert_eq!(bag.get("feature_fraction"), Some(&ParamValue::Float(0.5)));
    }

    /// A `ParamValue::Float(7.0)` satisfies every VALUE assertion above, but [`GbdtParams::apply`]
    /// routes floats and integers differently.
    #[test]
    fn an_integer_axis_is_an_integer_on_the_wire_and_not_a_float() {
        let bag = typed().as_param_point();
        for name in ["num_leaves", "max_depth", "min_data_in_leaf"] {
            assert_matches!(bag.get(name), Some(ParamValue::Int(_)), "{name} must be an Int");
        }
        for name in ["learning_rate", "feature_fraction"] {
            assert_matches!(bag.get(name), Some(ParamValue::Float(_)), "{name} must be a Float");
        }
    }

    /// The typed point goes through the SAME refusals an arbitrary one does.
    #[test]
    fn every_typed_axis_reaches_the_parameter_it_names_and_none_is_a_surviving_default() {
        let p = GbdtParams::default().with_point(&typed().as_param_point()).unwrap();
        assert_eq!(p.num_leaves, 7);
        assert_eq!(p.max_depth, 3);
        assert_eq!(p.min_data_in_leaf, 10);
        assert_eq!(p.learning_rate, 0.03);
        assert_eq!(p.feature_fraction, 0.5);
        // ...and none is the library default surviving untouched (a DROPPED axis).
        let d = GbdtParams::default();
        assert_ne!(p.num_leaves, d.num_leaves);
        assert_ne!(p.max_depth, d.max_depth);
        assert_ne!(p.min_data_in_leaf, d.min_data_in_leaf);
        assert_ne!(p.learning_rate, d.learning_rate);
        assert_ne!(p.feature_fraction, d.feature_fraction);
    }

    /// The fallback applies like any other point and is conservative on every axis — the only
    /// claim its doc makes about the numbers.
    #[test]
    fn the_default_point_applies_and_is_conservative_against_lightgbms_own_defaults() {
        let d = GbdtParams::default();
        let p = d.with_point(&DEFAULT_POINT.as_param_point()).unwrap();
        assert_eq!(p.num_leaves, 15);
        assert_eq!(p.max_depth, 4);
        assert_eq!(p.min_data_in_leaf, 20);
        assert_eq!(p.learning_rate, 0.05);
        assert_eq!(p.feature_fraction, 0.7);
        assert!(p.num_leaves < d.num_leaves, "a narrower tree than the library default");
        assert!(p.max_depth > 0 && d.max_depth < 0, "bounded depth against LightGBM's unlimited");
        assert!(p.learning_rate < d.learning_rate);
        assert!(p.feature_fraction < d.feature_fraction);
    }

    #[test]
    fn the_argmin_is_the_lowest_score() {
        assert_eq!(best_index(&[0.9, 0.3, 0.7]), Some(1));
    }

    /// The property the explicit `(score, index)` comparison exists for.
    #[test]
    fn a_tie_is_broken_by_the_lowest_index() {
        assert_eq!(best_index(&[0.5, 0.5, 0.5]), Some(0));
        assert_eq!(best_index(&[0.9, 0.4, 0.4, 0.9]), Some(1));
    }

    /// `INFINITY` is the unusable marker, and it loses to every finite score wherever it sits.
    #[test]
    fn an_infinite_score_never_wins_even_at_index_zero() {
        assert_eq!(best_index(&[f64::INFINITY, 0.4]), Some(1));
        assert_eq!(best_index(&[0.4, f64::INFINITY]), Some(0));
    }

    /// ⚠ The behaviour `best_index`'s doc warns about, PINNED so nobody "fixes" it here: the cure
    /// is at the caller, which scores an unusable candidate `INFINITY`.
    #[test]
    fn a_nan_or_a_negative_infinity_takes_the_whole_answer_down() {
        assert_eq!(best_index(&[f64::NAN, 0.4]), None);
        assert_eq!(best_index(&[f64::NEG_INFINITY, 0.4]), None);
        // ...and it is the ORDERING that does it: at a HIGH index a NaN still loses to the finite
        // score before it, so the fault is not symmetric.
        assert_eq!(best_index(&[0.4, f64::NAN]), Some(0));
    }

    #[test]
    fn all_scores_unusable_is_none_and_so_is_an_empty_slice() {
        assert_eq!(best_index(&[f64::INFINITY, f64::INFINITY]), None);
        assert_eq!(best_index(&[]), None);
    }
}
