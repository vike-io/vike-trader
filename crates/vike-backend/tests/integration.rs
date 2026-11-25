//! **vike-backend's plain integration suites as ONE test binary** (`cargo test -p vike-backend --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, and is a MODULE here, so a test's id is `<suite>::<test>`
//! and `cargo test -p vike-backend --test integration -- <suite>::` runs one suite. Every test and assertion
//! is the one the suite's own binary ran; one binary links instead of four. Each member runs the multicall as
//! a CHILD (`CARGO_BIN_EXE_vike-backend`, which cargo sets for every test target of the package) with its
//! own working directory and environment, so none mutates this process's state: grouped by
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". Each member's whole-file feature gate is
//! one `full` turns on, and the self dev-dependency turns `full` on in every test build, so the gates never
//! split the group. The suites that stay their own binary, and why, are the `[[test]]` rows of
//! `crates/vike-backend/Cargo.toml`.
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so every file keeps its path. A new
//! `tests/*.rs` is built only as a `mod` here or a `[[test]]` row, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails until it is one or the other.

mod fresh_project_quickstart;
mod gatewayhub_verb_dispatches;
// In process, no child: the workspace's refusal catalog, rendered here because this crate links every crate that refuses.
mod refusal_catalog;
mod report_verb_dispatches;
mod strategy_builder_verb_dispatches;
