//! **vike-hyperliquid's top-level test files as ONE test binary** (`cargo test -p vike-hyperliquid --test
//! integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-hyperliquid --test integration -- <name>::`
//! runs one file (a live smoke adds `--ignored --nocapture`, as its own `//!` says). One binary is compiled and
//! linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a
//! `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails
//! one that is neither.
//!
//! Still their own binaries: `hyperliquid_smokes`, the smokes folder, and `hyperliquid_reconcile_smoke`, named by
//! `.github/workflows/live-smokes.yml`, whose guard (`scripts/refuse_live_credentials.sh`) scans every file of a
//! binary the lane names and refuses a `#[path]`.

mod decoder_never_panics;
mod hyperliquid_dial_bound;
mod hyperliquid_market_feed_smoke;
mod hyperliquid_only_isolated_smoke;
mod hyperliquid_user_signed_smoke;
mod hyperliquid_userdata_reconnect;
mod hyperliquid_userdata_soak;
mod hyperliquid_venue_certified_vectors;
mod signed_payload_fixtures;
mod signing_vectors;
