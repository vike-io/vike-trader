//! `series` -- the per-kind HistStore series round trips, ONE test binary over six files. Each file
//! proves both halves of its kind: `DataFusionHist` and the `MemHistStore` test double.

mod chain_series;
mod cohort_series;
mod equity_series;
mod exec_log_series;
mod funding_series;
mod perp_metrics_series;
