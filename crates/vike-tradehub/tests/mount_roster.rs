//! **The roster-driven tests of `vike-mount`'s generic fold, run against the REAL registry.**
//! They lived in `vike-mount`'s own `src/*_tests.rs` and ran through its transitional registry
//! until the venue mount contract finished (docs/decisions/0096): `vike-mount` names no bridge
//! now, so it cannot build a registry of real venues, and a bridge cannot call `vike-mount`
//! without a dependency cycle. This crate holds the registry and sees both (the 2026-09-29
//! amendment put the registry here). One module per file they came from; their assertions are
//! the ones they had there.
//!
//! ⚠ **Default build only.** `vike-mount`'s registry carried ibkr, fxcm and polymarket
//! `FeatureAbsent` in every build, and these assertions were written against that. Under one of
//! this crate's features the matching row is the bridge's mount, and that venue's feature-on half
//! is `crates/vike-tradehub/tests/ibkr_mount.rs`, `crates/vike-tradehub/tests/fxcm_mount.rs` or
//! `crates/vike-tradehub/tests/polymarket_mount.rs` — so each assertion here runs exactly where it
//! was true, in the default roster lane. The crate-level `#![cfg]` is also why this is its own
//! test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod support;` would resolve
// against `tests/` (the root's own directory), not `tests/mount_roster/` — the same reason
// `crates/vike-tradehub/tests/daemon.rs` spells its members this way.
#[path = "mount_roster/support.rs"]
mod support;

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
