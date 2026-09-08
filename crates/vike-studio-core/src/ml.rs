//! A SAVED model becomes a probability series, and says which of its inputs mattered.
//!
//! [`ScoringModel`] is this crate's whole ML surface: hand it the TEXT of a model somebody already
//! trained and it scores rows through `vike_ml::ProbaModel` and reports per-feature importance.
//! Nothing here fits anything.
//!
//! # Why the surface stops at inference, and why that is the useful half
//!
//! Not a scoping preference — a platform fact, measured and already in the tree.
//! `scripts/fetch_release_tools.sh`'s `platform_default_tools` skips the LightGBM binary on every
//! non-Linux host, in its own words because it is "a LINUX x86-64 ELF" and "fetching it onto Windows
//! or macOS installs a file nothing there can execute". So a Studio user on Windows or macOS has no
//! trainer binary and cannot fit a model at all, while
//! `crates/vike-ml/src/train.rs`'s `TrainConfig` DRIVES that binary as a child process — a training
//! surface here would be dead code on the platform this crate's own GUI shell mostly runs on.
//! Inference is the opposite: `crates/vike-ml/src/infer.rs`'s `raw_score` and
//! `crates/vike-ml/src/importance.rs`'s `importance_from_model_text` are pure Rust over the model
//! TEXT and work identically everywhere, with nothing behind them. So the model is fitted wherever
//! it can be fitted, and Studio reads what came out.
//!
//! # Why loading parses BOTH halves and refuses as one
//!
//! A [`ScoringModel`] carries the walked model AND its importance table, parsed in one load from
//! one text, and a text whose two halves disagree is refused rather than half-loaded. Three
//! reasons, in order of how much they matter:
//!
//! * **The panel asks both questions.** "Which input mattered" is the first question of any ML
//!   panel and "what does it predict" is the second; a type that answers one and defers the other
//!   makes every caller carry an error path into the middle of a table render.
//! * **The disagreement set is exactly the doctored file.** LightGBM writes `split_feature=` and
//!   `split_gain=` for every tree it emits — even a constant one, which writes both EMPTY (verified
//!   against the pinned binary; `crates/vike-ml/src/importance.rs`'s module doc carries the
//!   provenance). So for any text the pinned trainer produced, both parses succeed together. One
//!   that pairs them differently was not written by this workspace's trainer.
//! * **It drops the text.** Keeping a megabyte of model text alive for the life of a loaded model,
//!   so that a question can be re-answered from bytes already parsed, is the cost this avoids.
//!
//! ⚠ The consequence, stated because it is a real narrowing: a text the WALKER accepts but whose
//! split arrays cannot be paired — `split_feature=` with no `split_gain=`, the shape a hand-written
//! minimal test model has — loads here as an error though `vike_ml::parse_model_text` would return
//! a model. A caller that genuinely wants the walker alone still calls that function directly; this
//! type is the surface that promises both answers.
//!
//! # The width checks, and why they are not paranoia
//!
//! A misaligned importance table attributes the model's behaviour to the wrong named feature, which
//! is worse than no table at all — `crates/vike-ml/src/importance.rs` refuses inside one text for
//! exactly that reason. Two more checks are only available from HERE, where both halves are in
//! hand: the walker's declared width against the importance parser's (they read the same
//! `max_feature_idx=` key but disagree on which line wins when a text carries two), and the model's
//! own `feature_names` count against that width. A model that names NO column is accepted — every
//! `FeatureWeight::name` is then `None` — because `crates/vike-ml/src/train.rs`'s `write_csv`
//! writes a headerless CSV, so LightGBM auto-generates `Column_0 Column_1 …` and index→name mapping
//! belongs to whoever packed the matrix. A model that names SOME of them is refused: that is the
//! shifted-table shape.

use std::path::Path;

use vike_ml::importance::FitImportance;
use vike_ml::{GbdtModel, MlError, ProbaModel, importance_from_model_text, parse_model_text};

/// Everything loading or scoring a saved model can refuse to do — a VALUE in every case.
///
/// A half-written export, a truncated copy and a file that was never a model are all ordinary
/// events in a directory a person put files in, so none of them is a panic. Each variant names what
/// was seen; `Display` renders the sentence a Studio panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    /// The file the caller named could not be read. `path` is repeated so the message names it.
    Read { path: String, what: String },
    /// The text is not a model `vike_ml`'s walker can evaluate — malformed, truncated, an
    /// unsupported version or an unsupported objective. The inner [`MlError`] says which.
    Text(MlError),
    /// The text walks, but its per-tree `split_feature=` / `split_gain=` arrays cannot be paired
    /// into an importance table. The message names the offending tree.
    Importance(String),
    /// The two halves of ONE text declare different feature widths — reachable only from a text
    /// carrying two `max_feature_idx=` lines, where the walker takes the first and the importance
    /// parser the last. Refused rather than reconciled: either width could be the honest one.
    Width { walker: usize, importance: usize },
    /// The model names SOME of its columns but not all of them, so an importance table zipped
    /// against those names would be shifted. Naming NONE of them is legal and is not this.
    FeatureNames { declared: usize, width: usize },
    /// A scoring call whose matrix or row does not fit the model. The inner [`MlError`] is
    /// `vike_ml`'s own shape refusal, which already names both widths.
    Rows(MlError),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, what } => write!(f, "model file {path}: {what}"),
            Self::Text(e) => write!(f, "not a usable model: {e}"),
            Self::Importance(msg) => write!(f, "model importance: {msg}"),
            Self::Width { walker, importance } => write!(
                f,
                "the model text declares its feature width twice and disagrees with itself — the \
                 walker read {walker} features, the importance parser {importance}. Refusing: an \
                 importance table indexed at one width and named at the other attributes the \
                 model's behaviour to the wrong feature"
            ),
            Self::FeatureNames { declared, width } => write!(
                f,
                "the model names {declared} columns but splits on {width} — refusing to zip an \
                 importance table against names that do not line up with it. A model that names \
                 NONE of its columns is accepted; this one names some"
            ),
            Self::Rows(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// One feature's row in the "which input mattered" report: where it sat in the packed matrix, what
/// the model calls it, and how much of the fit it accounts for.
///
/// `name` is `None` when the model names no column at all (see this module's doc) — never a
/// synthesized label, because a made-up name in a report about attribution is the one thing this
/// surface must not produce.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureWeight {
    /// Column position in the fitted matrix. The label column is not counted.
    pub index: usize,
    /// The model's own name for that column, or `None` when it names none.
    pub name: Option<String>,
    /// Total split gain this feature accumulated across every tree — `0.0` for one never split on.
    pub gain: f64,
    /// How many splits used it — `0` for one never split on.
    pub splits: u64,
}

/// A model somebody already trained, loaded from its TEXT: scores rows, and reports per-feature
/// importance.
///
/// `Send + Sync` (both halves are plain data), which is what lets a Studio run score a series from
/// the pool it already has rather than serially.
#[derive(Clone, Debug)]
pub struct ScoringModel {
    model: GbdtModel,
    importance: FitImportance,
}

impl ScoringModel {
    /// Parse a model out of its `save_model` TEXT — the walker and the importance table together.
    ///
    /// Every refusal in this module's doc happens here, once, so nothing downstream re-checks.
    pub fn from_text(text: &str) -> Result<Self, ModelError> {
        let model = parse_model_text(text).map_err(ModelError::Text)?;
        let importance = importance_from_model_text(text).map_err(ModelError::Importance)?;

        let width = model.n_features_expected();
        if importance.n_features != width {
            return Err(ModelError::Width { walker: width, importance: importance.n_features });
        }
        let declared = model.feature_names.len();
        if declared != 0 && declared != width {
            return Err(ModelError::FeatureNames { declared, width });
        }
        Ok(Self { model, importance })
    }

    /// [`ScoringModel::from_text`] over a file the CALLER named.
    ///
    /// No default location and no directory walk: a saved model is user content whose path the
    /// Studio already holds, and a library that guesses where its model lives is a library its
    /// caller cannot deploy (`crates/vike-ml/src/parse.rs`'s `load_model_file` makes the same
    /// point about the one file read in that crate).
    pub fn from_path(path: &Path) -> Result<Self, ModelError> {
        let text = std::fs::read_to_string(path).map_err(|e| ModelError::Read {
            path: path.display().to_string(),
            what: e.to_string(),
        })?;
        Self::from_text(&text)
    }

    /// How wide a feature row must be.
    pub fn n_features(&self) -> usize {
        self.model.n_features_expected()
    }

    /// The model's own column names, or an EMPTY slice when it names none. Never partially filled —
    /// [`ScoringModel::from_text`] refuses that shape.
    pub fn feature_names(&self) -> &[String] {
        &self.model.feature_names
    }

    /// The raw per-feature importance, indexed by column position.
    pub fn importance(&self) -> &FitImportance {
        &self.importance
    }

    /// The importance table as a UI reads it: every feature, most important first.
    ///
    /// ⚠ EVERY feature, including the ones no tree ever split on. "This input mattered not at all"
    /// is an answer, and it is the half of the report that tells a user what to stop computing;
    /// dropping the zero rows would leave a panel that silently omits its most actionable finding.
    ///
    /// The order is total and deterministic — descending `gain`, ties broken by ascending `index` —
    /// so a panel re-rendering the same model cannot reshuffle its own rows. Compared with
    /// `f64::total_cmp`, so a `NaN` gain out of a doctored text sorts to one end instead of
    /// corrupting the sort.
    pub fn importance_ranked(&self) -> Vec<FeatureWeight> {
        let named = !self.model.feature_names.is_empty();
        let mut rows: Vec<FeatureWeight> = (0..self.n_features())
            .map(|index| FeatureWeight {
                index,
                name: named.then(|| self.model.feature_names[index].clone()),
                gain: self.importance.gain[index],
                splits: self.importance.splits[index],
            })
            .collect();
        rows.sort_by(|a, b| b.gain.total_cmp(&a.gain).then(a.index.cmp(&b.index)));
        rows
    }

    /// Score ONE row, with the width check the trait method cannot make.
    ///
    /// A row narrower than the model is refused rather than scored: `vike_ml`'s walker substitutes
    /// `NaN` for a column the row does not carry (`crates/vike-ml/src/infer.rs`'s `feature_value`),
    /// which routes through the tree and returns a probability that looks like a probability. This
    /// is the checked door; the [`ProbaModel`] impl below is the unchecked one.
    ///
    /// A WIDER row is accepted and its extra trailing columns ignored — the same tolerance
    /// `crates/vike-ml/src/infer.rs`'s `rows` already grants a batch.
    pub fn score_row(&self, row: &[f64]) -> Result<f64, ModelError> {
        if row.len() < self.n_features() {
            return Err(ModelError::Rows(MlError::Shape(format!(
                "the row is {} wide; the model splits on feature {} so it needs {}",
                row.len(),
                self.model.max_feature_idx,
                self.n_features()
            ))));
        }
        Ok(self.model.predict_proba(row))
    }

    /// Score a ROW-MAJOR matrix — the probability series a Studio run turns a saved model into.
    ///
    /// `n_features` is the caller's own row stride, checked against the model's width once for the
    /// whole matrix; a length that is not a whole number of rows is refused too.
    pub fn score_rows(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, ModelError> {
        self.model.predict_proba_batch(flat_x, n_features).map_err(ModelError::Rows)
    }
}

/// The seam a Studio run scores through, so this type substitutes anywhere a `vike_ml` consumer
/// takes a model.
///
/// ⚠ UNCHECKED on purpose, because the trait's signature has nowhere to put a refusal: `&[f64]` in,
/// one `f64` out. A row narrower than the model yields a plausible WRONG probability in a release
/// build (loud in a debug one — `crates/vike-ml/src/infer.rs`'s `raw_score` carries a
/// `debug_assert!`). [`ScoringModel::score_row`] and [`ScoringModel::score_rows`] are the checked
/// entry points, and are what a Studio caller should reach for.
impl ProbaModel for ScoringModel {
    fn predict_proba(&self, row: &[f64]) -> f64 {
        self.model.predict_proba(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-tree binary model in the shape `crates/vike-ml/src/parse.rs`'s `parse_model_text`
    /// accepts, small enough that every routing decision below is arithmetic done by hand.
    ///
    /// Tree=0 splits feature 0 at 0.5 then feature 1 at 0.25; Tree=1 splits feature 0 at 0.75.
    /// Feature 2 is declared and NEVER split on, which is the zero row the report must still carry.
    fn two_tree_model() -> String {
        "tree\n\
         version=v4\n\
         num_class=1\n\
         num_tree_per_iteration=1\n\
         label_index=0\n\
         max_feature_idx=2\n\
         objective=binary sigmoid:1\n\
         feature_names=alpha beta gamma\n\
         tree_sizes=0 0\n\
         \n\
         Tree=0\n\
         num_leaves=3\n\
         num_cat=0\n\
         split_feature=0 1\n\
         split_gain=8 2\n\
         threshold=0.5 0.25\n\
         decision_type=2 2\n\
         left_child=1 -1\n\
         right_child=-3 -2\n\
         leaf_value=0.4 -0.6 0.2\n\
         is_linear=0\n\
         shrinkage=1\n\
         \n\
         Tree=1\n\
         num_leaves=2\n\
         num_cat=0\n\
         split_feature=0\n\
         split_gain=3\n\
         threshold=0.75\n\
         decision_type=2\n\
         left_child=-1\n\
         right_child=-2\n\
         leaf_value=0.05 -0.05\n\
         is_linear=0\n\
         shrinkage=0.1\n\
         \n\
         end of trees\n"
            .to_string()
    }

    /// `1 / (1 + exp(-raw))` for `sigmoid:1` — spelled as the arithmetic rather than as a decimal
    /// literal, so the assertion pins the transform and not a number somebody typed.
    fn sigmoid(raw: f64) -> f64 {
        1.0 / (1.0 + (-raw).exp())
    }

    #[test]
    fn a_model_text_loads_and_scores_rows_into_a_probability_series() {
        let m = ScoringModel::from_text(&two_tree_model()).unwrap();
        assert_eq!(m.n_features(), 3);
        // Row A: 0.0 <= 0.5 -> node 1; 0.0 <= 0.25 -> leaf 0 (0.4). Tree=1: 0.0 <= 0.75 -> 0.05.
        // Row B: 1.0 > 0.5 -> leaf 2 (0.2).            Tree=1: 1.0 > 0.75 -> -0.05.
        let series = m.score_rows(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 3).unwrap();
        assert_eq!(series.len(), 2);
        assert!((series[0] - sigmoid(0.45)).abs() < 1e-12, "{series:?}");
        assert!((series[1] - sigmoid(0.15)).abs() < 1e-12, "{series:?}");
        // ...and the single-row doors agree with the batch, through the trait and through the
        // checked call alike.
        assert_eq!(m.score_row(&[0.0, 0.0, 0.0]).unwrap().to_bits(), series[0].to_bits());
        assert_eq!(ProbaModel::predict_proba(&m, &[1.0, 0.0, 0.0]).to_bits(), series[1].to_bits());
    }

    #[test]
    fn the_importance_report_carries_every_feature_including_the_ones_no_tree_split_on() {
        let m = ScoringModel::from_text(&two_tree_model()).unwrap();
        let imp = m.importance();
        assert_eq!(imp.n_features, 3);
        // Feature 0: split in both trees (8 + 3). Feature 1: once (2). Feature 2: never.
        assert!((imp.gain[0] - 11.0).abs() < 1e-12);
        assert_eq!(imp.splits, vec![2, 1, 0]);

        let ranked = m.importance_ranked();
        assert_eq!(ranked.len(), 3, "the never-split feature must still have a row: {ranked:?}");
        assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(
            ranked.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            vec![Some("alpha".into()), Some("beta".into()), Some("gamma".into())]
        );
        assert_eq!(
            ranked[2],
            FeatureWeight { index: 2, name: Some("gamma".into()), gain: 0.0, splits: 0 }
        );
    }

    #[test]
    fn a_tie_in_gain_is_broken_by_column_index_so_a_panel_cannot_reshuffle_its_own_rows() {
        // Features 1 and 2 carry the same gain; feature 0 is never split on.
        let text = "tree\nversion=v4\nnum_class=1\nnum_tree_per_iteration=1\nlabel_index=0\n\
                    max_feature_idx=2\nobjective=binary sigmoid:1\n\
                    feature_names=alpha beta gamma\ntree_sizes=0\n\
                    Tree=0\nnum_leaves=3\nnum_cat=0\nsplit_feature=2 1\nsplit_gain=4 4\n\
                    threshold=0.5 0.5\ndecision_type=2 2\nleft_child=1 -1\nright_child=-3 -2\n\
                    leaf_value=0.1 0.2 0.3\nis_linear=0\nshrinkage=1\nend of trees\n";
        let ranked = ScoringModel::from_text(text).unwrap().importance_ranked();
        assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![1, 2, 0]);
        // ...and the order is the SAME every time it is asked, not merely once.
        let again = ScoringModel::from_text(text).unwrap().importance_ranked();
        assert_eq!(ranked, again);
    }

    #[test]
    fn a_truncated_model_text_comes_back_as_a_named_error_rather_than_a_panic() {
        // The half-written export: a byte-exact PREFIX of a real model, cut before its terminator.
        let full = two_tree_model();
        let cut = full.split("end of trees").next().unwrap().to_string();
        let e = ScoringModel::from_text(&cut).unwrap_err();
        assert!(matches!(e, ModelError::Text(MlError::Parse { .. })), "{e:?}");
        assert!(e.to_string().contains("TRUNCATED"), "{e}");
    }

    #[test]
    fn a_text_that_was_never_a_model_comes_back_as_a_named_error() {
        for junk in ["", "not a model at all", "{\"json\": true}"] {
            let e = ScoringModel::from_text(junk).unwrap_err();
            assert!(matches!(e, ModelError::Text(_)), "{junk:?} -> {e:?}");
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn a_model_file_that_cannot_be_read_names_the_path_it_tried() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-model.txt");
        let e = ScoringModel::from_path(&missing).unwrap_err();
        match &e {
            ModelError::Read { path, .. } => assert!(path.contains("no-such-model.txt"), "{path}"),
            other => panic!("expected a Read refusal, got {other:?}"),
        }
        // ...and the happy path through the same door, so the test above cannot pass vacuously.
        let good = dir.path().join("model.txt");
        std::fs::write(&good, two_tree_model()).unwrap();
        assert_eq!(ScoringModel::from_path(&good).unwrap().n_features(), 3);
    }

    #[test]
    fn a_row_narrower_than_the_model_is_refused_by_the_checked_entry_points() {
        let m = ScoringModel::from_text(&two_tree_model()).unwrap();
        let e = m.score_row(&[0.0, 0.0]).unwrap_err();
        assert!(e.to_string().contains('3'), "the refusal must name the width needed: {e}");
        assert!(m.score_rows(&[0.0, 0.0], 2).is_err());
        // A WIDER row is fine — the trailing columns are simply not split on.
        assert_eq!(
            m.score_row(&[0.0, 0.0, 0.0, 99.0]).unwrap().to_bits(),
            m.score_row(&[0.0, 0.0, 0.0]).unwrap().to_bits()
        );
    }

    #[test]
    fn a_matrix_that_is_not_a_whole_number_of_rows_is_refused() {
        let m = ScoringModel::from_text(&two_tree_model()).unwrap();
        let e = m.score_rows(&[0.0, 0.0, 0.0, 1.0], 3).unwrap_err();
        assert!(matches!(e, ModelError::Rows(MlError::Shape(_))), "{e:?}");
    }

    #[test]
    fn a_text_whose_two_halves_declare_different_widths_is_refused() {
        // The walker takes the FIRST `max_feature_idx=`, the importance parser the LAST. One text
        // carrying both is the only way they can disagree, and reconciling them would pick a width
        // at random.
        let text = two_tree_model()
            .replace("max_feature_idx=2\n", "max_feature_idx=2\nmax_feature_idx=4\n");
        let e = ScoringModel::from_text(&text).unwrap_err();
        assert_eq!(e, ModelError::Width { walker: 3, importance: 5 });
        assert!(e.to_string().contains("wrong feature"), "{e}");
    }

    #[test]
    fn a_model_that_names_some_of_its_columns_but_not_all_is_refused() {
        let text =
            two_tree_model().replace("feature_names=alpha beta gamma", "feature_names=alpha beta");
        let e = ScoringModel::from_text(&text).unwrap_err();
        assert_eq!(e, ModelError::FeatureNames { declared: 2, width: 3 });
    }

    #[test]
    fn a_model_that_names_no_column_at_all_still_loads_and_reports_unnamed_features() {
        let text = two_tree_model().replace("feature_names=alpha beta gamma\n", "");
        let m = ScoringModel::from_text(&text).unwrap();
        assert!(m.feature_names().is_empty());
        let ranked = m.importance_ranked();
        assert!(ranked.iter().all(|r| r.name.is_none()), "{ranked:?}");
        // The report is otherwise unchanged — the names were never what carried the attribution.
        assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(m.score_rows(&[0.0, 0.0, 0.0], 3).unwrap().len(), 1);
    }

    #[test]
    fn a_text_the_walker_accepts_but_whose_split_arrays_cannot_be_paired_is_refused_at_load() {
        // ⚠ The DECLARED narrowing, pinned so it is a decision on record rather than an accident:
        // `crates/vike-ml/src/parse.rs`'s `parse_tree` never reads `split_gain`, so this text walks
        // — and this surface still refuses it, because it promises both answers or neither.
        let text = "tree\nversion=v4\nnum_class=1\nnum_tree_per_iteration=1\nlabel_index=0\n\
                    max_feature_idx=1\nobjective=binary sigmoid:1\n\
                    Tree=0\nnum_leaves=2\nnum_cat=0\nsplit_feature=0\nthreshold=0.5\n\
                    decision_type=2\nleft_child=-1\nright_child=-2\nleaf_value=0.1 0.2\n\
                    is_linear=0\nshrinkage=1\nend of trees\n";
        assert!(vike_ml::parse_model_text(text).is_ok(), "the walker must still accept it");
        let e = ScoringModel::from_text(text).unwrap_err();
        assert!(matches!(e, ModelError::Importance(_)), "{e:?}");
        assert!(e.to_string().contains("Tree=0"), "the refusal must name the tree: {e}");
    }

    /// Compile-time, and the point is the same one `crates/vike-ml/src/learner.rs`'s
    /// `the_seam_carries_the_bounds_a_parallel_search_needs` makes: a caller scoring a series from
    /// a pool needs these bounds, and it lives in another crate that would fail with a `rayon`
    /// error instead of naming the cause.
    #[test]
    fn the_scoring_model_carries_the_bounds_a_parallel_scorer_needs() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<ScoringModel>();
        send_sync::<ModelError>();
    }
}
