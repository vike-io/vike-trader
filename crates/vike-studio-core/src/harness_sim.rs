//! The one implementation of [`vike_user_research::StudySim`] — an event-driven backtest, handed
//! to a study that asks for one.
//!
//! # Why the implementation is HERE and the trait is not
//!
//! `vike-user-research` hosts user studies and sits at layer 35. The simulator is
//! `vike-backtest` at 50, and that crate's own doc refuses the edge twice over: illegal by layer,
//! and wrong by kind — a vectorised signal Sharpe and an event-driven one must not silently
//! become one number. So the host declares the seam in types it owns
//! (`StudySim`, `SimOutcome`) and names no simulator, while THIS crate — which already links
//! `vike-backtest` for the Studio's own runs — supplies the only implementation.
//!
//! That keeps every layer where it is. Nothing moved to make this possible; the arrangement is
//! the same one the learner seam already uses, and the same one
//! `crates/vike/src/main.rs` uses to mount this crate's run table into the simulator's compute
//! server: the composition root names both halves so neither half has to name the other.
//!
//! ⚠ **What a study gets is a RESULT, not the simulator.** [`SimOutcome`] carries three numbers.
//! A study can aggregate them; it cannot reach the engine, the profile type, the fill model or
//! the store constructor. That narrowness is the answer to the by-kind objection rather than a
//! convenience — a wider seam would re-open exactly what the host refused.
//!
//! ⚠ **No DataFusion arrives with this.** `vike-backtest` is taken at `hist-replay`, the
//! TRAIT-ONLY half, and the store is the caller's `Arc<dyn HistStore>`. The
//! `studio-standalone` lane's structural check — that this crate's default normal-dep tree
//! carries no `datafusion` crate — is unaffected, and that is checked rather than assumed.

use std::sync::Arc;

use vike_backtest::harness::{self, BacktestProfile};
use vike_data::HistStore;
use vike_user_research::{SimOutcome, StudySim};

/// Runs one backtest through `vike_backtest::harness`.
///
/// Stateless: the store and the profile both arrive per call, so one instance serves every study
/// in a process and a `rayon` fan-out inside a study can clone the `Arc` freely.
pub struct HarnessSim;

impl StudySim for HarnessSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        // The host cannot name `BacktestProfile`, so the deserialization is ours. `deny_unknown_fields`
        // is on that type, which is what makes a typo in a study's profile an error here rather
        // than a silently ignored key — the failure mode the whole settings design is built to
        // refuse.
        let profile: BacktestProfile = profile
            .clone()
            .try_into()
            .map_err(|e| format!("profile does not describe a backtest: {e}"))?;

        let result = harness::run_backtest(&profile, store).map_err(|e| e.to_string())?;

        // `from_result` is what turns a run into the reported numbers, and it is the same call
        // the single-run path makes — so a study's numbers and a Studio run's numbers come from
        // one place rather than from two that could drift.
        let report = harness::BacktestReport::from_result(
            Some("study".to_string()),
            &result,
            harness::report::periods_per_year(&profile),
        );

        Ok(SimOutcome {
            total_return: report.total_return,
            n_trades: report.n_trades as u64,
            win_rate: report.win_rate,
        })
    }
}
