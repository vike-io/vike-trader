//! The data `alpha_pool`'s tests run it on, in ONE place for both test binaries that compile the study
//! (`crates/vike-user-research/tests/pipeline.rs` and `crates/vike-strategy-sandbox/tests/study_child.rs`):
//! twelve 1m bars of one symbol in the workspace's own store double, and three saved runs held in memory
//! (a `StudyRuns` double, as a host answers from `<project>/user_data/runs/`). `#[cfg(test)]` in the study,
//! so the study's own build never compiles it.
//!
//! The three runs, each one forecast a bar: `trend` says the sign of the next bar's return (it knows the
//! answer: the pool must weight it most), `fade` alternates its sign whatever comes, and `hold` is always
//! long by half.

use std::path::PathBuf;
use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;
use vike_model::{Forecast, ForecastRow};
use vike_user_research::{StudyContext, StudyRuns};

pub const VENUE: &str = "binance";
pub const SYMBOL: &str = "BTCUSDT";
pub const BAR_MS: i64 = 60_000;
pub const CLOSES: [f64; 12] =
    [100.0, 101.0, 99.0, 102.0, 104.0, 103.0, 105.0, 102.0, 106.0, 107.0, 105.0, 108.0];
pub const RECIPE: &str = "runs = [\"trend\", \"fade\", \"hold\"]\n";

/// The bars, and the window that covers them.
pub fn store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let store = HistoryStoreMemory::new();
    let bars: Vec<Bar> = CLOSES
        .iter()
        .zip(0i64..)
        .map(|(c, i)| Bar::ohlcv(BAR_MS * i, *c, *c, *c, *c, 1.0))
        .collect();
    store.append_bars(VENUE, SYMBOL, "1m", &bars, Some("fixture")).expect("the fixture bars");
    (Arc::new(store), TimestampRange::of(0, BAR_MS * (CLOSES.len() as i64 - 1)))
}

/// The three saved runs; any other id is the host's refusal, as a real one words it.
pub struct Runs;

impl StudyRuns for Runs {
    fn forecasts(&self, run_id: &str) -> Result<Vec<ForecastRow>, String> {
        let value = |t: usize| -> Option<f64> {
            match run_id {
                "trend" => {
                    let next = *CLOSES.get(t + 1)?;
                    Some(if next > CLOSES[t] { 1.0 } else { -1.0 })
                }
                "fade" => Some(if t.is_multiple_of(2) { 1.0 } else { -1.0 }),
                "hold" => Some(0.5),
                _ => None,
            }
        };
        if value(0).is_none() {
            return Err(format!("no run {run_id:?} under the runs root"));
        }
        Ok((0..CLOSES.len())
            .filter_map(|t| {
                // Decided at bar t's CLOSE, as a backtest stamps a bar step's forecast.
                let forecast = Forecast::new(value(t)?, BAR_MS, None).ok()?;
                Some(ForecastRow {
                    timestamp: BAR_MS * (t as i64 + 1),
                    symbol: SYMBOL.into(),
                    forecast,
                })
            })
            .collect())
    }

    /// The pool reads forecasts only; these runs keep no ledger, as a run whose producer saves none.
    fn trades(&self, run_id: &str) -> Result<Vec<vike_model::Trade>, String> {
        Err(format!("run {run_id:?} kept no trade ledger"))
    }

    /// None of these is a forward run.
    fn forward(&self, run_id: &str) -> Result<vike_user_research::StudyForwardRun, String> {
        Err(format!("run {run_id:?} is not a `forward` run"))
    }
}

/// The context the study runs in: the bars, the saved runs, a scratch path.
pub fn context(scratch: PathBuf) -> StudyContext {
    let (store, window) = store();
    StudyContext::new(store, window, scratch).with_runs(Arc::new(Runs))
}
