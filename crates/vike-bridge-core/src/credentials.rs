//! Per-venue API credentials from the credential store. Exact port of `exec/credentials.py`.
//!
//! `load_credentials_from` returns None when the venue's REQUIRED credential set is not fully
//! present — that absence IS the live gate (creds absent → stay paper). `names_for_prefix` is the
//! SINGLE naming site. HARD: the secret never reaches Debug/Display/any log line.
//!
//! # MORE THAN ONE ACCOUNT PER VENUE
//!
//! [`load_credentials_for_account`] is [`load_credentials_from`] with the account named, and
//! [`account_var`] is the same door for the BESPOKE key shapes this module's `_API_*` grid does not
//! cover (`OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`, the FX
//! `_USER`/`_PASSWORD` pair). Both compose the name through
//! [`vike_model::account_keys::account_key`], whose grammar appends `__{LABEL}` after the WHOLE of
//! today's key and never re-parses it — so a bespoke suffix is handled by construction rather than
//! by a table somebody has to extend, and the [`AccountLabel::Default`] account's names are the
//! same `String`s they were before any of this existed.
//!
//! ⚠ **The LABELLED half of the grid is NOT enumerable, and that is structural.** The label is an
//! operator-chosen name, so `vike_model::credential_keys::credential_keys` — the enumeration
//! `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared` demands rows
//! for — covers the DEFAULT account only and deliberately keeps doing so. A labelled key is a
//! computed map lookup over an unbounded name set: exactly the blind spot
//! [`vike_model::credential_keys`]' module doc describes, re-entered on purpose because there is no
//! finite grid to declare. Its DISCLOSURE surface is therefore the store itself — what is present
//! is what is enumerable — through `vike_model::account_keys::accounts_in_store`.
//!
//! ⚠ **The key names this module reads are ENUMERABLE, and that is load-bearing.** Every name here
//! is composed from [`vike_model::credential_keys`]' suffix/tier tables rather than spelled at the
//! call site, because a `format!`-built map key appears as no literal anywhere and was therefore
//! invisible to `vike_ops::settings::SETTINGS` — `OKX_LIVE_API_SECRET` was read on every credential
//! probe with neither a registry row nor a sighting, undetectably. That module's doc is the
//! authority on the blind spot; this file's `generated_key_grid_tests` is where the enumeration and
//! the READ are held equal, by folding this file's own `names_for_prefix` over the roster.
//!
//! ⚠ **"Required" is PER-VENUE, and the passphrase is the reason.** key+secret is the required set
//! for binance/bybit/deribit; OKX additionally requires `_API_PASSPHRASE`, because its signer sends
//! one on every request. Until the [`venue_passphrase`](mod@crate::venue_passphrase) table existed
//! the passphrase was optional for everyone, so an OKX store with two of the three credentials
//! LOADED, mounted LIVE, and then failed every signed request at the venue — the live gate
//! admitting the one state it exists to prevent. [`missing_required_passphrase`] is how a mount
//! says so out loud, by NAME.
//!
//! # ONE store: `<project>/settings/secrets.env`
//!
//! Every API key, every venue, every checkout of this project. `vike_secrets` owns the file and its
//! resolution; this module is the workspace-facing door onto it:
//!
//! | function | who calls it |
//! |---|---|
//! | [`load_workspace_secrets_from_env`] | **the composition roots**, out of the one `std::env::vars()` sweep they already own |
//! | [`load_workspace_secrets_at`] | a caller that has already isolated the settings-directory override |
//! | [`try_load_workspace_secrets_at`] | the same, but a hard error instead of an empty map (`vike-cli secrets`) |
//! | [`load_workspace_dotenv`] | a caller with nothing to pass — the walk answers on its own |
//! | [`load_workspace_dotenv_from`] | a TEST binary, which owns its own env read and wants the reader without this crate's `tracing` findings |
//!
//! All four return the SAME `HashMap<String, String>`, so every downstream consumer
//! (`load_credentials_from`, `attribution_code_from`, every venue `config.rs` loader) is written
//! against one shape.
//!
//! ⚠ **…and they all return the same CONTENT, which they did not before 2026-09-22.** Ruling 10
//! moved ten config-shaped names out of the `credential` table into `venue_setting`, and the fold
//! that rendered them back into the names a loader looked up lived in THIS file — so it was applied
//! by the rows above and by nothing below them. `vike_secrets::load_workspace_dotenv_from` and
//! `vike_secrets::load_workspace_dotenv_scoped`, which the ibkr smokes and
//! `crates/bridges/polymarket/src/egress.rs` read through, resolved the pre-move answer in silence
//! — measured both times, once as a smoke that self-skipped while printing `ok` and once as three
//! operator rows read by nothing. The grammar moved DOWN into the store, which folded inside its own
//! front door until decision 0095's Task 7 retired the fold outright: no `venue_setting` row reaches
//! any of these maps now, every venue reader takes the rows as
//! `vike_secrets::venue_setting::VenueSettings`, and a credential row still carrying a setting's old
//! name is refused at boot. `load_workspace_dotenv_from`'s own doc carries the measurement.
//!
//! Absent credentials ARE the live gate: no store ⇒ an empty map ⇒ every venue stays paper.
//!
//! ⚠ Nothing in this workspace deletes, moves or rewrites the store. It is the user's only copy of
//! live venue credentials.

use std::collections::HashMap;

use vike_model::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{
    API_KEY_SUFFIX, API_PASSPHRASE_SUFFIX, API_SECRET_SUFFIX, BROKER_CODE_SUFFIX,
    BUILDER_CODE_SUFFIX, attribution_key,
};

// The `venue_setting` NAMING GRAMMAR, which MOVED to `vike-secrets` on 2026-09-22. A private
// `use`, never a `pub use`: this crate is a CALLER now, not a second home, and re-exporting the
// names would leave the old spellings compiling — the shim CLAUDE.md forbids on a move. The
// classifier below goes the other way (name → row) over the same [`HAND_MAPPED_ACCOUNTS`] table,
// which is why that table is the one item here that had to become `pub` down there.
use vike_secrets::venue_setting::{
    HAND_MAPPED_ACCOUNTS, hand_mapped_prefix, parse_venue_setting_key, venue_setting_names,
};

use crate::venue_passphrase::venue_passphrase;

pub use vike_secrets::{
    SECRETS_FILE, SecretsError, Source, load_workspace_dotenv, load_workspace_dotenv_from,
    parse_dotenv, workspace_dotenv_path,
};
// The SCOPED read's vocabulary, re-exported for the same reason as the names above: the consumers
// that declare a scope (the `vike-backfill` bins, `vike-datahub`'s recorder, the two polymarket
// library readers) link no `vike-secrets` of their own, and the scoped loaders below would
// otherwise return a type none of them can name. Public-API surface, not a move shim.
pub use vike_secrets::{KeyScope, Lookup, ScopedSecrets, UndeclaredKey};
// The `account` table's vocabulary, re-exported for the same reason the four names above are:
// `vike-mount` and the other consumers above this crate link no `vike-secrets` of their own, and
// `load_workspace_accounts_from_env` below would otherwise return a type none of them can name.
// Public-API surface, not a move shim — nothing here changed homes.
pub use vike_secrets::{Account, AccountKeys, Accounts, NoAccountTable};

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

    /// Legacy env-var tier accepted as a fallback (pre-rename `.env` files used MAINNET).
    /// `pub` (not `pub(crate)`): the config loaders in the venue bridge crates (ig/oanda/fxcm/
    /// polymarket) call this directly across the crate boundary introduced by the bridge-core
    /// extraction (crate-reorg Phase 3, D4).
    pub fn legacy_str(&self) -> Option<&'static str> {
        match self {
            Environment::Live => Some("MAINNET"),
            _ => None,
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

/// The three names one `{VENUE}_{TIER}` prefix yields — the SINGLE naming site, and now one that
/// spells no suffix of its own. The three suffixes are [`vike_model::credential_keys`]' constants,
/// so the grid the settings registry enumerates and the grid this loader reads are composed from
/// the same table; `generated_key_grid_tests::the_loader_reads_exactly_the_enumerated_credential_grid`
/// asserts the two sides produce the same SET, folding this function over the roster.
fn names_for_prefix(prefix: &str) -> (String, String, String) {
    (
        format!("{prefix}{API_KEY_SUFFIX}"),
        format!("{prefix}{API_SECRET_SUFFIX}"),
        format!("{prefix}{API_PASSPHRASE_SUFFIX}"),
    )
}

/// The same three names, for ONE ACCOUNT — [`names_for_prefix`] with
/// [`vike_model::account_keys::account_key`] applied to each.
///
/// ⚠ **[`AccountLabel::Default`] returns [`names_for_prefix`]'s tuple UNCHANGED**, because
/// `account_key` returns its input unchanged for the default account. That is not a happy
/// coincidence to be re-checked at every call site: it is the step-1 contract of the grammar, and
/// it is what makes a single-account box's load path the SAME `String`s it was before this function
/// existed — `account_tests::the_default_account_composes_the_historical_names` folds both sides
/// over the whole roster and every tier and asserts it.
fn names_for_account(prefix: &str, label: &AccountLabel) -> (String, String, String) {
    let (key, secret, passphrase) = names_for_prefix(prefix);
    (account_key(&key, label), account_key(&secret, label), account_key(&passphrase, label))
}

/// **Read ONE credential of ONE account out of a var map** — the door for every credential shape
/// this module's own `{VENUE}_{TIER}_API_*` grid does NOT cover.
///
/// `base` is today's key, whatever its shape: `OANDA_DEMO_ACCOUNT_ID`, `IG_LIVE_IDENTIFIER`,
/// `ASTER_LIVE_PRIVATE_KEY`, `FXCM_DEMO_USER`. The account-aware name is
/// [`vike_model::account_keys::account_key`]'s, so a bespoke suffix needs no entry in any table
/// here and none can be forgotten — the label is appended after the WHOLE of `base`, and `base` is
/// never re-parsed. `account_tests::every_bespoke_credential_shape_is_reachable_by_label` proves it
/// over the real bespoke names rather than over a sample.
///
/// Returns the TRIMMED value, or `None` when the key is absent or blank — the same
/// "blank is absent" rule [`load_credentials_from`] applies, so a caller cannot accidentally treat
/// `KEY=` as configured.
///
/// ⚠ **There is NO fallback to the unlabelled key.** A labelled account whose credential is missing
/// reads as missing, exactly like a default account whose credential is missing; falling back would
/// sign account `ALT`'s orders with the default account's key, which is the one outcome worth more
/// than every convenience.
#[must_use]
pub fn account_var<'a>(
    vars: &'a HashMap<String, String>,
    base: &str,
    label: &AccountLabel,
) -> Option<&'a str> {
    vars.get(&account_key(base, label)).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Read creds from a var map (process env or a parsed `.env`); None when the venue's REQUIRED
/// credential set is not fully present with non-blank values (the live gate). `Live` also accepts
/// the legacy `MAINNET` tier so pre-rename `.env` files keep working.
///
/// The required set is `_API_KEY` + `_API_SECRET`, **plus `_API_PASSPHRASE` on a venue whose
/// [`venue_passphrase`] row is
/// [`Required`](crate::venue_passphrase::PassphraseNeed::Required)** (today: OKX, whose signer
/// sends it on every request). A half-configured venue is UNUSABLE credentials, and unusable
/// credentials must resolve exactly like absent ones — otherwise the venue mounts LIVE and every
/// signed request is rejected at the venue, which is a worse failure than staying paper and a
/// silent one at the mount.
///
/// SILENT by construction, at every layer: this function is called per-venue per-tier on hot paths
/// (`vike_connections::credential_status` re-runs the whole grid EVERY FRAME the GUI's Connections
/// tool is open), so it logs nothing at all. [`missing_required_passphrase`] returns the finding as
/// DATA — the same shape `vike-secrets` uses for its permission warning — and the ONE mount site
/// (`vike_mount::make_engine`) is where it becomes a log line an operator reads.
pub fn load_credentials_from(
    venue: &str,
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    load_credentials_for_account(venue, env, &AccountLabel::Default, vars)
}

/// [`load_credentials_from`] for a NAMED account — the whole of this module that a second
/// credential set per venue needs.
///
/// Everything [`load_credentials_from`] documents holds here word for word: the required set, the
/// per-venue passphrase rule, the legacy-tier fallback, the silence. The ONLY difference is the
/// NAMES read, and it is entirely [`names_for_account`]'s: `{VENUE}_{TIER}{SUFFIX}__{LABEL}`.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load_credentials_from`]** — the same
/// function, reached through it — so this is a widening of the loader and not a second one.
///
/// ⚠ **An INCOMPLETE labelled account is an ABSENT one**, which is the pre-existing behaviour and
/// deliberately not a new failure mode: key present and secret missing returns `None`, silently,
/// exactly as `BYBIT_DEMO_API_KEY` without `BYBIT_DEMO_API_SECRET` has always returned `None`.
/// There is no error, no log and no partial `Credentials`, because the caller of a credential load
/// is the live gate, and the live gate's vocabulary is `Some`/`None`. The one finding this module
/// ever produces stays [`missing_required_passphrase_for_account`]'s, which is DATA and names a
/// variable rather than reporting a value.
///
/// ⚠ **No fallback to the unlabelled key** — see [`account_var`] for why.
pub fn load_credentials_for_account(
    venue: &str,
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    load_for_prefix(venue, &prefix_for(venue, env.as_str()), label, vars)
        .ok()
        .or_else(|| load_for_prefix(venue, &prefix_for(venue, env.legacy_str()?), label, vars).ok())
}

/// The `{VENUE}_{TIER}` half of a credential key, as ONE site.
///
/// Extracted so the equivalence gate can fold the LOADER'S OWN composition
/// (`prefix_for` + [`names_for_prefix`]) instead of re-spelling it beside the assertion — a gate
/// whose expectation is written out by hand next to the thing it checks proves only that somebody
/// typed the same string twice.
fn prefix_for(venue: &str, tier: &str) -> String {
    format!("{}_{tier}", venue.to_uppercase())
}

/// **The NAME of the credential a half-configured venue is missing**, or `None` when the venue is
/// either fully configured or not configured at all.
///
/// `Some(name)` means exactly one thing: this venue's `_API_KEY` and `_API_SECRET` are both
/// present and non-blank, its [`venue_passphrase`] row is
/// [`Required`](crate::venue_passphrase::PassphraseNeed::Required), and the passphrase is unset or
/// blank — so [`load_credentials_from`] returned `None` and the venue stays PAPER while the
/// operator's store LOOKS configured. That is the case a mount must say out loud.
///
/// Returns the variable NAME, never a value — no credential (present or absent) is ever echoed.
/// Both tiers are consulted in `load_credentials_from`'s own order, so a `Live` mount whose legacy
/// `MAINNET` set is complete reports nothing.
///
/// PURE and log-free, deliberately: see [`load_credentials_from`]'s note on the per-frame callers.
#[must_use]
pub fn missing_required_passphrase(
    venue: &str,
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<String> {
    missing_required_passphrase_for_account(venue, env, &AccountLabel::Default, vars)
}

/// [`missing_required_passphrase`] for a NAMED account. The name it returns is the LABELLED one
/// (`OKX_DEMO_API_PASSPHRASE__ALT`), because that is the variable the operator has to write; a
/// finding that named the unlabelled key would send them to edit the account that is working.
///
/// [`AccountLabel::Default`] is [`missing_required_passphrase`], reached through it.
#[must_use]
pub fn missing_required_passphrase_for_account(
    venue: &str,
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<String> {
    if load_credentials_for_account(venue, env, label, vars).is_some() {
        return None;
    }
    let mut tiers = vec![env.as_str()];
    tiers.extend(env.legacy_str());
    tiers.into_iter().find_map(|tier| {
        match load_for_prefix(venue, &prefix_for(venue, tier), label, vars) {
            Err(NoCreds::Passphrase { name }) => Some(name),
            Ok(_) | Err(NoCreds::KeyOrSecret) => None,
        }
    })
}

/// Why one `{VENUE}_{TIER}` prefix yielded no credentials. Carried rather than reported in place
/// so [`load_credentials_from`] can stay silent about a tier its legacy fallback then satisfied,
/// and so "nothing configured" and "half configured" never look the same to a caller.
enum NoCreds {
    /// Key and/or secret unset or blank — the ORDINARY unconfigured state, and the live gate
    /// working as designed. Carries nothing: there is nothing to tell an operator who configured
    /// nothing.
    KeyOrSecret,
    /// Key AND secret present, but this venue's signer REQUIRES a passphrase and it is unset or
    /// blank. Carries the missing variable's NAME — never its value, and never the two credentials
    /// that WERE found.
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
    // The per-venue gate. `Optional`/`Unused` venues are untouched — byte-identical to before this
    // check existed — so binance/bybit/deribit and every bespoke-shape venue keep loading with two
    // credentials, and aster keeps loading with a derived signer address.
    if passphrase.is_none() && venue_passphrase(venue).is_required() {
        return Err(NoCreds::Passphrase { name: passphrase_name });
    }
    Ok(Credentials { api_key, api_secret, passphrase })
}

#[cfg(test)]
mod live_tier_tests {
    use super::*;

    #[test]
    fn live_reads_live_prefix_and_falls_back_to_mainnet() {
        let mut vars = HashMap::new();
        vars.insert("BINANCE_MAINNET_API_KEY".into(), "legacy-key".into());
        vars.insert("BINANCE_MAINNET_API_SECRET".into(), "legacy-secret".into());
        // legacy-only .env still loads under Live
        let c = load_credentials_from("binance", Environment::Live, &vars).unwrap();
        assert_eq!(c.api_key, "legacy-key");
        // LIVE_* wins when both are present
        vars.insert("BINANCE_LIVE_API_KEY".into(), "new-key".into());
        vars.insert("BINANCE_LIVE_API_SECRET".into(), "new-secret".into());
        let c = load_credentials_from("binance", Environment::Live, &vars).unwrap();
        assert_eq!(c.api_key, "new-key");
    }

    /// Each credential tier reads its venue settings at one `venue_setting` tier, and `Sim` is the
    /// `paper` one — the credential key's `SIM` token and the stored tier are different words.
    #[test]
    fn each_credential_tier_reads_its_settings_at_one_setting_tier() {
        use vike_secrets::venue_setting::SettingTier;
        assert_eq!(Environment::Sim.setting_tier(), SettingTier::Paper);
        assert_eq!(Environment::Demo.setting_tier(), SettingTier::Demo);
        assert_eq!(Environment::Live.setting_tier(), SettingTier::Live);
    }
}

/// **The half-credential gate**: a venue whose signer REQUIRES a passphrase must not load without
/// one, because loading is what mounts it live.
#[path = "required_passphrase_tests.rs"]
#[cfg(test)]
mod required_passphrase_tests;

/// **MORE THAN ONE ACCOUNT PER VENUE** — the labelled half of the loader, and the byte-identity of
/// the unlabelled half.
///
/// ⚠ No bespoke key NAME is spelled here. Every fixture is composed through the module's own
/// [`prefix_for`] + [`names_for_account`], for the reason `required_passphrase_tests::seed` gives:
/// `vike_ops::scan`'s map-lookup sweep harvests env-shaped literals wherever they appear, and a
/// fixture spelling one is indistinguishable from a real read. The BESPOKE families
/// (`OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`, the FX
/// `_USER`/`_PASSWORD` pair) are covered in
/// `crates/vike-bridge-core/tests/account_credential_shapes.rs`, which reaches them through each
/// bridge's OWN name function — the only way to prove the real names rather than a copy of them.
#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;

/// **The credentials, from the project's store**, with `VIKE_SETTINGS_DIR`'s value naming the
/// settings directory outright when a deployment supplies one.
///
/// Infallible: an unreadable store yields an empty map, which is the live-gate semantics (no creds
/// → stay paper). The failure is LOUD in the log and silent in the return value, deliberately —
/// the blast radius of the empty map is "cannot authenticate", which fails at the venue where an
/// operator sees it. Use [`try_load_workspace_secrets_at`] where the error can be surfaced to a
/// human instead (the `vike-cli secrets` commands do).
pub fn load_workspace_secrets_at(settings_dir: Option<&str>) -> HashMap<String, String> {
    load_workspace_secrets_at_checked(settings_dir).0
}

/// **Whether the store this map came from could be OPENED** — the one fact
/// [`load_workspace_secrets_at`] deliberately swallows, handed back as DATA for the callers that
/// cannot afford to swallow it.
///
/// ⚠ **Two ways to get an EMPTY map, and they are not the same event.** An ABSENT store is the
/// ordinary unconfigured state: the empty map is a real measurement, `0 credentials` is true, and
/// the live gate (no creds ⇒ every venue stays paper) is working as designed. A store that EXISTS
/// and will not open produces the SAME empty map for the opposite reason — nothing was measured at
/// all. The root `CLAUDE.md` states the rule these two must obey: *"Those two must never look the
/// same to an operator, because a permissions bug wearing the 'not configured' answer looks exactly
/// like a correct fresh install while every venue drops to paper for a different reason."*
///
/// An EXEC path may legitimately collapse them — it degrades to paper either way, and that failure
/// surfaces at the venue. A path that RENDERS A COUNT may not: a UI folding the empty map into
/// `0 set` states a measured number it did not measure. [`StoreHealth::Unreadable`] is how such a
/// caller knows to render "unknown" instead of a zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StoreHealth {
    /// The store ANSWERED. [`Source::None`] — no store on this box — is this arm too: the empty
    /// map it returns is the honest answer to a question that was actually asked.
    #[default]
    Readable,
    /// The store EXISTS and could not be opened. The map is empty for a reason that is NOT "no
    /// credentials", so any count folded from it is unknown rather than zero.
    ///
    /// The text is [`SecretsError`]'s own `Display` — a path and an OS reason, never file
    /// contents — so it is safe to log, print and render verbatim, which is the whole point of
    /// that type (its doc says so).
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
/// about the store it came from can never describe two different reads of two different files.
///
/// This is the whole implementation; the infallible wrapper above is `.0` of it, so the `tracing`
/// line and the empty-map degradation are unchanged for every existing caller.
///
/// ⚠ **No `venue_setting` row reaches this map, and none has since decision 0095's Task 7.** The
/// rows were FOLDED into it under their legacy credential names — here until 2026-09-22, inside the
/// store after — because every venue reader looked those names up. The readers take the rows as
/// `vike_secrets::venue_setting::VenueSettings` now, the fold is retired, and a credential row
/// still carrying a setting's old name is refused at boot
/// (`vike_config::refuse_stranded_venue_settings`).
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
///
/// This is the function a BINARY calls. A root that already built `std::env::vars().collect()` (for
/// the store root, the policy layer, the reconcile gate) answers the credential question from that
/// same sweep:
///
/// ```ignore
/// let env: HashMap<String, String> = std::env::vars().collect();
/// let vars = credentials::load_workspace_secrets_from_env(&env);
/// ```
///
/// Exactly ONE fact is taken out of `env` and nothing else: `VIKE_SETTINGS_DIR`, the settings
/// DIRECTORY, honoured by `vike_secrets::project_settings_dir_from`. That is the escape hatch a
/// deployment uses to name its settings directory outright; absent — the normal case — the runtime
/// walk answers.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map.** The two have the same
/// Rust type and opposite meanings: the credential map is this function's OUTPUT, and a store
/// cannot name where it itself lives.
///
/// PURE — it performs no environment read of its own, which is what keeps every root's credential
/// resolution testable without mutating process state, and why calling it adds no `Layer::Library`
/// row to `vike_ops::settings`.
pub fn load_workspace_secrets_from_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    load_workspace_secrets_from_env_checked(env).0
}

/// [`load_workspace_secrets_from_env`], plus [`StoreHealth`] — the sweep-taking twin of
/// [`load_workspace_secrets_at_checked`], for a composition root whose UI RENDERS A COUNT of what
/// the store holds.
///
/// Same single fact out of `env`, same purity, same one read. `load_workspace_secrets_from_env` is
/// `.0` of this, which is why the `VIKE_SETTINGS_DIR` literal below is the crate's ONE spelling of
/// that read rather than a second one the settings registry would have to learn about.
pub fn load_workspace_secrets_from_env_checked(
    env: &HashMap<String, String>,
) -> (HashMap<String, String>, StoreHealth) {
    load_workspace_secrets_at_checked(
        // The literal, not `vike_secrets::SETTINGS_DIR_ENV`: `vike_ops::scan`'s map-lookup sweep
        // resolves constants CRATE-wide, so an imported one would make this read invisible to the
        // settings registry — a declared read is the point. The two spellings are pinned equal by
        // `tests/settings_dir_spellings.rs`.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
    )
}

/// **The DECLARED credentials and nothing else**, with the error and the provenance returned
/// instead of swallowed — the scoped twin of [`try_load_workspace_secrets_at`].
///
/// Owner ruling, 2026-09-16: restrict what a process materialises. A caller that needs one or two
/// names declares them as a [`KeyScope`] and the process never holds the rest; what a core dump, a
/// panic payload or a future logging bug can reach shrinks to the declaration.
///
/// ⚠ **The store's three findings are logged from HERE, exactly as they are for the whole-map
/// read**, through the same `warn_once`. That placement is the point rather than a convenience: a
/// scoped caller is no less entitled to be told that its store is world-readable, that a
/// pre-one-store `.env` is sitting beside a project with no store, or that the settings database is
/// shadowing the file it is still hand-editing. A scoped path that skipped this function would lose
/// all three silently, which is the property `crates/vike-bridge-core/tests/shadowed_store_is_logged.rs`
/// exists to hold.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn try_load_workspace_secrets_scoped_at(
    settings_dir: Option<&str>,
    scope: &KeyScope,
) -> Result<ScopedSecrets, SecretsError> {
    let scoped = vike_secrets::resolve_project_scoped(settings_dir, scope)?;
    // The same three findings, the same types, the same `warn_once` — see that function's doc for
    // why each one prints paths and never a value.
    if let Some(w) = &scoped.warning {
        warn_once(&w.to_string());
    }
    if let Some(w) = &scoped.legacy {
        warn_once(&w.to_string());
    }
    if let Some(w) = &scoped.shadowed {
        warn_once(&w.to_string());
    }
    Ok(scoped)
}

/// [`try_load_workspace_secrets_scoped_at`], infallible — the scoped twin of
/// [`load_workspace_secrets_at`].
///
/// An unreadable store degrades to a scope in which every DECLARED name answers
/// [`Lookup::AbsentFromStore`] — the live gate, byte-identical in effect to the empty map the
/// whole-map reader produces — while an UNDECLARED name still answers [`Lookup::NotDeclared`]. The
/// two failures stay distinguishable through the degradation, which is the whole reason the scoped
/// answer is a type rather than a map.
///
/// The failure is LOUD in the log and silent in the return value, exactly as
/// [`load_workspace_secrets_at_checked`]'s is, and for the same reason.
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
/// calls.
///
/// Exactly ONE fact is taken out of `env` and nothing else: `VIKE_SETTINGS_DIR`, the settings
/// DIRECTORY. PURE, for the same reason and with the same consequence — calling it adds no
/// `Layer::Library` row to `vike_ops::settings`.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map**, and a `scope` is not an
/// env sweep either: the two have the same shape as name lists and opposite meanings.
#[must_use]
pub fn load_workspace_secrets_scoped_from_env(
    env: &HashMap<String, String>,
    scope: &KeyScope,
) -> ScopedSecrets {
    load_workspace_secrets_scoped_at(
        // The literal, not `vike_secrets::SETTINGS_DIR_ENV`, for the reason
        // `load_workspace_secrets_from_env_checked` states at its own copy of this line.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
        scope,
    )
}

/// **Which ACCOUNTS the credential store holds** — the database's own answer, from the one
/// process-environment sweep a composition root already owns.
///
/// The account twin of [`load_workspace_secrets_from_env`], and the door the mount/arming side
/// reaches the `account` table through: `vike-mount` links no `vike-secrets` of its own, so this
/// crate is where the store surfaces for it, exactly as it already is for the credential map.
///
/// ```ignore
/// let env: HashMap<String, String> = std::env::vars().collect();
/// match credentials::load_workspace_accounts_from_env(&env)? {
///     Accounts::Known(rows)          => /* the store answered; `rows` may be empty */,
///     Accounts::Unanswerable(reason) => /* ask the key names — `accounts_in_store` */,
/// }
/// ```
///
/// Exactly ONE fact is taken out of `env` and nothing else: `VIKE_SETTINGS_DIR`, the settings
/// DIRECTORY — the same fact, read the same way, as the credential loader above, so the two cannot
/// resolve different stores. PURE: it performs no environment read of its own, so calling it adds
/// no `Layer::Library` row to `vike_ops::settings`.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map.** Same trap as its sibling:
/// the two have the same Rust type and opposite meanings.
///
/// # ⚠ Three answers, and the third one is the point
///
/// It is `Result<Accounts, _>`, and [`Accounts`] itself has two arms, so there are three outcomes
/// and NONE of them may be collapsed into another:
///
/// * `Ok(Accounts::Known(rows))` — the store carries the table. An EMPTY `rows` means *this store
///   knows its accounts and has none*, which is a real answer.
/// * `Ok(Accounts::Unanswerable(_))` — this box has no `account` table: no settings database, or
///   one older than the table. Its credentials are present under their legacy key NAMES and
///   `vike_model::account_keys::accounts_in_store` is the reader that applies. Treating this as an
///   empty list would assert *this venue has no accounts* about a fully-configured store, and
///   downstream that reads as *this venue has no credentials* — every venue silently on paper.
/// * `Err(_)` — the store EXISTS and would not open. Loud, never an absence.
///
/// There is deliberately **no infallible twin** here, where [`load_workspace_secrets_at`] has one:
/// an empty credential map degrades safely because it IS the live gate, and an empty account list
/// carries no such guarantee.
///
/// # ⚠ It ARMS A VENUE — as of 2026-09-15, and this section said the opposite
///
/// It read *"Nothing mounts, arms, signs or reconciles from this yet"*, with
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §12's bar on joining
/// `arm_addresses_accounts` as the reason. Both halves went out of date with #1845: dukascopy's
/// mount resolves WHICH ACCOUNT — and therefore which LEGAL ENTITY an order reaches — from the
/// `account` rows this function returns (its `make_engine_for_account` arm then;
/// `crates/bridges/dukascopy/src/mount.rs` since the venue mount contract), and that venue joined
/// `arm_addresses_accounts` when it did. A stale "changes no behaviour" on a reader that decides a
/// counterparty is the worst kind of stale doc, so it is corrected rather than softened.
///
/// **Who calls it, and where the answer goes**: a composition root, once, beside its credential
/// load — `crates/vike-tradehub/src/tradehub_cli.rs` — and the result is carried to the mount and to
/// the arming projection as `vike_mount::MountPolicy::accounts`
/// (`crate::account_directory::AccountDirectory::read`, which takes this `Result` and its key-name
/// twin [`load_workspace_account_keys_from_env`] verbatim, errors included). A root that does not
/// read it leaves that field UNREAD, under which every labelled dukascopy account is refused by name
/// and the default account is byte-identical — the same answer an unmigrated box gets.
pub fn load_workspace_accounts_from_env(
    env: &HashMap<String, String>,
) -> Result<Accounts, SecretsError> {
    load_workspace_accounts_at(
        // The literal, not `vike_secrets::SETTINGS_DIR_ENV` — see the identical read in
        // `load_workspace_secrets_from_env` above for why the constant is not imported here.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
    )
}

/// [`load_workspace_account_keys_at`] from the one process-environment sweep a composition root
/// already owns — the key-NAME twin of [`load_workspace_accounts_from_env`], and the second half of
/// what `crate::account_directory::AccountDirectory::read` takes.
///
/// A caller needs BOTH: the rows say which accounts exist, and the key names say which CREDENTIAL
/// FAMILY each row owns — which for dukascopy's two demo rows is the only fact that separates them,
/// and therefore the one that decides the broker. See [`load_workspace_account_keys_at`] for why the
/// answer is `Option` and why nothing it returns can be a credential.
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
/// override (or `None` to let the project walk answer).
///
/// One line over `vike_secrets::resolve_accounts`, which is the same walk, the same override and
/// the same `Backend` probe the credential reader and the credential writer both ask — so this
/// function cannot resolve a different store than the map does.
pub fn load_workspace_accounts_at(settings_dir: Option<&str>) -> Result<Accounts, SecretsError> {
    vike_secrets::resolve_accounts(settings_dir)
}

/// **The credential key NAMES each `account` row owns** — the companion
/// [`load_workspace_accounts_at`] forces, surfaced above this crate for the same reason the reader
/// itself is: `vike-mount` links no `vike-secrets` of its own.
///
/// # Why a caller needs BOTH
///
/// An [`Account`] is `(id, venue, tier, label, venue_account_id, …)`, and for the pair this matters
/// most for — dukascopy's two demo rows — every one of those cells is identical except `id`, since
/// the owner refused a label on either. The fact that separates them is each row's own credential
/// key NAMES, and therefore its OWNER PREFIX: `DUKASCOPY_DEMO1_` against `DUKASCOPY_DEMO2_`. That
/// prefix is what `vike_dukascopy::resolve_account` maps onto a `vike_dukascopy::DukascopyAccount`,
/// i.e. onto which LEGAL ENTITY an order reaches.
///
/// ⚠ **NAMES ONLY, structurally.** `vike_secrets::read_account_keys` selects `name` and `field` and
/// never `credential.value`, so nothing reachable from the returned map — no row, no `Debug`, no
/// error — can be a credential. A caller may print every field of it.
///
/// `Ok(None)` means EXACTLY what [`Accounts::Unanswerable`] means — a store with no `account` table
/// to key — and a caller may not merge it with `Ok(Some(empty))`, which is *the table is there and
/// no row owns a live credential name*. Same `Backend` probe as the reader above, on the same
/// `settings_dir`, so the two cannot describe different stores.
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
/// This is also where the store's findings become log lines — its PERMISSION exposure, a leftover
/// PREDECESSOR store beside a project that has none, and the credential FILE the settings database
/// now shadows. `vike-secrets` returns all three as data because it carries no logging dependency;
/// both wrappers here route through this function, so no caller of either can forget to surface
/// them.
///
/// ⚠ **That placement is the point, not a convenience.** The seven composition roots read
/// credentials through [`load_workspace_secrets_from_env`], and a probe each of them had to remember
/// to call is one each of them can forget — the argument `vike_config::refuse_removed_project_file`
/// makes for folding its check into the loader rather than leaving a fourth chance to forget lying
/// around. One site, every root, including any root added later.
pub fn try_load_workspace_secrets_at(
    settings_dir: Option<&str>,
) -> Result<(HashMap<String, String>, Source), SecretsError> {
    let resolved = vike_secrets::resolve_project(settings_dir)?;
    if let Some(w) = &resolved.warning {
        // `PermissionWarning` prints paths and an octal mode, never a credential — and on the
        // `Finding::Symlink` arm the second path is the link's TARGET, still not a value.
        warn_once(&w.to_string());
    }
    if let Some(w) = &resolved.legacy {
        // `LegacyStoreWarning` prints two PATHS and never opened either file. It is set only when
        // the store is ABSENT — the case where this function otherwise returns an empty map in
        // complete silence and every venue drops to paper.
        warn_once(&w.to_string());
    }
    if let Some(w) = &resolved.shadowed {
        // ⚠ THE SAME SHAPE AND THE SAME PLACE as the two above, and it had to land here or nowhere:
        // `ShadowedStore` was RETURNED AS DATA by `vike-secrets` (which carries no logging
        // dependency) and CONSUMED BY NOTHING — no binary logged it and no CLI verb printed it — so
        // the entire operator-facing mitigation for the database shadowing `secrets.env` was
        // unreachable. An operator whose hand-edit stopped being read got no sentence anywhere
        // telling them why.
        //
        // This function is where every composition root's credential read converges, which is the
        // argument the two warnings above already make for themselves: a probe each root had to
        // remember to call is one each root can forget. Two PATHS, no value — the type's `Display`
        // formats nothing else.
        warn_once(&w.to_string());
    }
    // ⚠ There was a FOURTH finding here until decision 0095's Task 7: the store's fold reported
    // the names a `venue_setting` row and a credential row BOTH answered for, and this logged them
    // as a half-done move. The fold is retired — a venue setting reaches no credential map — so
    // there is nothing left to collide, and a credential row still carrying a setting's old name
    // is a boot REFUSAL instead (`vike_config::refuse_stranded_venue_settings`).
    Ok((resolved.secrets.into_map(), resolved.source))
}

/// **Every store finding, logged ONCE PER DISTINCT MESSAGE for the life of the process.**
///
/// ⚠⚠ **This function exists because the three `tracing::warn!`s above were per-CALL, and one
/// caller reads credentials PER FRAME.** `crates/vike-desktop/src/main.rs`'s `Connections` arm calls
/// `workspace_credentials_checked()` on every frame the window is drawn — deliberately, so the rail
/// stays live after a save or an external edit — and on a box migrated to the settings database
/// `resolved.shadowed` is the NORMAL, permanent state rather than a fault. MEASURED on the owner's
/// box on 2026-09-15: **36,979 copies of one `ShadowedStore` line in a single day, 14 MB, ~100% of
/// that day's WARN channel.** That is the `trace log fills disk` failure class the root `CLAUDE.md`
/// records at 341 GB, and it cost more than disk: it BURIED the channel, so the owner's own attempt
/// to diagnose a credential save by reading the log could not have worked.
///
/// Keyed on the RENDERED MESSAGE rather than on a plain `Once`, because the message embeds the
/// paths: two different settings directories in one process (a test, a tool that probes several)
/// each still get their own line, and a genuinely NEW finding is never swallowed by an old one.
/// The set is small and bounded by the number of distinct stores a process touches.
///
/// ⚠ A finding is a STATE, not an event — nothing downstream counts these lines, and none of the
/// three is a per-read measurement. Suppressing the repeats therefore loses no information:
/// `vike-cli secrets` prints the finding as DATA on every invocation (it returns from
/// `vike_secrets::resolve_project`, not from here), which is the surface an operator asks.
fn warn_once(message: &str) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    // A poisoned mutex here must not lose the warning: recover the guard rather than panicking in
    // a logging path.
    let fresh = match seen.lock() {
        Ok(mut g) => g.insert(message.to_string()),
        Err(poisoned) => poisoned.into_inner().insert(message.to_string()),
    };
    if fresh {
        tracing::warn!("{message}");
    }
}

/// Resolve a venue's attribution code from an already-loaded `.env` map, validated against the
/// venue's [`vike_model::attribution::AttributionMechanic`]. Tries both `{VENUE}_BROKER_CODE` and
/// `{VENUE}_BUILDER_CODE`. Returns `None` when the key is absent, the code fails the mechanic's
/// constraints (too long / venue has no mechanism), or is empty — so an unset/invalid code stamps
/// nothing (byte-identical). Reads the passed map, NOT process env (the `.env`-not-exported gotcha).
///
/// Both names come from [`vike_model::credential_keys::attribution_key`], so
/// [`vike_model::credential_keys::attribution_keys`] enumerates exactly what this reads — including
/// the mechanic-less early return, which is why a venue classified
/// [`None`](vike_model::attribution::AttributionMechanic::None) contributes no key to that grid and
/// gets no registry row: its key provably cannot be looked up.
pub fn attribution_code_from(vars: &HashMap<String, String>, venue: &str) -> Option<String> {
    let mech = vike_model::attribution::attribution_for(venue);
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
mod workspace_store_tests {
    use super::*;

    // The reader's OWN unit tests live with it in `vike_secrets::dotenv`. What is asserted here is
    // the RE-EXPORT contract: the historical `vike_bridge_core::credentials::…` paths still resolve
    // and still behave, so none of the ~179 existing call sites changed.

    #[test]
    fn the_store_reader_is_still_reachable_at_its_historical_path() {
        let m = parse_dotenv("A=1\n# c\nB=\"two\"\n");
        assert_eq!(m.get("A").map(String::as_str), Some("1"));
        assert_eq!(m.get("B").map(String::as_str), Some("two"));
    }

    #[test]
    fn loads_without_panic_and_returns_map() {
        // The store may or may not exist in a given checkout; either way the helper must return a
        // map (empty when absent) and never panic.
        let vars = load_workspace_dotenv();
        let _n: usize = vars.len();
    }

    #[test]
    fn the_store_is_secrets_env_inside_the_projects_settings_dir() {
        let path = workspace_dotenv_path();
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(vike_secrets::SECRETS_FILE));
        // The re-export must resolve the same directory `vike-secrets` does.
        assert_eq!(
            path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            Some(vike_model::state_path::PROJECT_SETTINGS_DIR)
        );
    }

    #[test]
    fn the_secrets_sibling_returns_the_same_shape_and_never_panics() {
        // A CI checkout has no store, so this exercises the absent arm: a `HashMap<String, String>`,
        // never a panic.
        let vars: HashMap<String, String> = load_workspace_secrets_at(None);
        let _n: usize = vars.len();
    }

    /// The two entry points a binary can take are the SAME resolution — the settings-directory
    /// escape hatch is the only thing `..._from_env` adds, and an environment that does not name
    /// one must therefore be indistinguishable from passing `None`.
    #[test]
    fn the_env_entry_point_is_the_no_override_entry_point_when_nothing_names_a_directory() {
        assert_eq!(
            load_workspace_secrets_from_env(&HashMap::new()),
            load_workspace_secrets_at(None)
        );
        assert_eq!(load_workspace_secrets_at(None), load_workspace_dotenv());
    }

    /// …and so is the parameterised RAW reader, whose reason to exist is that a TEST binary can
    /// name the settings directory the way `..._from_env` lets a daemon name it. Passing no
    /// override must stay indistinguishable from the historical call.
    #[test]
    fn the_parameterised_raw_reader_is_the_historical_one_when_nothing_names_a_directory() {
        assert_eq!(load_workspace_dotenv_from(None), load_workspace_dotenv());
    }

    /// ⚠ **THE TWO EMPTY MAPS ARE TELLABLE APART.** An ABSENT store and a store that EXISTS and
    /// will not open both return zero credentials; the root `CLAUDE.md` rule is that those must
    /// never look the same to an operator. [`StoreHealth`] is the fact that distinguishes them,
    /// and this asserts it over the REAL resolver rather than over a constructed value.
    ///
    /// The unreadable store is planted as a DIRECTORY named `secrets.env` — present on disk,
    /// refused by `read_to_string` on every platform this tree builds for, and requiring no
    /// permission bit a Windows box would ignore.
    ///
    /// Reddens on `load_workspace_secrets_at_checked` folding its error arm back into
    /// `StoreHealth::Readable`, which is the exact shape that lets a UI render a measured `0 set`
    /// for a store it never opened.
    #[test]
    fn an_unreadable_store_is_distinguishable_from_an_absent_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let sd = settings.to_str().expect("utf-8 temp path");

        // ABSENT: no `secrets.env` at all. Empty map, and the store ANSWERED.
        let (map, health) = load_workspace_secrets_at_checked(Some(sd));
        assert!(map.is_empty(), "an absent store holds nothing");
        assert_eq!(health, StoreHealth::Readable, "absent is an ANSWER, not a fault");
        assert!(health.is_readable());

        // PRESENT AND UNOPENABLE: same empty map, opposite verdict.
        std::fs::create_dir(settings.join(vike_secrets::SECRETS_FILE)).expect("plant a directory");
        let (map, health) = load_workspace_secrets_at_checked(Some(sd));
        assert!(map.is_empty(), "the degradation is unchanged — an empty map either way");
        assert!(!health.is_readable(), "…and THAT is the whole point: {health:?}");
        let StoreHealth::Unreadable(why) = &health else { panic!("{health:?}") };
        assert!(why.contains("could not be read"), "the fault is quotable: {why}");
        assert!(why.contains(vike_secrets::SECRETS_FILE), "…and names the store: {why}");
    }

    /// The checked reader is the ONE implementation: the infallible wrapper every existing caller
    /// uses is its `.0`, so the two can never resolve different stores.
    #[test]
    fn the_infallible_reader_is_the_checked_one_without_its_verdict() {
        assert_eq!(load_workspace_secrets_at(None), load_workspace_secrets_at_checked(None).0);
        let env = HashMap::new();
        assert_eq!(
            load_workspace_secrets_from_env(&env),
            load_workspace_secrets_from_env_checked(&env).0
        );
    }
}

#[path = "attribution_code_tests.rs"]
#[cfg(test)]
mod attribution_code_tests;

/// **The ENUMERATION and the READ, held equal.**
///
/// The blind spot these tests close is stated in [`vike_model::credential_keys`]' module doc: a key
/// built with `format!` and handed to a map `get` appears as no literal anywhere, so
/// `vike_ops::settings::SETTINGS` could neither declare it nor notice that it did not.
/// `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared` now demands a
/// row for every key the enumeration produces — which is only worth anything if the enumeration is
/// the set this loader actually reads. That is what this module asserts, twice over: by SET
/// equality against the loader's own composition, and by BEHAVIOUR, one key at a time.
#[path = "generated_key_grid_tests.rs"]
#[cfg(test)]
mod generated_key_grid_tests;

// ---------------------------------------------------------------------------------------------
// The settings database's account classification — schema 2
// ---------------------------------------------------------------------------------------------

/// **Which ACCOUNT a credential key name belongs to, and what its `field` is** — the classification
/// `vike_secrets`' schema 2 needs and cannot derive for itself.
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` is the authority for every row of
/// every table below; each one cites the section that put it there.
///
/// # ⚠ Why it lives HERE and not in the store
///
/// `vike-secrets` sits in tier 15, whose rule is *nothing above rank 10*
/// (`crates/vike-ops/tests/layer_gate.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary`), and this crate declares `layer = 25`
/// — so it cannot be named there, and it declares `vike-secrets` itself, making the reverse edge a
/// cycle as well as a band violation. The derivation therefore arrives there as a CLOSURE.
///
/// ⚠ **The reason this used to give was different and is now FALSE**: *"`vike-secrets` declares NO
/// `vike-*` dependency … The derivation needs [`vike_model::account_keys`] and the venue roster, so
/// it arrives there as a CLOSURE, exactly as `is_node_key` already does."*
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted 2026-09-20)
/// admitted `vike-model` into that crate, so `account_keys` and the roster ARE reachable from it —
/// `crates/vike-secrets/src/db.rs`'s `ensure_venue_rows` iterates `vike_model::venues::VENUES`
/// outright. What keeps THIS function a closure is the layer bound above, which 0072 does not
/// touch; `is_node_key` no longer shares it, so the two seams are no longer one argument.
/// `crates/vike-secrets/src/schema.rs`'s module doc is the store-side statement of the same thing.
/// The two consumers this crate's `vike-secrets` edge exists for (itself, which owns the venue
/// transport stack, and `vike-cli`, which is DataFusion-free and transport-free) still link it
/// without dragging the other in — the `vike-model` edge gains neither of them a transport, a TLS
/// stack or a query engine.
///
/// ⚠ **This said "this crate is the lowest layer that can see both halves (layer 30)", and that is
/// false.** `vike-config` declares layer 20 and `vike-boot` layer 25, and both take `vike-model`
/// AND `vike-secrets` as NORMAL dependencies, so either could host this function without
/// `crates/vike-ops/tests/layer_gate.rs` objecting. The reason it lives here is not altitude: this
/// is where the credential-key VOCABULARY already is — `credential_keys`, the tier tables, and the
/// venue-scoped and attribution families this file's own tables cite — and putting it in the
/// settings loader instead would put the venue roster inside `vike-config`. What the altitude claim
/// was reaching for is the second half of the sentence, which is true on its own:
/// every production credential writer —
/// `crates/bridges/ctrader/src/token_store.rs`, `crates/vike-connections/src/env_write.rs`,
/// `crates/vike-cli/src/cmd/secrets.rs` — already links it. It is deliberately NOT behind this
/// crate's `full` feature, for the reason the `credentials` module is not: `vike-cli` takes this
/// crate with `default-features = false`.
///
/// # The three answers
///
/// §5.1's three cases, as [`vike_secrets::Placement`]:
///
/// * **account-scoped** — `account_id` set, `venue` NULL. `OKX_DEMO_API_SECRET`, `POLY_PRIVATE_KEY`.
/// * **venue-scoped** — `account_id` NULL, `venue` set. `CTRADER_CLIENT_ID`/`_CLIENT_SECRET` carry
///   no tier token at all because they are APPLICATION credentials: one OAuth application, shared
///   by the demo account and any future live one. ⚠ `venue` here means *belongs to this venue's
///   plane*, not *issued by this venue*.
/// * **infrastructure** — both NULL. `CLOUDFLARE_API_TOKEN`, `FINNHUB_API_KEY`. Reached as the
///   CATCH-ALL (§11 step 6) and therefore reported as unrecognised, which is the point: a store
///   that declines to speak about an unusual name is how an unusual name rots.
#[must_use]
pub fn classify_credential_name(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    // A MACHINE-scoped venue setting's legacy name, matched WHOLE — see the function. First, because
    // `POLY_RATE_GATE` would otherwise reach the hand-map's `POLY_` row as an account field.
    if let Some(class) = classify_machine_setting(name) {
        return class;
    }

    // The POLY_* family SPLITS key by key — §5.2 — so it is consulted before the prefix table
    // below, whose `POLY_` row is its fallback.
    if let Some(class) = classify_poly(name) {
        return class;
    }

    // ⚠ **The ACCOUNT LABEL is split off BEFORE the hand-map, and it was not.** The suffix grammar
    // `{VENUE}_{TIER}{SUFFIX}__{LABEL}` is live in this tree for exactly this family:
    // `crates/bridges/polymarket/src/exec_plane/recon_client.rs`'s `signature_type_for_account` reads
    // `POLY_SIGNATURE_TYPE__{LABEL}` and refuses any fallback to the unlabelled key. Matching the
    // hand-map against the WHOLE name filed every such key against the venue's DEFAULT account with
    // the label buried inside `field` — so `POLY_FUNDER__HEDGE` became a `FUNDER__HEDGE` field of
    // the unlabelled account, silently losing BOTH per-`(venue, field)` table lookups with it
    // (`POLY_SIGNATURE_TYPE__X` was marked `secret = 1` where its unlabelled twin is `0`, and
    // `POLY_FUNDER__X` lost its `BookIdentifier` row, i.e. its place on §7's work-list). The venue
    // grammar arm below has always split first, through `account_ref_from_key`; this is that arm's
    // rule applied to the families the grammar misses.
    //
    // A malformed suffix (`AccountLabel::parse` refusing it) is NOT an error here: the name is left
    // whole and falls through to the catch-all, where it is REPORTED as unrecognised rather than
    // filed against a guess.
    let (base, label) = match vike_model::account_keys::split_account_key(name) {
        Ok(split) => (split.base, split.label.text().map(str::to_string)),
        Err(_) => (name, None),
    };

    // §11 step 2 — the venue families `account_ref_from_key` misses, hand-mapped with a reason.
    for (head, token, venue, tier, discriminator, _why) in HAND_MAPPED_ACCOUNTS {
        if let Some(field) = base.strip_prefix(&hand_mapped_prefix(head, token))
            && !field.is_empty()
        {
            return account_row(
                AccountKey {
                    venue: (*venue).to_string(),
                    tier: (*tier).to_string(),
                    // ⚠ The DISCRIMINATOR is dropped the moment a LABEL is present, and the two are
                    // not additive. The discriminator exists only to tell apart two accounts that
                    // `(venue, tier, label)` cannot — `vike_secrets::AccountKey::discriminator`
                    // says so at the field — and a labelled account is one `(venue, tier, label)`
                    // answers for by construction. Keeping both would key the account on a tuple
                    // the `account` table cannot reproduce (the discriminator reaches no column),
                    // so a re-run would miss its own row and try to INSERT a second account at the
                    // same `UNIQUE (venue, tier, label)`.
                    discriminator: if label.is_some() {
                        None
                    } else {
                        discriminator.map(str::to_string)
                    },
                    label: label.clone(),
                },
                field,
            );
        }
    }

    // §5.1's venue-scoped rows: an APPLICATION credential, and the attribution family, which
    // `account_ref_from_key` also answers `None` for (the token after the venue is no tier) and
    // which §11 step 6's catch-all would otherwise file as the DEPLOYMENT's.
    if let Some((venue, field)) = venue_scoped(name) {
        return Classification {
            placement: Placement::Venue(venue.to_string()),
            secret: is_secret(venue, field),
            field: field.to_string(),
            recognised: true,
            pending_move: pending_move(venue, field),
        };
    }

    // The venue grammar itself.
    if let Some(reference) = vike_model::account_keys::account_ref_from_key(name)
        && let Some(field) = field_after_tier(name, reference.venue)
    {
        return account_row(
            AccountKey {
                venue: reference.venue.to_string(),
                // ⚠ NOT `to_ascii_lowercase()`. §4.4 of the settings-store-plane design renamed the
                // account tier `sim` to `paper` while deliberately leaving the credential key's
                // `SIM` token alone, so the two vocabularies are no longer the same word in a
                // different case and this is the ONE site that joins them —
                // `vike_secrets::account_tier_of_key_token`, which names the legacy `MAINNET` ->
                // `LIVE` mapping as its own precedent. A bare lowercase here answers `sim`, which
                // the `account` table's own CHECK now refuses.
                tier: vike_secrets::account_tier_of_key_token(reference.tier),
                label: reference.label.text().map(str::to_string),
                discriminator: None,
            },
            field,
        );
    }

    // §11 step 6 / §11.1 — never dropped, never guessed at, always REPORTED.
    Classification::unrecognised(name)
}

/// Assemble an account-scoped row, applying the two per-`(venue, field)` tables.
fn account_row(key: vike_secrets::AccountKey, field: &str) -> vike_secrets::Classification {
    let secret = is_secret(&key.venue, field);
    let pending_move = pending_move(&key.venue, field);
    vike_secrets::Classification {
        secret,
        pending_move,
        field: field.to_string(),
        placement: vike_secrets::Placement::Account(key),
        recognised: true,
    }
}

/// **THE COLLISION CHECK §6.2 SAYS THE MIGRATION OWES** — every legacy name a settings row would
/// render that a LIVE credential row already holds.
///
/// Returns `rendered name -> the settings key that renders it`, empty when there is no collision.
///
/// # ⚠ Why this cannot be a database constraint
///
/// `credential.name` and the names [`venue_setting_names`] renders are ONE namespace, and SQLite
/// can constrain a name across two tables no more than across two databases:
/// `credential_one_live_name` holds inside `credential` alone. So nothing in the schema stops a
/// settings row from rendering a name a live credential row already holds — and a caller folding
/// both into one map would then hold one key with TWO candidate values, resolved by insertion
/// order. §6.2 states that consequence and puts the check here, in code, rather than in the DDL.
///
/// # ⚠ What a collision MEANS, and why the answer is refuse rather than prefer
///
/// It means the move is half-done: the value exists in both homes and the two may disagree. Picking
/// a winner would be choosing which of an operator's two statements to honour, silently, on the
/// path that decides which account a key signs for. The migration refuses instead, names both
/// sides, and leaves every row where it is — the same disposition `vike-cli config adopt` takes
/// when its own comparison is not identical.
///
/// # ⚠ It is the LIVE names that matter
///
/// A SUPERSEDED credential row keeps its name (that is what `credential_one_live_name`'s `WHERE`
/// admits — a tier alias filed beside its twin), and a superseded row is not what any reader
/// resolves. Handing this the whole `name` column instead of the live set would refuse a migration
/// over a row nothing reads. The caller supplies the live set; `crate::db::read_table` is what
/// answers with exactly that.
#[must_use]
pub fn rendered_name_collisions(
    settings_keys: &[String],
    live_credential_names: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for key in settings_keys {
        let Some((venue, tier, field)) = parse_venue_setting_key(key) else { continue };
        for name in venue_setting_names(&venue, tier.as_deref(), &field) {
            if live_credential_names.contains(&name) {
                out.insert(name, key.clone());
            }
        }
    }
    out
}

/// **A MACHINE-scoped venue setting's legacy credential name** — `POLY_RATE_GATE`,
/// `BINANCE_TRADE_LITE_FILL`, `OKX_MARK_STREAMS` — matched WHOLE, and DERIVED rather than listed:
/// the fields are `vike_model::venue_fields::VENUE_FIELDS` and the names are the store's own
/// renderer's ([`venue_setting_names`]), so a field declared later is classified by being declared.
///
/// A credential row under one of these names is configuration in the wrong table: decision 0095's
/// Task 7 retired the fold that read it there, and the boot refuses it
/// (`vike_config::refuse_stranded_venue_settings`). So it is a `PendingMove::VenueSetting`, which
/// `vike-cli secrets move-venue-config` relocates, and its `secret` flag is the catalog row's —
/// `venue.polymarket.socks_proxy` may carry `user:password@`, and the flag said `false` here while
/// the catalog said `true` until Task 7 made the catalog the one answer.
///
/// ⚠ **WHOLE, so a LABELLED spelling never matches.** No reader ever looked one up — one process has
/// one egress (`vike_polymarket::declare_egress`) and the toggles are one per machine — so a
/// labelled key falls through to the account grammar as an ordinary account row, which the move
/// verb does not touch and the boot does not refuse
/// (`vike_secrets::venue_setting::stranded_venue_setting_names` matches these whole for the same
/// reason). `crates/vike-bridge-core/tests/credential_classification.rs`'s
/// `the_boot_refusal_and_the_move_verb_agree_on_every_name` holds the two equal.
fn classify_machine_setting(name: &str) -> Option<vike_secrets::Classification> {
    use vike_secrets::{Classification, PendingMove, Placement};
    vike_model::venue_fields::VENUE_FIELDS.iter().filter(|f| !f.tier_scoped).find_map(|f| {
        let field = f.field.to_ascii_uppercase();
        let named = venue_setting_names(f.venue, None, &field).iter().any(|n| n == name);
        named.then(|| Classification {
            placement: Placement::Venue(f.venue.to_string()),
            field,
            secret: f.secret,
            recognised: true,
            pending_move: Some(PendingMove::VenueSetting),
        })
    })
}

/// **§5.2 — the `POLY_*` family, key by key**, for what is left of it once
/// [`classify_machine_setting`] has taken the machine-scoped settings: the builder code. Each row
/// was MEASURED against the code that reads it rather than inferred from the prefix.
/// ⚠ The row is matched on the FIELD — the part after the store head — for the reason
/// [`HAND_MAPPED_ACCOUNTS`] gives in full: a whole env-prefixed literal in a `src/` file is read by
/// `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence this file READS that variable,
/// and `POLY_BUILDER_CODE` is measured as read by NOTHING, so it has no `SETTINGS` row and must not
/// acquire one from here.
///
/// ⚠ **It matches the WHOLE name, deliberately, so a LABELLED key never reaches this arm.** It
/// answers `Placement::Venue`, which carries no account and therefore has nowhere to put a label —
/// matching a label-stripped base here would drop the operator's label in silence. A labelled key
/// falls through to the hand-map instead, where the label becomes the account's, and that is the
/// right answer for the one labelled spelling this family actually has
/// (`POLY_SIGNATURE_TYPE__{LABEL}`, read by `crates/bridges/polymarket/src/exec_plane/recon_client.rs`'s
/// `signature_type_for_account`).
fn classify_poly(name: &str) -> Option<vike_secrets::Classification> {
    use vike_secrets::{Classification, Placement};
    let field = name.strip_prefix(&hand_mapped_prefix("POLY", ""))?;
    if field == "BUILDER_CODE" {
        // §5.2: venue-scoped, `secret = 0` — and DEAD. `vike_model::attribution::attribution_for`
        // composes the venue's OWN attribution spelling, so no reader exists for this one. It stays
        // in `credential` precisely BECAUSE it is dead: moving an unread value into a table that
        // asserts *this is machine or tier configuration* would be asserting a fact from no
        // evidence.
        return Some(Classification {
            placement: Placement::Venue("polymarket".to_string()),
            field: field.to_string(),
            secret: false,
            recognised: true,
            pending_move: None,
        });
    }
    None
}

/// **§5.1's venue-scoped names** — `account_id` NULL, `venue` set.
///
/// Two families, and the second is a FORWARD hazard rather than a migration-day fact:
///
/// * cTrader's OAuth APPLICATION pair, which carries no tier token because one application serves
///   every account of the venue;
/// * the ATTRIBUTION family (`{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`).
///   `account_ref_from_key` answers `None` for these — its own doc says so, *the token after the
///   venue is no tier* — so §11 step 6's catch-all would file a future `HYPERLIQUID_BUILDER_CODE`
///   as the DEPLOYMENT's, which is wrong by §5.1's own reasoning and inconsistent with
///   `POLY_BUILDER_CODE`'s row above. None is in the live store today; this row is what stops the
///   first one being misfiled.
fn venue_scoped(name: &str) -> Option<(&'static str, &str)> {
    // The head is COMPOSED from the roster id rather than spelled, for the reason
    // [`HAND_MAPPED_ACCOUNTS`] gives: an env-prefixed literal in a `src/` file is read by
    // `crates/vike-ops/tests/settings_registry.rs`' sweep as a variable this file reads, and a
    // prefix is not a variable.
    if let Some(field) = name.strip_prefix(&hand_mapped_prefix("CTRADER", ""))
        && (field == "CLIENT_ID" || field == "CLIENT_SECRET")
    {
        return Some(("ctrader", field));
    }
    for venue in vike_model::VENUES {
        let head = format!("{}_", venue.to_uppercase());
        if let Some(field) = name.strip_prefix(&head)
            && (field == "BROKER_CODE" || field == "BUILDER_CODE")
        {
            return Some((venue, field));
        }
    }
    None
}

// vike:new-venue:note a CONFORMING venue needs NO row in any of the per-venue tables this file carries — that is what makes them exception tables rather than a roster. `{venue}` needs an `is_secret` row only for a key that holds no secret, and a `pending_move` row only for the key that IS its book or its machine/tier config; if its store keys do NOT parse as `{VENUE}_{TIER}_{FIELD}` it also needs a `vike_secrets::venue_setting::HAND_MAPPED_ACCOUNTS` row, which lives one crate down since 2026-09-22 (dukascopy bakes an account index into the tier token; alpaca spells its tier `SANDBOX`). Getting all three wrong for a conforming venue costs nothing: the defaults are account-scoped, `secret = 1`, and no pending move: crates/vike-bridge-core/src/credentials.rs's `classify_credential_name`
/// **§6 — the rows MEASURED as holding no secret.**
///
/// Keyed on `(venue, field)` rather than on the whole NAME, so a tier other than the one the live
/// store happens to carry is classified the same way: `IBKR_LIVE_HOST` is as much a host as
/// `IBKR_DEMO_HOST` is.
///
/// `true` is the default for everything else, deliberately: a value wrongly marked non-secret is a
/// worse error than one wrongly marked secret.
///
/// ⚠ A TIER-scoped declared venue field (`IBKR_DEMO_HOST`, `FXCM_LIVE_URL`, `DUKASCOPY_DEMO1_SERVER`)
/// takes its flag from the settings catalog (`vike_model::venue_fields`), the one answer for it
/// since decision 0095's Task 7 — these were §6's six config-shaped names spelled out here. A
/// MACHINE-scoped field reached here is a LABELLED spelling no reader ever looked up, and stays at
/// the conservative default; its unlabelled name is [`classify_machine_setting`]'s.
fn is_secret(venue: &str, field: &str) -> bool {
    if let Some(f) =
        vike_secrets::venue_setting::declared_field(venue, field).filter(|f| f.tier_scoped)
    {
        return f.secret;
    }
    !matches!(
        (venue, field),
        // The two of §6's twelve config-shaped names that STAY in `credential` at `secret = 0`:
        //
        // `IBKR_*_CLIENT_ID` — `crates/bridges/vike-ibkr/src/config.rs`'s
        // `load_ibkr_config_for_account` states it: the client ids must DIFFER between two live
        // TWS connections or the second socket is evicted by the gateway. So the GATEWAY is the
        // machine and the client id is a per-ACCOUNT SLOT inside it, which is the one thing
        // `venue_setting` is defined not to hold.
        ("ibkr", "CLIENT_ID")
            // `POLY_SIGNATURE_TYPE` — per-WALLET by the venue's own code:
            // `crates/bridges/polymarket/src/exec_plane/recon_client.rs`'s `signature_type_for_account` reads
            // `POLY_SIGNATURE_TYPE__{LABEL}` and refuses any fallback to the unlabelled key.
            // §13 item 1 put the placement to the owner and the signature kept it here.
            | ("polymarket", "SIGNATURE_TYPE")
    )
}

/// **The rows §7 and §6 will MOVE, and this change deliberately does not.**
///
/// Reported by name so the next change takes its work-list out of a run rather than out of prose.
/// `crates/vike-secrets/src/schema.rs`'s module doc carries the sequencing rule (§12: *until one
/// [ordering] is [written], THE ROWS MAY NOT MOVE*) that makes this a report rather than an act.
///
/// Keyed `(venue, field)` for the same reason [`is_secret`] is.
fn pending_move(venue: &str, field: &str) -> Option<vike_secrets::PendingMove> {
    use vike_secrets::PendingMove;
    match (venue, field) {
        // §7's book identifiers — the value IS the book, and folds into
        // `account.venue_account_id`. ⚠ FOUR of these are load-bearing for AUTHENTICATION rather
        // than for a label: `IG_*_IDENTIFIER` and `FXCM_*_USER` are the LOGIN, `ASTER_*_USER` is
        // the master address on the wire beside a separate agent signer, and `POLY_FUNDER` is the
        // maker and (for a `Poly1271` account) the contract an order is signed against. A renderer
        // bug there is a failed login or a rejected signature, which is why §7 requires the
        // renderer to be covered BEFORE the fold ships.
        ("oanda" | "alpaca" | "ctrader", "ACCOUNT_ID")
        | ("ibkr", "ACCOUNT")
        | ("hyperliquid", "ACCOUNT_ADDRESS")
        | ("aster" | "fxcm", "USER")
        | ("ig", "IDENTIFIER")
        | ("polymarket", "FUNDER") => Some(PendingMove::BookIdentifier),
        // §6 / ruling 10 — every TIER-scoped declared venue field (ibkr's gateway, fxcm's host and
        // connection, dukascopy's server), from the settings catalog rather than spelled: since
        // decision 0095's Task 7 the boot refuses a credential row under one of these names
        // (`vike_secrets::venue_setting::stranded_venue_setting_names`), and the move verb must
        // move exactly what the boot refuses. The machine-scoped fields are
        // [`classify_machine_setting`]'s, matched whole before this table is reached.
        _ if vike_secrets::venue_setting::declared_field(venue, field)
            .is_some_and(|f| f.tier_scoped) =>
        {
            Some(PendingMove::VenueSetting)
        }
        _ => None,
    }
}

/// §4.4's account-scoped derivation: the name with `{VENUE}_{TIER}_` removed, **in the STORE's own
/// tier spelling**.
///
/// It cannot simply strip `{VENUE}_{AccountRef::tier}_`, because that field is NORMALIZED: a legacy
/// `ASTER_MAINNET_API_KEY` classifies as tier `LIVE` while the name carries `MAINNET`. So the token
/// is found in the NAME.
///
/// ⚠ **The token is found in the LABEL-STRIPPED base, so `field` EXCLUDES a `__LABEL` suffix** —
/// `OKX_DEMO_API_KEY__HEDGE` yields `API_KEY`, not `API_KEY__HEDGE`. That is this function's first
/// line (`split_account_key(name)`) and it is deliberate.
///
/// ⚠ **This doc used to state the OPPOSITE** — *"The returned slice is taken from `name` rather
/// than from the label-stripped base, so a labelled key keeps its `__LABEL` suffix inside
/// `field`"* — and went on to argue that this is *"what makes `Classification::owner_prefix` (the
/// name minus the field) the same `{VENUE}_{TIER}_` for a labelled key as for its unlabelled
/// sibling, and therefore what stops one account's prefix resolving another account's row."* Both
/// halves are wrong, and the second is wrong in a way worth spelling out because it names the
/// hazard and then describes CAUSING it: if a labelled key's owner prefix were the same
/// `{VENUE}_{TIER}_` as its unlabelled sibling's, then `vike_secrets`' prefix lookup — which is
/// keyed on exactly that string — would resolve `OKX_DEMO_API_KEY__HEDGE` to the DEFAULT account's
/// row, filing one account's credential against another. What the code actually does is leave the
/// label out, so `name.strip_suffix(field)` fails for a labelled key and `owner_prefix` is `None`;
/// the account is then found by `(venue, tier, label)`, which is unique for it. That is the
/// disposition `vike_secrets::Classification::owner_prefix`'s own doc describes.
fn field_after_tier<'a>(name: &'a str, venue: &str) -> Option<&'a str> {
    let base = vike_model::account_keys::split_account_key(name).ok()?.base;
    let head = format!("{}_", venue.to_uppercase());
    let rest = base.strip_prefix(&head)?;
    for tier in vike_model::credential_keys::CREDENTIAL_TIERS
        .iter()
        .chain(vike_model::credential_keys::LEGACY_CREDENTIAL_TIERS.iter())
    {
        if let Some(field) = rest.strip_prefix(&format!("{tier}_"))
            && !field.is_empty()
        {
            return Some(field);
        }
    }
    None
}

/// **The renderer, proved to be the INVERSE of the classifier rather than merely resembling it.**
///
/// ⚠ **Only the half that needs THE CLASSIFIER is still here.** The renderer itself moved to
/// `vike_secrets::venue_setting::venue_setting_names` on 2026-09-22 and took its pure-grammar tests with it —
/// what renders, what parses back, and which spellings an operator types are that module's own
/// properties now (`crates/vike-secrets/src/venue_setting.rs`). What cannot move is anything that
/// asks `classify_credential_name` or `rendered_name_collisions` a question, because those live
/// here: the inverse proof, and the collision check the migration owes.
#[path = "venue_setting_renderer_tests.rs"]
#[cfg(test)]
mod venue_setting_renderer_tests;

// ⚠ A `load_workspace_dotenv_scoped` lived here until decision 0095's Task 7. It was
// `vike_secrets::load_workspace_dotenv_scoped` plus one thing — the LOG line for the store fold's
// half-done-move finding (a `venue_setting` row and a credential row answering one name). Task 7
// retired the fold, so the finding no longer exists, and what was left was the same function
// under a second name with no caller: deleted rather than kept as the alias CLAUDE.md forbids.
// A credential row under a venue setting's legacy name is refused at boot now
// (`vike_config::refuse_stranded_venue_settings`).
