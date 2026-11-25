//! **vike-recorder's alert suites as ONE test binary** (`cargo test -p vike-recorder --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>_alert.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-recorder --test integration -- <suite>::` runs one suite. One binary is
//! compiled and linked instead of four, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation": no member mutates process-global state. Each member's own auto-discovered target is
//! `test = false` in `crates/vike-recorder/Cargo.toml`, so it is not built twice.
//! `tests/family_collapse_independence_gate.rs` stays its own, auto-discovered binary: the public mirror deletes it.

mod cadence_alert;
mod family_collapse_alert;
mod resolve_alert;
mod silence_alert;
