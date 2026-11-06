//! FXCM ForexConnect login config: the login pair from the started ACCOUNT's credential fields, the
//! host URL and connection name from the venue's `venue_setting` rows.
//!
//! FXCM does NOT use the HMAC [`Credentials`](vike_secrets::credentials::Credentials) shape — it
//! auths with a user/password plus a host-discovery URL and a connection name ("Demo"/"Real"). The
//! login is the account's `USER` / `PASSWORD` fields ([`FIELDS`], FXCM's row of
//! `vike_venues::credential_schema`'s `VENUE_KEY_SCHEMAS`): a credential belongs to its ACCOUNT and
//! is read by FIELD, and no name carries a tier (decision 0156 item 6). The URL and the connection
//! are the TIER's settings — `venue.fxcm.<tier>.{url,connection}` (decision 0095, whose Task 7
//! retired the credential-map fold that carried them). The password never reaches Debug/Display.
//! Absent user/password → `None` (the live gate).

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::VenueSettings;

/// FXCM's default host-discovery URL (both demo and real resolve through it via `connection`).
const DEFAULT_HOST_URL: &str = "http://www.fxcorporate.com/Hosts.jsp";

/// The account fields the loader reads, both REQUIRED, in the schema row's order —
/// `(user, password)`. `the_fields_are_the_schemas` holds them to `vike_venues::credential_schema`'s
/// fxcm row.
pub(crate) const FIELDS: [&str; 2] = ["USER", "PASSWORD"];

/// ForexConnect session parameters.
#[derive(Clone)]
pub struct FxcmConfig {
    pub user: String,
    pub password: String,
    pub url: String,
    /// "Demo" | "Real"
    pub connection: String,
}

impl std::fmt::Debug for FxcmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the password
        write!(
            f,
            "FxcmConfig(user={}, connection={}, url={})",
            self.user, self.connection, self.url
        )
    }
}

fn default_connection(env: AccountTier) -> &'static str {
    match env {
        AccountTier::Live => "Real",
        AccountTier::Paper | AccountTier::Demo => "Demo",
    }
}

/// Read FXCM config from ONE account's credentials and the venue's settings at `env`. `None` when
/// user or password is absent or blank — that absence IS the live gate (no creds → do not run). URL
/// and connection fall back to sensible defaults when the tier has no row.
///
/// ⚠ **The account is the credentials**: `creds` holds only the started account's login, so no
/// account can log in as another. The URL and the connection are not an ACCOUNT's to carry at all:
/// they are the TIER's `venue.fxcm.<tier>.{url,connection}` rows (ruling 10, decision 0095), so
/// every account read at `env` reads the same host and connection.
pub fn load_fxcm_config_from(
    env: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> Option<FxcmConfig> {
    let [user, password] = FIELDS.map(|field| creds.get(field));
    let (user, password) = (user?.to_string(), password?.to_string());
    // EXACTLY the tier's row (a machine-scoped `any` row of a tier-scoped field is not read),
    // trimmed, and a blank row reads as no row — as the credential-map read did.
    let setting = |field: &str| {
        settings.get_exact(env.into(), field).map(str::trim).filter(|v| !v.is_empty())
    };
    let url = setting("url").unwrap_or(DEFAULT_HOST_URL).to_string();
    let connection = setting("connection").unwrap_or(default_connection(env)).to_string();
    Some(FxcmConfig { user, password, url, connection })
}

/// **What this demo-only arm does with ONE account's login** — the one answer `crate::adapter`'s
/// `resolution_for`, `start_with` and `report_not_running_account` all act on, so they cannot
/// disagree. The shim question is the start's; this is the STORE's half.
///
/// The account has ONE tier (decision 0156 item 6) and its login is that tier's: this arm starts
/// only the DEMO login, so a complete login on a `live` account is one it will not use.
pub(crate) enum DemoLogin {
    /// The account's tier is below `live` and it holds the whole login: the DEMO config, on the
    /// DEMO tier's `venue.fxcm.demo.{url,connection}` settings.
    Demo(FxcmConfig),
    /// The account's tier is `live` and it holds the whole login. This arm never selects the live
    /// tier, so the venue does not run and says so
    /// (`vike_bridge_core::venue_adapter::PaperCause::LiveTierNotWired`).
    LiveTierNotWired,
    /// The account holds no complete login — the ordinary, silent unconfigured state.
    Unconfigured,
}

/// [`DemoLogin`] for an account at `tier` holding `creds`, the DEMO tier's settings read from
/// `settings`. A `Paper` tier reads as `Demo` (the start contract's rule); only `Live` is refused.
pub(crate) fn demo_login(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    settings: &VenueSettings,
) -> DemoLogin {
    let Some(cfg) = load_fxcm_config_from(AccountTier::Demo, creds, settings) else {
        return DemoLogin::Unconfigured;
    };
    match tier {
        AccountTier::Paper | AccountTier::Demo => DemoLogin::Demo(cfg),
        AccountTier::Live => DemoLogin::LiveTierNotWired,
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
