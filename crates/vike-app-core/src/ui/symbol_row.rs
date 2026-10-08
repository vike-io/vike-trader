//! One shared instrument search-result row — the widget BOTH cross-venue symbol pickers paint:
//! the chart window's title-bar symbol dropdown (`crates/vike-desktop/src/chart_window.rs`, which
//! re-imports it under its original bare name) and the ƒx picker's per-study "source symbol" menu
//! ([`crate::ui::tool_views::fx_picker_popup`]).
//!
//! It moved down here from `vike-app` when the ƒx picker did (tool-view extraction batch 2):
//! keeping it in the CI-excluded binary would have meant either a second copy or threading a
//! callback through the moved popup. Pure `egui` painting over a `vike_catalog::Instrument` and
//! the shared palette — no eframe/wgpu, no app-local types — so it builds on CI like the rest of
//! this crate.

use vike_catalog::Instrument;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::symbol_picker;

/// One full-width instrument search-result row, rendered `raw_symbol | description | venue`: the
/// symbol on the left, the (possibly-empty) description dim in the middle, and a dim VENUE tag on
/// the right. Hover-highlighted and click-sensed across the whole row — the same allocate/paint
/// idiom as the chart-style menu rows. Returns `true` when clicked. `selected` marks the row with
/// a `stroke::EDGE`-wide ACCENT edge on its left; the symbol keeps its text ink (the accent is a shape, never the
/// colour of a word). The venue tag is what makes a cross-venue result unambiguous (two
/// `BTCUSDT` rows tagged `BINANCE` vs `BYBIT`). Ranking/filtering itself lives in
/// `Catalog::search` (the tiered logic ported from the retired `filter_symbols`).
pub fn search_result_row(ui: &mut egui::Ui, selected: bool, info: &Instrument) -> bool {
    let t = Tokens::of(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(symbol_picker::ROW_MIN_W), symbol_picker::ROW_H),
        egui::Sense::click(),
    );
    if resp.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(RADIUS), t.theme.hover);
    }
    if selected {
        // The chosen result is marked by a shape, never by the colour of its word: the kit's
        // `stroke::EDGE` accent edge on the row's left (the selected-row marker), clear of the
        // symbol at +8.
        let edge = egui::Rect::from_min_size(rect.min, egui::vec2(stroke::EDGE, rect.height()));
        ui.painter().rect_filled(edge, 0.0, t.theme.accent);
    }
    let sym_col = t.theme.text_ui;
    let dim = t.theme.text3;
    ui.painter().text(
        egui::pos2(rect.left() + space::LG, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &info.raw_symbol,
        egui::FontId::proportional(role_px(ui.ctx(), TextRole::Title)),
        sym_col,
    );
    // Middle column: the human description (empty for the crypto venues today — then nothing
    // paints, which is fine). Left-aligned at a fixed inset so it doesn't collide with the symbol.
    if !info.description.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + symbol_picker::DESC_INSET, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &info.description,
            egui::FontId::proportional(role_px(ui.ctx(), TextRole::Body)),
            dim,
        );
    }
    ui.painter().text(
        egui::pos2(rect.right() - space::LG, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        info.venue.to_uppercase(),
        egui::FontId::proportional(role_px(ui.ctx(), TextRole::Body)),
        dim,
    );
    resp.clicked()
}
