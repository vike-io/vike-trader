//! Alpaca login config from the started ACCOUNT's credential fields.
//!
//! Alpaca auths with an OAuth2 client-credentials pair (client_id + client_secret) exchanged for a
//! short-lived Bearer (see [`crate::auth`]) plus one pinned `account_id`. The three are the
//! account's `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` fields ([`FIELDS`], alpaca's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The tier picks the HOSTS
//! only ([`crate::hosts::hosts_for`]: Alpaca says "sandbox", not "demo", for every tier below
//! live). The secret never reaches Debug/Display. Any missing field → `None` (the live gate).

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

use crate::hosts::{AlpacaEnv, hosts_for};

/// The account fields the loader reads, all three REQUIRED, in the schema row's order —
/// `(client_id, client_secret, account_id)`. `the_fields_are_the_schemas` holds them to
/// `vike_venues::credential_schema`'s alpaca row.
pub(crate) const FIELDS: [&str; 3] = ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"];

/// Alpaca session parameters: the OAuth pair + pinned account + resolved hosts.
#[derive(Clone)]
pub struct AlpacaConfig {
    pub client_id: String,
    pub client_secret: String,
    pub account_id: String,
    /// The tier whose hosts these are.
    pub env: AccountTier,
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

/// Read Alpaca config from ONE account's credentials, on `env`'s hosts. `None` when any of
/// `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` is absent or blank — the live gate.
///
/// ⚠ **The account is the credentials**: every Alpaca request is scoped by the pinned
/// `ACCOUNT_ID`, and `creds` holds only the started account's fields, so there is nothing to
/// borrow from another account.
pub fn load_alpaca_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
) -> Option<AlpacaConfig> {
    let [client_id, client_secret, account_id] = FIELDS.map(|field| creds.get(field));
    Some(AlpacaConfig {
        client_id: client_id?.to_string(),
        client_secret: client_secret?.to_string(),
        account_id: account_id?.to_string(),
        env,
        hosts: hosts_for(env),
    })
}

/// **What this demo-only arm does with ONE account** — the one answer `crate::adapter`'s `resolve`,
/// `start`, `report_not_running_account` and `credential_probe` all act on, so they cannot disagree.
///
/// The account has ONE tier (decision 0156 item 6) and its credentials are that tier's: this arm
/// starts only the sandbox, so a complete set on a `live` account is one it will not use.
pub(crate) enum SandboxArm {
    /// The account's tier is below `live` and it holds the whole trio: the SANDBOX config.
    Sandbox(AlpacaConfig),
    /// The account's tier is `live` and it holds the whole trio. This arm never selects the live
    /// hosts, so the venue does not run and says so
    /// (`vike_bridge_core::venue_adapter::PaperCause::LiveTierNotWired`).
    LiveTierNotWired,
    /// The account holds no complete trio — the ordinary, silent unconfigured state.
    Unconfigured,
}

/// [`SandboxArm`] for an account at `tier` holding `creds`. A `Paper` tier reads as `Demo` (the
/// start contract's rule); only `Live` is refused.
pub(crate) fn sandbox_arm(tier: AccountTier, creds: &AccountCredentials<'_>) -> SandboxArm {
    let Some(cfg) = load_alpaca_config_from(AccountTier::Demo, creds) else {
        return SandboxArm::Unconfigured;
    };
    match tier {
        AccountTier::Paper | AccountTier::Demo => SandboxArm::Sandbox(cfg),
        AccountTier::Live => SandboxArm::LiveTierNotWired,
    }
}

#[cfg(test)]
mod tests {
    use vike_secrets::account_credentials::CredentialFields;
    use vike_venues::credential_schema::{FieldOwner, schema_for};

    use super::*;
    use crate::data::VENUE;

    /// **The fields this file reads are `vike_venues::credential_schema`'s**: alpaca's row holds
    /// exactly [`FIELDS`] for an account, every one required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        let schema = schema_for(VENUE).expect("alpaca is a schema row");
        let account: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(account, FIELDS);
        assert_eq!(schema.required(FieldOwner::Account), FIELDS);
        assert_eq!(schema.venue_fields().count(), 0);
    }

    /// The arm's one decision: a complete trio arms the sandbox below `live`, is refused at
    /// `live`, and anything less is unconfigured at every tier.
    #[test]
    fn the_sandbox_arm_follows_the_accounts_one_tier() {
        let full = CredentialFields::from_pairs(FIELDS.map(|f| (f, "x")));
        let half = CredentialFields::from_pairs(FIELDS[..2].iter().map(|f| (*f, "x")));
        let full = AccountCredentials::new(Some(1), Some(&full), None);
        let half = AccountCredentials::new(Some(1), Some(&half), None);
        for tier in [AccountTier::Paper, AccountTier::Demo] {
            assert!(matches!(sandbox_arm(tier, &full), SandboxArm::Sandbox(_)), "{tier}");
        }
        assert!(matches!(sandbox_arm(AccountTier::Live, &full), SandboxArm::LiveTierNotWired));
        for tier in AccountTier::ALL {
            assert!(matches!(sandbox_arm(tier, &half), SandboxArm::Unconfigured), "{tier}");
            assert!(
                matches!(sandbox_arm(tier, &AccountCredentials::none()), SandboxArm::Unconfigured),
                "{tier}"
            );
        }
    }
}
