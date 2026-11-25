//! **vike-polymarket's top-level keyless test files as ONE test binary** (`cargo test -p vike-polymarket --features
//! feeds --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract, its children in `tests/<name>/` and its own
//! `#![cfg(feature = "feeds")]`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-polymarket --features feeds --test integration -- <name>::` runs one file. One binary is
//! compiled and linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation". Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds
//! only as a `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`
//! fails one that is neither.
//!
//! Still their own binaries, both under `polymarket`: `offline`, the plain offline group (its module doc carries what
//! may join it), and `polymarket_smokes`, the smokes folder.

mod decoder_never_panics;
mod libm_platform_probe;
