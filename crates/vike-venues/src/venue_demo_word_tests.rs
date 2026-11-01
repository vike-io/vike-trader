use super::*;
use vike_model::VENUES;

/// **The verbatim matrix pin**: the whole table in roster order. A venue whose bridge moves its
/// demo endpoint to another environment changes here too, in a diff a reviewer reads as "the
/// operator is now told a different word".
#[test]
fn the_demo_word_matrix_is_pinned() {
    let matrix: Vec<(&str, Option<&str>)> =
        VENUES.iter().map(|v| (*v, venue_demo_word(v))).collect();
    assert_eq!(
        matrix,
        vec![
            ("binance", Some("demo trading")),
            ("bybit", Some("demo trading")),
            ("okx", Some("demo trading")),
            ("deribit", Some("testnet")),
            ("oanda", Some("practice")),
            ("ig", Some("demo")),
            ("fxcm", Some("demo")),
            ("dukascopy", Some("demo")),
            ("polymarket", None),
            ("ibkr", Some("paper account")),
            ("ctrader", Some("demo")),
            ("alpaca", Some("sandbox")),
            ("aster", Some("testnet")),
            ("hyperliquid", Some("testnet")),
        ],
        "a venue's own word for our `demo` moved — cite the endpoint the bridge now dials"
    );
}

/// **Completeness**: every roster venue is a NAMED row, so adding a bridge fails here until its
/// word is read off its demo endpoint — even when the answer is "it has none".
#[test]
fn every_roster_venue_has_a_named_demo_word_row() {
    for venue in VENUES {
        assert!(
            named_row(venue).is_some(),
            "{venue} rides the unknown-venue fallback — give it a NAMED row, even if it is `None`"
        );
    }
    assert_eq!(named_row("no-such-venue"), None);
    assert_eq!(venue_demo_word("no-such-venue"), None, "an unknown string has no word");
}

/// The words are DISPLAY text: lowercase, so a renderer can put one after a venue id without
/// re-casing it, and never a tier word of our own spelled bare — `paper`, `demo` or `live` alone
/// would read as one of OUR tiers, which is the confusion the hint exists to remove. (`demo` IS one
/// row's word: the venues that call it that; the guard is on `paper` and `live`.)
#[test]
fn a_word_is_lowercase_and_never_a_bare_tier_word_that_misleads() {
    for venue in VENUES {
        let Some(word) = venue_demo_word(venue) else { continue };
        assert!(!word.is_empty(), "{venue}");
        assert_eq!(word, word.to_lowercase(), "{venue}: `{word}` must be lowercase");
        assert!(word != "paper" && word != "live", "{venue}: `{word}` reads as our own tier");
    }
}
