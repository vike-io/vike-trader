//! The VIEWS over the inventory tree — All, Has gaps and Stale ([`DataManagerInventoryFilter`],
//! [`filter_tree`]) — and the facts they filter on: the per-series gap map, what a failed or pending
//! probe says ([`gaps_unknown`], [`GAPS_NOT_CHECKED`], [`CHECKING_MARK`]), and the stale rule over the
//! tree-wide span ([`global_span`], [`is_stale`]). Pure: no egui.

use std::collections::HashMap;

use super::{
    DataManagerInventorySeries, DataManagerInventorySeriesKey, DataManagerInventorySymbol,
    DataManagerInventoryVenue, rollup_of_series, rollup_of_symbols,
};

/// Per-series gap ranges (inclusive epoch-ms, `vike_store::HistoryStoreDataFusion::series_gaps`'s
/// convention), keyed by the same [`DataManagerInventorySeriesKey`] identity the grid already uses for selection.
/// An absent key means "no known gaps" — callers that haven't fetched gaps yet can pass an empty
/// map and get today's (gap-free) behavior unchanged. ⚠ An EMPTY `Vec` means the probe of that series
/// FAILED: its gaps are not known ([`gaps_unknown`]). A whole series gets no entry at all.
pub type DataManagerInventoryGapMap = HashMap<DataManagerInventorySeriesKey, Vec<(i64, i64)>>;

/// Which view is active for the rich grid. `All` (the default) shows the whole tree. `HasGaps` is
/// wired (data-manager-gap-viz): `filter_tree` takes a [`DataManagerInventoryGapMap`] and keeps only series with a non-empty gap
/// list (via [`has_gaps`]).
///
/// ⚠ A `Venue`, an `AssetClass` and a `Watchlist` variant stood here until 2026-10. Nothing but
/// the deleted `views_sidebar` picker ever set `Venue` or `Watchlist`, nothing set `AssetClass`,
/// and the latter two were placeholders that filtered nothing (the display tree has no asset-class
/// tag, and no caller supplies watchlist symbol membership). The window's rail sets one of the
/// three that remain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DataManagerInventoryFilter {
    #[default]
    All,
    HasGaps,
    Stale,
}

/// Tree-wide `(first_timestamp, last_timestamp)` window across every venue with at least one series, for the
/// Coverage column's span bars to share one comparable scale. `(0, 0)` (an inert, zero-width
/// window — every bar paints as empty) when nothing has data.
///
/// ⚠ **That empty-tree clause was a LIE until now.** The fold seeded `(i64::MAX, i64::MIN)` and
/// returned it unchanged when no venue had a series, so an empty tree answered with those sentinels
/// rather than `(0, 0)`. Nothing noticed while the only consumer was the span bar — `is_stale`
/// refuses a window whose end is not after its start, and a bar drawn on it is empty either way. It
/// stops being harmless the moment a caller RENDERS the window, which the By-venue destination
/// does: `i64::MAX` epoch-ms formats as a date roughly 292 million years out.
pub fn global_span(tree: &[DataManagerInventoryVenue]) -> (i64, i64) {
    let (lo, hi) =
        tree.iter().filter(|v| v.total.series > 0).fold((i64::MAX, i64::MIN), |(lo, hi), v| {
            (lo.min(v.total.first_timestamp), hi.max(v.total.last_timestamp))
        });
    if lo > hi { (0, 0) } else { (lo, hi) }
}

/// A series reads as "stale" once its `last_timestamp` lags more than 35% of the tree-wide span behind
/// the global max — an arbitrary but conservative fraction (recent data is typically within a few
/// percent; anything past a third of the whole window behind is very likely an abandoned/broken
/// feed, not just "yesterday's close"). The rule is [`stale_cutoff_ms`]: a series is stale when its
/// last row falls before that instant, so the date a screen prints and the rows it lists are one
/// judgement.
pub fn is_stale(last_timestamp: i64, global_first: i64, global_last: i64) -> bool {
    stale_cutoff_ms(global_first, global_last).is_some_and(|cut| last_timestamp < cut)
}

/// The instant a series' last row must fall before to read as stale ([`is_stale`]), given the
/// tree-wide window: `behind / span > 0.35` solved for `last_timestamp`. A zero-width window (the
/// empty tree) has no cut-off at all.
///
/// Integer, `span * 35 / 100` in `i128`: exact for every span an epoch-ms window can hold, where
/// `0.35` has no exact binary representation and an `f64` ratio misjudged a row on the boundary of a
/// wide enough window. For a whole-millisecond `last_timestamp`, `last_timestamp < cut` is exactly
/// `behind > 0.35 · span`.
pub fn stale_cutoff_ms(global_first: i64, global_last: i64) -> Option<i64> {
    if global_last <= global_first {
        return None;
    }
    let span = i128::from(global_last) - i128::from(global_first);
    Some((i128::from(global_last) - span * 35 / 100) as i64)
}

/// Whether a series has at least one missing-data gap — the pure predicate the `HasGaps` smart
/// view filters on, and the coverage bar's gap paint uses to skip the overpaint loop entirely
/// for gap-free series. A series absent from the [`DataManagerInventoryGapMap`] was answered whole, or never asked about;
/// one whose probe failed is listed with an EMPTY list ([`gaps_unknown`]), and has no gap to paint.
pub fn has_gaps(gaps: &[(i64, i64)]) -> bool {
    !gaps.is_empty()
}

/// How many series' gap probes failed, so that nobody can say whether they hold a hole: the [`DataManagerInventoryGapMap`]
/// entries with an empty list. ⚠ Such a series is NOT gap-free, and a screen that counts gaps says these
/// apart, in [`GAPS_NOT_CHECKED`]'s terms.
pub fn gaps_unknown(gaps: &DataManagerInventoryGapMap) -> usize {
    gaps.values().filter(|ranges| ranges.is_empty()).count()
}

/// What a series whose gap probe failed says: a row of the grid on hover.
pub const GAPS_NOT_CHECKED: &str = "The gap check of this series failed, so whether it has holes is not known. Refresh asks again.";

/// The mark a cell carries while its answer is still on the way, beside [`GAPS_NOT_CHECKED`]'s `?`.
pub const CHECKING_MARK: &str = "\u{2026}";

/// What a row whose gap probe has not answered yet says on hover.
pub const GAPS_CHECKING: &str = "checking\u{2026} this series' gap probe has not answered yet.";

/// What a Partial cell says on hover while the cross-kind coverage report has not answered yet.
pub const PARTIALS_CHECKING: &str =
    "checking\u{2026} the cross-kind coverage report has not answered yet.";

/// Keep only `node`'s series that [`has_gaps`], dropping symbols left with none and returning
/// `None` if the whole venue empties out. Mirrors [`filter_stale_venue`]'s shape; rollups are
/// rebuilt over the surviving rows for the same reason.
fn filter_has_gaps_venue(
    node: &DataManagerInventoryVenue,
    gaps: &DataManagerInventoryGapMap,
) -> Option<DataManagerInventoryVenue> {
    let symbols: Vec<DataManagerInventorySymbol> = node
        .symbols
        .iter()
        .filter_map(|symbol_name| {
            let series: Vec<DataManagerInventorySeries> = symbol_name
                .series
                .iter()
                .filter(|r| {
                    let key = DataManagerInventorySeriesKey::of(&node.venue, symbol_name, r);
                    gaps.get(&key).map(|g| has_gaps(g)).unwrap_or(false)
                })
                .cloned()
                .collect();
            if series.is_empty() {
                None
            } else {
                let total = rollup_of_series(&series);
                Some(DataManagerInventorySymbol {
                    symbol: symbol_name.symbol.clone(),
                    grouped: symbol_name.grouped,
                    series,
                    total,
                })
            }
        })
        .collect();
    if symbols.is_empty() {
        return None;
    }
    let total = rollup_of_symbols(&symbols);
    Some(DataManagerInventoryVenue { venue: node.venue.clone(), symbols, total })
}

/// Keep only `node`'s stale series (relative to the tree-wide `[global_first, global_last]`
/// window), dropping symbols left with none and returning `None` if the whole venue empties out.
/// Rollups are rebuilt over the surviving rows so the filtered tree's header summaries and
/// Coverage-column span bars stay internally consistent.
fn filter_stale_venue(
    node: &DataManagerInventoryVenue,
    global_first: i64,
    global_last: i64,
) -> Option<DataManagerInventoryVenue> {
    let symbols: Vec<DataManagerInventorySymbol> = node
        .symbols
        .iter()
        .filter_map(|symbol_name| {
            let series: Vec<DataManagerInventorySeries> = symbol_name
                .series
                .iter()
                .filter(|r| is_stale(r.coverage.last_timestamp, global_first, global_last))
                .cloned()
                .collect();
            if series.is_empty() {
                None
            } else {
                let total = rollup_of_series(&series);
                Some(DataManagerInventorySymbol {
                    symbol: symbol_name.symbol.clone(),
                    grouped: symbol_name.grouped,
                    series,
                    total,
                })
            }
        })
        .collect();
    if symbols.is_empty() {
        return None;
    }
    let total = rollup_of_symbols(&symbols);
    Some(DataManagerInventoryVenue { venue: node.venue.clone(), symbols, total })
}

/// Apply the active view to `tree`, returning a filtered
/// copy. `All` is the identity (a full clone); `Stale` keeps only
/// series whose coverage lags the tree-wide span (reusing [`is_stale`]), dropping symbols/venues
/// left with nothing. `HasGaps` keeps only series [`has_gaps`] for in `gaps` (a series absent from
/// `gaps`, or fetched with an empty list, is treated as gap-free).
///
/// ⚠ The caller sets [`DataManagerInventoryGridState::active_view`]. It used to be set by a `views_sidebar` picker
/// inside the grid's own body; that picker is deleted and
/// `crates/vike-app-core/src/ui/windows/data_manager/rail.rs` sets it from the window's rail instead.
///
/// [`DataManagerInventoryGridState::active_view`]: crate::DataManagerInventoryGridState::active_view
pub fn filter_tree(
    tree: &[DataManagerInventoryVenue],
    active: &DataManagerInventoryFilter,
    gaps: &DataManagerInventoryGapMap,
) -> Vec<DataManagerInventoryVenue> {
    match active {
        DataManagerInventoryFilter::All => tree.to_vec(),
        DataManagerInventoryFilter::Stale => {
            let (global_first, global_last) = global_span(tree);
            tree.iter()
                .filter_map(|node| filter_stale_venue(node, global_first, global_last))
                .collect()
        }
        DataManagerInventoryFilter::HasGaps => {
            tree.iter().filter_map(|node| filter_has_gaps_venue(node, gaps)).collect()
        }
    }
}

// ⚠ `views_sidebar` used to live here and is DELETED.
//
// It was the rich grid's own left navigation — All / one entry per venue / the Has-gaps and Stale smart
// views / a Watchlists group — and it sat INSIDE one of the Data Manager's seven sub-tabs, which
// made the window carry two stacked navigations. `crates/vike-app-core/src/ui/windows/data_manager/rail.rs`
// is that rail promoted to be the window's only one, and it owns `DataManagerInventoryGridState::active_view` directly.
//
// Deleted rather than left behind a `#[deprecated]` or kept "in case": it had no test, no caller
// outside the app body that replaced it, and the root `CLAUDE.md`'s "Conventions that will bite you
// if ignored" bans a shim kept so an old spelling compiles — which applies to a whole function just
// as much as to the re-export it names. [`DataManagerInventoryFilter`] and
// [`filter_tree`] are untouched — the FILTER is the contract, and the picker was never it.

#[path = "filter_tests.rs"]
#[cfg(test)]
mod filter_tests;
