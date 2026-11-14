//! The backtest harness (`BacktestNode`-lite): a config-driven, end-to-end backtest runner.
//! A DEFAULT build of this crate compiles it unconditionally (the 2026-09-27 feature collapse
//! retired the `hist-replay` feature that used to gate it) — it is built on the `tick_replay` tick
//! loader (`HistoryStore -> Vec<Tick> -> run_ticks`) alongside the bar-seeding path already in
//! this crate.
//!
//! A single run is four modules: [`profile`] — the `ProfileBacktest` TOML config + its
//! parse/validate, the strategy FILE it names included; [`strategy_builder`] — `ProfileStrategy::build`,
//! the one door a run builds its strategy through;
//! [`run`] — the `run_backtest` dispatcher (bar/tick modes);
//! [`report`] — the profile-coupled half of the `BacktestReport` summary the `backtesthub` bin prints
//! (the struct itself is `vike_analytics::report::BacktestReport`).
//!
//! The multi-run siblings all take the SAME `(&ProfileBacktest, Arc<dyn HistoryStore>)` shape and
//! read their own profile section: [`paramscan`] expands `[paramscan]`,
//! [`crate::walkforward::runner`] walks `[walkforward]`, [`euler`]/[`tpe`] search the same grid
//! under a budget. [`optimize`](mod@optimize) is the SEAM those three searchers now share — one
//! `Search` trait over the part that differs (the loop) and one `SearchEvaluator` over the part
//! that does not (evaluating a point, ranking it, and bounding the concurrency) — and each of the
//! three entry points above survives as an adapter over it. [`genetic`] is a fourth `Search`,
//! written against that seam. Around them: [`crate::search::select`] decides WHICH search a run
//! performs, [`trials`] is the trial ledger's writer and `--resume`'s warm cache, and
//! [`crate::walkforward::windows`] resolves every `[walkforward]` window form into one list of
//! splits. The searchers are modules of `crate::search` and the walk-forward runner of
//! `crate::walkforward`; the `pub use` lines below keep their vocabulary at `harness::`.

// The kept instrument rows a run reads its per-symbol facts from: named `harness::KeptRows` (below), its one path.
mod kept_rows;
pub mod optimize;
pub mod paramscan;
pub mod profile;
pub mod report;
pub mod run;
pub mod strategy_builder;
// The strategy file's text as read: named `harness::StrategySource` (below), its one path.
mod strategy_source;

pub use crate::search::euler::{
    SearchEulerBudget, run_paramscan_euler, run_paramscan_euler_execution,
};
// The optimizer seam's shared vocabulary — the types a caller needs to DISPATCH over a method
// (`Box<dyn Search>`) and to build the evaluator it runs against. The three METHODS themselves
// stay at their own module paths (`paramscan::SearchGrid`, `euler::SearchEuler`, `tpe::SearchTpe`),
// because a method's knowledge belongs in the method's file — only the seam is vocabulary.
pub use kept_rows::KeptRows;
pub use optimize::{
    Search, SearchCandidate, SearchEvaluation, SearchEvaluator, SearchEvaluatorBars,
    SearchEvaluatorStore, SearchOutcome, SearchReport, optimize, report_from_outcome,
};
pub use profile::{
    ProfileBacktest, ProfileData, ProfileEngine, ProfileEngineFee, ProfileEngineResolution,
    ProfileStrategy, ProfileWalkforward, WalkforwardWindowForm,
};
pub use run::{
    FundingJoinStats, refuse_foreign_quoted_picks, refuse_foreign_quoted_symbols,
    refuse_inverse_contracts, run_backtest, window_join_funding,
};
pub use strategy_source::StrategySource;
// The SELECTOR: which method, which knob belongs to it, which ranking, which evaluator. Vocabulary
// rather than a method's knowledge, so it re-exports like the seam above it — and unlike the three
// METHODS, which stay at their own module paths.
pub use crate::search::select::{RankChoice, SearchMethod, SearchSelection};
pub use paramscan::{
    PARAMSCAN_SEQUENTIAL_ENV, ParamscanExecution, ParamscanPoint, ParamscanReport, ParamscanRow,
    RankBy, RankMetric, expand_paramscan, install_paramscan_threads, map_bounded,
    paramscan_threads, run_paramscan, run_paramscan_execution, run_paramscan_with,
    run_paramscan_with_execution,
};
// ⚠ ONLY `run_tpe` is re-exported now. `ParamDomain`, `TpeConfig`, `TpeOptimizer` and
// `TpeSpace` stood here too until the ask/tell core went down to `vike_ml::search::tpe`; re-exporting
// them from their new home would be a second name for one symbol, which this workspace
// refuses on a MOVE (root CLAUDE.md). Callers name `vike_ml::search::tpe::` directly - this crate
// already declares that dependency, so nothing is unreachable from here.
pub use crate::search::tpe::run_tpe;
// The ledger writer and `--resume`'s warm cache. Re-exported because `backtest_cli` constructs one
// beside the evaluator it wraps, and the two should read as one vocabulary at the call site.
pub use crate::search::trials::{TrialRecorder, TrialRecorderTally, TrialWarm, warm_from};
// The walk-forward runner lives in the `crate::walkforward` family; `run_walkforward` stays
// vocabulary.
pub use crate::walkforward::runner::run_walkforward;

#[cfg(doc)]
use crate::search::{euler, genetic, tpe, trials};
use std::fmt;

/// Crate-wide harness error. Reused by the dispatcher/bin tasks that build on
/// [`profile`] — kept here rather than in `profile.rs` so later modules can depend on it
/// without depending on the profile parser specifically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    /// Filesystem I/O failure (e.g. reading a profile file).
    Io(String),
    /// TOML parse/deserialize failure, or an unparsable timestamp.
    Parse(String),
    /// A parsed profile failed a semantic validation rule.
    Validation(String),
    /// A downstream data-store failure: loading the bar/tick slice, or a slice the store holds that
    /// cannot be run as asked (empty, misaligned, short of coverage).
    Data(String),
}

impl HarnessError {
    /// The refusal SENTENCE alone, without the [`fmt::Display`] prefix.
    ///
    /// The `Display` rendering is right for one error coming back from a door
    /// (`harness validation error: engine.cash must be > 0, got 0`) and wrong for a LIST, where
    /// the prefix repeats on every row while the row already carries its own severity. That list
    /// is `profile::ProfileBacktest::validate_all`, which builds each `vike_model::Diagnostic`
    /// from this.
    ///
    /// ⚠ **The arms are spelled `Self::` deliberately, and that is not a style preference.**
    /// `crates/vike-backtest/src/profile_surface.rs` reads this file as TEXT and treats every
    /// `HarnessError::Validation(` occurrence as a refusal SITE; a site whose argument is an
    /// identifier rather than a message counts as an INDIRECT one, and its
    /// `INDIRECT_REFUSAL_SITES` declares how many each module has and fails both ways on a
    /// mismatch. Respelling this match would therefore add a phantom refusal to the published
    /// profile surface and redden that gate — for a method that raises nothing at all.
    pub fn message(&self) -> &str {
        match self {
            Self::Io(e) | Self::Parse(e) | Self::Validation(e) | Self::Data(e) => e,
        }
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HarnessError::Io(e) => write!(f, "harness io error: {e}"),
            HarnessError::Parse(e) => write!(f, "harness parse error: {e}"),
            HarnessError::Validation(e) => write!(f, "harness validation error: {e}"),
            HarnessError::Data(e) => write!(f, "harness data error: {e}"),
        }
    }
}

impl std::error::Error for HarnessError {}
