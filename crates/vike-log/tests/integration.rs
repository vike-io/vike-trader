//! **vike-log's test files that touch no process-global state, as ONE test binary**
//! (`cargo test -p vike-log --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-log --test integration -- <name>::` runs one file. Grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": `log_bridge`'s captures go through the
//! floored scoped door (`vike_log::capture::scoped`) and its `LogTracer` is idempotent, and `log_retention`
//! builds appenders and emits nothing. Every other file of `tests/` sets a process-global (`init`'s subscriber and
//! panic hook, `capture::global`'s default) or must be the process's only capture, so it stays a binary of its
//! own (`crates/vike-log/Cargo.toml`'s `[[test]]` rows). Test discovery is OFF (`autotests = false`), and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails on a root file that is neither a `mod` here nor a
//! row.

mod log_bridge;
mod log_retention;
