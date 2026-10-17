//! The two pins that decide whether a model file and this crate are talking about the same thing.
//!
//! Together in one file because the version refusal names BOTH and a bump changes both;
//! `crates/vike-ml/CLAUDE.md` is the bump procedure.
//!
//! ⚠ **Neither is the bump proof** (`crates/vike-ml/CLAUDE.md`'s "The two pins, and what neither
//! proves"). [`MODEL_VERSION`] does NOT change when fields are ADDED within `v4`, and the parser
//! ignores unknown `key=value` lines, so a new per-node ROUTING array would be dropped with this
//! check green. [`PINNED_LIGHTGBM_TAG`], asserted against the binary by
//! [`crate::train::cli::LightGbmCli::new`], catches a different LightGBM on the box, not a format change.
//! The evidence is `tests/train_infer_equality.rs`: LightGBM's own `task=predict` against this
//! crate's walker, row by row, over the same model file.

/// The model-text format version this parser implements, asserted rather than sniffed.
pub const MODEL_VERSION: &str = "v4";

/// The upstream LightGBM release this crate is built and validated against.
///
/// Upstream moved from `microsoft/LightGBM` to `lightgbm-org/LightGBM` in 4.7.0 (old URLs
/// redirect); `v4.7.0` is tag sha `8f7036f0`, released 2026-07-18.
pub const PINNED_LIGHTGBM_TAG: &str = "v4.7.0";
