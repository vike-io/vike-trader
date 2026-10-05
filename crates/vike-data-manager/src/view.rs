//! The egui render of the Data-Manager "Stored" catalog: a dense, sortable venue-grouped grid
//! over [`crate::model::build_tree`]'s output, searchable by venue/symbol, with a click-to-select
//! row. Extracted out of `vike-app`'s `data_tool_content` (the "Stored" tab render) and
//! `vike-studio`'s bespoke `data_browser.rs` grouping/render so both mount the identical view.
//! Deliberately NOT coupled to vike-app-core's `ToolView` or live `feeds`: this takes only a
//! `&[VenueNode]` + a `&mut String` query and returns a plain [`StoredSelection`] — the caller
//! (app or Studio) decides what a click means (open a chart, wire a `SlicePicker`, …).
//!
//! Density rewrite (dm-dense): the old per-symbol `CollapsingHeader` tree (chunky, awkward at
//! hundreds/thousands of symbols) is replaced with a flat table: a row-height rollup header row per
//! venue, then one row per (symbol, series) pair — the density's control height in the rich grid,
//! whose every row holds a checkbox, and its row height in the plain picker — with columns Symbol |
//! Kind | Coverage | Rows | Size | Updated. Columns are sortable (click a header to sort
//! ascending/descending, independently within each venue group); sort state lives in egui temp
//! memory keyed off `ui.id()` since the function signature can't change (both mounts depend on it).
//! The Coverage column paints a thin span bar over the tree-wide `[global_first, global_last]`
//! window so bars are comparable across every row, in [`CoverageStyle`]'s stale colour when a
//! series' `last_ts` lags far behind the global max.
//!
//! Per-series gap viz (dm-gap-viz): `stored_catalog_grid` (the app-only rich grid; the Studio's
//! plain `stored_catalog_ui` is UNCHANGED) additionally takes a `&GapMap` — the missing-day
//! ranges `vike_data::DataFusionHist::series_gaps` computes, keyed by [`SeriesKey`]. The
//! Coverage bar overpaints each gap as a dark cut-out within the filled span, and the `HasGaps`
//! smart view (`ViewFilter::HasGaps`) filters to series [`has_gaps`] for. Fetching the map is the
//! caller's job (vike-desktop's `refresh_stored`, off-thread, alongside the inventory tree) — this
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

/// Which view is active for the rich grid. `All` (the default) shows the whole tree. NOTE:
/// `AssetClass`/`Watchlist` are carried in the type today but [`filter_tree`] can't yet act on
/// them — the display tree
/// (`VenueNode`/`SymbolNode`/`SeriesRow`) has no asset-class tag, and no caller supplies watchlist
/// symbol MEMBERSHIP (only names); wiring either needs a
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

/// One bulk action the grid's selection bar can request. The caller (vike-app-core's Stored tab)
/// owns what each actually does — the grid only reports which button was clicked.
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

/// `venue.polymarket.socks_proxy`'s "no proxy" spelling. `vike_polymarket::egress` reads this (and `direct`) as
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
    if t.is_empty() { PROXY_DIRECT.to_string() } else { t.to_string() }
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
/// [`proxy_to_store`], so the caller writes it to `venue.polymarket.socks_proxy` verbatim. `None` every other
/// frame. Like [`BulkAction`], this reports the click and performs no I/O: the caller owns the
/// credential-store write — vike-app-core's `save_polymarket_proxy`, into the store the binary
/// resolved and handed down as a `vike_connections::CredentialWrite`. (This said "the caller
/// (vike-app) … because only a binary may touch that store" until 2026-09-28.)
///
/// The value is shown PLAINLY, embedded credentials included — a SOCKS URL may carry
/// `user:pass@`, and masking a value the operator is here to read and correct would defeat the box.
pub fn polymarket_proxy_ui(ui: &mut egui::Ui, state: &mut ProxyEdit) -> Option<String> {
    let t = Tokens::of(ui.ctx());
    let mut save = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Polymarket proxy")
                .font(t.font(TextRole::Body))
                .color(t.theme.text2),
        );
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = PROXY_BOX_W;
            input::text(ui, &mut state.buf, Field { hint: PROXY_HINT, ..Field::default() });
        });
        // Save is this row's primary action (spec §4.3).
        if ui.add(ActionButton::primary("Save")).clicked() {
            save = Some(proxy_to_store(&state.buf));
        }
    });
    ui.label(egui::RichText::new(PROXY_HELP).font(t.font(TextRole::Caption)).color(t.theme.text3));
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
pub const PROXY_HELP: &str = "SOCKS5 only, and socks5h:// (not socks5://) so DNS resolves at the proxy. \
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
        if state.ascending { ord } else { ord.reverse() }
    });
}

/// Tree-wide `(first_ts, last_ts)` window across every venue with at least one series, for the
/// Coverage column's span bars to share one comparable scale. `(0, 0)` (an inert, zero-width
/// window — every bar paints as empty) when nothing has data.
///
/// ⚠ **That empty-tree clause was a LIE until now.** The fold seeded `(i64::MAX, i64::MIN)` and
/// returned it unchanged when no venue had a series, so an empty tree answered with those sentinels
/// rather than `(0, 0)`. Nothing noticed while the only consumer was the span bar — `is_stale`
/// refuses a window whose end is not after its start, and a bar drawn on it is empty either way. It
/// stops being harmless the moment a caller RENDERS the window, which the By-venue destination
/// does: `i64::MAX` epoch-ms formats as a date roughly 292 million years out.
pub fn global_span(tree: &[VenueNode]) -> (i64, i64) {
    let (lo, hi) =
        tree.iter().filter(|v| v.total.series > 0).fold((i64::MAX, i64::MIN), |(lo, hi), v| {
            (lo.min(v.total.first_ts), hi.max(v.total.last_ts))
        });
    if lo > hi { (0, 0) } else { (lo, hi) }
}

/// A series reads as "stale" once its `last_ts` lags more than 35% of the tree-wide span behind
/// the global max — an arbitrary but conservative fraction (recent data is typically within a few
/// percent; anything past a third of the whole window behind is very likely an abandoned/broken
/// feed, not just "yesterday's close").
pub fn is_stale(last_ts: i64, global_first: i64, global_last: i64) -> bool {
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

/// Apply the active view to `tree`, returning a filtered
/// copy. `All` is the identity (a full clone); `Venue(v)` keeps only that venue; `Stale` keeps only
/// series whose coverage lags the tree-wide span (reusing [`is_stale`]), dropping symbols/venues
/// left with nothing. `HasGaps` keeps only series [`has_gaps`] for in `gaps` (a series absent from
/// `gaps`, or fetched with an empty list, is treated as gap-free). `Watchlist`/`AssetClass` are
/// still placeholders: no caller supplies watchlist symbol MEMBERSHIP (only names),
/// so there's no data here to filter by yet; both currently behave like `All`.
///
/// ⚠ The caller sets [`GridState::active_view`]. It used to be set by a `views_sidebar` picker
/// inside the grid's own body; that picker is deleted and
/// `crates/vike-app-core/src/ui/tool_views/data_rail.rs` sets it from the window's rail instead.
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

// ⚠ `views_sidebar` used to live here and is DELETED.
//
// It was the rich grid's own left nav — All / one entry per venue / the Has-gaps and Stale smart
// views / a Watchlists group — and it sat INSIDE one of the Data Manager's seven sub-tabs, which
// made the window carry two stacked navigations. `crates/vike-app-core/src/ui/tool_views/data_rail.rs`
// is that rail promoted to be the window's only one, and it owns `GridState::active_view` directly.
//
// Deleted rather than left behind a `#[deprecated]` or kept "in case": it had no test, no caller
// outside the app body that replaced it, and the root `CLAUDE.md`'s "Conventions that will bite you
// if ignored" bans a shim kept so an old spelling compiles — which applies to a whole function just
// as much as to the re-export it names. [`ViewFilter`] and
// [`filter_tree`] are untouched — the FILTER is the contract, and the picker was never it.

// The byte/count formatters live in the shared `vike_ui_theme::fmt` leaf crate now (F35 dedup —
// `vike-app`'s `human_bytes` was a line-for-line copy of `fmt_bytes`). A PRIVATE import: the
// re-export chain that minted `vike_data_manager::{fmt_bytes, fmt_count, fmt_count_compact}` (this
// line's `pub` plus lib.rs's) had no consumer through EITHER hop — every caller outside this crate
// already spells `vike_ui_theme::fmt::…` — so these names now serve only the call sites below.
use egui::Color32;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, toggle};
use vike_ui_theme::fmt::{fmt_bytes, fmt_count, fmt_count_compact};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::RADIUS;
use vike_ui_theme::type_scale::TextRole;

/// `"430 series · 2.1B rows · 18.4 GB"` — the venue group header's rollup summary.
fn venue_rollup_label(total: &RollUp) -> String {
    format!(
        "{} series · {} rows · {}",
        fmt_count(total.series as u64),
        fmt_count_compact(total.rows),
        fmt_bytes(total.bytes)
    )
}

/// The empty-state sentences. There are three, because they are three different facts (the one-rail
/// design's hazard 2): the store holds nothing; the store holds series but this VIEW matched none;
/// the view matched series but the search matched none.
const EMPTY_STORE: &str = "No stored data.";
const EMPTY_VIEW: &str = "No series match this view.";
const EMPTY_SEARCH: &str = "No matching series.";

/// The search box's hint. It replaces the hover text the old `Search:` field carried.
const SEARCH_HINT: &str = "venue or symbol";

/// The Polymarket proxy box's width: a whole `socks5h://user:pass@host:port` without scrolling.
const PROXY_BOX_W: f32 = 320.0;

/// The rich grid's checkbox column is one control height wide, so the kit checkbox (a mark three
/// fifths of the control height, then a gap) fits at every density. The header's blank cell and
/// every row read this one width.
fn check_w(t: &Tokens) -> f32 {
    t.metrics.control_h
}

/// The coverage bar's colours and height — ONE source, read by [`paint_coverage_bar`] and by
/// [`coverage_legend`]. The legend can therefore never again describe a bar the grid does not draw:
/// before this, it said covered was blue and stale grey, while the bars painted covered in the
/// hover grey (about 1.2:1 on the track) and stale in amber.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageStyle {
    /// The empty track behind a series' span.
    pub track: Color32,
    /// The span a series covers.
    pub covered: Color32,
    /// A span whose last row lags past the stale threshold ([`is_stale`]).
    pub stale: Color32,
    /// A missing-day range cut out of the span. It is the track's colour, so it reads as a hole.
    pub gap: Color32,
    /// The bar's height: half a table row.
    pub bar_h: f32,
}

impl CoverageStyle {
    pub fn of(t: &Tokens) -> CoverageStyle {
        CoverageStyle {
            track: t.theme.bg,
            covered: Status::Info.color(),
            stale: Status::Warning.color(),
            gap: t.theme.bg,
            bar_h: (t.metrics.row_h * 0.5).round(),
        }
    }
}

/// The legend for the coverage bars, painted in the bars' own [`CoverageStyle`]: a swatch and a
/// word for covered and for stale, then the partial-day mark. Laid out left to right; the caller
/// places it. The Data Manager's stored strip renders it
/// (`crates/vike-app-core/src/ui/tool_views/stored.rs`'s `stored_tool_content`); the Studio's plain
/// picker shows no legend.
pub fn coverage_legend(ui: &mut egui::Ui) {
    let t = Tokens::of(ui.ctx());
    let style = CoverageStyle::of(&t);
    let caption = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3));
    };
    for (color, what) in [(style.covered, "covered"), (style.stale, "stale")] {
        let size = egui::vec2(2.0 * style.bar_h, style.bar_h);
        let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(r, egui::CornerRadius::same(RADIUS), color);
        caption(ui, what);
    }
    ui.label(
        icons::WARNING.rich().size(t.text.px(TextRole::Caption)).color(Status::Warning.color()),
    );
    caption(ui, "partial day");
}

/// The search row both mounts open with: a caption, then a kit text input.
fn search_row(ui: &mut egui::Ui, t: &Tokens, query: &mut String) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Search").font(t.font(TextRole::Caption)).color(t.theme.text3),
        );
        input::text(ui, query, Field { hint: SEARCH_HINT, ..Field::default() });
    });
}

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

/// One clickable column header: label + the sort-direction icon on the active sort column.
/// Clicking toggles direction if already active, else selects the column ascending.
///
/// The icon FOLLOWS the words inside one label — one paragraph, so a right-aligned column keeps
/// "Rows" then its icon, which two widgets in a right-to-left cell would reverse.
fn header_cell(
    ui: &mut egui::Ui,
    width: f32,
    align_right: bool,
    label: &str,
    column: SortColumn,
    sort: &mut SortState,
) {
    let t = Tokens::of(ui.ctx());
    let words =
        |s: String| egui::RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3);
    let resp = cell(ui, width, align_right, |ui| {
        let text: egui::WidgetText = if sort.column == column {
            let icon = if sort.ascending { icons::SORT_ASCENDING } else { icons::SORT_DESCENDING };
            let mark = icon.rich().size(t.text.px(TextRole::Caption)).color(t.theme.text3);
            let mut job = egui::text::LayoutJob::default();
            for part in [words(format!("{label} ")), mark] {
                part.append_to(
                    &mut job,
                    ui.style(),
                    egui::FontSelection::Default,
                    egui::Align::Center,
                );
            }
            job.into()
        } else {
            words(label.to_string()).into()
        };
        ui.add(egui::Label::new(text).sense(egui::Sense::click()))
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
/// `[cov.first_ts, cov.last_ts]` mapped over `[global_first, global_last]`, in [`CoverageStyle`]'s
/// covered colour, or its stale colour when `is_stale`. A zero/empty series paints an empty track
/// (no segment). `gaps` (inclusive epoch-ms ranges, e.g. from `DataFusionHist::series_gaps`) are
/// then overpainted as cut-outs in the style's gap colour within the filled segment — a series with
/// no known gaps (`&[]`) paints no cut-out at all.
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
    let style = CoverageStyle::of(&Tokens::of(ui.ctx()));
    let round = egui::CornerRadius::same(RADIUS);
    let painter = ui.painter();
    painter.rect_filled(rect, round, style.track);
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
    painter.rect_filled(bar, round, if stale { style.stale } else { style.covered });

    // Gap cut-outs: overpaint each missing-day sub-range (clamped to the filled bar itself, so a
    // gap can never bleed past the segment it's punching a hole in) in the style's gap colour —
    // the track's — so gaps read as "missing" holes against the filled span.
    if !gaps.is_empty() {
        let gap_color = style.gap;
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

/// One row-height venue group header: venue name + its rollup summary, on the surface fill so it
/// reads as a separator between venues.
fn venue_header_row(ui: &mut egui::Ui, venue: &VenueNode) {
    let t = Tokens::of(ui.ctx());
    let desired = egui::vec2(ui.available_width(), t.metrics.row_h);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 0.0, t.theme.surface);
        let inner = rect.shrink2(egui::vec2(t.metrics.pad, 0.0));
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.label(
            vike_ui_theme::font::semibold(&venue.venue)
                .size(t.text.px(TextRole::Body))
                .color(t.theme.text),
        );
        row_ui.add_space(t.metrics.gap);
        row_ui.label(
            egui::RichText::new(venue_rollup_label(&venue.total))
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
        );
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
        let t = Tokens::of(ui.ctx());
        let hint = "Partial days: days where some of this instrument's kinds have data and others \
                    do not";
        let mark = icons::WARNING.rich().size(t.text.px(TextRole::Caption)).color(t.theme.text3);
        icons::named(cell(ui, W_PARTIAL, false, |ui| ui.label(mark)), hint);
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
    let t = Tokens::of(row_ui.ctx());
    let style = CoverageStyle::of(&t);
    cell(row_ui, W_SYMBOL, false, |ui| {
        ui.label(
            egui::RichText::new(&frow.symbol).font(t.mono(TextRole::Body)).color(t.theme.text),
        );
    });
    cell(row_ui, W_KIND, false, |ui| {
        ui.label(
            egui::RichText::new(kind_label(frow))
                .font(t.font(TextRole::Caption))
                .color(t.theme.text2),
        );
    });
    cell(row_ui, W_COVERAGE, false, |ui| {
        paint_coverage_bar(
            ui,
            W_COVERAGE - 8.0,
            style.bar_h,
            &frow.cov,
            global_first,
            global_last,
            gaps,
        )
    })
    .on_hover_text(coverage_label(&frow.cov));
    if let Some(label) = partial {
        // Allocated whether or not this instrument is partial, so every row's later columns line up
        // with the header — a blank cell IS the "nothing missing" rendering.
        let c = cell(row_ui, W_PARTIAL, false, |ui| {
            let mark =
                if label.is_empty() { egui::RichText::new("") } else { icons::WARNING.rich() };
            ui.label(mark.size(t.text.px(TextRole::Caption)).color(Status::Warning.color()))
        });
        if !label.is_empty() {
            icons::named(c, label);
        }
    }
    cell(row_ui, W_ROWS, true, |ui| {
        ui.label(
            egui::RichText::new(fmt_count(frow.cov.rows))
                .font(t.mono(TextRole::Body))
                .color(t.theme.text),
        );
    });
    cell(row_ui, W_SIZE, true, |ui| {
        ui.label(
            egui::RichText::new(fmt_bytes(frow.cov.bytes))
                .font(t.mono(TextRole::Body))
                .color(t.theme.text),
        );
    });
    cell(row_ui, W_UPDATED, true, |ui| {
        let stale = is_stale(frow.cov.last_ts, global_first, global_last);
        let label = if frow.cov.rows == 0 {
            "—".to_string()
        } else {
            vike_model::time::epoch_ms_to_utc_date(frow.cov.last_ts)
        };
        let ink = if stale { Status::Warning.color() } else { t.theme.text };
        ui.label(egui::RichText::new(label).font(t.mono(TextRole::Body)).color(ink));
    });
}

/// Render the dense, sortable venue-grouped stored-catalog grid, filtered by `query`
/// (case-insensitive, venue/symbol only). Returns `Some(StoredSelection)` the frame a row is
/// clicked. A row-height rollup header per venue, then one row-height row per (symbol, series)
/// pair: Symbol | Kind | Coverage (span bar) | Rows | Size | Updated. (The rich grid's rows are
/// the control height instead, because each holds a checkbox — owner decision 5.) Column headers
/// are clickable to sort (ascending/descending) the rows within each venue group; sort state
/// persists in egui temp memory across frames since this function's signature is a two-mount
/// contract that can't grow a parameter.
pub fn stored_catalog_ui(
    ui: &mut egui::Ui,
    tree: &[VenueNode],
    query: &mut String,
) -> Option<StoredSelection> {
    let t = Tokens::of(ui.ctx());
    let mut picked = None;

    search_row(ui, &t, query);
    ui.add_space(t.metrics.gap);

    if tree.is_empty() {
        state::view(ui, Load::Empty(EMPTY_STORE));
        return None;
    }

    let (global_first, global_last) = global_span(tree);

    let sort_id = ui.id().with("dm_stored_catalog_sort");
    let mut sort: SortState =
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<SortState>(sort_id));

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), t.metrics.row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| header_row_cells(ui, &mut sort, false),
    );
    ui.separator();

    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<SortState>(sort_id) = sort);

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("stored_catalog_grid").show(
        ui,
        |ui| {
            // Rows butt together, as the kit's table's do: the row height IS the pitch.
            ui.spacing_mut().item_spacing.y = 0.0;
            for venue_node in tree {
                let mut rows = flatten_venue(venue_node, query);
                if rows.is_empty() {
                    continue;
                }
                sort_flat_rows(&mut rows, sort);

                venue_header_row(ui, venue_node);

                for frow in &rows {
                    let desired = egui::vec2(ui.available_width(), t.metrics.row_h);
                    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        if response.hovered() {
                            ui.painter().rect_filled(rect, 0.0, t.theme.hover);
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
/// the caller's mode says delete cannot act here (e.g. the grid shows a REMOTE store and this
/// caller cannot reach the wire's delete verb) — and a click on it yields no action. `None` =
/// today's live button. Backfill/Update take no such knob: they exist in every mode (the wire
/// backfill verb covers the remote one).
///
/// ⚠ **The parenthetical used to end "and delete is a local-store operation", and that reason went
/// stale on 2026-09-07 while the CONCLUSION survived.** Deleting a series IS a wire verb now —
/// `vike_datahub_client::proto`'s `Request` carries `DeleteSeries` and
/// `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` answers it. What makes remote Delete
/// still wrong FROM THIS SEAT is narrower: that verb is served only by a KEYED datahub, and no
/// crate on this side resolves datahub keys. `crates/vike-app-core/src/data/stored_mode.rs`'s
/// `DELETE_LOCAL_ONLY` carries the whole argument and is the text the caller actually passes.
fn bulk_action_bar(
    ui: &mut egui::Ui,
    selected_count: usize,
    delete_disabled: Option<&str>,
) -> (Option<BulkAction>, [egui::Rect; 3]) {
    let t = Tokens::of(ui.ctx());
    let mut action = None;
    let mut rects = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        let r = ui.add(ActionButton::secondary("Backfill"));
        rects[0] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Backfill);
        }
        let r = ui.add(ActionButton::secondary("Update"));
        rects[1] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Update);
        }
        // Delete only OPENS the caller's confirm, whose own Delete is the danger button. The mode
        // gate is `delete_disabled`: see this function's doc for its reason.
        let delete = ActionButton::secondary("Delete");
        let delete = match delete_disabled {
            Some(why) => delete.disabled_because(why),
            None => delete,
        };
        let r = ui.add(delete);
        rects[2] = r.rect;
        if r.clicked() {
            action = Some(BulkAction::Delete);
        }
        ui.add_space(t.metrics.gap);
        ui.label(
            egui::RichText::new(format!("{selected_count} selected"))
                .font(t.font(TextRole::Body))
                .color(t.theme.text2),
        );
    });
    (action, rects)
}

/// The rich, app-only entry point over the same tree/render machinery as `stored_catalog_ui`:
/// adds a leading checkbox column (toggles `state.selected`), a header "select all (filtered)"
/// checkbox, and a bulk-action bar (Backfill/Update/Delete) shown once anything is selected.
/// `stored_catalog_ui`'s signature is untouched; this is an additive second entry point, not a
/// replacement (the Studio keeps using the simple picker). `state.active_view` is applied via
/// [`filter_tree`] before anything below renders — the caller, which owns `active_view` and sets it
/// from the Data Manager's rail, passes the *full* tree; this function does its
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
/// the button with `reason` as hover text — the honest rendering when the grid shows a store this
/// caller cannot delete FROM (a REMOTE store; the wire's delete verb is served only by a KEYED
/// datahub, and nothing on this side resolves datahub keys) — `None` is today's live button.
/// ⚠ This used to read "delete is a local-store operation", which was true when written and stopped
/// being true on 2026-09-07; the reason moved and the conclusion did not. `bulk_action_bar`'s doc
/// above carries the correction in full, and the two must be fixed together or they will disagree.
pub fn stored_catalog_grid(
    ui: &mut egui::Ui,
    tree: &[VenueNode],
    state: &mut GridState,
    gaps: &GapMap,
    partials: &PartialDayMap,
    delete_disabled: Option<&str>,
) -> GridResponse {
    let t = Tokens::of(ui.ctx());
    let mut resp = GridResponse::default();
    // Taken BEFORE `tree` is shadowed by the view: an empty VIEW over a full store is not an empty
    // store, and the two say different sentences (the one-rail design's hazard 2).
    let store_is_empty = tree.is_empty();
    let view_filtered = filter_tree(tree, &state.active_view, gaps);
    let tree: &[VenueNode] = &view_filtered;

    search_row(ui, &t, &mut state.query);
    ui.add_space(t.metrics.gap);

    if tree.is_empty() {
        state::view(ui, Load::Empty(if store_is_empty { EMPTY_STORE } else { EMPTY_VIEW }));
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
    let mut checked = all_selected;
    if toggle::checkbox(ui, &mut checked, "select all (filtered)").changed() {
        apply_select_all(&mut state.selected, &all_keys, checked);
    }
    ui.add_space(2.0);

    if by_venue.is_empty() {
        state::view(ui, Load::Empty(EMPTY_SEARCH));
        return resp;
    }

    let w_check = check_w(&t);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), t.metrics.row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            cell(ui, w_check, false, |_ui| {});
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
            // Rows butt together, as the kit's table's do: the row height IS the pitch.
            ui.spacing_mut().item_spacing.y = 0.0;
            for (venue_node, rows) in &by_venue {
                venue_header_row(ui, venue_node);

                for frow in rows {
                    let key = series_key(venue_node, frow);
                    // The control height, not the row height: every row holds a checkbox.
                    let desired = egui::vec2(ui.available_width(), t.metrics.control_h);
                    let (row_rect, _bg) = ui.allocate_exact_size(desired, egui::Sense::hover());
                    if !ui.is_rect_visible(row_rect) {
                        continue;
                    }
                    let check_rect = egui::Rect::from_min_size(
                        row_rect.min,
                        egui::vec2(w_check, t.metrics.control_h),
                    );
                    let rest_rect = egui::Rect::from_min_max(
                        egui::pos2(row_rect.min.x + w_check, row_rect.min.y),
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
                        ui.painter().rect_filled(row_rect, 0.0, t.theme.hover);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    let mut check_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(check_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let mut checked = state.selected.contains(&key);
                    if toggle::checkbox(&mut check_ui, &mut checked, "").changed() {
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

#[path = "view_tests.rs"]
#[cfg(test)]
mod view_tests;

#[path = "partial_day_tests.rs"]
#[cfg(test)]
mod partial_day_tests;

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

#[path = "instrument_key_tests.rs"]
#[cfg(test)]
mod instrument_key_tests;

/// The **Polymarket proxy** box: the pure store/display pair, and the row's no-I/O contract.
///
/// Its own module because the field shares nothing with the catalog grid above it — it is the one
/// piece of this view that writes rather than reads, and its rules are about a credential-store
/// value, not about series coverage.
#[path = "polymarket_proxy_tests.rs"]
#[cfg(test)]
mod polymarket_proxy_tests;
