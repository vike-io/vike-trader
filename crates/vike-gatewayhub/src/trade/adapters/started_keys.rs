//! **Which key set each running adapter started with** - the gatewayhub's half of "the key in the database
//! is no longer the key this account trades with" (owner ruling 2026-10-17: a credential changed under a
//! RUNNING account is a warning on that account, never an automatic restart, because a restart cancels its
//! resting orders).
//!
//! A start records a one-way fingerprint of the credentials the adapter was built from
//! ([`vike_secrets::account_credentials::CredentialFingerprint`]); the arming table, which re-reads the store
//! on every call (`super::refreshed`), asks [`StartedKeys::changed`] which of the recorded accounts the store
//! now disagrees on and serves their route keys as words (`ArmingTable::key_changed`). Neither a value nor a
//! fingerprint leaves this module. The map is shared by every generation of the lane
//! (`BridgeTrade::successor`) and is in memory only: a daemon restart drops every adapter with it.
//!
//! An account whose tier the rows no longer state as the recorded one is NOT reported here: that is the
//! tradehub watcher's stop or restart (decision 0169), and its fresh start records a fresh fingerprint.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_secrets::account_credentials::CredentialFingerprint;
use vike_secrets::account_directory::AccountDirectory;

/// The tier an adapter started at and the fingerprint of the key set it started from.
type StartedEntry = (AccountTier, Option<CredentialFingerprint>);

/// See the module doc.
#[derive(Debug, Default)]
pub(crate) struct StartedKeys {
    started: Mutex<BTreeMap<(String, AccountLabel), StartedEntry>>,
}

impl StartedKeys {
    /// Remember the key set `accounts` holds for `(venue, label)` at `tier`: what the adapter just started
    /// from. A later start of the same account replaces it, which is how a restart clears the warning.
    pub(crate) fn record(
        &self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
        tier: AccountTier,
    ) {
        let fingerprint = accounts.credentials_for(venue, label, tier).fingerprint();
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((venue.to_string(), label.clone()), (tier, fingerprint));
    }

    /// The route keys (sorted) of the recorded accounts that still state their recorded tier in `accounts`
    /// but hold another key set now (a value changed, a field added or removed).
    pub(crate) fn changed(&self, accounts: &AccountDirectory) -> Vec<String> {
        let started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        let mut keys: Vec<String> = started
            .iter()
            .filter(|((venue, label), (tier, _))| {
                vike_config::account_arming::account_tier(Some(accounts), venue, label) == Ok(*tier)
            })
            .filter(|((venue, label), (tier, then))| {
                accounts.credentials_for(venue, label, *tier).fingerprint() != *then
            })
            .map(|((venue, label), _)| route_key_of(venue, label))
            .collect();
        keys.sort();
        keys
    }
}

#[path = "started_keys_tests.rs"]
#[cfg(test)]
mod started_keys_tests;
