//! vike-ml-training — the TRAINING half of the workspace's model crates: gradient-boosted trees,
//! fitted by driving the upstream LightGBM CLI, searched over, cached and substituted in tests.
//!
//! # Why training and prediction are two crates
//!
//! [`train`] and [`train::cli`] drive the UPSTREAM LightGBM CLI BINARY as a child process: they render
//! LightGBM's own config text, write a data file, spawn the binary and read `output_model` back.
//! That binary is built once, from a pinned upstream tag, on the box that trains. Prediction, by
//! contrast, is compiled into every binary that runs a strategy — including a live daemon on a box
//! that signs real orders — and lives in `vike-ml-prediction`, which spawns nothing, writes nothing
//! and links no native library. This crate depends on it for the model it reads back
//! (`vike_ml_prediction::parse_model_text`), the prediction trait a fitted model answers through
//! (`vike_ml_prediction::Predictor`) and the one error type both return.
//!
//! **That split is only trustworthy if the two agree**, so it is gated rather than asserted:
//! `tests/train_infer_equality.rs` runs LightGBM's OWN `task=predict` over a trained model and
//! compares it to `vike-ml-prediction`'s walker, row by row, on the same model file, and its
//! `capture_the_golden_fixture` freezes one such model into `vike-ml-prediction`'s fixtures.
//!
//! # Why this crate is general-purpose
//!
//! It is the shared home for ML training, not one strategy's appendage — the precedent `vike-fills`
//! and `vike-analytics` set. Nothing in its API names a strategy, a venue or a study:
//! [`train::params::GbdtParams`] carries LightGBM's own parameter names, and [`train::TrainData`] is a
//! row-major `&[f64]` with a label slice.
//!
//! # The pure-Rust learners
//!
//! Beside the driven LightGBM, five modules fit IN PROCESS, spawning nothing and writing no file —
//! so a jailed study runs them too: [`linear`] (ordinary least squares, Ridge, LASSO, Elastic Net,
//! logistic regression), [`gaussian_process`] (the RBF Gaussian-process regressor and its log
//! marginal likelihood), [`ensemble`] (bagging and stacking over any learners), [`conformal`]
//! (split conformal calibration) and [`calibration`] (probability scoring — Brier, log loss, the
//! reliability diagram — and Platt and isotonic calibration). Each model is a
//! `vike-ml-prediction` type with ONE versioned text (`vike-ml <kind> v1`) that `vike_ml_prediction::any_model::AnyModel::from_text` reads back
//! bit for bit, so a strategy file runs it the way it runs a LightGBM model. The dense algebra is
//! `vike_math::linear_algebra`'s; none is written here. Each learner implements the seam below,
//! with a `fit_identity` (bagging's from its members'; stacking proves none), so the fit cache
//! serves them as it serves LightGBM.
//!
//! # Double machine learning
//!
//! [`double_ml`] estimates a treatment's EFFECT, its standard error and a confidence interval with
//! the confounders partialled out by any learners through the seam (the partially linear and the
//! interactive model, cross-fitted on the caller's folds). It fits through the seam and spawns
//! nothing of its own, so it runs in the jail whenever its learners do.
//!
//! # The learner seam, its double, and the fit cache
//!
//! [`seam::learner`] is the seam: fit a [`train::TrainData`] at a [`search::GridPoint`], read one
//! probability per row through the model it hands back. What it buys is SUBSTITUTION: code that
//! fits models can be tested against a double on a machine with no LightGBM binary, which is every
//! Windows box and every CI runner. `test_support` is that double, behind the crate's one cargo
//! feature. [`train::gbdt_learner`] is the one impl that drives the real trainer.
//!
//! [`seam::fit_cache`] is the cross-run cache over that seam: a validation score already computed for
//! these exact training bytes, this point, this seed and this validation half is not refitted. Its
//! module doc says what it made this crate own (a hash) and not own (an objective). The crate's two
//! `vike-*` normal dependencies are `vike-ml-prediction` and `vike-math`, neither of which names
//! a vike crate.
//!
//! # Out-of-sample feature importance
//!
//! [`feature_importance`] is AFML ch. 8's MDA and SFI over the same seam: fit per fold through a
//! caller's closure, score out of fold. The folds are a parameter (a purged splitter lives in
//! `vike-analytics`), and the in-sample MDI is the importance a capture fit already reports.
//!
//! ⚠ One qualification to "nothing in its API names a study": [`seam::fit_cache::KEY_SCHEMA`]'s VALUE
//! is the string `vike-research fit-cache v1`, and [`train::gbdt_learner`]'s fit-identity tag reads
//! `vike-research fit-identity v1`. Both are FROZEN cache-invalidation tokens rather than
//! namespaces — `crates/vike-ml-training/CLAUDE.md`'s "The learner seam, the double and the fit cache".

pub mod calibration;
pub mod conformal;
pub mod double_ml;
pub mod ensemble;
pub mod feature_importance;
pub mod gaussian_process;
pub mod linear;
pub mod pins;
pub mod seam;
pub mod search;
pub mod train;

pub use pins::PINNED_LIGHTGBM_TAG;
pub use seam::learner::{Capture, CapturedFit, Learner};
pub use search::{DEFAULT_POINT, GridPoint, ParamPoint, ParamValue, best_index};
pub use train::TrainData;
pub use train::gbdt_learner::{DEFAULT_BIN_CAPACITY, GbdtLearner};
pub use train::neural_learner::{NeuralLearner, NeuralModel};
pub use train::params::GbdtParams;
