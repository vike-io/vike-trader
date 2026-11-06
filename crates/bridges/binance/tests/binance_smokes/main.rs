//! **The binance demo smokes, probes and drills that share a credential helper, ONE test binary**
//! (`--test binance_smokes`), so `demo_creds` (the demo account's credentials), `drill_account` (the
//! classes and modes drills' account) and `perp_algo_venue` (the two perp algo drills' venue) each
//! compile ONCE instead of once per binary.
//!
//! Every smoke is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture binance_restore_smoke::`
//! (each test's name is unchanged after its module prefix; add `--test-threads=1` for the drills that
//! say so), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". Each member keeps its own
//! file, module doc and tests; the spot witness's offline instrument tests run in the ordinary lane as
//! they did.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `binance_reconcile_smoke` and
//! `binance_perp_fine_tick_price_smoke` from this binary, and `scripts/refuse_live_credentials.sh` scans
//! EVERY file the binary it names compiles, following plain `mod` declarations from this file; it
//! refuses one it cannot follow, and refuses the whole lane if any member asks for a live tier. A new
//! member is scanned by the lane too.

mod demo_creds;
mod drill_account;
mod perp_algo_venue;

mod binance_classes_exec_smoke;
mod binance_classes_smoke;
mod binance_fine_tick_price_smoke;
mod binance_identity_probe;
mod binance_modes_smoke;
mod binance_perp_algo_restore_smoke;
mod binance_perp_fine_tick_price_smoke;
mod binance_perp_restore_smoke;
mod binance_reconcile_smoke;
mod binance_restore_smoke;
mod perp_algo_smoke;
