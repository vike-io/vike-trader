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
/// `crates/vike-ops/tests/settings/settings_registry.rs`'s row demand honest rather than merely large.
#[test]
fn the_attribution_grid_is_exactly_what_the_reader_looks_up() {
    let enumerated: BTreeSet<String> = attribution_keys().into_iter().collect();
    for venue in VENUES {
        let mechanised = !vike_model::venues::attribution::attribution_for(venue).is_none();
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
