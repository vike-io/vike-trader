//! The one implementation of [`vike_user_research::StudySim`] — an event-driven backtest, handed
//! to a study that asks for one.
//!
//! # Why the implementation is HERE and the trait is not
//!
//! `vike-user-research` hosts user studies and sits at layer 25. ⚠ This doc said 35 and named
//! `vike-backtest` at 50 as "the simulator" until 2026-09-27
//! (`docs/decisions/0087-the-simulator-leaves-the-harness.md`), when the event-driven engine left
//! `vike-backtest` for its own crate, `vike-simulator` — at layer 30 then, 25 since 2026-09-28 —
//! and `vike-backtest` became the offline HARNESS around it, at layer 30 now. Neither number
//! changes the refusal: naming either one from `vike-user-research` (25) is illegal by layer twice
//! over (`vike-simulator` is an equal-rank edge, `vike-backtest` an upward one), and wrong by kind
//! regardless of the number — a vectorised signal Sharpe and an event-driven one must not silently
//! become one number. So the host declares the seam in types it
//! owns (`StudySim`, `SimOutcome`) and names no simulator, while THIS crate — which links
//! `vike-backtest` to serve every run — supplies the only implementation.
//!
//! ⚠ **It was `vike-studio-core`'s until 2026-11-10**, when that crate stopped linking the harness so
//! the desktop could link it without an engine
//! (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of that
//! day). The study runner (`vike_studio_core::run_study_plan`) no longer hands this seam to every
//! study unconditionally: the HOST puts it on `vike_studio_core::StudyRunPlan::sim`, and the two
//! hosts that can run a backtest — this daemon's `RunStudy` (`crate::studio_run::run_study_local`)
//! and `crate::study_cli` — hand [`HarnessSim`]. The Studio's Research pane hands none, so a study
//! that asks for a backtest there answers `StudyError::NoSim`, and runs on the server instead.
//!
//! That keeps every layer where it is. Nothing moved to make this possible; the arrangement is
//! the same one the learner seam already uses, and the same one
//! `crates/vike/src/main.rs` uses to mount this crate's run table into the simulator's compute
//! server: the composition root names both halves so neither half has to name the other.
//!
//! ⚠ **What a study gets is a RESULT, not the simulator.** [`SimOutcome`](vike_user_research::SimOutcome) carries three numbers.
//! A study can aggregate them; it cannot reach the engine, the profile type, the fill model or
//! the store constructor. That narrowness is the answer to the by-kind objection rather than a
//! convenience — a wider seam would re-open exactly what the host refused.
//!
//! ⚠ **No DataFusion arrives with this.** `vike-backtest` is taken at DEFAULT features (⚠ was
//! `features = ["hist-replay"]` until the 2026-09-27 feature collapse made that the default), the
//! TRAIT-ONLY half, and the store is the caller's `Arc<dyn HistoryStore>`: this crate's normal tree
//! holds no store (`ci.yml`'s `datahub-all` guard (K)), and that is checked rather than assumed.

use std::sync::{Arc, OnceLock};

use vike_backtest::harness::{self, KeptRows, ProfileBacktest};
use vike_data::{HistoryStore, HistoryStoreError};
use vike_user_research::{SimOutcome, StudySim};

/// Runs one backtest through `vike_backtest::harness`.
///
/// The store and the profile both arrive per call, so a `rayon` fan-out inside a study can clone the
/// `Arc` freely. A host builds ONE per study (`crate::studio_run::run_study_local`, `crate::study_cli`):
/// its one piece of state is the datahub's kept instrument rows, asked by the study's first sim and read by
/// every other, so a study of a thousand sims asks the datahub once.
#[derive(Default)]
pub struct HarnessSim {
    /// The store's answer to `vike_data::HistoryStore::kept_instruments`, asked once.
    kept: OnceLock<Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>>,
}

impl StudySim for HarnessSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistoryStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        // The host cannot name `ProfileBacktest`, so the deserialization is ours. `deny_unknown_fields`
        // is on that type, which is what makes a typo in a study's profile an error here rather
        // than a silently ignored key — the failure mode the whole settings design is built to
        // refuse.
        let mut profile: ProfileBacktest = profile
            .clone()
            .try_into()
            .map_err(|e| format!("profile does not describe a backtest: {e}"))?;
        let answer = self.kept.get_or_init(|| store.kept_instruments());
        let series: Vec<(String, String)> =
            profile.data.resolved_series().into_iter().map(|s| (s.venue, s.symbol)).collect();
        profile.kept = KeptRows::answered(answer.as_ref().map(Option::as_deref), &series);

        let result = harness::run_backtest(&profile, store).map_err(|e| e.to_string())?;

        // `from_result` is what turns a run into the reported numbers, and it is the same call
        // the single-run path makes — so a study's numbers and a Studio run's numbers come from
        // one place rather than from two that could drift.
        let report = vike_analytics::report::BacktestReport::from_result(
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
