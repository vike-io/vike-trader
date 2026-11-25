//! The Data-Manager "Stored" catalog: the inventory TREE (this root) and the egui views over it.
//!
//! # The tree
//!
//! Pure grouping of the HistoryStore inventory (`Vec<(SeriesId, SeriesCoverage)>`) into a
//! venue → symbol → (kind, interval) display tree with per-node rollups, for the Data Manager's
//! "Stored" view. No I/O — the caller fetches `HistoryStoreDataFusion::inventory()` off-thread and hands
//! the result here.
//!
//! The root also holds what every child walks: the identity of one series
//! ([`DataManagerInventorySeriesKey`]), the flat display row both views sort and draw
//! ([`flatten_venue`]), and the rollup fold the views rebuild a filtered subtree's totals with.
//!
//! # The children, by role
//!
//! * `filter` — the views over the tree (All, Has gaps, Stale), the per-series gap map, what a
//!   failed or pending probe says, and the stale rule over the tree-wide span;
//! * `partial_days` — the CROSS-KIND partial days, keyed per instrument, and their one-line label;
//! * `table` — the rows and cells both views draw: the sortable column headers, one row's cells,
//!   the coverage bar and its legend, the search row and the empty-state sentences;
//! * `list` — `inventory_list`, the Studio's plain picker;
//! * `grid` — `inventory_grid`, the app's rich multi-select grid and its bulk-action bar.
//!
//! # The render
//!
//! The egui render of the Data-Manager "Stored" catalog: a dense, sortable venue-grouped grid
//! over [`build_inventory_tree`]'s output, searchable by venue/symbol, with a click-to-select
//! row. Extracted out of `vike-app`'s `data_tool_content` (the "Stored" tab render) and
//! `vike-studio`'s bespoke `data_browser.rs` grouping/render so both mount the identical view.
//! Deliberately NOT coupled to vike-app-core's `WindowView` or live `feeds`: this takes only a
//! `&[DataManagerInventoryVenue]` + a `&mut String` query and returns a plain [`DataManagerInventorySeriesKey`] — the caller
//! (app or Studio) decides what a click means (open a chart, wire a `StudioSlicePicker`, …).
//!
//! Density rewrite (data-manager-dense): the old per-symbol `CollapsingHeader` tree (chunky, awkward at
//! hundreds/thousands of symbols) is replaced with a flat table: a row-height rollup header row per
//! venue, then one row per (symbol, series) pair — the density's control height in the rich grid,
//! whose every row holds a checkbox, and its row height in the plain picker — with columns Symbol |
//! Kind | Coverage | Rows | Size | Updated. Columns are sortable (click a header to sort
//! ascending/descending, independently within each venue group); sort state lives in egui temp
//! memory keyed off `ui.scope_id()` since the function signature can't change (both mounts depend on it).
//! The Coverage column paints a thin span bar over the tree-wide `[global_first, global_last]`
//! window so bars are comparable across every row, in `DataManagerInventoryCoverageStyle`'s stale colour when a
//! series' `last_timestamp` lags far behind the global max.
//!
//! Per-series gap viz (data-manager-gap-viz): `inventory_grid` (the app-only rich grid; the Studio's
//! plain `inventory_list` is UNCHANGED) additionally takes a `&DataManagerInventoryGapMap` — the missing-day
//! ranges `vike_store::HistoryStoreDataFusion::series_gaps` computes, keyed by [`DataManagerInventorySeriesKey`]. The
//! Coverage bar overpaints each gap as a dark cut-out within the filled span, and the `HasGaps`
//! smart view (`DataManagerInventoryFilter::HasGaps`) filters to series [`has_gaps`] for. Fetching the map is the
//! caller's job (vike-desktop's `refresh_stored`, off-thread, alongside the inventory tree) — this
//! crate never calls `series_gaps` itself.
//!
//! SP2 headless discipline: everything above the actual egui paint calls (`iec_bytes`,
//! `format_count`, `format_count_compact`, `is_stale`, `has_gaps`, `flatten_venue`,
//! `cmp_flat_row`/`sort_flat_rows`, `coverage_label`, `timestamp_range_to_x_fraction`) is pure and
//! unit-tested; `inventory_list`/`inventory_grid` (egui) are not.
//!
//! [`has_gaps`]: filter::has_gaps

use vike_data::{SeriesCoverage, SeriesId};
use vike_model::StoreKindId;

pub(crate) mod filter;
pub(crate) mod grid;
pub(crate) mod list;
pub(crate) mod partial_days;
pub(crate) mod table;

/// Aggregate totals for a subtree (a symbol's series, or a venue's symbols).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataManagerInventoryRollup {
    pub rows: u64,
    pub bytes: u64,
    pub series: usize,
    pub first_timestamp: i64,
    pub last_timestamp: i64,
}
impl DataManagerInventoryRollup {
    fn add(&mut self, c: &SeriesCoverage) {
        self.rows += c.rows;
        self.bytes += c.bytes;
        self.series += 1;
        self.first_timestamp = if self.series == 1 {
            c.first_timestamp
        } else {
            self.first_timestamp.min(c.first_timestamp)
        };
        self.last_timestamp = self.last_timestamp.max(c.last_timestamp);
    }
}

/// One stored series under a symbol: its data kind + optional bar interval + source lane + coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataManagerInventorySeries {
    pub kind: StoreKindId,
    pub interval: Option<String>,
    /// The `source=` lane the rows came down (`vike_data::SeriesId::source`); `None` where none was
    /// recorded, which is every series an inventory answers today (it serves no lane).
    pub source: Option<String>,
    pub coverage: SeriesCoverage,
}

/// One symbol under a venue: its series (sorted by kind then interval) + rollup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataManagerInventorySymbol {
    /// The series LABEL — the symbol for a per-symbol series, the group name for a grouped one.
    pub symbol: String,
    /// `true` when this node is a GROUPED series (`group=` on disk). Carried because a grouped and a
    /// per-symbol series can share a label while being genuinely different instruments, and because
    /// `vike_data::InstrumentCoverageKey` — which the cross-kind coverage report is keyed by — needs it.
    pub grouped: bool,
    pub series: Vec<DataManagerInventorySeries>,
    pub total: DataManagerInventoryRollup,
}

/// One venue: its symbols (sorted) + rollup across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataManagerInventoryVenue {
    pub venue: String,
    pub symbols: Vec<DataManagerInventorySymbol>,
    pub total: DataManagerInventoryRollup,
}

/// Group + rollup the flat inventory into a sorted venue → symbol → (kind, interval) tree.
/// Pure — no I/O. Sorted deterministically (BTreeMap keys) for a stable GUI render + test asserts.
pub fn build_inventory_tree(
    inventory: Vec<(SeriesId, SeriesCoverage)>,
) -> Vec<DataManagerInventoryVenue> {
    use std::collections::BTreeMap;
    let mut venues: BTreeMap<String, BTreeMap<(String, bool), Vec<DataManagerInventorySeries>>> =
        BTreeMap::new();
    for (id, coverage) in inventory {
        // `label()`, not `symbol`: a GROUPED series carries NO symbol (it holds many, told apart by
        // a row column), so keying on `symbol` would collapse every group in a venue into one
        // blank-named node in the customer's Data Manager. The group name is what identifies it on
        // disk and is what the customer should see.
        // Keyed on (label, GROUPED): a grouped series and a per-symbol one can share a name, and they
        // are genuinely different instruments — different directories, different manifests, and a
        // family mid-migration has history in both. Collapsing them here would make the two
        // indistinguishable in the Data Manager and make an `InstrumentCoverageKey` lookup impossible.
        let node = (id.label().to_string(), id.group.is_some());
        venues.entry(id.venue).or_default().entry(node).or_default().push(
            DataManagerInventorySeries {
                kind: id.kind,
                interval: id.interval,
                source: id.source,
                coverage,
            },
        );
    }
    venues
        .into_iter()
        .map(|(venue, symbol_names)| {
            let mut vtotal = DataManagerInventoryRollup::default();
            let symbols = symbol_names
                .into_iter()
                .map(|((symbol, grouped), mut rows)| {
                    rows.sort_by(|a, b| {
                        (a.kind, &a.interval, &a.source).cmp(&(b.kind, &b.interval, &b.source))
                    });
                    let mut stotal = DataManagerInventoryRollup::default();
                    for r in &rows {
                        stotal.add(&r.coverage);
                        vtotal.add(&r.coverage);
                    }
                    DataManagerInventorySymbol { symbol, grouped, series: rows, total: stotal }
                })
                .collect();
            DataManagerInventoryVenue { venue, symbols, total: vtotal }
        })
        .collect()
}

/// The full identity of one grid row's series (`vike_data::SeriesId`'s, spelled by its LABEL) —
/// `Ord` so it can live in a `BTreeSet` (deterministic iteration, no hashing) as the rich grid's
/// multi-select set, the key of [`DataManagerInventoryGapMap`], and what a click on a row reports —
/// enough for a caller to load the series (open a chart, feed a `StudioSlicePicker`, …) and to find that
/// one row again ([`Self::find`]).
///
/// ⚠ `grouped` and `source` are part of the identity: a grouped and a per-symbol series can share a
/// label, kind and interval (`DataManagerInventorySymbol::grouped`'s doc), and without them the two shared one gap
/// entry and one checkbox — a gap of one painted on both, a tick on one selecting both.
///
/// [`DataManagerInventoryGapMap`]: crate::DataManagerInventoryGapMap
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataManagerInventorySeriesKey {
    pub venue: String,
    /// The series LABEL: the symbol of a per-symbol series, the group name of a grouped one
    /// (`vike_data::SeriesId::label`).
    pub symbol: String,
    pub kind: StoreKindId,
    pub interval: Option<String>,
    /// Whether the series is GROUPED (`vike_data::SeriesId::group`), as [`DataManagerInventorySymbol::grouped`].
    pub grouped: bool,
    /// The series' `source=` lane, as [`DataManagerInventorySeries::source`].
    pub source: Option<String>,
}

impl DataManagerInventorySeriesKey {
    /// The key of series `row` of `node` under venue `venue`: the one spelling every walk over the
    /// tree builds a key by, so none of them drops a part of the identity.
    pub fn of(
        venue: &str,
        node: &DataManagerInventorySymbol,
        row: &DataManagerInventorySeries,
    ) -> DataManagerInventorySeriesKey {
        DataManagerInventorySeriesKey {
            venue: venue.to_string(),
            symbol: node.symbol.clone(),
            kind: row.kind,
            interval: row.interval.clone(),
            grouped: node.grouped,
            source: row.source.clone(),
        }
    }

    /// The node and the row this key names in `tree`, or `None` once a refresh dropped the
    /// series: matched on the whole identity, never on the first node that carries the label.
    pub fn find<'a>(
        &self,
        tree: &'a [DataManagerInventoryVenue],
    ) -> Option<(&'a DataManagerInventorySymbol, &'a DataManagerInventorySeries)> {
        tree.iter()
            .filter(|v| v.venue == self.venue)
            .flat_map(|v| &v.symbols)
            .filter(|s| s.symbol == self.symbol && s.grouped == self.grouped)
            .find_map(|s| {
                s.series
                    .iter()
                    .find(|r| {
                        r.kind == self.kind
                            && r.interval == self.interval
                            && r.source == self.source
                    })
                    .map(|r| (s, r))
            })
    }
}

/// The identity of one flattened display row, within its venue (a [`DataManagerInventoryRow`] doesn't carry its
/// own venue — it's produced per-venue by [`flatten_venue`]).
fn series_key(
    venue: &DataManagerInventoryVenue,
    row: &DataManagerInventoryRow,
) -> DataManagerInventorySeriesKey {
    DataManagerInventorySeriesKey {
        venue: venue.venue.clone(),
        symbol: row.symbol.clone(),
        kind: row.kind,
        interval: row.interval.clone(),
        grouped: row.grouped,
        source: row.source.clone(),
    }
}

/// Rebuild a symbol-level [`DataManagerInventoryRollup`] from a (possibly filtered) series slice — mirrors
/// `DataManagerInventoryRollup::add`'s fold, over a slice `filter_tree` has already filtered (all
/// `DataManagerInventoryRollup` fields are `pub` so this stays a plain, testable fold).
fn rollup_of_series(series: &[DataManagerInventorySeries]) -> DataManagerInventoryRollup {
    let mut r = DataManagerInventoryRollup::default();
    for (i, s) in series.iter().enumerate() {
        r.rows += s.coverage.rows;
        r.bytes += s.coverage.bytes;
        r.series += 1;
        r.first_timestamp = if i == 0 {
            s.coverage.first_timestamp
        } else {
            r.first_timestamp.min(s.coverage.first_timestamp)
        };
        r.last_timestamp = r.last_timestamp.max(s.coverage.last_timestamp);
    }
    r
}

/// Rebuild a venue-level [`DataManagerInventoryRollup`] from a (possibly filtered) symbol slice, summing each
/// symbol's already-rebuilt `total`. Same rationale as [`rollup_of_series`].
fn rollup_of_symbols(symbols: &[DataManagerInventorySymbol]) -> DataManagerInventoryRollup {
    let mut r = DataManagerInventoryRollup::default();
    for (i, s) in symbols.iter().enumerate() {
        r.rows += s.total.rows;
        r.bytes += s.total.bytes;
        r.series += s.total.series;
        r.first_timestamp = if i == 0 {
            s.total.first_timestamp
        } else {
            r.first_timestamp.min(s.total.first_timestamp)
        };
        r.last_timestamp = r.last_timestamp.max(s.total.last_timestamp);
    }
    r
}

/// One flattened (symbol, series) display row within a venue — the unit the grid sorts and
/// renders. Pure/owned so it can be built and sorted without egui.
#[derive(Debug, Clone, PartialEq)]
struct DataManagerInventoryRow {
    symbol: String,
    kind: StoreKindId,
    interval: Option<String>,
    coverage: SeriesCoverage,
    /// Which layout this row's symbol lives in. Carried because the cross-kind coverage report is
    /// keyed by [`vike_data::InstrumentCoverageKey`], and a grouped series and a per-symbol one can share a
    /// label — without this the Partial column would look one of them up under the other's key.
    grouped: bool,
    /// The series' `source=` lane ([`DataManagerInventorySeries::source`]), part of its [`DataManagerInventorySeriesKey`].
    source: Option<String>,
}

/// `"{kind}"` or `"{kind}/{interval}"` — mirrors the pre-dense tree row label.
fn kind_label(row: &DataManagerInventoryRow) -> String {
    match &row.interval {
        Some(iv) => format!("{}/{iv}", row.kind),
        None => row.kind.to_string(),
    }
}

/// Case-insensitive substring match against `venue`/`symbol` (an empty query matches everything).
/// Mirrors `data_browser::matches_query`'s contract (kind/interval aren't searched).
fn matches_query(venue: &str, symbol: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || venue.to_lowercase().contains(&q) || symbol.to_lowercase().contains(&q)
}

/// Flatten one venue's symbols/series into display rows, dropping symbols that don't match
/// `query`. Order is whatever `build_inventory_tree` produced (BTreeMap-sorted); callers sort afterwards.
fn flatten_venue(venue: &DataManagerInventoryVenue, query: &str) -> Vec<DataManagerInventoryRow> {
    venue
        .symbols
        .iter()
        .filter(|s| matches_query(&venue.venue, &s.symbol, query))
        .flat_map(|s| {
            s.series.iter().map(move |r| DataManagerInventoryRow {
                symbol: s.symbol.clone(),
                kind: r.kind,
                interval: r.interval.clone(),
                coverage: r.coverage.clone(),
                grouped: s.grouped,
                source: r.source.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
