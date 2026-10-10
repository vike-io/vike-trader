use super::*;
use crate::{CatalogCache, VenueStamp};
use std::assert_matches;
use vike_model::AssetClass;

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument::test_row(venue, sym, sym, "USD", AssetClass::Equity)
}

fn row(venue: &str, fetched: &str, qualifier: &str) -> BaselineVenue {
    BaselineVenue {
        venue: venue.into(),
        fetched: fetched.into(),
        qualifier: qualifier.into(),
        instruments: vec![inst(venue, "AAPL")],
    }
}

fn doc(rows: Vec<BaselineVenue>) -> Vec<u8> {
    serde_json::to_vec(&BaselineCatalog { note: String::new(), venues: rows }).unwrap()
}

#[test]
fn a_well_formed_alpaca_row_parses_and_carries_its_date() {
    let out = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", "demo")]))
        .expect("a stamped, qualified alpaca row is the honest case");
    let v = out.venue("alpaca").expect("the row is addressable by venue");
    assert_eq!(v.label(), "shipped 2026-09-16");
    // ⚠ It must not read like an AGE — that is rule 3's whole point.
    assert!(!v.label().contains("ago"), "{}", v.label());
}

/// Rule 1, held rather than left to an author: an undated list looks authoritative, so the
/// ARTIFACT is refused and the venue falls back to reading exactly as it does today.
#[test]
fn a_stampless_or_misdated_row_refuses_the_whole_artifact() {
    for bad in ["", "2026-9-16", "16-09-2026", "2026-09-16T00:00:00Z", "yesterday"] {
        let err = BaselineCatalog::parse(&doc(vec![row("alpaca", bad, "demo")]))
            .expect_err("a bad stamp must refuse");
        assert_eq!(err, BaselineError::NoStamp { venue: "alpaca".into() }, "{bad}");
        assert!(err.to_string().contains("YYYY-MM-DD"), "the fix is named: {err}");
    }
}

/// Decision 6, held structurally: ctrader ships nothing, and the refusal names the mechanism
/// rather than saying "not allowed".
///
/// ⚠ …and the OPERATOR's own ctrader fetch IS admitted, which is the same decision read
/// correctly rather than an exception to it: what that decision refuses is a SHARED list across
/// brokers whose symbol ids differ. A list fetched with the operator's own credentials against
/// their own account at their own broker is exactly the symbols they can trade — and it is the
/// only honest source ctrader could ever have.
#[test]
fn ctrader_may_never_ship_a_baseline_but_the_operator_may_fetch_their_own() {
    let err = BaselineCatalog::parse(&doc(vec![row("ctrader", "2026-09-16", "icmarkets")]))
        .expect_err("ctrader must be refused in a SHIPPED artifact");
    let msg = err.to_string();
    assert!(msg.contains("differ across brokers"), "{msg}");
    assert!(msg.contains("DIFFERENT instrument"), "the hazard is named: {msg}");

    BaselineCatalog::parse_local(&doc(vec![row("ctrader", "2026-09-16", "demo")]))
        .expect("the operator's OWN ctrader list is admitted");
}

/// …and neither may a venue that already has a live route, nor one that has none at all.
#[test]
fn only_a_credentialed_venue_is_eligible() {
    // Publicly enumerable — a live route exists, so a shipped list is a second source.
    let public = BaselineCatalog::parse(&doc(vec![row("binance", "2026-09-16", "x")]))
        .expect_err("a public venue must be refused");
    assert!(public.to_string().contains("publicly enumerable"), "{public}");
    // No bulk list at any price — there was nothing to fetch out of band either.
    let none = BaselineCatalog::parse(&doc(vec![row("ig", "2026-09-16", "x")]))
        .expect_err("a no-bulk-list venue must be refused");
    assert!(none.to_string().contains("no bulk instrument list"), "{none}");
    // Off the roster — fail-closed.
    let bogus = BaselineCatalog::parse(&doc(vec![row("kraken", "2026-09-16", "x")]))
        .expect_err("an unknown venue must be refused");
    assert!(bogus.to_string().contains("roster"), "{bogus}");
    // …and oanda, the second eligible venue, is ACCEPTED with a division declared.
    BaselineCatalog::parse(&doc(vec![row("oanda", "2026-09-16", "global-markets")]))
        .expect("oanda ships with a division declared");
}

/// The qualifier is what makes the list honest, so a blank one refuses — and alpaca's is
/// enumerable because its axis is MEASURED, while oanda's deliberately is not.
#[test]
fn a_qualifier_is_required_and_alpacas_is_the_measured_one() {
    for blank in ["", "   "] {
        let err = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", blank)]))
            .expect_err("a blank qualifier must refuse");
        assert!(err.to_string().contains("not common per venue"), "{err}");
    }
    let wrong = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", "sandbox")]))
        .expect_err("alpaca's environment set is enumerable and closed");
    assert!(wrong.to_string().contains("ENVIRONMENT"), "{wrong}");
    for ok in ALPACA_ENVIRONMENTS {
        BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", ok)])).expect(ok);
    }
    // oanda's is NON-BLANK only — this checkout cannot enumerate OANDA's divisions, and
    // refusing a correct artifact on the strength of a guess would be worse than not checking.
    BaselineCatalog::parse(&doc(vec![row("oanda", "2026-09-16", "whatever-division")]))
        .expect("oanda's qualifier is validated as present, not as a member of an invented set");
}

#[test]
fn a_row_may_not_hold_another_venues_instruments_and_may_not_appear_twice() {
    let mut mixed = row("alpaca", "2026-09-16", "demo");
    mixed.instruments.push(inst("oanda", "EUR_USD"));
    let err = BaselineCatalog::parse(&doc(vec![mixed])).expect_err("a mismatch must refuse");
    assert!(err.to_string().contains("tagged `oanda`"), "{err}");

    let twice = BaselineCatalog::parse(&doc(vec![
        row("alpaca", "2026-09-16", "demo"),
        row("alpaca", "2026-09-15", "live"),
    ]))
    .expect_err("one venue has one shipped list");
    assert!(twice.to_string().contains("appears twice"), "{twice}");
}

/// **Rule 3, and the direction that matters: a MEASURED EMPTY beats a shipped list.**
///
/// ⚠ Mutation proof: key the skip on `cache.instruments.iter().any(..)` instead of on the
/// stamp and the last assertion goes red — which is the whole bug, because a venue the
/// operator refreshed to nothing would silently get shipped rows back.
#[test]
fn the_operators_own_fetch_wins_even_when_it_measured_nothing() {
    let shipped = BaselineCatalog {
        note: String::new(),
        venues: vec![row("alpaca", "2026-09-16", "demo"), row("oanda", "2026-09-16", "eu")],
    };
    let none = BaselineCatalog::default();

    // Nothing measured: the shipped list fills both.
    let empty = CatalogCache::default();
    assert_eq!(merge_sources(&empty, &none, &shipped).len(), 2);

    // alpaca measured with rows: the operator's list stands, oanda still comes from the shipped
    // one, and the operator's own instrument is the one that survives.
    let mut cache = CatalogCache {
        fetched: vec![VenueStamp { venue: "alpaca".into(), last_refreshed_ms: 1, count: 1 }],
        instruments: vec![inst("alpaca", "TSLA")],
    };
    let merged = merge_sources(&cache, &none, &shipped);
    let alpaca: Vec<&str> =
        merged.iter().filter(|i| i.venue == "alpaca").map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(alpaca, ["TSLA"], "shipped data may not join a measured venue");
    assert_eq!(merged.iter().filter(|i| i.venue == "oanda").count(), 1);

    // …and the direction the mutation proof names: a MEASURED ZERO keeps its zero.
    cache.instruments.clear();
    cache.fetched[0].count = 0;
    let merged = merge_sources(&cache, &none, &shipped);
    assert_eq!(
        merged.iter().filter(|i| i.venue == "alpaca").count(),
        0,
        "a venue the operator refreshed to nothing must not get shipped rows back"
    );
}

/// **The operator's OWN CREDENTIALED fetch beats the shipped list** — their keys, their
/// account, their qualifier.
#[test]
fn a_local_credentialed_fetch_outranks_the_shipped_one() {
    let shipped = BaselineCatalog {
        note: String::new(),
        venues: vec![BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-01-01".into(),
            qualifier: "demo".into(),
            instruments: vec![inst("alpaca", "SHIPPED")],
        }],
    };
    let local = BaselineCatalog {
        note: String::new(),
        venues: vec![BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "live".into(),
            instruments: vec![inst("alpaca", "MINE")],
        }],
    };
    let merged = merge_sources(&CatalogCache::default(), &local, &shipped);
    let symbols: Vec<&str> = merged.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(symbols, ["MINE"], "shipped rows must not join a locally fetched venue");

    // …and a local fetch that measured NOTHING still wins, for `merge_sources`' own reason.
    let empty_local = BaselineCatalog {
        note: String::new(),
        venues: vec![BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "live".into(),
            instruments: vec![],
        }],
    };
    assert!(
        merge_sources(&CatalogCache::default(), &empty_local, &shipped).is_empty(),
        "a measured zero is a measured claim, whoever measured it"
    );
}

#[test]
fn a_row_always_says_which_source_answered() {
    assert_eq!(catalog_answer(true, true, true), CatalogAnswer::OperatorFetch);
    assert_eq!(catalog_answer(true, false, false), CatalogAnswer::OperatorFetch);
    assert_eq!(catalog_answer(false, true, true), CatalogAnswer::LocalCredentialed);
    assert_eq!(catalog_answer(false, false, true), CatalogAnswer::ShippedBaseline);
    assert_eq!(catalog_answer(false, false, false), CatalogAnswer::Nothing);
}

/// **A credentialed fetch that came back with nothing must not delete the list it had** — the
/// `merge_refresh` safety rule, restated for the file that has a second writer.
///
/// ⚠ Mutation proof: delete the `previous > 0` guard and the second assertion goes red, which
/// is the failure it names — a transient venue outage silently emptying the picker.
#[test]
fn a_local_upsert_never_replaces_a_list_with_nothing() {
    let mut doc = BaselineCatalog::default();
    let full = || BaselineVenue {
        venue: "alpaca".into(),
        fetched: "2026-09-16".into(),
        qualifier: "live".into(),
        instruments: vec![inst("alpaca", "AAPL"), inst("alpaca", "TSLA")],
    };
    let empty = || BaselineVenue { instruments: vec![], ..full() };

    // An empty answer for a venue that had NOTHING is a measured zero and IS recorded.
    assert_eq!(upsert_local(&mut doc, empty()), LocalUpsert::Recorded { count: 0, previous: 0 });
    assert_eq!(upsert_local(&mut doc, full()), LocalUpsert::Recorded { count: 2, previous: 0 });
    // …and now an empty answer keeps what was there, and says what it kept.
    assert_eq!(upsert_local(&mut doc, empty()), LocalUpsert::KeptExisting { kept: 2 });
    assert_eq!(doc.venue("alpaca").unwrap().instruments.len(), 2);
    // One venue, one row, however many times it is fetched.
    assert_eq!(doc.venues.len(), 1);
}

#[test]
fn malformed_bytes_are_refused_by_name_rather_than_read_as_an_empty_baseline() {
    let err = BaselineCatalog::parse(b"not json at all").expect_err("must refuse");
    assert_matches!(err, BaselineError::Malformed(_), "{err:?}");
    // An EMPTY artifact is legitimate and distinct: it is a document with no venue rows.
    let empty = BaselineCatalog::parse(br#"{"venues":[]}"#).expect("an empty artifact parses");
    assert!(empty.venues.is_empty());
}
