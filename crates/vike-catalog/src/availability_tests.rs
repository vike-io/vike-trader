//! Tests for the catalog availability table and the client routing derived from it.

use super::*;
use vike_model::VENUES;

#[test]
fn every_roster_venue_is_classified() {
    // The completeness test the capability-map playbook requires: a new venue fails HERE until
    // its row exists, rather than silently answering `UnknownVenue` to a server that would then
    // report it as a typo.
    for v in VENUES {
        assert_ne!(
            catalog_availability(v),
            CatalogAvailability::UnknownVenue,
            "roster venue {v} has no row in `catalog_availability`"
        );
    }
}

#[test]
fn an_unknown_venue_fails_closed_and_is_not_a_venue_property() {
    // `UnknownVenue` must be its OWN answer: reporting a typo as `NoBulkList` would tell an
    // operator a false fact about a venue that does not exist.
    for bad in ["binanc", "BINANCE", "", "kraken"] {
        assert_eq!(catalog_availability(bad), CatalogAvailability::UnknownVenue, "{bad}");
    }
    assert!(!CatalogAvailability::UnknownVenue.is_public(), "fail-closed");
}

#[test]
fn a_credentialed_venue_is_never_public() {
    // 0062's decision 3, held as a property rather than trusted to the server's table
    // construction: these three are what `is_public` must keep out of any provider table.
    for v in ["alpaca", "oanda", "ctrader"] {
        assert_eq!(catalog_availability(v), CatalogAvailability::Credentialed, "{v}");
        assert!(!catalog_availability(v).is_public(), "{v} must never be fetched keylessly");
    }
}

#[test]
fn the_two_un_enumerable_venues_carry_a_reason_an_operator_can_read() {
    for v in ["ig", "ibkr"] {
        match catalog_availability(v) {
            CatalogAvailability::NoBulkList { why } => {
                assert!(why.len() > 40, "{v}'s reason is too thin to render: {why}");
                assert!(!why.contains("TODO"), "{v} is still a scaffold row: {why}");
            }
            other => panic!("{v} must be NoBulkList, got {other:?}"),
        }
        assert!(!catalog_availability(v).is_public(), "{v}");
    }
}

#[test]
fn the_desktops_own_routing_is_one_direct_venue_and_eight_server_backed() {
    // The question the Data Manager's refresh control actually asks, answered over the REAL
    // roster for the REAL desktop (which links `deribit` alone since rulings 1 and 2 took the
    // venue bridges out of its graph — `spawn_catalog_fetcher`'s tombstone).
    const DESKTOP_LINKS: &[&str] = &["deribit"];
    assert_eq!(
        catalog_source_for("deribit", DESKTOP_LINKS),
        Some(CatalogSource::Direct),
        "the one bridge the desktop links must not be routed through a server"
    );
    for v in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "dukascopy", "fxcm"]
    {
        assert_eq!(
            catalog_source_for(v, DESKTOP_LINKS),
            Some(CatalogSource::ServerBacked),
            "{v} is publicly enumerable and unlinked, so it routes to the server"
        );
    }
    // ...and the five with no route at all, which is what a UI renders a SENTENCE for rather
    // than a button.
    for v in ["alpaca", "oanda", "ctrader", "ig", "ibkr"] {
        assert_eq!(catalog_source_for(v, DESKTOP_LINKS), None, "{v} has no refresh route");
    }
    // Every roster venue is decided one way or the other.
    let routed = VENUES.iter().filter(|v| catalog_source_for(v, DESKTOP_LINKS).is_some());
    assert_eq!(routed.count(), 9, "nine of fourteen are refreshable from a desktop");
}

#[test]
fn a_credentialed_venue_has_no_route_even_for_a_caller_that_links_it() {
    // The ⚠ on `catalog_source_for`, held rather than left as prose: linking alpaca's bridge
    // does not make its catalog refreshable, because the credentials are the gate and this
    // function cannot see them.
    assert_eq!(catalog_source_for("alpaca", &["alpaca", "oanda"]), None);
    assert_eq!(catalog_source_for("oanda", &["alpaca", "oanda"]), None);
}

#[test]
fn an_unknown_venue_has_no_route() {
    assert_eq!(catalog_source_for("kraken", &["kraken"]), None, "fail-closed");
}

#[test]
fn nine_roster_venues_are_publicly_enumerable_and_the_count_is_derived() {
    // Not a hand-copied number: it is COUNTED off the table, so the assertion is that the
    // public set is exactly the complement of the five that are not — which is the property a
    // server's table is built from. The named list is what makes a silent reclassification
    // (say, a venue quietly becoming `PublicBulk`) fail here rather than widen a server's
    // venue egress unnoticed.
    let public: Vec<&&str> =
        VENUES.iter().filter(|v| catalog_availability(v).is_public()).collect();
    let not_public: Vec<&&str> =
        VENUES.iter().filter(|v| !catalog_availability(v).is_public()).collect();
    assert_eq!(public.len() + not_public.len(), VENUES.len());
    assert_eq!(not_public.len(), 5, "not-public set changed: {not_public:?}");
    for v in ["alpaca", "oanda", "ctrader", "ig", "ibkr"] {
        assert!(not_public.iter().any(|x| **x == v), "{v} must not be publicly enumerable");
    }
}
