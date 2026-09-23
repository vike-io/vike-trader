//! **Every BESPOKE credential shape is reachable by account label** — the half of the
//! multi-account grammar that a generic `{VENUE}_{TIER}_API_*` fixture cannot prove.
//!
//! `vike_bridge_core::credentials`' own grid is three suffixes wide. The credential store also
//! holds shapes that grid never sees, and several of them are MULTI-WORD:
//! `OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `IG_{TIER}_PASSWORD`,
//! `FXCM_{TIER}_USER`/`_PASSWORD`, `ASTER_{TIER}_PRIVATE_KEY`,
//! `HYPERLIQUID_{TIER}_PRIVATE_KEY`, and dukascopy's numbered `DUKASCOPY_DEMO1_LOGIN`, which bakes
//! an account index into the tier token and conforms to nothing.
//!
//! Those shapes are precisely why `vike_model::account_keys` puts the label at the END:
//! `{VENUE}_{TIER}_{LABEL}{SUFFIX}` would read `OANDA_DEMO_ACCOUNT_ID` as *tier `DEMO`, label
//! `ACCOUNT`, suffix `_ID`*. This file is the behavioural half of that argument — the property is
//! not "the parse is unambiguous" but "a SECOND oanda account's account-id variable is reachable,
//! and it is reachable without this workspace holding a list of oanda's suffixes anywhere".
//!
//! # ⚠ Why the names come from the BRIDGES and not from this file
//!
//! Three of the shapes are read here through each bridge's own public name function
//! (`oanda_env_var_names`, `ig_env_var_names`, `fxcm_env_var_names`), which is the same function
//! that venue's loader calls. A test that spelled the names itself would prove that the grammar
//! works on a COPY of them, and would keep passing after the venue renamed one — the failure
//! `vike_model::venues`' `roster_matches_the_bridge_crates` was rewritten to stop shipping.
//!
//! The remaining shapes have no such function (aster and hyperliquid compose their keys inline;
//! dukascopy's `DukascopyAccount` is not a dev-dependency here), so those bases ARE spelled out.
//! That is safe in this file and would not be safe under `src/`: `crates/vike-ops/tests/
//! settings_registry.rs`'s `is_test_region_file` drops every literal in a `tests/` file from the
//! registry's read evidence, so a fixture name here cannot be mistaken for a read.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Environment, account_var};
use vike_model::account_keys::{ACCOUNT_SEPARATOR, AccountLabel, account_key, split_account_key};

/// The label every case uses. Deliberately a word that is also a plausible SUFFIX word, so a
/// hypothetical `{VENUE}_{TIER}_{LABEL}{SUFFIX}` grammar would have somewhere to go wrong.
const LABEL: &str = "ID";

fn label() -> AccountLabel {
    AccountLabel::parse(LABEL).expect("a legal label")
}

/// Every bespoke base key this workspace reads, as `(what it is, the key)`.
///
/// The first three families come from the bridges' own name functions; the rest are spelled (see
/// the module doc for why that is safe here and only here).
fn bespoke_bases() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();

    for env in [Environment::Demo, Environment::Live] {
        let (api_key, account_id) = vike_oanda::oanda_env_var_names(env);
        out.push(("oanda api key", api_key));
        out.push(("oanda ACCOUNT_ID — the two-word suffix that forced the grammar", account_id));

        let (ig_key, identifier, password) = vike_ig::ig_env_var_names(env);
        out.push(("ig api key", ig_key));
        out.push(("ig IDENTIFIER", identifier));
        out.push(("ig PASSWORD", password));

        let (user, fx_password, url, connection) = vike_fxcm::fxcm_env_var_names(env);
        out.push(("fxcm USER", user));
        out.push(("fxcm PASSWORD", fx_password));
        out.push(("fxcm URL", url));
        out.push(("fxcm CONNECTION", connection));
    }

    // No public name function — composed inline by the venue's own loader.
    out.push(("aster PRIVATE_KEY", "ASTER_LIVE_PRIVATE_KEY".to_string()));
    out.push(("aster USER", "ASTER_LIVE_USER".to_string()));
    out.push(("aster SIGNER", "ASTER_LIVE_SIGNER".to_string()));
    out.push(("hyperliquid PRIVATE_KEY", "HYPERLIQUID_LIVE_PRIVATE_KEY".to_string()));
    out.push(("hyperliquid ACCOUNT_ADDRESS", "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS".to_string()));
    // The one family that already ships a multi-account shape, with the index inside the TIER
    // token. `account_ref_from_key` classifies it as no credential at all — and the grammar still
    // has to work on it, because appending is all it does.
    out.push(("dukascopy numbered LOGIN", "DUKASCOPY_DEMO1_LOGIN".to_string()));
    out.push(("dukascopy numbered PASSWORD", "DUKASCOPY_DEMO1_PASSWORD".to_string()));
    out.push(("dukascopy numbered SERVER", "DUKASCOPY_DEMO1_SERVER".to_string()));

    out
}

/// The gate is only as good as its input — a `bespoke_bases()` that started returning nothing, or
/// that stopped covering the families the module doc names, would make every test below vacuous.
#[test]
fn the_bespoke_table_covers_the_shapes_it_claims_to() {
    let bases = bespoke_bases();
    assert!(bases.len() >= 20, "only {} bespoke bases — the table is broken", bases.len());
    for needed in [
        "_ACCOUNT_ID",
        "_IDENTIFIER",
        "_PRIVATE_KEY",
        "_USER",
        "_PASSWORD",
        "_LOGIN",
        "_CONNECTION",
    ] {
        assert!(
            bases.iter().any(|(_, k)| k.ends_with(needed)),
            "no bespoke base ends in {needed} — the table no longer covers that family"
        );
    }
    // Every base must be a name today's grammar produces: no `__` in it, or the split below would
    // be testing a key that could not exist.
    for (what, key) in &bases {
        assert!(!key.contains(ACCOUNT_SEPARATOR), "{what}: {key} already carries the separator");
    }
}

/// **THE PROPERTY.** For every bespoke shape: the labelled key is the WHOLE base plus the label,
/// the split gives the base back verbatim, and `account_var` reads the labelled account and only
/// the labelled account.
///
/// Note what is NOT here: any per-venue arm, any suffix table, any exception. The single
/// `account_key` composition is what every case goes through, which is the claim being made.
#[test]
fn every_bespoke_credential_shape_is_reachable_by_label() {
    let alt = label();
    for (what, base) in bespoke_bases() {
        let labelled = account_key(&base, &alt);
        assert_eq!(
            labelled,
            format!("{base}{ACCOUNT_SEPARATOR}{LABEL}"),
            "{what}: the label must be appended after the WHOLE of today's key"
        );

        // …and the parse gives the base back UNTOUCHED — the suffix was never re-parsed, which is
        // the whole reason the label goes at the end.
        let split = split_account_key(&labelled).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(split.base, base, "{what}: the base did not round-trip");
        assert_eq!(split.label, alt, "{what}: the label did not round-trip");

        // The READ: the labelled account's value, and nothing else's.
        let labelled_store: HashMap<String, String> =
            [(labelled.clone(), "  labelled-value  ".to_string())].into_iter().collect();
        assert_eq!(
            account_var(&labelled_store, &base, &alt),
            Some("labelled-value"),
            "{what}: the labelled account's credential must be readable"
        );
        assert_eq!(
            account_var(&labelled_store, &base, &AccountLabel::Default),
            None,
            "{what}: a labelled key must not configure the DEFAULT account"
        );

        let default_store: HashMap<String, String> =
            [(base.clone(), "default-value".to_string())].into_iter().collect();
        assert_eq!(
            account_var(&default_store, &base, &AccountLabel::Default),
            Some("default-value"),
            "{what}: the default account still reads today's key, unchanged"
        );
        assert_eq!(
            account_var(&default_store, &base, &alt),
            None,
            "{what}: a labelled account must NOT fall back to the default account's credential"
        );

        // Both accounts configured at once: each reads its own, which is the whole point.
        let both: HashMap<String, String> =
            [(base.clone(), "default-value".to_string()), (labelled, "labelled-value".to_string())]
                .into_iter()
                .collect();
        assert_eq!(account_var(&both, &base, &AccountLabel::Default), Some("default-value"));
        assert_eq!(account_var(&both, &base, &alt), Some("labelled-value"));
    }
}

/// ⚠ The mirror of the ambiguity argument, stated as a test rather than as prose: under the
/// REJECTED `{VENUE}_{TIER}_{LABEL}{SUFFIX}` grammar, `OANDA_DEMO_ACCOUNT_ID` would itself parse
/// as a labelled key. It does not — it is the DEFAULT account's, and the whole name is the base.
#[test]
fn a_real_bespoke_key_is_never_mistaken_for_a_labelled_one() {
    for (what, base) in bespoke_bases() {
        let split = split_account_key(&base).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(split.base, base, "{what}: today's key must be its own base");
        assert_eq!(
            split.label,
            AccountLabel::Default,
            "{what}: {base} is the DEFAULT account's key and names no account"
        );
    }
}
