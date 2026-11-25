//! **`VenueBackendPaper` — the backend of a box that dials nothing** (`flags.tradehub_live` off, or a
//! paper-only assembly): paper tiers run with no venue connection at all (drift register R21).
//!
//! It answers from the `account` table alone, the way `crate::stopped_by_the_live_switch` does: a
//! `paper` account runs the simulator, a `demo`/`live` account is NOT RUNNING with
//! [`ArmingBlock::LiveSwitchOff`] (the switch is a STOP, never a demotion, decision 0156 item 4), and
//! an account the table itself does not run carries the table's own cause. It has no registry, so
//! every roster venue's adapter is "present" (the engine rows of `VENUE_WIRING` are the wired markets;
//! a build without the `ibkr`/`fxcm`/`polymarket` feature used to drop those from the PAPER venue
//! list, and release builds carry all three) and it reads no credential, spawns no probe and starts
//! no client.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;

use vike_config::ArmingAccount;
use vike_config::account_arming::{account_ids, account_tier, known_accounts};
use vike_execution::EventSender;
use vike_execution::recon::ReconcileClient;
use vike_model::{AccountTier, ArmingBlock, GridSource, VenueAdapterDeclarationView};
use vike_secrets::account_directory::AccountDirectory;

use super::{
    AdapterBind, AdapterStartQuery, ArmingRow, ArmingTable, NetReading, PreflightPlan,
    PreflightQuery, VenueAdapterIdentityRecord, VenueBackend, VenueBackendError,
};
use vike_model::{CredentialGap, ServerTimeGap};

/// See the module doc.
#[derive(Debug, Clone)]
pub struct VenueBackendPaper {
    accounts: AccountDirectory,
}

impl VenueBackendPaper {
    /// A paper backend over the `account` table the composition root read.
    #[must_use]
    pub fn new(accounts: &AccountDirectory) -> Self {
        VenueBackendPaper { accounts: accounts.clone() }
    }
}

impl VenueBackend for VenueBackendPaper {
    fn arming_table(&self) -> Result<ArmingTable, VenueBackendError> {
        let accounts = Some(&self.accounts);
        let mut facts = BTreeMap::new();
        let mut rows = Vec::new();
        for venue in vike_model::VENUES.iter().copied() {
            facts.insert(
                venue.to_string(),
                VenueAdapterDeclarationView {
                    adapter_present: true,
                    addresses_accounts: true,
                    takes_reconcile_trigger: false,
                    grid_source: GridSource::NoGrid,
                    process_exclusive: None,
                    clock_declaration_word: vike_model::ClockDeclarationKind::Absent,
                },
            );
            for label in known_accounts(venue, accounts) {
                let (tier, effective, block) = match account_tier(accounts, venue, &label) {
                    Ok(AccountTier::Paper) => {
                        (Some(AccountTier::Paper), Some(AccountTier::Paper), ArmingBlock::PaperTier)
                    }
                    Ok(tier) => (Some(tier), None, ArmingBlock::LiveSwitchOff),
                    Err(block) => (None, None, block),
                };
                let ids = account_ids(accounts, venue, &label);
                rows.push(ArmingRow {
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
        Ok(ArmingTable::new(facts, rows))
    }

    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, VenueBackendError> {
        Ok(PreflightPlan::default())
    }

    fn server_time_ms(&self, _venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        Err(ServerTimeGap::Unreachable("the paper backend reads no venue clock".to_string()))
    }

    fn authed_read(&self, _venue: &str) -> Result<(), CredentialGap> {
        Err(CredentialGap::Rejected("the paper backend holds no venue client".to_string()))
    }

    fn net_reading(&self) -> NetReading {
        NetReading::Unknown
    }

    fn report_not_running(&self, _venue: &str, _rows: &[ArmingAccount]) {}

    fn start_adapter(
        &self,
        _q: AdapterStartQuery<'_>,
        _events: &EventSender,
        _trigger: Option<Sender<String>>,
    ) -> Result<AdapterBind, VenueBackendError> {
        Err(VenueBackendError::NotRunning(ArmingBlock::LiveSwitchOff))
    }

    fn record_identity(
        &self,
        _q: &VenueAdapterIdentityRecord<'_>,
        _reconcile: Option<&dyn ReconcileClient>,
    ) -> Result<(), VenueBackendError> {
        Ok(())
    }
}
