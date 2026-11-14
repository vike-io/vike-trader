//! The class vocabulary over names the venue really lists (the demo host's lists of 2026-10-30, whose symbol
//! sets are byte-identical to mainnet's) and over the venue's own `contractType` words.

use super::*;
use crate::history::Category;

#[test]
fn a_spot_pair_is_the_name_plus_the_spot_suffix_and_nothing_else() {
    assert_eq!(split_spot("DOGEUSDT.S"), ("DOGEUSDT", true));
    assert_eq!(split_spot("DOGEUSDT"), ("DOGEUSDT", false), "the bare name stays the perpetual");
    assert_eq!(split_spot("DOGEUSDT.P"), ("DOGEUSDT.P", false));
    assert_eq!(engine_symbol(Some("spot"), "DOGEUSDT"), "DOGEUSDT.S");
    for category in [Some("linear"), Some("inverse"), Some("option"), None] {
        assert_eq!(engine_symbol(category, "DOGEUSDT"), "DOGEUSDT", "{category:?}");
    }
    assert_eq!(engine_symbol(Some("spot"), ""), "", "no symbol, no suffix");
}

#[test]
fn the_venues_contract_type_names_the_derivative_book() {
    for (word, book) in [
        ("LinearPerpetual", Category::Linear),
        ("LinearFutures", Category::Linear),
        ("InversePerpetual", Category::Inverse),
        ("InverseFutures", Category::Inverse),
    ] {
        assert_eq!(category_of_contract_type(word), Some(book), "{word}");
    }
    assert_eq!(category_of_contract_type("SomethingNew"), None);
    assert_eq!(category_of_contract_type(""), None);
}

/// **A kept row's book**: its `wire.group` (the venue's word, as `crate::catalog` stamps it), and a row kept before
/// that stamp says it by its typed contract: a spot pair `spot`, a linear derivative `linear`, a coin-margined one
/// `inverse`. A group word of no book, and a row of neither: `None`.
#[test]
fn a_kept_rows_book_is_its_group_else_its_typed_contract() {
    use vike_model::{Contract, ContractPerp, Instrument, InstrumentVenueIds, Margining};
    let perp = |margining| {
        Contract::Perp(ContractPerp {
            margining,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    };
    let grouped = |contract, group: &str| {
        Instrument::new("bybit", "X", "X", "USDT", contract).with_wire(InstrumentVenueIds {
            group: Some(group.into()),
            ..InstrumentVenueIds::default()
        })
    };
    for (row, book) in [
        (grouped(Contract::CryptoSpot, "spot"), Some(Category::Spot)),
        (grouped(perp(Margining::Linear), "linear"), Some(Category::Linear)),
        (grouped(perp(Margining::Inverse), "inverse"), Some(Category::Inverse)),
        (grouped(perp(Margining::Linear), "option"), None),
        (Instrument::new("bybit", "X", "X", "USDT", Contract::CryptoSpot), Some(Category::Spot)),
        (
            Instrument::new("bybit", "X", "X", "USDT", perp(Margining::Inverse)),
            Some(Category::Inverse),
        ),
        (Instrument::new("bybit", "X", "X", "USDT", Contract::Index), None),
    ] {
        assert_eq!(Category::of_row(&row), book, "{row:?}");
    }
}

/// The shared spot row (`vike_venues::spot_pair_row`: what the fee lane, the wallet-cash reading and
/// `vike_catalog::names_spot_pair` take as a parameter) is THIS bridge's spelling, read by its own `split_spot`.
#[test]
fn the_shared_spot_row_is_this_bridges_spelling() {
    let row = vike_venues::spot_pair_row("bybit").expect("bybit states its spot spelling");
    assert_eq!(row.spelling, vike_venues::SpotPairSpelling::Suffix(SPOT_SUFFIX));
    for name in
        ["DOGEUSDT.S", "DOGEUSDT", "DOGEUSDT.P", "ETHUSDT-27NOV26", "BTCUSD", "BTCUSDZ26", ""]
    {
        let spot = split_spot(name).1;
        assert_eq!(
            (row.spelling.names_spot(name), row.spelling.names_derivative(name)),
            (spot, !spot),
            "{name}"
        );
    }
}
