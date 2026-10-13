//! cTrader OAuth + account config over the mounted ACCOUNT's credentials — the credential gate
//! (`CtraderConfig::from_credentials`; the caller owns the store read, per the settings-registry
//! rule that libraries take configuration as parameters). Deterministic, no process-env mutation —
//! see `tests/offline/config_env.rs`.
//!
//! # A credential belongs to its account (decision 0156 item 6)
//!
//! The loader reads FIELDS of the [`AccountCredentials`] it is handed and never composes a name: no
//! credential name carries a tier, and nothing here derives one. cTrader's OAuth2 shape is two
//! owners:
//!
//! * the VENUE's application pair, [`CLIENT_ID`] / [`CLIENT_SECRET`] — one Spotware app
//!   registration, held once for the venue and shared by every account
//!   (`AccountCredentials::venue_get`);
//! * the ACCOUNT's grant, [`ACCESS_TOKEN`] / [`REFRESH_TOKEN`] and the optional [`ACCOUNT_ID`] (one
//!   OAuth grant per account).
//!
//! The field words are `crates/vike-model/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS` ctrader
//! row, held equal to it by `the_fields_are_the_schemas`. The TIER is the account row's
//! (`MountInputs::tier`), and the only thing it selects here is the host ([`ctrader_host`]). No
//! Python twin — cTrader Open API (https://help.ctrader.com/open-api/).

use std::path::Path;

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

use crate::conn::ConnConfig;
use crate::token_store::{TokenPersist, settings_dir_beside_state_dir};

/// cTrader's TCP endpoint port (the protobuf handshake, both data + exec) — the same for every
/// account once you're pointed at the right host.
pub const CTRADER_PORT: u16 = 5035;

/// The account's OAuth ACCESS token field — required; rotated by `crate::token_store`'s `persist`.
pub const ACCESS_TOKEN: &str = "ACCESS_TOKEN";

/// The account's OAuth REFRESH token field — required; cTrader rotates it on every refresh, so
/// `crate::token_store`'s `persist` writes it back beside [`ACCESS_TOKEN`].
pub const REFRESH_TOKEN: &str = "REFRESH_TOKEN";

/// The account's cTrader trading-account id (`ctid`) field — OPTIONAL: when absent it is
/// discovered at connect. It is also the field `crate::mount`'s book identity names.
pub const ACCOUNT_ID: &str = "ACCOUNT_ID";

/// The VENUE's application field: the Spotware app registration's client id.
pub const CLIENT_ID: &str = "CLIENT_ID";

/// The VENUE's application field: the Spotware app registration's client secret.
pub const CLIENT_SECRET: &str = "CLIENT_SECRET";

/// Host for an account's tier: `demo.ctraderapi.com` (Demo, and Paper — cTrader has no separate
/// paper tier, and a bridge reads `Paper` as it reads `Demo`) or `live.ctraderapi.com` (Live).
pub fn ctrader_host(tier: AccountTier) -> &'static str {
    match tier {
        AccountTier::Live => "live.ctraderapi.com",
        AccountTier::Demo | AccountTier::Paper => "demo.ctraderapi.com",
    }
}

/// **Whether this DEMO-ONLY arm mounts an account at `tier`**: `Demo`, and `Paper` (read as demo),
/// yes; `Live` no — the mount dials the demo host only, so a live account's grant is never sent to
/// the venue. The one place the arm's tier decision is spelled; `crate::mount` asks it rather than
/// naming a tier (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` reads a tier named in a
/// bridge's mount as an arm that can bind it).
pub(crate) fn demo_arm(tier: AccountTier) -> bool {
    match tier {
        AccountTier::Demo | AccountTier::Paper => true,
        AccountTier::Live => false,
    }
}

/// Gated cTrader connection config: `client_id`/`client_secret` (the venue's Spotware app
/// registration, shared by every account) plus the account's OAuth token pair. `account_id` is
/// optional — when absent, `conn::connect_and_auth` discovers it via
/// `GetAccountListByAccessToken` (first non-live account only; see `conn.rs`'s NEVER-default-to-live
/// safety note). `Debug` is manually implemented to redact `client_secret`/`access_token`/
/// `refresh_token` — never let a secret leak into a log line or panic message (mirrors
/// `conn::ConnConfig`/`oauth::Token`'s manual `Debug`).
#[derive(Clone)]
pub struct CtraderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<i64>,
    pub host: String,
    pub port: u16,
    /// Where a REFRESHED grant is written back — the credential store and the account row whose
    /// grant this is, when the caller resolved a project.
    ///
    /// `None` (every caller of the plain [`CtraderConfig::from_credentials`] — tests, the catalog
    /// probe) keeps the historical behaviour EXACTLY: a refresh lives as long as the process and
    /// nothing is written. See [`crate::token_store`] for why the store is the home.
    pub token_persist: Option<TokenPersist>,
}

impl std::fmt::Debug for CtraderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtraderConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("account_id", &self.account_id)
            .field("host", &self.host)
            .field("port", &self.port)
            // A PATH and an account row id — no secret. Shown because "is rotation armed on this
            // mount" is exactly the question this Debug is read to answer.
            .field("token_persist", &self.token_persist)
            .finish()
    }
}

impl CtraderConfig {
    /// Build the [`ConnConfig`] `conn::connect_and_auth`/`connect_and_auth_exec` need, wired with
    /// this config's `refresh_token` so the actor's reconnect/token-refresh path (Task 6) has what
    /// it needs from the very first connect. Always TLS-on (`no_tls: false`) — this is the
    /// production entry point; `ConnConfig::for_test` is the only sanctioned way to speak
    /// plaintext, and only against the in-process fake server.
    pub fn to_conn_config(&self) -> ConnConfig {
        let mut conn_cfg = ConnConfig::new(
            self.host.clone(),
            self.port,
            self.client_id.clone(),
            self.client_secret.clone(),
            self.access_token.clone(),
        );
        conn_cfg.account_id = self.account_id;
        conn_cfg.refresh_token = Some(self.refresh_token.clone());
        conn_cfg.token_persist = self.token_persist.clone();
        conn_cfg
    }

    /// Read from one account's credentials (`crate::mount`'s `CtraderVenueMount` hands
    /// `MountInputs::credentials`, a smoke `vike_secrets::smoke_directory`'s, a test a fixture's),
    /// for an account at `tier` (the host). `None` when any REQUIRED field is absent or blank — the
    /// venue's [`CLIENT_ID`] / [`CLIENT_SECRET`] or the account's [`ACCESS_TOKEN`] /
    /// [`REFRESH_TOKEN`] — the live gate; [`ACCOUNT_ID`] is optional (discovered at connect time
    /// when unset, or when present but unparseable as an `i64`).
    pub fn from_credentials(
        tier: AccountTier,
        credentials: &AccountCredentials<'_>,
    ) -> Option<Self> {
        Self::from_credentials_with_store(tier, credentials, None)
    }

    /// [`CtraderConfig::from_credentials`] plus the ROTATION HOME: `state_dir` is
    /// `<project>/settings/state` as the composition root's boot resolved it, from which the
    /// settings directory is DERIVED (`crate::token_store::settings_dir_beside_state_dir`) — never
    /// re-walked here, per the root `CLAUDE.md`'s one-walk-decides rule.
    ///
    /// # Precedence: the store is the ONE home, so there is none to decide
    ///
    /// The grant is read from the account's fields and written back to the SAME account's same two
    /// fields. There is no second source of truth to rank against it — deliberately. The authorize
    /// bin's `settings/state/ctrader_token.json` stays that tool's own output record and is NOT
    /// read: two files that can each answer "what is the current grant" is the ambiguity this
    /// design removes, not a redundancy it wants.
    ///
    /// # ⚠ The ROTATION target is the account the grant was read from
    ///
    /// [`TokenPersist::account_row`] is `credentials.account_id()` — the `account.id` the fields
    /// belong to — so a grant refreshed on one account is written back to THAT account's two fields
    /// and never over another account's grant. Credentials handed with no account row still load
    /// (a test, a tool), and their rotation is REFUSED at the write rather than landing anywhere
    /// (`crate::token_store`'s `persist`).
    ///
    /// The application pair is the VENUE's: every account reads the same one. It cannot select an
    /// account — `access_token`/`refresh_token` do, and those are strictly per-account.
    ///
    /// `state_dir` of `None` yields `token_persist: None` — byte-identical to the pre-rotation
    /// behaviour, which is what every test and the catalog probe get.
    pub fn from_credentials_with_store(
        tier: AccountTier,
        credentials: &AccountCredentials<'_>,
        state_dir: Option<&Path>,
    ) -> Option<Self> {
        let client_id = credentials.venue_get(CLIENT_ID)?.to_string();
        let client_secret = credentials.venue_get(CLIENT_SECRET)?.to_string();
        let access_token = credentials.get(ACCESS_TOKEN)?.to_string();
        let refresh_token = credentials.get(REFRESH_TOKEN)?.to_string();
        let account_id = credentials.get(ACCOUNT_ID).and_then(|s| s.parse().ok());

        // ONE resolution, TWO derivations: the store to write and the state directory the change
        // journal hangs off. Both out of the caller's `state_dir`, so a rotation cannot record
        // itself into one project's ledger while writing another project's store.
        let token_persist = state_dir.and_then(|state| {
            settings_dir_beside_state_dir(state).map(|settings_dir| TokenPersist {
                settings_dir,
                account_row: credentials.account_id(),
                state_dir: state.to_path_buf(),
            })
        });

        Some(CtraderConfig {
            client_id,
            client_secret,
            access_token,
            refresh_token,
            account_id,
            host: ctrader_host(tier).to_string(),
            port: CTRADER_PORT,
            token_persist,
        })
    }
}

#[cfg(test)]
mod tests {
    use vike_model::credential_schema::{FieldOwner, schema_for};

    use super::*;

    /// **The fields this crate reads and writes are `vike_model::credential_schema`'s**: the
    /// account's two required grant fields and its optional `ACCOUNT_ID`, and the venue's two
    /// required application fields — each owned by the owner the schema names, so the GUI's form
    /// writes exactly what the loader reads and the rotation writes back.
    #[test]
    fn the_fields_are_the_schemas() {
        let schema = schema_for(crate::conn::VENUE).expect("ctrader is a schema row");
        assert_eq!(schema.required(FieldOwner::Account), [ACCESS_TOKEN, REFRESH_TOKEN]);
        assert_eq!(schema.required(FieldOwner::Venue), [CLIENT_ID, CLIENT_SECRET]);
        let account_id = schema.field(ACCOUNT_ID).expect("the optional ctid field");
        assert_eq!(account_id.owner, FieldOwner::Account);
        assert!(!account_id.required, "ACCOUNT_ID is discovered at connect when absent");
    }

    /// The tier selects the host and nothing else; `Paper` reads as `Demo`, never as `Live`.
    #[test]
    fn the_tier_selects_the_host() {
        assert_eq!(ctrader_host(AccountTier::Live), "live.ctraderapi.com");
        assert_eq!(ctrader_host(AccountTier::Demo), "demo.ctraderapi.com");
        assert_eq!(ctrader_host(AccountTier::Paper), "demo.ctraderapi.com");
    }
}
