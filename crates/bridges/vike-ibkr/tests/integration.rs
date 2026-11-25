//! **vike-ibkr's top-level test files as ONE test binary** (`cargo test -p vike-ibkr --features ibkr,test-support
//! --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract, its children in `tests/<name>/` and its own
//! `#![cfg(feature = …)]` gate; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-ibkr --features ibkr,test-support --test integration -- <name>::` runs one file. One binary
//! is compiled and linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation" (its sanctioned extension: a member's gate sits on the member). Discovery is off
//! (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a `mod` line here or a
//! `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails one that is neither.
//! The live smokes are the other binary, `tests/ibkr_smokes/`.

mod decoder_never_panics;
mod ibkr_amend;
mod ibkr_contract_routing;
mod ibkr_cpapi_ambiguous_place;
mod ibkr_cpapi_decode;
mod ibkr_cpapi_lifecycle;
mod ibkr_cpapi_market_data_decode;
mod ibkr_cpapi_restored_cancel;
mod ibkr_engine_fill;
mod ibkr_feeds_lifecycle;
mod ibkr_lifecycle;
mod ibkr_market_data_map;
mod ibkr_reconcile_parse;
mod ibkr_socket_reconcile_parse;
