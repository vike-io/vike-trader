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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn conf_for(params: &GbdtParams) -> String {
        TrainConfig {
            task: Task::Train,
            params,
            data: Path::new("/tmp/fold.bin"),
            output_model: Some(Path::new("/tmp/model.txt")),
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .unwrap()
    }

    fn line<'a>(conf: &'a str, key: &str) -> Option<&'a str> {
        conf.lines().find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
    }

    #[test]
    fn train_data_accepts_a_well_shaped_matrix() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let y = [0.0f32, 1.0];
        let d = TrainData::new(&x, &y, 3).unwrap();
        assert_eq!(d.n_rows, 2);
    }

    #[test]
    fn train_data_refuses_a_ragged_matrix() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0];
        assert!(matches!(TrainData::new(&x, &y, 3), Err(MlError::Shape(_))));
    }

    #[test]
    fn train_data_refuses_a_label_count_that_does_not_match_the_rows() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let y = [0.0f32];
        assert!(matches!(TrainData::new(&x, &y, 3), Err(MlError::Shape(_))));
    }

    #[test]
    fn train_data_refuses_zero_features() {
        assert!(matches!(TrainData::new(&[], &[], 0), Err(MlError::Shape(_))));
    }

    /// The seam's fixture, moved with the accessor it exercises: 6 rows x 3 cols, row-major.
    fn seam_data() -> (Vec<f64>, Vec<f32>) {
        ((0..18).map(|v| v as f64).collect(), vec![0.0, 1.0, 0.0, 1.0, 1.0, 0.0])
    }

    #[test]
    fn a_row_is_a_contiguous_slice_of_the_row_major_buffer() {
        let (x, y) = seam_data();
        let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
        assert_eq!(d.row(0), &[0.0, 1.0, 2.0]);
        assert_eq!(d.row(5), &[15.0, 16.0, 17.0]);
    }

    #[test]
    fn splitting_at_a_row_splits_both_the_features_and_the_labels() {
        let (x, y) = seam_data();
        let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
        let (a, b) = d.split_at_row(4);
        assert_eq!((a.n_rows, b.n_rows), (4, 2));
        assert_eq!(a.y, &[0.0, 1.0, 0.0, 1.0]);
        assert_eq!(b.y, &[1.0, 0.0]);
        assert_eq!(b.row(0), &[12.0, 13.0, 14.0]);
        assert_eq!(a.categorical, b.categorical, "the schema does not change across a split");
    }

    /// The assertion the test above CANNOT make, and the one a real trainer depends on.
    ///
    /// A `split_at_row` that handed the first half the parent's whole `x` buffer passes every
    /// assertion above: `a.row(0..4)` reads the same bytes either way, and `a.y` is already
    /// correct. The difference is only visible in `a.x.len()` — and [`TrainData::write_csv`]
    /// derives its own row count from `x.len() / n_cols`, so that half would train 6 rows of
    /// features against 4 labels. This is the assertion that says the flat buffer, not just the
    /// row accessor, was cut.
    #[test]
    fn a_split_half_carries_only_its_own_rows_in_the_flat_buffer() {
        let (x, y) = seam_data();
        let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
        let (a, b) = d.split_at_row(4);
        assert_eq!(a.x.len(), a.n_rows * a.n_cols);
        assert_eq!(b.x.len(), b.n_rows * b.n_cols);
        assert_eq!(a.y.len(), a.n_rows);
        assert_eq!(b.y.len(), b.n_rows);
        assert_eq!(b.x, &[12.0, 13.0, 14.0, 15.0, 16.0, 17.0]);
        // ...and both halves are still well-shaped, which is what makes a half directly fittable.
        a.validate().unwrap();
        b.validate().unwrap();
    }

    #[test]
    fn splitting_at_either_end_yields_an_empty_half_rather_than_panicking() {
        let (x, y) = seam_data();
        let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[] };
        let (a, b) = d.split_at_row(0);
        assert_eq!((a.n_rows, a.x.len(), a.y.len()), (0, 0, 0));
        assert_eq!(b.n_rows, 6);
        // A cut past the end is CLAMPED — a caller that computed it from a shorter axis gets an
        // empty second half, not a slice panic.
        let (a, b) = d.split_at_row(99);
        assert_eq!(a.n_rows, 6);
        assert_eq!((b.n_rows, b.x.len(), b.y.len()), (0, 0, 0));
    }

    /// The half of the merge that a struct literal can get wrong and [`TrainData::new`] cannot:
    /// an `n_rows` the buffer does not have. The CSV writer never reads the field, so this is the
    /// only place the disagreement is visible before a child process trains the wrong row count.
    #[test]
    fn validate_refuses_a_row_count_the_flat_buffer_does_not_have() {
        let (x, y) = seam_data();
        let honest = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[] };
        honest.validate().unwrap();
        let lying = TrainData { n_rows: 4, ..honest };
        let err = lying.validate().unwrap_err().to_string();
        assert!(err.contains("expected 12"), "{err}");
        // ...the labels are checked against the same declared count...
        let err = TrainData { y: &y[..5], ..honest }.validate().unwrap_err().to_string();
        assert!(err.contains("6 rows but 5 labels"), "{err}");
        // ...and a zero-width matrix is refused rather than divided by.
        let err = TrainData { x: &[], y: &[], n_rows: 0, n_cols: 0, categorical: &[] }
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("n_cols is 0"), "{err}");
    }

    #[test]
    fn a_categorical_index_outside_the_row_is_refused_rather_than_ignored() {
        // LightGBM ignores an out-of-range index in silence, bins the column as a NUMBER, and the
        // saved model's parameter echo still claims the column was categorical.
        let (x, y) = seam_data();
        let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[3] };
        let err = d.validate().unwrap_err().to_string();
        assert!(err.contains("categorical column 3"), "{err}");
        assert!(TrainData::new(&x, &y, 3).unwrap().with_categorical(&[3]).is_err());
        assert!(TrainData::new(&x, &y, 3).unwrap().with_categorical(&[2]).is_ok());
    }

    /// `new` derives the row count rather than taking one, so the disagreement above is
    /// unrepresentable on that path — and it declares NO categorical column, which is not the
    /// same as declaring an empty one.
    #[test]
    fn new_derives_the_row_count_and_declares_no_categorical_column() {
        let (x, y) = seam_data();
        let d = TrainData::new(&x, &y, 3).unwrap();
        assert_eq!(d.n_rows, 6);
        assert_eq!(d.n_cols, 3);
        assert!(d.categorical.is_empty());
    }

    #[test]
    fn the_data_file_puts_the_label_first_and_writes_no_header() {
        let x = [1.0, 2.0, 3.0, 4.0];
        let y = [0.0f32, 1.0];
        let mut out = Vec::new();
        TrainData::new(&x, &y, 2).unwrap().write_csv(&mut out).unwrap();
        // `label_column=0` is LightGBM's own default, and `header=false` means row 1 is data.
        assert_eq!(String::from_utf8(out).unwrap(), "0,1,2\n1,3,4\n");
    }

    #[test]
    fn the_writer_emits_the_shortest_decimal_that_round_trips_the_bits() {
        // ⚠ This tests OUR HALF ONLY, and says so, because the obvious version of this test is a
        // tautology: formatting with Rust's `{}` and parsing back with Rust's parser proves a
        // property of Rust and nothing about LightGBM. LightGBM's side is NOT correctly rounded by
        // default — `precise_float_parser` defaults to false and the default `Common::Atof` clamps
        // `expon` at 308 — which is why `render` writes `precise_float_parser=true` (asserted
        // below) and why the END-TO-END lossless claim is proven by the equality gate against the
        // real binary in Task 15, not here.
        let hard = [0.1, 1.0 / 3.0, f64::MIN_POSITIVE, 1e308, -0.0, 5e-324];
        let y = vec![0.0f32; hard.len()];
        let mut out = Vec::new();
        TrainData::new(&hard, &y, 1).unwrap().write_csv(&mut out).unwrap();
        let back: Vec<f64> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| l.split(',').nth(1).unwrap().parse::<f64>().unwrap())
            .collect();
        for (a, b) in hard.iter().zip(&back) {
            assert_eq!(a.to_bits(), b.to_bits(), "{a} did not survive our own formatter");
        }
    }

    #[test]
    fn a_prediction_file_carries_a_dummy_label_column_in_the_training_files_shape() {
        // ⚠ THE trap that would have killed the whole gate. `Predictor::Predict` sets
        // `label_idx = header ? -1 : boosting_->LabelIdx()` — 0 by default — and
        // `CSVParser::NumFeatures()` returns `total_columns_ - (label_idx_ >= 0)`. A label-less
        // prediction file therefore makes an N-feature model see N-1 features and Log::Fatal.
        // Upstream's own examples/binary_classification/binary.test carries a label column.
        let mut out = Vec::new();
        write_predict_csv(&[1.0, 2.0, 3.0, 4.0], 2, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "0,1,2\n0,3,4\n");
    }

    #[test]
    fn a_prediction_file_and_a_training_file_have_the_same_column_count() {
        // The property that actually matters, stated as a property rather than as two literals:
        // whatever the trainer saw, the predictor must see.
        let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let y = [0.0f32, 1.0];
        let mut train = Vec::new();
        TrainData::new(&x, &y, 3).unwrap().write_csv(&mut train).unwrap();
        let mut pred = Vec::new();
        write_predict_csv(&x, 3, &mut pred).unwrap();
        let cols = |b: &[u8]| String::from_utf8_lossy(b).lines().next().unwrap().split(',').count();
        assert_eq!(cols(&train), cols(&pred));
    }

    #[test]
    fn write_predict_csv_refuses_a_ragged_matrix() {
        assert!(matches!(
            write_predict_csv(&[1.0, 2.0, 3.0], 2, &mut Vec::new()),
            Err(MlError::Shape(_))
        ));
        assert!(matches!(write_predict_csv(&[], 0, &mut Vec::new()), Err(MlError::Shape(_))));
    }

    #[test]
    fn every_text_reading_task_asks_for_the_precise_float_parser() {
        // `precise_float_parser` defaults to FALSE, and only `true` routes through AtofPrecise.
        // The default Common::Atof is hand-rolled, clamps expon at 308 and is not correctly
        // rounded — so a value near a split threshold could route differently in LightGBM than in
        // the walker, and the equality gate would report a routing bug that does not exist.
        let p = GbdtParams::default();
        for task in [
            Task::Train,
            Task::Predict,
            Task::SaveBinary(BinPreFilter::MinDataAgnostic),
            Task::SaveBinary(BinPreFilter::MatchFit),
        ] {
            let conf = TrainConfig {
                task,
                params: &p,
                data: Path::new("/tmp/d.csv"),
                output_model: Some(Path::new("/tmp/m.txt")),
                input_model: Some(Path::new("/tmp/m.txt")),
                output_result: Some(Path::new("/tmp/r.txt")),
                predict_raw_score: false,
            }
            .render()
            .unwrap();
            assert_eq!(line(&conf, "precise_float_parser"), Some("true"), "{task:?}");
        }
    }

    #[test]
    fn binning_disables_feature_pre_filter_because_the_whole_grid_inherits_the_bins() {
        // ⚠ The SECOND binning decision every fit from the binary inherits, and the silent one.
        // LightGBM's docs, verbatim: "as dataset object is initialized only once and cannot be
        // changed after that, you may need to set this to false when searching parameters with
        // min_data_in_leaf, otherwise features are filtered by min_data_in_leaf firstly if you
        // don't reconstruct dataset object". `min_data_in_leaf` IS a grid axis here (Tasks 9/10),
        // and `DatasetLoader::CheckDataset` validates min_data_in_bin / max_bin / use_missing /
        // zero_as_missing — NOT min_data_in_leaf — so a mismatch never says anything.
        //
        // It is a CONSTRUCTION-time parameter: putting it in the fit config does nothing at all.
        let conf = TrainConfig {
            task: Task::SaveBinary(BinPreFilter::MinDataAgnostic),
            params: &GbdtParams::default(),
            data: Path::new("/tmp/d.csv"),
            output_model: None,
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .unwrap();
        assert_eq!(line(&conf, "feature_pre_filter"), Some("false"));
        assert_eq!(
            line(&conf, "min_data_in_leaf"),
            None,
            "an agnostic bin must not bake the value it exists to stay agnostic of"
        );
    }

    #[test]
    fn a_match_fit_binning_bakes_the_fits_min_data_in_leaf_and_keeps_the_default_pre_filter() {
        // The other arm: construction byte-identical to what a plain CSV fit performs, which means
        // the fit's own `min_data_in_leaf` under LightGBM's DEFAULT pre-filter. Writing
        // `feature_pre_filter=false` here — or omitting `min_data_in_leaf` — would each produce a
        // binary whose trees differ from the CSV fit's whenever the pre-filter bites, measured on
        // the real binary (see [`BinPreFilter`]'s doc). The parity claim itself is gated against
        // the real binary by `crates/vike-ml/tests/gbdt_learner_smoke.rs`'s matrix smoke.
        let params = GbdtParams { min_data_in_leaf: 50, ..GbdtParams::default() };
        let conf = TrainConfig {
            task: Task::SaveBinary(BinPreFilter::MatchFit),
            params: &params,
            data: Path::new("/tmp/d.csv"),
            output_model: None,
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .unwrap();
        assert_eq!(line(&conf, "min_data_in_leaf"), Some("50"));
        assert_eq!(
            line(&conf, "feature_pre_filter"),
            None,
            "the default pre-filter IS the CSV fit's construction — writing false here is the \
             measured tree-changing divergence, and writing true would be a redundant restatement \
             that could drift from upstream's default"
        );
    }

    #[test]
    fn a_missing_value_is_written_as_the_token_lightgbm_reads_as_missing() {
        let x = [f64::NAN, 1.0];
        let y = [0.0f32];
        let mut out = Vec::new();
        TrainData::new(&x, &y, 2).unwrap().write_csv(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), format!("0,{MISSING_TOKEN},1\n"));
    }

    #[test]
    fn the_config_pins_num_threads_to_one() {
        // ⚠ THE headline measurement of the backend bakeoff: on the 600x943 shape this study
        // trains, LightGBM's default (`num_threads` = all cores) measured 143,293 ms against
        // 2,105 ms at one thread — a 68x CLIFF, in the wrong direction, silently. The binary is
        // built `-DUSE_OPENMP=OFF` so the cliff cannot exist (Task 12), and the config says it
        // anyway so that a rebuild WITH OpenMP does not resurrect it. Two independent defences,
        // because this one costs weeks of wall clock when it goes wrong.
        assert_eq!(line(&conf_for(&GbdtParams::default()), "num_threads"), Some("1"));
    }

    #[test]
    fn the_config_carries_the_determinism_knobs() {
        let conf = conf_for(&GbdtParams::default());
        assert_eq!(line(&conf, "deterministic"), Some("true"));
        assert_eq!(line(&conf, "force_row_wise"), Some("true"));
        assert!(line(&conf, "seed").is_some());
    }

    #[test]
    fn a_categorical_feature_reaches_the_config_with_no_brackets() {
        let params = GbdtParams {
            categorical_feature: Some(GbdtParams::categorical_feature_csv(&[0, 3])),
            ..GbdtParams::default()
        };
        assert_eq!(line(&conf_for(&params), "categorical_feature"), Some("0,3"));
        assert!(!conf_for(&params).contains('['), "KV2Map strips quotes, not brackets");
    }

    #[test]
    fn no_categorical_feature_means_the_key_is_absent_rather_than_empty() {
        // `categorical_feature=` with an empty value is not the same as not saying it, and an
        // empty list is a value LightGBM has to interpret. Say nothing instead.
        assert_eq!(line(&conf_for(&GbdtParams::default()), "categorical_feature"), None);
    }

    #[test]
    fn each_task_names_the_files_that_task_actually_uses() {
        let p = GbdtParams::default();
        let train = conf_for(&p);
        assert_eq!(line(&train, "task"), Some("train"));
        assert_eq!(line(&train, "output_model"), Some("/tmp/model.txt"));
        assert_eq!(line(&train, "input_model"), None);

        let predict_conf = |raw: bool| {
            TrainConfig {
                task: Task::Predict,
                params: &p,
                data: Path::new("/tmp/eval.csv"),
                output_model: None,
                input_model: Some(Path::new("/tmp/model.txt")),
                output_result: Some(Path::new("/tmp/pred.txt")),
                predict_raw_score: raw,
            }
            .render()
            .unwrap()
        };

        let predict = predict_conf(false);
        assert_eq!(line(&predict, "task"), Some("predict"));
        assert_eq!(line(&predict, "input_model"), Some("/tmp/model.txt"));
        assert_eq!(line(&predict, "output_result"), Some("/tmp/pred.txt"));
        assert_eq!(
            line(&predict, "predict_raw_score"),
            Some("false"),
            "`false` is what makes this predict_proba rather than the margin"
        );
        assert_eq!(
            line(&predict_conf(true), "predict_raw_score"),
            Some("true"),
            "and `true` is the pre-transform margin the exact-equality gate compares"
        );
    }

    #[test]
    fn a_train_task_with_no_output_model_is_refused_rather_than_writing_somewhere_default() {
        // LightGBM's own default is `output_model=LightGBM_model.txt` IN THE WORKING DIRECTORY.
        // With 32 workers in one scratch root that is a collision, not a default.
        let err = TrainConfig {
            task: Task::Train,
            params: &GbdtParams::default(),
            data: Path::new("/tmp/fold.bin"),
            output_model: None,
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .unwrap_err();
        assert!(err.to_string().contains("output_model"), "{err}");
    }

    #[test]
    fn a_path_that_would_break_the_line_format_is_refused() {
        let err = TrainConfig {
            task: Task::Train,
            params: &GbdtParams::default(),
            data: Path::new("/tmp/two\nlines"),
            output_model: Some(Path::new("/tmp/model.txt")),
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .unwrap_err();
        assert!(err.to_string().contains("newline"), "{err}");
    }
}
