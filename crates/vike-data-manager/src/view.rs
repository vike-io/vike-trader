//! The egui render of the Data-Manager "Stored" catalog: a dense, sortable venue-grouped grid
//! over [`crate::model::build_tree`]'s output, searchable by venue/symbol, with a click-to-select
//! row. Extracted out of `vike-app`'s `data_tool_content` (the "Stored" tab render) and
//! `vike-studio`'s bespoke `data_browser.rs` grouping/render so both mount the identical view.
//! Deliberately NOT coupled to vike-app's `ToolView` or live `feeds`: this takes only a
//! `&[VenueNode]` + a `&mut String` query and returns a plain [`StoredSelection`] — the caller
//! (app or Studio) decides what a click means (open a chart, wire a `SlicePicker`, …).
//!
//! Density rewrite (dm-dense): the old per-symbol `CollapsingHeader` tree (chunky, awkward at
//! hundreds/thousands of symbols) is replaced with a flat table: one thin rollup header row per
//! venue, then one ~26px row per (symbol, series) pair with columns Symbol | Kind | Coverage |
//! Rows | Size | Updated. Columns are sortable (click a header to sort ascending/descending,
//! independently within each venue group); sort state lives in egui temp memory keyed off `ui.id()`
//! since the function signature can't change (both mounts depend on it). The Coverage column
//! paints a thin span bar over the tree-wide `[global_first, global_last]` window so bars are
//! comparable across every row, with a "stale" tint when a series' `last_ts` lags far behind the
//! global max.
//!
//! Per-series gap viz (dm-gap-viz): `stored_catalog_grid` (the app-only rich grid; the Studio's
//! plain `stored_catalog_ui` is UNCHANGED) additionally takes a `&GapMap` — the missing-day
//! ranges `vike_data::DataFusionHist::series_gaps` computes, keyed by [`SeriesKey`]. The
//! Coverage bar overpaints each gap as a dark cut-out within the filled span, and the `HasGaps`
//! smart view (`ViewFilter::HasGaps`) filters to series [`has_gaps`] for. Fetching the map is the
//! caller's job (vike-app's `refresh_stored`, off-thread, alongside the inventory tree) — this
//! crate never calls `series_gaps` itself.
//!
//! SP2 headless discipline: everything above the actual egui paint calls (`fmt_bytes`,
//! `fmt_count`, `fmt_count_compact`, `is_stale`, `has_gaps`, `flatten_venue`,
//! `cmp_flat_row`/`sort_flat_rows`, `coverage_label`, `ts_range_to_x_fraction`) is pure and
//! unit-tested; `stored_catalog_ui`/`stored_catalog_grid` (egui) are not.

use crate::model::{RollUp, SeriesRow, SymbolNode, VenueNode};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use vike_data::SeriesCoverage;

/// Per-series gap ranges (inclusive epoch-ms, `vike_data::DataFusionHist::series_gaps`'s
/// convention), keyed by the same [`SeriesKey`] identity the grid already uses for selection.
/// An absent key (or an empty `Vec`) means "no known gaps" — callers that haven't fetched gaps
/// yet can pass an empty map and get today's (gap-free) behavior unchanged.
pub type GapMap = HashMap<SeriesKey, Vec<(i64, i64)>>;

/// Per-INSTRUMENT cross-kind partial days, keyed by `vike_data::InstrumentKey` — one level ABOVE
/// [`GapMap`]'s per-series identity.
///
/// ⚠ Keyed on the WHOLE `InstrumentKey`, not on `(venue, label)`: that key also carries `grouped`,
/// and a grouped series and a per-symbol one can share a name. Dropping the flag merged exactly what
/// `coverage` deliberately keeps apart — they are different directories with different manifests, and
/// a family mid-migration genuinely has its history in both.
///
/// **Why a second map rather than more entries in [`GapMap`].** They answer different questions and
/// neither derives from the other. `GapMap` says *"this series is missing days 3–4"* — a hole in ONE
/// kind's own timeline. This says *"on day 11 the instrument has trades but no book"* — which is
/// invisible per-series, because each series is perfectly contiguous on its own.
///
/// That distinction is the difference between a complete backfill and a useless one. A Polymarket
/// venue-fill restores the trade tape and nothing else (no venue-direct source serves `book` — see
/// `vike_backfill::caps`), so the trade series closes its gap, the book series never had one, and
/// only the JOIN shows that a market-making backtest over that window would run on no book at all.
///
/// An absent key means "nothing partial" — a caller that has not fetched coverage passes an empty
/// map and gets today's behavior unchanged.
pub type PartialDayMap = BTreeMap<vike_data::InstrumentKey, Vec<vike_data::PartialDay>>;

/// Build a [`PartialDayMap`] from a store's cross-kind coverage report.
///
/// Takes the already-computed report rather than a store handle, so this crate stays DataFusion-free
/// and the function is unit-testable without opening anything.
pub fn partial_days_from_coverage(report: &[vike_data::InstrumentCoverage]) -> PartialDayMap {
    report
        .iter()
        .filter_map(|c| {
            let partial = c.partial_days();
            (!partial.is_empty()).then(|| (c.key.clone(), partial))
        })
        .collect()
}

/// Does this instrument have any day where some kinds have data and others do not?
pub fn has_partial_days(map: &PartialDayMap, key: &vike_data::InstrumentKey) -> bool {
    map.get(key).is_some_and(|v| !v.is_empty())
}

/// A one-line summary of what an instrument is missing — e.g. `"3 partial days (book, quote)"` — for
/// a grid cell or tooltip. Empty string when nothing is partial, so a caller renders it
/// unconditionally without a branch.
pub fn partial_days_label(map: &PartialDayMap, key: &vike_data::InstrumentKey) -> String {
    let Some(days) = map.get(key).filter(|v| !v.is_empty()) else {
        return String::new();
    };
    let mut kinds: BTreeSet<&str> = BTreeSet::new();
    for d in days {
        kinds.extend(d.missing_kinds.iter().map(String::as_str));
    }
    format!(
        "{} partial day{} ({})",
        days.len(),
        if days.len() == 1 { "" } else { "s" },
        kinds.into_iter().collect::<Vec<_>>().join(", ")
    )
}

/// The (venue, symbol, kind, interval) identifying a clicked series row — enough for a caller to
/// load it (open a chart, feed a `SlicePicker`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSelection {
    pub venue: String,
    pub symbol: String,
    pub kind: String,
    pub interval: Option<String>,
}

/// The (venue, symbol, kind, interval) identity of one grid row — `Ord` so it can live in a
/// `BTreeSet` (deterministic iteration, no hashing) as the rich grid's multi-select set. Same
/// four fields as [`StoredSelection`] (kept as a distinct type: a selection SET's element vs. a
/// one-shot click result are different roles, and `stored_catalog_ui`'s signature must not move).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeriesKey {
    pub venue: String,
    pub symbol: String,
    pub kind: String,
    pub interval: Option<String>,
}

/// The identity of one flattened display row, within its venue (a [`FlatRow`] doesn't carry its
/// own venue — it's produced per-venue by [`flatten_venue`]).
fn series_key(venue: &VenueNode, row: &FlatRow) -> SeriesKey {
    SeriesKey {
        venue: venue.venue.clone(),
        symbol: row.symbol.clone(),
        kind: row.kind.clone(),
        interval: row.interval.clone(),
    }
}

/// Which column the grid is currently sorted by. `Symbol` (ascending) is the default so the
/// initial render matches `build_tree`'s existing sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Symbol,
    Kind,
    Coverage,
    Rows,
    Size,
    Updated,
}

/// Persisted (in egui temp memory) sort state: which column, and which direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub column: SortColumn,
    pub ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self { column: SortColumn::Symbol, ascending: true }
    }
}

/// Which left-nav view is active for the rich grid (`stored_catalog_grid`'s `views_sidebar`
/// companion). `All` (the default) shows the whole tree. NOTE: `AssetClass`/`Watchlist` are
/// carried in the type today but a `filter_tree` helper (follow-up) can't yet act on them — the display tree
/// (`VenueNode`/`SymbolNode`/`SeriesRow`) has no asset-class tag, and `views_sidebar` only
/// receives watchlist *names* (`&[String]`), not their symbol membership; wiring either needs a
/// data source this crate doesn't have yet — both currently behave like `All`. `HasGaps` IS wired
/// (dm-gap-viz): `filter_tree` takes a [`GapMap`] and keeps only series with a non-empty gap list
/// (via [`has_gaps`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ViewFilter {
    #[default]
    All,
    Venue(String),
    AssetClass(String),
    HasGaps,
    Stale,
    Watchlist(String),
}

/// Persisted UI state for the rich multi-select grid (`stored_catalog_grid`) — the app-only
/// entry point; `stored_catalog_ui`'s `(query, sort-in-temp-memory)` pair stays as-is for the
/// Studio's simple picker.
#[derive(Debug, Clone, Default)]
pub struct GridState {
    pub query: String,
    pub sort: SortState,
    pub selected: BTreeSet<SeriesKey>,
    pub active_view: ViewFilter,
}

/// One bulk action the grid's selection bar can request. The caller (vike-app) owns what each
/// actually does — the grid only reports which button was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    Backfill,
    Update,
    Delete,
}

/// What happened this frame in `stored_catalog_grid`: at most one row-open (mirrors
/// `stored_catalog_ui`'s return) and/or at most one bulk-bar click.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridResponse {
    pub opened: Option<StoredSelection>,
    pub bulk: Option<BulkAction>,
}

/// The edit buffer behind the **Polymarket proxy** box. `buf` is what the user has typed; seed it
/// with [`proxy_display`] so the box opens showing the value already in force.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyEdit {
    pub buf: String,
}

/// `POLY_SOCKS_PROXY`'s "no proxy" spelling. `vike_polymarket::egress` reads this (and `direct`) as
/// an explicit request for a DIRECT connection, which is why an empty box writes it rather than
/// writing nothing: the resolver's built-in default is proxy-ON at a localhost tunnel, so writing
/// nothing would leave a user who simply is not geo-blocked dialling a tunnel that does not exist.
pub const PROXY_DIRECT: &str = "none";

/// What an empty box means once stored, and what a stored sentinel means back in the box.
///
/// The pair is deliberately not symmetric on whitespace: a box holding only spaces is a user who
/// cleared it, so it stores as [`PROXY_DIRECT`], while `direct` and `none` both come BACK as empty
/// because either spelling means the same thing to the resolver.
pub fn proxy_to_store(typed: &str) -> String {
    let t = typed.trim();
    if t.is_empty() {
        PROXY_DIRECT.to_string()
    } else {
        t.to_string()
    }
}

/// The inverse of [`proxy_to_store`]: render a stored value into the box. `None` (nothing stored)
/// and the direct sentinels show as EMPTY, so "no proxy" looks like no proxy.
pub fn proxy_display(stored: Option<&str>) -> String {
    match stored.map(str::trim) {
        None | Some("") => String::new(),
        Some(v) if v.eq_ignore_ascii_case(PROXY_DIRECT) || v.eq_ignore_ascii_case("direct") => {
            String::new()
        }
        Some(v) => v.to_string(),
    }
}

/// The **Polymarket proxy** row: a label, a single-line box, and a Save button.
///
/// Returns `Some(value)` on the frame Save is clicked — the value already run through
/// [`proxy_to_store`], so the caller writes it to `POLY_SOCKS_PROXY` verbatim. `None` every other
/// frame. Like [`BulkAction`], this reports the click and performs no I/O: the caller (vike-app)
/// owns the credential-store write, because only a binary may touch that store.
///
/// The value is shown PLAINLY, embedded credentials included — a SOCKS URL may carry
/// `user:pass@`, and masking a value the operator is here to read and correct would defeat the box.
pub fn polymarket_proxy_ui(ui: &mut egui::Ui, state: &mut ProxyEdit) -> Option<String> {
    let mut save = None;
    ui.horizontal(|ui| {
        ui.label("Polymarket proxy");
        ui.add(
            egui::TextEdit::singleline(&mut state.buf).hint_text(PROXY_HINT).desired_width(320.0),
        );
        if ui.button("Save").clicked() {
            save = Some(proxy_to_store(&state.buf));
        }
    });
    ui.label(PROXY_HELP);
    save
}

/// The empty box's placeholder — the exact shape a bought proxy is pasted in as.
pub const PROXY_HINT: &str = "socks5h://user:pass@host:1080";

/// The line under the box. Both constraints are load-bearing rather than pedantry, which is why
/// they are stated to the operator instead of left in a module doc:
///
/// - **`socks5h`, not `socks5`.** Plain `socks5` resolves the target's DNS LOCALLY, which is
///   exactly what a DNS-based geo-block catches — the tunnel carries the bytes but the lookup
///   already failed. `socks5h` hands the hostname to the proxy to resolve.
/// - **SOCKS5 only.** `vike_bridge_core::ws_proxy::WsProxy::parse` refuses every other scheme, so a
///   bought HTTP proxy will not work here however valid its credentials.
pub const PROXY_HELP: &str =
    "SOCKS5 only, and socks5h:// (not socks5://) so DNS resolves at the proxy. \
     Empty = direct, no proxy. Takes effect after restart.";

/// One flattened (symbol, series) display row within a venue — the unit the grid sorts and
/// renders. Pure/owned so it can be built and sorted without egui.
#[derive(Debug, Clone, PartialEq)]
struct FlatRow {
    symbol: String,
    kind: String,
    interval: Option<String>,
    cov: SeriesCoverage,
    /// Which layout this row's symbol lives in. Carried because the cross-kind coverage report is
    /// keyed by [`vike_data::InstrumentKey`], and a grouped series and a per-symbol one can share a
    /// label — without this the Partial column would look one of them up under the other's key.
    grouped: bool,
}

/// `"{kind}"` or `"{kind}/{interval}"` — mirrors the pre-dense tree row label.
fn kind_label(row: &FlatRow) -> String {
    match &row.interval {
        Some(iv) => format!("{}/{iv}", row.kind),
        None => row.kind.clone(),
    }
}

/// Format one series' coverage for display, e.g. `"1234 rows, 2024-01-01 → 2024-03-01"`. An
/// empty series (`rows == 0`) reads as `"no data"` rather than a nonsensical epoch-derived range.
/// Mirrors `vike-studio`'s (now-deleted) `data_browser::format_coverage` so the app's Data
/// Manager and the Studio's Data pane render coverage identically. Used today as the Coverage
/// column's hover tooltip.
pub fn coverage_label(cov: &SeriesCoverage) -> String {
    if cov.rows == 0 {
        return "no data".to_string();
    }
    format!(
        "{} rows, {} → {}",
        cov.rows,
        vike_model::time::epoch_ms_to_utc_date(cov.first_ts),
        vike_model::time::epoch_ms_to_utc_date(cov.last_ts)
    )
}

/// Case-insensitive substring match against `venue`/`symbol` (an empty query matches everything).
/// Mirrors `data_browser::matches_query`'s contract (kind/interval aren't searched).
fn matches_query(venue: &str, symbol: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || venue.to_lowercase().contains(&q) || symbol.to_lowercase().contains(&q)
}

/// Flatten one venue's symbols/series into display rows, dropping symbols that don't match
/// `query`. Order is whatever `build_tree` produced (BTreeMap-sorted); callers sort afterwards.
fn flatten_venue(venue: &VenueNode, query: &str) -> Vec<FlatRow> {
    venue
        .symbols
        .iter()
        .filter(|s| matches_query(&venue.venue, &s.symbol, query))
        .flat_map(|s| {
            s.series.iter().map(move |r| FlatRow {
                symbol: s.symbol.clone(),
                kind: r.kind.clone(),
                interval: r.interval.clone(),
                cov: r.cov.clone(),
                grouped: s.grouped,
            })
        })
        .collect()
}

/// Compare two rows by `column`'s primary key, breaking ties by symbol then kind/interval so
/// equal-key rows still land in a stable, deterministic order.
fn cmp_flat_row(a: &FlatRow, b: &FlatRow, column: SortColumn) -> std::cmp::Ordering {
    let primary = match column {
        SortColumn::Symbol => a.symbol.cmp(&b.symbol),
        SortColumn::Kind => kind_label(a).cmp(&kind_label(b)),
        SortColumn::Coverage => a.cov.first_ts.cmp(&b.cov.first_ts),
        SortColumn::Rows => a.cov.rows.cmp(&b.cov.rows),
        SortColumn::Size => a.cov.bytes.cmp(&b.cov.bytes),
        SortColumn::Updated => a.cov.last_ts.cmp(&b.cov.last_ts),
    };
    primary.then_with(|| a.symbol.cmp(&b.symbol)).then_with(|| kind_label(a).cmp(&kind_label(b)))
}

/// Sort `rows` in place per `state` (column + direction). The one pure, unit-tested sort helper
/// the egui render calls per venue group.
fn sort_flat_rows(rows: &mut [FlatRow], state: SortState) {
    rows.sort_by(|a, b| {
        let ord = cmp_flat_row(a, b, state.column);
        if state.ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

/// Tree-wide `(first_ts, last_ts)` window across every venue with at least one series, for the
/// Coverage column's span bars to share one comparable scale. `(0, 0)` (an inert, zero-width
/// window — every bar paints as empty) when nothing has data.
fn global_span(tree: &[VenueNode]) -> (i64, i64) {
    tree.iter().filter(|v| v.total.series > 0).fold((i64::MAX, i64::MIN), |(lo, hi), v| {
        (lo.min(v.total.first_ts), hi.max(v.total.last_ts))
    })
}

/// A series reads as "stale" once its `last_ts` lags more than 35% of the tree-wide span behind
/// the global max — an arbitrary but conservative fraction (recent data is typically within a few
/// percent; anything past a third of the whole window behind is very likely an abandoned/broken
/// feed, not just "yesterday's close").
fn is_stale(last_ts: i64, global_first: i64, global_last: i64) -> bool {
    if global_last <= global_first {
        return false;
    }
    let span = (global_last - global_first) as f64;
    let behind = (global_last - last_ts) as f64;
    behind / span > 0.35
}

/// Rebuild a symbol-level [`RollUp`] from a (possibly filtered) series slice — mirrors
/// `RollUp::add`'s fold (that method is private to `model`, so `filter_tree` needs its own copy;
/// all `RollUp` fields are `pub` so this stays a plain, testable fold).
fn rollup_of_series(series: &[SeriesRow]) -> RollUp {
    let mut r = RollUp::default();
    for (i, s) in series.iter().enumerate() {
        r.rows += s.cov.rows;
        r.bytes += s.cov.bytes;
        r.series += 1;
        r.first_ts = if i == 0 { s.cov.first_ts } else { r.first_ts.min(s.cov.first_ts) };
        r.last_ts = r.last_ts.max(s.cov.last_ts);
    }
    r
}

/// Rebuild a venue-level [`RollUp`] from a (possibly filtered) symbol slice, summing each
/// symbol's already-rebuilt `total`. Same rationale as [`rollup_of_series`].
fn rollup_of_symbols(symbols: &[SymbolNode]) -> RollUp {
    let mut r = RollUp::default();
    for (i, s) in symbols.iter().enumerate() {
        r.rows += s.total.rows;
        r.bytes += s.total.bytes;
        r.series += s.total.series;
        r.first_ts = if i == 0 { s.total.first_ts } else { r.first_ts.min(s.total.first_ts) };
        r.last_ts = r.last_ts.max(s.total.last_ts);
    }
    r
}

/// Whether a series has at least one missing-data gap — the pure predicate the `HasGaps` smart
/// view filters on, and the coverage bar's gap paint uses to skip the overpaint loop entirely
/// for gap-free series. A series absent from the [`GapMap`] (never fetched, or fetch failed) is
/// treated the same as "no gaps" — conservative (never mis-flags a series it has no data for).
pub fn has_gaps(gaps: &[(i64, i64)]) -> bool {
    !gaps.is_empty()
}

/// Keep only `node`'s series that [`has_gaps`], dropping symbols left with none and returning
/// `None` if the whole venue empties out. Mirrors [`filter_stale_venue`]'s shape; rollups are
/// rebuilt over the surviving rows for the same reason.
fn filter_has_gaps_venue(node: &VenueNode, gaps: &GapMap) -> Option<VenueNode> {
    let symbols: Vec<SymbolNode> = node
        .symbols
        .iter()
        .filter_map(|sym| {
            let series: Vec<SeriesRow> = sym
                .series
                .iter()
                .filter(|r| {
                    let key = SeriesKey {
                        venue: node.venue.clone(),
                        symbol: sym.symbol.clone(),
                        kind: r.kind.clone(),
                        interval: r.interval.clone(),
                    };
                    gaps.get(&key).map(|g| has_gaps(g)).unwrap_or(false)
                })
                .cloned()
                .collect();
            if series.is_empty() {
                None
            } else {
                let total = rollup_of_series(&series);
                Some(SymbolNode { symbol: sym.symbol.clone(), grouped: sym.grouped, series, total })
            }
        })
        .collect();
    if symbols.is_empty() {
        return None;
    }
    let total = rollup_of_symbols(&symbols);
    Some(VenueNode { venue: node.venue.clone(), symbols, total })
}

/// Keep only `node`'s stale series (relative to the tree-wide `[global_first, global_last]`
/// window), dropping symbols left with none and returning `None` if the whole venue empties out.
/// Rollups are rebuilt over the surviving rows so the filtered tree's header summaries and
/// Coverage-column span bars stay internally consistent.
fn filter_stale_venue(node: &VenueNode, global_first: i64, global_last: i64) -> Option<VenueNode> {
    let symbols: Vec<SymbolNode> = node
        .symbols
        .iter()
        .filter_map(|sym| {
            let series: Vec<SeriesRow> = sym
                .series
                .iter()
                .filter(|r| is_stale(r.cov.last_ts, global_first, global_last))
                .cloned()
                .collect();
            if series.is_empty() {
                None
            } else {
                let total = rollup_of_series(&series);
                Some(SymbolNode { symbol: sym.symbol.clone(), grouped: sym.grouped, series, total })
            }
        })
        .collect();
    if symbols.is_empty() {
        return None;
    }
    let total = rollup_of_symbols(&symbols);
    Some(VenueNode { venue: node.venue.clone(), symbols, total })
}

/// Apply the active left-nav view ([`views_sidebar`]'s companion) to `tree`, returning a filtered
/// copy. `All` is the identity (a full clone); `Venue(v)` keeps only that venue; `Stale` keeps only
/// series whose coverage lags the tree-wide span (reusing [`is_stale`]), dropping symbols/venues
/// left with nothing. `HasGaps` keeps only series [`has_gaps`] for in `gaps` (a series absent from
/// `gaps`, or fetched with an empty list, is treated as gap-free). `Watchlist`/`AssetClass` are
/// still placeholders: `views_sidebar` only receives watchlist *names* (`&[String]`), not symbol
/// membership, so there's no data here to filter by yet; both currently behave like `All`.
pub fn filter_tree(tree: &[VenueNode], active: &ViewFilter, gaps: &GapMap) -> Vec<VenueNode> {
    match active {
        ViewFilter::All => tree.to_vec(),
        ViewFilter::Venue(v) => tree.iter().filter(|node| &node.venue == v).cloned().collect(),
        ViewFilter::Stale => {
            let (global_first, global_last) = global_span(tree);
            tree.iter()
                .filter_map(|node| filter_stale_venue(node, global_first, global_last))
                .collect()
        }
        ViewFilter::HasGaps => {
            tree.iter().filter_map(|node| filter_has_gaps_venue(node, gaps)).collect()
        }
        // Placeholder: no symbol-membership data available in this crate yet.
        ViewFilter::Watchlist(_) | ViewFilter::AssetClass(_) => tree.to_vec(),
    }
}

/// The left-nav sidebar for the rich grid: **All**, one entry per distinct venue (with its series
/// count), the smart views **Has gaps**/**Stale**, and a **Watchlists** group built from
/// `watchlists`' names. Purely a thin selection UI over [`ViewFilter`] — clicking an entry sets
/// `*active`; the caller re-renders `stored_catalog_grid` (via [`filter_tree`]) off the new value.
pub fn views_sidebar(
    ui: &mut egui::Ui,
    tree: &[VenueNode],
    watchlists: &[String],
    active: &mut ViewFilter,
) {
    ui.label(egui::RichText::new("Views").strong());
    ui.add_space(4.0);
    ui.selectable_value(active, ViewFilter::All, "All");

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Venues").small().weak());
    for v in tree {
        let label = format!("{}  ({})", v.venue, fmt_count(v.total.series as u64));
        ui.selectable_value(active, ViewFilter::Venue(v.venue.clone()), label);
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Smart views").small().weak());
    ui.selectable_value(active, ViewFilter::HasGaps, "\u{26A0} Has gaps");
    ui.selectable_value(active, ViewFilter::Stale, "\u{23F1} Stale");

    if !watchlists.is_empty() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Watchlists").small().weak());
        for w in watchlists {
            ui.selectable_value(active, ViewFilter::Watchlist(w.clone()), w.as_str());
        }
    }
}

// The byte/count formatters live in the shared `vike_ui_theme::fmt` leaf crate now (F35 dedup —
// `vike-app`'s `human_bytes` was a line-for-line copy of `fmt_bytes`). A PRIVATE import: the
// re-export chain that minted `vike_data_manager::{fmt_bytes, fmt_count, fmt_count_compact}` (this
// line's `pub` plus lib.rs's) had no consumer through EITHER hop — every caller outside this crate
// already spells `vike_ui_theme::fmt::…` — so these names now serve only the call sites below.
use vike_ui_theme::fmt::{fmt_bytes, fmt_count, fmt_count_compact};

/// `"430 series · 2.1B rows · 18.4 GB"` — the venue group header's rollup summary.
fn venue_rollup_label(total: &RollUp) -> String {
    format!(
        "{} series · {} rows · {}",
        fmt_count(total.series as u64),
        fmt_count_compact(total.rows),
        fmt_bytes(total.bytes)
    )
}

const ROW_H: f32 = 26.0;
const VENUE_H: f32 = 22.0;
const HEADER_H: f32 = 20.0;

const W_SYMBOL: f32 = 150.0;
const W_KIND: f32 = 120.0;
const W_COVERAGE: f32 = 150.0;
const W_ROWS: f32 = 90.0;
const W_SIZE: f32 = 80.0;
const W_UPDATED: f32 = 100.0;
/// The cross-kind Partial column — a glyph, not text, so it stays narrow; the count and the missing
/// kinds are in the tooltip. Rendered by the rich grid only (`stored_catalog_ui` passes `""`).
const W_PARTIAL: f32 = 26.0;

/// Allocate a fixed-width cell within the current row `ui` and run `add_contents` inside it,
/// right-aligned for numeric columns. Successive calls on the same `ui` (itself laid out
/// left-to-right) advance left-to-right, giving manual, egui-`Grid`-free table columns whose
/// widths line up between the header row and every data row (both built from the same `W_*`
/// constants).
fn cell<R>(
    ui: &mut egui::Ui,
    width: f32,
    align_right: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let height = ui.available_height();
    let layout = if align_right {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(width, height), layout, add_contents).inner
}

/// One clickable column header: label + a ▲/▼ marker on the active sort column. Clicking toggles
/// direction if already active, else selects the column ascending.
fn header_cell(
    ui: &mut egui::Ui,
    width: f32,
    align_right: bool,
    label: &str,
    column: SortColumn,
    sort: &mut SortState,
) {
    let text = if sort.column == column {
        format!("{label} {}", if sort.ascending { "\u{25B2}" } else { "\u{25BC}" })
    } else {
        label.to_string()
    };
    let resp = cell(ui, width, align_right, |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(text).small().strong())
                .sense(egui::Sense::click()),
        )
    });
    if resp.clicked() {
        if sort.column == column {
            sort.ascending = !sort.ascending;
        } else {
            *sort = SortState { column, ascending: true };
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// Map one timestamp range `[t0, t1]` (inclusive epoch-ms, e.g. a gap or a coverage span) to an
/// x-fraction pair over `[global_first, global_last]`, clamped to `[0.0, 1.0]` — the mapping
/// [`paint_coverage_bar`] uses for both its filled span and its gap cut-outs, factored out so the
/// arithmetic is unit-tested without an egui harness. `None` when the window is degenerate
/// (`global_last <= global_first`, e.g. an empty tree).
fn ts_range_to_x_fraction(
    t0: i64,
    t1: i64,
    global_first: i64,
    global_last: i64,
) -> Option<(f32, f32)> {
    if global_last <= global_first {
        return None;
    }
    let span = (global_last - global_first) as f32;
    let f0 = ((t0 - global_first) as f32 / span).clamp(0.0, 1.0);
    let f1 = ((t1 - global_first) as f32 / span).clamp(0.0, 1.0);
    Some((f0, f1))
}

/// Paint the Coverage column's thin span bar: a background track plus a filled segment spanning
/// `[cov.first_ts, cov.last_ts]` mapped over `[global_first, global_last]`, tinted with the
/// warn color when `is_stale`. A zero/empty series paints an empty track (no segment). `gaps`
/// (inclusive epoch-ms ranges, e.g. from `DataFusionHist::series_gaps`) are then overpainted as
/// dark cut-outs within the filled segment — a series with no known gaps (`&[]`) paints exactly
/// as before this feature (empty-gaps behavior is byte-identical).
fn paint_coverage_bar(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    cov: &SeriesCoverage,
    global_first: i64,
    global_last: i64,
    gaps: &[(i64, i64)],
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if cov.rows == 0 || global_last <= global_first {
        return resp;
    }
    let span = (global_last - global_first) as f32;
    let frac0 = ((cov.first_ts - global_first) as f32 / span).clamp(0.0, 1.0);
    let frac1 = ((cov.last_ts - global_first) as f32 / span).clamp(0.0, 1.0);
    let x0 = rect.left() + frac0 * rect.width();
    let x1 = (rect.left() + frac1 * rect.width()).max(x0 + 1.5).min(rect.right());
    let bar = egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
    let stale = is_stale(cov.last_ts, global_first, global_last);
    let color = if stale { ui.visuals().warn_fg_color } else { ui.visuals().selection.bg_fill };
    painter.rect_filled(bar, 1.0, color);

    // Gap cut-outs: overpaint each missing-day sub-range (clamped to the filled bar itself, so a
    // gap can never bleed past the segment it's punching a hole in) in the panel's dark/extreme
    // color, so gaps read as "missing" holes against the filled span.
    if !gaps.is_empty() {
        let gap_color = ui.visuals().extreme_bg_color;
        for &(g0, g1) in gaps {
            if let Some((gf0, gf1)) = ts_range_to_x_fraction(g0, g1, global_first, global_last) {
                let gx0 = (rect.left() + gf0 * rect.width()).max(x0);
                let gx1 = (rect.left() + gf1 * rect.width()).min(x1);
                if gx1 > gx0 {
                    let gap_rect = egui::Rect::from_min_max(
                        egui::pos2(gx0, rect.top()),
                        egui::pos2(gx1, rect.bottom()),
                    );
                    painter.rect_filled(gap_rect, 0.0, gap_color);
                }
            }
        }
    }
    resp
}

/// One thin venue group header: venue name + its rollup summary, tinted with the panel's faint
/// background so it reads as a separator between venues.
fn venue_header_row(ui: &mut egui::Ui, venue: &VenueNode) {
    let desired = egui::vec2(ui.available_width(), VENUE_H);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
        let inner = rect.shrink2(egui::vec2(6.0, 0.0));
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.strong(&venue.venue);
        row_ui.add_space(8.0);
        row_ui.weak(venue_rollup_label(&venue.total));
    }
}

/// Render the six sortable column headers (Symbol/Kind/Coverage/Rows/Size/Updated) onto `ui`
/// (already laid out left-to-right at header height) — shared by `stored_catalog_ui` and
/// `stored_catalog_grid`'s header row. Callers that add their own leading columns (e.g. a
/// checkbox) render those first, on the same `ui`, before calling this.
///
/// `partial` adds the cross-kind Partial column after Coverage. It is NOT sortable (no
/// [`SortColumn`] variant): it is an instrument-level flag, so every row of one instrument carries
/// the same value and sorting by it would only cluster instruments, which grouping already does.
/// The flag must match what the caller passes `data_row_cells`, or header and rows misalign.
fn header_row_cells(ui: &mut egui::Ui, sort: &mut SortState, partial: bool) {
    header_cell(ui, W_SYMBOL, false, "Symbol", SortColumn::Symbol, sort);
    header_cell(ui, W_KIND, false, "Kind", SortColumn::Kind, sort);
    header_cell(ui, W_COVERAGE, false, "Coverage", SortColumn::Coverage, sort);
    if partial {
        let hint = "Partial days: days where some of this instrument's kinds have data and others \
                    do not";
        cell(ui, W_PARTIAL, false, |ui| ui.weak(egui::RichText::new("⚠").small()))
            .on_hover_text(hint);
    }
    header_cell(ui, W_ROWS, true, "Rows", SortColumn::Rows, sort);
    header_cell(ui, W_SIZE, true, "Size", SortColumn::Size, sort);
    header_cell(ui, W_UPDATED, true, "Updated", SortColumn::Updated, sort);
}

/// Render one row's six data cells (Symbol/Kind/Coverage/Rows/Size/Updated) onto `row_ui` (a
/// child `Ui` already positioned over the row's rect, left-to-right layout) — shared by
/// `stored_catalog_ui` and `stored_catalog_grid`'s row loop. Callers that add their own leading
/// columns (e.g. a checkbox) render those first, on the same `row_ui`, before calling this.
/// `gaps` is this row's series' gap ranges (`&[]` for `stored_catalog_ui`, which doesn't thread a
/// [`GapMap`] — its coverage bar stays a plain span, unchanged).
///
/// `partial` is the cross-kind column: `None` omits it entirely (`stored_catalog_ui`, which has no
/// coverage report to key it from), `Some(label)` allocates the cell and marks it when the label is
/// non-empty — see [`partial_days_label`]. It must match the flag passed to `header_row_cells`.
fn data_row_cells(
    row_ui: &mut egui::Ui,
    frow: &FlatRow,
    global_first: i64,
    global_last: i64,
    gaps: &[(i64, i64)],
    partial: Option<&str>,
) {
    cell(row_ui, W_SYMBOL, false, |ui| {
        ui.label(egui::RichText::new(&frow.symbol).monospace().size(12.0));
    });
    cell(row_ui, W_KIND, false, |ui| {
        ui.label(egui::RichText::new(kind_label(frow)).small());
    });
    cell(row_ui, W_COVERAGE, false, |ui| {
        paint_coverage_bar(ui, W_COVERAGE - 8.0, 9.0, &frow.cov, global_first, global_last, gaps)
    })
    .on_hover_text(coverage_label(&frow.cov));
    if let Some(label) = partial {
        // Allocated whether or not this instrument is partial, so every row's later columns line up
        // with the header — a blank cell IS the "nothing missing" rendering.
        let c = cell(row_ui, W_PARTIAL, false, |ui| {
            let glyph = if label.is_empty() { "" } else { "⚠" };
            ui.label(egui::RichText::new(glyph).small().color(ui.visuals().warn_fg_color))
        });
        if !label.is_empty() {
            c.on_hover_text(label);
        }
    }
    cell(row_ui, W_ROWS, true, |ui| {
        ui.label(egui::RichText::new(fmt_count(frow.cov.rows)).monospace().size(11.0));
    });
    cell(row_ui, W_SIZE, true, |ui| {
        ui.label(egui::RichText::new(fmt_bytes(frow.cov.bytes)).monospace().size(11.0));
    });
    cell(row_ui, W_UPDATED, true, |ui| {
        let stale = is_stale(frow.cov.last_ts, global_first, global_last);
        let label = if frow.cov.rows == 0 {
            "—".to_string()
        } else {
            vike_model::time::epoch_ms_to_utc_date(frow.cov.last_ts)
        };
        let mut rt = egui::RichText::new(label).monospace().size(11.0);
        if stale {
            rt = rt.color(ui.visuals().warn_fg_color);
        }
        ui.label(rt);
    });
}

/// Render the dense, sortable venue-grouped stored-catalog grid, filtered by `query`
/// (case-insensitive, venue/symbol only). Returns `Some(StoredSelection)` the frame a row is
/// clicked. One ~22px rollup header per venue, then one ~26px row per (symbol, series) pair:
/// Symbol | Kind | Coverage (span bar) | Rows | Size | Updated. Column headers are clickable to
/// sort (ascending/descending) the rows within each venue group; sort state persists in egui temp
/// memory across frames since this function's signature is a two-mount contract that can't grow a
/// parameter.
pub fn stored_catalog_ui(
    ui: &mut egui::Ui,
    tree: &[VenueNode],
    query: &mut String,
) -> Option<StoredSelection> {
    let mut picked = None;

    ui.horizontal(|ui| {
        ui.label("Search:");
        ui.text_edit_singleline(query).on_hover_text("filter by venue/symbol");
    });
    ui.add_space(4.0);

    if tree.is_empty() {
        ui.weak("No stored data.");
        return None;
    }

    let (global_first, global_last) = global_span(tree);

    let sort_id = ui.id().with("dm_stored_catalog_sort");
    let mut sort: SortState =
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<SortState>(sort_id));

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), HEADER_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| header_row_cells(ui, &mut sort, false),
    );
    ui.separator();

    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<SortState>(sort_id) = sort);

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("stored_catalog_grid").show(
        ui,
        |ui| {
            for venue_node in tree {
                let mut rows = flatten_venue(venue_node, query);
                if rows.is_empty() {
                    continue;
                }
                sort_flat_rows(&mut rows, sort);

                venue_header_row(ui, venue_node);

                for frow in &rows {
                    let desired = egui::vec2(ui.available_width(), ROW_H);
                    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                0.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let mut row_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        data_row_cells(&mut row_ui, frow, global_first, global_last, &[], None);
                    }
                    if response.clicked() {
                        picked = Some(StoredSelection {
                            venue: venue_node.venue.clone(),
                            symbol: frow.symbol.clone(),
                            kind: frow.kind.clone(),
                            interval: frow.interval.clone(),
                        });
                    }
                }
            }
        },
    );

    picked
}

const W_CHECK: f32 = 22.0;

/// Insert (`want: true`) or remove (`want: false`) `key` in `selected` — the one mutation both
/// the per-row checkbox and the header "select all" apply, factored out so it's testable without
/// an egui harness.
fn set_selected(selected: &mut BTreeSet<SeriesKey>, key: SeriesKey, want: bool) {
    if want {
        selected.insert(key);
    } else {
        selected.remove(&key);
    }
}

/// The [`SeriesKey`]s `stored_catalog_grid` currently renders for `query` over `tree` — the exact
/// set "select all (filtered)" toggles. `tree` is expected to already be view-filtered by the
/// caller (mirrors `stored_catalog_grid`'s own `filter_tree` → `flatten_venue` pipeline) so this
/// helper only re-does the query filter, kept pure/testable.
fn visible_keys(tree: &[VenueNode], query: &str) -> Vec<SeriesKey> {
    tree.iter()
        .flat_map(|v| flatten_venue(v, query).into_iter().map(move |r| series_key(v, &r)))
        .collect()
}

/// Apply `want` to every key in `keys` — the header checkbox's "select all (filtered)" / "clear
/// all (filtered)" action, factored out for the same reason as [`set_selected`].
fn apply_select_all(selected: &mut BTreeSet<SeriesKey>, keys: &[SeriesKey], want: bool) {
    for k in keys {
        set_selected(selected, k.clone(), want);
    }
}

/// The bulk-action bar shown above the grid whenever the selection is non-empty: a "N selected"
/// count and Backfill/Update/Delete buttons. Returns the clicked action (if any) plus each
/// button's screen rect — the rects exist purely so `tests::bulk_bar_*` can drive a real pointer
/// click at a known position rather than guessing pixel offsets from text/theme metrics.
///
/// `delete_disabled`: `Some(reason)` renders Delete GRAYED OUT with `reason` as its hover text —
/// the caller's mode says delete cannot act here (e.g. the grid shows a REMOTE store and delete
/// is a local-store operation) — and a click on it yields no action. `None` = today's live
/// button. Backfill/Update take no such knob: they exist in every mode (the wire backfill verb
/// covers the remote one).
fn bulk_action_bar(
    ui: &mut egui::Ui,
    selected_count: usize,
    delete_disabled: Option<&str>,
) -> (Option<BulkAction>, [egui::Rect; 3]) {
    let mut action = None;
    let mut rects = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        let bh = 20.0;
        let r = ui.add_sized([70.0, bh], egui::Button::new("Backfill"));
        rects[0] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Backfill);
        }
        let r = ui.add_sized([60.0, bh], egui::Button::new("Update"));
        rects[1] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Update);
        }
        let mut r = ui
            .add_enabled_ui(delete_disabled.is_none(), |ui| {
                ui.add_sized([60.0, bh], egui::Button::new("Delete"))
            })
            .inner;
        if let Some(reason) = delete_disabled {
            r = r.on_disabled_hover_text(reason);
        }
        rects[2] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Delete);
        }
        ui.add_space(8.0);
        ui.label(format!("{selected_count} selected"));
    });
    (action, rects)
}

/// The rich, app-only entry point over the same tree/render machinery as `stored_catalog_ui`:
/// adds a leading checkbox column (toggles `state.selected`), a header "select all (filtered)"
/// checkbox, and a bulk-action bar (Backfill/Update/Delete) shown once anything is selected.
/// `stored_catalog_ui`'s signature is untouched; this is an additive second entry point, not a
/// replacement (the Studio keeps using the simple picker). `state.active_view` is applied via
/// [`filter_tree`] before anything below renders — the caller (typically paired with
/// [`views_sidebar`] over the same `active_view`) passes the *full* tree; this function does its
/// own view filtering, same as it already does its own query filtering. `gaps` is the per-series
/// [`GapMap`] (an empty map = no known gaps anywhere, today's pre-gap-viz behavior): it feeds both
/// the `HasGaps` smart view (via `filter_tree`) and each row's coverage-bar gap cut-outs.
///
/// `partials` is its cross-kind sibling ([`partial_days_from_coverage`] over the store's
/// `coverage_report`): the gap map answers "is this SERIES missing days", `partials` answers "does
/// this INSTRUMENT have days where some kinds are present and others are not" — which a per-series
/// view structurally cannot show, since each series is contiguous on its own. An empty map renders
/// exactly as before, minus nothing: the column is allocated but every cell is blank.
///
/// `delete_disabled` is the bulk bar's Delete gate (see [`bulk_action_bar`]): `Some(reason)` grays
/// the button with `reason` as hover text — the honest rendering when the grid shows a store
/// delete cannot act on (a REMOTE store; delete is a local-store operation) — `None` is today's
/// live button.
pub fn stored_catalog_grid(
    ui: &mut egui::Ui,
    tree: &[VenueNode],
    state: &mut GridState,
    gaps: &GapMap,
    partials: &PartialDayMap,
    delete_disabled: Option<&str>,
) -> GridResponse {
    let mut resp = GridResponse::default();
    let view_filtered = filter_tree(tree, &state.active_view, gaps);
    let tree: &[VenueNode] = &view_filtered;

    ui.horizontal(|ui| {
        ui.label("Search:");
        ui.text_edit_singleline(&mut state.query).on_hover_text("filter by venue/symbol");
    });
    ui.add_space(4.0);

    if tree.is_empty() {
        ui.weak("No stored data.");
        return resp;
    }

    let (global_first, global_last) = global_span(tree);

    // Every (venue, rows) pair this frame shows — computed once (unsorted; sorted below once the
    // header has had a chance to update `state.sort` this frame), shared by the select-all
    // checkbox (needs the full filtered+queried key set) and the row loop.
    let mut by_venue: Vec<(&VenueNode, Vec<FlatRow>)> = Vec::new();
    for v in tree {
        let rows = flatten_venue(v, &state.query);
        if rows.is_empty() {
            continue;
        }
        by_venue.push((v, rows));
    }
    let all_keys: Vec<SeriesKey> = visible_keys(tree, &state.query);

    if !state.selected.is_empty() {
        let (action, _rects) = bulk_action_bar(ui, state.selected.len(), delete_disabled);
        resp.bulk = action;
        ui.add_space(4.0);
    }

    let all_selected = !all_keys.is_empty() && all_keys.iter().all(|k| state.selected.contains(k));
    ui.horizontal(|ui| {
        let mut checked = all_selected;
        if ui.checkbox(&mut checked, "select all (filtered)").changed() {
            apply_select_all(&mut state.selected, &all_keys, checked);
        }
    });
    ui.add_space(2.0);

    if by_venue.is_empty() {
        ui.weak("No matching series.");
        return resp;
    }

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), HEADER_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            cell(ui, W_CHECK, false, |_ui| {});
            header_row_cells(ui, &mut state.sort, true);
        },
    );
    ui.separator();

    // Sort now, using whatever `state.sort` is after the header above may have just changed it
    // this frame (mirrors `stored_catalog_ui`'s flatten-then-sort-after-header order, minus the
    // temp-memory round trip since sort lives in `state` directly here).
    for (_, rows) in &mut by_venue {
        sort_flat_rows(rows, state.sort);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("stored_catalog_grid_rich")
        .show(ui, |ui| {
            for (venue_node, rows) in &by_venue {
                venue_header_row(ui, venue_node);

                for frow in rows {
                    let key = series_key(venue_node, frow);
                    let desired = egui::vec2(ui.available_width(), ROW_H);
                    let (row_rect, _bg) = ui.allocate_exact_size(desired, egui::Sense::hover());
                    if !ui.is_rect_visible(row_rect) {
                        continue;
                    }
                    let check_rect =
                        egui::Rect::from_min_size(row_rect.min, egui::vec2(W_CHECK, ROW_H));
                    let rest_rect = egui::Rect::from_min_max(
                        egui::pos2(row_rect.min.x + W_CHECK, row_rect.min.y),
                        row_rect.max,
                    );
                    // A dedicated interact zone over everything BUT the checkbox column, so
                    // clicking the checkbox toggles selection only — it never also fires an
                    // "open" (the checkbox is its own nested `Sense::click` widget; overlapping
                    // it with this row's click sense would double-fire on every checkbox click).
                    let open_resp = ui.interact(
                        rest_rect,
                        ui.id().with(("dm_grid_row_open", &key)),
                        egui::Sense::click(),
                    );
                    if open_resp.hovered() {
                        ui.painter().rect_filled(
                            row_rect,
                            0.0,
                            ui.visuals().widgets.hovered.weak_bg_fill,
                        );
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    let mut check_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(check_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let mut checked = state.selected.contains(&key);
                    if check_ui.checkbox(&mut checked, "").changed() {
                        set_selected(&mut state.selected, key.clone(), checked);
                    }

                    let mut row_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rest_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let row_gaps: &[(i64, i64)] =
                        gaps.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                    // Instrument-level, so it repeats down an instrument's rows — deliberate: the
                    // warning is about the SET of kinds, and any one of them is where you notice it.
                    // Computed per VISIBLE row only (the `is_rect_visible` continue above).
                    let partial = partial_days_label(
                        partials,
                        &vike_data::InstrumentKey {
                            venue: venue_node.venue.clone(),
                            label: frow.symbol.clone(),
                            grouped: frow.grouped,
                        },
                    );
                    data_row_cells(
                        &mut row_ui,
                        frow,
                        global_first,
                        global_last,
                        row_gaps,
                        Some(&partial),
                    );

                    if open_resp.clicked() {
                        resp.opened = Some(StoredSelection {
                            venue: venue_node.venue.clone(),
                            symbol: frow.symbol.clone(),
                            kind: frow.kind.clone(),
                            interval: frow.interval.clone(),
                        });
                    }
                }
            }
        });

    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::SeriesCoverage;

    /// One headless frame whose texture uploads are deliberately discarded, and whose geometry is
    /// asserted sane.
    ///
    /// egui 0.36 made `TexturesDelta` PANIC on drop while it still holds unapplied deltas —
    /// "Deltas need to be handled. If you want to drop this intentionally call `clear` before
    /// dropping." Every harness below renders into no backend at all: they run a frame to read a
    /// widget rect, an action, or the emitted shapes. Discarding the uploads IS the intent, so it
    /// is stated once here rather than at eight call sites.
    ///
    /// Every frame also goes through `vike_ui_theme::frame_sanity::assert_frame_sane` — the ONE
    /// shared geometry invariant, behind that crate's `test-support` feature. It is stated here
    /// (once) for the same reason the clear is: every test that renders a frame gets it, including
    /// the ones added after this comment.
    ///
    /// ⚠ The assertion runs AFTER the clear, and the order is load-bearing. `clear` touches
    /// `textures_delta` and never `shapes`, so it cannot hide anything the assertion reads — while
    /// asserting first leaves the deltas unapplied, so dropping the frame during the assertion's
    /// unwind panics a SECOND time in the destructor and the process ABORTS with `SIGABRT`,
    /// printing a backtrace instead of the coordinate that was wrong.
    fn run_frame(
        ctx: &egui::Context,
        raw: egui::RawInput,
        f: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut out = ctx.run_ui(raw, f);
        out.textures_delta.clear();
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
        out
    }

    fn cov(rows: u64, bytes: u64, first: i64, last: i64) -> SeriesCoverage {
        SeriesCoverage { first_ts: first, last_ts: last, rows, bytes, parts: 1, dates: 1 }
    }

    fn row(symbol: &str, kind: &str, iv: Option<&str>, c: SeriesCoverage) -> FlatRow {
        FlatRow {
            symbol: symbol.into(),
            kind: kind.into(),
            interval: iv.map(Into::into),
            cov: c,
            grouped: false,
        }
    }

    #[test]
    fn coverage_label_shows_rows_and_span() {
        let cov = SeriesCoverage {
            first_ts: 0,
            last_ts: 86_400_000,
            rows: 1234,
            bytes: 0,
            parts: 1,
            dates: 2,
        };
        let s = coverage_label(&cov);
        assert!(s.contains("1234") || s.contains("1,234"));
        assert!(s.contains("1970")); // epoch-derived date rendered
        assert!(!s.contains("panicked"));
    }

    #[test]
    fn coverage_label_zero_rows_is_safe() {
        let cov = SeriesCoverage { first_ts: 0, last_ts: 0, rows: 0, bytes: 0, parts: 0, dates: 0 };
        let _ = coverage_label(&cov); // must not panic on an empty series
    }

    #[test]
    fn fmt_bytes_scales_units() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(1_572_864), "1.5 MB"); // 1.5 * 1024 * 1024
        assert_eq!(fmt_bytes(1024u64.pow(3) * 2), "2.0 GB");
    }

    #[test]
    fn fmt_count_adds_thousands_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(7), "7");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    #[test]
    fn fmt_count_compact_scales_to_k_m_b() {
        assert_eq!(fmt_count_compact(500), "500");
        assert_eq!(fmt_count_compact(2_145_000_000), "2.1B");
        assert_eq!(fmt_count_compact(430_000), "430.0K");
    }

    #[test]
    fn is_stale_flags_far_behind_global_max() {
        // global window is 0..1000; a series ending at 100 is 90% behind -> stale.
        assert!(is_stale(100, 0, 1000));
        // a series ending at 950 is only 5% behind -> not stale.
        assert!(!is_stale(950, 0, 1000));
        // degenerate window (no span) never flags stale.
        assert!(!is_stale(0, 0, 0));
    }

    #[test]
    fn sort_by_rows_desc_orders_largest_first() {
        let mut rows = vec![
            row("AAA", "bar", Some("1m"), cov(100, 10, 0, 10)),
            row("BBB", "bar", Some("1m"), cov(300, 10, 0, 10)),
            row("CCC", "bar", Some("1m"), cov(200, 10, 0, 10)),
        ];
        sort_flat_rows(&mut rows, SortState { column: SortColumn::Rows, ascending: false });
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
    }

    #[test]
    fn sort_by_symbol_asc_orders_alphabetically() {
        let mut rows = vec![
            row("ZZZ", "bar", None, cov(1, 1, 0, 1)),
            row("AAA", "bar", None, cov(1, 1, 0, 1)),
            row("MMM", "bar", None, cov(1, 1, 0, 1)),
        ];
        sort_flat_rows(&mut rows, SortState { column: SortColumn::Symbol, ascending: true });
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["AAA", "MMM", "ZZZ"]);
    }

    #[test]
    fn sort_by_updated_desc_orders_most_recent_first() {
        let mut rows = vec![
            row("AAA", "bar", Some("1m"), cov(1, 1, 0, 500)),
            row("BBB", "bar", Some("1m"), cov(1, 1, 0, 9000)),
            row("CCC", "bar", Some("1m"), cov(1, 1, 0, 4000)),
        ];
        sort_flat_rows(&mut rows, SortState { column: SortColumn::Updated, ascending: false });
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
    }

    #[test]
    fn sort_ties_break_deterministically_by_symbol_then_kind() {
        let mut rows = vec![
            row("AAA", "trade", None, cov(50, 1, 0, 1)),
            row("AAA", "bar", Some("1m"), cov(50, 1, 0, 1)),
        ];
        sort_flat_rows(&mut rows, SortState { column: SortColumn::Rows, ascending: true });
        // equal Rows -> falls back to symbol (equal) then kind_label ("bar/1m" < "trade").
        assert_eq!(rows[0].kind, "bar");
        assert_eq!(rows[1].kind, "trade");
    }

    #[test]
    fn flatten_venue_filters_by_query_and_flattens_series() {
        use crate::model::{RollUp, SeriesRow, SymbolNode};
        let venue = VenueNode {
            venue: "binance".into(),
            symbols: vec![
                SymbolNode {
                    symbol: "BTCUSDT".into(),
                    grouped: false,
                    series: vec![
                        SeriesRow {
                            kind: "bar".into(),
                            interval: Some("1m".into()),
                            cov: cov(100, 10, 0, 10),
                        },
                        SeriesRow { kind: "trade".into(), interval: None, cov: cov(50, 5, 0, 10) },
                    ],
                    total: RollUp::default(),
                },
                SymbolNode {
                    symbol: "ETHUSDT".into(),
                    grouped: false,
                    series: vec![],
                    total: RollUp::default(),
                },
            ],
            total: RollUp::default(),
        };
        let all = flatten_venue(&venue, "");
        assert_eq!(all.len(), 2); // BTCUSDT's two series; ETHUSDT has none

        let filtered = flatten_venue(&venue, "eth");
        assert!(filtered.is_empty()); // ETHUSDT matches the query but has no series rows

        let filtered = flatten_venue(&venue, "btc");
        assert_eq!(filtered.len(), 2);
    }

    fn venue_fixture(venue: &str, symbols: &[(&str, u64, i64, i64)]) -> VenueNode {
        use crate::model::{RollUp, SeriesRow, SymbolNode};
        let symbols = symbols
            .iter()
            .map(|(sym, rows, first, last)| SymbolNode {
                symbol: (*sym).to_string(),
                grouped: false,
                series: vec![SeriesRow {
                    kind: "bar".into(),
                    interval: Some("1m".into()),
                    cov: cov(*rows, 1, *first, *last),
                }],
                total: RollUp::default(),
            })
            .collect();
        VenueNode { venue: venue.to_string(), symbols, total: RollUp::default() }
    }

    fn key(venue: &str, symbol: &str) -> SeriesKey {
        SeriesKey {
            venue: venue.into(),
            symbol: symbol.into(),
            kind: "bar".into(),
            interval: Some("1m".into()),
        }
    }

    // ============================ GridState selection (commit 1) ============================

    #[test]
    fn set_selected_inserts_and_removes() {
        let mut selected = BTreeSet::new();
        let k = key("binance", "BTCUSDT");

        set_selected(&mut selected, k.clone(), true);
        assert!(selected.contains(&k));

        set_selected(&mut selected, k.clone(), false);
        assert!(!selected.contains(&k), "toggling off must remove the key");
    }

    #[test]
    fn set_selected_is_idempotent() {
        let mut selected = BTreeSet::new();
        let k = key("okx", "BTC-USDT");
        set_selected(&mut selected, k.clone(), true);
        set_selected(&mut selected, k.clone(), true);
        assert_eq!(selected.len(), 1, "inserting the same key twice must not duplicate it");
    }

    #[test]
    fn select_all_over_filtered_set_only_selects_matching_rows() {
        let tree = vec![
            venue_fixture("binance", &[("BTCUSDT", 10, 0, 10), ("ETHUSDT", 5, 0, 10)]),
            venue_fixture("okx", &[("BTC-USDT", 7, 0, 10)]),
        ];

        // "btc" matches BTCUSDT (binance) and BTC-USDT (okx), not ETHUSDT.
        let keys = visible_keys(&tree, "btc");
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|k| k.symbol.to_lowercase().contains("btc")));

        let mut selected = BTreeSet::new();
        apply_select_all(&mut selected, &keys, true);
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&key("binance", "BTCUSDT")));
        assert!(selected.contains(&key("okx", "BTC-USDT")));
        assert!(!selected.contains(&key("binance", "ETHUSDT")));

        // narrowing the query and re-running select-all(false) only clears what's now visible.
        apply_select_all(&mut selected, &visible_keys(&tree, "binance"), false);
        assert!(!selected.contains(&key("binance", "BTCUSDT")));
        assert!(
            selected.contains(&key("okx", "BTC-USDT")),
            "clearing a narrower filtered set must not touch keys outside it"
        );
    }

    #[test]
    fn visible_keys_empty_query_covers_every_row() {
        let tree = vec![venue_fixture("binance", &[("BTCUSDT", 10, 0, 10), ("ETHUSDT", 5, 0, 10)])];
        assert_eq!(visible_keys(&tree, "").len(), 2);
    }

    // ============================ bulk action bar (commit 1) ============================
    // Real egui interaction test: renders `bulk_action_bar`, reads back the Delete button's
    // actual screen rect from a probe frame, then drives a real pointer press+release over it —
    // proving the click really does resolve to `BulkAction::Delete`, not just that the enum
    // exists.

    fn bulk_bar_screen() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0))
    }

    #[test]
    fn bulk_bar_delete_button_click_sets_delete_action() {
        let ctx = egui::Context::default();
        let screen = bulk_bar_screen();

        // Frame 0: probe — discover the Delete button's rect (no pointer interaction).
        let mut rects = [egui::Rect::NOTHING; 3];
        let raw =
            egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
        let _ = run_frame(&ctx, raw, |ui| {
            let (_, r) = bulk_action_bar(ui, 3, None);
            rects = r;
        });
        let delete_pos = rects[2].center();

        // Frame 1: move onto + press the Delete button.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(1.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerMoved(delete_pos));
        raw.events.push(egui::Event::PointerButton {
            pos: delete_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = run_frame(&ctx, raw, |ui| {
            let _ = bulk_action_bar(ui, 3, None);
        });

        // Frame 2: release over the same position -> a real click.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(2.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerButton {
            pos: delete_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut action = None;
        let _ = run_frame(&ctx, raw, |ui| {
            let (a, _) = bulk_action_bar(ui, 3, None);
            action = a;
        });

        assert_eq!(action, Some(BulkAction::Delete));
    }

    /// The remote-mode gate (the #1378 seam close): with `delete_disabled = Some(reason)` the
    /// Delete button is GRAYED, so the same press+release that resolves to `BulkAction::Delete`
    /// above must yield NO action — a delete on a remote grid would act on the local store the
    /// grid is not showing, and a clickable button that silently no-ops is the dishonest variant.
    #[test]
    fn bulk_bar_disabled_delete_click_yields_no_action() {
        const REASON: Option<&str> = Some("delete is a local-store operation");
        let ctx = egui::Context::default();
        let screen = bulk_bar_screen();

        // Frame 0: probe — the disabled button still owns a rect to aim the click at.
        let mut rects = [egui::Rect::NOTHING; 3];
        let raw =
            egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
        let _ = run_frame(&ctx, raw, |ui| {
            let (_, r) = bulk_action_bar(ui, 3, REASON);
            rects = r;
        });
        let delete_pos = rects[2].center();
        assert!(rects[2] != egui::Rect::NOTHING, "the grayed Delete button still renders");

        // Frame 1: move onto + press the Delete button.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(1.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerMoved(delete_pos));
        raw.events.push(egui::Event::PointerButton {
            pos: delete_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = run_frame(&ctx, raw, |ui| {
            let _ = bulk_action_bar(ui, 3, REASON);
        });

        // Frame 2: release over the same position — the click that must NOT resolve.
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(2.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerButton {
            pos: delete_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut action = None;
        let _ = run_frame(&ctx, raw, |ui| {
            let (a, _) = bulk_action_bar(ui, 3, REASON);
            action = a;
        });

        assert_eq!(action, None, "a disabled Delete must not produce an action");
    }

    #[test]
    fn bulk_bar_backfill_button_click_sets_backfill_action() {
        let ctx = egui::Context::default();
        let screen = bulk_bar_screen();

        let mut rects = [egui::Rect::NOTHING; 3];
        let raw =
            egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
        let _ = run_frame(&ctx, raw, |ui| {
            let (_, r) = bulk_action_bar(ui, 1, None);
            rects = r;
        });
        let backfill_pos = rects[0].center();

        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(1.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerMoved(backfill_pos));
        raw.events.push(egui::Event::PointerButton {
            pos: backfill_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = run_frame(&ctx, raw, |ui| {
            let _ = bulk_action_bar(ui, 1, None);
        });

        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(2.0 / 60.0),
            ..Default::default()
        };
        raw.events.push(egui::Event::PointerButton {
            pos: backfill_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut action = None;
        let _ = run_frame(&ctx, raw, |ui| {
            let (a, _) = bulk_action_bar(ui, 1, None);
            action = a;
        });

        assert_eq!(action, Some(BulkAction::Backfill));
    }

    #[test]
    fn bulk_bar_no_click_reports_no_action() {
        let ctx = egui::Context::default();
        let screen = bulk_bar_screen();
        let mut action = None;
        for f in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(f as f64 / 60.0),
                ..Default::default()
            };
            let _ = run_frame(&ctx, raw, |ui| {
                let (a, _) = bulk_action_bar(ui, 2, None);
                action = a;
            });
        }
        assert_eq!(action, None);
    }

    // ============================ filter_tree / ViewFilter (commit 1) ============================

    /// A tree built via `build_tree` (real rollups, not the zero-filled `venue_fixture` totals
    /// above) so `global_span`/`is_stale` behave exactly as they do in production. Whole-tree span
    /// is `[0, 1000]` (from okx's `BTC-USDT` at 1000): binance/FRESH ends at 990 (1% behind, not
    /// stale), binance/STALE ends at 100 (90% behind, stale), okx/BTC-USDT ends at 1000 (not
    /// stale).
    fn tree_fixture() -> Vec<VenueNode> {
        use vike_data::SeriesId;
        let sid = |venue: &str, symbol: &str| {
            SeriesId::per_symbol("bar", venue, symbol, Some("1m".into()))
        };
        let inv = vec![
            (sid("binance", "FRESH"), cov(10, 1, 0, 990)),
            (sid("binance", "STALE"), cov(10, 1, 0, 100)),
            (sid("okx", "BTC-USDT"), cov(5, 1, 0, 1000)),
        ];
        crate::model::build_tree(inv)
    }

    // ======================= the cross-kind Partial column (dm-partial-cell) =====================
    //
    // Real egui frames, same shape as the `bulk_bar_*` interaction tests above: render the actual
    // grid and read the glyphs back out of the frame's shapes, so these prove the marker REACHES
    // THE SCREEN — not merely that `partial_days_label` returns a string.

    /// Every piece of text painted in one frame of `stored_catalog_grid` over `tree`/`partials`.
    fn grid_frame_text(tree: &[VenueNode], partials: &PartialDayMap) -> Vec<String> {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
        let raw =
            egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
        let mut state = GridState::default();
        let out = run_frame(&ctx, raw, |ui| {
            let _ = stored_catalog_grid(ui, tree, &mut state, &GapMap::new(), partials, None);
        });
        out.shapes
            .into_iter()
            .filter_map(|cs| match cs.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    fn partial_fixture(venue: &str, symbol: &str, missing: &[&str]) -> PartialDayMap {
        let key =
            vike_data::InstrumentKey { venue: venue.into(), label: symbol.into(), grouped: false };
        let day = vike_data::PartialDay {
            day: 20_666, // ~2026-08-01; the column shows a COUNT, so the exact day is irrelevant
            missing_kinds: missing.iter().map(|s| s.to_string()).collect(),
        };
        PartialDayMap::from([(key, vec![day])])
    }

    /// The column header exists whether or not anything is partial — it is the affordance that
    /// tells you the check was RUN, which a blank column alone would not.
    #[test]
    fn partial_column_header_renders_with_an_empty_map() {
        let text = grid_frame_text(&tree_fixture(), &PartialDayMap::new());
        assert!(text.iter().any(|t| t == "⚠"), "header glyph missing: {text:?}");
        assert!(text.iter().any(|t| t == "Coverage"), "grid did not render at all: {text:?}");
    }

    /// An empty map paints the header glyph and NOTHING else — one ⚠ total, no row markers.
    #[test]
    fn no_partial_days_marks_no_rows() {
        let text = grid_frame_text(&tree_fixture(), &PartialDayMap::new());
        assert_eq!(text.iter().filter(|t| *t == "⚠").count(), 1, "{text:?}");
    }

    /// **The point of the column.** `binance/FRESH` has one partial day, so its row is marked —
    /// and only its row: `STALE` and okx's `BTC-USDT` are untouched. The fixture's symbols each
    /// have ONE series, so one marked instrument is exactly one marked row (plus the header).
    #[test]
    fn a_partial_instrument_marks_its_row_and_no_other() {
        let partials = partial_fixture("binance", "FRESH", &["quote"]);
        let text = grid_frame_text(&tree_fixture(), &partials);
        assert_eq!(
            text.iter().filter(|t| *t == "⚠").count(),
            2,
            "expected header + exactly one row marker: {text:?}"
        );
    }

    /// **The distinction #1009 preserved, now load-bearing at the pixel level.** A grouped series
    /// and a per-symbol one can share a label; the map here is keyed `grouped: false`, so a tree of
    /// GROUPED series must not match it. Keying on `(venue, label)` alone would mark this row.
    #[test]
    fn a_grouped_row_does_not_match_a_per_symbol_key() {
        use vike_data::SeriesId;
        let tree = crate::model::build_tree(vec![(
            SeriesId::grouped("trade", "binance", "FRESH"),
            cov(10, 1, 0, 990),
        )]);
        let text = grid_frame_text(&tree, &partial_fixture("binance", "FRESH", &["quote"]));
        assert_eq!(
            text.iter().filter(|t| *t == "⚠").count(),
            1,
            "a grouped row matched a per-symbol partial key: {text:?}"
        );
    }

    #[test]
    fn filter_tree_all_keeps_everything() {
        let tree = tree_fixture();
        let filtered = filter_tree(&tree, &ViewFilter::All, &GapMap::new());
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].symbols.len() + filtered[1].symbols.len(), 3);
    }

    #[test]
    fn filter_tree_venue_keeps_only_that_venue() {
        let tree = tree_fixture();
        let filtered =
            filter_tree(&tree, &ViewFilter::Venue("binance".to_string()), &GapMap::new());
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].venue, "binance");
        assert_eq!(filtered[0].symbols.len(), 2);
    }

    #[test]
    fn filter_tree_stale_keeps_only_stale_rows() {
        let tree = tree_fixture();
        let filtered = filter_tree(&tree, &ViewFilter::Stale, &GapMap::new());
        // Only binance/STALE lags more than 35% of the tree-wide span behind the global max;
        // binance/FRESH and okx/BTC-USDT both drop out, and okx's venue disappears entirely.
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].venue, "binance");
        assert_eq!(filtered[0].symbols.len(), 1);
        assert_eq!(filtered[0].symbols[0].symbol, "STALE");
    }

    #[test]
    fn filter_tree_watchlist_and_asset_class_are_still_placeholders() {
        let tree = tree_fixture();
        let gaps = GapMap::new();
        assert_eq!(filter_tree(&tree, &ViewFilter::Watchlist("crypto".into()), &gaps).len(), 2);
        assert_eq!(filter_tree(&tree, &ViewFilter::AssetClass("spot".into()), &gaps).len(), 2);
    }

    // ============================ gap viz (dm-gap-viz) ============================

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
        let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &GapMap::new());
        assert!(filtered.is_empty(), "an empty GapMap must not accidentally keep every row");
    }

    #[test]
    fn filter_tree_has_gaps_keeps_only_gapped_series() {
        let tree = tree_fixture();
        let mut gaps = GapMap::new();
        // binance/FRESH has a recorded gap; binance/STALE and okx/BTC-USDT do not (absent from
        // the map at all, mirroring "never fetched" / "no gaps found").
        gaps.insert(
            SeriesKey {
                venue: "binance".into(),
                symbol: "FRESH".into(),
                kind: "bar".into(),
                interval: Some("1m".into()),
            },
            vec![(10, 20)],
        );
        let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &gaps);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].venue, "binance");
        assert_eq!(filtered[0].symbols.len(), 1);
        assert_eq!(filtered[0].symbols[0].symbol, "FRESH");
    }

    #[test]
    fn filter_tree_has_gaps_ignores_series_with_empty_gap_list() {
        let tree = tree_fixture();
        let mut gaps = GapMap::new();
        // Present in the map but with an empty Vec -> same as "no gaps" (`has_gaps` is false).
        gaps.insert(
            SeriesKey {
                venue: "binance".into(),
                symbol: "FRESH".into(),
                kind: "bar".into(),
                interval: Some("1m".into()),
            },
            vec![],
        );
        let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &gaps);
        assert!(filtered.is_empty());
    }

    #[test]
    fn ts_range_to_x_fraction_maps_within_window() {
        let (f0, f1) = ts_range_to_x_fraction(200, 300, 0, 1000).unwrap();
        assert!((f0 - 0.2).abs() < 1e-6);
        assert!((f1 - 0.3).abs() < 1e-6);
    }

    #[test]
    fn ts_range_to_x_fraction_clamps_outside_window() {
        let (f0, f1) = ts_range_to_x_fraction(-500, 2000, 0, 1000).unwrap();
        assert_eq!(f0, 0.0);
        assert_eq!(f1, 1.0);
    }

    #[test]
    fn ts_range_to_x_fraction_degenerate_window_is_none() {
        assert_eq!(ts_range_to_x_fraction(0, 10, 0, 0), None);
    }
}

#[cfg(test)]
mod partial_day_tests {
    use super::*;
    use vike_data::coverage::join_coverage;
    use vike_data::SeriesId;

    fn cov(
        venue: &str,
        sym: &str,
        rows: &[(&str, Vec<i64>)],
    ) -> Vec<vike_data::InstrumentCoverage> {
        join_coverage(
            &rows
                .iter()
                .map(|(k, d)| (SeriesId::per_symbol(*k, venue, sym, None), d.clone()))
                .collect::<Vec<_>>(),
        )
    }

    /// **The case per-series gaps cannot show.** A Polymarket venue-fill restores the trade tape and
    /// nothing else, so trades are contiguous and the book series simply lacks that day — neither
    /// series looks wrong on its own, and `GapMap` reports nothing.
    #[test]
    fn a_trades_only_day_is_partial_even_though_no_series_has_a_gap() {
        let report = cov(
            "polymarket",
            "TOK",
            &[("trade", vec![10, 11, 12]), ("quote", vec![10, 12]), ("book", vec![10, 12])],
        );
        let map = partial_days_from_coverage(&report);
        let key = report[0].key.clone();

        assert!(has_partial_days(&map, &key));
        assert_eq!(partial_days_label(&map, &key), "1 partial day (book, quote)");
    }

    /// A fully covered instrument produces NO entry — so an absent key means "fine", and a caller
    /// renders nothing without a branch.
    #[test]
    fn a_complete_instrument_has_no_entry_and_an_empty_label() {
        let report = cov(
            "polymarket",
            "TOK",
            &[("trade", vec![1, 2]), ("quote", vec![1, 2]), ("book", vec![1, 2])],
        );
        let map = partial_days_from_coverage(&report);
        let key = report[0].key.clone();

        assert!(map.is_empty());
        assert!(!has_partial_days(&map, &key));
        assert_eq!(partial_days_label(&map, &key), "");
    }

    /// An unknown instrument is "fine", not a panic — the map is a hint the caller may not have
    /// fetched yet.
    #[test]
    fn an_unfetched_instrument_reads_as_nothing_partial() {
        let map = PartialDayMap::new();
        let key = vike_data::InstrumentKey {
            venue: "binance".into(),
            label: "BTCUSDT".into(),
            grouped: false,
        };
        assert!(!has_partial_days(&map, &key));
        assert_eq!(partial_days_label(&map, &key), "");
    }

    /// The label pluralises and de-duplicates the kind list ACROSS days: day 1 lacks book, day 2
    /// lacks quote, days 3-4 lack both — four days, one line naming two kinds.
    #[test]
    fn the_label_summarises_many_days_into_one_line() {
        let report = cov(
            "binance",
            "BTCUSDT",
            &[("trade", vec![1, 2, 3, 4]), ("quote", vec![1]), ("book", vec![2])],
        );
        let map = partial_days_from_coverage(&report);
        assert_eq!(partial_days_label(&map, &report[0].key), "4 partial days (book, quote)");
    }

    /// **The false positive the live screenshot exposed.** binance `BTCUSDT.P` records `trade` and
    /// `depth` and nothing else — its L2 lives in the `depth` lane, not `book`. Held against the
    /// global kind list it read "missing book, quote" on every day it would ever have: a permanent
    /// ⚠ carrying no information. Held against the kinds it actually records, it is complete.
    #[test]
    fn a_trade_plus_depth_instrument_is_not_marked() {
        let report =
            cov("binance", "BTCUSDT.P", &[("trade", vec![1, 2, 3]), ("depth", vec![1, 2, 3])]);
        let map = partial_days_from_coverage(&report);
        assert!(map.is_empty(), "{map:?}");
        assert_eq!(partial_days_label(&map, &report[0].key), "");
    }

    /// …and the same instrument IS marked the day its depth lane stops while trades keep flowing —
    /// the half-failed recording this column exists to surface. Narrowing the comparison set does
    /// not weaken the signal.
    #[test]
    fn a_depth_outage_beside_a_live_trade_tape_is_marked() {
        let report =
            cov("binance", "BTCUSDT.P", &[("trade", vec![1, 2, 3]), ("depth", vec![1, 3])]);
        let map = partial_days_from_coverage(&report);
        assert_eq!(partial_days_label(&map, &report[0].key), "1 partial day (depth)");
    }

    /// Grouped and per-symbol series of one name are DIFFERENT instruments (different directories,
    /// different manifests), and the map keys them apart — merging would hide a mid-migration split.
    #[test]
    fn grouped_and_per_symbol_are_keyed_apart() {
        // Each side records trade AND quote, and each has one day where quote fell behind — so
        // both are partial. (A trade-ONLY instrument is complete at what it records, so it would
        // produce no entry at all and this test would prove nothing.)
        let report = join_coverage(&[
            (SeriesId::per_symbol("trade", "polymarket", "fam", None), vec![1, 2]),
            (SeriesId::per_symbol("quote", "polymarket", "fam", None), vec![1]),
            (SeriesId::grouped("trade", "polymarket", "fam"), vec![5, 6]),
            (SeriesId::grouped("quote", "polymarket", "fam"), vec![5]),
        ]);
        let map = partial_days_from_coverage(&report);
        // Two entries, not one merged: same venue, same label, different layout.
        assert_eq!(map.len(), 2, "{map:?}");
        assert!(map.keys().any(|k| k.grouped));
        assert!(map.keys().any(|k| !k.grouped));
    }
}

/// The [`vike_data::InstrumentKey`] a tree node denotes — the bridge from the Data Manager's
/// venue→symbol tree to the cross-kind coverage report, which is keyed by instrument rather than by
/// series.
///
/// This needs [`SymbolNode::grouped`], which is why the tree carries it: a grouped series and a
/// per-symbol one can share a label, and `coverage` treats them as different instruments.
pub fn instrument_key_of(venue: &str, sym: &SymbolNode) -> vike_data::InstrumentKey {
    vike_data::InstrumentKey {
        venue: venue.to_string(),
        label: sym.symbol.clone(),
        grouped: sym.grouped,
    }
}

/// The cross-kind warning for a tree node, ready to render — `""` when nothing is partial.
///
/// The Data Manager's existing gap column answers "is this SERIES missing days". This answers "does
/// this INSTRUMENT have days where some kinds are present and others are not" — the case a
/// per-series view structurally cannot show, because each series is contiguous on its own.
pub fn symbol_partial_label(map: &PartialDayMap, venue: &str, sym: &SymbolNode) -> String {
    partial_days_label(map, &instrument_key_of(venue, sym))
}

#[cfg(test)]
mod instrument_key_tests {
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
        let tree =
            build_tree(vec![(SeriesId::per_symbol("trade", "polymarket", "TOK", None), covg())]);
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
}

/// The **Polymarket proxy** box: the pure store/display pair, and the row's no-I/O contract.
///
/// Its own module because the field shares nothing with the catalog grid above it — it is the one
/// piece of this view that writes rather than reads, and its rules are about a credential-store
/// value, not about series coverage.
#[cfg(test)]
mod polymarket_proxy_tests {
    use super::*;

    /// An empty box is a REQUEST for a direct connection, not an absence of opinion — so it stores
    /// the sentinel rather than nothing. Writing nothing would leave `egress`'s built-in default
    /// (proxy ON at a localhost tunnel) in force, which is wrong for the user this box is for: one
    /// who simply is not geo-blocked and would otherwise dial a tunnel that does not exist.
    #[test]
    fn an_empty_box_stores_the_direct_sentinel() {
        assert_eq!(proxy_to_store(""), PROXY_DIRECT);
        assert_eq!(proxy_to_store("   "), PROXY_DIRECT, "a box cleared to spaces is still cleared");
    }

    /// A bought proxy arrives as one string WITH credentials in it. It must round-trip byte-for-byte
    /// — no lowercasing, no stripping, no re-encoding of the userinfo.
    #[test]
    fn a_bought_proxy_url_is_stored_verbatim_credentials_and_all() {
        let url = "socks5h://user:hunter2@1.2.3.4:1080";
        assert_eq!(proxy_to_store(url), url);
        assert_eq!(proxy_to_store(&format!("  {url}  ")), url, "surrounding space is trimmed");
        assert_eq!(proxy_display(Some(url)), url, "and it comes back to the box unchanged");
    }

    /// Nothing stored, and BOTH direct spellings, show as an empty box: "no proxy" must look like no
    /// proxy rather than like a literal `none` the operator has to know to delete.
    #[test]
    fn nothing_and_both_direct_spellings_display_as_an_empty_box() {
        assert_eq!(proxy_display(None), "");
        assert_eq!(proxy_display(Some("")), "");
        assert_eq!(proxy_display(Some(PROXY_DIRECT)), "");
        assert_eq!(proxy_display(Some("direct")), "");
        assert_eq!(proxy_display(Some("NONE")), "", "the sentinel is case-insensitive");
    }

    /// Clearing a configured proxy must round-trip to direct, which is the path a user takes when
    /// they stop being geo-blocked — the one direction an asymmetric pair would silently break.
    #[test]
    fn clearing_a_configured_proxy_round_trips_to_direct() {
        let stored = proxy_to_store("socks5h://1.2.3.4:1080");
        assert_eq!(proxy_display(Some(&stored)), "socks5h://1.2.3.4:1080");
        let cleared =
            proxy_to_store(&proxy_display(Some(&stored)).replace("socks5h://1.2.3.4:1080", ""));
        assert_eq!(cleared, PROXY_DIRECT);
        assert_eq!(proxy_display(Some(&cleared)), "");
    }

    /// The row reports the click and performs no I/O — the `BulkAction` contract. Save must hand
    /// back the STORED form (so the caller writes it verbatim), and every other frame must be
    /// `None`, or a caller driving this each frame would rewrite the credential store continuously.
    #[test]
    fn save_returns_the_stored_value_once_and_only_when_clicked() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 120.0));
        let mut state = ProxyEdit { buf: "socks5h://user:hunter2@1.2.3.4:1080".to_string() };

        // Render with no pointer input at all: the row must be inert.
        let mut saved = Some(String::new());
        let raw =
            egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
        // ⚠ The `FullOutput` must have its texture deltas CLEARED before it drops, or epaint
        // panics with "Dropped TexturesDelta with 1 unapplied deltas" — a real frame would upload
        // them. This is the same trap that made the egui 0.36 / wgpu 30 bump pass every test and
        // then panic on the first GPU run; the sibling module's `run_frame` exists for it.
        let mut out = ctx.run_ui(raw, |ui| {
            saved = polymarket_proxy_ui(ui, &mut state);
        });
        out.textures_delta.clear();
        assert_eq!(saved, None, "rendering alone never saves");

        // The buffer is untouched by rendering, so the value the operator sees is the value they
        // typed — and `proxy_to_store` is what Save would hand back, credentials intact.
        assert_eq!(state.buf, "socks5h://user:hunter2@1.2.3.4:1080");
        assert_eq!(proxy_to_store(&state.buf), "socks5h://user:hunter2@1.2.3.4:1080");
    }
}
