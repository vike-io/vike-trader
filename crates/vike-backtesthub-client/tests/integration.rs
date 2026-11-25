//! **vike-backtesthub-client's suites as ONE test binary** (`cargo test -p vike-backtesthub-client --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-backtesthub-client --test integration -- <suite>::` runs one suite. One
//! binary is compiled and linked instead of one per file, grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no suite mutates process-global state.
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a `tests/*.rs` file no `mod` line
//! here declares would compile into nothing; `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s
//! `every_root_test_file_is_built_where_discovery_is_off` fails until it is declared.

mod backtest_plane_wire_pin;
mod msgpack_negotiation;
mod paramscan_profile_summary_negotiation;
mod plane_guard;
mod run_curve_negotiation;
mod run_fields_negotiation;
mod run_summary_negotiation;
mod sdk_source_negotiation;
mod search_method_negotiation;
mod slippage_model_negotiation;
mod strategy_sdk_negotiation;
mod study_negotiation;
mod walkforward_search_negotiation;
mod wire_tag_fixtures;
