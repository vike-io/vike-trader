//! **`PaperBackend` — the backend of a box that dials nothing** (`flags.tradehub_live` off, or a
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

use vike_config::account_arming::{account_ids, account_tier, known_accounts};
use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;
use vike_secrets::account_directory::AccountDirectory;

use super::{
    AdapterBind, AdapterStartQuery, ArmingRow, ArmingTable, BackendError, IdentityRecord,
    NetReading, PreflightPlan, PreflightQuery, VenueBackend, VenueFacts,
};
use crate::preflight::{CredentialGap, ServerTimeGap};
use crate::symbol_grid::GridSource;

/// See the module doc.
#[derive(Debug, Clone)]
pub struct PaperBackend {
    accounts: AccountDirectory,
}

impl PaperBackend {
    /// A paper backend over the `account` table the composition root read.
    #[must_use]
    pub fn new(accounts: &AccountDirectory) -> Self {
        PaperBackend { accounts: accounts.clone() }
    }
}

impl VenueBackend for PaperBackend {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        let accounts = Some(&self.accounts);
        let mut facts = BTreeMap::new();
        let mut rows = Vec::new();
        for venue in vike_model::VENUES.iter().copied() {
            facts.insert(
                venue.to_string(),
                VenueFacts {
                    adapter_present: true,
                    addresses_accounts: true,
                    takes_recon_trigger: false,
                    grid_source: GridSource::NoGrid,
                    process_exclusive: None,
                    clock_decl_word: "absent",
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
                    arming: VenueArming { venue, label, tier, effective, block, account_ids: ids },
                    book: None,
                });
            }
        }
        Ok(ArmingTable::new(facts, rows))
    }

    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
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

    fn report_not_running(&self, _venue: &str, _rows: &[VenueArming]) {}

    fn start_adapter(
        &self,
        _q: AdapterStartQuery<'_>,
        _events: &EventSender,
        _trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        Err(BackendError::NotRunning(ArmingBlock::LiveSwitchOff))
    }

    fn record_identity(
        &self,
        _q: &IdentityRecord<'_>,
        _recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError> {
        Ok(())
    }
}
