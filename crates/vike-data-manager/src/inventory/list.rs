//! `inventory_list`, the plain picker the Studio's Data pane mounts: the search row, the six sortable
//! columns and one row-height row per series. Its sort lives in egui temp memory, because its
//! signature is a two-mount contract that cannot grow a parameter. A click reports the series' key.

use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::metrics::space;

use super::filter::global_span;
use super::table::{
    DataManagerInventorySort, EMPTY_STORE, data_row_cells, header_row_cells, search_row,
    sort_flat_rows, venue_header_row,
};
use super::{DataManagerInventorySeriesKey, DataManagerInventoryVenue, flatten_venue, series_key};

/// Render the dense, sortable venue-grouped stored-catalog grid, filtered by `query`
/// (case-insensitive, venue/symbol only). Returns `Some(DataManagerInventorySeriesKey)` the frame a row is
/// clicked. A row-height rollup header per venue, then one row-height row per (symbol, series)
/// pair: Symbol | Kind | Coverage (span bar) | Rows | Size | Updated. (The rich grid's rows are
/// the control height instead, because each holds a checkbox — owner decision 5.) Column headers
/// are clickable to sort (ascending/descending) the rows within each venue group; sort state
/// persists in egui temp memory across frames since this function's signature is a two-mount
/// contract that can't grow a parameter.
pub fn inventory_list(
    ui: &mut egui::Ui,
    tree: &[DataManagerInventoryVenue],
    query: &mut String,
) -> Option<DataManagerInventorySeriesKey> {
    let t = ThemeTokens::of(ui.ctx());
    let mut picked = None;

    search_row(ui, &t, query);
    ui.add_space(t.metrics.gap);

    if tree.is_empty() {
        state::view(ui, ThemeEmptyPane::Empty(EMPTY_STORE));
        return None;
    }

    let (global_first, global_last) = global_span(tree);

    let sort_id = ui.scope_id().with("data_manager_inventory_sort");
    let mut sort: DataManagerInventorySort =
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<DataManagerInventorySort>(sort_id));

    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), t.metrics.row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| header_row_cells(ui, &mut sort, false),
    );
    ui.separator();

    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<DataManagerInventorySort>(sort_id) = sort);

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("inventory_grid").show(
        ui,
        |ui| {
            // Rows butt together, as the kit's table's do: the row height IS the pitch.
            ui.spacing_mut().item_spacing.y = space::NONE;
            for venue_node in tree {
                let mut rows = flatten_venue(venue_node, query);
                if rows.is_empty() {
                    continue;
                }
                sort_flat_rows(&mut rows, sort);

                venue_header_row(ui, venue_node);

                for frow in &rows {
                    let desired = egui::vec2(ui.available_width(), t.metrics.row_height);
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
                        data_row_cells(
                            &mut row_ui,
                            frow,
                            global_first,
                            global_last,
                            None,
                            None,
                            (false, false),
                        );
                    }
                    if response.clicked() {
                        picked = Some(series_key(venue_node, frow));
                    }
                }
            }
        },
    );

    picked
}
