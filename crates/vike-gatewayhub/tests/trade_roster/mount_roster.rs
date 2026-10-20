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
//! was true, in the default roster lane. The crate-level `#![cfg]` is also why this is its own
//! test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod support;` would resolve
// against `tests/` (the root's own directory), not `tests/mount_roster/` — the same reason
// `crates/vike-tradehub/tests/daemon.rs` spells its members this way.
#[path = "mount_roster/support.rs"]
mod support;

#[path = "support/account_book.rs"]
mod account_book;
#[path = "support/permissive_grid.rs"]
mod permissive_grid;
// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

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
