//! `shipped_cli` -- ONE test binary over what used to be sixteen: every vike-cli integration suite
//! that drives the SHIPPED binary through `tests/common/` (the plain runners, the scratch credential
//! store and the positive-argv pin harness) or through `vike_cli::test_support` (the planted-engine
//! plant and its `ETXTBSY` retry, compiled once by the library).
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, so `tests/common/` used to be
//! compiled into each of the sixteen; here it is compiled ONCE. Each former `tests/<name>.rs` is now
//! `tests/shipped_cli/<name>.rs` (with its `#[path]` children, `backtest_cli/` and `data_cli/`),
//! included below as a plain module: the tests, their names and their bodies are unchanged -- only
//! the binary they link into moved, so a test's id is `shipped_cli <name>::<test>`.
//!
//! ```sh
//! cargo test -p vike-cli --test shipped_cli                # the whole group
//! cargo test -p vike-cli --test shipped_cli -- data_cli::  # one member, by module name
//! ```
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member
//! is plain -- no `#![cfg]` feature gate, no `#[ignore]`, no proptest sidecar, no process-global
//! mutation -- because plain `cargo test` (the `light-consumers` lane in `scripts/ci_feature_suite.sh`)
//! runs one binary's tests as THREADS in one process. `tests/planted_binary_retry.rs` (the retry's
//! own proof) stays OUT: its `catch_panic` swaps the process-wide panic hook, and muted for a whole
//! retry budget it would swallow the panic message of any member failing beside it. That is why the
//! half it shares with the group lives in the library rather than in `tests/common/`.
//!
//! The crate's other plain suites, which use no `tests/common/`, are the sibling group
//! `tests/cli_verbs/main.rs`: TWO groups rather than one, because a single test crate is one serial
//! rustc unit, and two build side by side (MEASURED 2026-10-31: one group doubled the cold wall time).

#[path = "../common/mod.rs"]
mod common;

mod argv_accepted_backtest;
mod argv_accepted_data_trade;
mod argv_accepted_ops;
mod backtest_cli;
mod backtest_runs_cli;
mod config_check_cli;
mod config_cli;
mod data_cli;
mod datahub_control_key_cli;
mod exit_codes;
mod study_report_refusal_cli;
mod trade_doctor_cli;
mod trade_hub_e2e;
mod trade_plane_cli;
mod trade_status_cli;
mod trade_venue_cli;
