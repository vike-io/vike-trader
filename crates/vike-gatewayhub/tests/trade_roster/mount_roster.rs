//! **The roster-driven tests of `vike-tradehub-core`'s generic fold, run against the REAL registry.**
//! They lived in `vike-tradehub-core`'s own `src/*_tests.rs` and ran through its transitional registry
//! until the venue-adapter contract finished (docs/decisions/0096): `vike-tradehub-core` names no bridge
//! now, so it cannot build a registry of real venues, and a bridge cannot call `vike-tradehub-core`
//! without a dependency cycle. This crate holds the registry and sees both (the 2026-09-29
//! amendment put the registry here). One module per file they came from; their assertions are
//! the ones they had there.
//!
//! ⚠ **Default build only.** `vike-tradehub-core`'s registry carried ibkr, fxcm and polymarket
//! `FeatureAbsent` in every build, and these assertions were written against that. Under one of
//! this crate's features the matching row is the bridge's mount, and that venue's feature-on half
//! is `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`, `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or
//! `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs` — so each assertion here runs exactly where it
//! was true, in the default roster lane. A member of the `trade_roster` group (`main.rs`), under its
//! module-level `#![cfg]`; the helpers its children name through `crate::` (`support`, `local`,
//! `account_book`, `permissive_grid`) are the group root's.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

#[path = "mount_roster/fee_schedule.rs"]
mod fee_schedule;
#[path = "mount_roster/policy.rs"]
mod policy;
#[path = "mount_roster/preconnect.rs"]
mod preconnect;
#[path = "mount_roster/preflight.rs"]
mod preflight;
#[path = "mount_roster/server_time.rs"]
mod server_time;
#[path = "mount_roster/startup.rs"]
mod startup;
#[path = "mount_roster/stay_paper_matrix.rs"]
mod stay_paper_matrix;
#[path = "mount_roster/symbol_grid.rs"]
mod symbol_grid;
