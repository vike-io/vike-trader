//! **vike-dukascopy's top-level test files as ONE test binary** (`cargo test -p vike-dukascopy --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-dukascopy --test integration -- <name>::`
//! runs one file (a live smoke adds `--ignored --nocapture`, as its own `//!` says). One binary is compiled and
//! linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation":
//! the one process-global write here is the kept list, held alike (the same rows) by every member that holds it.
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a
//! `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails
//! one that is neither.
//!
//! `support` is the fake-sidecar suites' shared helper (`tests/support/mod.rs`), declared once here so it compiles
//! once; `sidecar`'s children reach it as `crate::support`. Still its own binary: `no_kept_list`, which must run in
//! a process that holds no kept list.

mod support;

mod bi5_agrees_with_the_bridge;
mod decoder_never_panics;
mod dukascopy_adapter;
mod dukascopy_currency_money;
mod dukascopy_jar_probe;
mod dukascopy_live_smoke;
mod dukascopy_ready_balance;
mod dukascopy_recon_leg;
mod jdk_pin_gate;
mod position_legs_golden;
mod quotes;
mod sidecar;
