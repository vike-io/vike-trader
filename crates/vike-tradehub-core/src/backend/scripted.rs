//! **`VenueBackendScript` — a [`VenueBackend`] that answers what a test scripts and records what it
//! was asked.** The table is the test's own; each account's start answers from a queue of scripted
//! outcomes (a bind, or the block the account stops with); every verb the fold calls is appended to
//! [`VenueBackendScript::calls`] in order, so a test can pin WHEN the fold asked (the identity record
//! after the fee schedule, the budget refusal before the start).
//!
//! Gated `#[cfg(any(test, feature = "test-support"))]` like every double in the workspace: the roster
//! tests that STAY in the tradehub when the venue half moves (W9b) are integration binaries of another
//! crate and enable the feature as a DEV-dependency; a default build never compiles it.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use vike_config::ArmingAccount;
use vike_execution::EventSender;
use vike_execution::recon::ReconcileClient;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, Withhold};

use super::{
    AdapterBind, AdapterStartQuery, ArmingTable, NetReading, PreflightPlan, PreflightQuery,
    VenueAdapterIdentityRecord, VenueBackend, VenueBackendError,
};
use vike_model::{CredentialGap, ServerTimeGap};

/// One verb the fold called on a [`VenueBackendScript`], with what mattered about it.
#[derive(Debug, Clone, PartialEq)]
pub enum VenueBackendScriptCall {
    /// [`VenueBackend::start_adapter`].
    Start {
        /// The venue.
        venue: String,
        /// The account.
        account: AccountLabel,
        /// The tier it was started at.
        tier: AccountTier,
        /// What the fold asked to withhold.
        withhold: Option<Withhold>,
        /// Whether the start asked for a reconcile client.
        reconcile_enabled: bool,
        /// The whole-number leverage the start asked the venue to hold (decision 0182).
        account_leverage: Option<f64>,
    },
    /// [`VenueBackend::record_identity`].
    RecordIdentity {
        /// The venue.
        venue: String,
        /// The account.
        account: AccountLabel,
        /// The tier the mount bound.
        bound_tier: AccountTier,
        /// Whether a reconcile client came with it.
        reconcile_present: bool,
    },
    /// [`VenueBackend::release_refused_start`].
    ReleaseRefusedStart {
        /// The venue.
        venue: String,
        /// The account.
        account: AccountLabel,
        /// The route key the refused start ran under.
        route_key: String,
    },
    /// [`VenueBackend::report_not_running`].
    ReportNotRunning {
        /// The venue.
        venue: String,
        /// The labels of the rows it was handed.
        labels: Vec<AccountLabel>,
    },
}

/// See the module doc.
pub struct VenueBackendScript {
    table: ArmingTable,
    starts: Mutex<HashMap<String, VecDeque<Result<AdapterBind, VenueBackendError>>>>,
    calls: Mutex<Vec<VenueBackendScriptCall>>,
}

impl VenueBackendScript {
    /// A backend whose [`VenueBackend::arming_table`] answers a clone of `table`.
    #[must_use]
    pub fn new(table: ArmingTable) -> Self {
        VenueBackendScript {
            table,
            starts: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// **A backend over the GENERIC credentialed-venue table of `policy`'s `account` rows** (wave 2, W9b):
    /// what the roster tests that STAY in the tradehub need of a venue half now that the real one is the
    /// gatewayhub's. Every roster venue is present except those in `absent` (answered `FeatureAbsent`
    /// for a `demo`/`live` account); an account the table does not run carries the table's own cause
    /// (`vike_config::account_arming::account_tier`), a `paper` account runs the simulator, and a
    /// `demo`/`live` account arms at exactly its row's tier iff the directory holds ANY credential for it
    /// (`NoCredentials` otherwise). It knows no venue's own rules (aster's testnet fallback, ibkr's held
    /// orders, dukascopy's one sidecar): those are the real bridges' and are tested where they are, in
    /// `crates/vike-gatewayhub/tests/trade_roster/`.
    #[must_use]
    pub fn credentialed(policy: &crate::AdapterPolicy, absent: &[&str]) -> Self {
        use vike_config::account_arming::{account_ids, account_tier, known_accounts};

        let accounts = Some(&policy.accounts);
        let mut facts = std::collections::BTreeMap::new();
        let mut rows = Vec::new();
        for venue in vike_model::VENUES.iter().copied() {
            let present = !absent.contains(&venue);
            facts.insert(
                venue.to_string(),
                vike_model::VenueAdapterDeclarationView {
                    adapter_present: present,
                    addresses_accounts: true,
                    takes_reconcile_trigger: false,
                    grid_source: vike_model::GridSource::NoGrid,
                    process_exclusive: None,
                    clock_declaration_word: vike_model::ClockDeclarationKind::Absent,
                },
            );
            for label in known_accounts(venue, accounts) {
                let (tier, effective, block) = match account_tier(accounts, venue, &label) {
                    Ok(AccountTier::Paper) => (
                        Some(AccountTier::Paper),
                        Some(AccountTier::Paper),
                        vike_model::ArmingBlock::PaperTier,
                    ),
                    Ok(tier) if !present => {
                        (Some(tier), None, vike_model::ArmingBlock::FeatureAbsent)
                    }
                    Ok(tier)
                        if policy.accounts.credentials_for(venue, &label, tier).holds_any() =>
                    {
                        (Some(tier), Some(tier), vike_model::ArmingBlock::None)
                    }
                    // No usable key set: a `live` account never trades demo (the no-downgrade rule),
                    // so what it lacks is its LIVE keys; a `demo` one lacks any.
                    Ok(AccountTier::Live) => (
                        Some(AccountTier::Live),
                        None,
                        vike_model::ArmingBlock::LiveCredentialsAbsent,
                    ),
                    Ok(tier) => (Some(tier), None, vike_model::ArmingBlock::NoCredentials),
                    Err(block) => (None, None, block),
                };
                let ids = account_ids(accounts, venue, &label);
                rows.push(super::ArmingRow {
                    arming: ArmingAccount {
                        venue,
                        label,
                        tier,
                        effective,
                        block,
                        account_ids: ids,
                    },
                    book: None,
                });
            }
        }
        Self::new(super::ArmingTable::new(facts, rows))
    }

    /// Queue the next [`VenueBackend::start_adapter`] outcome of the account whose route key is
    /// `route_key` (`venue` for the default account, `venue#LABEL` otherwise). A start with nothing
    /// queued is `Err(VenueBackendError::Unavailable(..))`, so an unscripted start is loud.
    #[must_use]
    pub fn script_start(
        self,
        route_key: &str,
        outcome: Result<AdapterBind, VenueBackendError>,
    ) -> Self {
        self.starts
            .lock()
            .expect("the script")
            .entry(route_key.to_string())
            .or_default()
            .push_back(outcome);
        self
    }

    /// Every verb called so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<VenueBackendScriptCall> {
        self.calls.lock().expect("the call log").clone()
    }

    fn record(&self, call: VenueBackendScriptCall) {
        self.calls.lock().expect("the call log").push(call);
    }
}

impl std::fmt::Debug for VenueBackendScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenueBackendScript").finish_non_exhaustive()
    }
}

impl VenueBackend for VenueBackendScript {
    fn arming_table(&self) -> Result<ArmingTable, VenueBackendError> {
        Ok(self.table.clone())
    }

    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, VenueBackendError> {
        Ok(PreflightPlan::default())
    }

    fn server_time_ms(&self, _venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        Err(ServerTimeGap::Unreachable("the scripted backend reads no venue clock".to_string()))
    }

    fn authed_read(&self, _venue: &str) -> Result<(), CredentialGap> {
        Err(CredentialGap::Rejected("the scripted backend holds no venue client".to_string()))
    }

    fn net_reading(&self) -> NetReading {
        NetReading::Unknown
    }

    fn report_not_running(&self, venue: &str, rows: &[ArmingAccount]) {
        self.record(VenueBackendScriptCall::ReportNotRunning {
            venue: venue.to_string(),
            labels: rows.iter().map(|r| r.label.clone()).collect(),
        });
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        _events: &EventSender,
        _trigger: Option<Sender<String>>,
    ) -> Result<AdapterBind, VenueBackendError> {
        self.record(VenueBackendScriptCall::Start {
            venue: q.venue.to_string(),
            account: q.account.clone(),
            tier: q.tier,
            withhold: q.withhold,
            reconcile_enabled: q.reconcile_enabled,
            account_leverage: q.account_leverage,
        });
        self.starts
            .lock()
            .expect("the script")
            .get_mut(q.route_key)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| {
                Err(VenueBackendError::Unavailable(format!(
                    "no start was scripted for `{}`",
                    q.route_key
                )))
            })
    }

    fn record_identity(
        &self,
        q: &VenueAdapterIdentityRecord<'_>,
        recon: Option<&dyn ReconcileClient>,
    ) -> Result<(), VenueBackendError> {
        self.record(VenueBackendScriptCall::RecordIdentity {
            venue: q.venue.to_string(),
            account: q.account.clone(),
            bound_tier: q.bound_tier,
            reconcile_present: recon.is_some(),
        });
        // The signed read a real backend makes on the bind's reconcile client: a scripted client
        // counts it, which is how a test pins the identity read AFTER the fee-rate read.
        if let Some(recon) = recon {
            let _ = recon.fetch_account_identity();
        }
        Ok(())
    }

    fn release_refused_start(&self, venue: &str, account: &AccountLabel, route_key: &str) {
        self.record(VenueBackendScriptCall::ReleaseRefusedStart {
            venue: venue.to_string(),
            account: account.clone(),
            route_key: route_key.to_string(),
        });
    }
}
