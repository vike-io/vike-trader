//! **The live IBKR smokes and probes, ONE test binary** (`--test ibkr_smokes`): the files that share
//! `common`'s `ibkr_settings` (the `venue.ibkr.*` gateway rows from the settings database), so it compiles ONCE
//! instead of once per binary.
//!
//! Every member is `#[ignore]`d (each needs a gateway process on the box) and runs only when asked, by module:
//! `cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_smoke::` (each test's
//! name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Each member keeps its own file, module doc, tests and its feature gate (`#![cfg(feature = "ibkr")]`,
//! `"ibkr-socket"` or `"ibkr-cpapi"`, now a module-level one): a build compiles exactly the members it compiled
//! as binaries before, and `ibkr_smoke` (no gate) keeps `common` used in every build.

mod common;

mod ibkr_cpapi_mktdata_smoke;
mod ibkr_cpapi_smoke;
mod ibkr_data_probe_smoke;
mod ibkr_delayed_price_smoke;
mod ibkr_flagged_tif_smoke;
mod ibkr_forex_futures_probe;
mod ibkr_fx_smoke;
mod ibkr_mktdata_smoke;
mod ibkr_reconcile_smoke;
mod ibkr_restore_smoke;
mod ibkr_smoke;
mod ibkr_socket_reconcile_smoke;
mod ibkr_summary_cancel_probe;
mod ibkr_untaught_replay_smoke;
