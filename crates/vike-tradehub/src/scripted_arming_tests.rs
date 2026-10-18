//! **Scripted arming for the unit tests that read an arming snapshot** (wave 2, W9b): the registry-free
//! spellings of what `registry::local` was (the venue half moved to the gatewayhub, and these tests
//! judge the tradehub's folds over ACCOUNTS, not a bridge's rules). The table is
//! `vike_tradehub_core::ScriptedBackend::credentialed`'s: an account arms at exactly its row's tier iff it
//! holds a credential. The integration binaries' twin is `tests/support/local_backend.rs`.
#![allow(dead_code)]

use std::collections::HashMap;

use vike_tradehub_core::{AdapterPolicy, ArmingTable, ScriptedBackend, VenueBackend, WiredMarket};

/// The venues the scripted table carries `FeatureAbsent`: ibkr, fxcm and polymarket, the venues the DEFAULT
/// gatewayhub build answers `FeatureAbsent` for. The tradehub holds no cargo feature for them any more
/// (W15), so the fixture states the build whose answers these tests were written against.
pub(crate) fn absent_venues() -> Vec<&'static str> {
    vec!["ibkr", "fxcm", "polymarket"]
}

/// The arming snapshot the feed-plan unit tests hand `venue_feed_plan`: the scripted table of the policy.
pub(crate) fn plan_table(_vars: &HashMap<String, String>, policy: &AdapterPolicy) -> ArmingTable {
    ScriptedBackend::credentialed(policy, &absent_venues())
        .arming_table()
        .expect("the scripted backend always answers")
}

/// The markets a build mounts.
pub(crate) fn wired_markets() -> Vec<WiredMarket> {
    plan_table(&HashMap::new(), &AdapterPolicy::default()).wired_markets()
}

/// The markets the SHIPPED build mounts: the scripted table with NO venue absent, as the gatewayhub of
/// `vike-backend/full` answers (it carries `polymarket`, `ibkr` and `fxcm`). The tradehub's feed arms are the
/// same in every build since W15 (its own `polymarket` feature is gone), so the test that holds every feed arm
/// to an engine needs the table that has one for each, which the default build's `wired_markets` does not.
pub(crate) fn shipped_wired_markets() -> Vec<WiredMarket> {
    ScriptedBackend::credentialed(&AdapterPolicy::default(), &[])
        .arming_table()
        .expect("the scripted backend always answers")
        .wired_markets()
}

/// The pre-assembly armed set.
pub(crate) fn armed_live_venues(
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Vec<String> {
    vike_tradehub_core::armed_live_venues(&plan_table(vars, policy), policy)
}
