use super::*;
use vike_model::StoreKindId;

/// A venue node holding `series` series and no symbol detail — enough for the scope folds,
/// which read `total` and the gap map and reach into `symbols` only for the stale count.
fn node(venue: &str, series: usize) -> DataManagerInventoryVenue {
    DataManagerInventoryVenue {
        venue: venue.to_string(),
        symbols: Vec::new(),
        total: vike_data_manager::DataManagerInventoryRollup { series, ..Default::default() },
    }
}

fn gap_on(venue: &str) -> vike_data_manager::DataManagerInventoryGapMap {
    let mut gaps = vike_data_manager::DataManagerInventoryGapMap::new();
    gaps.insert(
        vike_data_manager::DataManagerInventorySeriesKey {
            venue: venue.to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: StoreKindId::Trade,
            interval: None,
            grouped: false,
            source: None,
        },
        vec![(1_000, 2_000)],
    );
    gaps
}

/// The default scope lists what the store holds, and nothing else.
#[test]
fn holds_series_drops_a_venue_whose_rollup_is_empty() {
    let tree = vec![node("binance", 4), node("okx", 0)];
    let rows = venue_rows(
        &tree,
        &vike_data_manager::DataManagerInventoryGapMap::new(),
        (0, 0),
        DataManagerVenueRollupScope::HoldsSeries,
    );
    assert_eq!(rows.iter().map(|r| r.venue).collect::<Vec<_>>(), vec!["binance"]);
}

/// The roster scope is the whole point of the control: a venue the tree built no node for gets
/// a row anyway, marked as holding nothing.
#[test]
fn the_roster_scope_gives_an_unstored_venue_a_row_of_its_own() {
    let tree = vec![node("binance", 4)];
    let rows = venue_rows(
        &tree,
        &vike_data_manager::DataManagerInventoryGapMap::new(),
        (0, 0),
        DataManagerVenueRollupScope::Roster,
    );
    assert_eq!(rows.len(), vike_model::VENUES.len());
    for venue in vike_model::VENUES.iter().copied() {
        let row = rows.iter().find(|r| r.venue == venue).expect("every roster venue has a row");
        assert_eq!(row.stored(), venue == "binance", "{venue} storedness");
    }
}

/// ...and it never doubles a venue that IS in the tree — the roster fill is a set union, not an
/// append. A duplicate row would read as two stores holding the same venue.
#[test]
fn the_roster_scope_does_not_double_a_venue_the_tree_already_holds() {
    let tree = vec![node("binance", 4), node("okx", 1)];
    let rows = venue_rows(
        &tree,
        &vike_data_manager::DataManagerInventoryGapMap::new(),
        (0, 0),
        DataManagerVenueRollupScope::Roster,
    );
    assert_eq!(rows.iter().filter(|r| r.venue == "binance").count(), 1);
    assert_eq!(rows.iter().filter(|r| r.venue == "okx").count(), 1);
}

/// A venue outside the roster — the tree can hold one, because the store is keyed by whatever
/// wrote it — still gets its row under the roster scope rather than vanishing.
#[test]
fn a_tree_venue_that_is_not_on_the_roster_still_gets_a_row() {
    let tree = vec![node("not-a-roster-venue", 2)];
    let rows = venue_rows(
        &tree,
        &vike_data_manager::DataManagerInventoryGapMap::new(),
        (0, 0),
        DataManagerVenueRollupScope::Roster,
    );
    assert!(rows.iter().any(|r| r.venue == "not-a-roster-venue"));
    assert_eq!(rows.len(), vike_model::VENUES.len() + 1);
}

/// The gap scope counts from the SAME map the stored grid paints, so the two cannot disagree.
#[test]
fn has_gaps_keeps_only_the_venues_the_gap_map_names() {
    let tree = vec![node("binance", 4), node("okx", 3)];
    let rows = venue_rows(&tree, &gap_on("okx"), (0, 0), DataManagerVenueRollupScope::HasGaps);
    assert_eq!(rows.iter().map(|r| r.venue).collect::<Vec<_>>(), vec!["okx"]);
    assert_eq!(rows[0].gaps, 1);
}
