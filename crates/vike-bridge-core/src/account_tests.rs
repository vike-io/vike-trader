use std::collections::BTreeSet;

use vike_model::VENUES;
use vike_model::accounts::account_keys::{ACCOUNT_SEPARATOR, AccountLabel};
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
            // …and it is SILENT: the only finding this module produces is the passphrase one,
            // and a store missing key or secret is UNCONFIGURED, not half-configured.
            assert_eq!(
                missing_required_passphrase_for_account(venue, Environment::Demo, &alt, &labelled),
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
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` to demand rows for. This pins that the
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
