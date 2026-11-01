//! **The roster tests, ONE binary** (`trade_roster`): the suites that moved with the venue half (wave 2, W9b)
//! and drive the REAL bridges' arming, preflight probes and adapter start through the in-process backend.
//!
//! They were fifteen binaries (`trade_live_lock.rs` came from `tests/`), and every one `#[path]`-included the
//! same helpers (`support/local_backend.rs` with its `bridge_backend.rs`, `account_row.rs`, ...), so each
//! helper compiled once per binary. Here each helper is declared ONCE, below, and every member names it
//! through `crate::`. A member keeps its own file, its module doc, its tests and its crate-level `#![cfg]`
//! (now a module-level one): the default build compiles the `not(any(ibkr, polymarket, fxcm))` members, a
//! build with those features the matching `*_adapter` members, and `registry_shape`, `trade_live_lock` and
//! `live_tier_not_wired_log` every build, exactly where each ran before. A test's name gained its file's module as a prefix
//! (`mount_roster::policy::…`); nothing else about it changed.
//!
//! ⚠ **nextest runs every test in its own process** (CI, every job), which is what the members' process-global
//! state needs: the process-exclusive claim (`crates/vike-gatewayhub/src/trade/adapters/exclusive.rs`'s
//! `CLAIMED`) and the HALT resolver. A plain `cargo test --test trade_roster` runs the members' tests as threads of
//! one process; a member that reads the log does it through `vike_log::capture` (scoped to its own thread),
//! never a global subscriber. `server_time_smoke.rs` (a live smoke) stays its own binary: it shares no
//! helper. `startup_preflight_smoke` (`#[ignore]`d, live) is a member:
//! `cargo test -p vike-gatewayhub --test trade_roster -- --ignored --nocapture startup_preflight_smoke`.

// The shared helpers, each compiled once for every member. A helper only `not(any(...))` members use
// carries their gate, so a feature build does not compile it unused. The account-row builders and the
// arming credential sets are the tradehub's fixtures too, so each has one copy compiled once for both
// crates: the builders in tradehub-core's lib (`test-support`), the credential sets in this crate's
// `testkit` (they name venues, which tradehub-core's `src/` may not); members still name them through
// `crate::`.
use vike_gatewayhub::testkit::arming_fixtures;
use vike_tradehub_core::test_support::account_book;
#[cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]
#[path = "support/account_row.rs"]
mod account_row;
#[path = "support/bridge_backend.rs"]
mod bridge_backend;
// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;
#[cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]
#[path = "support/permissive_grid.rs"]
mod permissive_grid;
// `mount_roster`'s own policy builders, which its children name as `crate::support`.
#[cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]
#[path = "mount_roster/support.rs"]
mod support;

mod account_credential_isolation;
mod arming_parity;
mod book_identity_table;
mod declared_symbol_grid_wiring;
mod dukascopy_sidecar_holder;
mod fxcm_adapter;
mod ibkr_adapter;
mod live_tier_not_wired_log;
mod mount_roster;
mod polymarket_adapter;
mod registry_shape;
mod shared_book_report;
mod startup_preflight_smoke;
mod trade_live_lock;
mod unaddressable_account_silence;
