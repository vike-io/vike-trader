//! The two pins that decide whether a model file and this crate are talking about the same thing.
//!
//! They live together, in their own file, because the version refusal has to name BOTH and because
//! a bump has to change both in one place. `crates/vike-ml/CLAUDE.md` is the bump procedure.
//!
//! # What each one actually proves — and what neither proves
//!
//! [`MODEL_VERSION`] is the model TEXT FORMAT's own `version=` line. It is a coarse instrument:
//! ⚠ **`v4` does NOT change when fields are ADDED within v4.** This crate's parser tolerates
//! unknown `key=value` lines silently (it must — LightGBM already writes several this walker has
//! no use for), so a new per-node array carrying ROUTING meaning would be read as noise, dropped,
//! and every prediction would be quietly wrong while this check stayed green.
//!
//! [`PINNED_LIGHTGBM_TAG`] is the upstream release the binary is built from
//! (`just lightgbm-build`), asserted against the binary at startup by
//! [`crate::cli::LightGbmCli::new`]. It catches "somebody put a different LightGBM on the box",
//! which is a real and common failure — but it says nothing about whether the format changed.
//!
//! **So the real bump proof is neither of these: it is `tests/train_infer_equality.rs`**, which
//! runs LightGBM's own `task=predict` against this crate's walker over the same model file and
//! compares every row. A silently-added routing field shows up there as a wrong number, and
//! nowhere else. Treat these two constants as the cheap tripwire and that gate as the evidence.

/// The model-text format version this parser implements, asserted rather than sniffed.
pub const MODEL_VERSION: &str = "v4";

/// The upstream LightGBM release this crate is built and validated against.
///
/// Upstream moved from `microsoft/LightGBM` to `lightgbm-org/LightGBM` in 4.7.0 (old URLs
/// redirect); `v4.7.0` is tag sha `8f7036f0`, released 2026-07-18.
pub const PINNED_LIGHTGBM_TAG: &str = "v4.7.0";
