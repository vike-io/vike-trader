//! The ENUMERATION and the READ, held equal: `vike_model::credential_keys` and this loader's own
//! composition are one set.
//!
//! A key built with `format!` and handed to a map `get` appears as no literal anywhere
//! ([`vike_model::credential_keys`]' module doc);
//! `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
//! `every_generated_key_is_declared` demands a row for every enumerated key, which is only worth
//! anything if the enumeration is the set this loader reads. Asserted by SET equality against the
//! loader's composition and by BEHAVIOUR, one key at a time.

use std::collections::BTreeSet;

use vike_model::VENUES;
use vike_model::credential_keys::{
    ATTRIBUTION_SUFFIXES, CREDENTIAL_TIERS, attribution_key, attribution_keys, credential_keys,
};

use super::*;

/// Every [`Environment`] variant. The `match` is EXHAUSTIVE on purpose and does nothing else: a
/// new tier is then a COMPILE error here, which is how it acquires a grid entry and a registry
/// row instead of becoming another silently-undeclared key family. Shared with `account_tests`.
pub(super) fn every_environment() -> [Environment; 3] {
    let all = [Environment::Sim, Environment::Demo, Environment::Live];
    for env in all {
        match env {
            Environment::Sim | Environment::Demo | Environment::Live => {}
        }
    }
    all
}

/// THE EQUIVALENCE. The names this loader composes over the whole roster ARE
/// `vike_model::credential_keys::credential_keys`.
///
/// Two provenances, which is the point: the left side is folded from [`prefix_for`] and
/// [`names_for_prefix`] — the two functions `load_credentials_from` calls — and the right side is
/// the shared table's product over [`vike_model::VENUES`]. A gate building its expectation from
/// the table it checks would gate nothing.
#[test]
fn the_loader_reads_exactly_the_enumerated_credential_grid() {
    let mut composed: BTreeSet<String> = BTreeSet::new();
    for venue in VENUES {
        for env in every_environment() {
            let (key, secret, passphrase) = names_for_prefix(&prefix_for(venue, env.as_str()));
            composed.extend([key, secret, passphrase]);
        }
    }
    let enumerated: BTreeSet<String> = credential_keys().into_iter().collect();
    assert_eq!(
        composed, enumerated,
        "the credential grid and the loader's own composition have drifted — a key on one side \
             and not the other is either an undeclared read or a declared row nothing reads"
    );
}

/// …and every enumerated name is LOAD-BEARING, proven through the real `load_credentials_from`:
/// removing one changes the answer.
///
/// The passphrase is REQUIRED only where [`venue_passphrase`] says so, so dropping it either
/// refuses the load or yields the same credentials with `passphrase: None`. Both prove the NAME was
/// read.
#[test]
fn every_enumerated_credential_key_changes_what_the_loader_returns() {
    let enumerated: BTreeSet<String> = credential_keys().into_iter().collect();
    for venue in VENUES {
        for env in every_environment() {
            let tier = env.as_str();
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

/// The ATTRIBUTION twin, BOTH ways in one loop: a mechanised venue's two keys are enumerated AND
/// looked up by the real reader; an unmechanised venue's are neither (`attribution_code_from`
/// returns before it builds a key, so declaring them would claim a read that cannot happen).
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

/// The tier spellings are the SHARED table's: `vike-model` cannot see this enum and this crate must
/// not re-spell the grid, so the two copies are pinned equal here (the same shape as
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs`).
#[test]
fn the_environment_tiers_are_the_shared_table() {
    let from_enum: BTreeSet<&str> = every_environment().iter().map(Environment::as_str).collect();
    let from_table: BTreeSet<&str> = CREDENTIAL_TIERS.iter().copied().collect();
    assert_eq!(
        from_enum, from_table,
        "`Environment`'s tier spellings and `vike_model::credential_keys`' tier table have \
             drifted — a tier in one and not the other is a whole column of undeclared keys"
    );
}
