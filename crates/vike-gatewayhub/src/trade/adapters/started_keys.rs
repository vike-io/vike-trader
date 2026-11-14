//! **Which key set each running adapter started with** - the gatewayhub's half of "the key in the database
//! is no longer the key this account trades with" (owner ruling 2026-10-17: a credential changed under a
//! RUNNING account is a warning on that account, never an automatic restart; the operator restarts in his
//! own batch).
//!
//! A start records a one-way fingerprint of the credentials the adapter was built from
//! ([`vike_secrets::account_credentials::CredentialFingerprint`]); the arming table, which re-reads the store
//! on every call (`super::refreshed`), asks [`StartedKeys::changed`] which of the recorded accounts the store
//! now disagrees on and serves their route keys as words (`ArmingTable::key_changed`). Neither a value nor a
//! fingerprint leaves this module. The map is shared by every generation of the lane
//! (`TradeLaneBridge::successor`) and is in memory only: a daemon restart drops every adapter with it.
//!
//! An account whose tier the rows no longer state as the recorded one is NOT reported here: that is the
//! tradehub watcher's stop or restart (decision 0169), and its fresh start records a fresh fingerprint.
//!
//! **The same reads also count what a start reads** ([`StartedKeys::start_inputs`]): the tradehub holds no value
//! and a key replaced in place keeps its row, its id and its name, so a NOT RUNNING account whose key an operator
//! corrected can only be told apart from a commit that changed nothing by the process that holds the values.
//! What crosses is a counter: it moves, it says nothing about what it counted.

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, RandomState};
use std::sync::{Mutex, PoisonError};

use vike_gatewayhub_client::proto::ArmingRowWire;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_secrets::account_credentials::CredentialFingerprint;
use vike_secrets::account_directory::AccountDirectory;

use super::snapshot::TradeLaneSnapshot;

/// The tier an adapter started at and the fingerprint of the key set it started from.
type StartedEntry = (AccountTier, Option<CredentialFingerprint>);

/// What the store last said one account's start would read, and the counter that moves with it.
#[derive(Debug)]
struct SeenInputs {
    keys: Option<CredentialFingerprint>,
    /// The venue's settings, the deployment rows and the flags, hashed under [`StartedKeys::hasher`].
    rest: u64,
    revision: u64,
}

/// See the module doc.
#[derive(Debug, Default)]
pub(crate) struct StartedKeys {
    started: Mutex<BTreeMap<(String, AccountLabel), StartedEntry>>,
    seen: Mutex<BTreeMap<(String, AccountLabel), SeenInputs>>,
    /// Drawn once per process: it keys [`SeenInputs::rest`], and its hash of nothing is where every counter
    /// starts, so a restart of this daemon reads as moved (nobody counted what changed while it was down).
    hasher: RandomState,
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

    /// **Route key -> how far what a start of that account reads has moved** (`ArmingTable::start_inputs`), one
    /// entry per row of the table that states a tier. Asking twice over one store answers the same, whoever asks,
    /// so no caller can eat another's news.
    pub(crate) fn start_inputs(
        &self,
        rows: &[ArmingRowWire],
        snapshot: &TradeLaneSnapshot,
        vars: &HashMap<String, String>,
    ) -> BTreeMap<String, u64> {
        let deployment: BTreeMap<&String, &String> = vars.iter().collect();
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counted = BTreeMap::new();
        for row in rows {
            let Some(tier) = row.tier else { continue };
            let label = row.label.as_deref().map_or(Ok(AccountLabel::Default), AccountLabel::parse);
            let Ok(label) = label else { continue };
            let keys = snapshot.accounts.credentials_for(&row.venue, &label, tier).fingerprint();
            let rest = self.hasher.hash_one((
                &deployment,
                snapshot.venue_settings.get(&row.venue),
                &snapshot.venue_settings_unread,
            ));
            let route_key = route_key_of(&row.venue, &label);
            let entry = seen.entry((row.venue.clone(), label)).or_insert_with(|| SeenInputs {
                keys,
                rest,
                revision: self.hasher.hash_one(()),
            });
            if entry.keys != keys || entry.rest != rest {
                entry.keys = keys;
                entry.rest = rest;
                entry.revision = entry.revision.wrapping_add(1);
            }
            counted.insert(route_key, entry.revision);
        }
        counted
    }
}

#[path = "started_keys_tests.rs"]
#[cfg(test)]
mod started_keys_tests;
