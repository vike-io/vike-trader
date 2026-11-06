//! **The live hyperliquid smokes and probes, ONE test binary** (`--test hyperliquid_smokes`): six of the seven
//! files that share the demo-credential gate in `tests/common/mod.rs` (`demo_creds`, `SMOKE_TIER`), so it
//! compiles once for all six instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-hyperliquid --test hyperliquid_smokes -- --ignored --nocapture hyperliquid_restore_smoke::`
//! (each test's name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **The seventh, `tests/hyperliquid_reconcile_smoke.rs`, stays a binary of its own, so the helper compiles
//! twice (here and there)**: `.github/workflows/live-smokes.yml` runs it, and `scripts/refuse_live_credentials.sh`
//! scans EVERY file of the binary a lane names. `hyperliquid_permissioning_smoke` (a member here) plants a
//! `live` account row to prove the demo view excludes it, and `hyperliquid_restore_smoke`'s doc names the live
//! tier it never touches; in reconcile's binary either would make the guard refuse the whole lane. The helper
//! stays at `tests/common/mod.rs`, where reconcile's plain `mod common;` (the only shape the guard can follow)
//! finds it.

#[path = "../common/mod.rs"]
mod common;
mod ingest;

mod hyperliquid_instant_fill_smoke;
mod hyperliquid_l1_class_transfer_probe;
mod hyperliquid_leverage_universe_smoke;
mod hyperliquid_permissioning_smoke;
mod hyperliquid_post_only_smoke;
mod hyperliquid_restore_smoke;
