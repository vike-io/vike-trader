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
//!    `vike_execution::Engine::new` already seeds), every key is path-safe, and
//!    `label_of_route_key` inverts `route_key_of` — `the_default_account_routes_by_the_bare_venue_id`,
//!    `every_route_key_is_path_safe` and the two inverse tests.
//! 3. **THE WIRE'S ONE DIVERGENCE.** `parse_wire_account` admits the reserved word and delegates
//!    every other rule.
//! 4. **THE BOOK TOKEN.** `parse_book_token` is what `vike-cli`'s selector and the Telegram channel
//!    both read — `a_book_token_names_a_venue_and_an_account_or_none` and
//!    `a_book_tokens_refusals_are_pinned_byte_for_byte`; a strategy's leg name reads its account through
//!    it — `a_leg_name_is_a_symbol_on_the_account_its_book_token_names`.
//! 5. **THE ACCOUNT A PEER NAMES.** `AccountNamed`: a roster account is typed, anything else is kept
//!    as received and routed by the same rule — the three tests of section 5.

use std::assert_matches;

use vike_model::VENUES;
use vike_model::accounts::account_keys::{
    AccountKeyError, AccountLabel, AccountNamed, AccountRef, BOOK_FORMS, LegOnAccount,
    MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL, label_of_route_key, parse_book_token, parse_leg_name,
    parse_wire_account, route_key_of, venue_of_route_key,
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

/// **The default account's routing key is the bare venue id** — what `vike_execution::Engine`'s
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
/// on both platforms — the constraint `Engine::route_key`'s own doc states and nothing
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
/// engine is (`vike_execution::VenueBlock::account`) runs this direction.
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

/// **`venue_of_route_key` reads back the venue every key was rendered for**, over every roster venue,
/// for the default account (the bare id) and labelled ones. A key whose label is empty or not a legal
/// one still names its venue: the label is `label_of_route_key`'s to judge.
#[test]
fn the_venue_of_every_route_key_is_the_venue_it_was_rendered_for() {
    for venue in VENUES {
        for want in [AccountLabel::Default, label("ALT"), label("HEDGE2")] {
            let key = route_key_of(venue, &want);
            assert_eq!(venue_of_route_key(&key), *venue, "{key}");
        }
    }
    assert_eq!(venue_of_route_key("binance#"), "binance");
    assert_eq!(venue_of_route_key("binance#alt"), "binance");
    assert_eq!(venue_of_route_key("sim"), "sim");
    assert_eq!(venue_of_route_key(""), "");
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

// ---------------------------------------------------------------------------------------------
// 4. The book token
// ---------------------------------------------------------------------------------------------

/// A bare venue names NO account and `/DEFAULT` names the unlabelled one: different words on a write
/// (decision 0254), so the reading keeps them apart.
#[test]
fn a_book_token_names_a_venue_and_an_account_or_none() {
    assert_eq!(parse_book_token("binance"), Ok(("binance", None)));
    assert_eq!(parse_book_token("binance/DEFAULT"), Ok(("binance", Some(AccountLabel::Default))));
    assert_eq!(parse_book_token("binance/ALT"), Ok(("binance", Some(label("ALT")))));
}

/// The refusals `vike-cli` has always printed, byte for byte: they are public words, and now the
/// Telegram channel's too.
#[test]
fn a_book_tokens_refusals_are_pinned_byte_for_byte() {
    let forms = "<venue> | <venue>/<LABEL> | <venue>/DEFAULT | @<mark>";
    assert_eq!(BOOK_FORMS, forms);
    let bad_char = AccountKeyError::LabelChar('a');
    for (token, want) in [
        ("", format!("an empty book selector - expected one of: {forms}")),
        (
            "@hedge",
            format!(
                "`@hedge` is a MARK, and marks are not built yet - name the book directly. The \
                 forms are: {forms}"
            ),
        ),
        (
            "binance#ALT",
            format!(
                "'binance#ALT' is a ROUTE KEY, not a book selector — this plane has ONE addressing \
                 grammar and it spells an account with '/', never '#'. Name it as `binance/ALT` \
                 instead. The forms are: {forms}"
            ),
        ),
        (
            "a/B/C",
            format!(
                "'a/B/C' has more than one '/' - a selector names a venue and at most one account. \
                 The forms are: {forms}"
            ),
        ),
        ("binance/alt", format!("'binance/alt': {bad_char} The forms are: {forms}")),
        ("/ALT", format!("'/ALT' names no venue - expected one of: {forms}")),
    ] {
        assert_eq!(parse_book_token(token), Err(want), "{token:?}");
    }
}

/// A strategy's leg name (decision 0293): `SYMBOL@BOOK` names the account, the book token read by the
/// one rule above; a plain symbol, and a name with nothing before its `@`, name no account.
#[test]
fn a_leg_name_is_a_symbol_on_the_account_its_book_token_names() {
    let on = |symbol, venue, account| Some(LegOnAccount { symbol, venue, account });
    assert_eq!(parse_leg_name("BTCUSDT.P"), Ok(None));
    assert_eq!(parse_leg_name("@107"), Ok(None), "nothing before the `@`: a plain name");
    assert_eq!(
        parse_leg_name("BTCUSDT.P@bybit/DEMO"),
        Ok(on("BTCUSDT.P", "bybit", Some(label("DEMO"))))
    );
    assert_eq!(parse_leg_name("BTCUSDT.P@bybit"), Ok(on("BTCUSDT.P", "bybit", None)));
    assert_eq!(
        parse_leg_name("XYZ:TSLAUSDC.P@hyperliquid/DEFAULT"),
        Ok(on("XYZ:TSLAUSDC.P", "hyperliquid", Some(AccountLabel::Default)))
    );
    let refused = parse_leg_name("BTCUSDT.P@bybit#DEMO").expect_err("a route key is no book");
    assert!(refused.starts_with("the leg `BTCUSDT.P@bybit#DEMO` names no account: "), "{refused}");
}

// ---------------------------------------------------------------------------------------------
// 5. The account a peer names (owner ruling 2026-11-12, variant 2B)
// ---------------------------------------------------------------------------------------------

/// A roster venue is a typed account, and its routing key is `route_key_of`'s, never carried beside it.
#[test]
fn a_roster_venues_account_is_known_and_routes_by_the_one_rule() {
    for &venue in VENUES {
        for l in [AccountLabel::Default, label("ALT")] {
            let named = AccountNamed::of(venue, l.clone());
            assert_eq!(named, AccountNamed::Known(AccountRef { venue, label: l.clone() }));
            assert_eq!(named.route_key(), route_key_of(venue, &l), "{venue}");
            assert_eq!((named.venue(), named.label()), (venue, Some(l)));
        }
    }
    let wire = AccountNamed::from_wire("binance", Some("ALT"));
    assert_eq!(wire, AccountNamed::of("binance", label("ALT")));
    assert_eq!(AccountNamed::from_wire("binance", None).route_key(), "binance");
    assert_eq!(AccountNamed::from_wire("binance", Some("DEFAULT")).route_key(), "binance");
}

/// **Version skew is never dropped**: a venue this build's roster does not have (`kalshi`, a newer node's) and a
/// label its grammar does not read are ONE explicit case, the venue and label as received, routed by the same
/// rendering, and its label is `None` to a reader that needs a parsed one.
#[test]
fn an_account_this_build_cannot_name_is_kept_as_received() {
    let kalshi = AccountNamed::from_wire("kalshi", Some("main"));
    assert_eq!(
        kalshi,
        AccountNamed::Unknown { venue: "kalshi".into(), label: Some("main".into()) }
    );
    assert_eq!((kalshi.venue(), kalshi.label_text()), ("kalshi", Some("main")));
    assert_eq!(kalshi.route_key(), "kalshi#main");
    assert_eq!(kalshi.label(), None, "`main` is no label of this build's grammar");

    let skewed = AccountNamed::from_wire("binance", Some("main"));
    assert!(matches!(skewed, AccountNamed::Unknown { .. }), "{skewed:?}");
    assert_eq!(skewed.route_key(), "binance#main", "never the default account's `binance`");

    let planted = AccountNamed::of("sim", label("ALT"));
    assert_eq!(planted, AccountNamed::Unknown { venue: "sim".into(), label: Some("ALT".into()) });
    assert_eq!(planted.route_key(), route_key_of("sim", &label("ALT")));
    assert_eq!(planted.label(), Some(label("ALT")));
    assert_eq!(AccountNamed::of("sim", AccountLabel::Default).route_key(), "sim");
}

/// A key reads back to the account it was minted for, and a key whose label this build does not read keeps itself.
#[test]
fn an_account_read_off_its_route_key_keeps_the_key() {
    for (venue, key) in [("binance", "binance"), ("binance", "binance#ALT"), ("sim", "sim#ALT")] {
        assert_eq!(AccountNamed::from_route_key(venue, key).route_key(), key);
    }
    let odd = AccountNamed::from_route_key("binance", "binance#main");
    assert_eq!(odd, AccountNamed::Unknown { venue: "binance".into(), label: Some("main".into()) });
    assert_eq!(odd.route_key(), "binance#main");
}
