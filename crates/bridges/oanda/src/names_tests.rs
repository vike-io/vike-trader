//! The user name ↔ wire name translation (decision 0290) over the tests' kept list
//! (`crate::test_support::kept_list_for_tests`: the practice account's captured list and the docs-shaped CFDs, parsed
//! by `crate::catalog::parse_instruments`), and the refusals with no list at all over the pure core (`&[]`), which no
//! process-wide list can reach.

use super::*;

/// **A user name is its kept row's wire name, and back** (decision 0290's oanda row): a pair, a metal and an index CFD.
/// The way back answers the canonical user name whatever case was typed.
#[test]
fn a_user_name_is_its_kept_rows_wire_name_and_back() {
    crate::test_support::kept_list_for_tests();
    for (user, wire) in [
        ("EURUSD", "EUR_USD"),
        ("USDJPY", "USD_JPY"),
        ("XAUUSD", "XAU_USD"),
        ("SPX500.CFD", "SPX500_USD"),
        ("BCO.CFD", "BCO_USD"),
    ] {
        assert_eq!(wire_name(user).as_deref(), Ok(wire), "{user}");
        assert_eq!(user_name(wire).as_deref(), Some(user), "{wire}");
        assert_eq!(user_name_or_wire(wire), user, "{wire}");
    }
    assert_eq!(wire_name("eurusd").as_deref(), Ok("EUR_USD"), "any case");
}

/// **A CFD round trip through the order path and the fill path**: an order on `SPX500.CFD` is written for the wire
/// `SPX500_USD`, and an unrecorded fill of `SPX500_USD` (a hand trade) books as `SPX500.CFD` again.
#[test]
fn a_cfd_order_goes_out_on_its_wire_name_and_its_fill_comes_back_under_its_user_name() {
    crate::test_support::kept_list_for_tests();
    let order = vike_model::OrderRequest {
        order_client_id: "c-cfd".into(),
        venue: VENUE.into(),
        symbol: "SPX500.CFD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    assert_eq!(crate::execution::build_order_body(&order)["order"]["instrument"], "SPX500_USD");
    let fill = serde_json::json!({"type": "ORDER_FILL", "id": "77", "time": "1.0",
        "instrument": "SPX500_USD", "units": "1", "price": "7640.8"});
    let events = crate::decode_transaction_events(&fill, &Default::default());
    let Some(vike_model::events::Event::Fill(fill)) = events.first() else {
        panic!("a bare Fill first: {events:?}")
    };
    assert_eq!(fill.symbol, "SPX500.CFD");
}

/// **No row, no guess** (the owner's ruling of 2026-11-13): the old wire spelling and a name the list carries no row
/// of are refused in the translator's words, and a venue instrument no kept row names keeps the venue's own name.
#[test]
fn a_name_with_no_kept_row_is_refused_in_words() {
    crate::test_support::kept_list_for_tests();
    for name in ["EUR_USD", "SPX500_USD", "SPX500USD", "NOPEUSD", ""] {
        let why = wire_name(name).expect_err(name);
        assert!(why.contains("oanda") && why.contains("kept list"), "{name:?}: {why}");
    }
    assert_eq!(user_name("NOPE_USD"), None);
    assert_eq!(user_name_or_wire("NOPE_USD"), "NOPE_USD");
}

/// **No kept list at all** (a held-session venue before its first account was listed): every name is refused with
/// the fix in the operator's words (`vike_catalog::no_kept_list_refusal`), and no venue instrument has a user name.
#[test]
fn with_no_kept_list_a_name_is_refused_with_the_fix() {
    let why = wire_name_in(&[], "EURUSD").expect_err("no list");
    assert!(why.contains(&vike_catalog::no_kept_list_refusal(VENUE)), "{why}");
    assert!(why.contains("vike-cli data catalog refresh --venue oanda"), "{why}");
    assert_eq!(user_name_in(&[], "EUR_USD"), None);
}

/// The pure core reads only oanda rows: another venue's row of the same name is no answer.
#[test]
fn another_venues_row_names_nothing_here() {
    let rows = crate::test_support::kept_rows_for_tests();
    let mut foreign = rows.iter().find(|r| r.raw_symbol == "EUR_USD").expect("EUR_USD").clone();
    foreign.venue = "fxcm".into();
    assert!(wire_name_in(std::slice::from_ref(&foreign), "EURUSD").is_err());
    assert_eq!(user_name_in(std::slice::from_ref(&foreign), "EUR_USD"), None);
    assert_eq!(wire_name_in(&rows, "EURUSD").as_deref(), Ok("EUR_USD"));
}
