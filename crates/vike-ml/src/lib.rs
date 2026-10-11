//! vike-ml — the workspace's model crate: gradient-boosted trees, trained once and predicted
//! everywhere.
//!
//! # Why training and inference are split
//!
//! [`train`] and [`train::cli`] drive the UPSTREAM LightGBM CLI BINARY as a child process: they render
//! LightGBM's own config text, write a data file, spawn the binary and read `output_model` back.
//! That binary is built once, from a pinned upstream tag, on the box that trains. Inference, by
//! contrast, is compiled into every binary that runs a strategy — including a live daemon on a box
//! that signs real orders. Shipping a C++ library there, to predict from a model that was already
//! trained, is a cost with no benefit.
//!
//! So [`model::infer`] is PURE RUST: it parses LightGBM's `save_model` text output (a documented format
//! the C++ predictor itself treats as canonical) and walks the trees. Training happens once, on a
//! build box, with the real binary; inference happens everywhere, with nothing behind it.
//!
//! **That split is only trustworthy if the two agree**, so it is gated rather than asserted:
//! `tests/train_infer_equality.rs` runs LightGBM's OWN `task=predict` over a trained model and
//! compares it to this crate's walker, row by row, on the same model file. `tests/real_model_golden.rs`
//! then freezes one such model and its outputs into the repository, so the comparison is re-run on
//! every CI run, on a machine with no LightGBM binary at all.
//!
//! # Why this crate is general-purpose
//!
//! It is the shared home for ML, not one strategy's appendage — the precedent `vike-fills` and
//! `vike-analytics` set. Nothing in its API names a strategy, a venue or a study:
//! [`train::params::GbdtParams`] carries LightGBM's own parameter names, and [`train::TrainData`] is a
//! row-major `&[f64]` with a label slice.
//!
//! # The learner seam, its double, and the fit cache
//!
//! [`seam::learner`] is the crate's only pair of traits: fit a [`train::TrainData`] at a
//! [`search::GridPoint`], read one probability per row. What it buys is SUBSTITUTION: code that
//! fits models can be tested against a double on a machine with no LightGBM binary, which is every
//! Windows box and every CI runner. `test_support` is that double, behind the crate's one cargo
//! feature. [`train::gbdt_learner`] is the one impl that drives the real trainer.
//!
//! [`seam::fit_cache`] is the cross-run cache over that seam: a validation score already computed for
//! these exact training bytes, this point, this seed and this validation half is not refitted. Its
//! module doc says what it made this crate own (a hash) and not own (an objective). The crate
//! names NO `vike-*` crate, which `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
//! `VIKE_FREE_CRATES` holds.
//!
//! ⚠ One qualification to "nothing in its API names a study": [`seam::fit_cache::KEY_SCHEMA`]'s VALUE
//! is the string `vike-research fit-cache v1`, and [`train::gbdt_learner`]'s fit-identity tag reads
//! `vike-research fit-identity v1`. Both are FROZEN cache-invalidation tokens rather than
//! namespaces — `crates/vike-ml/CLAUDE.md`'s "The learner seam, the double and the fit cache".

pub mod error;
pub mod model;
pub mod pins;
pub mod seam;
pub mod search;
pub mod train;

pub use error::MlError;
pub use model::categories::CategoryMap;
pub use model::importance::{FitImportance, importance_from_model_text};
pub use model::parse::{load_model_file, parse_model_text};
pub use model::{GbdtModel, Objective, Tree};
pub use pins::{MODEL_VERSION, PINNED_LIGHTGBM_TAG};
pub use seam::learner::{Capture, CapturedFit, Learner, ProbaModel};
pub use search::{DEFAULT_POINT, GridPoint, ParamPoint, ParamValue, best_index};
pub use train::gbdt_learner::{DEFAULT_BIN_CAPACITY, GbdtLearner};
pub use train::params::GbdtParams;
pub use train::{TrainConfig, TrainData};
