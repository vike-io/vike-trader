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
//! test reads as it always did.
//!
//! `#[path]`-included (a bare `mod` would resolve beside the binary root, not here), by the integration
//! binaries AND by the crate's own unit tests, so it names nothing but `vike_tradehub_core`.
//!
//! The tests that need a real bridge's rule (aster's testnet fallback, ibkr's held orders, dukascopy's one
//! sidecar, the polymarket exec flag) are not here: they are the gatewayhub's.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use vike_config::VenueArming;
use vike_tradehub_core::{
    AdapterEnv, AdapterPolicy, ArmingTable, ScriptedBackend, VenueBackend, WiredMarket,
    preflight::PreflightReport,
};

/// The venues the scripted table carries `FeatureAbsent`: ibkr, fxcm and polymarket, the venues the DEFAULT
/// gatewayhub build answers `FeatureAbsent` for. The tradehub holds no cargo feature for them any more
/// (W15), so the fixture states the build whose answers these tests were written against.
pub(crate) fn absent_venues() -> Vec<&'static str> {
    vec!["ibkr", "fxcm", "polymarket"]
}

/// A backend and the arming table it answered, built from the same inputs.
pub(crate) struct Rig {
    backend: Arc<ScriptedBackend>,
    table: ArmingTable,
}

impl Rig {
    /// A rig over the policy a test states (`None` reads the default policy: no account table, so no
    /// account runs). `vars` is kept for call-site parity with the real backend: the scripted table
    /// reads the `account` rows alone.
    pub(crate) fn new(_vars: &HashMap<String, String>, policy: Option<&AdapterPolicy>) -> Self {
        let backend = Arc::new(ScriptedBackend::credentialed(
            &policy.cloned().unwrap_or_default(),
            &absent_venues(),
        ));
        let table = backend.arming_table().expect("the scripted backend always answers");
        Rig { backend, table }
    }

    /// The env a start runs in: this rig's backend and table, nothing else set. A test that states a
    /// policy sets `env.policy`, as it always did.
    pub(crate) fn env<'a>(
        &'a self,
        live_events: &'a vike_exec::EventSender,
        live_venues: &'a mut HashSet<String>,
    ) -> AdapterEnv<'a> {
        AdapterEnv::new(&*self.backend, &self.table, live_events, live_venues)
    }

    /// The arming snapshot the rig's backend answered.
    pub(crate) fn table(&self) -> &ArmingTable {
        &self.table
    }

    /// The backend as the runtime assembly holds it.
    pub(crate) fn backend(&self) -> Arc<dyn VenueBackend> {
        self.backend.clone()
    }
}

/// The per-ACCOUNT rows of `venue` as the fold reads them — the old
/// `venue_account_arming(registry, venue, vars, policy)`.
pub(crate) fn rows(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<VenueArming> {
    let rig = Rig::new(vars, policy);
    vike_tradehub_core::venue_account_arming(rig.table(), venue, policy)
}

/// Every roster venue's account rows — the old `venue_arming(registry, vars, policy)`.
pub(crate) fn all_rows(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> Vec<VenueArming> {
    let rig = Rig::new(vars, Some(policy));
    vike_tradehub_core::venue_arming(rig.table(), policy)
}

/// The pre-assembly armed set — the old `armed_live_venues(registry, vars, policy)`.
pub(crate) fn armed_live_venues(
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Vec<String> {
    let rig = Rig::new(vars, Some(policy));
    vike_tradehub_core::armed_live_venues(rig.table(), policy)
}

/// The markets a build mounts — the old `wired_markets(registry)`.
pub(crate) fn wired_markets() -> Vec<WiredMarket> {
    Rig::new(&HashMap::new(), None).table().wired_markets()
}

/// The shared-book report for already-resolved rows — the old
/// `shared_books_for(registry, venue, rows, policy)`.
pub(crate) fn shared_books_for(
    rows: &[VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<vike_config::SharedBook> {
    let rig = Rig::new(vars, policy);
    vike_tradehub_core::shared_books_for(rig.table(), rows)
}

/// The unaddressable-account report — the old
/// `unaddressable_accounts_message(registry, vars, policy)`.
pub(crate) fn unaddressable_accounts_message(
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Option<String> {
    let rig = Rig::new(vars, policy);
    vike_tradehub_core::unaddressable_accounts_message(rig.table(), policy)
}

/// Whether any roster venue would go live — the old
/// `any_venue_would_go_live(registry, vars, policy)`.
pub(crate) fn any_venue_would_go_live(
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> bool {
    let rig = Rig::new(vars, policy);
    vike_tradehub_core::startup::any_venue_would_go_live(rig.table(), policy)
}

/// The startup preflight over the scripted backend (which holds no venue client, so only the disk leg
/// and the skip flag are real) — the old `run_startup_preflight(registry, vars, dirs, policy)`.
pub(crate) fn run_startup_preflight(
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&AdapterPolicy>,
) -> PreflightReport {
    let rig = Rig::new(vars, policy);
    vike_tradehub_core::startup::run_startup_preflight(
        &rig.backend(),
        rig.table(),
        vars,
        dirs,
        policy,
    )
}

/// The arming snapshot a scripted backend answers for the given policy — what the feed plan reads
/// (`venue_feed_plan`'s `table`).
pub(crate) fn table(vars: &HashMap<String, String>, policy: Option<&AdapterPolicy>) -> ArmingTable {
    Rig::new(vars, policy).table
}
