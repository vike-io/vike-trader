//! What a Studio run ANSWERS — the single run's [`RunOutcome`] and its [`RunError`], a sweep's
//! [`StudioParamscan`] — and the one worker-thread spawn every Studio caller waits on.
//!
//! The types stayed here when the run pipeline that produces them moved to the backtest server
//! (`vike_backtesthub::studio_run`, 2026-11-10): the desktop renders every answer and links no
//! engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of
//! that day), so the vocabulary sits below both sides.

use std::sync::mpsc::{Receiver, channel};

use vike_analytics::TradingRecord;
use vike_model::params::ParamValue;

/// Why a Run could not produce a result.
#[derive(Debug, Clone)]
pub enum RunError {
    /// The Rhai script failed to compile (parse/compile error, line/col in the message).
    Compile(String),
    /// The data slice could not be loaded, or yielded no bars/ticks.
    Data(String),
    /// A NATIVE strategy could not be built: an unknown registry name, or a params table the
    /// registry rejected. The native twin of [`RunError::Compile`] — kept a separate variant so
    /// the UI can say "no such strategy" instead of "compile error".
    Strategy(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Compile(msg) => write!(f, "compile error: {msg}"),
            RunError::Data(msg) => write!(f, "{msg}"),
            RunError::Strategy(msg) => write!(f, "strategy error: {msg}"),
        }
    }
}

impl std::error::Error for RunError {}

impl RunError {
    /// The one line a remote failure crosses the backtesthub's `Response::Error(String)` as:
    /// `"{kind}: {message}"`, the machine class FIRST (`compile` / `data` / `strategy`) so it survives
    /// the string channel and a client prefix-matches it without re-parsing.
    /// `crate::wire_run::run_error_from_wire` is the client's inverse.
    #[must_use]
    pub fn wire_line(&self) -> String {
        let (kind, message) = match self {
            RunError::Compile(m) => ("compile", m),
            RunError::Data(m) => ("data", m),
            RunError::Strategy(m) => ("strategy", m),
        };
        format!("{kind}: {message}")
    }
}

pub type RunOutcome = Result<TradingRecord, RunError>;

/// One sweep grid point's outcome.
#[derive(Debug, Clone)]
pub struct ParamscanEntry {
    pub overrides: Vec<(String, ParamValue)>,
    pub result: TradingRecord,
}

/// A ranked parameter sweep + its deflated Sharpe and PBO across the trial set.
#[derive(Debug, Clone)]
pub struct StudioParamscan {
    pub entries: Vec<ParamscanEntry>, // ranked best-first by annualized Sharpe
    /// The winner's deflated Sharpe; `NaN` = not assessed (see [`Self::pbo`]).
    pub dsr: f64,
    /// Probability of backtest overfitting (CSCV) across the trials; `NaN` = not assessed (< 2
    /// trials, a slice too short for `DEFAULT_CSCV_SPLITS` buckets, or a non-finite matrix).
    pub pbo: f64,
    pub best_index: usize, // always 0 (entries are pre-ranked) — kept explicit for the UI
}

impl StudioParamscan {
    /// The overfit verdict on THIS grid, the producer's rule: `overfit::overfit_verdict` over its own
    /// PBO and deflated Sharpe with NO walk-forward term — the call
    /// `crates/vike-backtest/src/search/optimize/overfit.rs`'s `overfit_stats` makes for the trial
    /// ledger. A pane renders it and never assembles one from two runs (a sweep's PBO beside some
    /// other walk-forward's consistency).
    #[must_use]
    pub fn verdict(&self) -> vike_analytics::overfit::OverfitVerdict {
        vike_analytics::overfit::overfit_verdict(self.pbo, self.dsr, None)
    }
}

/// Spawn `f` on a worker thread; its single return value arrives on the returned receiver. The ONE
/// `channel()` + `thread::spawn` + `tx.send(f())` site behind the study runner and every
/// `vike-studio` worker (the remote backend's spawns, Compare all's included, `StudioChatPane::send`) —
/// the UI polls the receiver with `try_recv()` each frame so the work never blocks the egui update
/// loop.
pub fn spawn_outcome<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("ui-studio-job".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .expect("spawn outcome thread");
    rx
}
