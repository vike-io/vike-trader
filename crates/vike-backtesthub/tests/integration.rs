//! **vike-backtesthub's plain integration suites as ONE test binary** (`cargo test -p vike-backtesthub --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-backtesthub --test integration -- <suite>::` runs one suite. One
//! binary is compiled and linked instead of one per file, grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no suite mutates process-global state.
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a `tests/*.rs` file that neither a
//! `mod` line here nor a `[[test]]` row names would compile into nothing;
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails on it.
//!
//! Its own binary on purpose, each a `[[test]]` row of its own: `engine_cli` (the group that spawns the shipped
//! `backtesthub`), `paramscan_profile_summary` and `studio_summary` (they summarize runs and read them back, and
//! the held runs are process-wide), `strategy_file_build` (the builder seam is process-global), `sdk_source` (the
//! SDK reader seam is process-global) and `sandbox_child_memory` (Linux-only, and it starts its own binary again
//! with `--exact <test>`). `backtesthub_profile_roundtrip` is the one member that holds runs; no other member does.

mod backtest_plane;
mod backtesthub_cancel;
// `pub`, as its own binary's crate root was: its `pub mod user_forward_report` (the book's fixture, compiled whole)
// stays exported, so the items this suite does not call are not dead code.
pub mod backtesthub_profile_roundtrip;
mod idle_read_timeout;
mod research_bin_read_failures;
mod studio_wire_parity;
mod study_cli_argv;
mod templates_execute;
mod walkforward_annualization;
