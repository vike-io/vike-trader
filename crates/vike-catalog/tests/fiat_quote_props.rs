//! Property tests for the symbol-name reader the offline refusals stand on (`vike_catalog::fiat_quote_of`
//! and its two callers, decision 0183 item 13): it never panics on any text, it reads a generated pair
//! name in every spelling to the quote it was built from, it never mistakes a crypto concatenation for
//! a fiat pair, and its list names an offender once however often it appears.
//!
//! A refusal that fires on a name that is not foreign is a wrong refusal of a run that worked, and one
//! that misses a foreign name books yen as dollars, so both directions are properties here.

use proptest::prelude::*;
use vike_catalog::{
    fiat_code, fiat_quote_of, foreign_fiat_quote_of, foreign_fiat_quoted_list, off_ledger_list,
};

const FIAT: &[&str] = &["USD", "EUR", "GBP", "JPY", "CHF", "AUD", "CAD", "NZD", "SEK", "ZAR"];
const SEPARATORS: &[&str] = &["", "_", "/", ".", "-"];
const CRYPTO_BASES: &[&str] = &["BTC", "ETH", "SOL", "DOGE", "AVAX", "HYPE", "ARB"];
const STABLES: &[&str] = &["USDT", "USDC", "BUSD", "FDUSD"];

/// Three-letter asset tickers, none of which is a fiat code.
const ASSETS3: &[&str] = &["BTC", "ETH", "SOL", "XRP", "ADA", "BNB", "LTC", "XAU"];
/// Four-and-five-letter crypto bases.
const BASES_LONG: &[&str] = &["DOGE", "AVAX", "SHIB", "LINK", "MATIC"];
const NON_USD_FIAT: &[&str] = &["EUR", "GBP", "JPY", "CHF", "AUD", "CAD", "NZD", "SEK", "ZAR"];
/// Broker suffixes of one to three lowercase letters (MT4/MT5 spellings).
const BROKER_SUFFIXES: &[&str] = &["m", "c", "i", "z", "pro", "ecn"];

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

    /// ⚠ A crypto asset quoted in a fiat currency states it with no separator (`BTCEUR`), in either
    /// case, and it is read: `None` there was "dollars" on the dollar ledger. A fiat PAIR is not
    /// misread as "asset + fiat" (the asset must not itself be a fiat code), which the generated
    /// pairs above already pin from the other side.
    #[test]
    fn an_asset_on_a_fiat_code_reads_to_the_fiat(
        asset in prop::sample::select(ASSETS3),
        quote in fiat(),
        lower in any::<bool>(),
    ) {
        let name = format!("{asset}{quote}");
        let name = if lower { name.to_ascii_lowercase() } else { name };
        prop_assert_eq!(fiat_quote_of(&name), Some(quote), "{}", name);
    }

    /// A longer base on a NON-dollar fiat code is read too (`DOGEEUR`, `MATICTRY`-shaped names),
    /// and the same base on the dollar is left alone (`None` is the dollar's own answer).
    #[test]
    fn a_longer_base_is_read_on_a_non_dollar_code_and_left_alone_on_the_dollar(
        base in prop::sample::select(BASES_LONG),
        quote in prop::sample::select(NON_USD_FIAT),
    ) {
        prop_assert_eq!(fiat_quote_of(&format!("{base}{quote}")), Some(quote));
        prop_assert_eq!(fiat_quote_of(&format!("{base}USD")), None);
    }

    /// A fiat pair with a broker suffix reads to the pair's quote, in a token or after a dot; and a
    /// dollar quote followed by an UPPERCASE stablecoin tail (`EURUSDT`) is not a suffixed pair.
    #[test]
    fn a_broker_suffix_is_read_and_a_stable_tail_is_not(
        base in fiat(),
        quote in fiat(),
        suffix in prop::sample::select(BROKER_SUFFIXES),
        tail in prop::sample::select(&["T", "C"][..]),
    ) {
        prop_assert_eq!(fiat_quote_of(&format!("{base}{quote}{suffix}")), Some(quote));
        prop_assert_eq!(fiat_quote_of(&format!("{base}{quote}.{suffix}")), Some(quote));
        prop_assert_eq!(fiat_quote_of(&format!("{base}USD{tail}")), None);
    }

    /// The foreign reading is the fiat reading minus the dollar.
    #[test]
    fn foreign_is_fiat_minus_the_dollar(base in fiat(), quote in fiat(), sep in separator()) {
        let name = format!("{base}{sep}{quote}");
        let want = if quote == "USD" { None } else { Some(quote) };
        prop_assert_eq!(foreign_fiat_quote_of(&name), want, "{}", name);
    }

    /// A pair fits a ledger exactly when its quote IS that ledger, whatever case the ledger was typed
    /// in; on the dollar ledger that is the old foreign reading, byte for byte.
    #[test]
    fn a_pair_fits_exactly_its_own_ledger(
        base in fiat(),
        quote in fiat(),
        ledger in fiat(),
        sep in separator(),
        lower in any::<bool>(),
    ) {
        let name = format!("{base}{sep}{quote}");
        let typed = if lower { ledger.to_ascii_lowercase() } else { ledger.to_string() };
        prop_assert_eq!(fiat_code(&typed), Some(ledger));
        let fits = off_ledger_list([name.as_str()], &typed).is_none();
        prop_assert_eq!(fits, quote == ledger, "{} on {}", name, typed);
        prop_assert_eq!(
            off_ledger_list([name.as_str()], "USD"),
            foreign_fiat_quoted_list([name.as_str()])
        );
    }

    /// A crypto pair on a stablecoin fits the dollar ledger (the convention that kept it running) and
    /// NO other: a yen ledger must never swallow it as yen.
    #[test]
    fn a_crypto_pair_fits_only_the_dollar_ledger(
        base in prop::sample::select(CRYPTO_BASES),
        stable in prop::sample::select(STABLES),
        sep in separator(),
        ledger in fiat(),
    ) {
        let name = format!("{base}{sep}{stable}");
        let fits = off_ledger_list([name.as_str()], ledger).is_none();
        prop_assert_eq!(fits, ledger == "USD", "{} on {}", name, ledger);
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
