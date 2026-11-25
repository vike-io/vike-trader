//! cTrader OAuth + account config over the started ACCOUNT's credentials — the credential gate
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
//! The field words are `crates/vike-venues/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS` ctrader
//! row, held equal to it by `the_fields_are_the_schemas`. The TIER is the account row's
//! (`AdapterInputs::tier`), and the only thing it selects here is the addresses ([`ctrader_host`]):
//! the venue's `venue_url` rows at the tier's type, never a literal (decision 0265). No
//! Python twin — cTrader Open API (https://help.ctrader.com/open-api/).

use std::path::Path;

use vike_model::AccountTier;
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlSocket, UrlType, VenueUrls};

use crate::transport::connection::ConnectionConfig;
use crate::transport::connection::grant::SharedGrant;
use crate::transport::token_store::{TokenPersist, settings_dir_beside_state_dir};

/// The account's OAuth ACCESS token field — required; rotated by `crate::transport::token_store`'s `persist`.
pub const ACCESS_TOKEN: &str = "ACCESS_TOKEN";

/// The account's OAuth REFRESH token field — required; cTrader rotates it on every refresh, so
/// `crate::transport::token_store`'s `persist` writes it back beside [`ACCESS_TOKEN`].
pub const REFRESH_TOKEN: &str = "REFRESH_TOKEN";

/// The account's cTrader trading-account id (`ctid`) field — OPTIONAL: when absent it is
/// discovered at connect. It is also the field `crate::adapter`'s book identity names.
pub const ACCOUNT_ID: &str = "ACCOUNT_ID";

/// The VENUE's application field: the Spotware app registration's client id.
pub const CLIENT_ID: &str = "CLIENT_ID";

/// The VENUE's application field: the Spotware app registration's client secret.
pub const CLIENT_SECRET: &str = "CLIENT_SECRET";

/// **cTrader's two addresses for one account**: the Open API socket (data and exec ride one
/// protobuf/TLS connection) and the OAuth2 base its grant is refreshed at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CtraderUrls {
    /// The socket's host: the host half of the `ctrader <type> trade socket` row.
    pub host: String,
    /// The socket's port: the port half of the same row.
    pub port: u16,
    /// The `ctrader <type> login rest` row; the code appends `/apps/auth` and `/apps/token`.
    pub oauth_base: String,
}

/// **The addresses for an account's tier**, out of the venue's `venue_url` rows `urls` at
/// `UrlType::of_tier(tier)`: `Paper` reads as `Demo` (cTrader has no separate paper tier), `Live` as
/// `live`, and a demo account never falls back to a live row (`VenueUrls::url`).
///
/// # Errors
/// The refusal's words, naming the row: a row that is missing, a demo row equal to the live one, or
/// a socket row that is not `host:port` by the rule its write is judged by (`UrlSocket::parse`).
/// The account is then NOT RUNNING.
pub fn ctrader_host(urls: &VenueUrls, tier: AccountTier) -> Result<CtraderUrls, String> {
    let ty = UrlType::of_tier(tier);
    let socket =
        urls.url(ty, UrlChannel::Trade, UrlProtocol::Socket, "").map_err(|e| e.to_string())?;
    let oauth_base = ctrader_oauth_base(urls, tier)?;
    let UrlSocket { host, port } = UrlSocket::parse(socket).map_err(|_| {
        crate::refusals::CtraderStartRefusal::SocketRowNotHostPort {
            ty: ty.to_string(),
            socket: socket.to_string(),
        }
        .to_string()
    })?;
    Ok(CtraderUrls { host, port, oauth_base })
}

/// **The OAuth2 base of an account's tier**: the `ctrader <type> login rest` row of `urls` at
/// `UrlType::of_tier(tier)`, where a grant is refreshed ([`ctrader_host`]) and the authorize tool's
/// browser flow and code exchange go.
///
/// # Errors
/// The refusal's words, naming the row.
pub fn ctrader_oauth_base(urls: &VenueUrls, tier: AccountTier) -> Result<String, String> {
    urls.url(UrlType::of_tier(tier), UrlChannel::Login, UrlProtocol::Rest, "")
        .map(str::to_string)
        .map_err(|e| e.to_string())
}

/// Gated cTrader connection config: `client_id`/`client_secret` (the venue's Spotware app
/// registration, shared by every account) plus the account's OAuth token pair. `account_id` is
/// optional — when absent, `transport::connection::connect_and_auth` discovers it via
/// `GetAccountListByAccessToken` (first non-live account only; see `transport/connection/mod.rs`'s NEVER-default-to-live
/// safety note). `client_secret`/`access_token`/`refresh_token` are [`Secret`]s, so `Debug` prints
/// each redacted and never lets one into a log line or panic message; the rest (`token_persist` too:
/// a PATH and an account row id, the question "is rotation armed on this start") prints as it is.
/// `Debug` is written by hand because [`CtraderConfig::fx_clock`] is a clock with none.
#[derive(Clone)]
pub struct CtraderConfig {
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub access_token: Secret<String>,
    pub refresh_token: Secret<String>,
    pub account_id: Option<i64>,
    pub host: String,
    pub port: u16,
    /// The OAuth2 base a refresh is sent to ([`CtraderUrls::oauth_base`]).
    pub oauth_base: String,
    /// Where a REFRESHED grant is written back — the credential store and the account row whose
    /// grant this is, when the caller resolved a project.
    ///
    /// `None` (every caller of the plain [`CtraderConfig::from_credentials`] — tests, the catalog
    /// probe) keeps the historical behaviour EXACTLY: a refresh lives as long as the process and
    /// nothing is written. See [`crate::transport::token_store`] for why the store is the home.
    pub token_persist: Option<TokenPersist>,
    /// The settings database's `account.id` the grant was READ from (`AccountCredentials::
    /// account_id`) — kept so [`CtraderConfig::with_rotation_home`] can name the account to write a
    /// rotation back to after the credentials are no longer in hand (the data feed's plan is made
    /// from the account snapshot long before its socket opens). ⚠ Not cTrader's trading-account id
    /// ([`CtraderConfig::account_id`]).
    pub account_row: Option<i64>,
    /// **The FX login gate's clock**, handed to every socket of this config ([`ConnectionConfig::fx_clock`]): the
    /// gatewayhub sets it on each config it builds or hands a start (`AdapterInputs::fx_clock`). `None` (the loaders'
    /// own answer, so every tool, smoke and test that does not care) is no gate.
    pub fx_clock: Option<std::sync::Arc<dyn vike_model::Clock + Send + Sync>>,
}

impl std::fmt::Debug for CtraderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtraderConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("account_id", &self.account_id)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("oauth_base", &self.oauth_base)
            .field("token_persist", &self.token_persist)
            .field("account_row", &self.account_row)
            .field("fx_clock", &self.fx_clock.is_some())
            .finish()
    }
}

impl CtraderConfig {
    /// Build the [`ConnectionConfig`] `transport::connection::connect_and_auth`/`connect_and_auth_execution` need, wired with
    /// this config's `refresh_token` so the actor's reconnect/token-refresh path (Task 6) has what
    /// it needs from the very first connect. Always TLS-on (`no_tls: false`) — this is the
    /// production entry point; `ConnectionConfig::for_test` is the only sanctioned way to speak
    /// plaintext, and only against the in-process fake server.
    ///
    /// ⚠ **Every config this returns for one grant shares ONE rotating pair** with its siblings in
    /// this process: it JOINS the process's [`SharedGrant`] for
    /// this refresh token (creating it when it is the first), so the exec start, the data feed and
    /// the reconcile client — each built from its own snapshot of the account — rotate, adopt and
    /// save through one cell instead of each spending the refresh token alone. The module doc of
    /// `crate::transport::connection::grant` carries the failure this ends.
    pub fn to_connection_config(&self) -> ConnectionConfig {
        let mut connection_config = ConnectionConfig::new(
            self.host.clone(),
            self.port,
            self.client_id.clone(),
            self.client_secret.expose().clone(),
            self.access_token.expose().clone(),
        );
        connection_config.account_id = self.account_id;
        connection_config.oauth_base = self.oauth_base.clone();
        connection_config.refresh_token = Some(self.refresh_token.clone());
        connection_config.token_persist = self.token_persist.clone();
        let (access, refresh) = (self.access_token.expose(), self.refresh_token.expose());
        let (grant, seen) = SharedGrant::join(access, refresh, self.token_persist.as_ref());
        connection_config.grant = Some(grant);
        connection_config.grant_seen = seen;
        connection_config.fx_clock = self.fx_clock.clone();
        connection_config
    }

    /// This config with the ROTATION HOME `state_dir` names (`<project>/settings/state`, as the
    /// composition root's boot resolved it — the SAME directory the exec start is handed), for the
    /// account row the grant was read from ([`CtraderConfig::account_row`]). What
    /// [`CtraderConfig::from_credentials_with_store`] does in one step; this is the second step for
    /// a config that was planned from the account snapshot before a project directory was in hand
    /// (the data feed's), so its rotations are written back through the same `persist` instead of
    /// living only as long as the process. A `state_dir` with no parent yields no home.
    #[must_use]
    pub fn with_rotation_home(mut self, state_dir: &Path) -> Self {
        self.token_persist =
            settings_dir_beside_state_dir(state_dir).map(|settings_dir| TokenPersist {
                settings_dir,
                account_row: self.account_row,
                state_dir: state_dir.to_path_buf(),
            });
        self
    }

    /// Read from one account's credentials (`crate::adapter`'s `VenueAdapterCtrader` hands
    /// `AdapterInputs::credentials`, a smoke `vike_secrets::smoke_directory`'s, a test a fixture's),
    /// dialled at `endpoints` (the account's tier's rows, [`ctrader_host`]). `None` when any REQUIRED
    /// field is absent or blank — the venue's [`CLIENT_ID`] / [`CLIENT_SECRET`] or the account's
    /// [`ACCESS_TOKEN`] / [`REFRESH_TOKEN`] — the live gate; [`ACCOUNT_ID`] is optional (discovered at
    /// connect time when unset, or when present but unparseable as an `i64`). The addresses never
    /// decide presence.
    pub fn from_credentials(
        endpoints: &CtraderUrls,
        credentials: &AccountCredentials<'_>,
    ) -> Option<Self> {
        Self::from_credentials_with_store(endpoints, credentials, None)
    }

    /// [`CtraderConfig::from_credentials`] plus the ROTATION HOME: `state_dir` is
    /// `<project>/settings/state` as the composition root's boot resolved it, from which the
    /// settings directory is DERIVED (`crate::transport::token_store::settings_dir_beside_state_dir`) — never
    /// re-walked here, per the root `CLAUDE.md`'s one-walk-decides rule.
    ///
    /// # Precedence: the store is the ONE home, so there is none to decide
    ///
    /// The grant is read from the account's fields and written back to the SAME account's same two
    /// fields. There is no second source of truth to rank against it — deliberately. The authorize
    /// bin writes no token file (since 2026-10-31 the grant goes into the store and nowhere else),
    /// and a leftover `settings/state/ctrader_token.json` is NOT read: two files that can each
    /// answer "what is the current grant" is the ambiguity this design removes.
    ///
    /// # ⚠ The ROTATION target is the account the grant was read from
    ///
    /// [`TokenPersist::account_row`] is `credentials.account_id()` — the `account.id` the fields
    /// belong to — so a grant refreshed on one account is written back to THAT account's two fields
    /// and never over another account's grant. Credentials handed with no account row still load
    /// (a test, a tool), and their rotation is REFUSED at the write rather than landing anywhere
    /// (`crate::transport::token_store`'s `persist`).
    ///
    /// The application pair is the VENUE's: every account reads the same one. It cannot select an
    /// account — `access_token`/`refresh_token` do, and those are strictly per-account.
    ///
    /// `state_dir` of `None` yields `token_persist: None` — byte-identical to the pre-rotation
    /// behaviour, which is what every test and the catalog probe get.
    pub fn from_credentials_with_store(
        endpoints: &CtraderUrls,
        credentials: &AccountCredentials<'_>,
        state_dir: Option<&Path>,
    ) -> Option<Self> {
        let client_id = credentials.venue_get(CLIENT_ID)?.to_string();
        let client_secret = Secret::new(credentials.venue_get(CLIENT_SECRET)?.to_string());
        let access_token = Secret::new(credentials.get(ACCESS_TOKEN)?.to_string());
        let refresh_token = Secret::new(credentials.get(REFRESH_TOKEN)?.to_string());
        let account_id = credentials.get(ACCOUNT_ID).and_then(|s| s.parse().ok());

        let config = CtraderConfig {
            client_id,
            client_secret,
            access_token,
            refresh_token,
            account_id,
            host: endpoints.host.clone(),
            port: endpoints.port,
            oauth_base: endpoints.oauth_base.clone(),
            token_persist: None,
            account_row: credentials.account_id(),
            fx_clock: None,
        };
        // ONE resolution, TWO derivations: the store to write and the state directory the change
        // journal hangs off. Both out of the caller's `state_dir` (in `with_rotation_home`), so a
        // rotation cannot record itself into one project's ledger while writing another project's
        // store.
        Some(match state_dir {
            Some(state) => config.with_rotation_home(state),
            None => config,
        })
    }
}

#[cfg(test)]
mod tests {
    use vike_venues::credential_schema::{KeyFieldOwner, schema_for};

    use super::*;

    /// **The fields this crate reads and writes are `vike_venues::credential_schema`'s**: the
    /// account's two required grant fields and its optional `ACCOUNT_ID`, and the venue's two
    /// required application fields — each owned by the owner the schema names, so the GUI's form
    /// writes exactly what the loader reads and the rotation writes back.
    #[test]
    fn the_fields_are_the_schemas() {
        let schema =
            schema_for(crate::transport::connection::VENUE).expect("ctrader is a schema row");
        assert_eq!(schema.required(KeyFieldOwner::Account), [ACCESS_TOKEN, REFRESH_TOKEN]);
        assert_eq!(schema.required(KeyFieldOwner::Venue), [CLIENT_ID, CLIENT_SECRET]);
        let account_id = schema.field(ACCOUNT_ID).expect("the optional ctid field");
        assert_eq!(account_id.owner, KeyFieldOwner::Account);
        assert!(!account_id.required, "ACCOUNT_ID is discovered at connect when absent");
    }

    /// The release seed's row of `(type, channel, protocol)` for ctrader.
    fn seed_row(ty: UrlType, channel: UrlChannel, protocol: UrlProtocol) -> &'static str {
        vike_venues::venue_urls::seed_url(
            crate::transport::connection::VENUE,
            ty,
            channel,
            protocol,
            "",
        )
        .expect("the seed holds ctrader's rows")
    }

    /// The tier selects the ROWS and nothing else; `Paper` reads as `Demo`, never as `Live`. The
    /// socket is the trade socket row split `host:port`, the OAuth base the login row as it stands.
    #[test]
    fn the_tier_selects_the_rows() {
        let seed = VenueUrls::seed(crate::transport::connection::VENUE);
        for (tier, ty) in [
            (AccountTier::Live, UrlType::Live),
            (AccountTier::Demo, UrlType::Demo),
            (AccountTier::Paper, UrlType::Demo),
        ] {
            let e = ctrader_host(&seed, tier).expect("the seed answers every tier");
            assert_eq!(
                format!("{}:{}", e.host, e.port),
                seed_row(ty, UrlChannel::Trade, UrlProtocol::Socket)
            );
            assert_eq!(e.oauth_base, seed_row(ty, UrlChannel::Login, UrlProtocol::Rest));
        }
        assert_ne!(
            ctrader_host(&seed, AccountTier::Demo).map(|e| e.host),
            ctrader_host(&seed, AccountTier::Live).map(|e| e.host),
            "a demo account never dials the live host"
        );
    }

    /// No row is no address, and a socket row that is not `host:port` is refused by name: the
    /// account does not run on a guess.
    #[test]
    fn a_missing_or_malformed_row_is_refused_by_name() {
        let none = ctrader_host(&VenueUrls::default(), AccountTier::Demo).expect_err("no rows");
        assert!(none.contains("trade socket"), "{none}");
        // The rule the write judges a socket row by (`UrlSocket::parse`): an empty host, port 0 and
        // a scheme are refused as well as a row with no port.
        for bad in ["nohost", ":5035", "h:0", "tcp://h:5035"] {
            let odd = VenueUrls::from_rows(
                crate::transport::connection::VENUE,
                [
                    ("ctrader", UrlType::Demo, UrlChannel::Trade, UrlProtocol::Socket, "", bad),
                    (
                        "ctrader",
                        UrlType::Demo,
                        UrlChannel::Login,
                        UrlProtocol::Rest,
                        "",
                        "https://x",
                    ),
                ],
            );
            let why = ctrader_host(&odd, AccountTier::Demo).expect_err("not host:port");
            let want = crate::refusals::CtraderStartRefusal::SocketRowNotHostPort {
                ty: UrlType::Demo.to_string(),
                socket: bad.to_string(),
            };
            assert_eq!(why, want.to_string());
        }
    }
}
