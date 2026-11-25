//! The saved-runs seam: [`StudyRuns`] reads what a FINISHED run of `<project>/user_data/runs/` holds,
//! by run id, for a study that measures runs rather than bars (book W21's alpha pool reads their
//! forecasts; the book's forward report, chapter 22, reads a backtest's closed trades).

use vike_model::{ForecastRow, Trade};

/// What a study may read of the project's saved runs, by run id: read-only, a finished run only, and
/// never a path (the host holds the id to one run directory's name). The host reads; inside the jail a
/// study file asks its parent, so the jailed process still reads nothing but its artifact.
///
/// OPTIONAL like the learner and the backtest ([`crate::StudyContext::runs`]): a host with no runs
/// root to read offers none, and a study that needs runs says so in its own words and stops.
pub trait StudyRuns: Send + Sync {
    /// Every forecast run `run_id`'s strategy published, in publishing order (the run's
    /// `vike_runs::FORECASTS_FILE`); empty for a finished run that published none. `Err` is the
    /// host's words: an id that names no run, a run still being written, a document that does not read.
    fn forecasts(&self, run_id: &str) -> Result<Vec<ForecastRow>, String>;

    /// Every trade run `run_id` closed, in closing order (the run's `vike_runs::TRADES_FILE`); empty for
    /// a run that closed none. `Err` is the host's words, as for [`Self::forecasts`], and also for a run
    /// that kept NO ledger (its producer writes none: that is not a run without trades) or kept only a
    /// prefix of it (a comparison over the rest would read trades that were made as missing).
    fn trades(&self, run_id: &str) -> Result<Vec<Trade>, String>;

    /// What the FORWARD run `run_id` recorded (book chapter 22: a strategy file mounted live, recorded by the
    /// trade daemon as a run of kind `forward`): its mount, its account and its window. `Err` is the host's
    /// words, as for [`Self::forecasts`], and also for a run that is not a forward run.
    fn forward(&self, run_id: &str) -> Result<StudyForwardRun, String>;
}

/// A forward run as a study reads it ([`StudyRuns::forward`]): the mount's facts the trade daemon recorded when
/// it opened, and the window the test traded in.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudyForwardRun {
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// `venue/LABEL`, `DEFAULT` for the venue's unlabelled account.
    pub account: String,
    /// The strategy file and the sha256 of the bytes the node ran.
    pub strategy_file: String,
    pub strategy_sha256: String,
    /// When the mount opened, epoch milliseconds (UTC).
    pub started_ms: i64,
    /// When it closed; `None` while it still runs (or its node stopped without an unmount).
    pub ended_ms: Option<i64>,
    /// The backtest run named to hold it against, when one was.
    pub reference_run: Option<String>,
}
