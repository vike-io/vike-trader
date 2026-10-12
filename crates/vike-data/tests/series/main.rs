//! `series` -- the per-kind HistStore series round trips, ONE test binary over six files. Gated
//! `any(..)`, so it also compiles and runs under `test-support` alone (no DataFusion).
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

mod chain_series;
mod cohort_series;
mod equity_series;
mod exec_log_series;
mod funding_series;
mod perp_metrics_series;
