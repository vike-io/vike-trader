//! IG (IG Group) v20-style REST login config from the started ACCOUNT's credential fields.
//!
//! IG auths in two parts: a per-app **API key** (`X-IG-API-KEY`) plus account **identifier +
//! password** exchanged at `POST /session` for short-lived `CST` / `X-SECURITY-TOKEN` headers. The
//! three are the account's `API_KEY` / `IDENTIFIER` / `PASSWORD` fields ([`FIELDS`], IG's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The tier picks the gateway
//! row only ([`ig_rest_base`], the venue's `venue_url` rows). None of the three secrets reach
//! Debug/Display. Absent any of them → `Ok(None)` (the live gate); a refused address row → the
//! refusal, never "no credentials".

use vike_bridge_core::venue_adapter::{AdapterInputs, ArmingDemoOnly};
use vike_model::AccountTier;
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The account fields the loader reads, all three REQUIRED, in the schema row's order —
/// `(api_key, identifier, password)`. `the_fields_are_the_schemas` holds them to
/// `vike_venues::credential_schema`'s ig row. `pub` since the history lane: the gatewayhub, which
/// owns the credential read for it (decision 0194), reads exactly these three FIELDS of the one
/// demo account and builds an [`IgConfig`] from them.
pub const FIELDS: [&str; 3] = ["API_KEY", "IDENTIFIER", "PASSWORD"];

/// IG REST base for a tier: the row `ig <type> trade rest` of `urls` (the seed's `all` row: the
/// demo gateway for `Paper`/`Demo`, the live gateway for `Live`; the base holds `/gateway/deal`).
/// `trade` for every read: each one logs in with the account's trio, so a demo lookup never falls
/// back to the live row. The Lightstreamer host has no row: the login reply names it.
///
/// # Errors
/// The refusal naming the missing (or demo-equals-live) row.
pub fn ig_rest_base(urls: &VenueUrls, env: AccountTier) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::of_tier(env), UrlChannel::Trade, UrlProtocol::Rest, "")
}

/// IG login parameters. `PartialEq` is the question "is this the same login" (a kept session is replaced
/// when a credential rotates), so it compares the login and not [`IgConfig::fx_clock`]. The trio is three
/// [`Secret`]s, so `Debug` prints each redacted; both are written by hand because the clock has neither.
#[derive(Clone)]
pub struct IgConfig {
    pub api_key: Secret<String>,
    pub identifier: Secret<String>,
    pub password: Secret<String>,
    pub rest_base: String,
    /// The account row's `venue_account_id`: the IG account id (the BOOK) the operator named for this account,
    /// which every login of this config puts its session on (`crate::account::pick`). `None` = the rule picks
    /// (the default account if it is a USD CFD one, else the one that is). Not a secret.
    pub account_pin: Option<String>,
    /// **The FX login gate's clock** (owner ruling 2026-11-13): no login of this config, and no dial of a stream it
    /// logs in for, while the FX market is closed at its time ([`IgConfig::fx_closed_words`],
    /// [`IgConfig::fx_held_pause`]). The gatewayhub sets it (`AdapterInputs::fx_clock`); `None` (the loader's own
    /// answer, so every tool, smoke and test that does not care) is no gate.
    pub fx_clock: Option<std::sync::Arc<dyn vike_model::Clock + Send + Sync>>,
}

impl std::fmt::Debug for IgConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IgConfig")
            .field("api_key", &self.api_key)
            .field("identifier", &self.identifier)
            .field("password", &self.password)
            .field("rest_base", &self.rest_base)
            .field("account_pin", &self.account_pin)
            .field("fx_clock", &self.fx_clock.is_some())
            .finish()
    }
}

impl PartialEq for IgConfig {
    fn eq(&self, other: &Self) -> bool {
        (&self.api_key, &self.identifier, &self.password, &self.rest_base, &self.account_pin)
            == (
                &other.api_key,
                &other.identifier,
                &other.password,
                &other.rest_base,
                &other.account_pin,
            )
    }
}

impl Eq for IgConfig {}

impl IgConfig {
    /// The words a login of this config is refused in while the FX market is closed on [`IgConfig::fx_clock`]
    /// (`FX market closed: login at ...`); `None` when it may log in, which it always may with no clock.
    pub(crate) fn fx_closed_words(&self) -> Option<String> {
        let now_ms = self.fx_clock.as_ref()?.now_ms();
        vike_venues::session::fx_login_closed_until(crate::reconcile::VENUE, now_ms)
            .map(vike_venues::session::fx_login_closed_words)
    }

    /// What a retry ladder of this config waits instead of its own `pause`: `pause` on an open market, ONE wait to the
    /// open when its next login would fall while the market is closed (`vike_venues::session::fx_login_held_pause`),
    /// and `pause` itself with no clock.
    pub(crate) fn fx_held_pause(&self, pause: std::time::Duration) -> std::time::Duration {
        self.fx_clock.as_ref().map_or(pause, |clock| {
            vike_venues::session::fx_login_held_pause(
                crate::reconcile::VENUE,
                pause,
                clock.now_ms(),
            )
        })
    }
}

/// Read IG config from ONE account's credentials, on `env`'s gateway row of `urls`
/// ([`ig_rest_base`]). `Ok(None)` when any of the three fields is absent or blank — the live gate,
/// decided before any row is looked up.
///
/// ⚠ **The account is the credentials.** All THREE fields are per-account here — IG's
/// `identifier`/`password` are exchanged at `POST /session` for the tokens every later request
/// carries — and `creds` holds only the started account's, so nothing can log one account into
/// another.
///
/// # Errors
/// The trio is complete and [`ig_rest_base`] refused `env`'s row.
pub fn load_ig_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> Result<Option<IgConfig>, UrlRefusal> {
    let [Some(api_key), Some(identifier), Some(password)] = FIELDS.map(|field| creds.get(field))
    else {
        return Ok(None);
    };
    Ok(Some(IgConfig {
        api_key: Secret::new(api_key.to_string()),
        identifier: Secret::new(identifier.to_string()),
        password: Secret::new(password.to_string()),
        rest_base: ig_rest_base(urls, env)?.to_string(),
        account_pin: None,
        fx_clock: None,
    }))
}

/// [`load_ig_config_from`] for the account a start resolved, on its venue's rows
/// (`inputs.settings.urls()`), WITH its row's `venue_account_id` as the book to trade
/// ([`IgConfig::account_pin`]): `inputs.accounts` is the settings database's `account` table, read once by
/// the composition root. A blank book is no book. The adapter's start and the gatewayhub's facts read both
/// build their config here, so the account whose money the node is told is the account the orders go to.
///
/// # Errors
/// As [`load_ig_config_from`].
pub fn load_ig_config_for(inputs: &AdapterInputs<'_>) -> Result<Option<IgConfig>, UrlRefusal> {
    let urls = inputs.settings.urls();
    let Some(mut config) = load_ig_config_from(AccountTier::Demo, &inputs.credentials, urls)?
    else {
        return Ok(None);
    };
    config.account_pin = pin_of(inputs);
    config.fx_clock = inputs.fx_clock.cloned();
    Ok(Some(config))
}

/// The account row's `venue_account_id` for `inputs`' `(label, tier)`, trimmed; `None` when the row has none.
pub(crate) fn pin_of(inputs: &AdapterInputs<'_>) -> Option<String> {
    let row = inputs.accounts.account_row(crate::reconcile::VENUE, inputs.account, inputs.tier)?;
    row.venue_account_id.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(str::to_string)
}

/// **What this demo-only arm does with ONE account** at `tier` holding `creds` — the one answer
/// `crate::adapter`'s `resolve`, `start` and `report_not_running_account` all act on, so they cannot
/// disagree. The arm starts only the demo gateway, so a complete trio on a `live` account is one it
/// will not use. A complete trio whose demo row `urls` refuse is still `Demo` (the arming probe judges
/// credentials), carrying the refusal the start says.
pub(crate) fn demo_only_arm(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
) -> ArmingDemoOnly<Result<IgConfig, UrlRefusal>> {
    ArmingDemoOnly::of(tier, load_ig_config_from(AccountTier::Demo, creds, urls).transpose())
}

#[cfg(test)]
mod tests {
    use vike_bridge_core::venue_adapter_fixture::assert_demo_only_arm_follows_the_tier;
    use vike_secrets::account_credentials::CredentialFields;
    use vike_secrets::credentials::assert_account_fields_are_the_schemas;

    use super::*;
    use crate::reconcile::VENUE;

    /// The seed's row `ig <type> all rest`: what the lookup must answer, byte for byte.
    fn seed_url(ty: UrlType) -> &'static str {
        vike_venues::venue_urls::seed_url("ig", ty, UrlChannel::All, UrlProtocol::Rest, "")
            .expect("the seed holds ig's rows")
    }

    /// **The FX login gate reads the clock the config CARRIES** (owner ruling 2026-11-13): on a Saturday clock a login
    /// is refused in the gate's words and a retry waits once to Sunday 21:00 UTC; on a Wednesday clock, and with no
    /// clock at all, nothing is held. The clock is not the login: two configs that differ only in it are the same one.
    #[test]
    fn the_fx_gate_reads_the_configs_own_clock() {
        let at = |stamp: &str| vike_model::time::parse_utc_timestamp_ms(stamp).expect("an instant");
        let fields = CredentialFields::from_pairs([
            ("API_KEY", "key-xyz"),
            ("IDENTIFIER", "myuser"),
            ("PASSWORD", "s3cr3t"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let urls = VenueUrls::seed("ig");
        let open = load_ig_config_from(AccountTier::Demo, &creds, &urls).unwrap().unwrap();
        let pause = std::time::Duration::from_secs(3);
        assert_eq!((open.fx_closed_words(), open.fx_held_pause(pause)), (None, pause), "no clock");
        let wednesday = IgConfig {
            fx_clock: Some(std::sync::Arc::new(move || at("2026-11-11T12:00:00Z"))),
            ..open.clone()
        };
        assert_eq!((wednesday.fx_closed_words(), wednesday.fx_held_pause(pause)), (None, pause));
        let saturday = IgConfig {
            fx_clock: Some(std::sync::Arc::new(move || at("2026-11-14T12:00:00Z"))),
            ..open.clone()
        };
        assert_eq!(
            saturday.fx_closed_words().as_deref(),
            Some("FX market closed: login at 2026-11-15T21:00:00Z")
        );
        assert_eq!(saturday.fx_held_pause(pause), std::time::Duration::from_secs(33 * 3600));
        assert_eq!(saturday, open, "the same login");
    }

    #[test]
    fn gate_and_no_secret_leak() {
        let urls = VenueUrls::seed("ig");
        let none = load_ig_config_from(AccountTier::Demo, &AccountCredentials::none(), &urls);
        assert!(none.expect("absent creds look nothing up").is_none());

        let fields = CredentialFields::from_pairs([
            ("API_KEY", "key-xyz"),
            ("IDENTIFIER", "myuser"),
            ("PASSWORD", "s3cr3t"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let c = load_ig_config_from(AccountTier::Demo, &creds, &urls).unwrap().unwrap();
        assert_eq!(c.rest_base, "https://demo-api.ig.com/gateway/deal");
        assert_eq!(c.rest_base, seed_url(UrlType::Demo));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("key-xyz") && !dbg.contains("myuser") && !dbg.contains("s3cr3t"));

        assert_eq!(ig_rest_base(&urls, AccountTier::Live), Ok(seed_url(UrlType::Live)));
        assert!(seed_url(UrlType::Live).contains("//api.ig.com"));
        assert_eq!(ig_rest_base(&urls, AccountTier::Paper), ig_rest_base(&urls, AccountTier::Demo));
    }

    /// No row, no gateway: a complete trio over an empty table is the refusal naming the row (never
    /// "no credentials"), the arm still `Demo`, and a missing demo row never falls back to live.
    #[test]
    fn a_missing_row_is_the_refusal_and_never_the_live_gateway() {
        let fields = CredentialFields::from_pairs([
            ("API_KEY", "k"),
            ("IDENTIFIER", "u"),
            ("PASSWORD", "p"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let why = load_ig_config_from(AccountTier::Demo, &creds, &VenueUrls::default())
            .expect_err("an empty table refuses");
        assert!(why.to_string().contains("venue_url"), "{why}");
        assert!(matches!(
            demo_only_arm(AccountTier::Demo, &creds, &VenueUrls::default()),
            ArmingDemoOnly::Demo(Err(_))
        ));
        let live_only = VenueUrls::from_rows(
            "ig",
            [("ig", UrlType::Live, UrlChannel::All, UrlProtocol::Rest, "", "https://live.example")],
        );
        assert!(ig_rest_base(&live_only, AccountTier::Demo).is_err(), "demo never reads live");
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
        let urls = VenueUrls::seed("ig");
        assert_demo_only_arm_follows_the_tier(FIELDS, |tier, creds| {
            demo_only_arm(tier, creds, &urls)
        });
    }
}
