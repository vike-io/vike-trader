//! IG (IG Group) v20-style REST login config from the started ACCOUNT's credential fields.
//!
//! IG auths in two parts: a per-app **API key** (`X-IG-API-KEY`) plus account **identifier +
//! password** exchanged at `POST /session` for short-lived `CST` / `X-SECURITY-TOKEN` headers. The
//! three are the account's `API_KEY` / `IDENTIFIER` / `PASSWORD` fields ([`FIELDS`], IG's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The tier picks the gateway
//! only ([`ig_rest_base`]). None of the three secrets reach Debug/Display. Absent any of them →
//! `None` (the live gate).

use vike_bridge_core::venue_adapter::{AdapterInputs, DemoOnlyArm};
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

/// The account fields the loader reads, all three REQUIRED, in the schema row's order —
/// `(api_key, identifier, password)`. `the_fields_are_the_schemas` holds them to
/// `vike_venues::credential_schema`'s ig row. `pub` since the history lane: the gatewayhub, which
/// owns the credential read for it (decision 0194), reads exactly these three FIELDS of the one
/// demo account and builds an [`IgConfig`] from them.
pub const FIELDS: [&str; 3] = ["API_KEY", "IDENTIFIER", "PASSWORD"];

/// IG REST base for a tier. `Paper`/`Demo` = the demo gateway, `Live` = the live gateway.
pub fn ig_rest_base(env: AccountTier) -> &'static str {
    match env {
        AccountTier::Live => "https://api.ig.com/gateway/deal",
        AccountTier::Paper | AccountTier::Demo => "https://demo-api.ig.com/gateway/deal",
    }
}

/// IG login parameters. `PartialEq` is the question "is this the same login" (a kept session is replaced
/// when a credential rotates); the derive prints nothing, the custom `Debug` below stays the only output.
#[derive(Clone, PartialEq, Eq)]
pub struct IgConfig {
    pub api_key: String,
    pub identifier: String,
    pub password: String,
    pub rest_base: String,
    /// The account row's `venue_account_id`: the IG account id (the BOOK) the operator named for this account,
    /// which every login of this config puts its session on (`crate::account_pick`). `None` = the rule picks
    /// (the default account if it is a USD CFD one, else the one that is). Not a secret.
    pub account_pin: Option<String>,
}

impl std::fmt::Debug for IgConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak api_key / identifier / password
        write!(f, "IgConfig(rest_base={})", self.rest_base)
    }
}

/// Read IG config from ONE account's credentials, on `env`'s gateway. `None` when any of the three
/// fields is absent or blank — the live gate.
///
/// ⚠ **The account is the credentials.** All THREE fields are per-account here — IG's
/// `identifier`/`password` are exchanged at `POST /session` for the tokens every later request
/// carries — and `creds` holds only the started account's, so nothing can log one account into
/// another.
pub fn load_ig_config_from(env: AccountTier, creds: &AccountCredentials<'_>) -> Option<IgConfig> {
    let [api_key, identifier, password] = FIELDS.map(|field| creds.get(field));
    Some(IgConfig {
        api_key: api_key?.to_string(),
        identifier: identifier?.to_string(),
        password: password?.to_string(),
        rest_base: ig_rest_base(env).to_string(),
        account_pin: None,
    })
}

/// [`load_ig_config_from`] for the account a start resolved, WITH its row's `venue_account_id` as the book to
/// trade ([`IgConfig::account_pin`]): `inputs.accounts` is the settings database's `account` table, read once by
/// the composition root. A blank book is no book. The adapter's start and the gatewayhub's facts read both
/// build their config here, so the account whose money the node is told is the account the orders go to.
pub fn load_ig_config_for(inputs: &AdapterInputs<'_>) -> Option<IgConfig> {
    let mut config = load_ig_config_from(AccountTier::Demo, &inputs.credentials)?;
    config.account_pin = pin_of(inputs);
    Some(config)
}

/// The account row's `venue_account_id` for `inputs`' `(label, tier)`, trimmed; `None` when the row has none.
pub(crate) fn pin_of(inputs: &AdapterInputs<'_>) -> Option<String> {
    let row =
        inputs.accounts.account_row(crate::recon_client::VENUE, inputs.account, inputs.tier)?;
    row.venue_account_id.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(str::to_string)
}

/// **What this demo-only arm does with ONE account** at `tier` holding `creds` — the one answer
/// `crate::adapter`'s `resolve`, `start` and `report_not_running_account` all act on, so they cannot
/// disagree. The arm starts only the demo gateway, so a complete trio on a `live` account is one it
/// will not use.
pub(crate) fn demo_arm(tier: AccountTier, creds: &AccountCredentials<'_>) -> DemoOnlyArm<IgConfig> {
    DemoOnlyArm::of(tier, load_ig_config_from(AccountTier::Demo, creds))
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter_fixture::assert_demo_only_arm_follows_the_tier;
    use vike_secrets::account_credentials::CredentialFields;
    use vike_secrets::credentials::assert_account_fields_are_the_schemas;

    use super::*;
    use crate::recon_client::VENUE;

    #[test]
    fn gate_and_no_secret_leak() {
        assert!(load_ig_config_from(AccountTier::Demo, &AccountCredentials::none()).is_none());

        let fields = CredentialFields::from_pairs([
            ("API_KEY", "key-xyz"),
            ("IDENTIFIER", "myuser"),
            ("PASSWORD", "s3cr3t"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let c = load_ig_config_from(AccountTier::Demo, &creds).unwrap();
        assert!(c.rest_base.contains("demo-api.ig.com"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("key-xyz") && !dbg.contains("myuser") && !dbg.contains("s3cr3t"));

        assert!(ig_rest_base(AccountTier::Live).contains("//api.ig.com"));
        assert_eq!(ig_rest_base(AccountTier::Paper), ig_rest_base(AccountTier::Demo));
    }

    /// **The fields this file reads are `vike_venues::credential_schema`'s**: IG's row holds exactly
    /// [`FIELDS`] for an account, every one required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        assert_account_fields_are_the_schemas(VENUE, &FIELDS);
    }

    /// The arm's one decision: a complete trio arms the demo gateway below `live`, is refused at
    /// `live`, and anything less is unconfigured at every tier.
    #[test]
    fn the_demo_arm_follows_the_accounts_one_tier() {
        assert_demo_only_arm_follows_the_tier(FIELDS, demo_arm);
    }
}
