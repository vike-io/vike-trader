//! The gate on `vike_model::account_keys` — **the multi-account credential-key grammar**.
//!
//! Step 1 declares a shape and changes no behaviour, so these tests are the whole of what the
//! change asserts. Three properties carry the design, and each has a test that goes red on its own:
//!
//! 1. **BYTE-IDENTITY.** An unlabelled key, the default account's `route_key`, and the parse of
//!    every key that exists today are unchanged. `the_default_account_is_byte_identical_everywhere`
//!    and `every_enumerable_grid_key_parses_as_its_own_default_account`.
//! 2. **THE AMBIGUITY IS GONE STRUCTURALLY.** The bespoke multi-word suffixes that make
//!    `{VENUE}_{TIER}_{LABEL}{SUFFIX}` unparseable are parsed correctly here, labelled and
//!    unlabelled — `a_bespoke_multi_word_suffix_is_never_read_as_a_label` is the test the design
//!    exists for, and `no_existing_credential_key_contains_the_separator` is the premise it rests
//!    on, checked against the real tree rather than asserted.
//! 3. **THE ENUMERATION COMES FROM THE STORE.** `accounts_in_store` folds key NAMES; nothing here
//!    holds a list of accounts. `the_account_set_is_derived_from_the_store_keys`.
//!
//! ⚠ This file lives in `tests/` rather than in a `#[cfg(test)]` module for a reason that is not
//! style: it spells real credential key names, and `crates/vike-ops/tests/settings_registry.rs`'s
//! direction-1 literal sweep would demand a registry row for a labelled name that nothing reads.
//! Its `read_evidence_literals` drops a `tests/` file's literals and does NOT drop a `src/` test
//! module's unless the file is in `SRC_TEST_MODULE_OVERRIDES`.

use std::fs;
use std::path::{Path, PathBuf};

use vike_model::account_keys::{
    account_key, account_ref_from_key, accounts_in_store, split_account_key, AccountKeyError,
    AccountLabel, AccountRef, ACCOUNT_SEPARATOR, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL,
};
use vike_model::credential_keys::{credential_keys, lookup_keys};
use vike_model::VENUES;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

/// A key composed rather than spelled, so this file states the venue, the tier and the suffix
/// separately and never hands the settings-registry sweep a whole name it could mistake for a read.
fn key(venue: &str, tier: &str, suffix: &str) -> String {
    format!("{}_{tier}_{suffix}", venue.to_uppercase())
}

// ---------------------------------------------------------------------------------------------
// 1. Byte-identity — the step-1 contract
// ---------------------------------------------------------------------------------------------

/// **The whole of what "changes no behaviour" means for a key name.** The default account renders
/// its key as its input, and its `route_key` as the bare venue id — which is what
/// `vike_exec::ExecutionEngine::new` already seeds `route_key` with, so a single-account process
/// routes bit-identically.
#[test]
fn the_default_account_is_byte_identical_everywhere() {
    for venue in VENUES {
        for tier in ["SIM", "DEMO", "LIVE"] {
            let base = key(venue, tier, "API_KEY");
            assert_eq!(account_key(&base, &AccountLabel::Default), base, "{base}");

            let parsed = account_ref_from_key(&base).unwrap_or_else(|| panic!("{base} is a key"));
            assert_eq!(parsed.venue, *venue);
            assert_eq!(parsed.tier, tier);
            assert!(parsed.is_default(), "{base} must be the default account");
            assert_eq!(parsed.route_key(), *venue, "{base}: the bare venue id, as today");
        }
    }
}

/// Every key the enumerable grid can produce — the tiers, the legacy tier and every suffix, over
/// the whole roster — parses back to its own venue as the DEFAULT account. Exhaustive over
/// `credential_keys()` rather than sampled: the grid is the set of names that exist today, and the
/// claim is about all of them.
#[test]
fn every_enumerable_grid_key_parses_as_its_own_default_account() {
    for name in credential_keys() {
        let parsed = account_ref_from_key(&name)
            .unwrap_or_else(|| panic!("{name} must parse as an account"));
        assert!(parsed.is_default(), "{name}");
        assert!(VENUES.contains(&parsed.venue), "{name} resolved a non-roster venue");
        assert!(name.starts_with(&parsed.venue.to_uppercase()), "{name} vs {}", parsed.venue);
        // The legacy tier NORMALIZES: a pre-rename store has one live account, not a MAINNET one.
        assert!(["SIM", "DEMO", "LIVE"].contains(&parsed.tier), "{name} -> {}", parsed.tier);
    }
}

/// The legacy spelling resolves onto the same account as the current one, and NOT to a second one.
#[test]
fn the_legacy_tier_resolves_to_the_same_live_account() {
    let legacy = account_ref_from_key(&key("bybit", "MAINNET", "API_KEY")).expect("a key");
    let current = account_ref_from_key(&key("bybit", "LIVE", "API_KEY")).expect("a key");
    assert_eq!(legacy, current, "MAINNET and LIVE are one account, not two");
    assert_eq!(legacy.tier, "LIVE");
}

// ---------------------------------------------------------------------------------------------
// 2. The ambiguity
// ---------------------------------------------------------------------------------------------

/// **The premise the whole grammar rests on**, checked against the real tree instead of asserted:
/// no credential key that exists today contains [`ACCOUNT_SEPARATOR`]. Two independent bodies of
/// evidence, because neither alone covers the keys:
///
/// * the ENUMERABLE grid (`lookup_keys`), which covers the computed `{VENUE}_{TIER}{SUFFIX}` family
///   and the attribution keys;
/// * a sweep of every bridge crate's own `src/` for venue-prefixed string literals, which is where
///   the BESPOKE shapes live — they are read with literal keys by each bridge's `config.rs` and
///   appear in no table.
///
/// If this ever fails, the separator has to change; the parse cannot be patched around it.
#[test]
fn no_existing_credential_key_contains_the_separator() {
    for name in lookup_keys() {
        assert!(!name.contains(ACCOUNT_SEPARATOR), "the enumerable grid holds {name}");
    }

    let bridges = workspace_root().join("crates").join("bridges");
    let mut swept = 0usize;
    for file in rust_sources(&bridges) {
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for lit in venue_prefixed_literals(&text) {
            swept += 1;
            assert!(
                !lit.contains(ACCOUNT_SEPARATOR),
                "{} spells {lit}, which contains the account separator",
                file.display()
            );
        }
    }
    // ⚠ An absence proves nothing unless the search would have found something. The bespoke keys
    // are the reason this sweep exists, so it must actually be seeing literals.
    assert!(swept > 50, "the literal sweep found only {swept} venue-prefixed names — it is broken");
}

/// **THE test the design exists for.** Under the obvious `{VENUE}_{TIER}_{LABEL}{SUFFIX}` spelling
/// each of these reads as *tier, then a label, then a shorter suffix* — `OANDA_DEMO_ACCOUNT_ID`
/// becomes label `ACCOUNT`, suffix `_ID`, and a real live credential silently becomes somebody's
/// second account. Under this grammar every one of them is the DEFAULT account, because none
/// contains the separator.
#[test]
fn a_bespoke_multi_word_suffix_is_never_read_as_a_label() {
    let bespoke = [
        ("oanda", "DEMO", "ACCOUNT_ID"),
        ("ig", "DEMO", "IDENTIFIER"),
        ("aster", "LIVE", "PRIVATE_KEY"),
        ("hyperliquid", "LIVE", "PRIVATE_KEY"),
        ("fxcm", "DEMO", "USER"),
        ("fxcm", "DEMO", "PASSWORD"),
    ];
    for (venue, tier, suffix) in bespoke {
        let name = key(venue, tier, suffix);
        let parsed = account_ref_from_key(&name).unwrap_or_else(|| panic!("{name} is a key"));
        assert_eq!(parsed.venue, venue, "{name}");
        assert_eq!(parsed.tier, tier, "{name}");
        assert!(
            parsed.is_default(),
            "{name} was read as account {} — this is the ambiguity the separator exists to close",
            parsed.label
        );
        // …and the split leaves the key WHOLE, which is what lets a bespoke suffix work without
        // being enumerated anywhere.
        assert_eq!(split_account_key(&name).expect("splits").base, name);
    }
}

/// The same keys, LABELLED: the suffix half survives untouched and the label comes off the end.
#[test]
fn a_labelled_bespoke_key_keeps_its_suffix_and_yields_its_label() {
    let alt = label("HEDGE");
    for (venue, tier, suffix) in [
        ("oanda", "DEMO", "ACCOUNT_ID"),
        ("ig", "LIVE", "IDENTIFIER"),
        ("bybit", "DEMO", "API_KEY"),
    ] {
        let base = key(venue, tier, suffix);
        let labelled = account_key(&base, &alt);
        assert_eq!(labelled, format!("{base}{ACCOUNT_SEPARATOR}HEDGE"));

        let split = split_account_key(&labelled).expect("splits");
        assert_eq!(split.base, base, "the suffix half must survive verbatim");
        assert_eq!(split.label, alt);

        let parsed = account_ref_from_key(&labelled).expect("an account");
        assert_eq!(parsed, AccountRef { venue, tier, label: alt.clone() });
        assert_eq!(parsed.route_key(), format!("{venue}#HEDGE"));
    }
}

/// A label is `[A-Z0-9]+`, bounded, case-sensitive, and never the reserved spelling. Each refusal
/// is a distinct variant because each has a distinct fix.
#[test]
fn a_label_is_uppercase_alphanumeric_bounded_and_never_the_reserved_word() {
    assert_eq!(AccountLabel::parse("ALT"), Ok(AccountLabel::Named("ALT".to_string())));
    assert_eq!(AccountLabel::parse("A2"), Ok(AccountLabel::Named("A2".to_string())));
    assert_eq!(AccountLabel::parse(""), Err(AccountKeyError::EmptyLabel));
    assert_eq!(AccountLabel::parse("alt"), Err(AccountKeyError::LabelChar('a')));
    assert_eq!(AccountLabel::parse("A_B"), Err(AccountKeyError::LabelChar('_')));
    assert_eq!(AccountLabel::parse("A-B"), Err(AccountKeyError::LabelChar('-')));
    assert_eq!(AccountLabel::parse(RESERVED_DEFAULT_LABEL), Err(AccountKeyError::ReservedLabel));

    let long = "A".repeat(MAX_LABEL_LEN + 1);
    assert_eq!(AccountLabel::parse(&long), Err(AccountKeyError::LabelTooLong(MAX_LABEL_LEN + 1)));
    assert!(AccountLabel::parse(&"A".repeat(MAX_LABEL_LEN)).is_ok(), "the bound is inclusive");

    // Every refusal explains itself — the message is what an operator gets from the loader.
    for err in [
        AccountKeyError::EmptyLabel,
        AccountKeyError::EmptyBase,
        AccountKeyError::LabelChar('_'),
        AccountKeyError::LabelTooLong(99),
        AccountKeyError::ReservedLabel,
    ] {
        assert!(err.to_string().len() > 30, "{err:?} explains nothing");
    }
}

/// The split refuses what it cannot address, rather than guessing. ⚠ The underscore refusal is what
/// makes "split at the FIRST separator" total: a second separator lands inside the label and the
/// label charset rejects it, so no input can be ambiguous about which separator was taken.
#[test]
fn the_split_refuses_a_malformed_label_rather_than_guessing() {
    let base = key("bybit", "DEMO", "API_KEY");
    assert_eq!(
        split_account_key(&format!("{base}{ACCOUNT_SEPARATOR}")),
        Err(AccountKeyError::EmptyLabel)
    );
    assert_eq!(
        split_account_key(&format!("{base}{ACCOUNT_SEPARATOR}A{ACCOUNT_SEPARATOR}B")),
        Err(AccountKeyError::LabelChar('_')),
        "a second separator is refused, so the FIRST-separator split is never ambiguous"
    );
    assert_eq!(
        split_account_key(&format!("{ACCOUNT_SEPARATOR}ALT")),
        Err(AccountKeyError::EmptyBase)
    );
    // …and a refused name is not an account at all, rather than being read as its base.
    assert_eq!(account_ref_from_key(&format!("{base}{ACCOUNT_SEPARATOR}")), None);
}

/// The families that are deliberately NOT accounts — a classification, not a gap. The last two are
/// the genuinely non-conforming credential shapes; pinning them by name means a change to either
/// is a red test rather than a silent reclassification.
#[test]
fn the_non_account_key_families_are_classified_and_pinned() {
    let not_accounts = [
        // attribution: the token after the venue is no tier.
        format!("{}_BROKER_CODE", "OKX"),
        format!("{}_BUILDER_CODE", "BINANCE"),
        // no roster venue is spelled `POLY` or `JFOREX`.
        format!("{}_API_KEY", "POLY"),
        format!("{}_BRIDGE_JAR", "JFOREX"),
        // dukascopy bakes an account INDEX into the tier token — the one venue already shipping a
        // multi-account shape, and one this grammar deliberately does not retro-fit.
        key("dukascopy", "DEMO1", "LOGIN"),
        // aster's TESTNET tier is outside the tier table.
        key("aster", "TESTNET", "PRIVATE_KEY"),
        // a venue and a tier and nothing else names no credential.
        format!("{}_DEMO", "BYBIT"),
        format!("{}_DEMO_", "BYBIT"),
        // a venue that is not on the roster.
        format!("{}_DEMO_API_KEY", "KRAKEN"),
    ];
    for name in not_accounts {
        assert_eq!(account_ref_from_key(&name), None, "{name} must not classify as an account");
    }
}

/// The longest-prefix venue match is not decoration: it is what keeps the parse correct if the
/// roster ever holds two ids in a prefix relation. Today it holds none, which is what this pins —
/// so a future venue that breaks the assumption fails HERE, next to the code that handles it.
#[test]
fn the_roster_has_no_prefix_pairs() {
    for a in VENUES {
        for b in VENUES {
            if a != b {
                assert!(!b.starts_with(*a), "{b} starts with {a} — the longest match now matters");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 3. The enumeration
// ---------------------------------------------------------------------------------------------

/// **The account set comes from the STORE.** A realistic store — one venue with a second account,
/// one with only a default, a bespoke-suffix venue, and three names that are not credential keys —
/// folds to exactly the accounts it holds, sorted and deduplicated. Nothing here is a list of
/// accounts anybody maintains.
#[test]
fn the_account_set_is_derived_from_the_store_keys() {
    let alt = label("ALT");
    let store: Vec<String> = vec![
        key("bybit", "DEMO", "API_KEY"),
        key("bybit", "DEMO", "API_SECRET"),
        account_key(&key("bybit", "DEMO", "API_KEY"), &alt),
        account_key(&key("bybit", "DEMO", "API_SECRET"), &alt),
        key("binance", "LIVE", "API_KEY"),
        key("oanda", "DEMO", "ACCOUNT_ID"),
        // …and three names a real store holds that are not credential keys.
        format!("{}_BROKER_CODE", "OKX"),
        format!("{}_BRIDGE_JAR", "JFOREX"),
        key("dukascopy", "DEMO1", "LOGIN"),
    ];
    let found = accounts_in_store(store.iter().map(String::as_str));
    assert_eq!(
        found,
        vec![
            AccountRef { venue: "binance", tier: "LIVE", label: AccountLabel::Default },
            AccountRef { venue: "bybit", tier: "DEMO", label: AccountLabel::Default },
            AccountRef { venue: "bybit", tier: "DEMO", label: alt },
            AccountRef { venue: "oanda", tier: "DEMO", label: AccountLabel::Default },
        ],
        "one account per (venue, tier, label), whatever the key count"
    );

    // The single-account box: exactly one account, and it is the one addressed today.
    let today = [key("bybit", "DEMO", "API_KEY"), key("bybit", "DEMO", "API_SECRET")];
    let found = accounts_in_store(today.iter().map(String::as_str));
    assert_eq!(found.len(), 1);
    assert!(found[0].is_default());
    assert_eq!(found[0].route_key(), "bybit");

    // An empty store holds no accounts — the live gate's own shape.
    assert!(accounts_in_store(std::iter::empty()).is_empty());
}

/// A `route_key` becomes a `LIVE-<route_key>.lock` FILENAME, so every account's must be path-safe
/// on both platforms — the constraint `ExecutionEngine::route_key`'s own doc states and nothing
/// checked, because nothing set the field.
#[test]
fn every_route_key_is_path_safe() {
    let mut refs: Vec<AccountRef> = Vec::new();
    for venue in VENUES {
        refs.push(AccountRef { venue, tier: "LIVE", label: AccountLabel::Default });
        refs.push(AccountRef { venue, tier: "LIVE", label: label("ALT2") });
        refs.push(AccountRef { venue, tier: "LIVE", label: label(&"Z".repeat(MAX_LABEL_LEN)) });
    }
    for r in refs {
        let rk = r.route_key();
        assert!(!rk.is_empty());
        for bad in ['/', '\\', ':', '*', '?', '"', '<', '>', '|'] {
            assert!(!rk.contains(bad), "{rk} carries {bad:?}");
        }
        assert!(!rk.contains(".."), "{rk}");
        assert!(rk.starts_with(r.venue), "{rk} must stay attributable to its venue");
    }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

/// Every `.rs` file under a bridge crate's `src/`, skipping the vendored upstream copy (frozen
/// third-party code, and not a place a vike credential key can be spelled).
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if name != "vendor" && name != "target" {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// String literals in `text` that look like a credential key for a roster venue: `{VENUE}_…`, all
/// uppercase/digit/underscore. Comments are not stripped — a comment's prose cannot satisfy the
/// shape, and `__cxa_finalize`-style tokens in this tree are lowercase and unquoted.
fn venue_prefixed_literals(text: &str) -> Vec<String> {
    let prefixes: Vec<String> = VENUES.iter().map(|v| format!("{}_", v.to_uppercase())).collect();
    text.split('"')
        .skip(1)
        .step_by(2)
        .filter(|lit| {
            lit.len() >= 5
                && lit.starts_with(|c: char| c.is_ascii_uppercase())
                && lit.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && prefixes.iter().any(|p| lit.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}
