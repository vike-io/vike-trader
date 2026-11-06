//! **The in-process backend, as the roster tests drive it** (wave 2, W9a; moved with the venue half by W9b):
//! the registry-taking entry points these tests called before the venue seam, spelled over a
//! [`BridgeBackend`] (the fold's `VenueBackend` port over the real `BridgeTrade` lane) and the arming
//! table it answers — so a test reads as it always did (`rows(REGISTRY, venue, &vars,
//! Some(&policy))` for the old `venue_account_arming(REGISTRY, venue, &vars, Some(&policy))`) and
//! the seam it exercises is the one production uses: a backend built from the SAME `vars` and
//! policy the env is handed, one table per call.
//!
//! Declared once, by the `trade_roster` group's `main.rs`, beside `bridge_backend.rs`, which it names
//! through `crate::`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::bridge_backend::{self, BridgeBackend};
use vike_config::VenueArming;
use vike_gatewayhub::trade::adapters::registry::VenueRow;
use vike_tradehub_core::test_support::rig::Rig;
use vike_tradehub_core::{AdapterPolicy, ArmingTable, WiredMarket, preflight::PreflightReport};

/// A rig (`vike_tradehub_core::test_support::rig`) over `registry`, the deployment map and the policy a
/// test states (`None` reads the default policy: no account table, so no account runs).
pub(crate) fn rig(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Rig<BridgeBackend> {
    Rig::over(BridgeBackend::new(registry, vars.clone(), &policy.cloned().unwrap_or_default()))
}

/// The per-ACCOUNT rows of `venue` as the fold reads them — the old
/// `venue_account_arming(registry, venue, vars, policy)`.
pub(crate) fn rows(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<VenueArming> {
    rig(registry, vars, policy).rows(venue, policy)
}

/// Every roster venue's account rows — the old `venue_arming(registry, vars, policy)`.
pub(crate) fn all_rows(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Vec<VenueArming> {
    rig(registry, vars, Some(policy)).all_rows(policy)
}

/// The pre-assembly armed set — the old `armed_live_venues(registry, vars, policy)`.
pub(crate) fn armed_live_venues(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Vec<String> {
    rig(registry, vars, Some(policy)).armed_live_venues(policy)
}

/// The markets a registry mounts — the old `wired_markets(registry)`.
pub(crate) fn wired_markets(registry: &'static [VenueRow]) -> Vec<WiredMarket> {
    rig(registry, &HashMap::new(), None).table().wired_markets()
}

/// The shared-book report for already-resolved rows — the old
/// `shared_books_for(registry, venue, rows, policy)`.
pub(crate) fn shared_books_for(
    registry: &'static [VenueRow],
    rows: &[VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<vike_config::SharedBook> {
    rig(registry, vars, policy).shared_books_for(rows)
}

/// The unaddressable-account report — the old
/// `unaddressable_accounts_message(registry, vars, policy)`.
pub(crate) fn unaddressable_accounts_message(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Option<String> {
    rig(registry, vars, policy).unaddressable_accounts_message(policy)
}

/// Whether any roster venue would go live — the old
/// `any_venue_would_go_live(registry, vars, policy)`.
pub(crate) fn any_venue_would_go_live(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> bool {
    rig(registry, vars, policy).any_venue_would_go_live(policy)
}

/// The startup preflight with the real probes — the old
/// `run_startup_preflight(registry, vars, dirs, policy)`.
pub(crate) fn run_startup_preflight(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&AdapterPolicy>,
) -> PreflightReport {
    rig(registry, vars, policy).run_startup_preflight(vars, dirs, policy)
}

/// The arming snapshot a backend over `registry` answers for the given inputs — what the feed plan
/// reads (`venue_feed_plan`'s `table`).
pub(crate) fn table(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> ArmingTable {
    rig(registry, vars, policy).into_table()
}

// ---- the venue half's own policy-taking probes, spelled over the fold's `AdapterPolicy` -----------------
//
// The venue half reads three fields of the policy (`TradeSnapshot`); a roster test plants the whole
// `AdapterPolicy` (the builders live there) and hands the same value to the fold. These wrappers are the
// registry-taking spellings the tests called when the venue half and the policy shared a crate.

/// [`vike_gatewayhub::trade::adapters::arming::would_go_live_under_policy`] over a policy.
pub(crate) fn would_go_live_under_policy(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> bool {
    let snapshot = policy.map(bridge_backend::snapshot_of);
    vike_gatewayhub::trade::adapters::arming::would_go_live_under_policy(
        registry,
        venue,
        vars,
        snapshot.as_ref(),
    )
}

/// [`vike_gatewayhub::trade::adapters::arming::shared_books_under`] over a policy.
pub(crate) fn shared_books_under(
    registry: &'static [VenueRow],
    venue: &str,
    rows: &[VenueArming],
    policy: Option<&AdapterPolicy>,
) -> Vec<vike_config::SharedBook> {
    let snapshot = policy.map(bridge_backend::snapshot_of);
    vike_gatewayhub::trade::adapters::arming::shared_books_under(
        registry,
        venue,
        rows,
        snapshot.as_ref(),
    )
}

/// The one-probe-per-credentialed-venue map, over a policy.
pub(crate) fn authed_read_probes(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> HashMap<String, Arc<dyn Fn() -> Result<(), String> + Send + Sync>> {
    let snapshot = policy.map(bridge_backend::snapshot_of);
    vike_gatewayhub::trade::adapters::startup::authed_read_probes(registry, vars, snapshot.as_ref())
}

/// The venues the clock leg runs for, over a policy.
pub(crate) fn clock_venues(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<String> {
    let snapshot = policy.map(bridge_backend::snapshot_of);
    vike_gatewayhub::trade::adapters::startup::clock_venues(registry, vars, snapshot.as_ref())
}

/// One venue's clock read, bounded, over a policy.
pub(crate) fn bounded_server_time_ms(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &Arc<HashMap<String, String>>,
    tier: vike_model::AccountTier,
    policy: &Arc<Option<AdapterPolicy>>,
) -> Result<i64, vike_gatewayhub::trade::adapters::probe::ServerTimeGap> {
    let snapshot = Arc::new(policy.as_ref().as_ref().map(bridge_backend::snapshot_of));
    vike_gatewayhub::trade::adapters::startup::bounded_server_time_ms(
        registry, venue, vars, tier, &snapshot,
    )
}

/// The thresholds and remedy the tradehub's preflight judges `venue`'s clock reading against: the
/// declared [`ClockRisk`](vike_bridge_core::venue_adapter::ClockRisk) turned into a policy
/// (`bridge_backend`'s mapping, which restates the one the fold has always applied).
pub(crate) fn clock_policy(
    registry: &'static [VenueRow],
    venue: &str,
) -> Option<vike_tradehub_core::preflight::ClockPolicy> {
    vike_gatewayhub::trade::adapters::server_time::clock_risk(registry, venue)
        .map(bridge_backend::clock_policy_of)
}

/// [`clock_policy`] for every listed venue that has one.
pub(crate) fn clock_policies(
    registry: &'static [VenueRow],
    venues: &[String],
) -> HashMap<String, vike_tradehub_core::preflight::ClockPolicy> {
    venues
        .iter()
        .filter_map(|venue| clock_policy(registry, venue).map(|p| (venue.clone(), p)))
        .collect()
}

/// The thresholds and remedy for a declared risk (`bridge_backend`'s mapping), for the one test that asks
/// about the mapping itself.
pub(crate) fn clock_policy_of(
    risk: vike_bridge_core::venue_adapter::ClockRisk,
) -> vike_tradehub_core::preflight::ClockPolicy {
    bridge_backend::clock_policy_of(risk)
}
