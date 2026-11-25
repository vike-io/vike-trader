//! The chain's toolbar and expiry strip: the `±N strikes` window pill, the expiry choice and the
//! refresh, drawn above the grid ([`super::draw`]).

use egui::RichText;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::chip::badge;
use vike_ui_theme::components::segmented::{ThemeSegment, segmented};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

use super::{OptionChainActions, OptionChainInputs};

/// The `±N strikes` display presets the toolbar pill cycles through (half-window sizes).
pub(crate) const STRIKE_WINDOW_PRESETS: [usize; 4] = [6, 12, 20, 30];

/// Next preset in the cycle after `current` (wraps around); an off-preset value snaps back to 12.
pub(super) fn next_strike_window(current: usize) -> usize {
    match STRIKE_WINDOW_PRESETS.iter().position(|&p| p == current) {
        Some(i) => STRIKE_WINDOW_PRESETS[(i + 1) % STRIKE_WINDOW_PRESETS.len()],
        None => 12,
    }
}

/// Draw phase — the toolbar: provider badge, underlying selector, the `±N strikes` and Refresh pills,
/// then the right-aligned spot · venue · expiry label. A selector or pill click writes the
/// cross-frame selection onto `inp` (and `actions.refresh_clicked`).
pub(super) fn chain_toolbar(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    inp: &mut OptionChainInputs<'_>,
    active_underlying: &str,
    expiry_label: &str,
    spot: Option<f64>,
    actions: &mut OptionChainActions,
) {
    // ---- toolbar: provider · underlying selector · scope pills ···· spot · venue · date ----
    // Wrapped: a window too narrow for the whole row carries the rest onto the next line.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        badge(ui, inp.venue_label, ThemeStatus::Muted);
        // Interactive underlying selector — one segment per fetched underlying. A change sets
        // `underlying_selection`.
        let segments: Vec<ThemeSegment<'_, &str>> = inp
            .underlyings
            .iter()
            .map(|u| ThemeSegment {
                value: u.as_str(),
                label: u.as_str(),
                why: "The underlying this chain shows",
            })
            .collect();
        let mut current = active_underlying;
        if segmented(ui, &mut current, &segments) {
            *inp.underlying_selection = Some(current.to_string());
        }
        badge(ui, "Next 30d", ThemeStatus::Muted);
        // ±N strikes — clickable: cycles the display half-window through the presets. No re-fetch;
        // the chain is fetched wide (±30) and the renderer trims to ±N (see `strike_window_rows`).
        let strike_label = format!("±{} strikes", *inp.strike_window);
        if ui
            .add(ThemeButtonAction::secondary(strike_label.as_str()))
            .on_hover_text("Strikes shown each side of the money — the next preset")
            .clicked()
        {
            *inp.strike_window = next_strike_window(*inp.strike_window);
        }
        // Refresh — its click is the one scope-pill outcome the caller reads (still inert today).
        actions.refresh_clicked =
            ui.add(ThemeButtonAction::secondary((icons::REFRESH, "Refresh"))).clicked();
        // The spot · venue · expiry words end the row at its right edge when the controls leave
        // them the room, else they take a line of their own. A right-to-left layout in whatever
        // room was left painted them over the pills in a narrow window (the owner's ~950pt one).
        let mut words: Vec<egui::WidgetText> = Vec::new();
        if let Some(s) = spot {
            let spot = format!("{} {}", active_underlying, vike_ui_theme::fmt::format_thousands(s));
            words.push(
                RichText::new(spot).font(t.mono(ThemeTextRole::Body)).color(t.theme.text).into(),
            );
        }
        words.push(RichText::new("·").color(t.theme.text_tertiary).into());
        words.push(RichText::new("deribit").color(t.theme.text_secondary).into());
        words.push(RichText::new("·").color(t.theme.text_tertiary).into());
        words.push(RichText::new(expiry_label).color(t.theme.text_secondary).into());
        let words: Vec<_> = words
            .into_iter()
            .map(|w| {
                let extend = Some(egui::TextWrapMode::Extend);
                w.into_galley(ui, extend, f32::INFINITY, egui::FontSelection::Default)
            })
            .collect();
        let gap = ui.spacing().item_spacing.x;
        let need = words.iter().map(|g| g.size().x + gap).sum::<f32>() - gap;
        if ui.available_size_before_wrap().x < need {
            ui.end_row();
        }
        ui.add_space((ui.available_size_before_wrap().x - need).floor().max(0.0));
        for g in words {
            ui.label(g);
        }
    });
    ui.add_space(space::MD);
}

/// Draw phase — the expiry date strip (segments; the active one is the selected label). A click
/// writes `inp.expiry_selection`.
pub(super) fn expiry_strip(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    inp: &mut OptionChainInputs<'_>,
    active: &str,
) {
    // ---- expiry date strip (segments; active = the selected label) ----
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        let segments: Vec<ThemeSegment<'_, &str>> = inp
            .expiries
            .iter()
            .map(|e| ThemeSegment {
                value: e.date.as_str(),
                label: e.label.as_str(),
                why: "The expiry whose strikes the grid shows",
            })
            .collect();
        let mut current = active;
        if segmented(ui, &mut current, &segments) {
            *inp.expiry_selection = Some(current.to_string());
        }
    });
    ui.add_space(space::LG);
}
