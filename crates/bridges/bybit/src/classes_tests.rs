//! The class vocabulary over names the venue really lists (the demo host's lists of 2026-10-30, whose symbol
//! sets are byte-identical to mainnet's) and over the venue's own `contractType` words.

use super::*;
use crate::data::Category;

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

/// Every name shape measured on the demo host, one assertion each: a name this reads wrongly is a venue
/// refusal at worst, but a name it cannot read must be `None` (the caller then asks the venue).
#[test]
fn a_name_alone_says_the_book_for_the_shapes_the_venue_lists() {
    use Category::{Inverse, Linear, Spot};
    for (name, book) in [
        ("BTCUSDT", Some(Linear)),
        ("1000PEPEUSDT", Some(Linear)),
        ("BTCPERP", Some(Linear)),
        ("ETHUSDC", Some(Linear)),
        ("ETHUSDT-06NOV26", Some(Linear)),
        ("DOGEUSDT.S", Some(Spot)),
        ("BTCUSD", Some(Inverse)),
        ("ADAUSD", Some(Inverse)),
        ("BTCUSDZ26", Some(Inverse)),
        ("ETHUSDH27", Some(Inverse)),
        ("BTC-31OCT26-75500-C-USDT", None),
        ("", None),
        (".S", None),
        ("XYZ", None),
    ] {
        assert_eq!(category_from_name(name), book, "{name}");
    }
}
