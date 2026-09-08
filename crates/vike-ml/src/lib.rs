//! vike-ml — the workspace's model crate: gradient-boosted trees, trained once and predicted
//! everywhere.
//!
//! # Why training and inference are split
//!
//! [`train`] and [`cli`] drive the UPSTREAM LightGBM CLI BINARY as a child process: they render
//! LightGBM's own config text, write a data file, spawn the binary and read `output_model` back.
//! That binary is built once, from a pinned upstream tag, on the box that trains. Inference, by
//! contrast, is compiled into every binary that runs a strategy — including a live daemon on a box
//! that signs real orders. Shipping a C++ library there, to predict from a model that was already
//! trained, is a cost with no benefit.
//!
//! So [`infer`] is PURE RUST: it parses LightGBM's `save_model` text output (a documented format
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
//! It is the workspace's first ML surface, and it is built as the shared home from the start
//! rather than as one strategy's appendage — the precedent `vike-fills` and `vike-analytics`
//! already set. Nothing in its API names a strategy, a venue or a study: [`params::GbdtParams`]
//! carries LightGBM's own parameter names, and [`train::TrainData`] is a row-major `&[f64]` with a
//! label slice.
//!
//! # The learner seam
//!
//! [`learner`] is the crate's only pair of traits: fit a [`train::TrainData`] at a
//! [`search::GridPoint`], read one probability per row. It arrived from the study that first
//! needed a model, where it had been written as that study's own view of a learner — correct
//! then, because this crate's API was still moving, and wrong once it stopped, since a general
//! abstraction is only useful where everything that fits a model can reach it. What it buys is
//! SUBSTITUTION: code that fits models can be tested against a double on a machine with no
//! LightGBM binary, which is every Windows box and every CI runner. [`test_support`] is that
//! double, behind the crate's one cargo feature — the seam was worth nothing while nothing
//! substitutable existed, and for a while nothing did.
//!
//! # The fit cache, and the two dependencies it cost
//!
//! [`fit_cache`] is the cross-run cache over that seam: a validation score already computed for
//! these exact training bytes, this point, this seed and this validation half is not refitted. It
//! arrived from the same study the seam did, for the same reason — a cache over a learner is
//! library work, and rewriting it is what a SECOND consumer of the seam would otherwise have to
//! do.
//!
//! ⚠ **It is what made this crate own a hash**, which it had deliberately refused to. The
//! dependency list was `vike-model` and nothing else; it is now `vike-model` plus `sha2` and
//! `hex`, both already linked into every binary in this workspace by `vike-bridge-core`'s signer.
//! What the crate still does NOT own is an objective: [`fit_cache::score_key`] takes the
//! objective's identity as a digest the caller computes, because the workspace's scorers live in
//! `vike-analytics`, which declares the same layer as this crate and is therefore unreachable
//! from it by construction.
//!
//! ⚠ One honest qualification to "nothing in its API names a study": [`fit_cache::KEY_SCHEMA`]'s
//! VALUE is the string `vike-research fit-cache v1`, and [`gbdt_learner`]'s fit-identity tag reads
//! `vike-research fit-identity v1`. Both are frozen cache-invalidation tokens
//! rather than namespaces — renaming either re-keys every entry in every existing on-disk cache —
//! and each constant's own doc carries the argument for leaving it spelled as it was minted. The
//! crate they name has since been deleted outright, which changes nothing: a domain-separation
//! token is allowed to name something that no longer exists.

pub mod categories;
pub mod cli;
pub mod error;
// The cross-run fit cache over the learner seam. NOT re-exported at the crate root: a consumer
// spells `vike_ml::fit_cache::FitCache`, which keeps `Transcript`/`DigestWriter` — general hashing
// tools that exist here only to serve a key — from reading as this crate's vocabulary, and keeps
// the module's own name on every call site that content-addresses a fit.
pub mod fit_cache;
// The one `Learner` in this workspace that drives a REAL trainer — everything else implementing
// the seam is a double. Promoted out of the research crate's own `studies/cohort/ml_adapter.rs`
// when that crate dissolved, on the two conditions the split design set (a caller-supplied base
// parameter bag, and a bin-cache capacity that is a constant rather than a `rayon` pool read), so
// the move added no dependency to this crate. NOT feature-gated: it spawns nothing until a caller
// hands it a verified binary path, so a default build compiles it and links no native code.
pub mod gbdt_learner;
pub mod grid;
pub mod importance;
pub mod infer;
pub mod learner;
pub mod metrics;
pub mod model;
pub mod params;
pub mod parse;
pub mod pins;
pub mod search;
// The shared learner double. Behind `test-support` so a shipped build compiles NONE of it, and
// `any(test, …)` so this crate's own unit tests reach it without the self-referential dev-dep —
// the `vike-data`/`vike-model` shape. It is NOT re-exported at the crate root: a consumer spells
// `vike_ml::test_support::ScriptedLearner`, so a double can never be mistaken for the real thing
// in a call site that ships.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod train;

pub use categories::CategoryMap;
pub use cli::LightGbmCli;
pub use error::MlError;
pub use gbdt_learner::{DEFAULT_BIN_CAPACITY, GbdtLearner};
pub use importance::{FitImportance, importance_from_model_text};
pub use learner::{Capture, CapturedFit, Learner, ProbaModel};
pub use model::{GbdtModel, Objective, Tree};
pub use params::GbdtParams;
pub use parse::{load_model_file, parse_model_text};
pub use pins::{MODEL_VERSION, PINNED_LIGHTGBM_TAG};
pub use search::{DEFAULT_POINT, GridPoint, ParamPoint, ParamValue, best_index};
pub use train::{TrainConfig, TrainData};
