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
//! | [`load_workspace_dotenv_from`] | a TEST binary, which owns its own env read and wants the raw reader (no `tracing` findings) |
//!
//! All four return the SAME `HashMap<String, String>`, so every downstream consumer
//! (`load_credentials_from`, `attribution_code_from`, every venue `config.rs` loader) is written
//! against one shape.
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
}

/// **The half-credential gate**: a venue whose signer REQUIRES a passphrase must not load without
/// one, because loading is what mounts it live.
#[cfg(test)]
mod required_passphrase_tests {
    use super::*;

    /// Seed a var map through the module's OWN naming site, so these fixtures can never drift
    /// from the names the loader reads. (It also keeps `{VENUE}_{TIER}_API_*` literals out of this
    /// file: `vike_ops::scan`'s map-lookup sweep harvests env-shaped string literals wherever they
    /// appear, and a fixture spelling one is indistinguishable from a real read.)
    fn seed(pairs: &[(&str, &str, Option<&str>)]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (venue, tier, pass) in pairs {
            let (k, s, p) = names_for_prefix(&prefix_for(venue, tier));
            out.insert(k, "k".to_string());
            out.insert(s, "s".to_string());
            if let Some(pass) = pass {
                out.insert(p, (*pass).to_string());
            }
        }
        out
    }

    /// THE DEFECT, pinned: key + secret and NO passphrase must resolve like ABSENT credentials on
    /// OKX. Before this gate the same map returned `Some`, mounted live, and every signed request
    /// came back `OK-ACCESS-PASSPHRASE cannot be empty`.
    #[test]
    fn okx_key_and_secret_without_a_passphrase_do_not_load() {
        let half = seed(&[("okx", "DEMO", None)]);
        assert!(
            load_credentials_from("okx", Environment::Demo, &half).is_none(),
            "okx key+secret with no passphrase must resolve like absent credentials (stay PAPER)"
        );
        // …and the SAME map plus the passphrase loads, so the gate is the passphrase and nothing
        // else (a gate that refused both ways would be indistinguishable from a broken loader).
        let full = seed(&[("okx", "DEMO", Some("p"))]);
        let c = load_credentials_from("okx", Environment::Demo, &full)
            .expect("all three credentials present ⇒ loads");
        assert_eq!(c.api_key, "k");
        assert_eq!(c.passphrase.as_deref(), Some("p"));
    }

    /// A BLANK passphrase is the same as an absent one. The wire cannot tell the difference — the
    /// signer sends an empty `OK-ACCESS-PASSPHRASE` header either way.
    #[test]
    fn a_blank_okx_passphrase_is_an_absent_one() {
        for blank in ["", "   ", "\t"] {
            let v = seed(&[("okx", "DEMO", Some(blank))]);
            assert!(
                load_credentials_from("okx", Environment::Demo, &v).is_none(),
                "a blank passphrase ({blank:?}) must not count as configured"
            );
        }
    }

    /// ⚠ **NOT a blanket requirement.** The venues whose signers take no passphrase must load with
    /// exactly two credentials, byte-identically to before the gate existed — demanding one
    /// everywhere would strand every working binance/bybit/deribit mount on paper. The roster's
    /// remaining venues ride bespoke shapes this loader never sees, so this covers every venue for
    /// which the generic path is the live gate.
    #[test]
    fn passphrase_free_venues_still_load_with_key_and_secret_alone() {
        for venue in ["binance", "bybit", "deribit"] {
            let v = seed(&[(venue, "DEMO", None)]);
            let c = load_credentials_from(venue, Environment::Demo, &v)
                .unwrap_or_else(|| panic!("{venue} must load with key+secret alone"));
            assert!(c.passphrase.is_none());
            assert_eq!(missing_required_passphrase(venue, Environment::Demo, &v), None);
        }
    }

    /// The `Optional` row is a real distinction, not a spelling: aster READS the field (as its
    /// agent signer address) and derives it when blank, so folding `Optional` into `Required`
    /// would strand it. Its own loader owns its var shape; this asserts the generic path's row.
    #[test]
    fn asters_optional_passphrase_never_gates() {
        let v = seed(&[("aster", "DEMO", None)]);
        assert!(load_credentials_from("aster", Environment::Demo, &v).is_some());
        assert_eq!(missing_required_passphrase("aster", Environment::Demo, &v), None);
    }

    /// The finding is the NAME of the missing variable — and NEVER a value. The two credentials
    /// that WERE found must not appear in it either: this string goes to a log.
    #[test]
    fn the_finding_names_the_variable_and_echoes_no_value() {
        let mut v = seed(&[("okx", "DEMO", None)]);
        for val in v.values_mut() {
            *val = "secret-material-Zx81".to_string();
        }
        let name = missing_required_passphrase("okx", Environment::Demo, &v)
            .expect("a half-configured okx store reports its missing credential");
        assert_eq!(name, "OKX_DEMO_API_PASSPHRASE");
        assert!(!name.contains("Zx81"), "no credential VALUE may reach the finding: {name}");
    }

    /// Nothing configured stays SILENT — the ordinary unconfigured state is not a finding, and an
    /// operator who wrote no keys must not be told a credential is missing.
    #[test]
    fn an_unconfigured_venue_reports_nothing() {
        assert_eq!(missing_required_passphrase("okx", Environment::Demo, &HashMap::new()), None);
        // key alone, then secret alone — still nothing to report
        let full = seed(&[("okx", "DEMO", Some("p"))]);
        for name in full.keys() {
            let one: HashMap<String, String> = HashMap::from([(name.clone(), "v".to_string())]);
            assert_eq!(
                missing_required_passphrase("okx", Environment::Demo, &one),
                None,
                "one credential alone is UNCONFIGURED, not half-configured ({name})"
            );
        }
    }

    /// The legacy `MAINNET` tier is a real fallback, not a formality: a `Live` load whose LIVE set
    /// is half-configured but whose MAINNET set is COMPLETE still loads — and reports nothing,
    /// because nothing is wrong.
    #[test]
    fn a_complete_legacy_tier_satisfies_a_half_configured_live_tier() {
        let v = seed(&[("okx", "LIVE", None), ("okx", "MAINNET", Some("lp"))]);
        assert!(
            load_credentials_from("okx", Environment::Live, &v).is_some(),
            "the complete legacy tier still loads"
        );
        assert_eq!(missing_required_passphrase("okx", Environment::Live, &v), None);
        // …but with the legacy tier ALSO missing its passphrase, nothing loads and the finding
        // names the tier `load_credentials_from` tried FIRST.
        let v = seed(&[("okx", "LIVE", None), ("okx", "MAINNET", None)]);
        assert!(load_credentials_from("okx", Environment::Live, &v).is_none());
        // The expected name is BUILT the way the loader builds it — `names_for_prefix` over the
        // primary `{VENUE}_{TIER}` prefix — so the assertion is still exact (it pins the LIVE tier
        // over the MAINNET one) with no env-shaped literal for `vike_ops::scan`'s map-lookup sweep
        // to mistake for a real read.
        let (_, _, want) = names_for_prefix(&prefix_for("okx", Environment::Live.as_str()));
        assert_eq!(missing_required_passphrase("okx", Environment::Live, &v), Some(want));
    }
}

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
#[cfg(test)]
mod account_tests {
    use std::collections::BTreeSet;

    use vike_model::VENUES;
    use vike_model::account_keys::{ACCOUNT_SEPARATOR, AccountLabel};
    use vike_model::credential_keys::credential_keys;

    use super::*;

    /// Both tiers one `Environment` is read at, primary first — the loader's own order.
    fn tiers_of(env: Environment) -> Vec<&'static str> {
        let mut out = vec![env.as_str()];
        out.extend(env.legacy_str());
        out
    }

    fn every_environment() -> [Environment; 3] {
        [Environment::Sim, Environment::Demo, Environment::Live]
    }

    fn label(text: &str) -> AccountLabel {
        AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
    }

    /// One venue/tier/account's three names filled in, values supplied per slot. `None` leaves the
    /// slot OUT of the map, which is what an unset variable is.
    fn seed_account(
        venue: &str,
        tier: &str,
        label: &AccountLabel,
        key: Option<&str>,
        secret: Option<&str>,
        passphrase: Option<&str>,
    ) -> HashMap<String, String> {
        let (k, s, p) = names_for_account(&prefix_for(venue, tier), label);
        let mut out = HashMap::new();
        for (name, value) in [(k, key), (s, secret), (p, passphrase)] {
            if let Some(v) = value {
                out.insert(name, v.to_string());
            }
        }
        out
    }

    /// **THE STEP-1 CONTRACT, over the whole grid.** For every roster venue and every tier the
    /// loader reads, the DEFAULT account's three names are the names this module composed before
    /// an account existed — the same `String`s, not merely compatible ones.
    ///
    /// Folded over both sides rather than asserted on an example: a label appended for the default
    /// account would change one name somewhere, and an example would have to be the one that moved.
    #[test]
    fn the_default_account_composes_the_historical_names() {
        for venue in VENUES {
            for env in every_environment() {
                for tier in tiers_of(env) {
                    let prefix = prefix_for(venue, tier);
                    assert_eq!(
                        names_for_account(&prefix, &AccountLabel::Default),
                        names_for_prefix(&prefix),
                        "{venue}/{tier}: the default account must compose today's names verbatim"
                    );
                }
            }
        }
    }

    /// …and the LOAD PATH is the historical one, proven through the two public functions rather
    /// than through the names again: same credentials, same finding, for every venue and tier.
    ///
    /// ⚠ **The fixture is seeded through [`names_for_prefix`], NOT through [`names_for_account`],
    /// and that was a CORRECTION rather than a first draft.** Seeded the other way, this test
    /// survived a MEASURED mutation that gave the default account a `__DEFAULT` suffix: the loader
    /// and the fixture both moved, both sides answered `None`, and an equality between two `None`s
    /// is an equality. Seeding from the historical composer is what makes the fixture an
    /// INDEPENDENT witness — and the `must_load` assertion below is the other half, because two
    /// `None`s would otherwise still agree.
    #[test]
    fn the_default_account_load_path_is_the_historical_one() {
        for venue in VENUES {
            for env in every_environment() {
                for tier in tiers_of(env) {
                    for pass in [None, Some("p")] {
                        let (k, s, p) = names_for_prefix(&prefix_for(venue, tier));
                        let mut vars: HashMap<String, String> =
                            [(k, "k".to_string()), (s, "s".to_string())].into_iter().collect();
                        if let Some(pass) = pass {
                            vars.insert(p, pass.to_string());
                        }
                        let historical = load_credentials_from(venue, env, &vars);
                        let account =
                            load_credentials_for_account(venue, env, &AccountLabel::Default, &vars);
                        assert_eq!(
                            historical.is_some(),
                            account.is_some(),
                            "{venue}/{tier}: the default account and the historical path disagree"
                        );
                        // …and it LOADS. Without this, a change that moved the default account's
                        // names would leave both sides `None` and both sides agreeing.
                        let must_load = pass.is_some() || !venue_passphrase(venue).is_required();
                        if must_load {
                            let c = account.as_ref().unwrap_or_else(|| {
                                panic!("{venue}/{tier}: today's key names must still load")
                            });
                            assert_eq!(c.api_key, "k");
                            assert_eq!(c.api_secret, "s");
                        }
                        if let (Some(a), Some(b)) = (&historical, &account) {
                            assert_eq!(a.api_key, b.api_key);
                            assert_eq!(a.api_secret, b.api_secret);
                            assert_eq!(a.passphrase, b.passphrase);
                        }
                        assert_eq!(
                            missing_required_passphrase(venue, env, &vars),
                            missing_required_passphrase_for_account(
                                venue,
                                env,
                                &AccountLabel::Default,
                                &vars
                            ),
                            "{venue}/{tier}: the finding differs between the two spellings"
                        );
                    }
                }
            }
        }
    }

    /// A LABELLED account loads from its OWN keys — and the two accounts are sealed off from each
    /// other in BOTH directions. The reverse direction is the one that matters: a labelled load
    /// that fell back to the unlabelled key would sign account `ALT`'s orders with the default
    /// account's credentials, silently.
    #[test]
    fn a_labelled_account_reads_its_own_keys_and_never_the_default_ones() {
        let alt = label("ALT");
        let labelled = seed_account("bybit", "DEMO", &alt, Some("alt-k"), Some("alt-s"), None);
        let c = load_credentials_for_account("bybit", Environment::Demo, &alt, &labelled)
            .expect("a complete labelled set loads");
        assert_eq!(c.api_key, "alt-k");
        assert_eq!(c.api_secret, "alt-s");

        // …and the DEFAULT account is not configured by it.
        assert!(
            load_credentials_from("bybit", Environment::Demo, &labelled).is_none(),
            "a labelled key must not configure the default account"
        );

        // The mirror: a store holding only the default account's keys arms no labelled account.
        let default_only =
            seed_account("bybit", "DEMO", &AccountLabel::Default, Some("k"), Some("s"), None);
        assert!(
            load_credentials_for_account("bybit", Environment::Demo, &alt, &default_only).is_none(),
            "a labelled account must NOT fall back to the default account's credentials"
        );
        // …nor does a DIFFERENT label reach ALT's keys.
        assert!(
            load_credentials_for_account("bybit", Environment::Demo, &label("OTHER"), &labelled)
                .is_none(),
            "one label must not read another label's credentials"
        );
    }

    /// The legacy `MAINNET` fallback is the account's too — a pre-rename store that grew a second
    /// account keeps the same tier behaviour, rather than acquiring a special case.
    #[test]
    fn a_labelled_account_keeps_the_legacy_tier_fallback() {
        let alt = label("ALT");
        let vars = seed_account("binance", "MAINNET", &alt, Some("legacy"), Some("s"), None);
        let c = load_credentials_for_account("binance", Environment::Live, &alt, &vars)
            .expect("the legacy tier answers for a labelled account too");
        assert_eq!(c.api_key, "legacy");
    }

    /// ⚠ **AN INCOMPLETE LABELLED ACCOUNT IS AN ABSENT ONE** — the SAME behaviour an incomplete
    /// default account has had since this loader existed, and deliberately not a new failure mode.
    ///
    /// Asserted as an EQUALITY between the two accounts rather than as `is_none()` on the labelled
    /// one, because the requirement is that they MATCH: if the default account's answer for a
    /// half-configured store ever changes, this test moves with it instead of pinning a copy.
    #[test]
    fn an_incomplete_labelled_account_behaves_exactly_like_an_incomplete_default_one() {
        let alt = label("ALT");
        for (key, secret) in [(Some("k"), None), (None, Some("s")), (None, None)] {
            for venue in ["bybit", "okx"] {
                let labelled = seed_account(venue, "DEMO", &alt, key, secret, None);
                let defaulted =
                    seed_account(venue, "DEMO", &AccountLabel::Default, key, secret, None);
                let a = load_credentials_for_account(venue, Environment::Demo, &alt, &labelled)
                    .is_some();
                let b = load_credentials_from(venue, Environment::Demo, &defaulted).is_some();
                assert_eq!(
                    a, b,
                    "{venue}: a labelled account missing key={key:?} secret={secret:?} must \
                     resolve exactly as the default account does"
                );
                assert!(!a, "{venue}: an incomplete account must stay unconfigured");
                // …and it is SILENT: the only finding this module produces is the passphrase one,
                // and a store missing key or secret is UNCONFIGURED, not half-configured.
                assert_eq!(
                    missing_required_passphrase_for_account(
                        venue,
                        Environment::Demo,
                        &alt,
                        &labelled
                    ),
                    None,
                    "{venue}: an unconfigured labelled account must report nothing"
                );
            }
        }
    }

    /// The one finding that DOES exist, for a labelled account: OKX's required passphrase, named
    /// as the LABELLED variable — the one the operator has to write.
    #[test]
    fn a_labelled_okx_without_a_passphrase_reports_the_labelled_variable() {
        let alt = label("ALT");
        let vars = seed_account("okx", "DEMO", &alt, Some("k"), Some("s"), None);
        assert!(
            load_credentials_for_account("okx", Environment::Demo, &alt, &vars).is_none(),
            "okx key+secret with no passphrase must stay unconfigured for a labelled account too"
        );
        let (_, _, want) = names_for_account(&prefix_for("okx", "DEMO"), &alt);
        assert_eq!(
            missing_required_passphrase_for_account("okx", Environment::Demo, &alt, &vars),
            Some(want.clone()),
            "the finding must name the LABELLED variable, not the default account's"
        );
        assert!(want.contains(ACCOUNT_SEPARATOR), "{want} carries no account separator");
        // …and the same map plus the labelled passphrase loads, so the gate is the passphrase.
        let full = seed_account("okx", "DEMO", &alt, Some("k"), Some("s"), Some("pp"));
        let c = load_credentials_for_account("okx", Environment::Demo, &alt, &full)
            .expect("all three labelled credentials present ⇒ loads");
        assert_eq!(c.passphrase.as_deref(), Some("pp"));
    }

    /// HARD, and it does not weaken for a second account: no credential VALUE reaches `Debug`.
    #[test]
    fn a_labelled_accounts_credentials_never_reach_debug() {
        let alt = label("ALT");
        let vars = seed_account(
            "okx",
            "DEMO",
            &alt,
            Some("key-material-Zx81"),
            Some("secret-material-Qw42"),
            Some("pass-material-Rt93"),
        );
        let c = load_credentials_for_account("okx", Environment::Demo, &alt, &vars)
            .expect("a complete labelled set loads");
        let rendered = format!("{c:?}");
        for leaked in ["key-material-Zx81", "secret-material-Qw42", "pass-material-Rt93"] {
            assert!(!rendered.contains(leaked), "a credential reached Debug: {rendered}");
        }
        // The four-character tail is the ONE disclosed fragment, and it is the same one the
        // default account discloses — the redaction did not change shape for a labelled account.
        assert!(rendered.contains("***Zx81"), "the redacted rendering changed: {rendered}");
        assert!(rendered.contains("passphrase=set"), "{rendered}");
    }

    /// [`account_var`]'s own contract: the labelled name, trimmed, blank-is-absent, no fallback.
    ///
    /// The base key here is a GENERIC one composed by the module itself — the real bespoke shapes
    /// are covered in `tests/account_credential_shapes.rs`, through their own bridges.
    #[test]
    fn account_var_reads_the_labelled_name_only() {
        let alt = label("ALT");
        let (base, _, _) = names_for_prefix(&prefix_for("bybit", "DEMO"));
        let labelled = account_key(&base, &alt);

        let vars: HashMap<String, String> =
            [(labelled.clone(), "  spaced  ".to_string())].into_iter().collect();
        assert_eq!(account_var(&vars, &base, &alt), Some("spaced"), "the value must be trimmed");
        assert_eq!(account_var(&vars, &base, &AccountLabel::Default), None, "no reverse fallback");

        let default_only: HashMap<String, String> =
            [(base.clone(), "v".to_string())].into_iter().collect();
        assert_eq!(account_var(&default_only, &base, &alt), None, "no fallback to the default");
        assert_eq!(account_var(&default_only, &base, &AccountLabel::Default), Some("v"));

        for blank in ["", "   ", "\t"] {
            let v: HashMap<String, String> =
                [(labelled.clone(), blank.to_string())].into_iter().collect();
            assert_eq!(account_var(&v, &base, &alt), None, "a blank value ({blank:?}) is absent");
        }
        assert_eq!(account_var(&HashMap::new(), &base, &alt), None);
    }

    /// ⚠ **The ENUMERATED grid stays the DEFAULT account's, and that is a decision.** A label is an
    /// operator-chosen name over an unbounded set, so there is no finite grid of labelled keys for
    /// `crates/vike-ops/tests/settings_registry.rs` to demand rows for. This pins that the
    /// enumeration was not quietly widened into something that cannot be complete — see this
    /// module's own doc for what replaces it (the STORE is the enumeration).
    #[test]
    fn the_enumerated_grid_stays_the_default_accounts() {
        let grid: BTreeSet<String> = credential_keys().into_iter().collect();
        assert!(!grid.is_empty(), "the grid must not be empty — this gate would assert nothing");
        for key in &grid {
            assert!(
                !key.contains(ACCOUNT_SEPARATOR),
                "{key}: the enumerable grid is the default account's, and a labelled key cannot be \
                 enumerated (the label set is unbounded)"
            );
        }
    }
}

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

/// **Fold the `setting` rows' rendered names into a credential map** — the READ half of §6.2's
/// ordering (A), at the one layer where both pieces are in scope.
///
/// Returns the names BOTH tables answered for, sorted; empty is the normal state in both
/// directions (before the move nothing renders, after a complete one nothing is left behind).
///
/// # ⚠ Why it lives HERE and not in `vike-secrets`
///
/// `vike_secrets::read_table_folded` is the primitive and takes the renderer as a closure, because
/// that crate declares no `vike-*` dependency. Composing a legacy name needs
/// [`HAND_MAPPED_ACCOUNTS`] and the venue roster, which live in THIS crate — so this is where the
/// closure can actually be supplied, and threading one through every `resolve_*` call site would
/// have been the alternative.
///
/// # ⚠ `credential` wins a collision
///
/// A name both tables answer for means the move is half-done. Keeping the credential value
/// resolves to what the box resolved BEFORE the move started — the only choice that cannot alter
/// live behaviour while the tables disagree. `vike_secrets::read_table_folded`'s own doc carries
/// the full argument; this function applies the same rule over an already-loaded map so a caller
/// that read through some other path still gets it.
fn fold_venue_settings(
    settings_dir: Option<&str>,
    map: &mut HashMap<String, String>,
) -> Vec<String> {
    // The SAME resolver the credential read above used, so the two cannot disagree about which
    // settings directory this box has — a second derivation is the defect `CREDENTIAL_STORE_PIN`
    // exists to ratchet down.
    let dir = vike_secrets::workspace_settings_dir_from(settings_dir);
    let Ok(vike_secrets::SettingsSource::Rows { rows, .. }) = vike_secrets::read_settings_in(&dir)
    else {
        // No database, or a store too old to carry the table: nothing has moved, so there is
        // nothing to fold and no collision to report.
        return Vec::new();
    };
    let mut collisions = Vec::new();
    for row in &rows.settings {
        let Some((venue, tier, field)) = parse_venue_setting_key(&row.key) else { continue };
        for name in venue_setting_names(&venue, tier.as_deref(), &field) {
            match map.entry(name.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => collisions.push(name),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(row.value.clone());
                }
            }
        }
    }
    collisions.sort_unstable();
    collisions.dedup();
    collisions
}

/// Fold, and SAY SO when both tables answered — the half-done move an operator must be told about.
///
/// Split from [`fold_venue_settings`] so the pure fold stays testable without a subscriber, which
/// is the same split every finding in this module already has.
fn fold_venue_settings_reported(settings_dir: Option<&str>, map: &mut HashMap<String, String>) {
    let collisions = fold_venue_settings(settings_dir, map);
    if !collisions.is_empty() {
        tracing::error!(
            names = ?collisions,
            "⚠ the credential store and the settings rows BOTH answer for these names — ruling \
             10's move is HALF DONE. The CREDENTIAL value is the one in force, which is what this \
             box resolved before the move started; nothing is guessed at. `vike-cli secrets \
             move-venue-config --dry-run` names what is left to move"
        );
    }
}
/// [`load_workspace_secrets_at`], plus [`StoreHealth`] — **ONE read**, so the map and the verdict
/// about the store it came from can never describe two different reads of two different files.
///
/// This is the whole implementation; the infallible wrapper above is `.0` of it, so the `tracing`
/// line and the empty-map degradation are unchanged for every existing caller.
pub fn load_workspace_secrets_at_checked(
    settings_dir: Option<&str>,
) -> (HashMap<String, String>, StoreHealth) {
    match try_load_workspace_secrets_at(settings_dir) {
        Ok((mut map, _source)) => {
            // ⚠ THE FOLD — §6.2 ordering (A). Ruling 10 takes ten config-shaped names out of
            // `credential` and into `setting` rows; this is what keeps every loader finding the
            // name it looks up, so the move has no reader-visible surface. On a box where nothing
            // has moved it is a no-op: no settings row renders a credential name.
            fold_venue_settings_reported(settings_dir, &mut map);
            (map, StoreHealth::Readable)
        }
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
/// `make_engine_for_account` arm resolves WHICH ACCOUNT — and therefore which LEGAL ENTITY an order
/// reaches — from the `account` rows this function returns, and that venue joined
/// `arm_addresses_accounts` when it did. A stale "changes no behaviour" on a reader that decides a
/// counterparty is the worst kind of stale doc, so it is corrected rather than softened.
///
/// **Who calls it, and where the answer goes**: a composition root, once, beside its credential
/// load — `crates/vike-tradehub/src/tradehub_cli.rs` — and the result is carried to the mount and to
/// the arming projection as `vike_mount::MountPolicy::accounts`
/// (`vike_mount::AccountDirectory::read`, which takes this `Result` and its key-name twin
/// [`load_workspace_account_keys_from_env`] verbatim, errors included). A root that does not read it
/// leaves that field UNREAD, under which every labelled dukascopy account is refused by name and the
/// default account is byte-identical — the same answer an unmigrated box gets.
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
/// what `vike_mount::AccountDirectory::read` takes.
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
/// prefix is what `vike_mount`'s dukascopy arm maps onto a `vike_dukascopy::DukascopyAccount`, i.e.
/// onto which LEGAL ENTITY an order reaches.
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
            Some(vike_secrets::SETTINGS_DIR)
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

#[cfg(test)]
mod attribution_code_tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn reads_broker_and_builder_code_keys() {
        let v =
            vars(&[("OKX_BROKER_CODE", "5328c82e5542BCDE"), ("POLYMARKET_BUILDER_CODE", "0xabc")]);
        assert_eq!(attribution_code_from(&v, "okx").as_deref(), Some("5328c82e5542BCDE"));
        assert_eq!(attribution_code_from(&v, "polymarket").as_deref(), Some("0xabc"));
    }

    #[test]
    fn absent_or_invalid_or_unmechanized_is_none() {
        // absent
        assert_eq!(attribution_code_from(&vars(&[]), "okx"), None);
        // present but too long for OKX's 16-char tag → rejected (treated as unset)
        assert_eq!(
            attribution_code_from(&vars(&[("OKX_BROKER_CODE", "way_too_long_broker_tag")]), "okx"),
            None
        );
        // venue with no order-level mechanism → None even if a stray key exists.
        //
        // ⚠ The stray key is COMPOSED, not spelled. Deribit is `AttributionMechanic::None`, so
        // `attribution_code_from` returns before it builds a key and that name is provably never
        // looked up — which is exactly why `vike_model::credential_keys::attribution_keys` leaves
        // it out of the grid. A bare literal here would hand it a `vike_ops::settings::SETTINGS`
        // row anyway (the registry's raw literal sweep has no call syntax to anchor on and cannot
        // tell a fixture from a read), and `vike-cli config show` would then report an unread key
        // as a real setting with a real source — positive confirmation of something false, the
        // defect CLAUDE.md's settings section calls worse than an unimplemented feature. Same
        // idiom, same reason, as `required_passphrase_tests`' `names_for_prefix(&prefix_for(…))`.
        let stray = attribution_key("deribit", BROKER_CODE_SUFFIX);
        assert_eq!(attribution_code_from(&vars(&[(stray.as_str(), "x")]), "deribit"), None);
    }
}

/// **The ENUMERATION and the READ, held equal.**
///
/// The blind spot these tests close is stated in [`vike_model::credential_keys`]' module doc: a key
/// built with `format!` and handed to a map `get` appears as no literal anywhere, so
/// `vike_ops::settings::SETTINGS` could neither declare it nor notice that it did not.
/// `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared` now demands a
/// row for every key the enumeration produces — which is only worth anything if the enumeration is
/// the set this loader actually reads. That is what this module asserts, twice over: by SET
/// equality against the loader's own composition, and by BEHAVIOUR, one key at a time.
#[cfg(test)]
mod generated_key_grid_tests {
    use std::collections::BTreeSet;

    use vike_model::VENUES;
    use vike_model::credential_keys::{
        ATTRIBUTION_SUFFIXES, CREDENTIAL_TIERS, LEGACY_CREDENTIAL_TIERS, attribution_key,
        attribution_keys, credential_keys,
    };

    use super::*;

    /// Every [`Environment`] variant. The `match` is EXHAUSTIVE on purpose and does nothing else: a
    /// new tier is then a COMPILE error here, which is how it acquires a grid entry and a registry
    /// row instead of becoming another silently-undeclared key family.
    fn every_environment() -> [Environment; 3] {
        let all = [Environment::Sim, Environment::Demo, Environment::Live];
        for env in all {
            match env {
                Environment::Sim | Environment::Demo | Environment::Live => {}
            }
        }
        all
    }

    /// Both tiers one `Environment` is read at, primary first — the loader's own order.
    fn tiers_of(env: Environment) -> Vec<&'static str> {
        let mut out = vec![env.as_str()];
        out.extend(env.legacy_str());
        out
    }

    /// THE EQUIVALENCE. The names this loader composes over the whole roster ARE
    /// `vike_model::credential_keys::credential_keys`.
    ///
    /// Two provenances, which is the whole point: the left side is folded from this file's own
    /// [`prefix_for`] and [`names_for_prefix`] — the exact two functions `load_credentials_from`
    /// calls — and the right side is the shared table's product over
    /// [`vike_model::VENUES`]. A gate that built its expectation from the table it is checking
    /// would gate nothing, which is the failure `vike_model::venues`'
    /// `roster_matches_the_bridge_crates` was rewritten to stop shipping.
    #[test]
    fn the_loader_reads_exactly_the_enumerated_credential_grid() {
        let mut composed: BTreeSet<String> = BTreeSet::new();
        for venue in VENUES {
            for env in every_environment() {
                for tier in tiers_of(env) {
                    let (key, secret, passphrase) = names_for_prefix(&prefix_for(venue, tier));
                    composed.extend([key, secret, passphrase]);
                }
            }
        }
        let enumerated: BTreeSet<String> = credential_keys().into_iter().collect();
        assert_eq!(
            composed, enumerated,
            "the credential grid and the loader's own composition have drifted — a key on one side \
             and not the other is either an undeclared read or a declared row nothing reads"
        );
    }

    /// …and every enumerated name is LOAD-BEARING, proven through the real `load_credentials_from`
    /// rather than through the composition again: removing one changes the answer.
    ///
    /// The passphrase is the interesting third: it is REQUIRED only where
    /// [`venue_passphrase`] says so, so dropping it either refuses the load (the required venues) or
    /// yields the same credentials with `passphrase: None`. Both prove the NAME was read; a name
    /// nothing looked up could do neither.
    #[test]
    fn every_enumerated_credential_key_changes_what_the_loader_returns() {
        let enumerated: BTreeSet<String> = credential_keys().into_iter().collect();
        for venue in VENUES {
            for env in every_environment() {
                for tier in tiers_of(env) {
                    let (key, secret, passphrase) = names_for_prefix(&prefix_for(venue, tier));
                    for name in [&key, &secret, &passphrase] {
                        assert!(enumerated.contains(name), "{name} is read but not enumerated");
                    }
                    let full: HashMap<String, String> = [
                        (key.clone(), "k".to_string()),
                        (secret.clone(), "s".to_string()),
                        (passphrase.clone(), "p".to_string()),
                    ]
                    .into_iter()
                    .collect();
                    let creds = load_credentials_from(venue, env, &full)
                        .unwrap_or_else(|| panic!("{venue}/{tier}: all three present must load"));
                    assert_eq!(creds.api_key, "k");
                    assert_eq!(creds.api_secret, "s");
                    assert_eq!(creds.passphrase.as_deref(), Some("p"));

                    for name in [&key, &secret] {
                        let mut one_short = full.clone();
                        one_short.remove(name);
                        assert!(
                            load_credentials_from(venue, env, &one_short).is_none(),
                            "{name} is enumerated but dropping it changed nothing — the live gate \
                             must refuse a half-configured venue"
                        );
                    }
                    let mut no_passphrase = full.clone();
                    no_passphrase.remove(&passphrase);
                    match load_credentials_from(venue, env, &no_passphrase) {
                        Some(c) => assert!(
                            c.passphrase.is_none(),
                            "{passphrase} is enumerated but its absence left a passphrase set"
                        ),
                        None => assert!(
                            venue_passphrase(venue).is_required(),
                            "{venue} refused a load without {passphrase} but its passphrase row \
                             does not require one"
                        ),
                    }
                }
            }
        }
    }

    /// The ATTRIBUTION twin, BOTH ways in one loop: a mechanised venue's two keys are enumerated
    /// AND are looked up by the real reader; an unmechanised venue's are neither.
    ///
    /// The second half is the one worth having. `attribution_code_from` returns before it builds a
    /// key when the venue has no mechanic, so declaring those names would claim a read that
    /// provably cannot happen — and the check that they stay OUT of the grid is what keeps
    /// `crates/vike-ops/tests/settings_registry.rs`'s row demand honest rather than merely large.
    #[test]
    fn the_attribution_grid_is_exactly_what_the_reader_looks_up() {
        let enumerated: BTreeSet<String> = attribution_keys().into_iter().collect();
        for venue in VENUES {
            let mechanised = !vike_model::attribution::attribution_for(venue).is_none();
            for suffix in ATTRIBUTION_SUFFIXES {
                let key = attribution_key(venue, suffix);
                // A short code every mechanic accepts (the tightest cap is OKX's 16-char tag, and
                // binance's coid-prefix budget is tighter still) — so a `None` below is the READ
                // failing, never the validation.
                let vars: HashMap<String, String> =
                    [(key.clone(), "abc".to_string())].into_iter().collect();
                assert_eq!(
                    enumerated.contains(&key),
                    mechanised,
                    "{key}: the grid must carry a mechanised venue's key and only that"
                );
                assert_eq!(
                    attribution_code_from(&vars, venue).as_deref(),
                    mechanised.then_some("abc"),
                    "{key}: the reader and the grid disagree about whether this key is read"
                );
            }
        }
    }

    /// The tier spellings are the SHARED table's. `vike-model` cannot see this enum and this crate
    /// must not re-spell the grid, so the two copies are pinned equal here — the same shape as
    /// `crates/vike-bridge-core/tests/settings_dir_spellings.rs`, which pins the settings-directory
    /// resolver's two copies for the same "neither crate can see the other" reason.
    #[test]
    fn the_environment_tiers_are_the_shared_table() {
        let mut from_enum: BTreeSet<&str> = BTreeSet::new();
        for env in every_environment() {
            from_enum.extend(tiers_of(env));
        }
        let from_table: BTreeSet<&str> =
            CREDENTIAL_TIERS.iter().chain(LEGACY_CREDENTIAL_TIERS.iter()).copied().collect();
        assert_eq!(
            from_enum, from_table,
            "`Environment`'s tier spellings and `vike_model::credential_keys`' tier tables have \
             drifted — a tier in one and not the other is a whole column of undeclared keys"
        );
    }
}

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
/// `vike-secrets` declares NO `vike-*` dependency — `crates/vike-secrets/Cargo.toml`'s layer-15
/// `leaf` tier, and two consumers (this crate, which owns the venue transport stack, and `vike-cli`,
/// which is DataFusion-free and transport-free) both link it without dragging the other in. The
/// derivation needs [`vike_model::account_keys`] and the venue roster, so it arrives there as a
/// CLOSURE, exactly as `is_node_key` already does.
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

    // The POLY_* family SPLITS key by key — §5.2 — so it is consulted before the prefix table
    // below, whose `POLY_` row is its fallback.
    if let Some(class) = classify_poly(name) {
        return class;
    }

    // ⚠ **The ACCOUNT LABEL is split off BEFORE the hand-map, and it was not.** The suffix grammar
    // `{VENUE}_{TIER}{SUFFIX}__{LABEL}` is live in this tree for exactly this family:
    // `crates/bridges/polymarket/src/recon_client.rs`'s `signature_type_for_account` reads
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
                tier: reference.tier.to_ascii_lowercase(),
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

/// **§11 step 2 — the venue families the key grammar misses**, each with its reason.
///
/// `(store head, store tier token, venue, tier, discriminator, why)`. The PREFIX is the first two
/// composed by [`hand_mapped_prefix`]; no prefix here is a prefix of another (`DUKASCOPY`+`DEMO1`
/// and `DUKASCOPY`+`DEMO2` differ at their token), so order decides nothing except that the
/// tokenless polymarket row is reached after [`classify_poly`] has had its say.
///
/// ⚠ **The head and the token are SEPARATE columns rather than one spelled prefix**, and that is
/// not a style choice: `crates/vike-ops/tests/settings_registry.rs`' loose sweep reads an
/// env-PREFIXED string literal in a `src/` file as evidence the file READS that variable, and then
/// demands a `SETTINGS` row for it. A prefix names no key in any store — it is exactly the part
/// §4.4 REMOVES — so such a row would declare a variable that does not exist, and `vike-cli config
/// show` reports a row as the ORIGIN of an effective value. Composing the prefix from two tokens
/// neither of which carries the trailing underscore keeps the literal out of that sweep while
/// saying the same thing more precisely.
///
/// ⚠ The DISCRIMINATOR is `vike_secrets::AccountKey`'s derivation-time-only field and reaches NO
/// column. It is how dukascopy's two accounts are told apart without the index token becoming an
/// identity again — the defect §1 of the spec is about, and §11.1's own rejected alternative. The
/// owner's signature rules that the `DEMO1`/`DEMO2` LABELS are not written at all, and they are not.
// vike:new-venue:note a CONFORMING venue needs NO row in any of the three tables below — that is what makes them exception tables rather than a roster. `{venue}` needs a `HAND_MAPPED_ACCOUNTS` row only if its store keys do NOT parse as `{VENUE}_{TIER}_{FIELD}` (dukascopy bakes an account index into the tier token; alpaca spells its tier `SANDBOX`); an `is_secret` row only for a key that holds no secret; and a `pending_move` row only for the key that IS its book or its machine/tier config. Getting all three wrong for a conforming venue costs nothing: the defaults are account-scoped, `secret = 1`, and no pending move: crates/vike-bridge-core/src/credentials.rs's `classify_credential_name`
const HAND_MAPPED_ACCOUNTS: &[HandMappedAccount] = &[
    (
        "ALPACA",
        "SANDBOX",
        "alpaca",
        "demo",
        None,
        "the tier token SANDBOX means demo; `CREDENTIAL_TIERS` does not carry that spelling. ONE \
         account.",
    ),
    (
        "DUKASCOPY",
        "DEMO1",
        "dukascopy",
        "demo",
        Some("DEMO1"),
        "ruling 1: DEMO1 and DEMO2 are TWO accounts of one venue at one tier — the index token \
         baked into the tier position becomes a ROW. This is the whole point of the schema, and \
         the ONLY (venue, tier) pair in the migration that yields two accounts.",
    ),
    (
        "DUKASCOPY",
        "DEMO2",
        "dukascopy",
        "demo",
        Some("DEMO2"),
        "the second of ruling 1's pair — see the row above.",
    ),
    (
        "POLY",
        "",
        "polymarket",
        "live",
        None,
        "head POLY, NO tier token, and no roster venue is spelled POLY: one account at tier live. \
         The family SPLITS key by key (§5.2) — `classify_poly` runs FIRST and this row is its \
         fallback.",
    ),
];

/// One row of [`HAND_MAPPED_ACCOUNTS`]: `(store head, store tier token, venue, tier,
/// discriminator, why)`. A named alias rather than the bare tuple because six positions is past
/// what a reader can hold, and because the `why` is the column that makes the table a table.
type HandMappedAccount =
    (&'static str, &'static str, &'static str, &'static str, Option<&'static str>, &'static str);

/// The store prefix a [`HAND_MAPPED_ACCOUNTS`] row names: `{HEAD}_{TOKEN}_`, or `{HEAD}_` for the
/// row whose family carries no tier token at all.
fn hand_mapped_prefix(head: &str, token: &str) -> String {
    if token.is_empty() { format!("{head}_") } else { format!("{head}_{token}_") }
}

/// **THE MAP RENDERER — the inverse of [`classify_credential_name`], and the thing §12 says must
/// exist before ruling 10's rows may move.**
///
/// Given one `venue_setting` row — `(venue, tier, field)`, where `tier` is `NULL` for a
/// machine-scoped row — answer every legacy `credential.name` that row stands for. A bridge loader
/// keeps taking one `&HashMap<String, String>` and keeps finding every name it looks up, so the
/// move becomes a STORAGE fact with no reader-visible surface.
///
/// # ⚠ Why this returns a LIST and not a name
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §11 measured it: the ten moving
/// config-shaped names land as **NINE rows**, because `DUKASCOPY_DEMO1_SERVER` and
/// `DUKASCOPY_DEMO2_SERVER` hold the same value in the live store and collapse onto one
/// `(dukascopy, demo, SERVER)` row — `venue_setting`'s own `venue_setting_one_per_tier` index
/// admits no second. So a renderer that answered ONE name would drop a key the operator wrote, and
/// dukascopy's sidecar would fall back to the default demo JNLP in silence.
///
/// The one-to-many falls out of [`HAND_MAPPED_ACCOUNTS`] rather than being spelled here: that table
/// carries two rows for `(dukascopy, demo)`, one per store tier token. A venue whose keys parse as
/// the ordinary `{VENUE}_{TIER}_{FIELD}` has no row there and renders exactly one name.
///
/// # ⚠ The collision this CANNOT see, and who owes the check
///
/// `credential.name` and the names this renders are ONE namespace, and SQLite can constrain a name
/// across two tables no more than it can across two databases — `credential_one_live_name` holds
/// inside `credential` alone. Nothing here stops a `venue_setting` row rendering a name a live
/// `credential` row already holds, and a caller folding both into one map would then hold one key
/// with two candidate values, resolved by insertion order. The spec names that consequence and
/// says the MIGRATION owes the check; this function is pure and is not the place for it.
///
/// # ⚠ A machine-scoped row takes the store head with NO tier token
///
/// `POLY_PROXY_HOST` carries no tier at all — the proxy family is machine-scoped by construction
/// ([`classify_poly`] argues why: `egress.rs`'s `PROXY_KEYS` is a fixed five-key allow-list read
/// once and cached for the deployment, so it is not account-aware and cannot become so without that
/// cache changing shape). So `tier: None` matches a hand-map row on its VENUE alone and uses that
/// row's head with an empty token, which is what makes `POLY` — a head no roster venue is spelled
/// as — render correctly.
#[must_use]
pub fn venue_setting_names(venue: &str, tier: Option<&str>, field: &str) -> Vec<String> {
    let mut out: Vec<String> = HAND_MAPPED_ACCOUNTS
        .iter()
        .filter(|(_, _, v, t, _, _)| {
            // A machine-scoped row has no tier to match on, so the VENUE is the whole key. A
            // tier-scoped one must match both, which is what keeps `(dukascopy, demo)`'s two rows
            // from rendering for `(dukascopy, live)`.
            *v == venue && tier.is_none_or(|want| *t == want)
        })
        .map(|(head, token, _, _, _, _)| {
            // ⚠ A machine-scoped row IGNORES the hand-map row's own token. `POLY`'s row spells an
            // empty one already, but stating it here is what stops a future head that DOES carry a
            // token from rendering `{HEAD}_{TOKEN}_{FIELD}` for a value that has no tier.
            let token = if tier.is_none() { "" } else { *token };
            format!("{}{field}", hand_mapped_prefix(head, token))
        })
        .collect();
    if out.is_empty() {
        // The ordinary grammar, for every conforming venue — ibkr and fxcm among the movers. The
        // tier token is the STORE's spelling of the normalized tier, which for a conforming venue
        // is the normalized tier uppercased; a venue whose store spells it differently is exactly
        // what earns a `HAND_MAPPED_ACCOUNTS` row, and it was matched above.
        let head = venue.to_uppercase();
        out.push(match tier {
            Some(t) => format!("{head}_{}_{field}", t.to_uppercase()),
            None => format!("{head}_{field}"),
        });
    }
    // Deterministic, and de-duplicated in case two hand-map rows ever share a prefix: the caller
    // folds these into a map, and a repeated name would make the fold's outcome depend on order.
    out.sort_unstable();
    out.dedup();
    out
}

/// **Where a venue's operational configuration LIVES — one `setting` row, not a third table.**
///
/// The dotted settings key for `(venue, tier, field)`: `venue.polymarket.proxy_host` for a
/// machine-scoped value, `venue.ibkr.demo.backend` for a tier-scoped one. Rendered under the
/// `config` section, so the operator-facing spelling is `config.venue.ibkr.demo.backend`.
///
/// # ⚠ Why this exists at all — the THIRD KEY SHAPE that was nearly added
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §6 gives these ten values their own
/// table, `venue_setting`, keyed `(venue, tier, field)`. That would make **three** key shapes for
/// one question — `setting`'s `(section, key)`, `venue_arming`'s `(venue, label)`, and a third — and
/// the spec itself names the cost in its own open-items list:
///
/// > the table itself arrives with **no gate of its own**: a `venue_setting` row nothing reads is
/// > the defect `vike_config::CONSUMPTION` exists to refuse for settings keys, and **this table is
/// > outside that gate's vocabulary because its keys are not `section.key` paths**.
///
/// A `section.key` path costs nothing and buys all of it: `vike-cli config show` renders the value,
/// `vike-cli config set` writes it, `CONSUMPTION` refuses it if nothing reads it, and a TOML file
/// still spells it as an ordinary nested table (`[venue.ibkr.demo] backend = "cpapi"`). So the
/// values move into `setting` and `venue_setting` is not used. Owner's ruling, 2026-09-20.
///
/// ⚠ The table still EXISTS in `crate::schema::DDL` and is still created empty. Dropping a table is
/// a migration with its own reasoning and does not belong in the same change as the key decision;
/// what this function does is make sure nothing ever fills it.
///
/// # ⚠ The grammar is disambiguated by the TIER VOCABULARY, not by counting segments
///
/// `venue.ibkr.demo.backend` is `(ibkr, demo, BACKEND)` and `venue.polymarket.proxy_host` is
/// `(polymarket, None, PROXY_HOST)`, and both are four-or-fewer segments. What separates them is
/// that the second segment is a tier **iff it is one of `sim`/`demo`/`live`**
/// ([`vike_model::credential_keys::CREDENTIAL_TIERS`], lowercased). That is safe because no field
/// this grammar carries is spelled as a tier, and
/// `a_field_is_never_mistaken_for_a_tier` is what keeps it that way.
#[must_use]
pub fn venue_setting_key(venue: &str, tier: Option<&str>, field: &str) -> String {
    let field = field.to_lowercase();
    match tier {
        Some(t) => format!("venue.{venue}.{}.{field}", t.to_lowercase()),
        None => format!("venue.{venue}.{field}"),
    }
}

/// The inverse of [`venue_setting_key`] — `None` for a key that is not one of these.
///
/// The FIELD comes back in the STORE's uppercase spelling, because that is what
/// [`venue_setting_names`] composes a legacy credential name from; the venue and tier come back
/// lowercased, which is how both the `account` table and the arming ceiling spell them.
#[must_use]
pub fn parse_venue_setting_key(key: &str) -> Option<(String, Option<String>, String)> {
    let rest = key.strip_prefix("venue.")?;
    let (venue, rest) = rest.split_once('.')?;
    if venue.is_empty() {
        return None;
    }
    // The tier is recognised by VOCABULARY. A `rest` that starts with a tier token AND carries
    // something after it is tier-scoped; anything else is machine-scoped and `rest` is the whole
    // field. See the grammar ⚠ on `venue_setting_key`.
    let tiered = rest.split_once('.').filter(|(head, tail)| {
        !tail.is_empty()
            && vike_model::credential_keys::CREDENTIAL_TIERS
                .iter()
                .any(|t| t.eq_ignore_ascii_case(head))
    });
    Some(match tiered {
        Some((tier, field)) => {
            (venue.to_lowercase(), Some(tier.to_lowercase()), field.to_uppercase())
        }
        None if rest.is_empty() => return None,
        None => (venue.to_lowercase(), None, rest.to_uppercase()),
    })
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

/// **§5.2 — the `POLY_*` family, key by key.** It is not one classification, and each row was
/// MEASURED against the code that reads it rather than inferred from the prefix.
/// ⚠ Every row is matched on the FIELD — the part after the store head — for the reason
/// [`HAND_MAPPED_ACCOUNTS`] gives in full: a whole env-prefixed literal in a `src/` file is read by
/// `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence this file READS that variable,
/// and `POLY_BUILDER_CODE` is measured as read by NOTHING, so it has no `SETTINGS` row and must not
/// acquire one from here.
///
/// ⚠ **It matches the WHOLE name, deliberately, so a LABELLED key never reaches these two arms.**
/// Both of them answer `Placement::Venue`, which carries no account and therefore has nowhere to
/// put a label — matching a label-stripped base here would drop the operator's label in silence.
/// A labelled key falls through to the hand-map instead, where the label becomes the account's,
/// and that is the right answer for the one labelled spelling this family actually has
/// (`POLY_SIGNATURE_TYPE__{LABEL}`, read by `crates/bridges/polymarket/src/recon_client.rs`'s
/// `signature_type_for_account`). The residual is a labelled PROXY key, which no reader exists for
/// — `egress.rs`'s `PROXY_KEYS` is a fixed five-key allow-list read once and cached — and which
/// therefore lands as a `secret = 1` account row rather than as venue configuration: the
/// conservative direction, and the one `is_secret`'s own doc argues for.
fn classify_poly(name: &str) -> Option<vike_secrets::Classification> {
    use vike_secrets::{Classification, PendingMove, Placement};
    let field = name.strip_prefix(&hand_mapped_prefix("POLY", ""))?;
    // The proxy family is machine-scoped by construction: `crates/bridges/polymarket/src/
    // egress.rs`'s `PROXY_KEYS` is a fixed five-key allow-list read ONCE and cached, so it is not
    // account-aware and cannot become so without that cache changing shape. Spelled as the FIELDS
    // of those five keys.
    const PROXY_FIELDS: [&str; 5] =
        ["SOCKS_PROXY", "PROXY_ENABLED", "PROXY_HOST", "PROXY_PORT", "WS_PROXY_ENABLED"];
    if PROXY_FIELDS.contains(&field) {
        return Some(Classification {
            placement: Placement::Venue("polymarket".to_string()),
            field: field.to_string(),
            secret: false,
            recognised: true,
            pending_move: Some(PendingMove::VenueSetting),
        });
    }
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

/// **§6 — the rows MEASURED as holding no secret.**
///
/// Keyed on `(venue, field)` rather than on the whole NAME, so a tier other than the one the live
/// store happens to carry is classified the same way: `IBKR_LIVE_HOST` is as much a host as
/// `IBKR_DEMO_HOST` is.
///
/// `true` is the default for everything else, deliberately: a value wrongly marked non-secret is a
/// worse error than one wrongly marked secret.
fn is_secret(venue: &str, field: &str) -> bool {
    !matches!(
        (venue, field),
        // §6's twelve config-shaped names, minus the polymarket ones `classify_poly` answers for.
        ("ibkr", "HOST" | "PORT" | "BACKEND")
            | ("fxcm", "URL" | "CONNECTION")
            | ("dukascopy", "SERVER")
            // ...and the two of the twelve that STAY in `credential` at `secret = 0`:
            //
            // `IBKR_*_CLIENT_ID` — `crates/bridges/vike-ibkr/src/config.rs`'s
            // `load_ibkr_config_for_account` states it: the client ids must DIFFER between two live
            // TWS connections or the second socket is evicted by the gateway. So the GATEWAY is the
            // machine and the client id is a per-ACCOUNT SLOT inside it, which is the one thing
            // `venue_setting` is defined not to hold.
            | ("ibkr", "CLIENT_ID")
            // `POLY_SIGNATURE_TYPE` — per-WALLET by the venue's own code:
            // `crates/bridges/polymarket/src/recon_client.rs`'s `signature_type_for_account` reads
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
        // §6 / ruling 10 — machine rows (ibkr) and tier rows (fxcm, dukascopy).
        ("ibkr", "HOST" | "PORT" | "BACKEND")
        | ("fxcm", "URL" | "CONNECTION")
        | ("dukascopy", "SERVER") => Some(PendingMove::VenueSetting),
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
#[cfg(test)]
mod venue_setting_renderer_tests {
    use super::*;

    /// ⚠ **THE ONE THIS FUNCTION EXISTS FOR.** Ten moving names land as NINE `venue_setting` rows
    /// because dukascopy's two SERVER keys hold one value and collapse onto one row — so the
    /// renderer must give BOTH back. A renderer that answered one name would drop a key the
    /// operator wrote, and `spawn_with_program` would fall back to the default demo JNLP in
    /// silence: a working mount with a changed meaning, which is the failure shape §6.2 measured.
    #[test]
    fn one_dukascopy_row_renders_both_legacy_names() {
        assert_eq!(
            venue_setting_names("dukascopy", Some("demo"), "SERVER"),
            vec!["DUKASCOPY_DEMO1_SERVER".to_string(), "DUKASCOPY_DEMO2_SERVER".to_string()],
        );
    }

    /// A MACHINE-scoped row carries no tier, and its head is one no roster venue is spelled as.
    #[test]
    fn the_machine_scoped_proxy_family_renders_under_the_poly_head() {
        for field in
            ["PROXY_ENABLED", "PROXY_HOST", "PROXY_PORT", "SOCKS_PROXY", "WS_PROXY_ENABLED"]
        {
            assert_eq!(
                venue_setting_names("polymarket", None, field),
                vec![format!("POLY_{field}")],
                "the proxy family is machine-scoped and takes the head with NO tier token"
            );
        }
    }

    /// A CONFORMING venue has no hand-map row and renders exactly one ordinary name.
    #[test]
    fn a_conforming_venue_renders_the_ordinary_grammar() {
        // ⚠ ONE name is spelled here, and it is spelled because it is a key that really exists in
        // the store and therefore carries a `SETTINGS` row. A whole env-prefixed literal in a
        // `src/` file is read by `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence
        // this file READS that variable, so inventing a tier variant to assert against fails that
        // gate — measured, on the LIVE-tier fxcm and dukascopy names this test used to spell.
        assert_eq!(venue_setting_names("ibkr", Some("demo"), "BACKEND"), vec!["IBKR_DEMO_BACKEND"]);
        // The second venue asserts the whole name WITHOUT ever writing it as one literal: it is
        // decomposed on the separator and the parts are compared. ⚠ A `starts_with("FXCM_")` was
        // the first attempt and the sweep flagged `FXCM_` too — the TRAILING UNDERSCORE is what
        // makes a literal look like an env prefix, which is the same reason
        // `HAND_MAPPED_ACCOUNTS` stores its head and token apart. Composing the expected string
        // with the fallback's own rule would be worse still: an assertion that cannot fail for its
        // stated reason.
        let fxcm = venue_setting_names("fxcm", Some("demo"), "URL");
        assert_eq!(fxcm.len(), 1, "a conforming venue has no hand-map row: {fxcm:?}");
        assert_eq!(
            fxcm[0].split('_').collect::<Vec<_>>(),
            vec!["FXCM", "DEMO", "URL"],
            "head, tier, field — in the store's own uppercase: {fxcm:?}"
        );
    }

    /// A venue whose STORE spells the tier differently is what earns a hand-map row, and the
    /// renderer must use the store's spelling rather than the normalized one.
    #[test]
    fn a_hand_mapped_tier_token_beats_the_normalized_tier() {
        assert_eq!(
            venue_setting_names("alpaca", Some("demo"), "ACCOUNT_ID"),
            vec!["ALPACA_SANDBOX_ACCOUNT_ID"],
            "the store spells this tier SANDBOX; `CREDENTIAL_TIERS` does not carry that spelling"
        );
    }

    /// Dukascopy's pair belongs to `demo` and must not leak into another tier's rendering.
    #[test]
    fn a_tier_scoped_row_renders_only_its_own_tier() {
        // ⚠ The expected name is NOT spelled, and not composed by this test either. Spelling
        // the LIVE-tier dukascopy name would plant a whole env-prefixed literal in a `src/` file, which
        // `crates/vike-ops/tests/settings_registry.rs`' sweep reads as evidence that this file
        // READS that variable — it does not, and no such key exists, so the gate fails (it did).
        // Composing it with the same `{HEAD}_{TIER}_{FIELD}` rule the fallback uses would be worse:
        // an assertion that cannot fail for its stated reason, because it would reimplement the
        // thing under test. So the CLAIM is asserted instead — one name, and the demo pair does not
        // leak into another tier — which is what this test was ever about.
        let names = venue_setting_names("dukascopy", Some("live"), "SERVER");
        assert_eq!(names.len(), 1, "no hand-map row matches (dukascopy, live): {names:?}");
        assert!(
            !names.iter().any(|n| n.contains("DEMO1") || n.contains("DEMO2")),
            "the demo pair must not leak into another tier's rendering: {names:?}"
        );
    }

    /// ⚠ **THE PROOF.** Every name the renderer produces for the nine rows ruling 10 actually moves
    /// must classify BACK to the venue, tier and field it was rendered from. That is what makes
    /// this the inverse of [`classify_credential_name`] rather than a second function that happens
    /// to agree today — and it is the property the whole move rests on, because a loader looks the
    /// legacy name up and the classifier is what put the row where it is.
    #[test]
    fn every_rendered_name_classifies_back_to_the_row_it_came_from() {
        use vike_secrets::Placement;
        // The nine rows, spelled as `(venue, tier, field)` exactly as `venue_setting` holds them.
        let rows: &[(&str, Option<&str>, &str)] = &[
            ("ibkr", Some("demo"), "HOST"),
            ("ibkr", Some("demo"), "PORT"),
            ("ibkr", Some("demo"), "BACKEND"),
            ("fxcm", Some("demo"), "URL"),
            ("fxcm", Some("demo"), "CONNECTION"),
            ("dukascopy", Some("demo"), "SERVER"),
            ("polymarket", None, "PROXY_ENABLED"),
            ("polymarket", None, "PROXY_HOST"),
            ("polymarket", None, "PROXY_PORT"),
        ];
        for (venue, tier, field) in rows {
            let names = venue_setting_names(venue, *tier, field);
            assert!(!names.is_empty(), "({venue}, {tier:?}, {field}) rendered nothing");
            for name in &names {
                let class = classify_credential_name(name);
                assert!(class.recognised, "{name} rendered but does not classify");
                assert_eq!(&class.field, field, "{name} classifies to a different FIELD");
                let got_venue = match &class.placement {
                    Placement::Account(key) => key.venue.clone(),
                    Placement::Venue(v) => v.clone(),
                    Placement::Infrastructure => String::new(),
                };
                assert_eq!(&got_venue, venue, "{name} classifies to a different VENUE");
            }
        }
    }

    /// ⚠ …and every one of those names is one the classifier ALREADY marks as owed to
    /// `venue_setting`. Without this the test above would pass for a name nobody ever intends to
    /// move, and the renderer's roster would be free to drift away from `pending_move`'s.
    /// ⚠ **THE ROUND TRIP THAT MAKES (A′) SAFE.** A settings key must carry the same
    /// `(venue, tier, field)` back out, because that triple is what
    /// [`venue_setting_names`] composes the legacy credential name from. If the key lost the tier,
    /// `IBKR_DEMO_BACKEND` would render as `IBKR_BACKEND` and the loader would find nothing.
    #[test]
    fn a_settings_key_carries_the_whole_triple_back_out() {
        let rows: &[(&str, Option<&str>, &str)] = &[
            ("ibkr", Some("demo"), "HOST"),
            ("ibkr", Some("demo"), "PORT"),
            ("ibkr", Some("demo"), "BACKEND"),
            ("fxcm", Some("demo"), "URL"),
            ("fxcm", Some("demo"), "CONNECTION"),
            ("dukascopy", Some("demo"), "SERVER"),
            ("polymarket", None, "PROXY_ENABLED"),
            ("polymarket", None, "PROXY_HOST"),
            ("polymarket", None, "PROXY_PORT"),
        ];
        for (venue, tier, field) in rows {
            let key = venue_setting_key(venue, *tier, field);
            let got =
                parse_venue_setting_key(&key).unwrap_or_else(|| panic!("{key} did not parse back"));
            assert_eq!(
                (got.0.as_str(), got.1.as_deref(), got.2.as_str()),
                (*venue, *tier, *field),
                "{key} round-tripped to a different row"
            );
            // …and the whole point: the triple still renders the legacy names a loader looks up.
            assert!(
                !venue_setting_names(&got.0, got.1.as_deref(), &got.2).is_empty(),
                "{key} parsed but renders no credential name"
            );
        }
    }

    /// The operator-facing spellings, pinned so a rename is a decision rather than a diff.
    #[test]
    fn the_keys_are_the_spellings_an_operator_types() {
        assert_eq!(
            venue_setting_key("polymarket", None, "PROXY_HOST"),
            "venue.polymarket.proxy_host"
        );
        assert_eq!(venue_setting_key("ibkr", Some("demo"), "BACKEND"), "venue.ibkr.demo.backend");
        assert_eq!(
            venue_setting_key("dukascopy", Some("demo"), "SERVER"),
            "venue.dukascopy.demo.server"
        );
    }

    /// ⚠ **THE GRAMMAR'S ONE HAZARD.** `venue.<v>.<x>.<y>` is tier-scoped iff `<x>` is a TIER, and
    /// both shapes have the same segment count — so a field that happened to be spelled `demo`
    /// would be read as a tier and its value would render the wrong credential name. No field this
    /// grammar carries is, and this test is what says so rather than assuming it.
    #[test]
    fn a_field_is_never_mistaken_for_a_tier() {
        for field in [
            "HOST",
            "PORT",
            "BACKEND",
            "URL",
            "CONNECTION",
            "SERVER",
            "PROXY_ENABLED",
            "PROXY_HOST",
            "PROXY_PORT",
            "SOCKS_PROXY",
            "WS_PROXY_ENABLED",
        ] {
            assert!(
                !vike_model::credential_keys::CREDENTIAL_TIERS
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(field)),
                "{field} is spelled as a tier — the venue-settings key grammar cannot tell it from \
                 one, and its value would render the wrong credential name"
            );
        }
    }

    /// A multi-segment FIELD is kept whole when it is not preceded by a tier — the machine-scoped
    /// shape — so a future `venue.x.a.b` field does not silently lose its head.
    #[test]
    fn a_machine_scoped_field_may_carry_dots() {
        assert_eq!(
            parse_venue_setting_key("venue.polymarket.ws_proxy_enabled"),
            Some(("polymarket".into(), None, "WS_PROXY_ENABLED".into()))
        );
    }

    /// ⚠ **THE CHECK §6.2 SAYS THE MIGRATION OWES.** A settings row rendering a name a LIVE
    /// credential row already holds puts one key in the map with two candidate values, resolved by
    /// insertion order. SQLite cannot refuse it — `credential_one_live_name` holds inside
    /// `credential` alone — so this does.
    ///
    /// ⚠ The live set is built FROM the renderer rather than spelled, and the reason is this file:
    /// a whole env-prefixed literal in `src/` is read by
    /// `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence that THIS CRATE reads that
    /// variable, and a row under the owning bridge does not declare a read here — rows are keyed
    /// `(name, krate)`. The negative control below is what keeps this from being circular.
    #[test]
    fn a_rendered_name_that_a_live_credential_already_holds_is_a_collision() {
        let key = venue_setting_key("ibkr", Some("demo"), "BACKEND");
        let rendered = venue_setting_names("ibkr", Some("demo"), "BACKEND");
        let live: std::collections::BTreeSet<String> = rendered.iter().cloned().collect();
        let found = rendered_name_collisions(std::slice::from_ref(&key), &live);
        assert_eq!(found.len(), rendered.len(), "every rendered name collides: {found:?}");
        for name in &rendered {
            assert_eq!(found.get(name), Some(&key), "…and each names the settings key");
        }
    }

    /// ⚠ **THE NEGATIVE CONTROL.** Without it the test above would pass for a function that
    /// reported every key it was handed. The live set here holds a name no grammar renders.
    #[test]
    fn no_overlap_is_no_collision() {
        let live: std::collections::BTreeSet<String> =
            ["nothing-a-renderer-would-ever-produce".to_string()].into_iter().collect();
        let keys = [
            venue_setting_key("ibkr", Some("demo"), "BACKEND"),
            venue_setting_key("polymarket", None, "PROXY_HOST"),
        ];
        assert!(rendered_name_collisions(&keys, &live).is_empty());
    }

    /// ⚠ **THE DUKASCOPY ROW COLLIDES ON EITHER OF ITS TWO NAMES.** One row renders both, so a
    /// half-finished move that left only the second behind must still be caught — a check that
    /// looked at one rendered name would pass while the map carried two candidate values for it.
    #[test]
    fn a_one_to_many_row_collides_on_either_name() {
        let key = venue_setting_key("dukascopy", Some("demo"), "SERVER");
        let rendered = venue_setting_names("dukascopy", Some("demo"), "SERVER");
        assert_eq!(rendered.len(), 2, "this row is the one-to-many one: {rendered:?}");
        for surviving in &rendered {
            // ONE of the pair survives in `credential` — the half-done move, either way round.
            let live: std::collections::BTreeSet<String> =
                [surviving.clone()].into_iter().collect();
            let found = rendered_name_collisions(std::slice::from_ref(&key), &live);
            assert_eq!(found.get(surviving), Some(&key), "the row renders it: {found:?}");
            assert_eq!(found.len(), 1, "and only the surviving one collides: {found:?}");
        }
    }

    /// A key this grammar does not own is SKIPPED, not guessed at — the `setting` table carries
    /// every `config.*` key on the box and only the `venue.` ones render a credential name.
    #[test]
    fn an_unrelated_settings_key_renders_nothing_and_collides_with_nothing() {
        let live: std::collections::BTreeSet<String> =
            ["nothing-a-renderer-would-ever-produce".to_string()].into_iter().collect();
        let keys = ["log_dir".to_string(), "max_notional_per_order".to_string()];
        assert!(rendered_name_collisions(&keys, &live).is_empty());
    }

    /// Anything that is not one of these keys is refused rather than guessed at.
    #[test]
    fn a_key_that_is_not_a_venue_setting_is_refused() {
        for key in
            ["poly_proxy_host", "venue.", "venue.polymarket", "venues.x.y", "", "venue..host"]
        {
            assert_eq!(parse_venue_setting_key(key), None, "{key:?} should not parse");
        }
    }

    #[test]
    fn every_rendered_name_is_one_the_classifier_marked_as_moving() {
        use vike_secrets::PendingMove;
        for (venue, tier, field) in [
            ("ibkr", Some("demo"), "BACKEND"),
            ("fxcm", Some("demo"), "CONNECTION"),
            ("dukascopy", Some("demo"), "SERVER"),
            ("polymarket", None, "PROXY_HOST"),
        ] {
            for name in venue_setting_names(venue, tier, field) {
                assert_eq!(
                    classify_credential_name(&name).pending_move,
                    Some(PendingMove::VenueSetting),
                    "{name} is rendered by the venue_setting renderer but is not marked as moving \
                     there — the two rosters have drifted"
                );
            }
        }
    }
}

/// **The scoped credential read, WITH the settings rows folded in.**
///
/// The scoped twin of the fold `load_workspace_secrets_at_checked` performs, and it exists because
/// the five polymarket proxy keys reach the store through THIS path and not through a composition
/// root's map — `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars` calls it directly.
/// Without it those five are the one family ruling 10's move would break while every other reader
/// kept working.
///
/// ⚠ **It is a FUNCTION here, not a re-export of `vike_secrets::load_workspace_dotenv_scoped`**,
/// and that is the whole change: this crate owns the renderer, so this is the layer that can fold.
/// The `vike-secrets` original stays what it is — the unfolded primitive — and a caller that wants
/// the pre-move answer can still name it.
///
/// ⚠ **A folded name is added only where the scope DECLARED it.** The scope is the read's own
/// narrowing (owner ruling: a process materialises only what it asked for), and folding a name
/// nobody declared would widen it back — quietly handing a reader a key it never asked for.
#[must_use]
pub fn load_workspace_dotenv_scoped(scope: &KeyScope) -> ScopedSecrets {
    let mut base = vike_secrets::load_workspace_dotenv_scoped(scope);
    let mut rendered = HashMap::new();
    let _ = fold_venue_settings(None, &mut rendered);
    let mut collisions = Vec::new();
    for (name, value) in &rendered {
        // ⚠ `NotDeclared` is DROPPED in silence: the scope is the read's own narrowing (owner
        // ruling — a process materialises only what it asked for), and folding a name nobody
        // declared would widen it back.
        if base.fold_in(name, value) == vike_secrets::FoldOutcome::Collided {
            collisions.push(name.clone());
        }
    }
    collisions.sort_unstable();
    if !collisions.is_empty() {
        tracing::error!(
            names = ?collisions,
            "⚠ the credential store and the settings rows BOTH answer for these names — ruling \
             10's move is HALF DONE. The CREDENTIAL value is the one in force"
        );
    }
    base
}
