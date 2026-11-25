//! Alpaca login config from the started ACCOUNT's credential fields.
//!
//! Alpaca auths with an OAuth2 client-credentials pair (client_id + client_secret) exchanged for a
//! short-lived Bearer (see [`crate::transport::sign`]) plus one pinned `account_id`. The three are the
//! account's `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` fields ([`FIELDS`], alpaca's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The tier picks the HOSTS
//! only, out of alpaca's `venue_url` rows ([`crate::transport::urls::urls_for`]: Alpaca says "sandbox", not
//! "demo", for every tier below live). The secret never reaches Debug/Display. Any missing field →
//! `None` (the live gate).

use vike_bridge_core::venue_adapter::ArmingDemoOnly;
use vike_model::AccountTier;
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use crate::transport::urls::{AlpacaUrls, urls_for};

/// The account fields the loader reads, all three REQUIRED, in the schema row's order —
/// `(client_id, client_secret, account_id)`. `the_fields_are_the_schemas` holds them to
/// `vike_venues::credential_schema`'s alpaca row.
pub(crate) const FIELDS: [&str; 3] = ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"];

/// Alpaca session parameters: the OAuth pair + pinned account + resolved hosts. Both halves of the
/// pair are [`Secret`]s (the client id is credential-adjacent), so the derived `Debug` prints neither.
#[derive(Clone, Debug)]
pub struct AlpacaConfig {
    pub client_id: Secret<String>,
    pub client_secret: Secret<String>,
    pub account_id: String,
    /// The tier whose hosts these are.
    pub env: AccountTier,
    pub hosts: AlpacaUrls,
}

/// Read Alpaca config from ONE account's credentials, on `env`'s hosts out of alpaca's address rows
/// `urls` ([`urls_for`]). `None` when any of `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` is absent
/// or blank — the live gate; `Some(Err)` when the trio is whole and a host row is refused (the
/// account does not run, with the row's words).
///
/// ⚠ **The account is the credentials**: every Alpaca request is scoped by the pinned
/// `ACCOUNT_ID`, and `creds` holds only the started account's fields, so there is nothing to
/// borrow from another account.
pub fn load_alpaca_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> Option<Result<AlpacaConfig, UrlRefusal>> {
    let [client_id, client_secret, account_id] = FIELDS.map(|field| creds.get(field));
    let (client_id, client_secret, account_id) = (client_id?, client_secret?, account_id?);
    Some(urls_for(env, urls).map(|hosts| AlpacaConfig {
        client_id: Secret::new(client_id.to_string()),
        client_secret: Secret::new(client_secret.to_string()),
        account_id: account_id.to_string(),
        env,
        hosts,
    }))
}

/// **What this demo-only arm does with ONE account** for an account at `tier` holding `creds` — the
/// one answer `crate::adapter`'s `resolve`, `start`, `report_not_running_account` and
/// `credential_probe` all act on, so they cannot disagree. The arm starts only the sandbox (its
/// DEMO config, on the sandbox rows of `urls`), so a complete set on a `live` account is one it will
/// not use; a refused sandbox row is the `Demo` arm's `Err`, which `start` refuses with.
pub(crate) fn sandbox_arm(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> ArmingDemoOnly<Result<AlpacaConfig, UrlRefusal>> {
    ArmingDemoOnly::of(tier, load_alpaca_config_from(AccountTier::Demo, creds, urls))
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter_fixture::assert_demo_only_arm_follows_the_tier;
    use vike_secrets::credentials::assert_account_fields_are_the_schemas;

    use super::*;
    use crate::feed::VENUE;

    /// **The fields this file reads are `vike_venues::credential_schema`'s**: alpaca's row holds
    /// exactly [`FIELDS`] for an account, every one required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        assert_account_fields_are_the_schemas(VENUE, &FIELDS);
    }

    /// The arm's one decision: a complete trio arms the sandbox below `live`, is refused at
    /// `live`, and anything less is unconfigured at every tier.
    #[test]
    fn the_sandbox_arm_follows_the_accounts_one_tier() {
        let seed = VenueUrls::seed(VENUE);
        assert_demo_only_arm_follows_the_tier(FIELDS, |tier, creds| {
            sandbox_arm(tier, creds, &seed)
        });
    }
}
