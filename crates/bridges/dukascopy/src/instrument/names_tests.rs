//! `crate::instrument::names`' tests: a user name to its kept row's wire name and back, over the rows this bridge's
//! catalog makes of the recorded-shape list (`crate::test_support::kept_list_for_tests`) and over rows an operator writes;
//! the refusals in words; and the ONE pair rule held to the Java (`OrderMap.canonicalSymbol`, audit flag DUK-7). Every
//! test here calls the pure functions over the rows it names, so none depends on the process's kept list.

use vike_model::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractFx, Instrument,
};

use super::*;

/// A currency pair as an operator writes its row (`vike-cli data instruments add`): its name as typed.
fn operator_pair(name: &str, base: &str, quote: &str) -> Instrument {
    let fx = ContractFx { pip_size: None, lot_size: None, swap_long: None, swap_short: None };
    Instrument::new(VENUE, name, base, quote, Contract::Fx(fx))
}

/// A share CFD as an operator writes its row: the venue's name, no base (the datahub's `stated_row`).
fn operator_share(name: &str, quote: &str) -> Instrument {
    let cfd = ContractCfd {
        underlying_kind: ContractCfdUnderlying::Share,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Rolling,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    };
    Instrument::new(VENUE, name, "", quote, Contract::Cfd(cfd))
}

/// **A user name is its kept row's wire name, and the wire name the sidecar answers in is that user name again**
/// (decision 0290's wire table for dukascopy), over the rows the sidecar's own list becomes.
#[test]
fn a_user_name_is_its_kept_rows_wire_name_and_back() {
    let rows = crate::test_support::kept_list_for_tests();
    for (typed, user, wire) in [
        ("EURUSD", "EURUSD", "EURUSD"),
        ("eurusd", "EURUSD", "EURUSD"),
        ("USDJPY", "USDJPY", "USDJPY"),
        ("XAUUSD", "XAUUSD", "XAUUSD"),
        ("AAPL.CFD", "AAPL.CFD", "AAPL.US/USD"),
        (" aapl.cfd ", "AAPL.CFD", "AAPL.US/USD"),
        ("BMW.CFD", "BMW.CFD", "BMW.DE/EUR"),
    ] {
        assert_eq!(
            wire_symbol_in(&rows, typed),
            Ok((wire.to_string(), user.to_string())),
            "{typed:?}"
        );
        assert_eq!(user_symbol_in(&rows, wire).as_deref(), Some(user), "{wire}");
    }
}

/// **DUK-7: the wire the sidecar answers in maps back to the row, whichever way the row spells it.** A pair an operator
/// wrote `EUR/USD` is sent and answered as `EURUSD` (the sidecar's `OrderMap.canonicalSymbol` drops the slash of two
/// three-letter codes), and a share CFD keeps its slashed name both ways.
#[test]
fn a_row_spelled_with_a_slash_is_sent_and_answered_in_the_sidecars_spelling() {
    let rows = vike_catalog::spelled(vec![
        operator_pair("EUR/USD", "EUR", "USD"),
        operator_share("AAPL.US/USD", "USD"),
    ]);
    assert_eq!(wire_symbol_in(&rows, "EURUSD"), Ok(("EURUSD".into(), "EURUSD".into())));
    assert_eq!(wire_symbol_in(&rows, "AAPL.CFD"), Ok(("AAPL.US/USD".into(), "AAPL.CFD".into())));
    assert_eq!(user_symbol_in(&rows, "EURUSD").as_deref(), Some("EURUSD"), "the fill's spelling");
    assert_eq!(user_symbol_in(&rows, "AAPL.US/USD").as_deref(), Some("AAPL.CFD"));
    assert_eq!(wire_of_row(&rows[0]), "EURUSD");
}

/// **No list, no row, or two rows: a refusal in words, never a guess** (decision 0290). No list names its fix; an old
/// spelling (the wire name typed as a user name, the SDK's enum-style name) names no row; two rows one user name
/// answers (an operator's `EUR/USD` beside the venue's `EURUSD`, audit flag DUK-8) name none, and the way back answers
/// nothing for a wire name two rows share.
#[test]
fn no_list_no_row_or_two_rows_is_a_refusal_in_words() {
    let why = wire_symbol_in(&[], "EURUSD").expect_err("no list");
    assert!(why.starts_with("dukascopy: "), "{why}");
    assert!(why.contains(&vike_catalog::no_kept_list_refusal(VENUE)), "the fix is named: {why}");
    assert_eq!(user_symbol_in(&[], "EURUSD"), None);

    let rows = crate::test_support::kept_list_for_tests();
    for old in ["AAPL.US/USD", "AAPLUSUSD", "EUR/USD", "GBPNOK", "COPPER.CMD/USD", ""] {
        let why = wire_symbol_in(&rows, old).expect_err(old);
        assert!(why.contains("carries no row"), "{old:?}: {why}");
    }
    assert_eq!(user_symbol_in(&rows, "NOPE.US/USD"), None, "a wire no row names");

    let two = vike_catalog::spelled(vec![
        operator_pair("EURUSD", "EUR", "USD"),
        operator_pair("EUR/USD", "EUR", "USD"),
    ]);
    assert!(wire_symbol_in(&two, "EURUSD").expect_err("two rows").contains("names two"));
    assert_eq!(user_symbol_in(&two, "EURUSD"), None, "one spelling of two rows answers nothing");
}

/// **The one pair rule** (`OrderMap.canonicalSymbol`): the slash of two three-letter upper-case codes is dropped, and
/// every other name is itself (a four-letter coin, a dotted CFD, a lower-case or a doubly slashed name).
#[test]
fn the_sidecar_spelling_drops_the_slash_of_two_three_letter_codes_only() {
    for (name, spelled) in [
        ("EUR/USD", "EURUSD"),
        ("XAU/USD", "XAUUSD"),
        ("BTC/USD", "BTCUSD"),
        ("EURUSD", "EURUSD"),
        ("AAPL.US/USD", "AAPL.US/USD"),
        ("COPPER.CMD/USD", "COPPER.CMD/USD"),
        ("DOGE/USD", "DOGE/USD"),
        ("eur/usd", "eur/usd"),
        ("EUR/USD/X", "EUR/USD/X"),
        ("EU1/USD", "EU1/USD"),
        ("", ""),
    ] {
        assert_eq!(sidecar_spelling(name), spelled, "{name:?}");
    }
}

/// **The Rust rule IS the Java rule** (audit flag DUK-7: one rule in Rust, the Java copy held to it). The Java is read
/// at run time (the public mirror ships no `jforex-bridge/` sources a test may `include_str!`) and its
/// `canonicalSymbol` must still be the one-line rule [`sidecar_spelling`] copies; and the line the sidecar writes for the
/// SDK's 180 predefined instruments (the recorded-shape fixture: `symbol` is `OrderMap.canonicalSymbol` of the SDK's
/// name `pair`, which `InstrumentSpellingTest.java` holds against the real SDK) agrees with the Rust rule row by row.
#[test]
fn the_rust_rule_is_the_java_rule() {
    for row in crate::test_support::listed() {
        assert_eq!(sidecar_spelling(&row.pair), row.symbol, "{}", row.pair);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("jforex-bridge/src/main/java/vike/jforex/OrderMap.java");
    let Ok(java) = std::fs::read_to_string(&path) else {
        eprintln!("SKIPPED: {} is not in this checkout", path.display());
        return;
    };
    let lines: Vec<&str> = java.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|line| *line == "static String canonicalSymbol(Instrument instrument) {")
        .unwrap_or_else(|| panic!("{} declares no canonicalSymbol", path.display()));
    assert_eq!(
        lines[at + 1..at + 4],
        [
            "String sdk = instrument.toString();",
            r#"return sdk.matches("[A-Z]{3}/[A-Z]{3}") ? sdk.replace("/", "") : sdk;"#,
            "}",
        ],
        "OrderMap.canonicalSymbol changed: change `sidecar_spelling` with it (one rule, two languages)"
    );
}
