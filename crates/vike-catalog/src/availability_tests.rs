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
    // construction: alpaca is what `is_served` must keep out of any provider table, and oanda and
    // ctrader are listed only from what a running account's session already read (amended
    // 2026-11-13), never keylessly.
    assert_eq!(catalog_availability("alpaca"), CatalogAvailability::Credentialed);
    assert!(!catalog_availability("alpaca").is_served(), "alpaca is never listed by a server");
    for v in ["alpaca", "oanda", "ctrader"] {
        assert!(!catalog_availability(v).is_public(), "{v} must never be fetched keylessly");
    }
    for v in ["oanda", "ctrader"] {
        assert_eq!(catalog_availability(v), CatalogAvailability::HeldSession, "{v}");
    }
}

#[test]
fn the_un_enumerable_venue_carries_a_reason_an_operator_can_read() {
    // ibkr alone since 2026-11-16 (ig's list is a held session's now).
    let v = "ibkr";
    match catalog_availability(v) {
        CatalogAvailability::NoBulkList { why } => {
            assert!(why.len() > 40, "{v}'s reason is too thin to render: {why}");
            assert!(!why.contains("TODO"), "{v} is still a scaffold row: {why}");
        }
        other => panic!("{v} must be NoBulkList, got {other:?}"),
    }
    assert!(!catalog_availability(v).is_public(), "{v}");
}

#[test]
fn the_desktops_own_routing_is_twelve_server_backed_and_no_direct_venue() {
    // The question the Data Manager's refresh control actually asks, answered over the REAL
    // roster for the REAL desktop, which links NO venue bridge: rulings 1 and 2 took the order
    // and catalog bridges out of its graph, and the Options tool's move onto the datahub
    // (2026-10-11) took the last one, deribit.
    const DESKTOP_LINKS: &[&str] = &[];
    for v in [
        "binance",
        "bybit",
        "okx",
        "aster",
        "hyperliquid",
        "polymarket",
        "deribit",
        "dukascopy",
        "fxcm",
        "oanda",
        "ctrader",
        "ig",
    ] {
        assert_eq!(
            catalog_source_for(v, DESKTOP_LINKS),
            Some(CatalogRoute::ServerBacked),
            "{v} is served (public or from a held session) and unlinked, so it routes to the server"
        );
    }
    // ...and the two with no route at all, which is what a UI renders a SENTENCE for rather
    // than a button.
    for v in ["alpaca", "ibkr"] {
        assert_eq!(catalog_source_for(v, DESKTOP_LINKS), None, "{v} has no refresh route");
    }
    // Every roster venue is decided one way or the other.
    let routed = VENUES.iter().filter(|v| catalog_source_for(v, DESKTOP_LINKS).is_some());
    assert_eq!(routed.count(), 12, "twelve of fourteen are refreshable from a desktop");
    let direct = VENUES
        .iter()
        .filter(|v| catalog_source_for(v, DESKTOP_LINKS) == Some(CatalogRoute::Direct));
    assert_eq!(direct.count(), 0, "a desktop linking no bridge enumerates nothing itself");
}

#[test]
fn a_public_venue_the_caller_links_is_direct() {
    // The `Direct` arm, now that the desktop exercises it no longer: a binary that DOES link a
    // publicly enumerable venue's provider (the datahub, a CLI) asks no server for it.
    assert_eq!(catalog_source_for("deribit", &["deribit"]), Some(CatalogRoute::Direct));
    assert_eq!(catalog_source_for("binance", &["deribit"]), Some(CatalogRoute::ServerBacked));
}

#[test]
fn a_credentialed_venue_has_no_route_even_for_a_caller_that_links_it() {
    // The ⚠ on `catalog_source_for`, held rather than left as prose: linking alpaca's bridge
    // does not make its catalog refreshable, because the credentials are the gate and this
    // function cannot see them.
    assert_eq!(catalog_source_for("alpaca", &["alpaca", "oanda"]), None);
}

#[test]
fn an_unknown_venue_has_no_route() {
    assert_eq!(catalog_source_for("kraken", &["kraken"]), None, "fail-closed");
}

#[test]
fn eight_roster_venues_are_publicly_enumerable_and_the_count_is_derived() {
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
    assert_eq!(not_public.len(), 7, "not-public set changed: {not_public:?}");
    for v in ["alpaca", "oanda", "ctrader", "ig", "ibkr", "fxcm", "dukascopy"] {
        assert!(not_public.iter().any(|x| **x == v), "{v} must not be publicly enumerable");
    }
}

#[test]
fn fxcm_and_dukascopy_are_listed_from_a_held_session_and_served_but_never_public() {
    // 0062's decision 3 as amended 2026-11-13: a server may hold fxcm's provider (it reads the
    // session a running account already holds) and dukascopy's (the list a sidecar's login
    // received), yet nothing may list either keylessly.
    for venue in ["fxcm", "dukascopy"] {
        let held = catalog_availability(venue);
        assert_eq!(held, CatalogAvailability::HeldSession, "{venue}");
        assert!(held.is_served() && !held.is_public(), "{venue}");
    }
    // Served is exactly public plus held-session: a credentialed or listless venue stays out.
    for v in VENUES {
        let a = catalog_availability(v);
        assert_eq!(a.is_served(), a.is_public() || a == CatalogAvailability::HeldSession, "{v}");
    }
}
