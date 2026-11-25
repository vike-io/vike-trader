//! `series` -- the per-kind HistoryStore series round trips, ONE test binary over eight files. Each
//! file proves both halves of its kind: `HistoryStoreDataFusion` and the `HistoryStoreMemory` test
//! double.

mod chain_series;
mod cohort_series;
mod equity_series;
mod execution_log_series;
mod forecast_series;
mod funding_series;
mod perp_metrics_series;
mod sec_series;
mod text_score_series;
mod text_series;
