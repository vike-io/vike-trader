//! Tests of `crate::instrument::names` over fxcm's kept list as the catalog makes it of the MEASURED Offers tables
//! (`kept_list_for_tests`, which this crate's unit tests hold): an order's USER name sends its row's WIRE name, a venue
//! name comes back under its user name, and no list, no row or an old spelling is refused in words (decision 0290).

use super::*;

/// **An order on a user name sends its kept row's wire name**: a pair `EURUSD` is `EUR/USD`, a metal `XAUUSD` is
/// `XAU/USD`, and a CFD keeps the row's case (`USOIL.CFD` is `USOil`, `NVDA.EXT.CFD` is `NVDA.ext`), however the asker
/// cased the user name.
#[test]
fn an_order_sends_its_kept_rows_wire_name() {
    for (user, wire) in [
        ("EURUSD", "EUR/USD"),
        ("eurusd", "EUR/USD"),
        ("XAUUSD", "XAU/USD"),
        ("USOIL.CFD", "USOil"),
        ("NVDA.EXT.CFD", "NVDA.ext"),
        ("BTCUSD.CFD", "BTC/USD"),
        ("US30.CFD", "US30"),
    ] {
        assert_eq!(wire_name(user).as_deref(), Ok(wire), "{user}");
    }
}

/// **A name no row answers is refused in words, nothing guessed**: the venue's own spelling, the old engine spelling of
/// a CFD (`USOIL`) and an unlisted pair each say the kept list carries no row they name.
#[test]
fn an_old_spelling_or_an_unlisted_name_is_refused_in_words() {
    for name in ["EUR/USD", "USOIL", "USOil", "NVDA.EXT", "ZZZQQQ"] {
        let why = wire_name(name).expect_err(name);
        assert!(
            why.starts_with("fxcm: ") && why.contains("carries no row it names"),
            "{name}: {why}"
        );
    }
}

/// **No kept list: an order is refused naming the fix** (`vike_catalog::no_kept_list_refusal`), the pure half asked
/// with no rows, as a process that holds no list asks it. The way back finds nothing either.
#[test]
fn no_kept_list_is_a_refusal_that_names_the_fix() {
    let why = names_among(&[], "EURUSD").expect_err("no list");
    assert!(why.contains(&vike_catalog::no_kept_list_refusal("fxcm")), "{why}");
    assert_eq!(user_name_among(&[], "EUR/USD"), None);
}

/// **A venue name comes back under its user name** (a fill, a position, a rollover): `EUR/USD` is `EURUSD`, the
/// mixed-case `USOil` is `USOIL.CFD`; a name no kept row names is booked under the venue's own spelling.
#[test]
fn a_venue_name_comes_back_under_its_user_name() {
    assert_eq!(user_name("EUR/USD").as_deref(), Some("EURUSD"));
    assert_eq!(user_name("USOil").as_deref(), Some("USOIL.CFD"));
    assert_eq!(user_name("XAU/USD").as_deref(), Some("XAUUSD"));
    assert_eq!(user_name("ZZZ/QQQ"), None);
    assert_eq!(user_name_or_wire("ZZZ/QQQ"), "ZZZ/QQQ");
    assert_eq!(user_name_or_wire("USOil"), "USOIL.CFD");
}

/// **An Offers row is named by the name stored on its kept row** (the fixture list, spelled once by its writer), the
/// read the facts, the price lane and a mount make off the table in hand; an Offers row no kept row carries has none.
#[test]
fn an_offers_row_is_named_by_the_catalogs_row() {
    let mut usoil = crate::test_support::offer("USOil", 70.0, 70.1);
    usoil.instrument_type = 3;
    usoil.contract_currency = "USD".to_string();
    assert_eq!(user_name_of_offer(&usoil).as_deref(), Some("USOIL.CFD"));
    let pair = crate::test_support::offer("EUR/USD", 1.1, 1.1002);
    assert_eq!(user_name_of_offer(&pair).as_deref(), Some("EURUSD"));
    let unlisted = crate::test_support::offer("ZZZ/QQQ", 1.0, 1.1);
    assert_eq!(
        user_name_of_offer(&unlisted),
        None,
        "an Offers row no kept row carries has no name"
    );
}

/// **A name is READ off the kept row's JSON, never spelled at a read** (the owner's ruling «Б», 2026-11-16): a row whose
/// stored user name differs from what the grammar would spell of it answers the STORED one, and a row stored with no
/// name (a list written before «Б») answers none, through the same lookup `user_name_of_offer` asks.
#[test]
fn an_offers_row_reads_the_name_stored_on_its_kept_row() {
    let offer = crate::test_support::offer("EUR/USD", 1.1, 1.1002);
    let mut row = crate::catalog::instrument_of(&offer).expect("the catalog's row");
    row.naming = Some(vike_model::InstrumentNaming::User("STORED.EURUSD".to_string()));
    let json = serde_json::to_string(&row).expect("a row is JSON");
    let stored: vike_model::Instrument = serde_json::from_str(&json).expect("and back");
    assert_eq!(
        user_name_among(std::slice::from_ref(&stored), "EUR/USD").as_deref(),
        Some("STORED.EURUSD")
    );
    let mut old = stored;
    old.naming = None;
    assert_eq!(user_name_among(&[old], "EUR/USD"), None, "a row stored before «Б» names nothing");
}
