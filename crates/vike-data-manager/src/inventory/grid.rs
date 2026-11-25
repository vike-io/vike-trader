//! `inventory_grid`, the rich multi-select grid the app's Data Manager mounts over the same rows and
//! cells as `list`: a checkbox column, a "select all (filtered)" box, the bulk-action bar (Backfill,
//! Update, Delete) and the state that persists behind them ([`DataManagerInventoryGridState`]). It
//! reports clicks and performs no I/O.

use std::collections::BTreeSet;

use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeTokens, toggle};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

use super::filter::{
    DataManagerInventoryFilter, DataManagerInventoryGapMap, filter_tree, global_span,
};
use super::partial_days::{DataManagerInventoryPartialDayMap, partial_days_label};
use super::table::{
    DataManagerInventorySort, EMPTY_SEARCH, EMPTY_STORE, EMPTY_VIEW, cell, data_row_cells,
    header_row_cells, search_row, sort_flat_rows, venue_header_row,
};
use super::{
    DataManagerInventoryRow, DataManagerInventorySeriesKey, DataManagerInventoryVenue,
    flatten_venue, kind_label, series_key,
};

/// Persisted UI state for the rich multi-select grid (`inventory_grid`) — the app-only
/// entry point; `inventory_list`'s `(query, sort-in-temp-memory)` pair stays as-is for the
/// Studio's simple picker.
#[derive(Debug, Clone, Default)]
pub struct DataManagerInventoryGridState {
    pub query: String,
    pub sort: DataManagerInventorySort,
    pub selected: BTreeSet<DataManagerInventorySeriesKey>,
    pub active_view: DataManagerInventoryFilter,
}

/// One bulk action the grid's selection bar can request. The caller (vike-app-core's Stored tab)
/// owns what each actually does — the grid only reports which button was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataManagerInventoryGridBulkAction {
    Backfill,
    Update,
    Delete,
}

/// What happened this frame in `inventory_grid`: at most one row-open (mirrors
/// `inventory_list`'s return) and/or at most one bulk-bar click.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataManagerInventoryGridResponse {
    pub opened: Option<DataManagerInventorySeriesKey>,
    pub bulk: Option<DataManagerInventoryGridBulkAction>,
}

/// The rich grid's checkbox column is one control height wide, so the kit checkbox (a mark three
/// fifths of the control height, then a gap) fits at every density. The header's blank cell and
/// every row read this one width.
fn check_w(t: &ThemeTokens) -> f32 {
    t.metrics.control_height
}

/// Insert (`want: true`) or remove (`want: false`) `key` in `selected` — the one mutation both
/// the per-row checkbox and the header "select all" apply, factored out so it's testable without
/// an egui harness.
fn set_selected(
    selected: &mut BTreeSet<DataManagerInventorySeriesKey>,
    key: DataManagerInventorySeriesKey,
    want: bool,
) {
    if want {
        selected.insert(key);
    } else {
        selected.remove(&key);
    }
}

/// The [`DataManagerInventorySeriesKey`]s `inventory_grid` currently renders for `query` over `tree` — the exact
/// set "select all (filtered)" toggles. `tree` is expected to already be view-filtered by the
/// caller (mirrors `inventory_grid`'s own `filter_tree` → `flatten_venue` pipeline) so this
/// helper only re-does the query filter, kept pure/testable.
fn visible_keys(
    tree: &[DataManagerInventoryVenue],
    query: &str,
) -> Vec<DataManagerInventorySeriesKey> {
    tree.iter()
        .flat_map(|v| flatten_venue(v, query).into_iter().map(move |r| series_key(v, &r)))
        .collect()
}

/// Apply `want` to every key in `keys` — the header checkbox's "select all (filtered)" / "clear
/// all (filtered)" action, factored out for the same reason as [`set_selected`].
fn apply_select_all(
    selected: &mut BTreeSet<DataManagerInventorySeriesKey>,
    keys: &[DataManagerInventorySeriesKey],
    want: bool,
) {
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
/// crate on this side resolves datahub keys. `crates/vike-app-core/src/data/data_manager/mode.rs`'s
/// `DELETE_LOCAL_ONLY` carries the whole argument and is the text the caller actually passes.
fn bulk_action_bar(
    ui: &mut egui::Ui,
    selected_count: usize,
    delete_disabled: Option<&str>,
) -> (Option<DataManagerInventoryGridBulkAction>, [egui::Rect; 3]) {
    let t = ThemeTokens::of(ui.ctx());
    let mut action = None;
    let mut rects = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        let r = ui.add(ThemeButtonAction::secondary("Backfill"));
        rects[0] = r.rect;
        if r.clicked() {
            action = Some(DataManagerInventoryGridBulkAction::Backfill);
        }
        let r = ui.add(ThemeButtonAction::secondary("Update"));
        rects[1] = r.rect;
        if r.clicked() {
            action = Some(DataManagerInventoryGridBulkAction::Update);
        }
        // Delete only OPENS the caller's confirm, whose own Delete is the danger button. The mode
        // gate is `delete_disabled`: see this function's doc for its reason.
        let delete = ThemeButtonAction::secondary("Delete");
        let delete = match delete_disabled {
            Some(why) => delete.disabled_because(why),
            None => delete,
        };
        let r = ui.add(delete);
        rects[2] = r.rect;
        if r.clicked() {
            action = Some(DataManagerInventoryGridBulkAction::Delete);
        }
        ui.add_space(t.metrics.gap);
        ui.label(
            egui::RichText::new(format!("{selected_count} selected"))
                .font(t.font(ThemeTextRole::Body))
                .color(t.theme.text_secondary),
        );
    });
    (action, rects)
}

/// The rich, app-only entry point over the same tree/render machinery as `inventory_list`:
/// adds a leading checkbox column (toggles `state.selected`), a header "select all (filtered)"
/// checkbox, and a bulk-action bar (Backfill/Update/Delete) shown once anything is selected.
/// `inventory_list`'s signature is untouched; this is an additive second entry point, not a
/// replacement (the Studio keeps using the simple picker). `state.active_view` is applied via
/// [`filter_tree`] before anything below renders — the caller, which owns `active_view` and sets it
/// from the Data Manager's rail, passes the *full* tree; this function does its
/// own view filtering, same as it already does its own query filtering. `gaps` is the per-series
/// [`DataManagerInventoryGapMap`] (an empty map = no known gaps anywhere, today's pre-gap-viz behavior): it feeds both
/// the `HasGaps` smart view (via `filter_tree`) and each row's coverage-bar gap cut-outs.
///
/// `partials` is its cross-kind sibling ([`partial_days_from_coverage`] over the store's
/// `coverage_report`): the gap map answers "is this SERIES missing days", `partials` answers "does
/// this INSTRUMENT have days where some kinds are present and others are not" — which a per-series
/// view structurally cannot show, since each series is contiguous on its own. An empty map renders
/// exactly as before, minus nothing: the column is allocated but every cell is blank.
///
/// `delete_disabled` is the bulk bar's Delete gate (see `bulk_action_bar`): `Some(reason)` grays
/// the button with `reason` as hover text — the honest rendering when the grid shows a store this
/// caller cannot delete FROM (a REMOTE store; the wire's delete verb is served only by a KEYED
/// datahub, and nothing on this side resolves datahub keys) — `None` is today's live button.
/// ⚠ This used to read "delete is a local-store operation", which was true when written and stopped
/// being true on 2026-09-07; the reason moved and the conclusion did not. `bulk_action_bar`'s doc
/// above carries the correction in full, and the two must be fixed together or they will disagree.
///
/// `gaps_checking` names the series whose gap probe has not answered yet and `partials_checking`
/// says the coverage report has not: their cells carry [`CHECKING_MARK`] until the answer lands, and
/// nothing about a row's place or size depends on either, so rows already drawn never move.
///
/// [`partial_days_from_coverage`]: crate::partial_days_from_coverage
/// [`CHECKING_MARK`]: crate::CHECKING_MARK
pub fn inventory_grid(
    ui: &mut egui::Ui,
    tree: &[DataManagerInventoryVenue],
    state: &mut DataManagerInventoryGridState,
    gaps: &DataManagerInventoryGapMap,
    partials: &DataManagerInventoryPartialDayMap,
    delete_disabled: Option<&str>,
    gaps_checking: &BTreeSet<DataManagerInventorySeriesKey>,
    partials_checking: bool,
) -> DataManagerInventoryGridResponse {
    let t = ThemeTokens::of(ui.ctx());
    let mut resp = DataManagerInventoryGridResponse::default();
    // Taken BEFORE `tree` is shadowed by the view: an empty VIEW over a full store is not an empty
    // store, and the two say different sentences (the one-rail design's hazard 2).
    let store_is_empty = tree.is_empty();
    let view_filtered = filter_tree(tree, &state.active_view, gaps);
    let tree: &[DataManagerInventoryVenue] = &view_filtered;

    search_row(ui, &t, &mut state.query);
    ui.add_space(t.metrics.gap);

    if tree.is_empty() {
        state::view(
            ui,
            ThemeEmptyPane::Empty(if store_is_empty { EMPTY_STORE } else { EMPTY_VIEW }),
        );
        return resp;
    }

    let (global_first, global_last) = global_span(tree);

    // Every (venue, rows) pair this frame shows — computed once (unsorted; sorted below once the
    // header has had a chance to update `state.sort` this frame), shared by the select-all
    // checkbox (needs the full filtered+queried key set) and the row loop.
    let mut by_venue: Vec<(&DataManagerInventoryVenue, Vec<DataManagerInventoryRow>)> = Vec::new();
    for v in tree {
        let rows = flatten_venue(v, &state.query);
        if rows.is_empty() {
            continue;
        }
        by_venue.push((v, rows));
    }
    let all_keys: Vec<DataManagerInventorySeriesKey> = visible_keys(tree, &state.query);

    if !state.selected.is_empty() {
        let (action, _rects) = bulk_action_bar(ui, state.selected.len(), delete_disabled);
        resp.bulk = action;
        ui.add_space(space::SM);
    }

    let all_selected = !all_keys.is_empty() && all_keys.iter().all(|k| state.selected.contains(k));
    let mut checked = all_selected;
    if toggle::checkbox(ui, &mut checked, "select all (filtered)").changed() {
        apply_select_all(&mut state.selected, &all_keys, checked);
    }
    ui.add_space(space::XS);

    if by_venue.is_empty() {
        state::view(ui, ThemeEmptyPane::Empty(EMPTY_SEARCH));
        return resp;
    }

    let w_check = check_w(&t);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), t.metrics.row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            cell(ui, w_check, false, |_ui| {});
            header_row_cells(ui, &mut state.sort, true);
        },
    );
    ui.separator();

    // Sort now, using whatever `state.sort` is after the header above may have just changed it
    // this frame (mirrors `inventory_list`'s flatten-then-sort-after-header order, minus the
    // temp-memory round trip since sort lives in `state` directly here).
    for (_, rows) in &mut by_venue {
        sort_flat_rows(rows, state.sort);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("stored_catalog_grid_rich")
        .show(ui, |ui| {
            // Rows butt together, as the kit's table's do: the row height IS the pitch.
            ui.spacing_mut().item_spacing.y = space::NONE;
            for (venue_node, rows) in &by_venue {
                venue_header_row(ui, venue_node);

                for frow in rows {
                    let key = series_key(venue_node, frow);
                    // The control height, not the row height: every row holds a checkbox.
                    let desired = egui::vec2(ui.available_width(), t.metrics.control_height);
                    let (row_rect, _bg) = ui.allocate_exact_size(desired, egui::Sense::hover());
                    if !ui.is_rect_visible(row_rect) {
                        continue;
                    }
                    let check_rect = egui::Rect::from_min_size(
                        row_rect.min,
                        egui::vec2(w_check, t.metrics.control_height),
                    );
                    let rest_rect = egui::Rect::from_min_max(
                        egui::pos2(row_rect.min.x + w_check, row_rect.min.y),
                        row_rect.max,
                    );
                    // A dedicated interact zone over everything BUT the checkbox column, so
                    // clicking the checkbox toggles selection only — it never also fires an
                    // "open" (the checkbox is its own nested `Sense::click` widget; overlapping
                    // it with this row's click sense would double-fire on every checkbox click).
                    let open_response = ui.interact(
                        rest_rect,
                        ui.scope_id().with(("data_manager_grid_row_open", &key)),
                        egui::Sense::click(),
                    );
                    if open_response.hovered() {
                        ui.painter().rect_filled(row_rect, 0.0, t.theme.hover);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    let mut check_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(check_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let mut checked = state.selected.contains(&key);
                    // Named by the row's own words (its symbol and kind cells, drawn after it).
                    let row_name = format!("{} {}", frow.symbol, kind_label(frow));
                    if toggle::checkbox(&mut check_ui, &mut checked, "")
                        .accessible_name(row_name)
                        .changed()
                    {
                        set_selected(&mut state.selected, key.clone(), checked);
                    }

                    let mut row_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rest_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let row_gaps = gaps.get(&key).map(|v| v.as_slice());
                    // Instrument-level, so it repeats down an instrument's rows — deliberate: the
                    // warning is about the SET of kinds, and any one of them is where you notice it.
                    // Computed per VISIBLE row only (the `is_rect_visible` continue above).
                    let partial = partial_days_label(
                        partials,
                        &vike_data::InstrumentCoverageKey {
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
                        (gaps_checking.contains(&key), partials_checking),
                    );

                    if open_response.clicked() {
                        resp.opened = Some(series_key(venue_node, frow));
                    }
                }
            }
        });

    resp
}

#[path = "grid_tests.rs"]
#[cfg(test)]
mod grid_tests;
