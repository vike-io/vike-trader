//! The persistent trial ledger + learnings store behind the `develop_strategy_with_ledger` authoring
//! loop — the memory that loop never had. Every trial it runs (its code, its OOS Sharpe, its equity
//! curve, why it was rejected) used to be discarded on return, so the loop restarted cold every
//! session and re-derived the same duds, and — the statistically real cost — deflation, the single
//! most important overfitting control, could only see the candidates of ONE call and therefore
//! reported `0.0` on the path production actually uses (a single-candidate run).
//!
//! Two best-effort JSON files, conventionally colocated with the store root (see [`LedgerPaths`]):
//!
//! ```text
//! <store root>/ai_trials.json      // the trial ledger (bounded, append-ordered)
//! <store root>/ai_learnings.json   // curated durable notes the model chose to record
//! ```
//!
//! **Env boundary (settings STEP 2).** This module reads NO environment variable — every entry
//! point takes a `&Path`, exactly the `vike_alerting::persist` idiom: the BINARY (or, here, the
//! Studio shell that already owns a `DataFusionHist`) resolves the location and passes it in. That
//! keeps the settings-registry `Library` count from growing and keeps the ledger testable against
//! a temp dir with no ambient state.
//!
//! **Best-effort throughout, the studio idiom.** A missing file is `Default`, a corrupt file is
//! `Default` plus one `tracing::warn!`, a failed write is one `tracing::warn!` and nothing else.
//! The authoring loop must never fail because the ledger is unavailable — a lost note is a lost
//! note; a bricked loop is a bricked feature.
//!
//! **Forward-compat** lives in the serde model: every field on every struct here (and on
//! [`crate::AgentResult`], which gained `Serialize`/`Deserialize` for this module) is
//! `#[serde(default)]`, so a file written by an older build still loads — the
//! `studio_strategies.json` precedent.

use std::path::{Path, PathBuf};

#[cfg(doc)]
use store::prune;
#[cfg(doc)]
use trial::Trial;

pub mod learning;
pub mod store;
pub mod summary;
pub mod trial;

/// The trial ledger's filename under a store root.
pub const TRIALS_FILE: &str = "ai_trials.json";

/// The learnings store's filename under a store root.
pub const LEARNINGS_FILE: &str = "ai_learnings.json";

/// How many trials the ledger retains. Unbounded growth is a real failure mode here, not a
/// theoretical one: every accepted trial carries an `oos_equity_curve` `Vec<f64>` as long as the
/// held-out window. Oldest rows are dropped first (the ledger is append-ordered).
pub const MAX_TRIALS: usize = 500;

/// How many of the newest ACCEPTED trials keep their `oos_equity_curve`. Older rows keep every
/// scalar — including [`Trial::sr_per_obs`], which is exactly what the deflation trial set needs —
/// and have their curve emptied. See [`prune`].
pub const MAX_CURVE_ROWS: usize = 50;

/// Hard cap on one learning's text, in BYTES (truncated at a char boundary — never mid-sequence).
pub const MAX_LEARNING_TEXT: usize = 1024;

/// How many learnings the store retains; oldest dropped first.
pub const MAX_LEARNINGS: usize = 200;

/// Where the two ledger files live. Resolved by the CALLER (see the module doc's env boundary):
/// the library never derives this from the environment, only from a path it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPaths {
    pub trials: PathBuf,
    pub learnings: PathBuf,
}

impl LedgerPaths {
    /// The CONVENTIONAL pair under a store root — `<root>/ai_trials.json` +
    /// `<root>/ai_learnings.json`, colocated with `studio_strategies.json` so the authoring memory
    /// sits next to the data it was earned on.
    pub fn under(store_root: &Path) -> Self {
        Self { trials: store_root.join(TRIALS_FILE), learnings: store_root.join(LEARNINGS_FILE) }
    }
}

#[cfg(test)]
mod tests;
