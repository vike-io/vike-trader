//! **The scripted backend, as the roster tests that STAY in the tradehub drive the fold** (wave 2, W9b).
//!
//! These tests exercise the tradehub's own folds (the account fan-out, the live-lock set, the paper
//! assembly, the feed plans) and used to run them over `LocalBackend` and the real `REGISTRY`. The venue
//! half moved to the gatewayhub (`crates/vike-gatewayhub/src/trade/adapters/`), where the roster tests that
//! need the REAL bridges' arming went with it (`crates/vike-gatewayhub/tests/trade_roster/`); what stays
//! here asks the fold a question about ACCOUNTS, so it is answered by
//! [`vike_tradehub_core::ScriptedBackend::credentialed`], the generic credentialed-venue table of the
//! `account` rows the test plants: an account arms at exactly its row's tier iff it holds a credential, a
//! `paper` account runs the simulator, an account the table does not run carries the table's own cause.
//! The registry-taking entry points the tests used keep their spellings minus the registry argument, so a
//! test reads as it always did; each is one question of the shared rig
//! (`vike_tradehub_core::test_support::rig`, which the gatewayhub's roster tests ask over their backend).
//!
//! `#[path]`-included once by `tests/daemon.rs`, its one user binary (every roster-driven suite is a
//! `daemon` member), so an edit to it rebuilds that binary alone; the crate's own unit tests keep their
//! twin (`src/scripted_arming_tests.rs`). It names nothing but `vike_tradehub_core` and the two crates
//! its signatures carry.
//!
//! The tests that need a real bridge's rule (aster's testnet fallback, ibkr's held orders, dukascopy's one
//! sidecar, the polymarket exec flag) are not here: they are the gatewayhub's.

use std::collections::HashMap;
use std::path::PathBuf;

use vike_config::VenueArming;
use vike_tradehub_core::test_support::rig::Rig;
use vike_tradehub_core::{
    AdapterPolicy, ArmingTable, ScriptedBackend, WiredMarket, preflight::PreflightReport,
};

/// The venues the scripted table carries `FeatureAbsent`: ibkr, fxcm and polymarket, the venues the DEFAULT
/// gatewayhub build answers `FeatureAbsent` for. The tradehub holds no cargo feature for them any more
/// (W15), so the fixture states the build whose answers these tests were written against.
pub fn absent_venues() -> Vec<&'static str> {
    vec!["ibkr", "fxcm", "polymarket"]
}

/// A rig over the policy a test states (`None` reads the default policy: no account table, so no
/// account runs). `vars` is kept for call-site parity with the real backend: the scripted table
/// reads the `account` rows alone.
pub fn rig(
    _vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Rig<ScriptedBackend> {
    Rig::over(ScriptedBackend::credentialed(&policy.cloned().unwrap_or_default(), &absent_venues()))
}

/// The per-ACCOUNT rows of `venue` as the fold reads them — the old
/// `venue_account_arming(registry, venue, vars, policy)`.
pub fn rows(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<VenueArming> {
    rig(vars, policy).rows(venue, policy)
}

/// Every roster venue's account rows — the old `venue_arming(registry, vars, policy)`.
pub fn all_rows(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> Vec<VenueArming> {
    rig(vars, Some(policy)).all_rows(policy)
}

/// The pre-assembly armed set — the old `armed_live_venues(registry, vars, policy)`.
pub fn armed_live_venues(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> Vec<String> {
    rig(vars, Some(policy)).armed_live_venues(policy)
}

/// The markets a build mounts — the old `wired_markets(registry)`.
pub fn wired_markets() -> Vec<WiredMarket> {
    rig(&HashMap::new(), None).table().wired_markets()
}

/// The shared-book report for already-resolved rows — the old
/// `shared_books_for(registry, venue, rows, policy)`.
pub fn shared_books_for(
    rows: &[VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<vike_config::SharedBook> {
    rig(vars, policy).shared_books_for(rows)
}

/// The unaddressable-account report — the old
/// `unaddressable_accounts_message(registry, vars, policy)`.
pub fn unaddressable_accounts_message(
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Option<String> {
    rig(vars, policy).unaddressable_accounts_message(policy)
}

/// Whether any roster venue would go live — the old
/// `any_venue_would_go_live(registry, vars, policy)`.
pub fn any_venue_would_go_live(
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> bool {
    rig(vars, policy).any_venue_would_go_live(policy)
}

/// The startup preflight over the scripted backend (which holds no venue client, so only the disk leg
/// and the skip flag are real) — the old `run_startup_preflight(registry, vars, dirs, policy)`.
pub fn run_startup_preflight(
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&AdapterPolicy>,
) -> PreflightReport {
    rig(vars, policy).run_startup_preflight(vars, dirs, policy)
}

/// The arming snapshot a scripted backend answers for the given policy — what the feed plan reads
/// (`venue_feed_plan`'s `table`).
pub fn table(vars: &HashMap<String, String>, policy: Option<&AdapterPolicy>) -> ArmingTable {
    rig(vars, policy).into_table()
}
