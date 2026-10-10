//! More than one account per venue: the labelled half of the loader, and the default account's
//! names unchanged.
//!
//! ⚠ No bespoke key NAME is spelled here: every fixture is composed through the module's own
//! `prefix_for` + `names_for_account`, for the reason `required_passphrase_tests::seed` gives. The
//! BESPOKE families are covered in `crates/vike-bridge-core/tests/account_credential_shapes.rs`,
//! through each bridge's OWN name function — the only way to prove the real names.

use std::collections::BTreeSet;

use vike_model::VENUES;
use vike_model::accounts::account_keys::{ACCOUNT_SEPARATOR, AccountLabel};
use vike_model::credential_keys::credential_keys;

use super::generated_key_grid_tests::every_environment;
use super::*;

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

/// **The default account's names ARE the unlabelled ones**, for every roster venue and every tier
/// the loader reads — the same `String`s, folded over both sides rather than asserted on an example.
#[test]
fn the_default_account_composes_the_historical_names() {
    for venue in VENUES {
        for env in every_environment() {
            let tier = env.as_str();
            let prefix = prefix_for(venue, tier);
            assert_eq!(
                names_for_account(&prefix, &AccountLabel::Default),
                names_for_prefix(&prefix),
                "{venue}/{tier}: the default account must compose today's names verbatim"
            );
        }
    }
}

/// …and the LOAD PATH is the unlabelled one, proven through the two public functions: same
/// credentials, same finding, for every venue and tier.
///
/// ⚠ **The fixture is seeded through [`names_for_prefix`], NOT [`names_for_account`].** Seeded the
/// other way, a mutation giving the default account a `__DEFAULT` suffix moves the loader and the
/// fixture together and both sides answer `None`; the `must_load` assertion is the other half,
/// because two `None`s would otherwise still agree.
#[test]
fn the_default_account_load_path_is_the_historical_one() {
    for venue in VENUES {
        for env in every_environment() {
            let tier = env.as_str();
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
                // …and it LOADS, or a change moving the default account's names would leave
                // both sides `None` and agreeing.
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

/// A LABELLED account loads from its OWN keys, and the two accounts are sealed off in BOTH
/// directions. The reverse one matters most: a labelled load falling back to the unlabelled key
/// would sign account `ALT`'s orders with the default account's credentials.
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

/// A labelled account has no `MAINNET` fallback either: that spelling is no tier (owner ruling
/// 2026-10-09), so a labelled `MAINNET` set leaves the account's live tier absent.
#[test]
fn a_labelled_account_has_no_mainnet_fallback() {
    let alt = label("ALT");
    let vars = seed_account("binance", "MAINNET", &alt, Some("old"), Some("s"), None);
    assert!(
        load_credentials_for_account("binance", Environment::Live, &alt, &vars).is_none(),
        "a MAINNET-spelled labelled set must not load under Live"
    );
}

/// ⚠ **AN INCOMPLETE LABELLED ACCOUNT IS AN ABSENT ONE**, exactly as an incomplete default account
/// is. Asserted as an EQUALITY between the two accounts, so if the default account's answer for a
/// half-configured store changes, this test moves with it instead of pinning a copy.
#[test]
fn an_incomplete_labelled_account_behaves_exactly_like_an_incomplete_default_one() {
    let alt = label("ALT");
    for (key, secret) in [(Some("k"), None), (None, Some("s")), (None, None)] {
        for venue in ["bybit", "okx"] {
            let labelled = seed_account(venue, "DEMO", &alt, key, secret, None);
            let defaulted = seed_account(venue, "DEMO", &AccountLabel::Default, key, secret, None);
            let a =
                load_credentials_for_account(venue, Environment::Demo, &alt, &labelled).is_some();
            let b = load_credentials_from(venue, Environment::Demo, &defaulted).is_some();
            assert_eq!(
                a, b,
                "{venue}: a labelled account missing key={key:?} secret={secret:?} must \
                     resolve exactly as the default account does"
            );
            assert!(!a, "{venue}: an incomplete account must stay unconfigured");
            // …and it is SILENT: a store missing key or secret is UNCONFIGURED, not
            // half-configured.
            assert_eq!(
                missing_required_passphrase_for_account(venue, Environment::Demo, &alt, &labelled),
                None,
                "{venue}: an unconfigured labelled account must report nothing"
            );
        }
    }
}

/// The one finding a labelled account DOES have: OKX's required passphrase, named as the
/// LABELLED variable — the one the operator has to write.
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

/// HARD, for a second account too: no credential VALUE reaches `Debug`.
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
    // The four-character tail is the ONE disclosed fragment, the same as for the default account.
    assert!(rendered.contains("***Zx81"), "the redacted rendering changed: {rendered}");
    assert!(rendered.contains("passphrase=set"), "{rendered}");
}

/// [`account_var`]'s own contract: the labelled name, trimmed, blank-is-absent, no fallback. The
/// base key is a GENERIC one the module composes; the real bespoke shapes are
/// `tests/account_credential_shapes.rs`'s.
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

/// ⚠ **The ENUMERATED grid stays the DEFAULT account's, by decision.** A label is operator-chosen
/// over an unbounded set, so there is no finite grid of labelled keys for
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` to demand rows for; the STORE is
/// the enumeration.
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
