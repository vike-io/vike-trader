//! Property tests for the symbol-name reader the offline refusals stand on (`vike_catalog::fiat_quote_of`
//! and its two callers, decision 0183 item 13): it never panics on any text, it reads a generated pair
//! name in every spelling to the quote it was built from, it never mistakes a crypto concatenation for
//! a fiat pair, and its list names an offender once however often it appears.
//!
//! A refusal that fires on a name that is not foreign is a wrong refusal of a run that worked, and one
//! that misses a foreign name books yen as dollars, so both directions are properties here.

use proptest::prelude::*;
use vike_catalog::{fiat_quote_of, foreign_fiat_quote_of, foreign_fiat_quoted_list};

const FIAT: &[&str] = &["USD", "EUR", "GBP", "JPY", "CHF", "AUD", "CAD", "NZD", "SEK", "ZAR"];
const SEPARATORS: &[&str] = &["", "_", "/", ".", "-"];
const CRYPTO_BASES: &[&str] = &["BTC", "ETH", "SOL", "DOGE", "AVAX", "HYPE", "ARB"];
const STABLES: &[&str] = &["USDT", "USDC", "BUSD", "FDUSD"];

fn fiat() -> impl Strategy<Value = &'static str> {
    prop::sample::select(FIAT)
}

fn separator() -> impl Strategy<Value = &'static str> {
    prop::sample::select(SEPARATORS)
}

proptest! {
    /// Any text at all is answered or declined, never a panic: a symbol name reaches this from a
    /// store, a journal and a wire frame, and a byte-sliced non-ASCII token must not abort a run.
    #[test]
    fn any_text_is_answered_or_declined(text in "\\PC{0,40}") {
        if let Some(quote) = fiat_quote_of(&text) {
            prop_assert_eq!(quote.len(), 3, "a three-letter table code: {}", quote);
            prop_assert!(quote.bytes().all(|b| b.is_ascii_uppercase()), "{}", quote);
        }
    }

    /// A pair built from two fiat codes, in either case and any separator, is quoted in its second
    /// code. (`base` + `quote` with no separator is the six-letter token; with one it is two tokens.)
    #[test]
    fn a_generated_pair_reads_to_its_quote(
        base in fiat(),
        quote in fiat(),
        sep in separator(),
        lower in any::<bool>(),
    ) {
        let name = format!("{base}{sep}{quote}");
        let name = if lower { name.to_ascii_lowercase() } else { name };
        prop_assert_eq!(fiat_quote_of(&name), Some(quote), "{}", name);
    }

    /// A venue's own name around the pair (an IG epic, a dotted exchange suffix) changes nothing.
    #[test]
    fn an_epic_around_a_pair_reads_to_its_quote(base in fiat(), quote in fiat()) {
        let epic = format!("CS.D.{base}{quote}.CFD.IP");
        prop_assert_eq!(fiat_quote_of(&epic), Some(quote), "{}", epic);
    }

    /// A crypto base on a stablecoin, spelled any way, is never a fiat quote: the offline refusals must
    /// not touch a run that worked.
    #[test]
    fn a_crypto_pair_on_a_stable_is_never_fiat(
        base in prop::sample::select(CRYPTO_BASES),
        stable in prop::sample::select(STABLES),
        sep in separator(),
        suffix in prop::sample::select(&["", ".P", "-SWAP", "-PERPETUAL"][..]),
    ) {
        let name = format!("{base}{sep}{stable}{suffix}");
        prop_assert_eq!(fiat_quote_of(&name), None, "{}", name);
    }

    /// The foreign reading is the fiat reading minus the dollar.
    #[test]
    fn foreign_is_fiat_minus_the_dollar(base in fiat(), quote in fiat(), sep in separator()) {
        let name = format!("{base}{sep}{quote}");
        let want = if quote == "USD" { None } else { Some(quote) };
        prop_assert_eq!(foreign_fiat_quote_of(&name), want, "{}", name);
    }

    /// Listing a symbol twice names it once, and repeating a whole list changes nothing.
    #[test]
    fn the_list_names_an_offender_once(
        pairs in prop::collection::vec((fiat(), fiat()), 0..6),
    ) {
        let names: Vec<String> = pairs.iter().map(|(b, q)| format!("{b}_{q}")).collect();
        let once = foreign_fiat_quoted_list(names.iter().map(String::as_str));
        let twice = foreign_fiat_quoted_list(names.iter().chain(names.iter()).map(String::as_str));
        prop_assert_eq!(&once, &twice);
        if let Some(list) = once {
            let offenders = pairs.iter().filter(|(_, q)| *q != "USD").count();
            prop_assert!(list.matches('(').count() <= offenders, "{list}");
        }
    }
}
