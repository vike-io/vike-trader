//! Per-venue API credentials, read from the credential store (the settings database).
//!
//! [`load_credentials_from`] returns `None` unless the venue's REQUIRED credential set is fully
//! present: that absence IS the live gate (no credentials ⇒ the venue stays paper).
//! `names_for_prefix` is the SINGLE naming site. HARD: a secret never reaches `Debug`, `Display`
//! or any log line.
//!
//! # More than one account per venue
//!
//! [`load_credentials_for_account`] is [`load_credentials_from`] with the account named, and
//! [`account_var`] is the same door for the BESPOKE key shapes this module's `_API_*` grid does not
//! cover (`OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`, the FX
//! `_USER`/`_PASSWORD` pair). Both compose the name through
//! [`vike_model::accounts::account_keys::account_key`], which appends `__{LABEL}` after the WHOLE
//! key and never re-parses it, so a bespoke suffix needs no table, and the
//! [`AccountLabel::Default`] account's names are the unlabelled ones, unchanged.
//!
//! ⚠ **The LABELLED half of the grid is NOT enumerable, structurally.** The label is
//! operator-chosen, so `vike_model::credential_keys::credential_keys` (the enumeration
//! `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
//! `every_generated_key_is_declared` demands rows for) covers the DEFAULT account only. A labelled
//! key's disclosure surface is the store itself, through
//! `vike_model::accounts::account_keys::accounts_in_store`.
//!
//! ⚠ **The key names this module reads are ENUMERABLE, and that is load-bearing.** Every name is
//! composed from [`vike_model::credential_keys`]' suffix/tier tables, never spelled at the call
//! site: a `format!`-built map key appears as no literal, so the settings registry sees it only
//! because the enumeration and the read are one set. That module's doc owns the blind spot;
//! `generated_key_grid_tests` holds the two equal by folding `names_for_prefix` over the roster.
//!
//! ⚠ **"Required" is PER-VENUE.** key+secret is the required set for binance/bybit/deribit; OKX
//! also requires `_API_PASSPHRASE`, because its signer sends one on every request
//! ([`venue_passphrase`](mod@crate::venue_passphrase)). Without that row an OKX store holding two
//! of the three would mount LIVE and fail every signed request at the venue.
//! [`missing_required_passphrase`] is how a mount names the missing key.
//!
//! # One store: the settings database
//!
//! `vike_secrets` owns the store and its resolution; this module is the workspace-facing door:
//!
//! | function | who calls it |
//! |---|---|
//! | [`load_workspace_secrets_from_env`] | **the composition roots**, out of the one `std::env::vars()` sweep they already own |
//! | [`load_workspace_secrets_at`] | a caller that has already isolated the settings-directory override |
//! | [`try_load_workspace_secrets_at`] | the same, but a hard error instead of an empty map (`vike-cli secrets`) |
//!
//! A caller with nothing to pass hands `None` to [`load_workspace_secrets_at`]: the walk answers.
//! The SILENT reader — the same map with no `tracing` findings — is
//! `vike_secrets::load_project_secrets`, named by its own path; this module re-exports none of
//! `vike_secrets`' loaders.
//!
//! All return the SAME `HashMap<String, String>` shape, so every consumer
//! (`load_credentials_from`, `attribution_code_from`, every venue `config.rs` loader) is written
//! against one shape. No `venue_setting` row reaches it: a venue reader takes those rows as
//! `vike_secrets::venue_setting::VenueSettings`, and a credential row under a setting's old name is
//! read by nothing.
//!
//! A box with no settings database has NO credentials (an empty map ⇒ every venue stays paper).
//!
//! ⚠ Nothing in this workspace deletes, moves or rewrites the store. It is the user's only copy of
//! live venue credentials.

use std::collections::HashMap;

use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{
    API_KEY_SUFFIX, API_PASSPHRASE_SUFFIX, API_SECRET_SUFFIX, BROKER_CODE_SUFFIX,
    BUILDER_CODE_SUFFIX, attribution_key,
};

use crate::venue_passphrase::venue_passphrase;

pub use vike_secrets::{SecretsError, Source};
// The scoped read's and the `account` table's vocabulary: the consumers above this crate link no
// `vike-secrets` of their own, and the loaders below return these types. Public-API surface, not a
// move shim.
pub use vike_secrets::{Account, AccountKeys, Accounts, NoAccountTable};
pub use vike_secrets::{KeyScope, Lookup, ScopedSecrets, UndeclaredKey};

mod classify;
pub use classify::classify_credential_name;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Environment {
    Sim,
    Demo,
    /// Real account, real money (venue-neutral term; crypto venues call this "mainnet").
    Live,
}

impl Environment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Environment::Sim => "SIM",
            Environment::Demo => "DEMO",
            Environment::Live => "LIVE",
        }
    }

    /// The `venue_setting` tier this credential tier reads its settings at — `Sim` is the `paper`
    /// tier (its key token is `SIM`, `vike_secrets`' `key_token_of_account_tier`).
    #[must_use]
    pub fn setting_tier(self) -> vike_secrets::venue_setting::SettingTier {
        use vike_secrets::venue_setting::SettingTier;
        match self {
            Environment::Sim => SettingTier::Paper,
            Environment::Demo => SettingTier::Demo,
            Environment::Live => SettingTier::Live,
        }
    }
}

#[derive(Clone)]
pub struct Credentials {
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail =
            if self.api_key.len() >= 4 { &self.api_key[self.api_key.len() - 4..] } else { "" };
        write!(
            f,
            "Credentials(api_key=***{tail}, passphrase={})",
            if self.passphrase.is_some() { "set" } else { "None" }
        )
    }
}

/// The three names one `{VENUE}_{TIER}` prefix yields — the SINGLE naming site, spelling no suffix
/// of its own: the suffixes are [`vike_model::credential_keys`]' constants, so the grid the
/// settings registry enumerates and the grid this loader reads are one table.
/// `generated_key_grid_tests::the_loader_reads_exactly_the_enumerated_credential_grid` asserts the
/// two are the same SET.
fn names_for_prefix(prefix: &str) -> (String, String, String) {
    (
        format!("{prefix}{API_KEY_SUFFIX}"),
        format!("{prefix}{API_SECRET_SUFFIX}"),
        format!("{prefix}{API_PASSPHRASE_SUFFIX}"),
    )
}

/// The same three names, for ONE ACCOUNT — [`names_for_prefix`] with
/// [`vike_model::accounts::account_keys::account_key`] applied to each.
///
/// ⚠ **[`AccountLabel::Default`] returns [`names_for_prefix`]'s tuple UNCHANGED** (`account_key`
/// returns its input for the default account). That is the grammar's contract, and it keeps a
/// single-account box's load path the unlabelled names —
/// `account_tests::the_default_account_composes_the_historical_names` folds it over the roster.
fn names_for_account(prefix: &str, label: &AccountLabel) -> (String, String, String) {
    let (key, secret, passphrase) = names_for_prefix(prefix);
    (account_key(&key, label), account_key(&secret, label), account_key(&passphrase, label))
}

/// **Read ONE credential of ONE account out of a var map** — the door for every credential shape
/// this module's own `{VENUE}_{TIER}_API_*` grid does NOT cover.
///
/// `base` is the unlabelled key, whatever its shape: `OANDA_DEMO_ACCOUNT_ID`, `IG_LIVE_IDENTIFIER`,
/// `ASTER_LIVE_PRIVATE_KEY`, `FXCM_DEMO_USER`. The label is appended after the WHOLE of `base`
/// ([`vike_model::accounts::account_keys::account_key`]), so no bespoke suffix needs a table here.
///
/// Returns the TRIMMED value, or `None` when the key is absent or blank — the "blank is absent"
/// rule [`load_credentials_from`] applies.
///
/// ⚠ **There is NO fallback to the unlabelled key.** Falling back would sign account `ALT`'s orders
/// with the default account's key.
#[must_use]
pub fn account_var<'a>(
    vars: &'a HashMap<String, String>,
    base: &str,
    label: &AccountLabel,
) -> Option<&'a str> {
    vars.get(&account_key(base, label)).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Read credentials from a credential map; `None` when the venue's REQUIRED credential set is not
/// fully present with non-blank values (the live gate). One tier spelling per [`Environment`]
/// ([`Environment::as_str`]) and no fallback: a `{VENUE}_MAINNET_*` set is an unknown name, so a
/// `Live` load that finds only one answers `None`.
///
/// The required set is `_API_KEY` + `_API_SECRET`, **plus `_API_PASSPHRASE` on a venue whose
/// [`venue_passphrase`] row is
/// [`Required`](crate::venue_passphrase::PassphraseNeed::Required)** (OKX). Unusable credentials
/// must resolve exactly like absent ones: otherwise the venue mounts LIVE and every signed request
/// is rejected at the venue.
///
/// SILENT by construction: it is called per venue per tier on hot paths
/// (`vike_connections::credential_status` re-runs the whole grid every frame the GUI's Connections
/// tool is open), so it logs nothing. [`missing_required_passphrase`] returns the finding as DATA,
/// and the venue's own mount (`crates/bridges/okx/src/mount.rs`) turns it into a log line.
pub fn load_credentials_from(
    venue: &str,
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    load_credentials_for_account(venue, env, &AccountLabel::Default, vars)
}

/// [`load_credentials_from`] for a NAMED account: the same required set, passphrase rule and
/// silence. The ONLY difference is the names read,
/// `{VENUE}_{TIER}{SUFFIX}__{LABEL}` (`names_for_account`); [`AccountLabel::Default`] is
/// byte-identically [`load_credentials_from`].
///
/// ⚠ **An INCOMPLETE labelled account is an ABSENT one**, silently, exactly as an incomplete
/// default account is: the live gate's vocabulary is `Some`/`None`, so there is no error, no log
/// and no partial `Credentials`.
///
/// ⚠ **No fallback to the unlabelled key** — see [`account_var`] for why.
pub fn load_credentials_for_account(
    venue: &str,
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    load_for_prefix(venue, &prefix_for(venue, env.as_str()), label, vars).ok()
}

/// The `{VENUE}_{TIER}` half of a credential key, as ONE site, so the equivalence gate folds the
/// loader's OWN composition (`prefix_for` + [`names_for_prefix`]) rather than a hand copy of it.
fn prefix_for(venue: &str, tier: &str) -> String {
    format!("{}_{tier}", venue.to_uppercase())
}

/// **The NAME of the credential a half-configured venue is missing**, or `None` when the venue is
/// fully configured or not configured at all.
///
/// `Some(name)` means: `_API_KEY` and `_API_SECRET` are present and non-blank, the
/// [`venue_passphrase`] row is [`Required`](crate::venue_passphrase::PassphraseNeed::Required),
/// and the passphrase is unset or blank — so [`load_credentials_from`] returned `None` and the
/// venue stays PAPER while the store LOOKS configured. That is the case a mount must say out loud.
///
/// Returns the variable NAME, never a value. PURE and log-free (see [`load_credentials_from`] on
/// the per-frame callers).
#[must_use]
pub fn missing_required_passphrase(
    venue: &str,
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<String> {
    missing_required_passphrase_for_account(venue, env, &AccountLabel::Default, vars)
}

/// [`missing_required_passphrase`] for a NAMED account. The name it returns is the LABELLED one
/// (`OKX_DEMO_API_PASSPHRASE__ALT`): the variable the operator has to write, never the default
/// account's, which would send them to edit the account that works.
#[must_use]
pub fn missing_required_passphrase_for_account(
    venue: &str,
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<String> {
    match load_for_prefix(venue, &prefix_for(venue, env.as_str()), label, vars) {
        Err(NoCreds::Passphrase { name }) => Some(name),
        Ok(_) | Err(NoCreds::KeyOrSecret) => None,
    }
}

/// **The key names ONE spelling of one tier's credential set is written under, for ONE account** —
/// what a mount needs to say WHICH keys of a half-written set are missing, by name.
///
/// A loader answers `Some`/`None` only, so the report needs a second reader of the SAME names;
/// every bridge builds its `TierKeys` from the function that composes its loader's names, so the
/// report and the loader cannot disagree about what a complete set is.
///
/// Both lists are the LABEL-COMPOSED names (the variable an operator writes for THIS account).
/// Names only: [`Self::missing_in`] reads presence and never returns, stores or prints a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierKeys {
    /// Every name the loader REQUIRES at this spelling of the tier, in the order it reads them.
    pub required: Vec<String>,
    /// The names that BELONG to this tier, whose presence says a set was started here.
    ///
    /// ⚠ A TIER-LESS key is not among them, which is why this is a second list: cTrader's app
    /// registration pair is required by every tier and names none, so a store holding only that
    /// pair has started no live set, and reporting its live tokens "missing" would be false.
    pub tier_named: Vec<String>,
}

impl TierKeys {
    /// The REQUIRED names this store lacks (absent or blank), in the loader's order — or `None`
    /// when no tier-named key is present at all (nothing was started: the ordinary, silent
    /// unconfigured state) or when nothing required is missing (the set is complete).
    #[must_use]
    pub fn missing_in(&self, vars: &HashMap<String, String>) -> Option<Vec<String>> {
        let present = |name: &String| vars.get(name).is_some_and(|v| !v.trim().is_empty());
        if !self.tier_named.iter().any(present) {
            return None;
        }
        let missing: Vec<String> = self.required.iter().filter(|n| !present(n)).cloned().collect();
        (!missing.is_empty()).then_some(missing)
    }
}

/// [`TierKeys`] for a venue read through [`load_credentials_for_account`] — the ONE tier spelling
/// the loader reads for `env`. A `Vec` because `venue_mount::LiveTierSet::read` takes the slice
/// every bridge's own `tier_keys` returns.
///
/// `_API_KEY` and `_API_SECRET` are always required; `_API_PASSPHRASE` joins them exactly when the
/// [`venue_passphrase`] row says the venue's signer needs one (`load_for_prefix`'s rule) and is
/// otherwise a tier-named key that may be absent.
#[must_use]
pub fn tier_keys_for_account(venue: &str, env: Environment, label: &AccountLabel) -> Vec<TierKeys> {
    let (key, secret, passphrase) = names_for_account(&prefix_for(venue, env.as_str()), label);
    let mut required = vec![key.clone(), secret.clone()];
    if venue_passphrase(venue).is_required() {
        required.push(passphrase.clone());
    }
    vec![TierKeys { required, tier_named: vec![key, secret, passphrase] }]
}

/// Why one `{VENUE}_{TIER}` prefix yielded no credentials. Carried rather than reported in place
/// so [`load_credentials_from`] stays silent, and so "nothing configured" and "half configured"
/// never look the same to a caller.
enum NoCreds {
    /// Key and/or secret unset or blank — the ORDINARY unconfigured state: nothing to report.
    KeyOrSecret,
    /// Key AND secret present, but this venue's signer REQUIRES a passphrase and it is unset or
    /// blank. Carries the missing variable's NAME — never a value.
    Passphrase { name: String },
}

fn load_for_prefix(
    venue: &str,
    prefix: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Result<Credentials, NoCreds> {
    let (key_name, secret_name, passphrase_name) = names_for_account(prefix, label);
    let get = |name: &str| vars.get(name).map(|v| v.trim().to_string()).unwrap_or_default();
    let api_key = get(&key_name);
    let api_secret = get(&secret_name);
    if api_key.is_empty() || api_secret.is_empty() {
        return Err(NoCreds::KeyOrSecret);
    }
    let passphrase = Some(get(&passphrase_name)).filter(|p| !p.is_empty());
    // The per-venue gate. `Optional`/`Unused` venues load with two credentials (aster with a
    // derived signer address); only a `Required` row refuses.
    if passphrase.is_none() && venue_passphrase(venue).is_required() {
        return Err(NoCreds::Passphrase { name: passphrase_name });
    }
    Ok(Credentials { api_key, api_secret, passphrase })
}

#[cfg(test)]
mod live_tier_tests;

#[cfg(test)]
mod required_passphrase_tests;

#[cfg(test)]
mod account_tests;

/// **The credentials, from the project's store**, with `VIKE_SETTINGS_DIR`'s value naming the
/// settings directory outright when a deployment supplies one.
///
/// Infallible: an unreadable store yields an empty map (the live gate: every venue stays paper),
/// LOUD in the log and silent in the return value — "cannot authenticate" fails at the venue,
/// where an operator sees it. [`try_load_workspace_secrets_at`] is the form for a caller that can
/// surface the error to a human (`vike-cli secrets`).
pub fn load_workspace_secrets_at(settings_dir: Option<&str>) -> HashMap<String, String> {
    load_workspace_secrets_at_checked(settings_dir).0
}

/// **Whether the store this map came from could be OPENED** — the one fact
/// [`load_workspace_secrets_at`] swallows, handed back as DATA.
///
/// ⚠ **Two ways to get an EMPTY map, and they are not the same event.** An ABSENT store is the
/// ordinary unconfigured state: `0 credentials` is a real measurement. A store that EXISTS and will
/// not open produces the SAME empty map with nothing measured — the root `CLAUDE.md` rule: *a store
/// that EXISTS and cannot be read is an ERROR, not "no credentials"*.
///
/// An EXEC path may collapse them (it degrades to paper either way). A path that RENDERS A COUNT
/// may not: [`StoreHealth::Unreadable`] is how it knows to render "unknown" instead of a zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StoreHealth {
    /// The store ANSWERED. [`Source::None`] — no store on this box — is this arm too: its empty map
    /// answers a question that was actually asked.
    #[default]
    Readable,
    /// The store EXISTS and could not be opened, so any count folded from the map is unknown, not
    /// zero. The text is [`SecretsError`]'s `Display` — a path and an OS reason, never file
    /// contents — so it is safe to log, print and render verbatim.
    Unreadable(String),
}

impl StoreHealth {
    /// Whether the store answered at all — `true` for [`Self::Readable`], including the absent
    /// store. The negation is the state in which a count is not a measurement.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        matches!(self, StoreHealth::Readable)
    }
}

/// [`load_workspace_secrets_at`], plus [`StoreHealth`] — **ONE read**, so the map and the verdict
/// can never describe two different reads. The infallible wrapper is `.0` of this.
pub fn load_workspace_secrets_at_checked(
    settings_dir: Option<&str>,
) -> (HashMap<String, String>, StoreHealth) {
    match try_load_workspace_secrets_at(settings_dir) {
        Ok((map, _source)) => (map, StoreHealth::Readable),
        Err(e) => {
            // `SecretsError` carries no secret material (see its doc) — safe to log verbatim.
            tracing::error!(
                "credential store could not be opened, continuing with NO credentials \
                 (every venue stays paper): {e}"
            );
            (HashMap::new(), StoreHealth::Unreadable(e.to_string()))
        }
    }
}

/// **The credentials, from the ONE process-environment sweep a composition root already owns.**
/// This is the function a BINARY calls:
///
/// ```ignore
/// let env: HashMap<String, String> = std::env::vars().collect();
/// let vars = credentials::load_workspace_secrets_from_env(&env);
/// ```
///
/// TWO facts are taken out of `env` and nothing else:
///
/// * `VIKE_SETTINGS_DIR`, the settings DIRECTORY (`vike_secrets::project_settings_dir_from`);
///   absent — the normal case — the runtime walk answers.
/// * `VIKE_CREDENTIAL_SCOPE`, which can only NARROW the answer. Unset or blank is the whole map.
///   `demo` is the DEMO-ONLY scope: the store returns no name
///   `vike_secrets::withheld_by_demo_scope` matches (`_LIVE_`/`_MAINNET_`, `ASTER_`, `POLY_`), so a
///   live request however it spells the tier finds it ABSENT. Any OTHER value is refused: an empty
///   map and an error line, never a guess.
///
/// ⚠ The scope is an environment variable deliberately: it is a property of ONE PROCESS, and the
/// database is shared by every process on the box.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map.** Same Rust type, opposite
/// meanings: the credential map is this function's OUTPUT.
///
/// PURE — it performs no environment read of its own, so every root's credential resolution is
/// testable without mutating process state, and calling it adds no `Layer::Library` row to
/// `vike_ops::settings`.
pub fn load_workspace_secrets_from_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    load_workspace_secrets_from_env_checked(env).0
}

/// [`load_workspace_secrets_from_env`], plus [`StoreHealth`] — for a root whose UI RENDERS A COUNT.
/// Same two facts out of `env`, same purity, same one read; the two literals below are the crate's
/// ONE spelling of each read.
pub fn load_workspace_secrets_from_env_checked(
    env: &HashMap<String, String>,
) -> (HashMap<String, String>, StoreHealth) {
    // The literal, not `vike_secrets::SETTINGS_DIR_ENV`: `vike_model::scan`'s map-lookup sweep
    // resolves constants CRATE-wide, so an imported one would hide this read from the settings
    // registry. `tests/settings_dir_spellings.rs` pins the two spellings equal.
    let settings_dir = env.get("VIKE_SETTINGS_DIR").map(String::as_str);
    match env.get("VIKE_CREDENTIAL_SCOPE").map(|s| s.trim()) {
        None | Some("") => load_workspace_secrets_at_checked(settings_dir),
        Some("demo") => load_workspace_secrets_demo_only_at_checked(settings_dir),
        Some(other) => {
            // The scope's value is a mode word, never a credential, so naming it is safe.
            let why = format!(
                "VIKE_CREDENTIAL_SCOPE={other:?} is not a scope this build knows (the one scope is \
                 `demo`), so NO credential was read rather than a guess at what was meant"
            );
            tracing::error!("{why}");
            (HashMap::new(), StoreHealth::Unreadable(why))
        }
    }
}

/// [`load_workspace_secrets_at_checked`] under the DEMO-ONLY scope (`VIKE_CREDENTIAL_SCOPE=demo`):
/// the same store findings, plus one line saying how many names the store withheld, so a run that
/// found its live key missing can see why.
fn load_workspace_secrets_demo_only_at_checked(
    settings_dir: Option<&str>,
) -> (HashMap<String, String>, StoreHealth) {
    let dir = vike_secrets::workspace_settings_dir_from(settings_dir);
    match vike_secrets::resolve_store_demo_only_in(&dir) {
        Ok((resolved, withheld)) => {
            warn_store_findings(&resolved);
            tracing::warn!(
                "DEMO-ONLY credential scope (VIKE_CREDENTIAL_SCOPE=demo): the store withheld \
                 {withheld} live/mainnet/ASTER_/POLY_ name(s); a request for any of them reads as \
                 absent"
            );
            (resolved.secrets.into_map(), StoreHealth::Readable)
        }
        Err(e) => {
            // `SecretsError` carries no secret material (see its doc) — safe to log verbatim.
            tracing::error!(
                "credential store could not be opened under the demo-only scope, continuing with \
                 NO credentials: {e}"
            );
            (HashMap::new(), StoreHealth::Unreadable(e.to_string()))
        }
    }
}

/// **The DECLARED credentials and nothing else**, with the error and the provenance returned —
/// the scoped twin of [`try_load_workspace_secrets_at`]. A caller declares the names it needs as a
/// [`KeyScope`] and the process never holds the rest.
///
/// ⚠ **The store's permission finding is logged from HERE, exactly as for the whole-map read**: a
/// scoped caller is no less entitled to be told its store is world-readable.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn try_load_workspace_secrets_scoped_at(
    settings_dir: Option<&str>,
    scope: &KeyScope,
) -> Result<ScopedSecrets, SecretsError> {
    let scoped = vike_secrets::resolve_project_scoped(settings_dir, scope)?;
    // The same finding, the same `warn_once` — paths, never a value.
    if let Some(w) = &scoped.warning {
        warn_once(&w.to_string());
    }
    Ok(scoped)
}

/// [`try_load_workspace_secrets_scoped_at`], infallible — the scoped twin of
/// [`load_workspace_secrets_at`].
///
/// An unreadable store degrades to a scope in which every DECLARED name answers
/// [`Lookup::AbsentFromStore`] (the live gate) while an UNDECLARED name still answers
/// [`Lookup::NotDeclared`]: the two failures stay distinguishable, which is why the scoped answer
/// is a type rather than a map. LOUD in the log, silent in the return value.
#[must_use]
pub fn load_workspace_secrets_scoped_at(
    settings_dir: Option<&str>,
    scope: &KeyScope,
) -> ScopedSecrets {
    match try_load_workspace_secrets_scoped_at(settings_dir, scope) {
        Ok(scoped) => scoped,
        Err(e) => {
            // `SecretsError` carries no secret material (see its doc) — safe to log verbatim.
            tracing::error!(
                "credential store could not be opened, continuing with NO credentials \
                 (every venue stays paper): {e}"
            );
            ScopedSecrets::empty(scope, Source::None)
        }
    }
}

/// **The DECLARED credentials, from the ONE process-environment sweep a composition root already
/// owns** — the scoped twin of [`load_workspace_secrets_from_env`], and the function a BINARY
/// calls. ONE fact is taken out of `env`: `VIKE_SETTINGS_DIR`. PURE, for the same reason.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map**, and a `scope` is not an
/// env sweep either.
#[must_use]
pub fn load_workspace_secrets_scoped_from_env(
    env: &HashMap<String, String>,
    scope: &KeyScope,
) -> ScopedSecrets {
    load_workspace_secrets_scoped_at(
        // The literal, for the reason `load_workspace_secrets_from_env_checked` gives.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
        scope,
    )
}

/// **Which ACCOUNTS the credential store holds**, from the one process-environment sweep a
/// composition root already owns — the account twin of [`load_workspace_secrets_from_env`], and
/// the door the mount side reaches the `account` table through (`vike-mount` links no
/// `vike-secrets` of its own).
///
/// ```ignore
/// let env: HashMap<String, String> = std::env::vars().collect();
/// match credentials::load_workspace_accounts_from_env(&env)? {
///     Accounts::Known(rows)          => /* the store answered; `rows` may be empty */,
///     Accounts::Unanswerable(reason) => /* ask the key names — `accounts_in_store` */,
/// }
/// ```
///
/// ONE fact is taken out of `env`: `VIKE_SETTINGS_DIR`, read the same way as the credential loader,
/// so the two cannot resolve different stores. PURE. ⚠ **`env` is the REAL PROCESS ENVIRONMENT,
/// never the credential map.**
///
/// # ⚠ Three answers, and none may be collapsed into another
///
/// * `Ok(Accounts::Known(rows))` — the store carries the table. An EMPTY `rows` is a real answer.
/// * `Ok(Accounts::Unanswerable(_))` — no `account` table on this box (no settings database, or one
///   older than the table); `vike_model::accounts::account_keys::accounts_in_store` is the reader
///   that applies. Treating it as an empty list would put every venue silently on paper.
/// * `Err(_)` — the store EXISTS and would not open. Loud, never an absence.
///
/// There is deliberately **no infallible twin**: an empty credential map degrades safely because it
/// IS the live gate, and an empty account list carries no such guarantee.
///
/// # ⚠ It ARMS A VENUE
///
/// Dukascopy's mount resolves WHICH ACCOUNT — and so which LEGAL ENTITY an order reaches — from
/// these rows (`crates/bridges/dukascopy/src/mount.rs`). A composition root reads it once, beside
/// its credential load (`crates/vike-tradehub/src/tradehub_cli/live_mount.rs`), and carries it to
/// the mount and the arming projection as `vike_mount::MountPolicy::accounts`
/// (`crate::account_directory::AccountDirectory::read`, which takes this `Result` and its key-name
/// twin [`load_workspace_account_keys_from_env`] verbatim, errors included). A root that does not
/// read it leaves that field UNREAD: every labelled dukascopy account is refused by name and the
/// default account resolves unchanged.
pub fn load_workspace_accounts_from_env(
    env: &HashMap<String, String>,
) -> Result<Accounts, SecretsError> {
    load_workspace_accounts_at(
        // The literal, for the reason `load_workspace_secrets_from_env_checked` gives.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
    )
}

/// [`load_workspace_account_keys_at`] from the one process-environment sweep — the key-NAME twin of
/// [`load_workspace_accounts_from_env`], and the second half of what
/// `crate::account_directory::AccountDirectory::read` takes. A caller needs BOTH: the rows say
/// which accounts exist, the key names say which CREDENTIAL FAMILY each row owns (for dukascopy's
/// two demo rows the only fact that separates them, and so the one that decides the broker).
///
/// # Errors
/// [`SecretsError`] when a store that EXISTS will not open — loud, never an absence.
pub fn load_workspace_account_keys_from_env(
    env: &HashMap<String, String>,
) -> Result<Option<std::collections::BTreeMap<i64, AccountKeys>>, SecretsError> {
    load_workspace_account_keys_at(
        // The literal, for the reason its sibling above gives.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
    )
}

/// [`load_workspace_accounts_from_env`] for a caller that already holds the `VIKE_SETTINGS_DIR`
/// override (or `None` to let the project walk answer). One line over
/// `vike_secrets::resolve_accounts`: the same walk, override and backend probe the credential
/// reader and writer ask, so it cannot resolve a different store than the map does.
pub fn load_workspace_accounts_at(settings_dir: Option<&str>) -> Result<Accounts, SecretsError> {
    vike_secrets::resolve_accounts(settings_dir)
}

/// **The credential key NAMES each `account` row owns** — the companion
/// [`load_workspace_accounts_at`] needs, surfaced here because `vike-mount` links no
/// `vike-secrets`.
///
/// For dukascopy's two demo rows every [`Account`] cell but `id` is identical (the owner refused a
/// label on either); what separates them is each row's OWNER PREFIX, `DUKASCOPY_DEMO1_` against
/// `DUKASCOPY_DEMO2_`, which `vike_dukascopy::resolve_account` maps onto the LEGAL ENTITY an order
/// reaches.
///
/// ⚠ **NAMES ONLY, structurally.** `vike_secrets::read_account_keys` never selects
/// `credential.value`, so nothing reachable from the returned map can be a credential.
///
/// `Ok(None)` means exactly [`Accounts::Unanswerable`] (no `account` table to key) and must not be
/// merged with `Ok(Some(empty))` (the table is there and no row owns a live credential name).
///
/// # Errors
/// [`SecretsError`] when a store that EXISTS will not open — loud, never an absence.
pub fn load_workspace_account_keys_at(
    settings_dir: Option<&str>,
) -> Result<Option<std::collections::BTreeMap<i64, AccountKeys>>, SecretsError> {
    vike_secrets::resolve_account_keys(settings_dir)
}

/// [`load_workspace_secrets_at`], but returning the error and the provenance instead of swallowing
/// them — for a human-facing caller (`vike-cli secrets`) that can print both.
///
/// ⚠ **This is where the store's finding becomes a log line**: its PERMISSION exposure.
/// `vike-secrets` returns it as data (it carries no logging dependency); every whole-map read here
/// routes through this function, so no composition root can forget to surface it.
pub fn try_load_workspace_secrets_at(
    settings_dir: Option<&str>,
) -> Result<(HashMap<String, String>, Source), SecretsError> {
    let resolved = vike_secrets::resolve_project(settings_dir)?;
    warn_store_findings(&resolved);
    Ok((resolved.secrets.into_map(), resolved.source))
}

/// The finding a resolved store carries, logged ONCE per process — shared by every
/// whole-map read here ([`try_load_workspace_secrets_at`] and the demo-only scope). Its `Display`
/// prints paths and a mode, never a value.
fn warn_store_findings(resolved: &vike_secrets::Resolved) {
    if let Some(w) = &resolved.warning {
        // `PermissionWarning`: paths and an octal mode; on `Finding::Symlink` the second path is
        // the link's TARGET, still not a value.
        warn_once(&w.to_string());
    }
}

/// `true` the first time `message` is seen in this process — [`warn_once`]'s dedupe, so a per-frame
/// caller cannot flood the log.
fn first_time(message: &str) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    // A poisoned mutex must not lose the finding: recover the guard rather than panic in a logging
    // path.
    match seen.lock() {
        Ok(mut g) => g.insert(message.to_string()),
        Err(poisoned) => poisoned.into_inner().insert(message.to_string()),
    }
}

/// **Every store finding, logged ONCE PER DISTINCT MESSAGE for the life of the process.**
///
/// ⚠ A per-call `warn!` floods the log: one caller reads credentials PER FRAME
/// (`crates/vike-desktop/src/main.rs`'s `workspace_credentials_checked`), and a store finding is a
/// permanent state, not a fault — a per-call line buried the whole WARN channel.
///
/// Keyed on the RENDERED MESSAGE, not a plain `Once`: the message embeds the paths, so two settings
/// directories in one process each get their line and a NEW finding is never swallowed by an old
/// one. A finding is a STATE, not an event, so the repeats carry no information; `vike-cli secrets`
/// prints the finding as data on every invocation.
fn warn_once(message: &str) {
    if first_time(message) {
        tracing::warn!("{message}");
    }
}

/// Resolve a venue's attribution code from the loaded credential map (NOT the process
/// environment), validated against the venue's
/// [`vike_model::venues::attribution::AttributionMechanic`]. Tries `{VENUE}_BROKER_CODE`, then
/// `{VENUE}_BUILDER_CODE`. `None` when the key is absent or blank, the code fails the mechanic's
/// constraints, or the venue has no mechanic — so an unset or invalid code stamps nothing.
///
/// Both names come from [`vike_model::credential_keys::attribution_key`], so
/// [`vike_model::credential_keys::attribution_keys`] enumerates exactly what this reads — including
/// the mechanic-less early return, which is why a venue classified
/// [`None`](vike_model::venues::attribution::AttributionMechanic::None) contributes no key to that
/// grid and gets no registry row: its key provably cannot be looked up.
pub fn attribution_code_from(vars: &HashMap<String, String>, venue: &str) -> Option<String> {
    let mech = vike_model::venues::attribution::attribution_for(venue);
    if mech.is_none() {
        return None;
    }
    let code = vars
        .get(&attribution_key(venue, BROKER_CODE_SUFFIX))
        .or_else(|| vars.get(&attribution_key(venue, BUILDER_CODE_SUFFIX)))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    mech.validate_code(code).ok()?;
    Some(code.to_string())
}

#[cfg(test)]
mod workspace_store_tests;

#[cfg(test)]
mod attribution_code_tests;

#[cfg(test)]
mod generated_key_grid_tests;
