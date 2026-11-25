use super::*;
use crate::test_support::test_row;
use crate::{CatalogCache, CatalogCacheVenueStamp};
use vike_model::AssetClass;

fn instrument(venue: &str, symbol: &str) -> Instrument {
    test_row(venue, symbol, symbol, "USD", AssetClass::Equity)
}

fn row(venue: &str, fetched: &str, qualifier: &str) -> InstrumentList {
    InstrumentList {
        venue: venue.into(),
        fetched: fetched.into(),
        qualifier: qualifier.into(),
        instruments: vec![instrument(venue, "AAPL")],
    }
}

fn own(rows: Vec<InstrumentList>) -> Result<InstrumentLists, InstrumentListError> {
    let lists = InstrumentLists { venues: rows };
    lists.check().map(|()| lists)
}

#[test]
fn a_well_formed_alpaca_row_is_admitted_and_addressable_by_venue() {
    let out = own(vec![row("alpaca", "2026-09-16", "demo")])
        .expect("a stamped, qualified alpaca row is the honest case");
    assert_eq!(out.venue("alpaca").expect("the row is addressable by venue").fetched, "2026-09-16");
}

/// Rule 1, held rather than left to an author: an undated list looks authoritative, so the
/// SET is refused and the venue reads as it does with no list.
#[test]
fn a_stampless_or_misdated_row_refuses_the_whole_set() {
    for bad in ["", "2026-9-16", "16-09-2026", "2026-09-16T00:00:00Z", "yesterday"] {
        let err = own(vec![row("alpaca", bad, "demo")]).expect_err("a bad stamp must refuse");
        assert_eq!(err, InstrumentListError::NoStamp { venue: "alpaca".into() }, "{bad}");
        assert!(err.to_string().contains("YYYY-MM-DD"), "the fix is named: {err}");
    }
}

/// The operator's own ctrader list is admitted: a list fetched with the operator's own credentials
/// against their own account at their own broker is exactly the symbols they can trade.
#[test]
fn the_operators_own_ctrader_list_is_admitted() {
    own(vec![row("ctrader", "2026-09-16", "demo")])
        .expect("the operator's OWN ctrader list is admitted");
}

/// Only a credentialed venue may carry an operator list: not one that already has a live route,
/// nor one that has none at all.
#[test]
fn only_a_credentialed_venue_is_eligible() {
    // Publicly enumerable: the datahub keeps its list, so an operator list is a second source.
    let public =
        own(vec![row("binance", "2026-09-16", "x")]).expect_err("a public venue must be refused");
    assert!(public.to_string().contains("publicly enumerable"), "{public}");
    // No bulk list at any price: there is nothing to fetch.
    let none = own(vec![row("ibkr", "2026-09-16", "x")])
        .expect_err("a no-bulk-list venue must be refused");
    assert!(none.to_string().contains("no bulk instrument list"), "{none}");
    // Off the roster: fail-closed.
    let bogus =
        own(vec![row("kraken", "2026-09-16", "x")]).expect_err("an unknown venue must be refused");
    assert!(bogus.to_string().contains("roster"), "{bogus}");
    // ...and oanda is ACCEPTED with a division declared.
    own(vec![row("oanda", "2026-09-16", "global-markets")])
        .expect("oanda is admitted with a division declared");
}

/// The qualifier is what makes the list honest, so a blank one refuses, and alpaca's is
/// enumerable because its axis is MEASURED, while oanda's deliberately is not.
#[test]
fn a_qualifier_is_required_and_alpacas_is_the_measured_one() {
    for blank in ["", "   "] {
        let err = own(vec![row("alpaca", "2026-09-16", blank)])
            .expect_err("a blank qualifier must refuse");
        assert!(err.to_string().contains("not common per venue"), "{err}");
    }
    let wrong = own(vec![row("alpaca", "2026-09-16", "sandbox")])
        .expect_err("alpaca's environment set is enumerable and closed");
    assert!(wrong.to_string().contains("ENVIRONMENT"), "{wrong}");
    for ok in ALPACA_ENVIRONMENTS {
        own(vec![row("alpaca", "2026-09-16", ok)]).expect(ok);
    }
    // oanda's is NON-BLANK only: this checkout cannot enumerate OANDA's divisions, and refusing a
    // correct list on the strength of a guess would be worse than not checking.
    own(vec![row("oanda", "2026-09-16", "whatever-division")])
        .expect("oanda's qualifier is validated as present, not as a member of an invented set");
}

#[test]
fn a_row_may_not_hold_another_venues_instruments_and_may_not_appear_twice() {
    let mut mixed = row("alpaca", "2026-09-16", "demo");
    mixed.instruments.push(instrument("oanda", "EUR_USD"));
    let err = own(vec![mixed]).expect_err("a mismatch must refuse");
    assert!(err.to_string().contains("tagged `oanda`"), "{err}");

    let twice = own(vec![row("alpaca", "2026-09-16", "demo"), row("alpaca", "2026-09-15", "live")])
        .expect_err("one venue has one list");
    assert!(twice.to_string().contains("appears twice"), "{twice}");
}

/// **Rule 3, and the direction that matters: a MEASURED EMPTY beats the operator's list.**
///
/// ⚠ Mutation proof: key the skip on `cache.instruments.iter().any(..)` instead of on the
/// stamp and the last assertion goes red, which is the whole bug: a venue refreshed to nothing
/// would silently get the other source's rows back.
#[test]
fn the_kept_list_wins_even_when_it_measured_nothing() {
    let local = InstrumentLists {
        venues: vec![row("alpaca", "2026-09-16", "demo"), row("oanda", "2026-09-16", "eu")],
    };

    // Nothing measured: the operator's lists fill both.
    let empty = CatalogCache::default();
    assert_eq!(merge_sources(&empty, &local).len(), 2);

    // alpaca measured with rows: the measured list stands, oanda still comes from the operator's,
    // and the measured instrument is the one that survives.
    let mut cache = CatalogCache {
        fetched: vec![CatalogCacheVenueStamp {
            venue: "alpaca".into(),
            last_refreshed_ms: 1,
            count: 1,
        }],
        instruments: vec![instrument("alpaca", "TSLA")],
    };
    let merged = merge_sources(&cache, &local);
    let alpaca: Vec<&str> =
        merged.iter().filter(|i| i.venue == "alpaca").map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(alpaca, ["TSLA"], "the operator's rows may not join a measured venue");
    assert_eq!(merged.iter().filter(|i| i.venue == "oanda").count(), 1);

    // ...and the direction the mutation proof names: a MEASURED ZERO keeps its zero.
    cache.instruments.clear();
    cache.fetched[0].count = 0;
    let merged = merge_sources(&cache, &local);
    assert_eq!(
        merged.iter().filter(|i| i.venue == "alpaca").count(),
        0,
        "a venue refreshed to nothing must not get the operator's rows back"
    );
}

#[test]
fn a_row_always_says_which_source_answered() {
    assert_eq!(catalog_answer(true, true), CatalogAnswer::OperatorFetch);
    assert_eq!(catalog_answer(true, false), CatalogAnswer::OperatorFetch);
    assert_eq!(catalog_answer(false, true), CatalogAnswer::LocalCredentialed);
    assert_eq!(catalog_answer(false, false), CatalogAnswer::Nothing);
}

/// **A credentialed fetch that came back with nothing must not delete the list it had**: the
/// `merge_refresh` safety rule, restated for the operator's own lists.
///
/// ⚠ Mutation proof: delete the `previous > 0` guard and the second assertion goes red, which
/// is the failure it names: a transient venue outage silently emptying the picker.
#[test]
fn a_local_upsert_never_replaces_a_list_with_nothing() {
    let mut doc = InstrumentLists::default();
    let full = || InstrumentList {
        venue: "alpaca".into(),
        fetched: "2026-09-16".into(),
        qualifier: "live".into(),
        instruments: vec![instrument("alpaca", "AAPL"), instrument("alpaca", "TSLA")],
    };
    let empty = || InstrumentList { instruments: vec![], ..full() };

    // An empty answer for a venue that had NOTHING is a measured zero and IS recorded.
    assert_eq!(
        upsert_local(&mut doc, empty()),
        CatalogLocalUpsert::Recorded { count: 0, previous: 0 }
    );
    assert_eq!(
        upsert_local(&mut doc, full()),
        CatalogLocalUpsert::Recorded { count: 2, previous: 0 }
    );
    // ...and now an empty answer keeps what was there, and says what it kept.
    assert_eq!(upsert_local(&mut doc, empty()), CatalogLocalUpsert::KeptExisting { kept: 2 });
    assert_eq!(doc.venue("alpaca").unwrap().instruments.len(), 2);
    // One venue, one row, however many times it is fetched.
    assert_eq!(doc.venues.len(), 1);
}
