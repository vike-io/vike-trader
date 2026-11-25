//! The ROWS and CELLS both views draw: the sortable column headers ([`DataManagerInventorySort`]),
//! one row's data cells, the venue group header, the search row and the empty-state sentences, and
//! the coverage bar with its legend, which read ONE [`DataManagerInventoryCoverageStyle`]. `list` and
//! `grid` compose them. The pure parts (the sort, `timestamp_range_to_x_fraction`, [`coverage_label`])
//! are unit-tested without egui.

use vike_data::SeriesCoverage;

// The count formatters live in the shared `vike_ui_theme::fmt` leaf crate now (F35 dedup), and the
// byte-count display in `vike_utility::bytes` (this grid speaks `iec_bytes`). A PRIVATE import: the
// re-export chain that minted `vike_data_manager::{format_count, format_count_compact}` (this
// line's `pub` plus lib.rs's) had no consumer through EITHER hop — every caller outside this crate
// already spells `vike_ui_theme::fmt::…` — so these names now serve only the call sites below.
use egui::Color32;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::fmt::{format_count, format_count_compact};
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{RADIUS, space};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data_manager;
use vike_utility::bytes::iec_bytes;

use super::filter::{CHECKING_MARK, GAPS_CHECKING, GAPS_NOT_CHECKED, PARTIALS_CHECKING, is_stale};
use super::{
    DataManagerInventoryRollup, DataManagerInventoryRow, DataManagerInventoryVenue, kind_label,
};

/// Which column the grid is currently sorted by. `Symbol` (ascending) is the default so the
/// initial render matches `build_inventory_tree`'s existing sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataManagerInventorySortColumn {
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
pub struct DataManagerInventorySort {
    pub column: DataManagerInventorySortColumn,
    pub ascending: bool,
}

impl Default for DataManagerInventorySort {
    fn default() -> Self {
        Self { column: DataManagerInventorySortColumn::Symbol, ascending: true }
    }
}

/// Compare two rows by `column`'s primary key, breaking ties by symbol then kind/interval so
/// equal-key rows still land in a stable, deterministic order.
fn cmp_flat_row(
    a: &DataManagerInventoryRow,
    b: &DataManagerInventoryRow,
    column: DataManagerInventorySortColumn,
) -> std::cmp::Ordering {
    let primary = match column {
        DataManagerInventorySortColumn::Symbol => a.symbol.cmp(&b.symbol),
        DataManagerInventorySortColumn::Kind => kind_label(a).cmp(&kind_label(b)),
        DataManagerInventorySortColumn::Coverage => {
            a.coverage.first_timestamp.cmp(&b.coverage.first_timestamp)
        }
        DataManagerInventorySortColumn::Rows => a.coverage.rows.cmp(&b.coverage.rows),
        DataManagerInventorySortColumn::Size => a.coverage.bytes.cmp(&b.coverage.bytes),
        DataManagerInventorySortColumn::Updated => {
            a.coverage.last_timestamp.cmp(&b.coverage.last_timestamp)
        }
    };
    primary.then_with(|| a.symbol.cmp(&b.symbol)).then_with(|| kind_label(a).cmp(&kind_label(b)))
}

/// Sort `rows` in place per `state` (column + direction). The one pure, unit-tested sort helper
/// the egui render calls per venue group.
pub(super) fn sort_flat_rows(
    rows: &mut [DataManagerInventoryRow],
    state: DataManagerInventorySort,
) {
    rows.sort_by(|a, b| {
        let ord = cmp_flat_row(a, b, state.column);
        if state.ascending { ord } else { ord.reverse() }
    });
}

/// Format one series' coverage for display, e.g. `"1234 rows, 2024-01-01 → 2024-03-01"`. An
/// empty series (`rows == 0`) reads as `"no data"` rather than a nonsensical epoch-derived range.
/// Mirrors `vike-studio`'s (now-deleted) `data_browser::format_coverage` so the app's Data
/// Manager and the Studio's Data pane render coverage identically. Used today as the Coverage
/// column's hover tooltip.
pub fn coverage_label(coverage: &SeriesCoverage) -> String {
    if coverage.rows == 0 {
        return "no data".to_string();
    }
    format!(
        "{} rows, {} → {}",
        coverage.rows,
        vike_model::time::epoch_ms_to_utc_date(coverage.first_timestamp),
        vike_model::time::epoch_ms_to_utc_date(coverage.last_timestamp)
    )
}

/// `"430 series · 2.1B rows · 18.4 GiB"` — the venue group header's rollup summary.
fn venue_rollup_label(total: &DataManagerInventoryRollup) -> String {
    format!(
        "{} series · {} rows · {}",
        format_count(total.series as u64),
        format_count_compact(total.rows),
        iec_bytes(total.bytes)
    )
}

/// The empty-state sentences. There are three, because they are three different facts (the one-rail
/// design's hazard 2): the store holds nothing; the store holds series but this VIEW matched none;
/// the view matched series but the search matched none.
pub(super) const EMPTY_STORE: &str = "No stored data.";
pub(super) const EMPTY_VIEW: &str = "No series match this view.";
pub(super) const EMPTY_SEARCH: &str = "No matching series.";

/// The search box's hint. It replaces the hover text the old `Search:` field carried.
const SEARCH_HINT: &str = "venue or symbol";

/// The coverage bar's colours and height — ONE source, read by [`paint_coverage_bar`] and by
/// [`coverage_legend`]. The legend can therefore never again describe a bar the grid does not draw:
/// before this, it said covered was blue and stale grey, while the bars painted covered in the
/// hover grey (about 1.2:1 on the track) and stale in amber. Each colour is the `colour` of its row in
/// `ui-theme.toml`'s `coverage` map (`maps::coverage`); the height is the bar's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DataManagerInventoryCoverageStyle {
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

impl DataManagerInventoryCoverageStyle {
    pub fn of(t: &ThemeTokens) -> DataManagerInventoryCoverageStyle {
        DataManagerInventoryCoverageStyle {
            track: maps::coverage::TRACK.colour.resolve(t),
            covered: maps::coverage::COVERED.colour.resolve(t),
            stale: maps::coverage::STALE.colour.resolve(t),
            gap: maps::coverage::GAP.colour.resolve(t),
            bar_h: (t.metrics.row_height * data_manager::BAR_HEIGHT_RATIO).round(),
        }
    }
}

/// The legend for the coverage bars, painted in the bars' own `DataManagerInventoryCoverageStyle`: a swatch and a
/// word (its row's `word`) for covered and for stale, then the partial-day mark (the `PARTIAL_DAY`
/// row's icon, colour and word, which the grid's Partial column draws from the same row). Laid out
/// left to right; the caller places it. The Data Manager's stored strip renders it
/// (`crates/vike-app-core/src/ui/windows/data_manager/inventory/mod.rs`'s `stored_tool_content`); the Studio's plain
/// picker shows no legend.
pub fn coverage_legend(ui: &mut egui::Ui) {
    let t = ThemeTokens::of(ui.ctx());
    let style = DataManagerInventoryCoverageStyle::of(&t);
    let caption = |ui: &mut egui::Ui, s: &str| {
        ui.label(
            egui::RichText::new(s)
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
    };
    for (color, row) in
        [(style.covered, &maps::coverage::COVERED), (style.stale, &maps::coverage::STALE)]
    {
        let size = egui::vec2(data_manager::LEGEND_SWATCH_ASPECT * style.bar_h, style.bar_h);
        let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(r, egui::CornerRadius::same(RADIUS), color);
        caption(ui, row.word.unwrap_or(""));
    }
    let partial = &maps::coverage::PARTIAL_DAY;
    if let Some(icon) = partial.icon() {
        ui.label(
            icon.rich().size(t.text.px(ThemeTextRole::Caption)).color(partial.colour.resolve(&t)),
        );
    }
    caption(ui, partial.word.unwrap_or(""));
}

/// The search row both mounts open with: a caption, then a kit text input.
pub(super) fn search_row(ui: &mut egui::Ui, t: &ThemeTokens, query: &mut String) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Search")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
        input::text(ui, query, ThemeInputField { hint: SEARCH_HINT, ..ThemeInputField::default() });
    });
}

/// Allocate a fixed-width cell within the current row `ui` and run `add_contents` inside it,
/// right-aligned for numeric columns. Successive calls on the same `ui` (itself laid out
/// left-to-right) advance left-to-right, giving manual, egui-`Grid`-free table columns whose
/// widths line up between the header row and every data row (both built from the same
/// `data_manager::W_*` constants, which `ui-theme.toml` holds).
pub(super) fn cell<R>(
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
    column: DataManagerInventorySortColumn,
    sort: &mut DataManagerInventorySort,
) {
    let t = ThemeTokens::of(ui.ctx());
    let words = |s: String| {
        egui::RichText::new(s).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary)
    };
    let resp = cell(ui, width, align_right, |ui| {
        let text: egui::WidgetText = if sort.column == column {
            let icon = if sort.ascending { icons::SORT_ASCENDING } else { icons::SORT_DESCENDING };
            let mark =
                icon.rich().size(t.text.px(ThemeTextRole::Caption)).color(t.theme.text_tertiary);
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
            *sort = DataManagerInventorySort { column, ascending: true };
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
fn timestamp_range_to_x_fraction(
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
/// `[coverage.first_timestamp, coverage.last_timestamp]` mapped over `[global_first, global_last]`, in [`DataManagerInventoryCoverageStyle`]'s
/// covered colour, or its stale colour when `is_stale`. A zero/empty series paints an empty track
/// (no segment). `gaps` (inclusive epoch-ms ranges, e.g. from `HistoryStoreDataFusion::series_gaps`) are
/// then overpainted as cut-outs in the style's gap colour within the filled segment — a series with
/// no known gaps (`&[]`) paints no cut-out at all.
fn paint_coverage_bar(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    coverage: &SeriesCoverage,
    global_first: i64,
    global_last: i64,
    gaps: &[(i64, i64)],
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let style = DataManagerInventoryCoverageStyle::of(&ThemeTokens::of(ui.ctx()));
    let round = egui::CornerRadius::same(RADIUS);
    let painter = ui.painter();
    painter.rect_filled(rect, round, style.track);
    if coverage.rows == 0 || global_last <= global_first {
        return resp;
    }
    let span = (global_last - global_first) as f32;
    let fraction0 = ((coverage.first_timestamp - global_first) as f32 / span).clamp(0.0, 1.0);
    let fraction1 = ((coverage.last_timestamp - global_first) as f32 / span).clamp(0.0, 1.0);
    let x0 = rect.left() + fraction0 * rect.width();
    let x1 = (rect.left() + fraction1 * rect.width())
        .max(x0 + data_manager::BAR_MIN_WIDTH)
        .min(rect.right());
    let bar = egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
    let stale = is_stale(coverage.last_timestamp, global_first, global_last);
    painter.rect_filled(bar, round, if stale { style.stale } else { style.covered });

    // Gap cut-outs: overpaint each missing-day sub-range (clamped to the filled bar itself, so a
    // gap can never bleed past the segment it's punching a hole in) in the style's gap colour —
    // the track's — so gaps read as "missing" holes against the filled span.
    if !gaps.is_empty() {
        let gap_color = style.gap;
        for &(g0, g1) in gaps {
            if let Some((gf0, gf1)) =
                timestamp_range_to_x_fraction(g0, g1, global_first, global_last)
            {
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
pub(super) fn venue_header_row(ui: &mut egui::Ui, venue: &DataManagerInventoryVenue) {
    let t = ThemeTokens::of(ui.ctx());
    let desired = egui::vec2(ui.available_width(), t.metrics.row_height);
    let (rect, _response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 0.0, t.theme.surface);
        let inner = rect.shrink2(egui::vec2(t.metrics.padding, space::NONE));
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.label(
            egui::RichText::new(&venue.venue)
                .size(t.text.px(ThemeTextRole::Body))
                .color(t.theme.text),
        );
        row_ui.add_space(t.metrics.gap);
        row_ui.label(
            egui::RichText::new(venue_rollup_label(&venue.total))
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
    }
}

/// Render the six sortable column headers (Symbol/Kind/Coverage/Rows/Size/Updated) onto `ui`
/// (already laid out left-to-right at header height) — shared by `inventory_list` and
/// `inventory_grid`'s header row. Callers that add their own leading columns (e.g. a
/// checkbox) render those first, on the same `ui`, before calling this.
///
/// `partial` adds the cross-kind Partial column after Coverage. It is NOT sortable (no
/// [`DataManagerInventorySortColumn`] variant): it is an instrument-level flag, so every row of one instrument carries
/// the same value and sorting by it would only cluster instruments, which grouping already does.
/// The flag must match what the caller passes `data_row_cells`, or header and rows misalign.
pub(super) fn header_row_cells(
    ui: &mut egui::Ui,
    sort: &mut DataManagerInventorySort,
    partial: bool,
) {
    header_cell(
        ui,
        data_manager::WIDTH_SYMBOL,
        false,
        "Symbol",
        DataManagerInventorySortColumn::Symbol,
        sort,
    );
    header_cell(
        ui,
        data_manager::WIDTH_KIND,
        false,
        "Kind",
        DataManagerInventorySortColumn::Kind,
        sort,
    );
    header_cell(
        ui,
        data_manager::WIDTH_COVERAGE,
        false,
        "Coverage",
        DataManagerInventorySortColumn::Coverage,
        sort,
    );
    if partial {
        let t = ThemeTokens::of(ui.ctx());
        let hint = "Partial days: days where some of this instrument's kinds have data and others \
                    do not";
        let mark = icons::WARNING
            .rich()
            .size(t.text.px(ThemeTextRole::Caption))
            .color(t.theme.text_tertiary);
        // Its full width reserved, as the rows reserve theirs (`data_row_cells`), so the two line up.
        let header = cell(ui, data_manager::WIDTH_PARTIAL, false, |ui| {
            ui.set_min_width(data_manager::WIDTH_PARTIAL);
            ui.label(mark)
        });
        icons::named(header, hint);
    }
    header_cell(
        ui,
        data_manager::WIDTH_ROWS,
        true,
        "Rows",
        DataManagerInventorySortColumn::Rows,
        sort,
    );
    header_cell(
        ui,
        data_manager::WIDTH_SIZE,
        true,
        "Size",
        DataManagerInventorySortColumn::Size,
        sort,
    );
    header_cell(
        ui,
        data_manager::WIDTH_UPDATED,
        true,
        "Updated",
        DataManagerInventorySortColumn::Updated,
        sort,
    );
}

/// Render one row's six data cells (Symbol/Kind/Coverage/Rows/Size/Updated) onto `row_ui` (a
/// child `Ui` already positioned over the row's rect, left-to-right layout) — shared by
/// `inventory_list` and `inventory_grid`'s row loop. Callers that add their own leading
/// columns (e.g. a checkbox) render those first, on the same `row_ui`, before calling this.
/// `gaps` is this row's [`DataManagerInventoryGapMap`] entry (`None` for `inventory_list`, which doesn't thread one — its
/// coverage bar stays a plain span, unchanged). An EMPTY entry is a probe that failed, which the
/// bar marks and its hover says ([`GAPS_NOT_CHECKED`]), so that row never reads as gap-free.
///
/// `partial` is the cross-kind column: `None` omits it entirely (`inventory_list`, which has no
/// coverage report to key it from), `Some(label)` allocates the cell and marks it when the label is
/// non-empty — see [`partial_days_label`]. It must match the flag passed to `header_row_cells`.
///
/// `checking` is `(gap probe, coverage report)` still to answer: each marks its cell with
/// [`CHECKING_MARK`] in the space the answer's own mark takes, so the row's layout never moves when
/// the answer lands.
///
/// [`DataManagerInventoryGapMap`]: super::filter::DataManagerInventoryGapMap
/// [`partial_days_label`]: super::partial_days::partial_days_label
pub(super) fn data_row_cells(
    row_ui: &mut egui::Ui,
    frow: &DataManagerInventoryRow,
    global_first: i64,
    global_last: i64,
    gaps: Option<&[(i64, i64)]>,
    partial: Option<&str>,
    checking: (bool, bool),
) {
    let t = ThemeTokens::of(row_ui.ctx());
    let not_checked = gaps.is_some_and(<[_]>::is_empty);
    let style = DataManagerInventoryCoverageStyle::of(&t);
    cell(row_ui, data_manager::WIDTH_SYMBOL, false, |ui| {
        ui.label(
            egui::RichText::new(&frow.symbol).font(t.mono(ThemeTextRole::Body)).color(t.theme.text),
        );
    });
    cell(row_ui, data_manager::WIDTH_KIND, false, |ui| {
        ui.label(
            egui::RichText::new(kind_label(frow))
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
    });
    let coverage = cell(row_ui, data_manager::WIDTH_COVERAGE, false, |ui| {
        let bar = paint_coverage_bar(
            ui,
            data_manager::WIDTH_COVERAGE - space::LG,
            style.bar_h,
            &frow.coverage,
            global_first,
            global_last,
            gaps.unwrap_or(&[]),
        );
        // Beside the bar, in the column's own margin: over the bar's end it read as a speck.
        let mark = if not_checked {
            Some(("?", maps::coverage::PARTIAL_DAY.colour.resolve(&t)))
        } else {
            checking.0.then_some((CHECKING_MARK, t.theme.text_tertiary))
        };
        if let Some((mark, ink)) = mark {
            ui.painter().text(
                bar.rect.right_center() + egui::vec2(space::XS, 0.0),
                egui::Align2::LEFT_CENTER,
                mark,
                t.font(ThemeTextRole::Caption),
                ink,
            );
        }
        bar
    });
    let said = coverage_label(&frow.coverage);
    coverage.on_hover_text(match (not_checked, checking.0) {
        (true, _) => format!("{said}\n{GAPS_NOT_CHECKED}"),
        (false, true) => format!("{said}\n{GAPS_CHECKING}"),
        (false, false) => said,
    });
    if let Some(label) = partial {
        // Allocated whether or not this instrument is partial, so every row's later columns line up
        // with the header — a blank cell IS the "nothing missing" rendering. ⚠ The width is RESERVED:
        // egui sizes a left-aligned cell to its content, so a blank, an icon and the checking mark
        // would each move every cell after this one (the mark did, as its answer landed).
        let c = cell(row_ui, data_manager::WIDTH_PARTIAL, false, |ui| {
            ui.set_min_width(data_manager::WIDTH_PARTIAL);
            let warning = &maps::coverage::PARTIAL_DAY;
            let mark = match warning.icon() {
                _ if checking.1 => egui::RichText::new(CHECKING_MARK).color(t.theme.text_tertiary),
                Some(icon) if !label.is_empty() => icon.rich().color(warning.colour.resolve(&t)),
                _ => egui::RichText::new(""),
            };
            ui.label(mark.size(t.text.px(ThemeTextRole::Caption)))
        });
        if checking.1 {
            icons::named(c, PARTIALS_CHECKING);
        } else if !label.is_empty() {
            icons::named(c, label);
        }
    }
    cell(row_ui, data_manager::WIDTH_ROWS, true, |ui| {
        ui.label(
            egui::RichText::new(format_count(frow.coverage.rows))
                .font(t.mono(ThemeTextRole::Body))
                .color(t.theme.text),
        );
    });
    cell(row_ui, data_manager::WIDTH_SIZE, true, |ui| {
        ui.label(
            egui::RichText::new(iec_bytes(frow.coverage.bytes))
                .font(t.mono(ThemeTextRole::Body))
                .color(t.theme.text),
        );
    });
    cell(row_ui, data_manager::WIDTH_UPDATED, true, |ui| {
        let stale = is_stale(frow.coverage.last_timestamp, global_first, global_last);
        let label = if frow.coverage.rows == 0 {
            "—".to_string()
        } else {
            vike_model::time::epoch_ms_to_utc_date(frow.coverage.last_timestamp)
        };
        let ink = if stale { style.stale } else { t.theme.text };
        ui.label(egui::RichText::new(label).font(t.mono(ThemeTextRole::Body)).color(ink));
    });
}

#[path = "table_tests.rs"]
#[cfg(test)]
mod table_tests;
