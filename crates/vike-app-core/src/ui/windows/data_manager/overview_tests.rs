use super::*;

/// Overview's "GO TO" row never offers the landing screen the operator is already standing on.
#[test]
fn the_go_to_row_never_offers_the_screen_it_is_printed_on() {
    assert!(!GO_TO.contains(&DataManagerDestination::Overview));
    assert!(!GO_TO.contains(&DataManagerDestination::ByVenue));
}

/// ...and it DOES lead with the stored grid, the screen holding the most content in the window.
///
/// Pinned because it was dropped once, on an argument that holds only while something needs
/// attention: with an empty attention list — the GOOD state — no row above this strip links
/// anywhere, and the grid became reachable from the landing screen through the rail alone.
#[test]
fn the_go_to_row_leads_with_the_stored_grid() {
    assert_eq!(GO_TO.first(), Some(&DataManagerDestination::AllSeries));
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
    let (slice, span) = stale_slice(&[], &vike_data_manager::DataManagerInventoryGapMap::new());
    assert!(slice.is_empty());
    assert_eq!(span, (0, 0));
}
