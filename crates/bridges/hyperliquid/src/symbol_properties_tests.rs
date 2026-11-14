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
    SymbolPropertiesListSpecHyperliquid.row_answer(&row.raw_symbol, &row.raw_symbol, row, 0, &WORDS)
}

/// The two venue words are written by the hyperliquid catalog and read by the facts: an isolated-only coin is
/// ANSWERED with its margin mode (decision 0218), a delisted one is refused, and a plain perpetual states nothing.
#[test]
fn the_venue_words_are_the_ones_the_hyperliquid_catalog_writes() {
    let iso = answer(&hl_perp("RLB").with_venue_word(VENUE_WORD_ISOLATED_ONLY))
        .expect("an isolated-only coin is admitted");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    let old = answer(&hl_perp("OLD").with_venue_word(VENUE_WORD_DELISTED)).expect_err("delisted");
    assert_eq!(old, "hyperliquid: `OLD` is delisted; no order can open on it");
    assert_eq!(answer(&hl_perp("BTC")), Ok(SymbolPropertiesListAnswer::default()));
}
