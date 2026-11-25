//! The "Chart settings" dialog body, split out of `chart.rs` (chart refactor PR-1):
//! `settings_dialog_and_options` is the phase `chart::draw` runs, and `settings_dialog_body` the
//! tabbed body under it. The group heading (`section`) and the colour well (`swatch`) are shared
//! with the "Indicator settings" dialog (`crate::chart::study::dialog`). Bodies are verbatim.

use crate::chart::colors::ChartColors;
use crate::chart::settings::{ChartSettings, ChartSettingsDialog, ChartSettingsDialogTab};
use egui::{Color32, RichText};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::{ThemeButtonAction, ThemeButtonIcon};
use vike_ui_theme::components::segmented::{ThemeSegment, segmented};
use vike_ui_theme::components::tabs::{ThemeTab, underline};
use vike_ui_theme::components::toggle::checkbox;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::chart_dialogs;

/// A group heading: the uppercase Caption line TradingView's settings use (CANDLES, GRID LINES).
pub(crate) fn section(ui: &mut egui::Ui, text: &str) {
    let t = ThemeTokens::of(ui.ctx());
    ui.add_space(space::SM);
    ui.label(
        RichText::new(text.to_uppercase())
            .font(t.font(ThemeTextRole::Caption))
            .color(t.theme.text_tertiary),
    );
    ui.add_space(space::XS);
}

/// One labelled color-swatch row (label left, swatch right) inside a 2-col grid. `slot` is the
/// window's override and `shown` the colour the chart paints for it this frame.
fn color_row(ui: &mut egui::Ui, label: &str, slot: &mut Option<[u8; 3]>, shown: Color32) {
    let name = ui.label(label).id;
    swatch(ui, slot, shown, name);
    ui.end_row();
}

/// A colour well over an override: it shows the colour the chart paints — the theme's or the
/// market set's until the user picks one — and a pick makes it this chart's own. While it is the
/// user's, a revert button beside it hands it back (spec §3.5).
///
/// ⚠ The revert is offered only while `ui.is_enabled()` — never inside a disabled `Ui` (the Grid
/// lines section's master-off gate) without checking first. The kit's `ThemeButtonIcon` debug-panics
/// on exactly that (`refuse_a_silent_disable`, Part B's `disabled_because` contract): a button
/// disabled by an ANCESTOR rather than its own `disabled_because` is a control the kit refuses to
/// paint silently, and a revert beside a swatch nobody can operate would be a control with
/// nothing behind it anyway (§4.2) — so the guard is `is_enabled()` first, never a caught panic.
///
/// The row is as tall as the revert button (`control_height`) whether or not the button is there: the
/// colour well alone is `interact_size.y`, 3 pt shorter, so a row that GAINED the button on a pick
/// grew by 3 pt and the whole dialog jumped under the pointer.
///
/// `name` is the widget that names the well in the accessibility tree: the row's visible label.
pub(crate) fn swatch(
    ui: &mut egui::Ui,
    slot: &mut Option<[u8; 3]>,
    shown: Color32,
    name: egui::Id,
) {
    let row_h = ThemeTokens::of(ui.ctx()).metrics.control_height;
    ui.horizontal(|ui| {
        ui.set_min_height(row_h);
        let mut rgb = [shown.r(), shown.g(), shown.b()];
        if ui.color_edit_button_srgb(&mut rgb).labelled_by(name).changed() {
            *slot = Some(rgb);
        }
        if slot.is_some()
            && ui.is_enabled()
            && ui.add(ThemeButtonIcon::new(icons::REVERT, "Use the theme's colour")).clicked()
        {
            *slot = None;
        }
    });
}

/// The "Chart settings" dialog body (chart-UX bundle T6), restructured into
/// TradingView's tabbed layout: a left navigation column (Symbol / Canvas / Scales) and
/// the selected section's panel on the right. Mutates the working copy directly —
/// the caller commits it on OK. `tab` is the persisted navigation selection.
pub(crate) fn settings_dialog_body(
    ui: &mut egui::Ui,
    tab: &mut ChartSettingsDialogTab,
    w: &mut ChartSettings,
) {
    let tabs: Vec<ThemeTab<'_, ChartSettingsDialogTab>> = ChartSettingsDialogTab::ALL
        .iter()
        .map(|s| ThemeTab { value: *s, label: s.label(), count: None })
        .collect();
    underline(ui, tab, &tabs);
    ui.add_space(space::LG);
    // What the chart paints for each colour this frame: an unset one shows the theme's or the
    // market set's (spec §3.5).
    let shown = ChartColors::resolve(w, &vike_ui_theme::appearance::current(ui.ctx()));
    ui.vertical(|ui| {
        ui.set_min_width(chart_dialogs::SETTINGS_MIN_WIDTH);
        match *tab {
            ChartSettingsDialogTab::Symbol => symbol_panel(ui, w, &shown),
            ChartSettingsDialogTab::Canvas => canvas_panel(ui, w, &shown),
            ChartSettingsDialogTab::Scales => scales_panel(ui, w),
        }
    });
}

/// TV "Symbol": candle colors + price precision.
fn symbol_panel(ui: &mut egui::Ui, w: &mut ChartSettings, shown: &ChartColors) {
    section(ui, "Candles");
    checkbox(ui, &mut w.color_bars_previous_close, "Color bars based on previous close");
    ui.add_space(space::XS);
    // Body / Borders / Wick — the three TV up/down color-pair rows (unset Borders + Wick
    // follow the body colours, so an unedited chart draws one colour per candle).
    egui::Grid::new("settings_symbol_colors").num_columns(2).spacing([space::XL, space::MD]).show(
        ui,
        |ui| {
            color_row(ui, "Body up", &mut w.up, shown.up);
            color_row(ui, "Body down", &mut w.down, shown.down);
            color_row(ui, "Borders up", &mut w.border_up, shown.border_up);
            color_row(ui, "Borders down", &mut w.border_down, shown.border_down);
            color_row(ui, "Wick up", &mut w.wick_up, shown.wick_up);
            color_row(ui, "Wick down", &mut w.wick_down, shown.wick_down);
        },
    );
    ui.add_space(space::LG);
    section(ui, "Data modification");
    ui.horizontal(|ui| {
        let name = ui.label("Precision").id;
        let mut auto = w.precision.is_none();
        if checkbox(ui, &mut auto, "Auto").changed() {
            w.precision = if auto { None } else { Some(2) };
        }
        ui.add_enabled_ui(!auto, |ui| {
            let mut p = w.precision.unwrap_or(2);
            if ui.add(egui::DragValue::new(&mut p).range(0..=8)).labelled_by(name).changed() {
                w.precision = Some(p);
            }
        });
    });
}

/// A checkbox + color-swatch row (label-less checkbox left, swatch right) inside a
/// 2-col grid — TV's "☑ Vertical grid  ▢color" shape, where the checkbox toggles
/// visibility and the swatch always edits the (shared) color.
fn toggle_color_row(
    ui: &mut egui::Ui,
    label: &str,
    on: &mut bool,
    slot: &mut Option<[u8; 3]>,
    shown: Color32,
) {
    let name = checkbox(ui, on, label).id;
    swatch(ui, slot, shown, name);
    ui.end_row();
}

/// Solid or gradient canvas (TV "Background").
const BG_MODES: [ThemeSegment<'static, bool>; 2] = [
    ThemeSegment { value: false, label: "Solid", why: "One colour behind every pane" },
    ThemeSegment {
        value: true,
        label: "Gradient",
        why: "The top colour fading into the bottom one",
    },
];

/// TV "Canvas": background, the vertical/horizontal grid split, crosshair, the
/// series/appearance colors, and the top/bottom chart margins. Mirrors
/// TradingView's Canvas tab (Background / Grid lines / Crosshair / Margins).
fn canvas_panel(ui: &mut egui::Ui, w: &mut ChartSettings, shown: &ChartColors) {
    section(ui, "Background");
    // TV "Background" row: a Solid|Gradient mode selector + the stop swatch(es).
    // Solid → one swatch (`background`). Gradient → the TOP-stop swatch (`background_top`) then the
    // BOTTOM-stop swatch (`bg`), left-to-right like TradingView's two color chips.
    egui::Grid::new("settings_canvas_bg").num_columns(2).spacing([space::XL, space::MD]).show(
        ui,
        |ui| {
            let name = ui.label("Background").id;
            ui.horizontal(|ui| {
                segmented(ui, &mut w.background_gradient, &BG_MODES);
                if w.background_gradient {
                    swatch(ui, &mut w.background_top, shown.background_top, name); // top stop
                }
                // bottom stop (== the solid color)
                swatch(ui, &mut w.background, shown.background, name);
            });
            ui.end_row();
        },
    );
    ui.add_space(space::LG);
    section(ui, "Grid lines");
    // Master toggle (legacy `show_grid`) gates the whole grid; the per-axis
    // checkboxes AND with it (see `ChartSettings::grid_show`). A single shared grid
    // color drives both axes (vike keeps one grid color, unlike TV's per-axis).
    checkbox(ui, &mut w.show_grid, "Grid");
    ui.add_enabled_ui(w.show_grid, |ui| {
        egui::Grid::new("settings_canvas_grid")
            .num_columns(2)
            .spacing([space::XL, space::MD])
            .show(ui, |ui| {
                toggle_color_row(
                    ui,
                    "Vertical grid",
                    &mut w.show_vertical_grid,
                    &mut w.grid,
                    shown.grid,
                );
                toggle_color_row(
                    ui,
                    "Horizontal grid",
                    &mut w.show_horizontal_grid,
                    &mut w.grid,
                    shown.grid,
                );
            });
    });
    ui.add_space(space::LG);
    section(ui, "Crosshair");
    egui::Grid::new("settings_canvas_cross").num_columns(2).spacing([space::XL, space::MD]).show(
        ui,
        |ui| {
            color_row(ui, "Crosshair", &mut w.crosshair, shown.cross);
        },
    );
    ui.add_space(space::LG);
    section(ui, "Series colors");
    egui::Grid::new("settings_series_colors").num_columns(2).spacing([space::XL, space::MD]).show(
        ui,
        |ui| {
            color_row(ui, "Up (histogram/markers)", &mut w.up_graphic, shown.up_graphic);
            color_row(ui, "Down (histogram/markers)", &mut w.down_graphic, shown.down_graphic);
            color_row(ui, "Line / area", &mut w.line, shown.line);
        },
    );
    ui.add_space(space::LG);
    section(ui, "Margins");
    // Top/bottom % of the visible span reserved as empty room above/below the
    // series when y-autofit is engaged (extent::y_pad). 5% == today's fixed pad.
    egui::Grid::new("settings_canvas_margins").num_columns(2).spacing([space::XL, space::MD]).show(
        ui,
        |ui| {
            let name = ui.label("Top, %").id;
            ui.add(egui::DragValue::new(&mut w.margin_top_percent).range(0.0..=50.0).speed(0.5))
                .labelled_by(name);
            ui.end_row();
            let name = ui.label("Bottom, %").id;
            ui.add(egui::DragValue::new(&mut w.margin_bottom_percent).range(0.0..=50.0).speed(0.5))
                .labelled_by(name);
            ui.end_row();
        },
    );
}

/// TV "Scales and lines": price labels/lines + the volume pane.
fn scales_panel(ui: &mut egui::Ui, w: &mut ChartSettings) {
    section(ui, "Price labels & lines");
    checkbox(ui, &mut w.show_last_price, "Last price line");
    ui.add_space(space::LG);
    section(ui, "Panes");
    checkbox(ui, &mut w.show_volume, "Volume pane");
}

/// Draw phase — the "Chart settings" dialog, drawn FIRST so this frame's edits to
/// `settings.working` drive the body's live preview. Returns this frame's EFFECTIVE options (the
/// dialog's working copy while it is open and not abandoned, else the committed set) and, when OK
/// was pressed, the new committed set the caller applies to `WindowState::options`.
pub(crate) fn settings_dialog_and_options(
    ui: &mut egui::Ui,
    settings: &mut ChartSettingsDialog,
    options: &ChartSettings,
) -> (ChartSettings, Option<ChartSettings>) {
    let mut options_change: Option<ChartSettings> = None;
    let dialog_was_open = settings.open; // frame-START state — drives THIS frame's effective options
    let mut abandoned = false; // Cancel / window-close: discard the working copy this frame
    if settings.open {
        let mut keep_open = true;
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("Chart settings")
            .id(ui.scope_id().with("chart_settings"))
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .default_width(chart_dialogs::CHART_SETTINGS_WIDTH)
            .order(egui::Order::Foreground) // float above the chart window that spawned it
            .show(ui.ctx(), |ui| {
                settings_dialog_body(ui, &mut settings.tab, &mut settings.working);
                ui.separator();
                ui.horizontal(|ui| {
                    ok = ui.add(ThemeButtonAction::primary("OK")).clicked();
                    cancel = ui.add(ThemeButtonAction::secondary("Cancel")).clicked();
                });
            });
        if ok {
            options_change = Some(settings.working.clone());
        } else if cancel || !keep_open {
            abandoned = true;
        }
        if ok || cancel || !keep_open {
            settings.open = false;
        }
    }
    // EFFECTIVE options for THIS frame: the dialog's working copy when it was
    // open AND not abandoned (live preview — OK's preview flows straight into
    // the committed value with no flicker), else the committed set. Cloned
    // (owned) so no borrow of `settings`/`options` lingers into the
    // `&mut settings` gear toggle below.
    let effective_options: ChartSettings =
        if dialog_was_open && !abandoned { settings.working.clone() } else { options.clone() };
    (effective_options, options_change)
}
