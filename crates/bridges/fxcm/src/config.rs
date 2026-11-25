//! FXCM ForexConnect login config: the login pair from the started ACCOUNT's credential fields, the
//! host-discovery URL from the venue's `venue_url` address rows.
//!
//! FXCM does NOT use the HMAC [`Credentials`](vike_secrets::credentials::Credentials) shape — it
//! auths with a user/password plus a host-discovery URL and a connection name ("Demo"/"Real"). The
//! login is the account's `USER` / `PASSWORD` fields ([`FIELDS`], FXCM's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The URL is the TIER's
//! `fxcm <type> login rest <connection>` address row, whose PRODUCT is the connection word
//! ([`connection_of`]): one tier, one connection, one row. The password never reaches Debug/Display.
//! Absent user/password → `None` (the live gate).

use vike_bridge_core::venue_adapter::ArmingDemoOnly;
use vike_model::AccountTier;
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType};

/// The account fields the loader reads, both REQUIRED, in the schema row's order —
/// `(user, password)`. `the_fields_are_the_schemas` holds them to `vike_venues::credential_schema`'s
/// fxcm row.
pub(crate) const FIELDS: [&str; 2] = ["USER", "PASSWORD"];

/// ForexConnect session parameters. The password is a [`Secret`]: `Debug` prints it redacted, and is
/// written by hand because [`FxcmConfig::fx_clock`] is a clock with none.
#[derive(Clone)]
pub struct FxcmConfig {
    pub user: String,
    pub password: Secret<String>,
    pub url: String,
    /// "Demo" | "Real"
    pub connection: String,
    /// **The FX login gate's clock** (owner ruling 2026-11-13): a login retry of this config's sessions that would
    /// fall while the FX market is closed at its time waits ONCE for the open
    /// (`crate::reconcile::redial::pause_to_a_login`). The start sets it (`AdapterInputs::fx_clock`); `None` (the
    /// loader's own answer, so every tool, smoke and test that does not care) is no gate.
    pub fx_clock: Option<std::sync::Arc<dyn vike_model::Clock + Send + Sync>>,
}

impl std::fmt::Debug for FxcmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxcmConfig")
            .field("user", &self.user)
            .field("password", &self.password)
            .field("url", &self.url)
            .field("connection", &self.connection)
            .field("fx_clock", &self.fx_clock.is_some())
            .finish()
    }
}

/// The ForexConnect connection word of `env`'s accounts — `Real` at live, `Demo` below (a `Paper`
/// tier reads as `Demo`) — and the PRODUCT of that tier's `fxcm <type> login rest` address row.
fn connection_of(env: AccountTier) -> &'static str {
    match env {
        AccountTier::Live => "Real",
        AccountTier::Paper | AccountTier::Demo => "Demo",
    }
}

/// Read FXCM config from ONE account's credentials and the venue's addresses at `env`. `None` when
/// user or password is absent or blank — that absence IS the live gate (no creds → do not run).
/// `Some(Err(refusal))` when the login is whole and the tier's `fxcm <type> login rest <connection>`
/// address row is missing: the refusal names the row, and nothing is dialled.
///
/// ⚠ **The account is the credentials**: `creds` holds only the started account's login, so no
/// account can log in as another. The URL and the connection are not an ACCOUNT's to carry at all:
/// they are the TIER's address row, so every account read at `env` reads the same host and
/// connection.
pub fn load_fxcm_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> Option<Result<FxcmConfig, UrlRefusal>> {
    let [user, password] = FIELDS.map(|field| creds.get(field));
    let (user, password) = (user?.to_string(), Secret::new(password?.to_string()));
    let connection = connection_of(env);
    let url = settings.urls().url(
        UrlType::of_tier(env),
        UrlChannel::Login,
        UrlProtocol::Rest,
        connection,
    );
    Some(url.map(|url| FxcmConfig {
        user,
        password,
        url: url.to_string(),
        connection: connection.to_string(),
        fx_clock: None,
    }))
}

/// **What this demo-only arm does with ONE account's login** — the one answer `crate::adapter`'s
/// `resolution_for`, `start_with` and `report_not_running_account` all act on, so they cannot
/// disagree. The shim question is the start's; this is the STORE's half.
///
/// The account has ONE tier (decision 0156 item 6) and its login is that tier's: this arm starts
/// only the DEMO login, so a complete login on a `live` account is one it will not use
/// ([`ArmingDemoOnly::LiveTierNotWired`]). The DEMO config rides on the `fxcm demo login rest Demo`
/// address row — or that row's refusal, which ARMS (the login decides the probe) and which the start
/// says by name instead of dialling. A `Paper` tier reads as `Demo` (the start contract's rule).
pub(crate) fn demo_login(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> ArmingDemoOnly<Result<FxcmConfig, UrlRefusal>> {
    ArmingDemoOnly::of(tier, load_fxcm_config_from(AccountTier::Demo, creds, settings))
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
