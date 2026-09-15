//! One shared instrument search-result row — the widget BOTH cross-venue symbol pickers paint:
//! the chart window's title-bar symbol dropdown (`vike-app`'s `chart_window.rs`, which re-imports
//! it under its original bare name) and the ƒx picker's per-study "source symbol" menu
//! ([`crate::tool_views::fx_picker_popup`]).
//!
//! It moved down here from `vike-app` when the ƒx picker did (tool-view extraction batch 2):
//! keeping it in the CI-excluded binary would have meant either a second copy or threading a
//! callback through the moved popup. Pure `egui` painting over a `vike_catalog::Instrument` and
//! the shared palette — no eframe/wgpu, no app-local types — so it builds on CI like the rest of
//! this crate.

use vike_catalog::Instrument;
use vike_ui_theme::palette as theme;

/// One full-width instrument search-result row, rendered `raw_symbol | description | venue`: the
/// symbol on the left, the (possibly-empty) description dim in the middle, and a dim VENUE tag on
/// the right. Hover-highlighted and click-sensed across the whole row — the same allocate/paint
/// idiom as the chart-style menu rows. Returns `true` when clicked. `selected` tints the
/// symbol ACCENT. The venue tag is what makes a cross-venue result unambiguous (two `BTCUSDT` rows
/// tagged `BINANCE` vs `BYBIT`). Ranking/filtering itself lives in `Catalog::search` (the
/// tiered logic ported from the retired `filter_symbols`).
pub fn search_result_row(ui: &mut egui::Ui, selected: bool, info: &Instrument) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(230.0), 22.0),
        egui::Sense::click(),
    );
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, theme::HOVER);
    }
    let sym_col = if selected { theme::ACCENT } else { theme::TEXT_UI };
    let dim = egui::Color32::from_rgb(120, 128, 140);
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &info.raw_symbol,
        egui::FontId::proportional(13.0),
        sym_col,
    );
    // Middle column: the human description (empty for the crypto venues today — then nothing
    // paints, which is fine). Left-aligned at a fixed inset so it doesn't collide with the symbol.
    if !info.description.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + 110.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &info.description,
            egui::FontId::proportional(11.0),
            dim,
        );
    }
    ui.painter().text(
        egui::pos2(rect.right() - 8.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        info.venue.to_uppercase(),
        egui::FontId::proportional(11.0),
        dim,
    );
    resp.clicked()
}
