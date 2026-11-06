//! The settings table: the scaled fixed-width columns, the sticky header and the rows.

use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};
use vike_ui_theme::components::{Status, Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::backend_settings;

use super::edit::{SettingsEditState, SettingsWriteRequest, start_edit};
use super::editor::settings_edit_panel;
use super::rows::{
    SettingsFilter, UNREAD_VERDICT_POINTER, origin_kind, row_is_finding, row_is_set,
    row_read_by_nothing,
};

// Column widths for the settings table. Fixed, because a STICKY header has to be drawn OUTSIDE
// the scroll area and therefore cannot inherit an `egui::Grid`'s measured columns — the same
// construction `crates/vike-data-manager/src/view.rs`'s `stored_catalog_grid` uses, and the only
// sticky-header idiom in this tree. The gap between columns is `space::MD`, in the header row and
// the data rows alike, so the sticky header stays registered with the columns under it. The widths
// and heights are rows of `ui-theme.toml` (group `backend_settings`): `W_KEY`, `W_VALUE`,
// `W_ORIGIN`, `W_READ`, `W_EDIT`, `HEADER_H`, `ROW_H`, and `MIN_COL_SCALE`, how far the columns may
// shrink before they stop shrinking.

/// The columns' widths for THIS width. Fixed proportions, scaled down together so the sticky
/// header — which is drawn outside the scroll area and therefore cannot inherit a measured
/// column — stays registered with the rows at any window width. The design must work at ~400pt,
/// where the unscaled table is half again too wide.
#[derive(Debug, Clone, Copy)]
struct Cols {
    key: f32,
    value: f32,
    origin: f32,
    read: f32,
    edit: f32,
}

impl Cols {
    fn for_width(avail: f32) -> Self {
        let natural = backend_settings::W_KEY
            + backend_settings::W_VALUE
            + backend_settings::W_ORIGIN
            + backend_settings::W_READ
            + backend_settings::W_EDIT
            + 4.0 * space::MD;
        // Below `MIN_COL_SCALE` the row overflows its clip rect rather than becoming five unreadable
        // slivers — the window's own scroll is then the answer, not a narrower table.
        let min_scale = backend_settings::MIN_COL_SCALE;
        let k = if natural > 0.0 { (avail / natural).clamp(min_scale, 1.0) } else { 1.0 };
        Cols {
            key: backend_settings::W_KEY * k,
            value: backend_settings::W_VALUE * k,
            origin: backend_settings::W_ORIGIN * k,
            read: backend_settings::W_READ * k,
            edit: backend_settings::W_EDIT * k,
        }
    }
}

/// One fixed-width cell.
fn cell(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, backend_settings::ROW_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            add(ui);
        },
    );
}

/// A cell whose text ELIDES rather than wrapping or spilling — the key and value columns, which
/// are the two that genuinely overflow a narrow window. The full text stays reachable on hover,
/// so nothing is lost, only folded.
fn elided(ui: &mut egui::Ui, w: f32, text: &str, color: egui::Color32) {
    cell(ui, w, |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(color),
            )
            .truncate(),
        )
        .on_hover_text(text);
    });
}

/// The settings table: a STICKY header row drawn outside the scroll area, then the filtered rows,
/// with the inline editor expanding as a row beneath the one being edited.
pub(super) fn settings_table(
    ui: &mut egui::Ui,
    show: &WireSettingsShow,
    filter: SettingsFilter,
    control_armed: bool,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
) {
    let t = Tokens::of(ui.ctx());
    let head = |ui: &mut egui::Ui, w: f32, text: &str| {
        cell(ui, w, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .strong()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(t.theme.text3),
            );
        });
    };
    // ⚠ ONE `Cols` for the header and the rows both. The header is drawn OUTSIDE the scroll area
    // (that is what makes it sticky), so it cannot inherit a measured column — two independent
    // width computations here would put the labels out of register with their columns at every
    // width but one.
    let cols = Cols::for_width(ui.available_width());
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), backend_settings::HEADER_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            head(ui, cols.key, "KEY");
            head(ui, cols.value, "VALUE");
            head(ui, cols.origin, "ORIGIN");
            head(ui, cols.read, "READ");
            head(ui, cols.edit, "");
        },
    );
    ui.separator();

    let rows: Vec<&WireSettingsRow> =
        show.rows.iter().filter(|r| filter == SettingsFilter::All || row_is_set(r)).collect();
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("no rows match this filter")
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body))
                .weak(),
        );
        return;
    }

    let mut open: Option<SettingsEditState> = None;
    egui::ScrollArea::vertical()
        .max_height(backend_settings::ROWS_MAX_H)
        .id_salt("backend-settings-rows")
        .show(ui, |ui| {
            for row in rows {
                let finding = row_is_finding(row);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::MD;
                    elided(ui, cols.key, &row.key, t.theme.text2);
                    // `""` means UNSET on the wire; the CLI prints `-` and so does this.
                    let v = if row.value.is_empty() { "-" } else { row.value.as_str() };
                    elided(ui, cols.value, v, t.theme.text);
                    cell(ui, cols.origin, |ui| {
                        // ⚠ `env:VAR` and `<file>.toml` are DIFFERENT answers and both are on the
                        // wire; only `default` is painted down. Collapsing the first two into
                        // "set" is what would hide the env-outranks-the-row hazard below.
                        let color = match origin_kind(&row.origin) {
                            "env" => Status::Warning.color(),
                            "default" => t.theme.text3,
                            _ => t.theme.text2,
                        };
                        ui.label(
                            egui::RichText::new(&row.origin)
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Caption))
                                .color(color),
                        );
                    });
                    cell(ui, cols.read, |ui| {
                        // THREE values, and the panel neither reduces them to two nor rewords
                        // them: a binary's short name, `yes` (a library reads it — so every
                        // binary linking it does), or `NO`.
                        let color =
                            if row_read_by_nothing(row) { t.theme.text3 } else { t.theme.text2 };
                        ui.label(
                            egui::RichText::new(&row.read_by)
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Caption))
                                .color(color),
                        )
                        .on_hover_text(READ_COLUMN_HINT);
                    });
                    cell(ui, cols.edit, |ui| {
                        if finding {
                            let mark = icons::WARNING
                                .rich()
                                .size(role_px(ui.ctx(), TextRole::Caption))
                                .color(ui.visuals().warn_fg_color);
                            let words =
                                format!("set, and read by nothing — {UNREAD_VERDICT_POINTER}");
                            icons::named(ui.label(mark), &words);
                        }
                        if ui
                            .small_button(
                                egui::RichText::new("edit")
                                    .size(role_px(ui.ctx(), TextRole::Caption)),
                            )
                            .clicked()
                        {
                            open = Some(start_edit(row));
                        }
                    });
                });
                // The editor expands as a row BENEATH the one being edited.
                let editing_this =
                    matches!(edit, SettingsEditState::Editing { key, .. } if key == &row.key);
                let reporting_this = matches!(
                    edit,
                    SettingsEditState::Saving { key }
                        | SettingsEditState::Saved { key, .. }
                        | SettingsEditState::Failed { key, .. }
                    if key == &row.key
                );
                if editing_this || reporting_this {
                    let panel = settings_edit_panel(
                        ui,
                        edit,
                        write,
                        row,
                        show.settings_dir.as_deref(),
                        control_armed,
                    );
                    // ⚠ The panel opens INSIDE this 300pt scroll area, so beneath a row near its
                    // bottom it opened past the lower edge: a panel cut off mid-line and no Save
                    // (measured on the live Backend tab, 2026-09-28). Bring it into view ONCE per
                    // opening — doing it every frame would pin the table and take the scroll
                    // away from the operator for as long as the editor stays open.
                    let shown = ui.ctx().data(|d| d.get_temp::<String>(editor_shown_id()));
                    if shown.as_deref() != Some(row.key.as_str()) {
                        ui.scroll_to_rect(panel.rect, None);
                        ui.ctx().data_mut(|d| d.insert_temp(editor_shown_id(), row.key.clone()));
                    }
                }
            }
        });
    if matches!(edit, SettingsEditState::Idle) {
        ui.ctx().data_mut(|d| d.remove::<String>(editor_shown_id()));
    }
    if let Some(next) = open {
        *edit = next;
    }
}

/// What the READ column means, on hover — the CLI's own legend, because "which binary reads this
/// key" and "is this key in force on the node I am looking at" are different questions and only
/// the first one is answered.
const READ_COLUMN_HINT: &str = "which BINARY reads this key, workspace-wide: a binary's short name, `yes` (a library reads \
     it, so every binary linking it does), or `NO` (nothing reads it). It is NOT a claim about \
     this node — a tradehub row can name a key only the GUI reads.";

/// Which row's editor the section last scrolled into view — so it does that ONCE per opening.
fn editor_shown_id() -> egui::Id {
    egui::Id::unique("backend-settings-editor-shown")
}
