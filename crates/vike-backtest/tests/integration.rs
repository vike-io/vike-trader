//! **vike-backtest's plain integration suites as ONE test binary** (`cargo test -p vike-backtest --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-backtest --test integration -- <suite>::` runs one suite. One binary
//! is compiled and linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation": no suite mutates process-global state. Discovery is off (`autotests = false` in
//! this crate's `Cargo.toml`), so a `tests/*.rs` file that neither a `mod` line here nor a `[[test]]` row names
//! would compile into nothing; `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s
//! `every_root_test_file_is_built_where_discovery_is_off` fails on it.
//!
//! Its own binary on purpose, each a `[[test]]` row of its own: `libm_platform_probe` (the per-crate probe
//! decision 0074 names, run as `--test libm_platform_probe` in every crate that has one) and
//! `engine_cfg_reaches_the_engine` (it `#[path]`-includes vike-ops' `tests/common/repo.rs`, which the public mirror
//! withholds, so `scripts/publish_mirror.sh` withholds this file too and a module of it here would stop the mirror's
//! test build).

mod bar_source;
mod cheap_np_profile;
mod funding_carry_demo;
mod funding_join;
mod funding_schedule;
mod harness_risk_wiring;
mod option_legs;
mod portfolio_mounts;
mod rotation_top_k_profile;
mod run_record_completeness;
mod tick_replay;
mod venue_hold_store_replay;
mod walkforward_optimize;
mod walkforward_windows;
