use vike_catalog::{CatalogAvailability, catalog_availability};
use vike_gatewayhub_client::catalog::CATALOG_VENUES;
use vike_gatewayhub_client::history::{FUNDING_VENUES, KLINE_VENUES, TICK_VENUE};
use vike_gatewayhub_client::resolve::FAMILY_VENUE;
use vike_venues::LiveVerb;

use super::*;

/// The table has one row per lane, in `Lane::ALL`'s order, and no lane twice.
#[test]
fn the_table_has_exactly_one_row_per_lane() {
    assert_eq!(ROSTERS.len(), Lane::ALL.len());
    for (row, lane) in ROSTERS.iter().zip(Lane::ALL) {
        assert_eq!(row.lane, lane, "ROSTERS and Lane::ALL are in different orders");
        assert!(!row.venues.is_empty(), "the {} lane serves no venue", lane.name());
    }
    for (i, lane) in Lane::ALL.iter().enumerate() {
        assert!(!Lane::ALL[..i].contains(lane), "{} is listed twice in Lane::ALL", lane.name());
    }
}

/// **The re-homed `a_client_venue_has_every_site_it_needs`.** Every venue a lane serves has, in every
/// table the lane is built from, the row it needs; and the client's rosters (what the datahub folds)
/// are this table's, IN ORDER. T5 and T6 add the arms and extend this test over them.
#[test]
fn a_registry_venue_has_every_site_it_needs() {
    for row in &ROSTERS {
        for (i, venue) in row.venues.iter().enumerate() {
            assert!(
                vike_model::VENUES.contains(venue),
                "{}: `{venue}` is not a vike_model::VENUES slug",
                row.lane.name()
            );
            assert!(
                !row.venues[..i].contains(venue),
                "{}: `{venue}` is listed twice",
                row.lane.name()
            );
        }
    }

    // The three ordered rosters the datahub folds, plus the single-venue lanes the client names.
    assert_eq!(
        roster(Lane::Klines),
        KLINE_VENUES,
        "the kline roster must equal the client's, in order"
    );
    assert_eq!(
        roster(Lane::Funding),
        FUNDING_VENUES,
        "the funding roster must equal the client's, in order"
    );
    assert_eq!(
        roster(Lane::Catalog),
        CATALOG_VENUES,
        "the catalog roster must equal the client's, in order"
    );
    assert_eq!(roster(Lane::Ticks), [TICK_VENUE]);
    assert_eq!(roster(Lane::Family), [FAMILY_VENUE]);

    // The client's rosters are each present: every venue the datahub can ask for is served.
    for v in KLINE_VENUES {
        assert!(
            lanes_of(v).contains(&Lane::Klines),
            "client kline venue `{v}` is not in the registry"
        );
    }
    for v in FUNDING_VENUES {
        assert!(
            lanes_of(v).contains(&Lane::Funding),
            "client funding venue `{v}` is not in the registry"
        );
    }
    for v in CATALOG_VENUES {
        assert!(
            lanes_of(v).contains(&Lane::Catalog),
            "client catalog venue `{v}` is not in the registry"
        );
    }

    // A stream venue is one the venue caps table declares at least one live verb for: a stream lane
    // for a venue with no live data would be an advertisement nothing could serve.
    for venue in roster(Lane::Stream) {
        let live = &vike_venues::caps_for(venue).live_data;
        let any =
            [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth]
                .into_iter()
                .any(|verb| live.supports(verb));
        assert!(
            any,
            "`{venue}` is a stream venue and declares no live verb in VenueCaps.live_data"
        );
    }
}

/// The catalog roster is the public bulk set without deribit (the one finding wave 1 leaves alone).
#[test]
fn the_catalog_roster_is_the_public_set_without_deribit() {
    for &venue in vike_model::VENUES {
        let public = catalog_availability(venue) == CatalogAvailability::PublicBulk;
        let listed = roster(Lane::Catalog).contains(&venue);
        if venue == "deribit" {
            assert!(public && !listed, "deribit is public and deliberately not served");
        } else {
            assert_eq!(
                public, listed,
                "`{venue}`: the catalog roster and the availability table disagree"
            );
        }
    }
}

/// A venue is keyless-streamed or credentialed-fetched, never both: the stream plane serves keyless
/// venues only, and the daemon never spends a credential on a client's stream.
#[test]
fn no_venue_is_both_streamed_and_credentialed() {
    for lane in [Lane::CredentialedKlines, Lane::CredentialedStream] {
        for v in roster(lane) {
            assert!(
                !roster(Lane::Stream).contains(v),
                "`{v}` is in both the stream and the {} rosters",
                lane.name()
            );
        }
    }
}

/// **The strategy lane's rosters stand on the lanes they extend.** A pump is a keyless stream venue's
/// (it opens on the same `DataOpen` session), a network is taken by a stream venue, and the credentialed
/// streams are the credentialed venues whose data the tradehub's feeds log in to.
#[test]
fn the_strategy_lane_rosters_stand_on_the_stream_rosters() {
    for v in roster(Lane::Pump) {
        assert!(roster(Lane::Stream).contains(v), "pump venue `{v}` is not a stream venue");
    }
    for v in roster(Lane::StreamNetwork) {
        assert!(roster(Lane::Stream).contains(v), "network venue `{v}` is not a stream venue");
    }
    assert_eq!(
        roster(Lane::StreamNetwork),
        ["hyperliquid"],
        "only hyperliquid's feed follows the tier"
    );
    assert!(
        roster(Lane::CredentialedKlines)
            .iter()
            .all(|v| roster(Lane::CredentialedStream).contains(v))
    );
}

#[test]
fn lanes_of_reads_the_table() {
    assert_eq!(
        lanes_of("binance"),
        vec![Lane::Stream, Lane::Klines, Lane::Funding, Lane::Catalog, Lane::Pump]
    );
    assert_eq!(lanes_of("oanda"), vec![Lane::CredentialedKlines, Lane::CredentialedStream]);
    assert_eq!(lanes_of("alpaca"), vec![Lane::CredentialedStream]);
    assert_eq!(lanes_of("aster").last(), Some(&Lane::Pump));
    assert!(lanes_of("no-such-venue").is_empty());
    assert_eq!(owned(Lane::Funding), vec!["binance".to_string(), "hyperliquid".to_string()]);
}
