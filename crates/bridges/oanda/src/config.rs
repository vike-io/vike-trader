//! OANDA v20 login config from the mounted ACCOUNT's credential fields.
//!
//! OANDA auths with a personal API token (Bearer) + an account id — not the HMAC
//! [`Credentials`](vike_secrets::credentials::Credentials) shape. The two are the account's
//! `API_KEY` / `ACCOUNT_ID` fields ([`FIELDS`], OANDA's row of `vike_venues::credential_schema`'s
//! `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and is read by FIELD, and no name
//! carries a tier (decision 0156 item 6). The tier picks the hosts only ([`oanda_hosts`]). The
//! token never reaches Debug/Display. Absent token or account id → `None` (the live gate).

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

/// The account's API-token field — the Bearer every request carries, and the ONE field the history
/// lane reads ([`load_oanda_history_token`]).
pub(crate) const API_KEY: &str = "API_KEY";

/// The account's id field — embedded in every v20 REST path, so it IS the account an order lands
/// in.
const ACCOUNT_ID: &str = "ACCOUNT_ID";

/// The account fields the config loader reads, both REQUIRED, in the schema row's order.
/// `the_fields_are_the_schemas` holds them to `vike_venues::credential_schema`'s oanda row.
pub(crate) const FIELDS: [&str; 2] = [API_KEY, ACCOUNT_ID];

/// OANDA REST + streaming hosts for a tier. `Paper`/`Demo` = fxPractice, `Live` = fxTrade.
pub fn oanda_hosts(env: AccountTier) -> (&'static str, &'static str) {
    match env {
        AccountTier::Live => ("https://api-fxtrade.oanda.com", "https://stream-fxtrade.oanda.com"),
        AccountTier::Paper | AccountTier::Demo => {
            ("https://api-fxpractice.oanda.com", "https://stream-fxpractice.oanda.com")
        }
    }
}

/// ForexConnect-free v20 session parameters.
#[derive(Clone)]
pub struct OandaConfig {
    pub api_token: String,
    pub account_id: String,
    pub rest_base: String,
    pub stream_base: String,
}

impl std::fmt::Debug for OandaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the token
        write!(f, "OandaConfig(account_id={}, rest_base={})", self.account_id, self.rest_base)
    }
}

/// Read OANDA config from ONE account's credentials, on `env`'s hosts. `None` when the token or the
/// account id is absent or blank — the live gate.
///
/// ⚠ **The account is the credentials.** OANDA's v20 REST paths embed the account id, and `creds`
/// holds only the mounted account's fields, so no account can place its orders in another's.
pub fn load_oanda_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
) -> Option<OandaConfig> {
    let [api_token, account_id] = FIELDS.map(|field| creds.get(field));
    let (rest_base, stream_base) = oanda_hosts(env);
    Some(OandaConfig {
        api_token: api_token?.to_string(),
        account_id: account_id?.to_string(),
        rest_base: rest_base.to_string(),
        stream_base: stream_base.to_string(),
    })
}

/// **The one API token OANDA's history lane reads**, out of ONE account's credentials — in
/// production the DEMO (practice) oanda account's, which the datahub reads by field; any account's
/// in a test. `None` when it is absent or blank (the same blank-is-absent rule
/// [`load_oanda_config_from`] applies).
///
/// ⚠ **Not [`load_oanda_config_from`], and the difference is the point.** That loader needs the
/// account id as well, because an order is placed IN an account; this needs none, because a candles
/// read is account-free. An account holding a token and no account id serves history and arms
/// nothing.
///
/// ⚠ **Which account is the caller's choice, and a LIVE account is never refused here.**
/// [`mountable_tier`] refuses a live account's pair because a store that says "live" while orders
/// trade the practice account is the silent wrong-account failure that refusal exists to prevent.
/// History places no order, so there is no account to be wrong about; the datahub asks for the
/// practice account's token and nothing else.
pub fn load_oanda_history_token(creds: &AccountCredentials<'_>) -> Option<String> {
    creds.get(API_KEY).map(str::to_string)
}

/// **A LIVE account's complete pair that no code path can select** — the evidence behind
/// [`MountableTier::LiveUnreachable`], and an ERROR rather than an absence: an ABSENT credential is
/// the ordinary unconfigured state and is silent, while one that is PRESENT and unusable must be
/// said out loud (the same class, and the same disposition, as
/// `vike_secrets::credentials::missing_required_passphrase`).
///
/// Carries the ACCOUNT the refusal speaks for and NOTHING else: no field value and no credential
/// name (a credential belongs to its account, and no name carries a tier — decision 0156 item 6).
/// An OANDA API token is a bearer credential, so no value reaches this type, its derived `Debug`,
/// or the line [`Display`] renders — the same rule [`OandaConfig`]'s hand-written `Debug` keeps.
///
/// [`Display`]: std::fmt::Display
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableLiveTier {
    /// The account the refusal speaks for — a `live` account holding its whole pair.
    pub account: AccountLabel,
}

impl std::fmt::Display for UnreachableLiveTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whose = self
            .account
            .text()
            .map_or_else(|| "the default account".to_string(), |label| format!("account {label}"));
        write!(
            f,
            "oanda: {whose} is a LIVE account (tier `live`) holding its API_KEY and ACCOUNT_ID, \
             but this build can reach NO fxTrade tier — `oanda_hosts` maps AccountTier::Live onto \
             the live hosts and nothing in the workspace ever asks `load_oanda_config_from` for \
             that tier, so a live account selects nothing. OANDA therefore stays PAPER, and \
             deliberately does NOT fall through to the practice tier: trading fxPractice under a \
             live account is exactly the silent ignore this refusal exists to prevent. To trade \
             the PRACTICE account, give it its own account row at tier `demo` holding its \
             practice token and account id (`vike-cli secrets accounts` names what the store \
             holds); there is no supported way to trade an fxTrade account from this build.",
        )
    }
}

/// What an OANDA mount may actually do with the account it was handed — the ONE decision, so
/// `crates/bridges/oanda/src/mount.rs`'s `OandaVenueMount` — its `mount` and its `resolve`, the
/// arming probe — cannot drift about which tier is reachable.
#[derive(Debug)]
pub enum MountableTier {
    /// The practice (fxPractice) tier: an account below `live` holding its whole pair. The only
    /// tier with a caller.
    Practice(OandaConfig),
    /// A `live` account holding its whole pair. REFUSED — see [`UnreachableLiveTier`].
    LiveUnreachable(UnreachableLiveTier),
    /// No complete pair. The ordinary unconfigured state: absent credentials ARE the live gate, and
    /// an absence is reported by staying paper, silently.
    Unconfigured,
}

/// Resolve what an OANDA mount may do for `account`, an account at `tier` holding `creds`.
///
/// An account has ONE tier and its credentials are that tier's (decision 0156 item 6): a `live`
/// account's complete pair is a live intent this build cannot serve, and it is REFUSED — never read
/// as "unconfigured" (the configured-and-inert silence `crate::mount`'s module doc names) and never
/// served from the practice hosts (real orders on the practice account while the operator believes
/// they are live). A `Paper` tier reads as `Demo` (the mount contract's rule). An incomplete pair
/// is unconfigured at every tier: the loader calls it absent.
pub fn mountable_tier(
    account: &AccountLabel,
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
) -> MountableTier {
    let Some(cfg) = load_oanda_config_from(AccountTier::Demo, creds) else {
        return MountableTier::Unconfigured;
    };
    match tier {
        AccountTier::Paper | AccountTier::Demo => MountableTier::Practice(cfg),
        AccountTier::Live => {
            MountableTier::LiveUnreachable(UnreachableLiveTier { account: account.clone() })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use vike_secrets::account_credentials::CredentialFields;
    use vike_venues::credential_schema::{FieldOwner, schema_for};

    use super::*;
    use crate::recon_client::VENUE;

    #[test]
    fn gate_hosts_and_no_token_leak() {
        assert!(load_oanda_config_from(AccountTier::Demo, &AccountCredentials::none()).is_none());

        let fields = CredentialFields::from_pairs([
            ("API_KEY", "tok-abc-123"),
            ("ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let c = load_oanda_config_from(AccountTier::Demo, &creds).unwrap();
        assert_eq!(c.account_id, "101-004-1234567-001");
        assert!(c.rest_base.contains("fxpractice"));
        assert!(!format!("{c:?}").contains("tok-abc-123")); // token must not leak

        let (rest, stream) = oanda_hosts(AccountTier::Live);
        assert!(rest.contains("fxtrade") && stream.contains("fxtrade"));
        assert_eq!(oanda_hosts(AccountTier::Paper), oanda_hosts(AccountTier::Demo));
    }

    /// **The fields this file reads are `vike_venues::credential_schema`'s**: OANDA's row holds
    /// exactly [`FIELDS`] for an account, both required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        let schema = schema_for(VENUE).expect("oanda is a schema row");
        let account: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(account, FIELDS);
        assert_eq!(schema.required(FieldOwner::Account), FIELDS);
        assert_eq!(schema.venue_fields().count(), 0);
    }

    /// The refusal names the account it speaks for, in both directions of the default/labelled
    /// split — and the account it names is the one asked about, whatever another account holds.
    #[test]
    fn the_refusal_names_the_account_it_speaks_for() {
        let fields = CredentialFields::from_pairs([("API_KEY", "tok"), ("ACCOUNT_ID", "001-1")]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let MountableTier::LiveUnreachable(refusal) =
            mountable_tier(&alt, AccountTier::Live, &creds)
        else {
            panic!("a live account's whole pair is refused");
        };
        assert_eq!(refusal.account, alt);
        assert!(refusal.to_string().contains("account ALT"), "{refusal}");
        assert_matches!(
            mountable_tier(&AccountLabel::Default, AccountTier::Live, &creds),
            MountableTier::LiveUnreachable(u) if u.to_string().contains("the default account")
        );
    }
}
