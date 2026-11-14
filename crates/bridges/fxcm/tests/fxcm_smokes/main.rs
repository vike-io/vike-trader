//! **The live FXCM smokes and the offline gate on how they find the demo login, ONE test binary**
//! (`--test fxcm_smokes`): the eight files that share `common` (`demo_config`, `demo_config_from`,
//! `fxcm_settings`), so `common` compiles ONCE instead of once per binary.
//!
//! Every smoke is `#[ignore]`d and runs only when asked, by module, with the daemon's own FXCM account
//! deactivated for the run (one session per login): `cargo test -p vike-fxcm --test fxcm_smokes -- --ignored
//! --nocapture fxcm_live_smoke::` (each test's name is unchanged after its module prefix), per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". `fxcm_smoke_login` is the offline gate
//! (no `#[ignore]`), and its source pin reads every member here. Each member keeps its own file, module doc
//! and tests.

mod common;

mod fxcm_classes_smoke;
mod fxcm_close_smoke;
mod fxcm_currency_smoke;
mod fxcm_live_smoke;
mod fxcm_reconcile_smoke;
mod fxcm_smoke_login;
mod fxcm_stop_smoke;
mod fxcm_subscribe_smoke;
