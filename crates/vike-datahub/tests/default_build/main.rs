//! **The default build's suites, as ONE test binary** (`cargo test -p vike-datahub --test default_build`).
//!
//! Each suite is still its own file here (`tests/default_build/<suite>.rs`, children in
//! `tests/default_build/<suite>/`) and is a MODULE of this binary, so a test's id is `<suite>::<test>`
//! and `-- <suite>::` runs one suite. Every test and assertion is the one the suite's own binary ran;
//! one binary links once instead of twenty-three times.
//!
//! Grouped by `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member compiles on
//! the default build (no feature gate) and none mutates process-global state (`env::set_var`,
//! `set_current_dir`, a global `tracing` subscriber, a panic hook), because a threaded `cargo test` runs
//! all of them in this one process. `feed_broker`'s one `#[ignore]`d test is a light, run-once
//! measurement (`-- --ignored --nocapture feed_broker::m3_filter_cost_per_event`), not a live call.
//! Which suites stay their own binary, and why, is `crates/vike-datahub/CLAUDE.md`'s `tests/` entry.

mod archive_import;
mod auth_roundtrip;
mod backfill_cancel;
mod backfill_readback;
mod csv_upload;
mod delete_gate;
mod dukascopy_bus;
mod execution_fills_two_doors;
mod execution_history_roundtrip;
mod feed_broker;
mod gateway_links;
mod gateway_status;
mod help_cli;
mod history_channels;
mod load_bars_bounded;
mod market_data_attach;
mod market_data_hub;
mod materialize_state_machine_fold;
mod news_logo_assets;
mod operator_instruments;
mod options_chain;
mod plane_split;
mod roundtrip;
mod scan_ceilings;
mod start_history;
mod strategy_gateway;
mod strategy_lane;
mod symbol_sets;
mod tool_feeds;
mod venue_catalog;
mod wire_latency;
