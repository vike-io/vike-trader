//! IG (IG Group) v20-style REST login config from the mounted ACCOUNT's credential fields.
//!
//! IG auths in two parts: a per-app **API key** (`X-IG-API-KEY`) plus account **identifier +
//! password** exchanged at `POST /session` for short-lived `CST` / `X-SECURITY-TOKEN` headers. The
//! three are the account's `API_KEY` / `IDENTIFIER` / `PASSWORD` fields ([`FIELDS`], IG's row of
//! `vike_model::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The tier picks the gateway
//! only ([`ig_rest_base`]). None of the three secrets reach Debug/Display. Absent any of them →
//! `None` (the live gate).

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

/// The account fields the loader reads, all three REQUIRED, in the schema row's order —
/// `(api_key, identifier, password)`. `the_fields_are_the_schemas` holds them to
/// `vike_model::credential_schema`'s ig row.
pub(crate) const FIELDS: [&str; 3] = ["API_KEY", "IDENTIFIER", "PASSWORD"];

/// IG REST base for a tier. `Paper`/`Demo` = the demo gateway, `Live` = the live gateway.
pub fn ig_rest_base(env: AccountTier) -> &'static str {
    match env {
        AccountTier::Live => "https://api.ig.com/gateway/deal",
        AccountTier::Paper | AccountTier::Demo => "https://demo-api.ig.com/gateway/deal",
    }
}

/// IG login parameters.
#[derive(Clone)]
pub struct IgConfig {
    pub api_key: String,
    pub identifier: String,
    pub password: String,
    pub rest_base: String,
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
/// carries — and `creds` holds only the mounted account's, so nothing can log one account into
/// another.
pub fn load_ig_config_from(env: AccountTier, creds: &AccountCredentials<'_>) -> Option<IgConfig> {
    let [api_key, identifier, password] = FIELDS.map(|field| creds.get(field));
    Some(IgConfig {
        api_key: api_key?.to_string(),
        identifier: identifier?.to_string(),
        password: password?.to_string(),
        rest_base: ig_rest_base(env).to_string(),
    })
}

/// **What this demo-only arm does with ONE account** — the one answer `crate::mount`'s `resolve`,
/// `mount` and `report_unmounted_account` all act on, so they cannot disagree.
///
/// The account has ONE tier (decision 0156 item 6) and its credentials are that tier's: this arm
/// mounts only the demo gateway, so a complete trio on a `live` account is one it will not use.
pub(crate) enum DemoArm {
    /// The account's tier is below `live` and it holds the whole trio: the DEMO config.
    Demo(IgConfig),
    /// The account's tier is `live` and it holds the whole trio. This arm never selects the live
    /// gateway, so the venue does not run and says so
    /// (`vike_bridge_core::venue_mount::PaperCause::LiveTierNotWired`).
    LiveTierNotWired,
    /// The account holds no complete trio — the ordinary, silent unconfigured state.
    Unconfigured,
}

/// [`DemoArm`] for an account at `tier` holding `creds`. A `Paper` tier reads as `Demo` (the mount
/// contract's rule); only `Live` is refused.
pub(crate) fn demo_arm(tier: AccountTier, creds: &AccountCredentials<'_>) -> DemoArm {
    let Some(cfg) = load_ig_config_from(AccountTier::Demo, creds) else {
        return DemoArm::Unconfigured;
    };
    match tier {
        AccountTier::Paper | AccountTier::Demo => DemoArm::Demo(cfg),
        AccountTier::Live => DemoArm::LiveTierNotWired,
    }
}

#[cfg(test)]
mod tests {
    use vike_model::credential_schema::{FieldOwner, schema_for};
    use vike_secrets::account_credentials::CredentialFields;

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

    /// **The fields this file reads are `vike_model::credential_schema`'s**: IG's row holds exactly
    /// [`FIELDS`] for an account, every one required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        let schema = schema_for(VENUE).expect("ig is a schema row");
        let account: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(account, FIELDS);
        assert_eq!(schema.required(FieldOwner::Account), FIELDS);
        assert_eq!(schema.venue_fields().count(), 0);
    }

    /// The arm's one decision: a complete trio arms the demo gateway below `live`, is refused at
    /// `live`, and anything less is unconfigured at every tier.
    #[test]
    fn the_demo_arm_follows_the_accounts_one_tier() {
        let full = CredentialFields::from_pairs(FIELDS.map(|f| (f, "x")));
        let half = CredentialFields::from_pairs(FIELDS[..2].iter().map(|f| (*f, "x")));
        let full = AccountCredentials::new(Some(1), Some(&full), None);
        let half = AccountCredentials::new(Some(1), Some(&half), None);
        for tier in [AccountTier::Paper, AccountTier::Demo] {
            assert!(matches!(demo_arm(tier, &full), DemoArm::Demo(_)), "{tier}");
        }
        assert!(matches!(demo_arm(AccountTier::Live, &full), DemoArm::LiveTierNotWired));
        for tier in AccountTier::ALL {
            assert!(matches!(demo_arm(tier, &half), DemoArm::Unconfigured), "{tier}");
            assert!(
                matches!(demo_arm(tier, &AccountCredentials::none()), DemoArm::Unconfigured),
                "{tier}"
            );
        }
    }
}
