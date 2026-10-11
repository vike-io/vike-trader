//! cTrader OAuth + account config over a caller-supplied vars map — the credential gate
//! (`CtraderConfig::from_vars`; the caller owns the `.env` I/O, per the settings-registry rule
//! that libraries take configuration as parameters). Mirrors `vike_oanda::config`'s
//! HashMap-driven load style (deterministic, no process-env mutation — see `tests/offline/config_env.rs`)
//! plus `vike-bridge-core::credentials`'s `{VENUE}_{TIER}_...` naming convention, adapted to
//! cTrader's OAuth2 shape: an app-level `client_id`/`client_secret` (one Spotware app
//! registration, shared across environments) plus a per-tier `access_token`/`refresh_token`/
//! (optional)`account_id` (one OAuth grant per environment/account). No Python twin — cTrader
//! Open API (https://help.ctrader.com/open-api/).

use std::collections::HashMap;
use std::path::Path;

use vike_bridge_core::credentials::{Environment, TierKeys, account_var};
use vike_model::accounts::account_keys::{AccountLabel, account_key};

use crate::conn::ConnConfig;
use crate::token_store::{TokenKeys, TokenPersist, settings_dir_beside_state_dir};

/// cTrader's TCP endpoint port (the protobuf handshake, both data + exec) — the same for every
/// account once you're pointed at the right host.
pub const CTRADER_PORT: u16 = 5035;

/// The app registration pair's names — tier-less, shared by every tier — read by
/// [`CtraderConfig::from_vars_with_store_for_account`] and named by [`tier_keys`].
const CLIENT_ID_KEY: &str = "CTRADER_CLIENT_ID";
const CLIENT_SECRET_KEY: &str = "CTRADER_CLIENT_SECRET";

/// The `(access_token, refresh_token, account_id)` variable names for one tier STRING — the single
/// composition site for the three per-tier keys, folded through by the loader and by [`tier_keys`].
fn tier_var_names(tier: &str) -> (String, String, String) {
    (
        format!("CTRADER_{tier}_ACCESS_TOKEN"),
        format!("CTRADER_{tier}_REFRESH_TOKEN"),
        format!("CTRADER_{tier}_ACCOUNT_ID"),
    )
}

/// The names ONE account's grant at `env` is written under, for a report of which are missing from
/// a half-written one ([`vike_bridge_core::credentials::TierKeys`]). Label-composed.
///
/// `required` is what [`CtraderConfig::from_vars_for_account`] needs: the app registration pair and
/// the tier's two tokens. `tier_named` is the three keys that BELONG to the tier — the optional
/// `ACCOUNT_ID` included — and NOT the app pair, which every tier shares: a store that holds only
/// the app pair has started no live grant, and a report that called the live tokens "missing" from
/// it would accuse an operator who never wrote one.
#[must_use]
pub fn tier_keys(env: Environment, label: &AccountLabel) -> Vec<TierKeys> {
    let compose = |n: &str| account_key(n, label);
    let (access, refresh, account) = tier_var_names(env.as_str());
    let (access, refresh, account) = (compose(&access), compose(&refresh), compose(&account));
    vec![TierKeys {
        required: vec![
            compose(CLIENT_ID_KEY),
            compose(CLIENT_SECRET_KEY),
            access.clone(),
            refresh.clone(),
        ],
        tier_named: vec![access, refresh, account],
    }]
}

/// Host for a given [`Environment`]: `demo.ctraderapi.com` (Demo, and Sim — cTrader has no
/// separate paper tier; Demo already IS the paper account) or `live.ctraderapi.com` (Live).
pub fn ctrader_host(env: Environment) -> &'static str {
    match env {
        Environment::Live => "live.ctraderapi.com",
        Environment::Demo | Environment::Sim => "demo.ctraderapi.com",
    }
}

/// Gated cTrader connection config: `client_id`/`client_secret` (the Spotware app registration,
/// shared across tiers) plus a per-tier OAuth token pair. `account_id` is optional — when absent,
/// `conn::connect_and_auth` discovers it via `GetAccountListByAccessToken` (first non-live account
/// only; see `conn.rs`'s NEVER-default-to-live safety note). `Debug` is manually implemented to
/// redact `client_secret`/`access_token`/`refresh_token` — never let a secret leak into a log line
/// or panic message (mirrors `conn::ConnConfig`/`oauth::Token`'s manual `Debug`).
#[derive(Clone)]
pub struct CtraderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<i64>,
    pub host: String,
    pub port: u16,
    /// Where a REFRESHED grant is written back — the credential store and this tier's two token
    /// keys, when the caller resolved a project.
    ///
    /// `None` (every caller of the plain [`CtraderConfig::from_vars`] — tests, the catalog probe)
    /// keeps the historical behaviour EXACTLY: a refresh lives as long as the process and nothing
    /// is written. See [`crate::token_store`] for why the store is the home.
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
            // A PATH and two KEY NAMES — no secret. Shown because "is rotation armed on this
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

    /// Read from a caller-supplied var map (an already-loaded workspace `.env` in production —
    /// `crate::mount`'s `CtraderVenueMount` and `CtraderCatalog::new` both pass one — or a literal
    /// map in tests). `None` when any REQUIRED field (`CTRADER_CLIENT_ID`/`_SECRET`,
    /// `CTRADER_{tier}_ACCESS_TOKEN`/`_REFRESH_TOKEN`) is absent or blank — the live gate;
    /// `account_id` is optional (discovered at connect time when unset, or when present but
    /// unparseable as an `i64`).
    pub fn from_vars(env: Environment, vars: &HashMap<String, String>) -> Option<Self> {
        Self::from_vars_with_store(env, vars, None)
    }

    /// [`CtraderConfig::from_vars`] for ONE NAMED ACCOUNT, with no rotation home — the pure probe
    /// `crate::mount`'s `CtraderVenueMount::resolve` calls. See
    /// [`CtraderConfig::from_vars_with_store_for_account`] for the whole of what `label` changes.
    pub fn from_vars_for_account(
        env: Environment,
        label: &AccountLabel,
        vars: &HashMap<String, String>,
    ) -> Option<Self> {
        Self::from_vars_with_store_for_account(env, label, vars, None)
    }

    /// [`CtraderConfig::from_vars`] plus the ROTATION HOME: `state_dir` is `<project>/settings/state`
    /// as the composition root's boot resolved it, from which the settings directory is DERIVED
    /// (`crate::token_store::settings_dir_beside_state_dir`) — never re-walked here, per the root
    /// `CLAUDE.md`'s one-walk-decides rule.
    ///
    /// # Precedence: the store is the ONE home, so there is none to decide
    ///
    /// The grant is read from `vars` (the loaded store) and written back to the same store's same
    /// two keys. There is no second source of truth to rank against it — deliberately. The
    /// authorize bin's `settings/state/ctrader_token.json` stays that tool's own output record and
    /// is NOT read: two files that can each answer "what is the current grant" is the ambiguity
    /// this design removes, not a redundancy it wants.
    ///
    /// `state_dir` of `None` yields `token_persist: None` — byte-identical to the pre-rotation
    /// behaviour, which is what every test and the catalog probe get.
    pub fn from_vars_with_store(
        env: Environment,
        vars: &HashMap<String, String>,
        state_dir: Option<&Path>,
    ) -> Option<Self> {
        Self::from_vars_with_store_for_account(env, &AccountLabel::Default, vars, state_dir)
    }

    /// [`CtraderConfig::from_vars`] for ONE NAMED ACCOUNT — `CTRADER_{TIER}_{SUFFIX}__{LABEL}`, and
    /// `CTRADER_CLIENT_ID__{LABEL}` / `CTRADER_CLIENT_SECRET__{LABEL}` for the app pair.
    ///
    /// ⚠ **[`AccountLabel::Default`] is byte-identically [`CtraderConfig::from_vars`]**, reached
    /// through it — the grammar returns every key name unchanged for that account, the ROTATION
    /// KEYS included.
    ///
    /// # ⚠ The app registration is labelled too, with NO fallback — and that costs a duplicated line
    ///
    /// `CTRADER_CLIENT_ID`/`_CLIENT_SECRET` are an APP-level Spotware registration, shared across
    /// tiers, so "fall back to the unlabelled pair" is a tempting convenience and it is refused
    /// here. One rule — *an account is configured by its own keys, all of them* — is what makes
    /// this loader auditable by reading it: there is no name in this function that can resolve to
    /// another account's value, so no reviewer has to reason about which of two rungs a given box
    /// lands on. An operator running two cTrader accounts through one app registration writes the
    /// pair twice. The cost of the alternative is that the one exception would have to be
    /// re-justified at every future call site.
    ///
    /// It is not a live-money hazard in either direction: the app pair cannot SELECT an account —
    /// `access_token`/`refresh_token` do, and those are strictly per-account — so a missing
    /// `__{LABEL}` app pair yields `None` and the account stays PAPER, which is the ordinary live
    /// gate rather than a mis-trade.
    ///
    /// # ⚠ The ROTATION target moves with the account
    ///
    /// [`TokenKeys::for_account`] is what [`TokenPersist::keys`] carries, so a grant refreshed on
    /// account `ALT` is written back to `CTRADER_{TIER}_ACCESS_TOKEN__ALT` — never over the default
    /// account's grant. A `TokenKeys::for_env` here would have made every refresh on a second
    /// account silently overwrite the FIRST account's tokens in the shared store, which is the one
    /// way this venue could corrupt a stored credential rather than merely mis-read one.
    pub fn from_vars_with_store_for_account(
        env: Environment,
        label: &AccountLabel,
        vars: &HashMap<String, String>,
        state_dir: Option<&Path>,
    ) -> Option<Self> {
        let get = |k: &str| account_var(vars, k, label).map(str::to_string).unwrap_or_default();
        let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };

        let client_id = non_empty(get(CLIENT_ID_KEY))?;
        let client_secret = non_empty(get(CLIENT_SECRET_KEY))?;

        let tier = env.as_str(); // "DEMO" / "LIVE" / "SIM"
        let (access_token_k, refresh_token_k, account_id_k) = tier_var_names(tier);
        let access_token = non_empty(get(&access_token_k))?;
        let refresh_token = non_empty(get(&refresh_token_k))?;
        let account_id = non_empty(get(&account_id_k)).and_then(|s| s.parse().ok());

        // ONE resolution, TWO derivations: the store to write and the state directory the change
        // journal hangs off. Both out of the caller's `state_dir`, so a rotation cannot record
        // itself into one project's ledger while writing another project's store.
        let token_persist = state_dir.and_then(|state| {
            settings_dir_beside_state_dir(state).map(|settings_dir| TokenPersist {
                settings_dir,
                keys: TokenKeys::for_account(env, label),
                state_dir: state.to_path_buf(),
            })
        });

        Some(CtraderConfig {
            client_id,
            client_secret,
            access_token,
            refresh_token,
            account_id,
            host: ctrader_host(env).to_string(),
            port: CTRADER_PORT,
            token_persist,
        })
    }
}
