//! OANDA v20 login config from the started ACCOUNT's credential fields.
//!
//! OANDA auths with a personal API token (Bearer) + an account id — not the HMAC
//! [`Credentials`](vike_secrets::credentials::Credentials) shape. The two are the account's
//! `API_KEY` / `ACCOUNT_ID` fields ([`FIELDS`], OANDA's row of `vike_venues::credential_schema`'s
//! `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and is read by FIELD, and no name
//! carries a tier (decision 0156 item 6). The tier picks the address rows only ([`oanda_hosts`], the
//! venue's `venue_url` rows). The token never reaches Debug/Display. Absent token or account id →
//! `Ok(None)` (the live gate); a refused address row → the refusal, never "no credentials".

use vike_bridge_core::venue_adapter::ArmingDemoOnly;
use vike_model::AccountTier;
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The account's API-token field — the Bearer every request carries, and the ONE field the history
/// lane reads ([`load_oanda_history_token`]).
pub(crate) const API_KEY: &str = "API_KEY";

/// The account's id field — embedded in every v20 REST path, so it IS the account an order lands
/// in.
const ACCOUNT_ID: &str = "ACCOUNT_ID";

/// The account fields the config loader reads, both REQUIRED, in the schema row's order.
/// `the_fields_are_the_schemas` holds them to `vike_venues::credential_schema`'s oanda row.
pub(crate) const FIELDS: [&str; 2] = [API_KEY, ACCOUNT_ID];

/// OANDA REST + streaming bases for a tier: the rows `oanda <type> trade rest` and `oanda <type> trade
/// stream` of `urls` (the seed's `all` rows: fxPractice for `Paper`/`Demo`, fxTrade for `Live`).
/// `trade` for every read: each one carries the account's token, so a demo lookup never falls back to
/// the live rows.
///
/// # Errors
/// The refusal naming the missing (or demo-equals-live) row.
pub fn oanda_hosts(urls: &VenueUrls, env: AccountTier) -> Result<(String, String), UrlRefusal> {
    let base = |protocol| {
        urls.url(UrlType::of_tier(env), UrlChannel::Trade, protocol, "").map(str::to_string)
    };
    Ok((base(UrlProtocol::Rest)?, base(UrlProtocol::Stream)?))
}

/// ForexConnect-free v20 session parameters. The token is a [`Secret`]: the derived `Debug` prints
/// it redacted.
#[derive(Clone, Debug)]
pub struct OandaConfig {
    pub api_token: Secret<String>,
    pub account_id: String,
    pub rest_base: String,
    pub stream_base: String,
}

/// Read OANDA config from ONE account's credentials, on `env`'s rows of `urls` ([`oanda_hosts`]).
/// `Ok(None)` when the token or the account id is absent or blank — the live gate, decided before any
/// row is looked up.
///
/// ⚠ **The account is the credentials.** OANDA's v20 REST paths embed the account id, and `creds`
/// holds only the started account's fields, so no account can place its orders in another's.
///
/// # Errors
/// The pair is complete and [`oanda_hosts`] refused `env`'s rows.
pub fn load_oanda_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> Result<Option<OandaConfig>, UrlRefusal> {
    let [Some(api_token), Some(account_id)] = FIELDS.map(|field| creds.get(field)) else {
        return Ok(None);
    };
    let (rest_base, stream_base) = oanda_hosts(urls, env)?;
    Ok(Some(OandaConfig {
        api_token: Secret::new(api_token.to_string()),
        account_id: account_id.to_string(),
        rest_base,
        stream_base,
    }))
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
/// [`demo_only_arm`] refuses a live account's pair because a store that says "live" while orders
/// trade the practice account is the silent wrong-account failure that refusal exists to prevent.
/// History places no order, so there is no account to be wrong about; the datahub asks for the
/// practice account's token and nothing else.
pub fn load_oanda_history_token(creds: &AccountCredentials<'_>) -> Option<String> {
    creds.get(API_KEY).map(str::to_string)
}

/// What an OANDA start may actually do with an account at `tier` holding `creds` — the ONE decision, so
/// `crates/bridges/oanda/src/adapter.rs`'s `VenueAdapterOanda` — its `start` and its `resolve`, the
/// arming probe — cannot drift about which tier is reachable. The shared demo-only arm
/// (`vike_bridge_core::venue_adapter::ArmingDemoOnly`) over the practice (fxPractice) config:
///
/// * `Demo(config)`: an account below `live` holding its whole pair. The only tier with a caller. `Err` when the
///   practice address rows were refused: the pair is still present (the arming probe arms on it), and the start
///   refuses with the refusal's words.
/// * `LiveTierNotWired`: a `live` account holding its whole pair. An account has ONE tier and its credentials are
///   that tier's (decision 0156 item 6), so this is a live intent this build cannot serve, and it is REFUSED —
///   never read as "unconfigured" (the configured-and-inert silence `crate::adapter`'s module doc names) and never
///   served from the practice hosts (real orders on the practice account while the operator believes they are
///   live). It is an ERROR rather than an absence (an ABSENT credential is the ordinary unconfigured state and is
///   silent, one PRESENT and unusable is said out loud), in the shared demo-only phrase
///   (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`), which names the account and never a
///   credential name or value.
/// * `Unconfigured`: no complete pair, at every tier (the loader calls an incomplete pair absent). The ordinary
///   unconfigured state: absent credentials ARE the live gate, and an absence is reported as the account not
///   running, with its reason.
///
/// A `Paper` tier reads as `Demo` (the start contract's rule). `urls` are the venue's address rows; only the
/// practice ones are ever asked for.
pub fn demo_only_arm(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> ArmingDemoOnly<Result<OandaConfig, UrlRefusal>> {
    ArmingDemoOnly::of(tier, load_oanda_config_from(AccountTier::Demo, creds, urls).transpose())
}

#[cfg(test)]
mod tests {
    use vike_secrets::account_credentials::CredentialFields;
    use vike_secrets::credentials::assert_account_fields_are_the_schemas;

    use super::*;
    use crate::reconcile::VENUE;

    /// The seed's row `oanda <type> all <protocol>`: what the lookup must answer, byte for byte.
    fn seed_url(ty: UrlType, protocol: UrlProtocol) -> &'static str {
        vike_venues::venue_urls::seed_url("oanda", ty, UrlChannel::All, protocol, "")
            .expect("the seed holds oanda's rows")
    }

    #[test]
    fn gate_hosts_and_no_token_leak() {
        let urls = VenueUrls::seed("oanda");
        let none = load_oanda_config_from(AccountTier::Demo, &AccountCredentials::none(), &urls);
        assert!(none.expect("absent creds look nothing up").is_none());

        let fields = CredentialFields::from_pairs([
            ("API_KEY", "tok-abc-123"),
            ("ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let c = load_oanda_config_from(AccountTier::Demo, &creds, &urls).unwrap().unwrap();
        assert_eq!(c.account_id, "101-004-1234567-001");
        assert_eq!(c.rest_base, "https://api-fxpractice.oanda.com");
        assert_eq!(c.rest_base, seed_url(UrlType::Demo, UrlProtocol::Rest));
        assert_eq!(c.stream_base, seed_url(UrlType::Demo, UrlProtocol::Stream));
        assert!(!format!("{c:?}").contains("tok-abc-123")); // token must not leak

        let (rest, stream) = oanda_hosts(&urls, AccountTier::Live).unwrap();
        assert_eq!(rest, seed_url(UrlType::Live, UrlProtocol::Rest));
        assert_eq!(stream, seed_url(UrlType::Live, UrlProtocol::Stream));
        assert!(rest.contains("fxtrade") && stream.contains("fxtrade"));
        assert_eq!(oanda_hosts(&urls, AccountTier::Paper), oanda_hosts(&urls, AccountTier::Demo));
    }

    /// No row, no address: a complete pair over an empty table is the refusal naming the row, never
    /// "no credentials", and a missing demo row never falls back to the live one.
    #[test]
    fn a_missing_row_is_the_refusal_and_never_the_live_host() {
        let fields = CredentialFields::from_pairs([("API_KEY", "tok"), ("ACCOUNT_ID", "001-1")]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let why = load_oanda_config_from(AccountTier::Demo, &creds, &VenueUrls::default())
            .expect_err("an empty table refuses");
        assert!(why.to_string().contains("venue_url"), "{why}");
        let live_only = VenueUrls::from_rows(
            "oanda",
            [(
                "oanda",
                UrlType::Live,
                UrlChannel::All,
                UrlProtocol::Rest,
                "",
                "https://live.example",
            )],
        );
        assert!(oanda_hosts(&live_only, AccountTier::Demo).is_err(), "demo never reads live");
        assert!(matches!(
            demo_only_arm(AccountTier::Demo, &creds, &VenueUrls::default()),
            ArmingDemoOnly::Demo(Err(_))
        ));
    }

    /// **The fields this file reads are `vike_venues::credential_schema`'s**: OANDA's row holds
    /// exactly [`FIELDS`] for an account, both required, and nothing for the venue.
    #[test]
    fn the_fields_are_the_schemas() {
        assert_account_fields_are_the_schemas(VENUE, &FIELDS);
    }

    /// A live account's whole pair is refused, never the practice hosts in its place (the sentence, and the
    /// account it names, are the adapter's: `crate::adapter`'s tests).
    #[test]
    fn a_live_accounts_pair_is_refused() {
        let fields = CredentialFields::from_pairs([("API_KEY", "tok"), ("ACCOUNT_ID", "001-1")]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let urls = VenueUrls::seed("oanda");
        assert!(matches!(
            demo_only_arm(AccountTier::Live, &creds, &urls),
            ArmingDemoOnly::LiveTierNotWired
        ));
    }
}
