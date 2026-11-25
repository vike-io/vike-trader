//! The USER name ↔ broker name translation (decision 0290) over explicit kept rows ([`names_in`], [`user_name_in`]),
//! built as the catalog keeps a broker's listing (`crate::test_support::listed_rows`): no
//! process-wide list is read here, so "no list" is `&[]`.

use super::*;
use crate::test_support::listed_rows;

/// The translator's own words a refusal carries (vike-catalog's, which this crate does not word).
fn words(refusal: CtraderNameRefusal) -> String {
    let CtraderNameRefusal::Untranslated { why } = refusal;
    why
}

/// A broker that suffixes its pairs: its `EURUSDm` is the pair `EURUSD`, its `US500` the index CFD `US500.CFD`, its
/// `BTCUSD` the coin CFD `BTCUSD.CFD`, its `XAUUSD` the metal `XAUUSD`.
fn suffixed() -> Vec<Instrument> {
    listed_rows(&[
        (1, "EURUSDm", "Forex", "EUR", "USD"),
        (2, "US500", "Indices", "US500", "USD"),
        (3, "BTCUSD", "Cryptocurrencies", "BTC", "USD"),
        (4, "XAUUSD", "Metals", "XAU", "USD"),
    ])
}

/// Each user name reads its broker's name off its kept row, case kept, and the broker's name comes back as the user
/// name; a typed name is read without case and answered canonically, the broker's never.
#[test]
fn a_user_name_reads_the_brokers_name_off_its_kept_row_and_back() {
    let rows = suffixed();
    for (user, wire) in [
        ("EURUSD", "EURUSDm"),
        ("US500.CFD", "US500"),
        ("BTCUSD.CFD", "BTCUSD"),
        ("XAUUSD", "XAUUSD"),
    ] {
        let names = names_in(&rows, user).unwrap_or_else(|why| panic!("{user}: {why}"));
        assert_eq!((names.user.as_str(), names.wire.as_str()), (user, wire));
        assert_eq!(user_name_in(&rows, wire).as_deref(), Some(user), "{wire} comes back as {user}");
    }
    assert_eq!(names_in(&rows, "eurusd").expect("a typed name").user, "EURUSD");
    assert_eq!(user_name_in(&rows, "EURUSDM"), None, "the broker's names are case-significant");
}

/// No list, no row, or two rows answering one name is a refusal in the translator's words; with no list they name
/// the fix. The broker's own spellings (`EURUSDm`, `US500`, `BTCUSD`) and another venue's (`EUR/USD`) name no row.
#[test]
fn no_list_no_row_or_two_rows_is_refused_in_words() {
    let none = words(names_in(&[], "EURUSD").expect_err("no list"));
    assert!(none.contains(&vike_catalog::no_kept_list_refusal("ctrader")), "{none}");
    let rows = suffixed();
    for old in ["EURUSDm", "US500", "BTCUSD", "EUR/USD"] {
        let why = words(names_in(&rows, old).expect_err(old));
        assert!(why.contains(old) && why.contains("no user name"), "{old}: {why}");
    }
    let mut two = rows.clone();
    two.extend(listed_rows(&[(9, "EURUSD", "Forex", "EUR", "USD")]));
    let why = words(names_in(&two, "EURUSD").expect_err("two rows"));
    assert!(why.contains("names two ctrader instruments"), "{why}");
    assert_eq!(user_name_in(&[], "EURUSD"), None);
}

/// **CTR-2's read**: a name goes through its kept row to the broker's own case; a kept list without the name, and no
/// list at all, refuse (never a name read as the broker's).
#[test]
fn a_name_reads_through_its_row_to_the_brokers_case_and_no_row_refuses() {
    let rows = suffixed();
    let names = names_in(&rows, "eurusd").expect("its row");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD", "EURUSDm"));
    assert!(names_in(&rows, "GBPUSD").is_err(), "a kept list without it");
    let why = words(names_in(&[], "EURUSD").expect_err("no list"));
    assert!(why.contains(&vike_catalog::no_kept_list_refusal("ctrader")), "{why}");
}

/// A venue row no kept row names is booked under the broker's own name, every time (said once at WARN).
#[test]
fn a_venue_row_no_kept_row_names_is_booked_under_the_brokers_own_name() {
    assert_eq!(user_name_or_wire("NOTKEPT_9"), "NOTKEPT_9");
    assert_eq!(user_name_or_wire("NOTKEPT_9"), "NOTKEPT_9");
}
