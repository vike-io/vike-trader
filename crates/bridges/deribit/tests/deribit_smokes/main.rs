//! **The deribit demo smokes and probes that resolve their account through `demo_creds`, ONE test
//! binary** (`--test deribit_smokes`), so `demo_creds` (the demo account's credentials) compiles ONCE
//! instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-deribit --test deribit_smokes -- --ignored --nocapture deribit_restore_smoke::`
//! (each test's name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `deribit_reconcile_smoke` and `deribit_fine_tick_price_smoke`
//! from this binary, and `scripts/refuse_live_credentials.sh` scans EVERY file the binary it names
//! compiles, following plain `mod` declarations from this file; it refuses one it cannot follow, and
//! refuses the whole lane if any member asks for a live tier. A new member is scanned by the lane too.

mod demo_creds;

mod deribit_fine_tick_price_smoke;
mod deribit_identity_probe;
mod deribit_native_stop_testnet_smoke;
mod deribit_reconcile_smoke;
mod deribit_restore_smoke;
mod deribit_stop_amend_testnet_smoke;
