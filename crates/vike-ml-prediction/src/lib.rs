//! vike-ml-prediction — the PREDICTION half of the workspace's model crates: a trained model, read
//! and answered, and nothing else.
//!
//! # Why prediction and training are two crates
//!
//! `vike-ml-training` DRIVES the upstream LightGBM CLI binary as a child process and writes its
//! data, config and model files. This crate parses LightGBM's `save_model` text output (a
//! documented format the C++ predictor itself treats as canonical) and walks the trees in PURE
//! RUST: no process spawn, no file write, no native dependency and no `vike-*` dependency but the
//! formula floor `vike-math` (the one logistic, logit and softmax). That is
//! what a live binary — and a strategy FILE, built against the strategy template — may link to
//! predict from a model that was already trained. Training happens once, on a build box, with the
//! real binary; prediction happens everywhere, with nothing behind it.
//!
//! **That split is only trustworthy if the two agree**, so it is gated rather than asserted:
//! `crates/vike-ml-training/tests/train_infer_equality.rs` runs LightGBM's OWN `task=predict` over
//! a trained model and compares it to this crate's walker, row by row, on the same model file.
//! `tests/real_model_golden.rs` then freezes one such model and its outputs into the repository,
//! so the comparison is re-run on every CI run, on a machine with no LightGBM binary at all.
//!
//! # What is here
//!
//! [`model`] is the parsed text shape and what reads it ([`model::parse`], [`model::infer`],
//! [`model::explain`] — exact TreeSHAP, so a study or a strategy can say WHY a row scored what it
//! did — [`model::importance`], [`model::categories`]); [`predictor`] is the prediction trait a model
//! answers through; [`pins`] is the model-text format pin; [`error`] is the one error type both
//! model crates return.
//!
//! Beside LightGBM's trees, the model kinds `vike-ml-training` fits ITSELF, each with ONE versioned
//! text format whose writer and reader sit together in its module: [`linear`] (ordinary least
//! squares, Ridge, LASSO, Elastic Net, logistic regression), [`gaussian_process`] (mean and
//! variance), [`ensemble`] (a bagged mean, or a stacked meta model), [`conformal`] (split
//! conformal intervals and prediction sets) and [`calibration`] (Platt and isotonic calibration of
//! a forecast probability, alone or stacked over a model). [`text_format`] is their one envelope, and
//! [`any_model::AnyModel::from_text`] reads any model text, LightGBM's included — what a strategy
//! file handed a run's model text calls.
//!
//! # Why this crate is general-purpose
//!
//! Nothing in its API names a strategy, a venue or a study. The crate names NO `vike-*` crate but
//! `vike-utility`, which `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
//! `VIKE_FREE_CRATES` and `FLOOR_EDGES` hold.

pub mod any_model;
pub mod calibration;
pub mod conformal;
pub mod ensemble;
pub mod error;
pub mod gaussian_process;
pub mod linear;
pub mod model;
pub mod pins;
pub mod predictor;
pub mod text_format;

pub use error::MlError;
pub use model::categories::CategoryMap;
pub use model::importance::{FitImportance, importance_from_model_text};
pub use model::parse::{load_model_file, parse_model_text};
pub use model::{GbdtModel, Objective, Tree};
pub use pins::MODEL_VERSION;
pub use predictor::{PredictionKind, Predictor};
