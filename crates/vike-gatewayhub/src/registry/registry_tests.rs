use vike_venues::DataClientVerb;

use super::*;

/// The table has one row per lane, in `RosterLane::ALL`'s order, and no lane twice.
#[test]
fn the_table_has_exactly_one_row_per_lane() {
    assert_eq!(ROSTERS.len(), RosterLane::ALL.len());
    for (row, lane) in ROSTERS.iter().zip(RosterLane::ALL) {
        assert_eq!(row.lane, lane, "ROSTERS and Lane::ALL are in different orders");
        assert!(!row.venues.is_empty(), "the {} lane serves no venue", lane.name());
    }
    for (i, lane) in RosterLane::ALL.iter().enumerate() {
        assert!(
            !RosterLane::ALL[..i].contains(lane),
            "{} is listed twice in Lane::ALL",
            lane.name()
        );
    }
}

/// Every venue a lane serves has, in every table the lane is built from, the row it needs. (The
/// rosters the datahub folds need no equality check: those rows ARE the client's consts.)
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

    // A stream venue is one the venue caps table declares at least one live verb for: a stream lane
    // for a venue with no live data would be an advertisement nothing could serve.
    for venue in roster(RosterLane::Stream) {
        let live = &vike_venues::capabilities_for(venue).live_data;
        let any = [
            DataClientVerb::Bars,
            DataClientVerb::Quotes,
            DataClientVerb::Trades,
            DataClientVerb::Book,
            DataClientVerb::Depth,
        ]
        .into_iter()
        .any(|verb| live.supports(verb));
        assert!(
            any,
            "`{venue}` is a stream venue and declares no live verb in VenueCapabilities.live_data"
        );
    }
}

/// A venue is keyless-streamed or credentialed-fetched, never both: the stream plane serves keyless
/// venues only, and the daemon never spends a credential on a client's stream.
#[test]
fn no_venue_is_both_streamed_and_credentialed() {
    for lane in [RosterLane::CredentialedKlines, RosterLane::CredentialedStream] {
        for v in roster(lane) {
            assert!(
                !roster(RosterLane::Stream).contains(v),
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
    for v in roster(RosterLane::Pump) {
        assert!(roster(RosterLane::Stream).contains(v), "pump venue `{v}` is not a stream venue");
    }
    for v in roster(RosterLane::StreamNetwork) {
        assert!(
            roster(RosterLane::Stream).contains(v),
            "network venue `{v}` is not a stream venue"
        );
    }
    assert_eq!(
        roster(RosterLane::StreamNetwork),
        ["hyperliquid"],
        "only hyperliquid's feed follows the tier"
    );
    assert!(
        roster(RosterLane::CredentialedKlines)
            .iter()
            .all(|v| roster(RosterLane::CredentialedStream).contains(v))
    );
}

#[test]
fn lanes_of_reads_the_table() {
    assert_eq!(
        lanes_of("binance"),
        vec![
            RosterLane::Stream,
            RosterLane::Klines,
            RosterLane::Funding,
            RosterLane::Catalog,
            RosterLane::Pump
        ]
    );
    assert_eq!(
        lanes_of("oanda"),
        vec![RosterLane::Catalog, RosterLane::CredentialedKlines, RosterLane::CredentialedStream]
    );
    assert_eq!(
        lanes_of("ig"),
        vec![RosterLane::CredentialedKlines, RosterLane::CredentialedStream]
    );
    assert_eq!(
        lanes_of("ctrader"),
        vec![RosterLane::Catalog, RosterLane::CredentialedKlines, RosterLane::CredentialedStream]
    );
    assert_eq!(
        owned(RosterLane::CredentialedKlines),
        vec!["oanda".to_string(), "ig".to_string(), "ctrader".to_string()]
    );
    assert_eq!(lanes_of("alpaca"), vec![RosterLane::CredentialedStream]);
    assert_eq!(lanes_of("aster").last(), Some(&RosterLane::Pump));
    assert!(lanes_of("no-such-venue").is_empty());
    assert_eq!(
        owned(RosterLane::Funding),
        ["binance", "hyperliquid", "bybit", "okx", "deribit", "aster"].map(String::from).to_vec()
    );
    assert_eq!(
        lanes_of("deribit"),
        vec![RosterLane::Stream, RosterLane::Klines, RosterLane::Funding, RosterLane::Options],
        "deribit's funding lane sits beside its klines"
    );
}
