use super::*;

/// `day` refuses to render the sentinel an empty tree used to produce.
///
/// ⚠ This is the reason `global_span` was fixed rather than worked around here. Before it
/// returned `(0, 0)` for an empty tree it returned `(i64::MAX, i64::MIN)`, and By venue is the
/// first caller that RENDERS the window as a date — `i64::MAX` epoch-ms is a date about 292
/// million years out.
#[test]
fn an_empty_window_renders_as_a_dash_not_a_sentinel_date() {
    assert_eq!(day(0), "—");
    assert_eq!(day(-1), "—");
    assert_ne!(day(1_700_000_000_000), "—");
}

/// The empty tree's window is `(0, 0)`, which is what the doc has always claimed.
#[test]
fn global_span_of_an_empty_tree_is_zero_width() {
    assert_eq!(vike_data_manager::global_span(&[]), (0, 0));
}

/// A venue node holding `series` series and no symbol detail — enough for the scope folds,
/// which read `total` and the gap map and reach into `symbols` only for the stale count.
fn node(venue: &str, series: usize) -> VenueNode {
    VenueNode {
        venue: venue.to_string(),
        symbols: Vec::new(),
        total: vike_data_manager::RollUp { series, ..Default::default() },
    }
}

fn gap_on(venue: &str) -> vike_data_manager::GapMap {
    let mut gaps = vike_data_manager::GapMap::new();
    gaps.insert(
        vike_data_manager::SeriesKey {
            venue: venue.to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: "trade".to_string(),
            interval: None,
        },
        vec![(1_000, 2_000)],
    );
    gaps
}

/// The default scope lists what the store holds, and nothing else.
#[test]
fn holds_series_drops_a_venue_whose_rollup_is_empty() {
    let tree = vec![node("binance", 4), node("okx", 0)];
    let rows =
        venue_rows(&tree, &vike_data_manager::GapMap::new(), (0, 0), VenueScope::HoldsSeries);
    assert_eq!(rows.iter().map(|r| r.venue).collect::<Vec<_>>(), vec!["binance"]);
}

/// The roster scope is the whole point of the control: a venue the tree built no node for gets
/// a row anyway, marked as holding nothing.
#[test]
fn the_roster_scope_gives_an_unstored_venue_a_row_of_its_own() {
    let tree = vec![node("binance", 4)];
    let rows = venue_rows(&tree, &vike_data_manager::GapMap::new(), (0, 0), VenueScope::Roster);
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
    let rows = venue_rows(&tree, &vike_data_manager::GapMap::new(), (0, 0), VenueScope::Roster);
    assert_eq!(rows.iter().filter(|r| r.venue == "binance").count(), 1);
    assert_eq!(rows.iter().filter(|r| r.venue == "okx").count(), 1);
}

/// A venue outside the roster — the tree can hold one, because the store is keyed by whatever
/// wrote it — still gets its row under the roster scope rather than vanishing.
#[test]
fn a_tree_venue_that_is_not_on_the_roster_still_gets_a_row() {
    let tree = vec![node("not-a-roster-venue", 2)];
    let rows = venue_rows(&tree, &vike_data_manager::GapMap::new(), (0, 0), VenueScope::Roster);
    assert!(rows.iter().any(|r| r.venue == "not-a-roster-venue"));
    assert_eq!(rows.len(), vike_model::VENUES.len() + 1);
}

/// The gap scope counts from the SAME map the stored grid paints, so the two cannot disagree.
#[test]
fn has_gaps_keeps_only_the_venues_the_gap_map_names() {
    let tree = vec![node("binance", 4), node("okx", 3)];
    let rows = venue_rows(&tree, &gap_on("okx"), (0, 0), VenueScope::HasGaps);
    assert_eq!(rows.iter().map(|r| r.venue).collect::<Vec<_>>(), vec!["okx"]);
    assert_eq!(rows[0].gaps, 1);
}

/// Overview's "GO TO" row never offers the landing screen the operator is already standing on.
#[test]
fn the_go_to_row_never_offers_the_screen_it_is_printed_on() {
    assert!(!GO_TO.contains(&DataDest::Overview));
    assert!(!GO_TO.contains(&DataDest::ByVenue));
}

/// ...and it DOES lead with the stored grid, the screen holding the most content in the window.
///
/// Pinned because it was dropped once, on an argument that holds only while something needs
/// attention: with an empty attention list — the GOOD state — no row above this strip links
/// anywhere, and the grid became reachable from the landing screen through the rail alone.
#[test]
fn the_go_to_row_leads_with_the_stored_grid() {
    assert_eq!(GO_TO.first(), Some(&DataDest::AllSeries));
}

/// No destination is offered twice. A duplicate pill is two routes to one screen taking the
/// space of a screen the strip does not reach at all.
#[test]
fn the_go_to_row_never_offers_one_destination_twice() {
    for (i, d) in GO_TO.iter().enumerate() {
        assert!(!GO_TO[i + 1..].contains(d), "{} is offered twice", d.label());
    }
}

/// Overview's one DEAD row action carries a real reason, on the same terms the Store screen's
/// three do — a hover text that echoes the label teaches nothing.
#[test]
fn overviews_dead_row_action_explains_itself() {
    assert!(PARTIAL_NO_KEY.len() > 40);
    assert!(PARTIAL_NO_KEY.ends_with('.'));
    assert_ne!(PARTIAL_NO_KEY, "Backfill");
    // ...and its LIVE twin says what it will actually do, including the skip.
    assert!(BACKFILL_HOVER.contains("skipped"));
}

/// A series carrying an interval is named with it. Two series that differ only by bar size are
/// otherwise one row printed twice, which reads as a duplicate rather than as two findings.
#[test]
fn a_row_title_keeps_the_interval_that_tells_two_series_apart() {
    assert_eq!(
        series_title("binance", "BTCUSDT", "bar", Some("1m")),
        "binance / BTCUSDT / bar · 1m"
    );
    assert_eq!(series_title("okx", "LTC-USDT", "quote", None), "okx / LTC-USDT / quote");
    assert_ne!(
        series_title("binance", "BTCUSDT", "bar", Some("1m")),
        series_title("binance", "BTCUSDT", "bar", Some("1h"))
    );
}

/// The stale fold is the Stale destination's own — an empty tree yields no rows and no count,
/// rather than the whole tree filtered by a predicate that cannot judge a zero-width window.
#[test]
fn an_empty_tree_has_no_stale_slice() {
    let (slice, span) = stale_slice(&[], &vike_data_manager::GapMap::new());
    assert!(slice.is_empty());
    assert_eq!(span, (0, 0));
}

/// Every dead Store action carries a REASON, and the reason is a sentence rather than the label
/// said twice. A blank or echoing hover text is the failure this table exists to prevent.
#[test]
fn every_disabled_store_action_explains_itself() {
    for (label, why) in STORE_ACTIONS {
        assert!(why.len() > 40, "{label} has no real reason on it");
        assert!(why.ends_with('.'), "{label}'s reason is not a sentence");
        assert_ne!(label, why);
    }
}
