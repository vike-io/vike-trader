//! Unit tests of the tree: the grouping and rollups of `build_inventory_tree`, and the flat rows
//! and the series keys both views walk.

use super::*;
use vike_data::{SeriesCoverage, SeriesId};

fn sid(kind: StoreKindId, venue: &str, symbol: &str, iv: Option<&str>) -> SeriesId {
    SeriesId::per_symbol(kind, venue, symbol, iv.map(Into::into))
}
fn coverage(rows: u64, bytes: u64, a: i64, b: i64) -> SeriesCoverage {
    SeriesCoverage { first_timestamp: a, last_timestamp: b, rows, bytes, parts: 1, dates: 1 }
}

/// A GROUPED series shows under its GROUP name, not a blank node.
///
/// `SeriesId.symbol` is deliberately empty for a grouped series — it holds many symbols, told
/// apart by a row column — so keying the tree on `symbol` collapsed every group in a venue into
/// one unnamed row. The customer would see a single blank entry with everything under it.
#[test]
fn a_grouped_series_shows_under_its_group_name() {
    let g = SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m");
    let s = SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "ETHUSDT", None);
    let tree =
        build_inventory_tree(vec![(g, SeriesCoverage::default()), (s, SeriesCoverage::default())]);
    assert_eq!(tree.len(), 1, "one venue");
    let names: Vec<&str> = tree[0].symbols.iter().map(|n| n.symbol.as_str()).collect();
    assert!(names.contains(&"btc-5m"), "the group is named, not blank: {names:?}");
    assert!(names.contains(&"ETHUSDT"), "per-symbol series unaffected: {names:?}");
    assert!(!names.contains(&""), "no blank node: {names:?}");
}

#[test]
fn groups_by_venue_then_symbol_and_rolls_up() {
    let inventory = vec![
        (sid(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m")), coverage(100, 10, 1, 9)),
        (sid(StoreKindId::Trade, "binance", "BTCUSDT", None), coverage(50, 5, 3, 7)),
        (sid(StoreKindId::Bar, "okx", "BTC-USDT", Some("1m")), coverage(20, 2, 2, 8)),
    ];
    let tree = build_inventory_tree(inventory);
    assert_eq!(tree.len(), 2); // binance, okx (sorted)
    assert_eq!(tree[0].venue, "binance");
    assert_eq!(tree[0].symbols.len(), 1);
    let btc = &tree[0].symbols[0];
    assert_eq!(btc.series.len(), 2); // bar/1m + trade
    assert_eq!(btc.total.rows, 150);
    assert_eq!(btc.total.bytes, 15);
    assert_eq!(tree[0].total.rows, 150); // venue rollup
    assert_eq!(tree[0].total.first_timestamp, 1);
    assert_eq!(tree[0].total.last_timestamp, 9);
}

#[test]
fn empty_inventory_is_empty_tree() {
    assert!(build_inventory_tree(Vec::new()).is_empty());
}

#[test]
fn flatten_venue_filters_by_query_and_flattens_series() {
    use crate::inventory::{
        DataManagerInventoryRollup, DataManagerInventorySeries, DataManagerInventorySymbol,
    };
    let venue = DataManagerInventoryVenue {
        venue: "binance".into(),
        symbols: vec![
            DataManagerInventorySymbol {
                symbol: "BTCUSDT".into(),
                grouped: false,
                series: vec![
                    DataManagerInventorySeries {
                        kind: StoreKindId::Bar,
                        interval: Some("1m".into()),
                        source: None,
                        coverage: coverage(100, 10, 0, 10),
                    },
                    DataManagerInventorySeries {
                        kind: StoreKindId::Trade,
                        interval: None,
                        source: None,
                        coverage: coverage(50, 5, 0, 10),
                    },
                ],
                total: DataManagerInventoryRollup::default(),
            },
            DataManagerInventorySymbol {
                symbol: "ETHUSDT".into(),
                grouped: false,
                series: vec![],
                total: DataManagerInventoryRollup::default(),
            },
        ],
        total: DataManagerInventoryRollup::default(),
    };
    let all = flatten_venue(&venue, "");
    assert_eq!(all.len(), 2); // BTCUSDT's two series; ETHUSDT has none

    let filtered = flatten_venue(&venue, "eth");
    assert!(filtered.is_empty()); // ETHUSDT matches the query but has no series rows

    let filtered = flatten_venue(&venue, "btc");
    assert_eq!(filtered.len(), 2);
}

/// A click on either of two series that share a label, kind and interval opens THAT series: the
/// selection a row reports finds its own node and row again, not the first node with the label
/// (the selection used to carry no `grouped`, so both rows opened one series).
#[test]
fn a_click_opens_the_series_clicked_when_two_share_a_label() {
    use vike_data::SeriesId;
    let grouped = SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m");
    let single = SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "btc-5m", None);
    let tree = crate::inventory::build_inventory_tree(vec![
        (grouped, coverage(10, 1, 0, 10)),
        (single, coverage(20, 1, 0, 10)),
    ]);
    let rows = flatten_venue(&tree[0], "");
    assert_eq!(rows.len(), 2, "two rows");
    for row in &rows {
        let picked = series_key(&tree[0], row);
        assert_eq!(picked.grouped, row.grouped);
        let (node, found) = picked.find(&tree).expect("the clicked series is in the tree");
        assert_eq!(node.grouped, row.grouped, "the clicked layout, not the first with the label");
        assert_eq!(found.coverage.rows, row.coverage.rows, "the clicked series' own coverage");
    }
    let gone =
        DataManagerInventorySeriesKey { venue: "okx".into(), ..series_key(&tree[0], &rows[0]) };
    assert!(gone.find(&tree).is_none(), "a series a refresh dropped finds nothing");
}
