//! **The `backfill-serve` suites, as ONE test binary**
//! (`cargo test -p vike-datahub --features backfill-serve --test backfill_serve`).
//!
//! Each suite keeps its file (`tests/backfill_serve/<suite>.rs`) and its own
//! `#![cfg(feature = "backfill-serve")]`, and is a MODULE here, so a test's id is `<suite>::<test>`.
//! The gate below is the one every member carries: the sanctioned extension of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" (a crate-level gate on the group entry
//! when every member shares it identically). None mutates process-global state.
//! `tests/start_history_store.rs` shares the gate and stays its own binary: its two `#[ignore]`d
//! measurements are run by `--test start_history_store`.
#![cfg(feature = "backfill-serve")]

mod backfill_roundtrip;
mod oanda_history_lane;
mod seed_series;
