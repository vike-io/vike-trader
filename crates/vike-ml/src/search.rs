//! The parameter-point vocabulary: the FIVE named axes a caller searches, the free-form name/value
//! point that applies onto [`crate::params::GbdtParams`], and the argmin that picks a winner.
//!
//! # What used to be here, and why it is gone
//!
//! This file also carried an exhaustive grid — a `GridSpace` of named axes, an odometer
//! enumeration with the LAST axis varying fastest, and a `search_grid` fold that scored every
//! point and broke ties toward the earlier one. It was written as the deterministic replacement
//! for a Bayesian sampler, and it never acquired a caller: the crate's one consumer enumerates its
//! own five-axis space, because what it scores a point with is a purged walk-forward
//! mini-backtest rather than a closure over one point. Dead code that LOOKS like the thing to
//! reach for is worse than no code — it is read as a recommendation — so the enumeration is
//! deleted and what is reached stays.
//!
//! # ⚠ TWO point types, and which name each one got
//!
//! There were two `GridPoint`s in this workspace with the same name and different shapes: this
//! crate's `Vec<(String, ParamValue)>` bag, and a five-field `Copy` struct in the study that
//! searches it — which is why the one file that saw both imported one under an alias and left a
//! comment saying "the two GridPoints are different types with the same name". They are now both
//! here, and the collision is resolved by ROLE rather than by seniority:
//!
//! * [`GridPoint`] is the five-field struct, because it is the thing a GRID is made of: its axes
//!   are fixed at compile time, it is `Copy` and const-constructible, so a caller can write its
//!   whole search space as a `const` and enumerate it with nested loops. A `Vec` of `String` keys
//!   can be none of that.
//! * [`ParamPoint`] is the free-form bag, named for what it is — a set of parameter assignments;
//!   nothing enumerates it. It keeps [`crate::params::GbdtParams::with_point`] as its verb.
//!
//! # The bag is a TYPE argument, not a convenience
//!
//! An axis value is three concrete kinds rather than a string, so `"7"` and `"15"` cannot compare
//! and format as text and a typo cannot be a silent no-op; and
//! [`crate::params::GbdtParams::apply`] refuses a name no parameter matches and an integer that
//! does not fit its field, so a swept knob that was never connected is an error here rather than
//! a winner that means nothing. That refusal is what a free-form parameter editor needs: it can
//! offer any name a user types and be told, by the library, that the name is not a parameter.
//! [`GridPoint::as_param_point`] is how the typed point reaches it, so the five fixed axes are
//! checked by exactly the same name/type/range rules an arbitrary one would be.

use std::fmt;

/// One value on one axis. Deliberately three concrete kinds rather than a string: an axis of
/// `"7"` and `"15"` would compare and format as text, and a typo would be a silent no-op.
///
/// `Text` is never applicable to a [`crate::params::GbdtParams`] field: every parameter
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
/// is written. See this module's doc for why the two carry different names.
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
/// ⚠ **The NON-searched parameters are deliberately NOT here**, and that is the whole reason this
/// is five fields rather than a second copy of [`crate::params::GbdtParams`]. Everything a caller
/// fixes once — the objective, the iteration count, the regularisation, the bagging — belongs in
/// the base parameters it applies this onto; a base copy of a searched axis is dead state whose
/// only effect would be to give a DROPPED mapping a plausible value to fail silently with.
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
/// Conservative on all five axes relative to LightGBM's own defaults (`31 / -1 / 20 / 0.1 / 1.0`):
/// a shallower, depth-bounded tree at a lower learning rate on a sampled feature set, which is
/// what a fit that could not be tuned should be. ⚠ It is a FALLBACK, not a recommendation: a
/// caller that reaches it has learned nothing about its data, and this value's job is to be a
/// fixed, reviewable point rather than a good one.
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
    /// Going through the bag rather than assigning [`crate::params::GbdtParams`] fields directly
    /// is deliberate: `apply` refuses a name no parameter matches and refuses an integer that does
    /// not fit its field, so a renamed axis is an error here instead of a knob that was swept and
    /// never connected.
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
/// `+INFINITY` is the "unusable" marker: a candidate that errored, or one whose score came back
/// unusable, is scored `INFINITY` by its caller and then loses to every finite score — and when
/// EVERY candidate is unusable the minimum is itself infinite, `filter` drops it, and the answer
/// is `None` rather than an index into a candidate nobody can use.
///
/// ⚠ **The unusable marker must be `+INFINITY`, and NOT NaN — this function cannot rescue a
/// caller that passes one.** NaN `partial_cmp`s to `None` against every finite score, so the
/// fallback ordering below makes it "not greater than anything"; a NaN at a LOW index therefore
/// becomes the minimum, `filter` drops it, and the WHOLE answer is `None` — every good candidate
/// discarded because one was unscoreable. `-INFINITY` does the same thing for the same reason. A
/// caller folds both into `INFINITY` at the point it scores, where the "this one is unusable"
/// decision belongs; `a_nan_or_a_negative_infinity_takes_the_whole_answer_down` pins that this is
/// the behaviour rather than an oversight.
///
/// ⚠ **The explicit index tiebreak is load-bearing and must not be simplified away.** `min_by`
/// alone happens to keep the first of equal elements today, but `min_by_key`, a sort, and rayon's
/// own `min_by` promise no such thing — so a caller whose winner is decided by a tie (a constant
/// scorer, an all-flat candidate set) would start depending on which spelling this function used,
/// and on thread scheduling if it ever became parallel. Comparing `(score, index)` makes
/// first-wins a property of the COMPARISON instead of a property of the iterator adapter.
///
/// `partial_cmp`'s `None` arm is unreachable for a caller that already replaced NaN with infinity,
/// and is spelled as `Equal` rather than a panic so that a caller which did not is decided by the
/// index rather than aborted.
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
    use crate::params::GbdtParams;

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
        // `-1 as u32` is 4_294_967_295, not an error: a swept knob silently set to a value nobody
        // declared, through the VALUE rather than the name.
        let mut params = GbdtParams::default();
        assert!(params.apply("num_leaves", &ParamValue::Int(-1)).is_err());
    }

    /// A value's rendering is what a caller puts in a report next to a score, so the three kinds
    /// must print as themselves rather than as `Int(15)`.
    #[test]
    fn a_value_displays_as_the_value_and_not_as_its_variant() {
        assert_eq!(ParamValue::Int(15).to_string(), "15");
        assert_eq!(ParamValue::Float(0.03).to_string(), "0.03");
        assert_eq!(ParamValue::Text("gbdt".into()).to_string(), "gbdt");
    }

    /// Every axis differs from `GbdtParams::default()` (`31 / -1 / 20 / 0.1 / 1.0`) — load-bearing
    /// rather than arbitrary, since a value that COINCIDES with a library default lets a dropped
    /// mapping pass the assertions below.
    fn typed() -> GridPoint {
        GridPoint {
            num_leaves: 7,
            max_depth: 3,
            min_data_in_leaf: 10,
            learning_rate: 0.03,
            feature_fraction: 0.5,
        }
    }

    /// Moved here with [`GridPoint::as_param_point`], and it is the assertion the field-by-field
    /// one below CANNOT make: a SWAP of two same-typed axes reads correctly from both sides at
    /// once. This one reads the bag directly — five entries, these names, in this order, each
    /// carrying the field it is named after.
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

    /// Moved here with the conversion, and it is not the same claim as the one above: a
    /// `ParamValue::Float(7.0)` renders as `num_leaves=7` and satisfies every VALUE assertion
    /// there — but [`GbdtParams::apply`] routes floats and integers to different fields, and a
    /// float on an integer axis is an error the moment an axis carries a value that is not whole.
    #[test]
    fn an_integer_axis_is_an_integer_on_the_wire_and_not_a_float() {
        let bag = typed().as_param_point();
        for name in ["num_leaves", "max_depth", "min_data_in_leaf"] {
            assert!(matches!(bag.get(name), Some(ParamValue::Int(_))), "{name} must be an Int");
        }
        for name in ["learning_rate", "feature_fraction"] {
            assert!(matches!(bag.get(name), Some(ParamValue::Float(_))), "{name} must be a Float");
        }
    }

    /// The typed point goes through the SAME refusals an arbitrary one does — which is the whole
    /// reason it is converted rather than assigned field by field.
    #[test]
    fn every_typed_axis_reaches_the_parameter_it_names_and_none_is_a_surviving_default() {
        let p = GbdtParams::default().with_point(&typed().as_param_point()).unwrap();
        assert_eq!(p.num_leaves, 7);
        assert_eq!(p.max_depth, 3);
        assert_eq!(p.min_data_in_leaf, 10);
        assert_eq!(p.learning_rate, 0.03);
        assert_eq!(p.feature_fraction, 0.5);
        // ...and none of them is merely the library default surviving untouched, which is what a
        // DROPPED axis looks like from the inside.
        let d = GbdtParams::default();
        assert_ne!(p.num_leaves, d.num_leaves);
        assert_ne!(p.max_depth, d.max_depth);
        assert_ne!(p.min_data_in_leaf, d.min_data_in_leaf);
        assert_ne!(p.learning_rate, d.learning_rate);
        assert_ne!(p.feature_fraction, d.feature_fraction);
    }

    /// The fallback is a real point that applies like any other — a value nobody could fit with
    /// would be worse than no fallback at all — and it is conservative on every axis, which is
    /// the only claim its doc makes about the numbers.
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

    /// ⚠ The behaviour the doc warns about, PINNED so nobody mistakes it for an oversight and
    /// nobody "fixes" it here: NaN and `-INFINITY` are not unusable markers this function can
    /// absorb. Either one at a low index becomes the minimum under the fallback ordering, and the
    /// whole answer is `None` — every good candidate discarded. The cure is at the caller, which
    /// scores an unusable candidate `INFINITY`.
    #[test]
    fn a_nan_or_a_negative_infinity_takes_the_whole_answer_down() {
        assert_eq!(best_index(&[f64::NAN, 0.4]), None);
        assert_eq!(best_index(&[f64::NEG_INFINITY, 0.4]), None);
        // ...and it is the ORDERING that does it, not the filter alone: at a HIGH index a NaN
        // still loses to the finite score that precedes it, so the fault is not symmetric and a
        // fixture that only tested this direction would look green.
        assert_eq!(best_index(&[0.4, f64::NAN]), Some(0));
    }

    #[test]
    fn all_scores_unusable_is_none_and_so_is_an_empty_slice() {
        assert_eq!(best_index(&[f64::INFINITY, f64::INFINITY]), None);
        assert_eq!(best_index(&[]), None);
    }
}
