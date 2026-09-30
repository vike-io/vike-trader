//! A test double for bridge tests of the venue mount contract: owned inputs a test borrows
//! `MountInputs` / `MountRequest` from. `test-support`-gated like `scripted`, so no shipped build
//! compiles it.

use std::collections::HashMap;

use vike_exec::EventSender;
use vike_model::HaltAdmit;
use vike_model::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use crate::account_directory::AccountDirectory;
use crate::venue_mount::{MountInputs, MountRequest, ProcessFacts};

/// Owned inputs. Fields are public: a test sets `account` for a labelled account or `accounts` for
/// a read store.
pub struct MountFixture {
    pub vars: HashMap<String, String>,
    pub account: AccountLabel,
    pub settings: VenueSettings,
    pub accounts: AccountDirectory,
    pub process: ProcessFacts,
}

impl MountFixture {
    /// A fixture over `pairs`, for the default account, with an unread account table.
    #[must_use]
    pub fn new(pairs: &[(&str, &str)]) -> Self {
        MountFixture {
            vars: pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            account: AccountLabel::Default,
            settings: VenueSettings::default(),
            accounts: AccountDirectory::unread(),
            process: ProcessFacts::default(),
        }
    }

    #[must_use]
    pub fn inputs(&self, live_permitted: bool) -> MountInputs<'_> {
        MountInputs {
            account: &self.account,
            secrets: &self.vars,
            settings: &self.settings,
            live_permitted,
            accounts: &self.accounts,
            process: &self.process,
        }
    }

    /// A request with reconciliation off, no trigger, no recorder, no profile and the default
    /// halt-admit mode — the offline shape every paper-agreement test uses.
    #[must_use]
    pub fn request<'a>(
        &'a self,
        live_permitted: bool,
        symbol: &'a str,
        events: &'a EventSender,
    ) -> MountRequest<'a> {
        MountRequest {
            inputs: self.inputs(live_permitted),
            symbol,
            declared_legs: &[],
            events,
            recon_enabled: false,
            recon_trigger: None,
            properties_rec: None,
            risk_profile: None,
            market_slippage: None,
            halt_admit: HaltAdmit::default(),
        }
    }
}
