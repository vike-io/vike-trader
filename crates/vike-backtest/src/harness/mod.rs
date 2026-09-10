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
//! `[walkforward]`, [`euler`]/[`tpe`] search the same grid under a budget.

pub mod euler;
pub mod profile;
pub mod registry;
pub mod report;
pub mod run;
pub mod sweep;
pub mod tpe;
pub mod walkforward;

pub use euler::{EulerBudget, run_sweep_euler, run_sweep_euler_exec};
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
