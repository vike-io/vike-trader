//! The gate on `vike_model::accounts::account_keys` — **the account LABEL grammar and the ROUTING
//! key that addresses one account's engine**.
//!
//! Since decision 0156 item 6 a label names an `account` ROW and never a credential name: no
//! credential name carries a label or a tier, and nothing parses one out of a name. What is left
//! here is what the module still owns, each with a test that goes red on its own:
//!
//! 1. **THE LABEL GRAMMAR.** `[A-Z0-9]+`, bounded, case-sensitive, never the reserved word —
//!    `a_label_is_uppercase_alphanumeric_bounded_and_never_the_reserved_word`.
//! 2. **THE ROUTING KEY.** The default account's key is the bare venue id (what
//!    `vike_exec::ExecutionEngine::new` already seeds), every key is path-safe, and
//!    `label_of_route_key` inverts `route_key_of` — `the_default_account_routes_by_the_bare_venue_id`,
//!    `every_route_key_is_path_safe` and the two inverse tests.
//! 3. **THE WIRE'S ONE DIVERGENCE.** `parse_wire_account` admits the reserved word and delegates
//!    every other rule.

use std::assert_matches;

use vike_model::VENUES;
use vike_model::accounts::account_keys::{
    AccountKeyError, AccountLabel, AccountRef, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL,
    label_of_route_key, parse_wire_account, route_key_of,
};

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

// ---------------------------------------------------------------------------------------------
// 1. The label grammar
// ---------------------------------------------------------------------------------------------

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
        AccountKeyError::LabelChar('_'),
        AccountKeyError::LabelTooLong(99),
        AccountKeyError::ReservedLabel,
    ] {
        assert!(err.to_string().len() > 30, "{err:?} explains nothing");
    }
}

/// The default account has NO label text, and its screen rendering is the reserved word — the one
/// spelling `AccountLabel::parse` refuses, so a rendering can never be read back as a second account.
#[test]
fn the_default_account_has_no_text_and_renders_as_the_reserved_word() {
    assert_eq!(AccountLabel::Default.text(), None);
    assert!(AccountLabel::Default.is_default());
    assert_eq!(AccountLabel::Default.to_string(), RESERVED_DEFAULT_LABEL);
    assert_eq!(label("ALT").text(), Some("ALT"));
    assert!(!label("ALT").is_default());
    assert_eq!(label("ALT").to_string(), "ALT");
}

// ---------------------------------------------------------------------------------------------
// 2. The routing key
// ---------------------------------------------------------------------------------------------

/// **The default account's routing key is the bare venue id** — what `vike_exec::ExecutionEngine`'s
/// `new` already seeds `route_key` with — so a single-account process routes bit-identically, and a
/// labelled account suffixes `#{LABEL}`.
#[test]
fn the_default_account_routes_by_the_bare_venue_id() {
    for venue in VENUES {
        let default = AccountRef { venue, label: AccountLabel::Default };
        assert!(default.is_default());
        assert_eq!(default.route_key(), *venue, "{venue}: the bare venue id");
        assert_eq!(route_key_of(venue, &AccountLabel::Default), *venue);

        let labelled = AccountRef { venue, label: label("HEDGE") };
        assert!(!labelled.is_default());
        assert_eq!(labelled.route_key(), format!("{venue}#HEDGE"));
    }
}

/// A `route_key` becomes a `LIVE-<route_key>.lock` FILENAME, so every account's must be path-safe
/// on both platforms — the constraint `ExecutionEngine::route_key`'s own doc states and nothing
/// checked, because nothing set the field.
#[test]
fn every_route_key_is_path_safe() {
    let mut refs: Vec<AccountRef> = Vec::new();
    for venue in VENUES {
        refs.push(AccountRef { venue, label: AccountLabel::Default });
        refs.push(AccountRef { venue, label: label("ALT2") });
        refs.push(AccountRef { venue, label: label(&"Z".repeat(MAX_LABEL_LEN)) });
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

/// **`label_of_route_key` is `route_key_of`'s inverse over every roster venue**, for the default
/// account and for a labelled one — the round trip, asserted rather than assumed, because the two
/// functions spell the `#` convention separately and a reader that wants to SAY which account an
/// engine is (`vike_exec::VenueBlock::account`) runs this direction.
#[test]
fn the_route_key_inverse_round_trips_for_every_venue() {
    for venue in VENUES {
        for want in [AccountLabel::Default, label("ALT"), label("HEDGE2")] {
            let key = route_key_of(venue, &want);
            assert_eq!(
                label_of_route_key(venue, &key).as_ref(),
                Some(&want),
                "{venue}: {key} must invert to the label it was rendered from"
            );
        }
    }
}

/// …and it answers `None` rather than inventing a label: a key belonging to ANOTHER venue, a key
/// whose suffix is not a legal label, and the empty suffix — the `account: Some("")` hole §9 item 5
/// of the account-routing spec forbids explicitly, one plane down.
#[test]
fn a_route_key_that_is_not_this_venues_names_no_account() {
    assert_eq!(label_of_route_key("binance", "bybit"), None, "another venue entirely");
    assert_eq!(label_of_route_key("binance", "bybit#ALT"), None, "…labelled, still another venue");
    assert_eq!(
        label_of_route_key("binance", "binance#"),
        None,
        "an EMPTY label is not the default"
    );
    assert_eq!(label_of_route_key("binance", "binance#alt"), None, "a lower-case label is refused");
    assert_eq!(
        label_of_route_key("binance", &format!("binance#{RESERVED_DEFAULT_LABEL}")),
        None,
        "the reserved spelling never round-trips out of a key nothing mints"
    );
    // A venue this workspace does not carry can only ever be its own default account, which is
    // what `route_key_of`'s own doc says about the bare-id arm.
    assert_eq!(label_of_route_key("sim", "sim"), Some(AccountLabel::Default));
}

// -------------------------------------------------------------------------------------------
// 3. `parse_wire_account` — the WIRE's spelling, and the asymmetry it deliberately carries
// -------------------------------------------------------------------------------------------

/// **The one word the wire admits and an account label refuses**, asserted as a PAIR so the
/// asymmetry cannot be "tidied" away by making the two agree.
///
/// An account label refuses `DEFAULT` because there it would be a second name for an account that
/// already has a home (the venue's account row with no label). The wire has no second home: a
/// routing DESTINATION for the default account has exactly one spelling.
#[test]
fn the_reserved_word_is_a_wire_destination_and_a_file_refusal() {
    assert_eq!(
        parse_wire_account(RESERVED_DEFAULT_LABEL).expect("the wire must address the default"),
        AccountLabel::Default
    );
    assert_matches!(
        AccountLabel::parse(RESERVED_DEFAULT_LABEL),
        Err(AccountKeyError::ReservedLabel),
        "the FILE must keep refusing it — two questions, only one already had an answer"
    );
}

/// Every OTHER rule is `AccountLabel::parse`'s, verbatim — one implementation, so the charset, the
/// length bound and the case-sensitivity cannot drift between the file and the wire.
#[test]
fn every_other_rule_is_delegated_and_cannot_drift() {
    for bad in ["", "alt", "A_B", "A-B", "ALT!", &"A".repeat(MAX_LABEL_LEN + 1)] {
        assert_eq!(
            parse_wire_account(bad).is_err(),
            AccountLabel::parse(bad).is_err(),
            "the wire and the file must agree about {bad:?}"
        );
    }
    assert_eq!(
        parse_wire_account("ALT").expect("a plain label parses"),
        AccountLabel::Named("ALT".to_string())
    );
}

/// ⚠ The reserved word is the ONLY divergence. Asserted by construction rather than by example:
/// for every other input the two functions answer identically, including on what they ACCEPT.
#[test]
fn the_reserved_word_is_the_only_divergence() {
    for text in ["ALT", "HEDGE2", "A", "ZZZ9", "default", "Default", "DEFAULT2"] {
        assert_eq!(
            parse_wire_account(text).ok(),
            AccountLabel::parse(text).ok(),
            "only `{RESERVED_DEFAULT_LABEL}` may differ; {text:?} did"
        );
    }
}
