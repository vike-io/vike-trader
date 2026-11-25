//! **vike-datahub-client's integration suites as ONE test binary** (`cargo test -p vike-datahub-client --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-datahub-client --test integration -- <suite>::` runs one suite. One
//! binary is compiled and linked instead of one per file, grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no suite mutates process-global state.
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a `tests/*.rs` file no `mod` line
//! here declares would compile into nothing; `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s
//! `every_root_test_file_is_built_where_discovery_is_off` fails until it is declared.
//!
//! `thin_link_negotiation` stays its own binary, a `[[test]]` row of its own: it turns on the process-wide
//! `DatahubClient::set_thin_link_for_this_process`, so in this process every other suite's dial would ask LZ4.

mod archive_import_negotiation;
mod backfill_cancel_negotiation;
mod backfill_negotiation;
mod connect_with_keys;
mod coverage_negotiation;
mod exec_history_negotiation;
mod execution_stream_negotiation;
mod execution_stream_wire;
mod forecasts_negotiation;
mod funding_history_negotiation;
mod gateway_status_negotiation;
mod handshake;
mod history_channels_negotiation;
mod input_bars_negotiation;
mod instrument_lists_of_venues_negotiation;
mod kept_rows_stamped_negotiation;
mod lists_wire;
mod market_data_lane_caps;
mod market_data_negotiation;
mod options_chain_negotiation;
mod options_poll;
mod remote_bar_pages;
mod remote_bar_resample;
mod remote_pool;
mod remote_redial;
mod remote_roundtrip;
mod remote_tick_columns;
mod reply_timeout;
mod seed_class_negotiation;
mod series_days_negotiation;
mod server_handshake;
mod source_negotiation;
mod strat_feed;
mod strategy_lane_wire;
mod symbol_facts_negotiation;
mod text_history_negotiation;
mod tool_feeds_negotiation;
mod tool_feeds_poll;
mod wire_codec_negotiation;
mod wire_forms_probe;
