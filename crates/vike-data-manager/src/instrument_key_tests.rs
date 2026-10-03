use super::*;
use crate::model::build_tree;
use vike_data::{SeriesCoverage, SeriesId};

fn covg() -> SeriesCoverage {
    SeriesCoverage { first_ts: 0, last_ts: 1, rows: 1, bytes: 1, parts: 1, dates: 1 }
}

/// **The distinction the tree used to lose.** A grouped series and a per-symbol one sharing a
/// name are different instruments on disk; before this they collapsed into ONE node, which made
/// them indistinguishable in the Data Manager and an `InstrumentKey` lookup impossible.
#[test]
fn a_group_and_a_symbol_of_the_same_name_are_two_nodes() {
    let tree = build_tree(vec![
        (SeriesId::per_symbol("trade", "polymarket", "fam", None), covg()),
        (SeriesId::grouped("trade", "polymarket", "fam"), covg()),
    ]);
    assert_eq!(tree.len(), 1, "one venue");
    assert_eq!(tree[0].symbols.len(), 2, "two instruments, not one merged node");
    assert!(tree[0].symbols.iter().any(|s| s.grouped));
    assert!(tree[0].symbols.iter().any(|s| !s.grouped));
    // ...and they produce DIFFERENT keys, so a coverage lookup resolves each correctly.
    let keys: Vec<_> =
        tree[0].symbols.iter().map(|s| instrument_key_of(&tree[0].venue, s)).collect();
    assert_ne!(keys[0], keys[1]);
}

/// A plain per-symbol series is `grouped: false` and keys to itself — the ordinary case is
/// unchanged.
#[test]
fn a_plain_series_keys_to_its_own_symbol() {
    let tree =
        build_tree(vec![(SeriesId::per_symbol("trade", "binance", "BTCUSDT", None), covg())]);
    let key = instrument_key_of("binance", &tree[0].symbols[0]);
    assert_eq!(key.label, "BTCUSDT");
    assert!(!key.grouped);
}

/// The rendered warning reaches the right instrument through the tree.
#[test]
fn the_label_resolves_through_a_tree_node() {
    let tree = build_tree(vec![(SeriesId::per_symbol("trade", "polymarket", "TOK", None), covg())]);
    let report = vike_data::coverage::join_coverage(&[
        (SeriesId::per_symbol("trade", "polymarket", "TOK", None), vec![1, 2, 3]),
        (SeriesId::per_symbol("quote", "polymarket", "TOK", None), vec![1]),
    ]);
    let map = partial_days_from_coverage(&report);

    assert_eq!(
        symbol_partial_label(&map, "polymarket", &tree[0].symbols[0]),
        // Days 2 and 3 have trades and no quotes. `book`/`depth` were never recorded here at
        // all, so they are absent rather than missing and are named on no day.
        "2 partial days (quote)"
    );
}
