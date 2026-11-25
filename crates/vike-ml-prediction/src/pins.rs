//! The model-text FORMAT pin: whether a model file and this parser are talking about the same thing.
//!
//! Its twin, the upstream LightGBM release the trainer is pinned to, is `vike-ml-training`'s
//! `PINNED_LIGHTGBM_TAG` (`crates/vike-ml-training/src/pins.rs`); a LightGBM bump moves both, and
//! `crates/vike-ml-training/CLAUDE.md` is the bump procedure.
//!
//! ⚠ **Neither is the bump proof** (`crates/vike-ml-training/CLAUDE.md`'s "The two pins, and what
//! neither proves"). [`MODEL_VERSION`] does NOT change when fields are ADDED within `v4`, and the
//! parser ignores unknown `key=value` lines, so a new per-node ROUTING array would be dropped with
//! this check green. The evidence is `crates/vike-ml-training/tests/train_infer_equality.rs`:
//! LightGBM's own `task=predict` against this crate's walker, row by row, over the same model file.

/// The model-text format version this parser implements, asserted rather than sniffed.
pub const MODEL_VERSION: &str = "v4";
