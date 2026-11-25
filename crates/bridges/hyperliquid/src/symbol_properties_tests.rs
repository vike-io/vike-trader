//! hyperliquid's list spec on rows carrying the venue words this crate's catalog writes (moved here with
//! `hyperliquid_class` from the gatewayhub's `symbol_facts_tests.rs`, where the cache-driven tests stay).

use vike_model::{Contract, ContractPerp, Instrument, MarginMode, Margining};

use super::*;

const WORDS: SymbolPropertiesWords = SymbolPropertiesWords {
    not_answered: "not answered",
    no_instrument: ": none ",
    expired: "gone",
};

fn hl_perp(coin: &str) -> Instrument {
    Instrument::new(
        "hyperliquid",
        coin,
        coin,
        "USDC",
        Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_currency: None,
            funding_interval_ms: Some(3_600_000),
            settling_ms: None,
        }),
    )
}

fn answer(row: &Instrument) -> Result<SymbolPropertiesListAnswer, String> {
    SymbolPropertiesListSpecHyperliquid
        .row_answer(&row.raw_symbol, &row.raw_symbol, row, 0, &WORDS)
        .map_err(|e| e.to_string())
}

/// The two venue words are written by the hyperliquid catalog and read by the facts: an isolated-only coin is
/// ANSWERED with its margin mode (decision 0218), a delisted one is refused, and a plain perpetual states nothing.
#[test]
fn the_venue_words_are_the_ones_the_hyperliquid_catalog_writes() {
    let iso = answer(&hl_perp("RLB").with_venue_word(VENUE_WORD_ISOLATED_ONLY))
        .expect("an isolated-only coin is admitted");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    let old = hl_perp("OLD").with_venue_word(VENUE_WORD_DELISTED);
    assert_eq!(
        hyperliquid_class(&old, "OLD", &WORDS),
        Err(HyperliquidPropertiesRefusal::Delisted { symbol: "OLD".to_string() })
    );
    assert_eq!(answer(&hl_perp("BTC")), Ok(SymbolPropertiesListAnswer::default()));
}

/// **A USER name finds its catalog row by its kept row's listing name** (decision 0290): `BTCUSDC.P` is the row `BTC`,
/// `KPEPEUSDC.P` the case-kept `kPEPE`, `HYPEUSDC` `HYPE/USDC`, a builder-dex `XYZ:TSLAUSDC.P` `xyz:TSLA` (read one at
/// a time); an old spelling (`BTC`, `HYPE/USDC`) and the wire name `@107` are refused before any read.
#[test]
fn a_user_name_keys_its_kept_rows_listing_name_and_an_old_spelling_is_refused() {
    crate::test_support::hold_kept_rows();
    let spec = SymbolPropertiesListSpecHyperliquid;
    for (user, key) in [
        ("BTCUSDC.P", "BTC"),
        ("KPEPEUSDC.P", "kPEPE"),
        ("HYPEUSDC", "HYPE/USDC"),
        ("PURRUSDC", "PURR/USDC"),
        ("XYZ:TSLAUSDC.P", "xyz:TSLA"),
    ] {
        assert_eq!(spec.symbol_refusal(user, &WORDS), None, "{user}");
        assert_eq!(spec.key(user), key, "{user}");
        assert_eq!(spec.one_at_a_time(key), user.contains(':'), "{user}");
    }
    assert!(!spec.case_kept("kpepeusdc.p"), "a user name is upper case");
    for old in ["BTC", "kPEPE", "HYPE/USDC", "xyz:TSLA"] {
        let why = name_refusal(old, &WORDS).expect(old);
        assert!(matches!(why, HyperliquidPropertiesRefusal::NoKeptRow { .. }), "{old}: {why}");
        assert_eq!(spec.symbol_refusal(old, &WORDS), Some(why.carried()), "{old}");
    }
    assert_eq!(
        name_refusal("@107", &WORDS),
        Some(HyperliquidPropertiesRefusal::WireName {
            not_answered: WORDS.not_answered,
            symbol: "@107".to_string(),
        })
    );
}
