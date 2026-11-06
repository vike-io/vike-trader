//! **The `datafusion-serve` suites, as ONE test binary**
//! (`cargo test -p vike-datahub --features datafusion-serve --test datafusion_serve`).
//!
//! Each suite keeps its file (`tests/datafusion_serve/<suite>.rs`) and its own
//! `#![cfg(feature = "datafusion-serve")]`, and is a MODULE here, so a test's id is `<suite>::<test>`.
//! The gate below is the one every member carries: the sanctioned extension of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" (a crate-level gate on the group entry
//! when every member shares it identically). None mutates process-global state.
#![cfg(feature = "datafusion-serve")]

mod composed_store_roundtrip;
mod grouped_tick_paging;
mod source_lane_wire;
mod wire_six_verbs_roundtrip;
