//! **vike-alpaca's top-level test files as ONE test binary** (`cargo test -p vike-alpaca --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, and its `//!` contract; here it is a MODULE, so a test's id is
//! `<name>::<test>` and `cargo test -p vike-alpaca --test integration -- <name>::` runs one file (a live smoke adds
//! `--ignored --nocapture`, as its own `//!` says). One binary is compiled and linked instead of one per file, by
//! the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": the one process-global write here is
//! alpaca's kept list, held alike (the same rows) by every member that holds it. Discovery is off
//! (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a `mod` line here or a
//! `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails one that is neither.
//!
//! `common` is the shared helper (`tests/common/mod.rs`), declared once here so it compiles once; a member reaches
//! it as `crate::common`. Still its own binary: `alpaca_reconcile_smoke`, named by
//! `.github/workflows/live-smokes.yml`, whose guard (`scripts/refuse_live_credentials.sh`) scans every file of a
//! binary the lane names and follows only a plain `mod` tree.

mod common;

mod alpaca_live_smoke;
mod auth_refresh;
mod config_env;
mod decoder_never_panics;
mod engine_fill;
mod execution_actor_wiring;
mod id_less_fills_are_refused;
mod mapper_data;
mod mapper_orders;
mod reconcile_client_parse;
mod whole_share_quantity;
