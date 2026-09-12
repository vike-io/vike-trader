//! The backtest harness (`BacktestNode`-lite): a config-driven, end-to-end backtest runner.
//! Gated entirely behind the `hist-replay` feature — it is built on the `hist_replay` tick
//! loader (`HistStore -> Vec<Tick> -> run_ticks`) alongside the bar-seeding path already in
//! this crate.
//!
//! Task 1: [`profile`] — the `BacktestProfile` TOML config + its parse/validate. Task 2:
//! [`registry`] — the name -> `Box<dyn Strategy<SimBroker>>` lookup the profile's
//! `strategy.name` resolves through. Task 3: [`run`] — the `run_backtest` dispatcher (bar/tick
//! modes). Task 4: [`report`] — the `BacktestReport` metrics summary the `backtest` bin prints.
//!
//! The multi-run siblings all take the SAME `(&BacktestProfile, Arc<dyn HistStore>)` shape and
//! read their own profile section: [`sweep`] expands `[sweep]`, [`walkforward`] walks
//! `[walkforward]`, [`euler`]/[`tpe`] search the same grid under a budget. [`optimize`](mod@optimize) is the SEAM
//! those three searchers now share — one `Optimizer` trait over the part that differs (the loop) and
//! one `PointEvaluator` over the part that does not (evaluating a point, ranking it, and bounding
//! the concurrency) — and each of the three entry points above survives as an adapter over it.

pub mod euler;
// The FOURTH `Optimizer`, and the first written AGAINST the seam rather than read out of it. It
// deliberately joins no `pub use` block below: a method's knowledge stays at the method's module
// path (`genetic::GeneticSearch`), exactly as `sweep::GridSearch`, `euler::EulerSearch` and
// `tpe::TpeSearch` do — only the SEAM is vocabulary. ⚠ That includes `GeneticConfig`, which
// `crates/vike-backtest/src/backtest_cli.rs` therefore names by MODULE PATH while it names
// `TpeConfig` as a re-export one line apart; the asymmetry is this rule, not an oversight.
// ⚠ This line read "deliberately wired to no CLI dispatch yet" until that follow-up landed — it is
// `backtest --optimizer genetic --seed N` now, and `genetic`'s own module doc carries what the
// wiring cost and the one rule it had to widen.
pub mod genetic;
pub mod optimize;
pub mod profile;
pub mod registry;
pub mod report;
pub mod run;
pub mod sweep;
pub mod tpe;
pub mod walkforward;

pub use euler::{EulerBudget, run_sweep_euler, run_sweep_euler_exec};
// The optimizer seam's shared vocabulary — the types a caller needs to DISPATCH over a method
// (`Box<dyn Optimizer>`) and to build the evaluator it runs against. The three METHODS themselves
// stay at their own module paths (`sweep::GridSearch`, `euler::EulerSearch`, `tpe::TpeSearch`),
// because a method's knowledge belongs in the method's file — only the seam is vocabulary.
pub use optimize::{
    BarsEvaluator, Candidate, Evaluated, Optimized, Optimizer, PointEvaluator, SearchOutcome,
    StoreEvaluator, optimize, report_from_outcome, require_overridable_params,
};
pub use profile::{
    BacktestProfile, DataCfg, DataKind, EngineCfg, FeeCfg, ImpactCfg, ResolutionCfg, StrategyCfg,
    WalkforwardCfg,
};
pub use registry::{BuyHold, STRATEGIES, strategy_by_name};
pub use report::BacktestReport;
pub use run::{FundingJoinStats, run_backtest, window_join_funding};
pub use sweep::{
    RankBy, RankMetric, SWEEP_SEQUENTIAL_ENV, SWEEP_THREADS_ENV, SweepExec, SweepPoint,
    SweepReport, SweepRow, cmp_scores_desc, expand_sweep, map_bounded, run_sweep, run_sweep_exec,
    run_sweep_with, run_sweep_with_exec, sweep_threads,
};
pub use tpe::{ParamDomain, TpeConfig, TpeOptimizer, TpeSpace, run_tpe};
pub use walkforward::run_walkforward;

use std::fmt;

/// Crate-wide harness error. Reused by the registry/dispatcher/bin tasks that build on
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
    /// A downstream data-store failure (reserved for later tasks: bar/tick loading).
    Data(String),
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
