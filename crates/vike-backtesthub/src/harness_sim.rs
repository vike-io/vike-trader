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
//! day). The study runner (`vike_study_runner::study_dispatch::run_study_plan`) no longer hands this seam to every
//! study unconditionally: the HOST puts it on `vike_study_runner::study_dispatch::StudyRunPlan::sim`, and the two
//! hosts that can run a backtest — this daemon's `RunStudy` (`crate::studio_run::run_study_local`)
//! and `crate::study_cli` — hand [`HarnessSim`]. The Studio's Research pane hands none, so a study
//! that asks for a backtest there answers `StudyError::NoSim`, and runs on the server instead.
//!
//! That keeps every layer where it is. Nothing moved to make this possible; the arrangement is
//! the same one the learner seam already uses, and the same one
//! `crates/vike-backend/src/main.rs` uses to mount this crate's run table into the simulator's compute
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

//! ⚠ **A backtest a study asks for is a backtest like any other** (2026-11-15, decision 0284): it is
//! the daemon's own `RunBacktest` request, answered by its own route
//! (`crate::backtesthub_server::dispatch::answer_unsaved`, `handle_request` less the saved run), so a
//! `.rs` strategy FILE in the profile is
//! read under the project's `strategies/` folder, built by the strategy builder and run in its
//! sandboxed child, and a `.rhai` one runs here, exactly as a `vike-cli backtest` of that profile does.
//! There is no second path. Until that day this ran the harness directly, and every profile naming a
//! strategy file (every profile since plan 2b) answered "no route to a sandboxed child".

use std::path::PathBuf;
use std::sync::Arc;

use vike_backtesthub_client::proto::{Request, Response};
use vike_data::HistoryStore;
use vike_user_research::{SimOutcome, StudySim};

use crate::backtesthub_server::StudyDirs;
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFiles;

/// Runs one backtest the way the daemon runs a `RunBacktest` request: the profile's strategy FILE read
/// under [`HarnessSim::new`]'s folder, a `.rs` file built and run in its sandboxed child.
///
/// The store and the profile both arrive per call, so a `rayon` fan-out inside a study can clone the
/// `Arc` freely. A host builds ONE per study (`crate::studio_run::run_study_local`, `crate::study_cli`).
pub struct HarnessSim {
    /// Where a profile's strategy file is read, and how a `.rs` one is built: the hub's own.
    study: StudyDirs,
}

impl HarnessSim {
    /// A backtest whose profile's strategy files are read under `strategies` (the project's
    /// `strategies/` folder), a `.rs` file run in its sandboxed child (`StrategyFiles::hub`).
    pub fn new(strategies: PathBuf) -> Self {
        HarnessSim {
            study: StudyDirs {
                strategies: StrategyFiles::hub(strategies),
                ..StudyDirs::working_directory()
            },
        }
    }
}

impl StudySim for HarnessSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistoryStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        let profile_toml = toml::to_string(profile)
            .map_err(|e| format!("profile does not describe a backtest: {e}"))?;
        // Unsaved: this backtest is a step of the study's run, which the study runner saves whole.
        let response = crate::backtesthub_server::dispatch::answer_unsaved(
            Request::RunBacktest(profile_toml),
            &store,
            &self.study,
            NamedRunLane::DISARMED,
            None,
        );
        match response {
            // `TradingResults::from_record`'s three numbers, the ones a single run reports.
            Response::Report(json) => outcome_of(&json),
            Response::Error(why) => Err(why),
            other => Err(format!("the backtest answered {other:?}, not a report")),
        }
    }
}

/// A report's three numbers. Read off the JSON value rather than the typed report: a non-finite `f64`
/// is written as `null`, which a typed read refuses, and it is read back as `NaN` (the one value it can
/// have been; a non-finite return or win rate is no number to round to zero).
fn outcome_of(json: &str) -> Result<SimOutcome, String> {
    let report: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("the backtest's report does not read: {e}"))?;
    let number = |key: &str| match &report[key] {
        serde_json::Value::Null => Ok(f64::NAN),
        v => v.as_f64().ok_or_else(|| format!("the backtest's report has no number `{key}`")),
    };
    let n_trades = report["n_trades"]
        .as_u64()
        .ok_or_else(|| "the backtest's report has no `n_trades`".to_string())?;
    Ok(SimOutcome {
        total_return: number("total_return")?,
        n_trades,
        win_rate: number("win_rate")?,
    })
}
