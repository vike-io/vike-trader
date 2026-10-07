//! Training parameters, spelled with LightGBM's OWN names.
//!
//! Not a vike vocabulary over them: a caller who knows LightGBM can set a field without a
//! translation table, and a field that is missing here is looked up in LightGBM's parameter docs
//! rather than in ours.
//!
//! # ⚠ The list-valued-parameter trap
//!
//! Every field here is rendered as one `key=value` line of the config file [`crate::train`] writes,
//! and LightGBM's `Config::KV2Map` parses it. It runs `RemoveQuotationSymbol` on the value, so a
//! quoted string is harmless — but it does NOT strip brackets, and it splits list parameters on
//! COMMAS:
//!
//! * `categorical_feature=0,3` — correct, names two columns;
//! * `categorical_feature=[0,3]` — the shape anyone reaches for who has used the Python API, and it
//!   names NOTHING. `[0` and `3]` parse as no column.
//!
//! The parameter would then name no column at all, LightGBM would bin the categorical column as if
//! it were numerical, nothing would error, and the model would simply be worse in a way no test
//! reports — while the saved model's `parameters:` echo block still claimed the column was
//! categorical. The defence is a TYPE, not a comment: [`GbdtParams::categorical_feature`] is an
//! `Option<String>`, so a list cannot be written. [`GbdtParams::categorical_feature_csv`] is how a
//! list of column indices becomes one.
//!
//! Indices are 0-based over FEATURE columns and do not count the label column, which is LightGBM's
//! own convention and matches the indices the walker uses in `split_feature`.

use crate::error::MlError;
use crate::search::{ParamPoint, ParamValue};

/// LightGBM training parameters. Defaults are LightGBM's own, except the determinism knobs, plus a
/// pinned seed and a quiet verbosity.
#[derive(Clone, Debug, PartialEq)]
pub struct GbdtParams {
    pub objective: String,
    pub num_iterations: u32,
    pub learning_rate: f64,
    pub num_leaves: u32,
    /// `-1` means unlimited, as in LightGBM.
    pub max_depth: i32,
    pub min_data_in_leaf: u32,
    pub feature_fraction: f64,
    pub bagging_fraction: f64,
    pub bagging_freq: u32,
    pub lambda_l1: f64,
    pub lambda_l2: f64,
    /// LightGBM derives `bagging_seed` / `feature_fraction_seed` / `data_random_seed` from this
    /// when they are not set individually.
    pub seed: u64,
    pub num_threads: u32,
    pub deterministic: bool,
    pub force_row_wise: bool,
    /// `-1` silences LightGBM's own stdout chatter. This crate does no logging of its own.
    pub verbosity: i32,
    /// A COMMA-SEPARATED list of column indices, never a JSON array — see the module doc.
    pub categorical_feature: Option<String>,
}

impl Default for GbdtParams {
    /// LightGBM's defaults, with departures for the determinism knobs, plus a pinned seed and a
    /// quiet verbosity — not every departure here is about reproducibility.
    ///
    /// `deterministic` defaults to `false` upstream. Turning it on alone is NOT enough: with both
    /// `force_row_wise` and `force_col_wise` false, LightGBM benchmarks the two histogram
    /// strategies at runtime and picks whichever is faster ON THAT RUN, which changes the
    /// floating-point reduction order — LightGBM's own docs say to set one of them explicitly
    /// alongside `deterministic`. `num_threads = 1` is the belt-and-braces third: determinism
    /// across thread counts is claimed by the docs and contradicted by several open issues, and a
    /// reproducible model is worth more here than one core. `seed = 42` pins a specific
    /// reproducible run rather than LightGBM's own default of `0` — still about reproducibility,
    /// just not about the CROSS-RUN kind the three above guard. `verbosity = -1` is not: it just
    /// silences LightGBM's own stdout chatter, which this crate has no logging of its own to
    /// replace.
    fn default() -> Self {
        Self {
            objective: "binary".to_string(),
            num_iterations: 100,
            learning_rate: 0.1,
            num_leaves: 31,
            max_depth: -1,
            min_data_in_leaf: 20,
            feature_fraction: 1.0,
            bagging_fraction: 1.0,
            bagging_freq: 0,
            lambda_l1: 0.0,
            lambda_l2: 0.0,
            seed: 42,
            num_threads: 1,
            deterministic: true,
            force_row_wise: true,
            verbosity: -1,
            categorical_feature: None,
        }
    }
}

impl GbdtParams {
    /// Column indices as the comma-separated string LightGBM's config parser expects.
    ///
    /// This exists so a caller never hand-writes the string and never reaches for a JSON array —
    /// see the module doc for what an array does. An empty slice returns `""` — a declaration
    /// naming nothing — which [`crate::train`]'s config renderer is expected to skip rather than
    /// write as `categorical_feature=`.
    pub fn categorical_feature_csv(indices: &[usize]) -> String {
        indices.iter().map(usize::to_string).collect::<Vec<_>>().join(",")
    }

    /// Set one parameter BY LIGHTGBM'S OWN NAME.
    ///
    /// An unknown name is an error rather than a no-op: a search axis whose name does not match a
    /// parameter would otherwise sweep a knob that was never connected, report a winner, and
    /// mean nothing.
    pub fn apply(&mut self, name: &str, value: &ParamValue) -> Result<(), MlError> {
        let as_i = |v: &ParamValue| match v {
            ParamValue::Int(i) => Ok(*i),
            other => Err(MlError::Shape(format!("`{name}` needs an integer, got `{other}`"))),
        };
        let as_f = |v: &ParamValue| match v {
            ParamValue::Float(f) => Ok(*f),
            ParamValue::Int(i) => Ok(*i as f64),
            other => Err(MlError::Shape(format!("`{name}` needs a number, got `{other}`"))),
        };
        // `as u32`/`as i32`/`as u64` on an i64 TRUNCATE and WRAP rather than refuse — `Int(-1)` on
        // a u32 field becomes 4_294_967_295, not an error — which is a swept knob silently set to a
        // value nobody declared, the same failure class the unknown-name arm below refuses, arriving
        // through the VALUE instead of the name. `try_from` refuses instead of wrapping.
        let as_u32 = |v: &ParamValue| -> Result<u32, MlError> {
            let i = as_i(v)?;
            u32::try_from(i).map_err(|_| {
                MlError::Shape(format!(
                    "`{name}` needs a non-negative value that fits u32, got {i}"
                ))
            })
        };
        let as_i32 = |v: &ParamValue| -> Result<i32, MlError> {
            let i = as_i(v)?;
            i32::try_from(i).map_err(|_| {
                MlError::Shape(format!("`{name}` needs a value that fits i32, got {i}"))
            })
        };
        let as_u64 = |v: &ParamValue| -> Result<u64, MlError> {
            let i = as_i(v)?;
            u64::try_from(i).map_err(|_| {
                MlError::Shape(format!(
                    "`{name}` needs a non-negative value that fits u64, got {i}"
                ))
            })
        };
        match name {
            "num_iterations" => self.num_iterations = as_u32(value)?,
            "num_leaves" => self.num_leaves = as_u32(value)?,
            "max_depth" => self.max_depth = as_i32(value)?,
            "min_data_in_leaf" => self.min_data_in_leaf = as_u32(value)?,
            "bagging_freq" => self.bagging_freq = as_u32(value)?,
            "seed" => self.seed = as_u64(value)?,
            "learning_rate" => self.learning_rate = as_f(value)?,
            "feature_fraction" => self.feature_fraction = as_f(value)?,
            "bagging_fraction" => self.bagging_fraction = as_f(value)?,
            "lambda_l1" => self.lambda_l1 = as_f(value)?,
            "lambda_l2" => self.lambda_l2 = as_f(value)?,
            other => {
                return Err(MlError::Shape(format!(
                    "`{other}` is not a parameter this struct carries — add a field, or fix the \
                     axis name; a silent no-op here means a swept knob that was never connected"
                )));
            }
        }
        Ok(())
    }

    /// A copy of these parameters with every axis of `point` applied.
    pub fn with_point(&self, point: &ParamPoint) -> Result<GbdtParams, MlError> {
        let mut out = self.clone();
        for (name, value) in &point.values {
            out.apply(name, value)?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_pin_determinism_rather_than_leaving_it_to_the_box() {
        let p = GbdtParams::default();
        assert_eq!(p.objective, "binary");
        assert!(p.deterministic, "LightGBM's default is false");
        assert!(
            p.force_row_wise,
            "with both force_* false LightGBM BENCHMARKS the two histogram \
                                   strategies at runtime and picks by TIMING, which changes the \
                                   float reduction order run to run"
        );
        assert_eq!(
            p.num_threads, 1,
            "pinned as well: determinism across thread counts is claimed, \
                                      not proven, and reproducibility is worth one core here"
        );
    }

    #[test]
    fn a_categorical_feature_list_is_a_comma_separated_string_with_no_brackets() {
        assert_eq!(GbdtParams::categorical_feature_csv(&[0]), "0");
        assert_eq!(GbdtParams::categorical_feature_csv(&[0, 3]), "0,3");
        assert_eq!(GbdtParams::categorical_feature_csv(&[]), "");
        let csv = GbdtParams::categorical_feature_csv(&[0, 3]);
        assert!(
            !csv.contains('['),
            "a bracketed list reaches LightGBM's config parser as the \
                                     LITERAL `[0,3]` — KV2Map strips quotation marks, not brackets \
                                     — and then names no column at all, silently"
        );
        assert!(!csv.contains(' '), "the config parser splits on commas, not on whitespace");
    }

    #[test]
    fn no_categorical_feature_is_declared_by_default() {
        assert_eq!(GbdtParams::default().categorical_feature, None);
    }
}
