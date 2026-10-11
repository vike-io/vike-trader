//! Alpaca login config from the gitignored workspace `.env`.
//!
//! Alpaca auths with an OAuth2 client-credentials pair (client_id + client_secret) exchanged for a
//! short-lived Bearer (see [`crate::auth`]) plus one pinned `account_id`. Alpaca says "sandbox",
//! not "demo", so [`alpaca_tier`] maps `Sim`/`Demo` → `SANDBOX`. The secret never reaches
//! Debug/Display. Any missing field → `None` (the live gate).

use std::collections::HashMap;
use vike_bridge_core::credentials::{Environment, TierKeys, account_var};
use vike_model::accounts::account_keys::{AccountLabel, account_key};

use crate::hosts::{AlpacaEnv, hosts_for};

/// The `.env` tier string for Alpaca (`SANDBOX` for sim/demo, `LIVE` for live).
pub fn alpaca_tier(env: Environment) -> &'static str {
    match env {
        Environment::Live => "LIVE",
        _ => "SANDBOX",
    }
}

/// Alpaca session parameters: the OAuth pair + pinned account + resolved hosts.
#[derive(Clone)]
pub struct AlpacaConfig {
    pub client_id: String,
    pub client_secret: String,
    pub account_id: String,
    pub env: Environment,
    pub hosts: AlpacaEnv,
}

impl std::fmt::Debug for AlpacaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the secret (or the client_id, which is credential-adjacent)
        write!(
            f,
            "AlpacaConfig(account_id={}, env={:?}, broker={})",
            self.account_id, self.env, self.hosts.broker
        )
    }
}

/// Read Alpaca config from a var map (process env or a parsed `.env`). `None` when any of
/// client_id / client_secret / account_id is unset or blank — the live gate.
pub fn load_alpaca_config_from(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<AlpacaConfig> {
    load_alpaca_config_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_alpaca_config_from`] for ONE NAMED ACCOUNT —
/// `ALPACA_{SANDBOX|LIVE}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_alpaca_config_from`] documents holds word for word; the ONLY difference is the
/// NAMES read, composed by `vike_bridge_core::credentials::account_var`, which appends the label
/// after the WHOLE of today's key. `ALPACA_SANDBOX_CLIENT_SECRET` — a two-word bespoke suffix under
/// a tier token that is not even a `vike_model::credential_keys` tier — therefore needs no entry in
/// any table.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_alpaca_config_from`]**, reached through
/// it.
///
/// ⚠ **No fallback to the unlabelled key**, and here the `_ACCOUNT_ID` half is the reason it
/// matters most: every Alpaca request is scoped by that pinned id, so borrowing the default
/// account's would place account `ALT`'s orders in the FIRST account's book even with a distinct
/// OAuth pair.
pub fn load_alpaca_config_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<AlpacaConfig> {
    let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();
    let [client_id_k, client_secret_k, account_id_k] = tier_var_names(env);
    let client_id = get(&client_id_k);
    let client_secret = get(&client_secret_k);
    let account_id = get(&account_id_k);
    if client_id.is_empty() || client_secret.is_empty() || account_id.is_empty() {
        return None;
    }
    Some(AlpacaConfig { client_id, client_secret, account_id, env, hosts: hosts_for(env) })
}

/// The `(client_id, client_secret, account_id)` variable names for one tier — the single
/// composition site, read by [`load_alpaca_config_for_account`] and by [`tier_keys`], so a rename
/// cannot leave the loader reading one spelling and a report naming another.
fn tier_var_names(env: Environment) -> [String; 3] {
    let prefix = format!("ALPACA_{}", alpaca_tier(env));
    ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"].map(|k| format!("{prefix}_{k}"))
}

/// The names ONE account's key set at `env` is written under, for a report of which are missing
/// from a half-written set ([`vike_bridge_core::credentials::TierKeys`]). All three are required
/// and all three belong to the tier. Label-composed, so the names are the ones to write for THIS
/// account.
#[must_use]
pub(crate) fn tier_keys(env: Environment, label: &AccountLabel) -> Vec<TierKeys> {
    let names = tier_var_names(env).map(|n| account_key(&n, label));
    vec![TierKeys { required: names.to_vec(), tier_named: names.to_vec() }]
}
