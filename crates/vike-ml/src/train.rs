//! Training input: a fit expressed as LightGBM's own config text plus a data file.
//!
//! # Why there is no process in this module
//!
//! Everything here is a pure function of its arguments — `&str` in, `String` out, or a `Write` sink
//! the caller owns. That is what lets the two traps below be tested on a machine with no LightGBM
//! binary anywhere, which is every developer machine and every CI runner. [`crate::cli`] is the
//! twelve lines that actually spawn something, and it is the only part that needs a box with the
//! binary on it.
//!
//! # The seam
//!
//! [`TrainConfig::render`] emits the text LightGBM reads; LightGBM emits a model text;
//! [`crate::parse_model_text`] reads THAT. Nothing else crosses between the halves — no shared
//! memory, no FFI struct, no linked symbol. `tests/train_infer_equality.rs` asserts the round trip
//! agrees against LightGBM's own scorer, and nothing else has to.

use std::io::Write;
use std::path::Path;

use crate::error::MlError;
use crate::params::GbdtParams;

/// How a missing value is spelled in the data file.
///
/// LightGBM's own `Common::Atof` recognises `nan` (case-insensitively) and maps it to the missing
/// value its `missing_type` handling then routes. ⚠ This is PROVEN rather than assumed: Task 13's
/// smoke writes a row carrying it, predicts, and asserts the walker and LightGBM agree on where
/// that row lands. If the proof ever fails, the fix is this constant plus that test — never a
/// tolerance.
pub const MISSING_TOKEN: &str = "nan";

/// Which of LightGBM's tasks a config describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Train,
    Predict,
    /// Bin a data file once into LightGBM's binary dataset format. See [`crate::grid`] for why
    /// this is the difference between a 30-minute grid and an 11-hour one. Which fits the binary
    /// may then serve is decided HERE, at construction — see [`BinPreFilter`].
    SaveBinary(BinPreFilter),
}

/// How a [`Task::SaveBinary`] treats LightGBM's construction-time `min_data_in_leaf` pre-filter —
/// the ONE searched training parameter that is also consumed when the dataset is BUILT.
///
/// Under LightGBM's default `feature_pre_filter=true`, binning drops every feature that cannot
/// produce a split satisfying `min_data_in_leaf` — and a dropped feature is not merely
/// unsplittable, it leaves the `feature_fraction` sampling universe, so the pre-filter changes the
/// TREES of any fit whose `feature_fraction < 1`. Measured on the real binary (2026-08-11, the
/// probe behind `crates/vike-ml/src/gbdt_learner.rs`'s per-fold reuse): a bin built at `min_data_in_leaf=10` and trained
/// at `50` differs from the plain-CSV fit at `50` (`feature_infos` shows the pre-filtered column
/// as `[0:1]` vs `none`), and so does a `feature_pre_filter=false` bin — while a bin built at the
/// fit's OWN `min_data_in_leaf` reproduces the CSV fit byte-for-byte (only the `[data:]` path echo
/// differs). Hence two modes, one per legitimate reuse shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinPreFilter {
    /// `feature_pre_filter=false`: one bin serves fits that SWEEP `min_data_in_leaf` —
    /// [`crate::grid`]'s bin-once-fit-many shape, where every fit shares the bins and the sweep
    /// axes must not be baked into them. ⚠ The trees are NOT byte-identical to what per-fit CSV
    /// construction would produce whenever the default pre-filter would have dropped a feature —
    /// acceptable for a self-consistent grid, wrong for a reuse that promises CSV parity.
    MinDataAgnostic,
    /// LightGBM's default pre-filter, with the fit's own `min_data_in_leaf` written into the
    /// binning config: construction is byte-identical to what a plain CSV fit at that value
    /// performs, so a fit FROM this bin reproduces the CSV fit bit-for-bit. The bin may only serve
    /// fits at that same `min_data_in_leaf` — a caller amortizing across a search keys its cache
    /// on the value (`crates/vike-ml/src/gbdt_learner.rs`'s `GbdtLearner` does exactly that).
    MatchFit,
}

impl Task {
    fn as_str(self) -> &'static str {
        match self {
            Self::Train => "train",
            Self::Predict => "predict",
            Self::SaveBinary(_) => "save_binary",
        }
    }
}

/// A row-major feature matrix, its labels and its schema.
///
/// `f64` features and `f32` labels is LightGBM's own shape, kept even though the wire is now text:
/// a caller that already has these slices for one backend has them for the other.
///
/// # This type is a MERGE of two that had the same job
///
/// It carried `flat_x`/`labels`/`n_features` and a validating constructor and nothing else, while
/// the study that first needed a learner carried a second `TrainData` of its own — the same three
/// facts under different names, plus [`TrainData::categorical`], an explicit
/// [`TrainData::n_rows`], [`TrainData::row`], [`TrainData::split_at_row`] and `Copy`. The two met
/// at one adapter, which imported this one under an alias and rebuilt the labels into a fresh
/// `Vec<f32>` on every fit to cross between them. Neither half was wrong and neither was
/// complete, so this is both: those accessors and that schema, on this crate's validation.
///
/// ⚠ **Both construction styles are supported deliberately, and they are not equivalent.**
/// [`TrainData::new`] derives the row count and validates before anything is written or spawned;
/// a struct literal is what a caller that just packed its own buffers writes, and it can state a
/// [`TrainData::n_rows`] its buffer does not have. [`TrainData::validate`] is the same check
/// reachable on a literal-built value, and a producer that spawns a trainer should run it: the
/// CSV writer derives its own row count from `x.len() / n_cols` and never reads `n_rows`, so a
/// disagreement trains a different number of rows than the caller believes — quietly.
#[derive(Clone, Copy)]
pub struct TrainData<'a> {
    /// `n_rows * n_cols` values, row-major.
    ///
    /// ⚠ A consumer reads this WHOLESALE and derives its own row count from `x.len() / n_cols`,
    /// so this slice must span EXACTLY these rows — see
    /// `a_split_half_carries_only_its_own_rows_in_the_flat_buffer`.
    pub x: &'a [f64],
    /// One label per row. `f32` because that is LightGBM's own label width; a binary caller
    /// writes `0.0`/`1.0` and loses nothing, and a regression caller is not shut out by a type
    /// chosen for the binary case.
    pub y: &'a [f32],
    pub n_rows: usize,
    pub n_cols: usize,
    /// Column indices the trainer must treat as CATEGORICAL — FEATURE positions, with the label
    /// column NOT counted, which is [`TrainData::write_csv`]'s convention and LightGBM's own.
    ///
    /// Empty declares nothing, which is not the same as declaring an empty list — see
    /// [`GbdtParams::categorical_feature_csv`].
    pub categorical: &'a [usize],
}

impl<'a> TrainData<'a> {
    /// Validate the shape ONCE, here, rather than letting a child process discover it and report
    /// it as a parse error forty lines into a file. Declares no categorical column — see
    /// [`TrainData::with_categorical`].
    pub fn new(x: &'a [f64], y: &'a [f32], n_cols: usize) -> Result<Self, MlError> {
        if n_cols == 0 {
            return Err(MlError::Shape("n_cols must be > 0".into()));
        }
        if !x.len().is_multiple_of(n_cols) {
            return Err(MlError::Shape(format!(
                "{} values is not a whole number of {n_cols}-wide rows",
                x.len()
            )));
        }
        let d = Self { x, y, n_rows: x.len() / n_cols, n_cols, categorical: &[] };
        d.validate()?;
        Ok(d)
    }

    /// The same matrix with `categorical` declared, refusing an index outside a row.
    pub fn with_categorical(self, categorical: &'a [usize]) -> Result<Self, MlError> {
        let d = Self { categorical, ..self };
        d.validate()?;
        Ok(d)
    }

    /// The shape check [`TrainData::new`] runs, reachable on a value built as a struct literal.
    ///
    /// ⚠ The categorical rule is a REFUSAL rather than a clamp because LightGBM ignores an
    /// out-of-range categorical index in SILENCE and bins the column as a number — and the saved
    /// model's parameter echo still claims the column was categorical, so the mistake is invisible
    /// in the artifact too.
    pub fn validate(&self) -> Result<(), MlError> {
        if self.n_cols == 0 {
            return Err(MlError::Shape(
                "n_cols is 0: a fit needs at least one feature column".into(),
            ));
        }
        let want = self.n_rows.checked_mul(self.n_cols).ok_or_else(|| {
            MlError::Shape(format!("{} rows x {} cols overflows usize", self.n_rows, self.n_cols))
        })?;
        if self.x.len() != want {
            return Err(MlError::Shape(format!(
                "{} feature values for {} rows x {} cols (expected {want}); a trainer derives its \
                 own row count from the buffer and would train a different number of rows",
                self.x.len(),
                self.n_rows,
                self.n_cols
            )));
        }
        if self.y.len() != self.n_rows {
            return Err(MlError::Shape(format!(
                "{} rows but {} labels",
                self.n_rows,
                self.y.len()
            )));
        }
        if let Some(bad) = self.categorical.iter().find(|&&c| c >= self.n_cols) {
            return Err(MlError::Shape(format!(
                "categorical column {bad} is outside a {}-wide row; LightGBM ignores an \
                 out-of-range index in SILENCE and bins the column as a number",
                self.n_cols
            )));
        }
        Ok(())
    }

    /// Row `i` as a contiguous slice of the flat buffer.
    pub fn row(&self, i: usize) -> &'a [f64] {
        &self.x[i * self.n_cols..(i + 1) * self.n_cols]
    }

    /// Rows `[0, cut)` and `[cut, n_rows)`, sharing the same schema.
    ///
    /// A `cut` past the end yields an empty second half rather than panicking, so a caller that
    /// computed its split from a shorter axis degrades instead of aborting.
    pub fn split_at_row(&self, cut: usize) -> (TrainData<'a>, TrainData<'a>) {
        let c = cut.min(self.n_rows);
        (
            TrainData {
                x: &self.x[..c * self.n_cols],
                y: &self.y[..c],
                n_rows: c,
                n_cols: self.n_cols,
                categorical: self.categorical,
            },
            TrainData {
                x: &self.x[c * self.n_cols..],
                y: &self.y[c..],
                n_rows: self.n_rows - c,
                n_cols: self.n_cols,
                categorical: self.categorical,
            },
        )
    }

    /// Write the training file: label first, then the features, comma separated, no header.
    ///
    /// This matches LightGBM's defaults (`label_column=0`, `header=false`) so neither has to be
    /// stated in the config, and it is why [`GbdtParams::categorical_feature`] indices are FEATURE
    /// indices with the label uncounted — LightGBM applies the same rule.
    pub fn write_csv<W: Write>(&self, w: &mut W) -> Result<(), MlError> {
        for (row, label) in self.x.chunks_exact(self.n_cols).zip(self.y) {
            write_field(w, f64::from(*label))?;
            for v in row {
                io(w.write_all(b","))?;
                write_field(w, *v)?;
            }
            io(w.write_all(b"\n"))?;
        }
        Ok(())
    }
}

/// The `task=predict` twin — same column shape as [`TrainData::write_csv`], with a DUMMY label.
///
/// ⚠ **The dummy label column is mandatory, not cosmetic.** `Predictor::Predict` sets
/// `label_idx = header ? -1 : boosting_->LabelIdx()`, which is `0` for these models, and
/// `CSVParser::NumFeatures()` returns `total_columns_ - (label_idx_ >= 0)`. So a label-less
/// prediction file makes an N-feature model see N-1 features, trip the shape check and
/// `Log::Fatal`. Upstream's own `examples/binary_classification/binary.test` carries a label
/// column for the same reason. The VALUE is never read — only the column position matters — so it
/// is written as `0`.
///
/// ⚠⚠ `predict_disable_shape_check=true` is NOT the fix. It silences the error without changing
/// the parse: column 0 is still consumed as the label, every feature shifts down by one, the last
/// feature is dropped, and the model returns a full set of plausible probabilities computed from
/// the wrong columns.
pub fn write_predict_csv<W: Write>(
    flat_x: &[f64],
    n_features: usize,
    w: &mut W,
) -> Result<(), MlError> {
    if n_features == 0 || !flat_x.len().is_multiple_of(n_features) {
        return Err(MlError::Shape(format!(
            "{} values is not a whole number of {n_features}-wide rows",
            flat_x.len()
        )));
    }
    for row in flat_x.chunks_exact(n_features) {
        io(w.write_all(b"0"))?; // the dummy label — position matters, value does not
        for v in row {
            io(w.write_all(b","))?;
            write_field(w, *v)?;
        }
        io(w.write_all(b"\n"))?;
    }
    Ok(())
}

/// One value: [`MISSING_TOKEN`] for NaN, otherwise Rust's shortest round-tripping decimal.
///
/// `{}` on an `f64` emits the shortest decimal string that parses back to the SAME BITS. That is
/// half of a lossless wire; the other half is LightGBM's parser, which **is not correctly rounded
/// by default** — `precise_float_parser=false` is the default and routes to a hand-rolled
/// `Common::Atof` that clamps `expon` at 308. [`TrainConfig::render`] therefore writes
/// `precise_float_parser=true` on every text-reading task, and the wire is lossless BECAUSE WE ASK
/// FOR IT, not because both sides happen to be careful. Do not swap in a hand-rolled "fast" float
/// formatter here either: the equality gate depends on this end to end.
fn write_field<W: Write>(w: &mut W, v: f64) -> Result<(), MlError> {
    if v.is_nan() { io(w.write_all(MISSING_TOKEN.as_bytes())) } else { io(write!(w, "{v}")) }
}

fn io(r: std::io::Result<()>) -> Result<(), MlError> {
    r.map_err(|e| MlError::Io(format!("writing the data file: {e}")))
}

/// One LightGBM invocation, expressed as its config text.
pub struct TrainConfig<'a> {
    pub task: Task,
    pub params: &'a GbdtParams,
    /// The input data file: a CSV for a first fit, or a `save_binary` output for every later one.
    pub data: &'a Path,
    /// Where the trained model goes. REQUIRED for [`Task::Train`] — see `render`.
    pub output_model: Option<&'a Path>,
    /// The model to load: a `predict` task's subject, or a warm start.
    pub input_model: Option<&'a Path>,
    /// Where per-row predictions go, for [`Task::Predict`].
    pub output_result: Option<&'a Path>,
    /// `false` (the default, and what [`crate::cli::LightGbmCli::predict`] passes) gives the
    /// objective's transformed output, i.e. `predict_proba`. `true` gives the PRE-transform margin,
    /// which is what the exact-equality half of the gate compares.
    ///
    /// Always written explicitly on a predict task, never left to the default: a margin and a
    /// probability are both plausible-looking numbers, and confusing them is silent.
    pub predict_raw_score: bool,
}

impl TrainConfig<'_> {
    /// Render the config file LightGBM reads.
    ///
    /// # What is deliberately not left to a default
    ///
    /// * **`output_model`** — LightGBM defaults it to `LightGBM_model.txt` in the WORKING
    ///   DIRECTORY. With 32 concurrent workers in one scratch root that is a collision, and the
    ///   symptom is a model file holding somebody else's fit. A `Task::Train` with no
    ///   `output_model` is refused.
    /// * **`num_threads`** — always written, always `1`. The binary is built without OpenMP so the
    ///   cliff cannot exist, and this line means a binary rebuilt WITH it does not resurrect a 68x
    ///   regression silently.
    /// * **`predict_raw_score`** — written explicitly as `false` on a predict task, because it is
    ///   the difference between a probability and a margin and both look like plausible numbers.
    pub fn render(&self) -> Result<String, MlError> {
        let p = self.params;
        let mut out = String::new();
        out.push_str("# written by vike-ml — do not edit; see crates/vike-ml/src/train.rs\n");
        out.push_str(&format!("task={}\n", self.task.as_str()));
        out.push_str(&format!("data={}\n", path_value(self.data, "data")?));

        match self.task {
            Task::Train => {
                let Some(model) = self.output_model else {
                    return Err(MlError::Shape(
                        "a train task must name `output_model`: LightGBM's default writes \
                         LightGBM_model.txt into the working directory, which collides the moment \
                         two fits share a scratch root"
                            .into(),
                    ));
                };
                out.push_str(&format!("output_model={}\n", path_value(model, "output_model")?));
                out.push_str(&format!("objective={}\n", p.objective));
                out.push_str(&format!("num_iterations={}\n", p.num_iterations));
                out.push_str(&format!("learning_rate={}\n", p.learning_rate));
                out.push_str(&format!("num_leaves={}\n", p.num_leaves));
                out.push_str(&format!("max_depth={}\n", p.max_depth));
                out.push_str(&format!("min_data_in_leaf={}\n", p.min_data_in_leaf));
                out.push_str(&format!("feature_fraction={}\n", p.feature_fraction));
                out.push_str(&format!("bagging_fraction={}\n", p.bagging_fraction));
                out.push_str(&format!("bagging_freq={}\n", p.bagging_freq));
                out.push_str(&format!("lambda_l1={}\n", p.lambda_l1));
                out.push_str(&format!("lambda_l2={}\n", p.lambda_l2));
                out.push_str(&format!("seed={}\n", p.seed));
                out.push_str(&format!("deterministic={}\n", p.deterministic));
                out.push_str(&format!("force_row_wise={}\n", p.force_row_wise));
                if let Some(cf) = &p.categorical_feature {
                    out.push_str(&format!("categorical_feature={cf}\n"));
                }
            }
            Task::Predict => {
                let Some(model) = self.input_model else {
                    return Err(MlError::Shape("a predict task must name `input_model`".into()));
                };
                let Some(result) = self.output_result else {
                    return Err(MlError::Shape("a predict task must name `output_result`".into()));
                };
                out.push_str(&format!("input_model={}\n", path_value(model, "input_model")?));
                out.push_str(&format!("output_result={}\n", path_value(result, "output_result")?));
                out.push_str(&format!("predict_raw_score={}\n", self.predict_raw_score));
            }
            Task::SaveBinary(pre_filter) => {
                if let Some(cf) = &p.categorical_feature {
                    // ⚠ Load-bearing, and the whole reason the FFI bindings were disqualified:
                    // LightGBM decides categorical-vs-numeric binning AT DATASET CONSTRUCTION.
                    // A save_binary that omits this produces an all-numeric binary that every
                    // later fit silently inherits, however loudly those fits declare the column.
                    out.push_str(&format!("categorical_feature={cf}\n"));
                }
                // ⚠ Load-bearing for the same reason, and the SECOND construction-time decision
                // every fit from this binary inherits. LightGBM's docs, verbatim: "as dataset
                // object is initialized only once and cannot be changed after that, you may need
                // to set this to false when searching parameters with min_data_in_leaf, otherwise
                // features are filtered by min_data_in_leaf firstly if you don't reconstruct
                // dataset object" — and the mismatch is SILENT: `DatasetLoader::CheckDataset`
                // validates min_data_in_bin, max_bin, use_missing and zero_as_missing, and
                // `min_data_in_leaf` is not in that list. Which way to bake it is the caller's
                // declared [`BinPreFilter`]; both lines belong HERE and nowhere else, because the
                // pre-filter is consumed at construction and writing either into a fit config
                // accomplishes exactly nothing.
                match pre_filter {
                    BinPreFilter::MinDataAgnostic => out.push_str("feature_pre_filter=false\n"),
                    BinPreFilter::MatchFit => {
                        out.push_str(&format!("min_data_in_leaf={}\n", p.min_data_in_leaf));
                    }
                }
            }
        }

        if let (Task::Train | Task::SaveBinary(_), Some(model)) = (self.task, self.input_model) {
            out.push_str(&format!("input_model={}\n", path_value(model, "input_model")?));
        }
        out.push_str(&format!("num_threads={}\n", p.num_threads));
        out.push_str(&format!("verbosity={}\n", p.verbosity));
        // ⚠ Defaults to FALSE, and only `true` routes text->f64 through `AtofPrecise`. The default
        // `Common::Atof` is hand-rolled, clamps `expon` at 308, and is not correctly rounded — so a
        // value near a split threshold can route differently in LightGBM than in the walker, and
        // the equality gate would then report a routing bug that does not exist. Written for every
        // task: a binned dataset inherits whatever the CSV parse produced, so `save_binary` needs
        // it as much as `train` does. Free in the grid, where fits read the `.bin` and parse no text.
        out.push_str("precise_float_parser=true\n");
        Ok(out)
    }
}

/// A path as a config value, refusing the two shapes that would corrupt the file.
fn path_value<'a>(path: &'a Path, key: &str) -> Result<&'a str, MlError> {
    let s = path.to_str().ok_or_else(|| {
        MlError::Io(format!("`{key}` path is not valid UTF-8: {}", path.display()))
    })?;
    if s.contains('\n') || s.contains('\r') {
        return Err(MlError::Io(format!("`{key}` path contains a newline: {s:?}")));
    }
    Ok(s)
}

#[path = "train_tests.rs"]
#[cfg(test)]
mod train_tests;
