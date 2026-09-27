//! The already-free chart/indicator settings dialog bodies split out of
//! `chart.rs` (chart refactor PR-1). `settings_dialog_body` +
//! `indicator_settings_dialog` are driven by `chart::draw`; `seeded_params`
//! is the pure length-normalizer the indicator dialog seeds through. Bodies
//! are verbatim.

use crate::indicators::{Active, LineDash, Source};
use crate::options::{ChartOptions, IndicatorDialog, IndicatorEdit, IndicatorTab, SettingsTab};
use egui::{Color32, RichText};

/// A dim uppercase section header, matching TradingView's grey group labels
/// (CANDLES / DATA MODIFICATION / CHART BASIC STYLES / …).
fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text.to_uppercase()).size(11.0).color(Color32::from_gray(130)));
    ui.add_space(2.0);
}

/// One labelled color-swatch row (label left, swatch right) inside a 2-col grid.
fn color_row(ui: &mut egui::Ui, label: &str, c: &mut [u8; 3]) {
    ui.label(label);
    ui.color_edit_button_srgb(c);
    ui.end_row();
}

/// The "Chart settings" dialog body (chart-UX bundle T6), restructured into
/// TradingView's tabbed layout: a left nav column (Symbol / Canvas / Scales) and
/// the selected section's panel on the right. Mutates the working copy directly —
/// the caller commits it on OK. `tab` is the persisted nav selection.
pub(crate) fn settings_dialog_body(ui: &mut egui::Ui, tab: &mut SettingsTab, w: &mut ChartOptions) {
    ui.horizontal_top(|ui| {
        // Left nav: one selectable per section (TV's vertical tab rail).
        ui.vertical(|ui| {
            ui.set_min_width(130.0);
            for t in SettingsTab::ALL {
                if ui.selectable_label(*tab == t, t.label()).clicked() {
                    *tab = t;
                }
            }
        });
        ui.separator();
        // Right panel: the selected section's controls.
        ui.vertical(|ui| {
            ui.set_min_width(300.0);
            match *tab {
                SettingsTab::Symbol => symbol_panel(ui, w),
                SettingsTab::Canvas => canvas_panel(ui, w),
                SettingsTab::Scales => scales_panel(ui, w),
            }
        });
    });
}

/// TV "Symbol": candle colors + price precision.
fn symbol_panel(ui: &mut egui::Ui, w: &mut ChartOptions) {
    section(ui, "Candles");
    ui.checkbox(&mut w.color_bars_prev_close, "Color bars based on previous close");
    ui.add_space(2.0);
    // Body / Borders / Wick — the three TV up/down color-pair rows (Borders + Wick
    // default-equal to the body colors, so an unedited chart looks the same).
    egui::Grid::new("settings_symbol_colors").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        color_row(ui, "Body up", &mut w.up);
        color_row(ui, "Body down", &mut w.down);
        color_row(ui, "Borders up", &mut w.border_up);
        color_row(ui, "Borders down", &mut w.border_down);
        color_row(ui, "Wick up", &mut w.wick_up);
        color_row(ui, "Wick down", &mut w.wick_down);
    });
    ui.add_space(8.0);
    section(ui, "Data modification");
    ui.horizontal(|ui| {
        ui.label("Precision");
        let mut auto = w.precision.is_none();
        if ui.checkbox(&mut auto, "Auto").changed() {
            w.precision = if auto { None } else { Some(2) };
        }
        ui.add_enabled_ui(!auto, |ui| {
            let mut p = w.precision.unwrap_or(2);
            if ui.add(egui::DragValue::new(&mut p).range(0..=8)).changed() {
                w.precision = Some(p);
            }
        });
    });
}

/// A checkbox + color-swatch row (label-less checkbox left, swatch right) inside a
/// 2-col grid — TV's "☑ Vertical grid  ▢color" shape, where the checkbox toggles
/// visibility and the swatch always edits the (shared) color.
fn toggle_color_row(ui: &mut egui::Ui, label: &str, on: &mut bool, c: &mut [u8; 3]) {
    ui.checkbox(on, label);
    ui.color_edit_button_srgb(c);
    ui.end_row();
}

/// TV "Canvas": background, the vertical/horizontal grid split, crosshair, the
/// series/appearance colors, and the top/bottom chart margins. Mirrors
/// TradingView's Canvas tab (Background / Grid lines / Crosshair / Margins).
fn canvas_panel(ui: &mut egui::Ui, w: &mut ChartOptions) {
    section(ui, "Background");
    // TV "Background" row: a Solid|Gradient mode selector + the stop swatch(es).
    // Solid → one swatch (`bg`). Gradient → the TOP-stop swatch (`bg_top`) then the
    // BOTTOM-stop swatch (`bg`), left-to-right like TradingView's two color chips.
    egui::Grid::new("settings_canvas_bg").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Background");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("bg_mode")
                .selected_text(if w.bg_gradient { "Gradient" } else { "Solid" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut w.bg_gradient, false, "Solid");
                    ui.selectable_value(&mut w.bg_gradient, true, "Gradient");
                });
            if w.bg_gradient {
                ui.color_edit_button_srgb(&mut w.bg_top); // top stop
            }
            ui.color_edit_button_srgb(&mut w.bg); // bottom stop (== the solid color)
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    section(ui, "Grid lines");
    // Master toggle (legacy `show_grid`) gates the whole grid; the per-axis
    // checkboxes AND with it (see `ChartOptions::grid_show`). A single shared grid
    // color drives both axes (vike keeps one grid color, unlike TV's per-axis).
    ui.checkbox(&mut w.show_grid, "Grid");
    ui.add_enabled_ui(w.show_grid, |ui| {
        egui::Grid::new("settings_canvas_grid").num_columns(2).spacing([12.0, 6.0]).show(
            ui,
            |ui| {
                toggle_color_row(ui, "Vertical grid", &mut w.show_vgrid, &mut w.grid);
                toggle_color_row(ui, "Horizontal grid", &mut w.show_hgrid, &mut w.grid);
            },
        );
    });
    ui.add_space(8.0);
    section(ui, "Crosshair");
    egui::Grid::new("settings_canvas_cross").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        color_row(ui, "Crosshair", &mut w.cross);
    });
    ui.add_space(8.0);
    section(ui, "Series colors");
    egui::Grid::new("settings_series_colors").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        color_row(ui, "Up (histogram/markers)", &mut w.up_s);
        color_row(ui, "Down (histogram/markers)", &mut w.down_s);
        color_row(ui, "Line / area", &mut w.line);
    });
    ui.add_space(8.0);
    section(ui, "Margins");
    // Top/bottom % of the visible span reserved as empty room above/below the
    // series when y-autofit is engaged (extent::y_pad). 5% == today's fixed pad.
    egui::Grid::new("settings_canvas_margins").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Top, %");
        ui.add(egui::DragValue::new(&mut w.margin_top_pct).range(0.0..=50.0).speed(0.5));
        ui.end_row();
        ui.label("Bottom, %");
        ui.add(egui::DragValue::new(&mut w.margin_bottom_pct).range(0.0..=50.0).speed(0.5));
        ui.end_row();
    });
}

/// TV "Scales and lines": price labels/lines + the volume pane.
fn scales_panel(ui: &mut egui::Ui, w: &mut ChartOptions) {
    section(ui, "Price labels & lines");
    ui.checkbox(&mut w.show_last_price, "Last price line");
    ui.add_space(8.0);
    section(ui, "Panes");
    ui.checkbox(&mut w.show_volume, "Volume pane");
}

/// Final-review fix: normalize a possibly wrong-length params vec to EXACTLY
/// `spec_params.len()` entries (pad missing with each param's default, drop
/// extras). `Active::params` normally index-aligns with `spec.params`, but
/// T10 workspace persistence applies a persisted vec verbatim
/// (`set_params(ind.params.clone(), &[])`), so an evolved/corrupt workspace
/// file can hand the dialog a vec that's shorter or longer. Pulled out as a
/// pure fn (rather than inlined at the one seed call site) so the
/// length-safety is unit-testable without an `egui::Ui`.
fn seeded_params(raw: &[f64], spec_params: &[vike_indicators::registry::ParamSpec]) -> Vec<f64> {
    (0..spec_params.len()).map(|i| raw.get(i).copied().unwrap_or(spec_params[i].default)).collect()
}

/// The "Indicator settings" dialog (chart-UX bundle T8). Renders the `egui::Window`
/// for `dialog.open_uid` (if set): a `DragValue` per `spec.params` (range/step from
/// the `ParamSpec`) and a colour-edit + width `DragValue` per output line. Edits
/// mutate the dialog's `working` copy; because the `indicators` slice is immutable
/// (the T0 contract), changes are signalled back through `edit_out` for the caller
/// to apply:
/// - a param `DragValue` **release** (`drag_stopped`/`lost_focus`) — debounced,
///   since each apply is an O(history) `set_params` refold;
/// - any colour/width change — immediately (cheap, no refold);
/// - **OK** — the final working copy once more (covers a value typed then
///   OK-clicked in one frame), then close;
/// - **Cancel / window-close** — the on-open SNAPSHOT, restoring params + styles.
///
/// Seeds `working`+`snapshot` from the live `Active` the first frame it's open.
pub(crate) fn indicator_settings_dialog(
    ui: &mut egui::Ui,
    indicators: &[Active],
    dialog: &mut IndicatorDialog,
    edit_out: &mut Option<(u64, IndicatorEdit)>,
) {
    let Some(uid) = dialog.open_uid else { return };
    // Target vanished (e.g. removed while the dialog was open) → close silently.
    let Some(a) = indicators.iter().find(|a| a.uid == uid) else {
        *dialog = IndicatorDialog::default();
        return;
    };
    // Fresh open: seed the working + snapshot edit copies from the live `Active`.
    if dialog.working.is_none() {
        let cur = IndicatorEdit {
            // Final-review fix: length-normalize against `spec.params` (see
            // `seeded_params`) so every `working.params[i]` index below (i in
            // 0..spec.params.len()) is safe even from a stale/corrupt
            // persisted params vec — a bad file must never crash the dialog.
            params: seeded_params(&a.params, a.spec.params),
            source: a.source,
            lines: a.outputs.iter().map(|o| (o.color, o.width, o.visible, o.line_style)).collect(),
            show_bands: a.show_bands,
            bands: a.bands.iter().map(|b| (b.value, b.color, b.show)).collect(),
            show_ob_os_fill: a.show_ob_os_fill,
            ob_fill: a.ob_fill,
            os_fill: a.os_fill,
            visible: a.visible,
        };
        dialog.snapshot = Some(cur.clone());
        dialog.working = Some(cur);
    }
    let tab = dialog.tab;
    let working = dialog.working.as_mut().expect("seeded just above");

    let mut keep_open = true;
    let (mut ok, mut cancel) = (false, false);
    let mut param_released = false; // a param DragValue drag/edit ended this frame
    let mut style_changed = false; // a colour/width/visible/band edit this frame
    let mut source_changed = false; // the "Source" selector changed this frame (refold)
    let mut new_tab = tab;
    egui::Window::new(format!("{} settings", a.spec.pretty))
        .id(ui.id().with(("indicator_settings", uid)))
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .default_width(300.0)
        .order(egui::Order::Foreground) // float above the chart window that spawned it
        .show(ui.ctx(), |ui| {
            // Top tab rail — TradingView's oscillator layout (Inputs / Style / Visibility).
            ui.horizontal(|ui| {
                for t in IndicatorTab::ALL {
                    if ui.selectable_label(tab == t, t.label()).clicked() {
                        new_tab = t;
                    }
                }
            });
            ui.separator();
            match tab {
                IndicatorTab::Inputs => {
                    // TradingView "Source" selector (chart source selector): the price
                    // series the indicator computes off, ABOVE the numeric params.
                    // Present for EVERY indicator — even paramless ones (Vwap/Obv) still
                    // carry this one input, so it replaces the old "no inputs" text.
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        egui::ComboBox::from_id_salt(("ind_source", uid))
                            .selected_text(working.source.label())
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for opt in Source::ALL {
                                    if ui
                                        .selectable_value(&mut working.source, opt, opt.label())
                                        .changed()
                                    {
                                        source_changed = true;
                                    }
                                }
                            });
                    });
                    ui.add_space(4.0);
                    egui::Grid::new("ind_params").num_columns(2).spacing([12.0, 6.0]).show(
                        ui,
                        |ui| {
                            for (i, ps) in a.spec.params.iter().enumerate() {
                                ui.label(ps.name);
                                // Integer periods (step ≥ 1) show 0 dp; fractional params 3 dp.
                                let dp = if ps.step >= 1.0 { 0 } else { 3 };
                                let resp = ui.add(
                                    egui::DragValue::new(&mut working.params[i])
                                        .range(ps.min..=ps.max)
                                        .speed(ps.step)
                                        .max_decimals(dp),
                                );
                                // Release-only: a live `changed()` would refold every drag frame.
                                if resp.drag_stopped() || resp.lost_focus() {
                                    param_released = true;
                                }
                                ui.end_row();
                            }
                        },
                    );
                }
                IndicatorTab::Style => {
                    section(ui, "Plots");
                    egui::Grid::new("ind_lines").num_columns(5).spacing([10.0, 6.0]).show(
                        ui,
                        |ui| {
                            for (i, out) in a.outputs.iter().enumerate() {
                                let (col, wid, vis, ls) = &mut working.lines[i];
                                // Per-plot show/hide (TradingView "Style" checkbox).
                                if ui.checkbox(vis, "").changed() {
                                    style_changed = true;
                                }
                                ui.label(out.name);
                                let mut rgb = [col.r(), col.g(), col.b()];
                                if ui.color_edit_button_srgb(&mut rgb).changed() {
                                    *col = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                    style_changed = true;
                                }
                                if ui
                                    .add(
                                        egui::DragValue::new(wid)
                                            .range(0.5..=6.0)
                                            .speed(0.1)
                                            .max_decimals(1),
                                    )
                                    .changed()
                                {
                                    style_changed = true;
                                }
                                // Per-plot dash pattern (TradingView "Style" line-style
                                // picker: Solid / Dashed / Dotted).
                                egui::ComboBox::from_id_salt(("ind_line_dash", uid, i))
                                    .selected_text(ls.label())
                                    .width(78.0)
                                    .show_ui(ui, |ui| {
                                        for opt in LineDash::ALL {
                                            if ui.selectable_value(ls, opt, opt.label()).changed() {
                                                style_changed = true;
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        },
                    );
                    // Reference bands (RSI 30/50/70, stoch 20/80, …) — a master show
                    // toggle plus a PER-LEVEL row (show checkbox + level value + colour
                    // swatch), matching TradingView's Upper/Middle/Lower band rows, and an
                    // overbought/oversold translucent-fill toggle. Only shown for
                    // oscillators that define bands (`spec.bands` non-empty).
                    if !a.spec.bands.is_empty() {
                        section(ui, "Bands");
                        if ui.checkbox(&mut working.show_bands, "Show levels").changed() {
                            style_changed = true;
                        }
                        ui.add_enabled_ui(working.show_bands, |ui| {
                            egui::Grid::new("ind_bands").num_columns(3).spacing([10.0, 6.0]).show(
                                ui,
                                |ui| {
                                    for (value, col, show) in working.bands.iter_mut() {
                                        // Per-level show/hide (TradingView per-band checkbox).
                                        if ui.checkbox(show, "").changed() {
                                            style_changed = true;
                                        }
                                        ui.label(format!("{value:.0}"));
                                        let mut rgb = [col.r(), col.g(), col.b()];
                                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                                            *col = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                            style_changed = true;
                                        }
                                        ui.end_row();
                                    }
                                },
                            );
                            // Overbought/oversold translucent fill (green above the top
                            // band, red below the bottom) + its two editable fill colours.
                            if ui
                                .checkbox(&mut working.show_ob_os_fill, "Overbought/oversold fill")
                                .changed()
                            {
                                style_changed = true;
                            }
                            ui.add_enabled_ui(working.show_ob_os_fill, |ui| {
                                ui.horizontal(|ui| {
                                    let mut ob = [
                                        working.ob_fill.r(),
                                        working.ob_fill.g(),
                                        working.ob_fill.b(),
                                    ];
                                    if ui.color_edit_button_srgb(&mut ob).changed() {
                                        working.ob_fill = Color32::from_rgba_unmultiplied(
                                            ob[0],
                                            ob[1],
                                            ob[2],
                                            working.ob_fill.a(),
                                        );
                                        style_changed = true;
                                    }
                                    ui.weak("overbought");
                                    let mut os = [
                                        working.os_fill.r(),
                                        working.os_fill.g(),
                                        working.os_fill.b(),
                                    ];
                                    if ui.color_edit_button_srgb(&mut os).changed() {
                                        working.os_fill = Color32::from_rgba_unmultiplied(
                                            os[0],
                                            os[1],
                                            os[2],
                                            working.os_fill.a(),
                                        );
                                        style_changed = true;
                                    }
                                    ui.weak("oversold");
                                });
                            });
                        });
                    }
                }
                IndicatorTab::Visibility => {
                    section(ui, "Visibility");
                    if ui.checkbox(&mut working.visible, "Show on chart").changed() {
                        style_changed = true;
                    }
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    dialog.tab = new_tab;

    // Clone the working copy out BEFORE mutating `dialog` (ends its borrow).
    let working = dialog.working.as_mut().expect("seeded above");
    let working_now = working.clone();
    if param_released || style_changed || source_changed {
        *edit_out = Some((uid, working_now.clone()));
    }
    if ok {
        *edit_out = Some((uid, working_now)); // apply the final working copy, then close
        *dialog = IndicatorDialog::default();
    } else if cancel || !keep_open {
        if let Some(snap) = dialog.snapshot.take() {
            *edit_out = Some((uid, snap)); // restore the on-open snapshot (params + styles)
        }
        *dialog = IndicatorDialog::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Final-review fix: the indicator settings dialog must not OOB-panic
    // when a persisted (T10) params vec no longer matches `spec.params.len()`
    // (an evolved/corrupt workspace file). `seeded_params` is the pure
    // normalization the dialog's seed site delegates to. ---

    #[test]
    fn seeded_params_pads_a_too_short_vec_with_spec_defaults() {
        // macd = fast/slow/signal, 3 params — a persisted 1-entry vec (e.g.
        // from an older workspace format) must pad the missing 2 with the
        // CURRENT spec's defaults, never panic on out-of-range indexing.
        let spec = vike_indicators::get("macd").unwrap();
        assert_eq!(spec.params.len(), 3);
        let raw = [7.0]; // only "fast" persisted
        let seeded = seeded_params(&raw, spec.params);
        assert_eq!(seeded.len(), spec.params.len());
        assert_eq!(seeded[0], 7.0); // present entry preserved
        assert_eq!(seeded[1], spec.params[1].default);
        assert_eq!(seeded[2], spec.params[2].default);
    }

    #[test]
    fn seeded_params_truncates_a_too_long_vec() {
        let spec = vike_indicators::get("sma").unwrap(); // 1 param
        assert_eq!(spec.params.len(), 1);
        let raw = [9.0, 99.0, 999.0]; // stale extra entries
        let seeded = seeded_params(&raw, spec.params);
        assert_eq!(seeded, vec![9.0]);
    }

    #[test]
    fn seeded_params_exact_length_passes_through_unchanged() {
        let spec = vike_indicators::get("macd").unwrap();
        let raw = [12.0, 26.0, 9.0];
        assert_eq!(seeded_params(&raw, spec.params), raw.to_vec());
    }
}
