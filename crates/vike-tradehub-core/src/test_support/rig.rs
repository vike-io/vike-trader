//! **A backend and the arming table it answered, built from the same inputs**: the rig every fold-driving
//! suite starts the fold through, over whichever [`VenueBackend`] the suite plants (the tradehub's roster
//! tests: [`crate::ScriptedBackend::credentialed`]; the gatewayhub's unit and roster tests: its
//! `BridgeBackend` over a registry), and the questions those suites ask of ONE assembly's table, spelled
//! once, with [`not_running`], the read of a start the fold refused. Each suite keeps its own constructor
//! (what it builds the backend from) and its registry-taking spellings over these methods, so a test reads
//! as it always did.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use vike_config::{SharedBook, VenueArming};

use crate::preflight::PreflightReport;
use crate::{
    AdapterEnv, AdapterPolicy, ArmingTable, EngineAndRecon, EngineError, NotRunning, VenueBackend,
};

/// The account a start reported NOT RUNNING for `venue` (decision 0156: an account runs at its row's tier
/// or not at all), or a panic naming the venue and what came back instead — an engine, or another refusal.
pub fn not_running(venue: &str, out: Result<EngineAndRecon, EngineError>) -> NotRunning {
    match out {
        Err(EngineError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("{venue}: expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => {
            panic!("{venue}: expected NOT RUNNING, got an engine at {}", engine.tier)
        }
    }
}

/// A backend and the arming table it answered (see the module doc).
pub struct Rig<B> {
    backend: Arc<B>,
    table: ArmingTable,
}

impl<B: VenueBackend + 'static> Rig<B> {
    /// A rig over `backend`, holding the table it answers now.
    pub fn over(backend: B) -> Self {
        let table = backend.arming_table().expect("a test backend always answers");
        Rig { backend: Arc::new(backend), table }
    }

    /// The env a start runs in: this rig's backend and table, nothing else set. A test that states a
    /// policy sets `env.policy`, as it always did.
    pub fn env<'a>(
        &'a self,
        live_events: &'a vike_exec::EventSender,
        live_venues: &'a mut HashSet<String>,
    ) -> AdapterEnv<'a> {
        AdapterEnv::new(&*self.backend, &self.table, live_events, live_venues)
    }

    /// The arming snapshot the rig's backend answered.
    pub fn table(&self) -> &ArmingTable {
        &self.table
    }

    /// The arming snapshot, kept past the rig.
    pub fn into_table(self) -> ArmingTable {
        self.table
    }

    /// The backend as the runtime assembly holds it.
    pub fn backend(&self) -> Arc<dyn VenueBackend> {
        self.backend.clone()
    }

    /// The per-ACCOUNT rows of `venue` as the fold reads them, with the startup preflight's stop laid over.
    pub fn rows(&self, venue: &str, policy: Option<&AdapterPolicy>) -> Vec<VenueArming> {
        crate::venue_account_arming(&self.table, venue, policy)
    }

    /// Every roster venue's account rows.
    pub fn all_rows(&self, policy: &AdapterPolicy) -> Vec<VenueArming> {
        crate::venue_arming(&self.table, policy)
    }

    /// The pre-assembly armed set.
    pub fn armed_live_venues(&self, policy: &AdapterPolicy) -> Vec<String> {
        crate::armed_live_venues(&self.table, policy)
    }

    /// The shared-book report for already-resolved rows.
    pub fn shared_books_for(&self, rows: &[VenueArming]) -> Vec<SharedBook> {
        crate::shared_books_for(&self.table, rows)
    }

    /// The unaddressable-account report.
    pub fn unaddressable_accounts_message(&self, policy: Option<&AdapterPolicy>) -> Option<String> {
        crate::unaddressable_accounts_message(&self.table, policy)
    }

    /// Whether any roster venue would go live.
    pub fn any_venue_would_go_live(&self, policy: Option<&AdapterPolicy>) -> bool {
        crate::startup::any_venue_would_go_live(&self.table, policy)
    }

    /// The startup preflight over this rig's backend and table.
    pub fn run_startup_preflight(
        &self,
        vars: &HashMap<String, String>,
        dirs: &[(String, PathBuf)],
        policy: Option<&AdapterPolicy>,
    ) -> PreflightReport {
        crate::startup::run_startup_preflight(&self.backend(), &self.table, vars, dirs, policy)
    }
}
