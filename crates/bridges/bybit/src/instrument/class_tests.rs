//! The user-name translation (decision 0290) over the bridge's held kept list (`crate::history::kept_list_for_tests`,
//! rows keyed and typed as `crate::catalog` keeps them), and the class vocabulary over the venue's own `contractType`
//! words.

use super::*;
use crate::history::Category;

/// **A USER name signs its kept row's venue name in its kept row's book** (decision 0290's wire table for bybit), and
/// a venue row comes back under that user name: the perpetual and the spot pair of one venue name are two
/// instruments, never one name read two ways.
#[test]
fn a_user_name_is_its_kept_rows_venue_name_and_book_and_back() {
    crate::history::kept_list_for_tests();
    for (user, wire, book) in [
        ("BTCUSDT.P", "BTCUSDT", Category::Linear),
        ("BTCUSDT", "BTCUSDT", Category::Spot),
        ("BTCUSDC.P", "BTCPERP", Category::Linear),
        ("BTCUSD.P", "BTCUSD", Category::Inverse),
        ("BTCUSD", "BTCUSD", Category::Spot),
        ("ETHUSDT_261106", "ETHUSDT-06NOV26", Category::Linear),
        ("BTCUSD_261225", "BTCUSDZ26", Category::Inverse),
        ("BTCUSDC_261227", "BTC-27DEC26", Category::Linear),
    ] {
        assert_eq!(venue_symbol(user), Ok((wire.to_string(), book)), "{user}");
        assert_eq!(user_symbol(book, wire).as_deref(), Some(user), "{wire} in {}", book.wire());
        assert_eq!(user_symbol_or_wire(Some(book), wire), user, "{wire}");
    }
    assert_eq!(venue_symbol("btcusdt.p"), Ok(("BTCUSDT".into(), Category::Linear)), "any case");
}

/// **No row, no guess** (decision 0290, the owner's ruling of 2026-11-13): an old exec spelling (`BTCUSDT.S`, a dated
/// contract's venue name) and a name the list carries no row of are refused in words, never read for a book; a venue
/// row no kept row names keeps the venue's own name, and a row that states no category its venue name too.
#[test]
fn a_name_with_no_kept_row_is_refused_in_words() {
    crate::history::kept_list_for_tests();
    let rows = vike_catalog::kept_list(VENUE).expect("the test list is held");
    for name in ["BTCUSDT.S", "ETHUSDT-06NOV26", "BTCUSDZ26", "NOPEUSDT.P", ""] {
        let why = vike_catalog::kept_row_of_user_name(&rows, VENUE, name).expect_err(name);
        let refusal = crate::refusals::BybitSymbolRefusal::NoKeptRow { why };
        assert_eq!(venue_symbol(name), Err(refusal.to_string()), "{name}");
    }
    assert_eq!(user_symbol(Category::Linear, "NOPEUSDT"), None);
    assert_eq!(user_symbol_or_wire(Some(Category::Linear), "NOPEUSDT"), "NOPEUSDT");
    assert_eq!(user_symbol_or_wire(None, "BTCUSDT"), "BTCUSDT", "no category word");
    assert_eq!(user_symbol(Category::Spot, "BTCPERP"), None, "the wire name in another book");
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
/// `vike_catalog::names_spot_pair` take as a parameter) spells a spot pair as THIS bridge's kept rows name one: every
/// user name of the held list is spelled spot exactly when its row's book is `spot`, and a derivative otherwise.
#[test]
fn the_shared_spot_row_is_this_bridges_spelling() {
    crate::history::kept_list_for_tests();
    let row = vike_venues::spot_pair_row("bybit").expect("bybit states its spot spelling");
    assert_eq!(row.spelling, vike_venues::SpotPairSpelling::Unmarked);
    let kept = vike_catalog::kept_list("bybit").expect("the held list");
    for kept_row in kept.iter() {
        let user =
            vike_catalog::user_name_of_row(kept_row).expect("every held row has a user name");
        let spot = venue_symbol(&user).expect("a held row's name").1 == Category::Spot;
        assert_eq!(
            (row.spelling.names_spot(&user), row.spelling.names_derivative(&user)),
            (spot, !spot),
            "{user}"
        );
    }
}

/// **BYB-4: an inverse contract is its kept row's**: the inverse perpetual `BTCUSD.P` and dated `BTCUSD_261225`
/// settle in BTC, the SPOT pair `BTCUSD` of the same venue name is no inverse contract.
#[test]
fn the_catalogs_inverse_reading_follows_the_kept_row() {
    crate::history::kept_list_for_tests();
    for (user, coin) in
        [("BTCUSD.P", Some("BTC")), ("BTCUSD_261225", Some("BTC")), ("BTCUSD", None)]
    {
        assert_eq!(vike_catalog::is_inverse_contract("bybit", user), coin.is_some(), "{user}");
        assert_eq!(vike_catalog::inverse_settle_coin("bybit", user).as_deref(), coin, "{user}");
    }
}
