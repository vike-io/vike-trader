//! Unit tests of the views: the stale rule to the millisecond, and what All, Stale and Has gaps keep.

use super::*;
use crate::test_support::{key, tree_fixture};

#[test]
fn is_stale_flags_far_behind_global_max() {
    // global window is 0..1000; a series ending at 100 is 90% behind -> stale.
    assert!(is_stale(100, 0, 1000));
    // a series ending at 950 is only 5% behind -> not stale.
    assert!(!is_stale(950, 0, 1000));
    // degenerate window (no span) never flags stale.
    assert!(!is_stale(0, 0, 0));
}

/// The stale rule is ONE judgement: `is_stale` reads `stale_cutoff_ms`, so a row a millisecond
/// before the printed cut-off is stale and one on it is not, on every window. On a window wide
/// enough (2^60 ms) the old `f64` ratio rounded the first row's `behind / span` down to 0.35 and
/// called it fresh, while the cut-off a screen prints listed it as stale.
#[test]
fn is_stale_is_the_cutoff_to_the_millisecond() {
    let day = 86_400_000;
    for (first, last) in
        [(0, 1000), (0, 100 * day), (1_700_000_000_000, 1_763_000_000_000), (0, 1 << 60)]
    {
        let cut = stale_cutoff_ms(first, last).expect("a window with a span");
        assert!(is_stale(cut - 1, first, last), "{first}..{last}: before the cut-off");
        assert!(!is_stale(cut, first, last), "{first}..{last}: on the cut-off");
    }
    assert_eq!(stale_cutoff_ms(0, 100 * day), Some(65 * day));
    assert_eq!(stale_cutoff_ms(5, 5), None);
}

// ============================ filter_tree / DataManagerInventoryFilter (commit 1) ============================

#[test]
fn filter_tree_all_keeps_everything() {
    let tree = tree_fixture();
    let filtered =
        filter_tree(&tree, &DataManagerInventoryFilter::All, &DataManagerInventoryGapMap::new());
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].symbols.len() + filtered[1].symbols.len(), 3);
}

#[test]
fn filter_tree_stale_keeps_only_stale_rows() {
    let tree = tree_fixture();
    let filtered =
        filter_tree(&tree, &DataManagerInventoryFilter::Stale, &DataManagerInventoryGapMap::new());
    // Only binance/STALE lags more than 35% of the tree-wide span behind the global max;
    // binance/FRESH and okx/BTC-USDT both drop out, and okx's venue disappears entirely.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].venue, "binance");
    assert_eq!(filtered[0].symbols.len(), 1);
    assert_eq!(filtered[0].symbols[0].symbol, "STALE");
}

// ============================ gap viz (data-manager-gap-viz) ============================

#[test]
fn has_gaps_true_for_nonempty_list() {
    assert!(has_gaps(&[(0, 100)]));
}

#[test]
fn has_gaps_false_for_empty_list() {
    assert!(!has_gaps(&[]));
}

#[test]
fn filter_tree_has_gaps_empty_map_keeps_nothing() {
    let tree = tree_fixture();
    // No series has an entry in the gap map at all -> HasGaps keeps nothing.
    let filtered = filter_tree(
        &tree,
        &DataManagerInventoryFilter::HasGaps,
        &DataManagerInventoryGapMap::new(),
    );
    assert!(
        filtered.is_empty(),
        "an empty DataManagerInventoryGapMap must not accidentally keep every row"
    );
}

#[test]
fn filter_tree_has_gaps_keeps_only_gapped_series() {
    let tree = tree_fixture();
    let mut gaps = DataManagerInventoryGapMap::new();
    // binance/FRESH has a recorded gap; binance/STALE and okx/BTC-USDT do not (absent from
    // the map at all, mirroring "never fetched" / "no gaps found").
    gaps.insert(key("binance", "FRESH"), vec![(10, 20)]);
    let filtered = filter_tree(&tree, &DataManagerInventoryFilter::HasGaps, &gaps);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].venue, "binance");
    assert_eq!(filtered[0].symbols.len(), 1);
    assert_eq!(filtered[0].symbols[0].symbol, "FRESH");
}

#[test]
fn filter_tree_has_gaps_ignores_series_with_empty_gap_list() {
    let tree = tree_fixture();
    let mut gaps = DataManagerInventoryGapMap::new();
    // Present in the map but with an empty Vec -> same as "no gaps" (`has_gaps` is false).
    gaps.insert(key("binance", "FRESH"), vec![]);
    let filtered = filter_tree(&tree, &DataManagerInventoryFilter::HasGaps, &gaps);
    assert!(filtered.is_empty());
}
