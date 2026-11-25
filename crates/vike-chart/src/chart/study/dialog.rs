//! The "Indicator settings" dialog (chart-UX bundle T8), split out of `chart.rs` (chart refactor
//! PR-1): `indicator_settings_dialog` is driven by `chart::draw`; `seeded_params` is the pure
//! length-normalizer it seeds through. Bodies are verbatim; the group heading and the colour well
//! are the "Chart settings" dialog's (`crate::chart::settings::dialog`).

use crate::chart::colors::ChartColors;
use crate::chart::settings::dialog::{section, swatch};
use crate::chart::study::{
    ChartStudy, ChartStudyDialog, ChartStudyDialogTab, ChartStudyEdit, ChartStudyLineDash,
    ChartStudySource,
};
use vike_ui_theme::color::{rgb, with_alpha};
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::tabs::{ThemeTab, underline};
use vike_ui_theme::components::toggle::checkbox;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::chart_dialogs;

/// Final-review fix: normalize a possibly wrong-length params vec to EXACTLY
/// `spec_params.len()` entries (pad missing with each param's default, drop
/// extras). `ChartStudy::params` normally index-aligns with `spec.params`, but
/// T10 workspace persistence applies a persisted vec verbatim
/// (`set_params(indicator.params.clone(), &[])`), so an evolved/corrupt workspace
/// file can hand the dialog a vec that's shorter or longer. Pulled out as a
/// pure fn (rather than inlined at the one seed call site) so the
/// length-safety is unit-testable without an `egui::Ui`.
fn seeded_params(raw: &[f64], spec_params: &[vike_indicators::ParamSpec]) -> Vec<f64> {
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
/// Seeds `working`+`snapshot` from the live `ChartStudy` the first frame it's open. `colors` is what
/// the chart paints with this frame: an oscillator level with no colour chosen shows its
/// `level_line` in the Bands swatch.
pub(crate) fn indicator_settings_dialog(
    ui: &mut egui::Ui,
    indicators: &[ChartStudy],
    dialog: &mut ChartStudyDialog,
    colors: &ChartColors,
    edit_out: &mut Option<(u64, ChartStudyEdit)>,
) {
    let Some(uid) = dialog.open_uid else { return };
    // Target vanished (e.g. removed while the dialog was open) → close silently.
    let Some(a) = indicators.iter().find(|a| a.uid == uid) else {
        *dialog = ChartStudyDialog::default();
        return;
    };
    // Fresh open: seed the working + snapshot edit copies from the live `ChartStudy`.
    if dialog.working.is_none() {
        let current = ChartStudyEdit {
            // Final-review fix: length-normalize against `spec.params` (see
            // `seeded_params`) so every `working.params[i]` index below (i in
            // 0..spec.params.len()) is safe even from a stale/corrupt
            // persisted params vec — a bad file must never crash the dialog.
            params: seeded_params(&a.params, a.spec.params),
            source: a.source,
            lines: a.outputs.iter().map(|o| (o.color, o.width, o.visible, o.line_style)).collect(),
            show_bands: a.show_bands,
            bands: a.bands.iter().map(|b| (b.value, b.color, b.show)).collect(),
            show_overbought_oversold_fill: a.show_overbought_oversold_fill,
            overbought_fill: a.overbought_fill,
            oversold_fill: a.oversold_fill,
            visible: a.visible,
        };
        dialog.snapshot = Some(current.clone());
        dialog.working = Some(current);
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
        .id(ui.scope_id().with(("indicator_settings", uid)))
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .default_width(chart_dialogs::INDICATOR_DIALOG_WIDTH)
        .order(egui::Order::Foreground) // float above the chart window that spawned it
        .show(ui.ctx(), |ui| {
            // Top tab rail — TradingView's oscillator layout (Inputs / Style / Visibility).
            let tabs: Vec<ThemeTab<'_, ChartStudyDialogTab>> = ChartStudyDialogTab::ALL
                .iter()
                .map(|s| ThemeTab { value: *s, label: s.label(), count: None })
                .collect();
            underline(ui, &mut new_tab, &tabs);
            ui.separator();
            match tab {
                ChartStudyDialogTab::Inputs => {
                    // TradingView "Source" selector (chart source selector): the price
                    // series the indicator computes off, ABOVE the numeric params.
                    // Present for EVERY indicator — even paramless ones (Vwap/Obv) still
                    // carry this one input, so it replaces the old "no inputs" text.
                    ui.horizontal(|ui| {
                        let name = ui.label("Source").id;
                        egui::ComboBox::from_id_salt(("ind_source", uid))
                            .selected_text(working.source.label())
                            .width(chart_dialogs::SOURCE_COMBO_WIDTH)
                            .show_ui(ui, |ui| {
                                for opt in ChartStudySource::ALL {
                                    if ui
                                        .selectable_value(&mut working.source, opt, opt.label())
                                        .changed()
                                    {
                                        source_changed = true;
                                    }
                                }
                            })
                            .response
                            .labelled_by(name);
                    });
                    ui.add_space(space::SM);
                    egui::Grid::new("ind_params")
                        .num_columns(2)
                        .spacing([space::XL, space::MD])
                        .show(ui, |ui| {
                            for (i, ps) in a.spec.params.iter().enumerate() {
                                let name = ui.label(ps.name).id;
                                // Integer periods (step ≥ 1) show 0 dp; fractional params 3 dp.
                                let dp = if ps.step >= 1.0 { 0 } else { 3 };
                                let resp = ui
                                    .add(
                                        egui::DragValue::new(&mut working.params[i])
                                            .range(ps.min..=ps.max)
                                            .speed(ps.step)
                                            .max_decimals(dp),
                                    )
                                    .labelled_by(name);
                                // Release-only: a live `changed()` would refold every drag frame.
                                if resp.drag_stopped() || resp.lost_focus() {
                                    param_released = true;
                                }
                                ui.end_row();
                            }
                        });
                }
                ChartStudyDialogTab::Style => {
                    section(ui, "Plots");
                    egui::Grid::new("ind_lines")
                        .num_columns(5)
                        .spacing([space::XL, space::MD])
                        .show(ui, |ui| {
                            for (i, out) in a.outputs.iter().enumerate() {
                                let (col, wid, vis, ls) = &mut working.lines[i];
                                // Per-plot show/hide (TradingView "Style" checkbox). Every
                                // control of the row is named by the plot's label beside it.
                                let shown = checkbox(ui, vis, "");
                                let name = ui.label(out.name).id;
                                if shown.labelled_by(name).changed() {
                                    style_changed = true;
                                }
                                let mut picked = [col.r(), col.g(), col.b()];
                                if ui
                                    .color_edit_button_srgb(&mut picked)
                                    .labelled_by(name)
                                    .changed()
                                {
                                    *col = rgb(picked);
                                    style_changed = true;
                                }
                                if ui
                                    .add(
                                        egui::DragValue::new(wid)
                                            .range(0.5..=6.0)
                                            .speed(0.1)
                                            .max_decimals(1),
                                    )
                                    .labelled_by(name)
                                    .changed()
                                {
                                    style_changed = true;
                                }
                                // Per-plot dash pattern (TradingView "Style" line-style
                                // picker: Solid / Dashed / Dotted).
                                egui::ComboBox::from_id_salt(("ind_line_dash", uid, i))
                                    .selected_text(ls.label())
                                    .width(chart_dialogs::LINE_DASH_COMBO_WIDTH)
                                    .show_ui(ui, |ui| {
                                        for opt in ChartStudyLineDash::ALL {
                                            if ui.selectable_value(ls, opt, opt.label()).changed() {
                                                style_changed = true;
                                            }
                                        }
                                    })
                                    .response
                                    .labelled_by(name);
                                ui.end_row();
                            }
                        });
                    // Reference bands (RSI 30/50/70, stoch 20/80, …) — a master show
                    // toggle plus a PER-LEVEL row (show checkbox + level value + colour
                    // swatch), matching TradingView's Upper/Middle/Lower band rows, and an
                    // overbought/oversold translucent-fill toggle. Only shown for
                    // oscillators that define bands (`spec.bands` non-empty).
                    if !a.spec.bands.is_empty() {
                        section(ui, "Bands");
                        if checkbox(ui, &mut working.show_bands, "Show levels").changed() {
                            style_changed = true;
                        }
                        ui.add_enabled_ui(working.show_bands, |ui| {
                            egui::Grid::new("ind_bands")
                                .num_columns(3)
                                .spacing([space::XL, space::MD])
                                .show(ui, |ui| {
                                    for (value, col, show) in working.bands.iter_mut() {
                                        // Per-level show/hide (TradingView per-band checkbox),
                                        // named, like the well, by the level's label beside it.
                                        let shown = checkbox(ui, show, "");
                                        let name = ui.label(format!("{value:.0}")).id;
                                        if shown.labelled_by(name).changed() {
                                            style_changed = true;
                                        }
                                        // The well shows what the level paints: the theme's level
                                        // line until the user picks a colour, and the revert
                                        // button hands it back (the `swatch` override shape).
                                        let before = col.map(|c| [c.r(), c.g(), c.b()]);
                                        let mut slot = before;
                                        swatch(
                                            ui,
                                            &mut slot,
                                            col.unwrap_or(colors.level_line),
                                            name,
                                        );
                                        if slot != before {
                                            *col = slot.map(rgb);
                                            style_changed = true;
                                        }
                                        ui.end_row();
                                    }
                                });
                            // Overbought/oversold translucent fill (green above the top
                            // band, red below the bottom) + its two editable fill colours.
                            if checkbox(
                                ui,
                                &mut working.show_overbought_oversold_fill,
                                "Overbought/oversold fill",
                            )
                            .changed()
                            {
                                style_changed = true;
                            }
                            ui.add_enabled_ui(working.show_overbought_oversold_fill, |ui| {
                                // Each well is named by the word printed after it.
                                ui.horizontal(|ui| {
                                    let mut overbought_rgb = [
                                        working.overbought_fill.r(),
                                        working.overbought_fill.g(),
                                        working.overbought_fill.b(),
                                    ];
                                    let well = ui.color_edit_button_srgb(&mut overbought_rgb);
                                    let name = ui.weak("overbought").id;
                                    if well.labelled_by(name).changed() {
                                        working.overbought_fill = with_alpha(
                                            rgb(overbought_rgb),
                                            working.overbought_fill.a(),
                                        );
                                        style_changed = true;
                                    }
                                    let mut oversold_rgb = [
                                        working.oversold_fill.r(),
                                        working.oversold_fill.g(),
                                        working.oversold_fill.b(),
                                    ];
                                    let well = ui.color_edit_button_srgb(&mut oversold_rgb);
                                    let name = ui.weak("oversold").id;
                                    if well.labelled_by(name).changed() {
                                        working.oversold_fill = with_alpha(
                                            rgb(oversold_rgb),
                                            working.oversold_fill.a(),
                                        );
                                        style_changed = true;
                                    }
                                });
                            });
                        });
                    }
                }
                ChartStudyDialogTab::Visibility => {
                    section(ui, "Visibility");
                    if checkbox(ui, &mut working.visible, "Show on chart").changed() {
                        style_changed = true;
                    }
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.add(ThemeButtonAction::primary("OK")).clicked();
                cancel = ui.add(ThemeButtonAction::secondary("Cancel")).clicked();
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
        *dialog = ChartStudyDialog::default();
    } else if cancel || !keep_open {
        if let Some(snapshot) = dialog.snapshot.take() {
            *edit_out = Some((uid, snapshot)); // restore the on-open snapshot (params + styles)
        }
        *dialog = ChartStudyDialog::default();
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
