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

use vike_model::account_keys::{account_key, AccountLabel};
use vike_model::credential_keys::{
    attribution_key, API_KEY_SUFFIX, API_PASSPHRASE_SUFFIX, API_SECRET_SUFFIX, BROKER_CODE_SUFFIX,
    BUILDER_CODE_SUFFIX,
};

use crate::venue_passphrase::venue_passphrase;

pub use vike_secrets::{
    load_workspace_dotenv, load_workspace_dotenv_from, parse_dotenv, workspace_dotenv_path,
    SecretsError, Source, SECRETS_FILE,
};

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

    use vike_model::account_keys::{AccountLabel, ACCOUNT_SEPARATOR};
    use vike_model::credential_keys::credential_keys;
    use vike_model::VENUES;

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
    match try_load_workspace_secrets_at(settings_dir) {
        Ok((map, _source)) => map,
        Err(e) => {
            // `SecretsError` carries no secret material (see its doc) — safe to log verbatim.
            tracing::error!(
                "credential store could not be opened, continuing with NO credentials \
                 (every venue stays paper): {e}"
            );
            HashMap::new()
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
    load_workspace_secrets_at(
        // The literal, not `vike_secrets::SETTINGS_DIR_ENV`: `vike_ops::scan`'s map-lookup sweep
        // resolves constants CRATE-wide, so an imported one would make this read invisible to the
        // settings registry — a declared read is the point. The two spellings are pinned equal by
        // `tests/settings_dir_spellings.rs`.
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
    )
}

/// [`load_workspace_secrets_at`], but returning the error and the provenance instead of swallowing
/// them — for a human-facing caller (`vike-cli secrets`) that can print both.
///
/// This is also where the store's findings become log lines — its PERMISSION exposure, and a
/// leftover PREDECESSOR store beside a project that has none. `vike-secrets` returns both as data
/// because it carries no logging dependency; both wrappers here route through this function, so no
/// caller of either can forget to surface them.
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
        tracing::warn!("{w}");
    }
    if let Some(w) = &resolved.legacy {
        // `LegacyStoreWarning` prints two PATHS and never opened either file. It is set only when
        // the store is ABSENT — the case where this function otherwise returns an empty map in
        // complete silence and every venue drops to paper.
        tracing::warn!("{w}");
    }
    Ok((resolved.secrets.into_map(), resolved.source))
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

    use vike_model::credential_keys::{
        attribution_key, attribution_keys, credential_keys, ATTRIBUTION_SUFFIXES, CREDENTIAL_TIERS,
        LEGACY_CREDENTIAL_TIERS,
    };
    use vike_model::VENUES;

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
