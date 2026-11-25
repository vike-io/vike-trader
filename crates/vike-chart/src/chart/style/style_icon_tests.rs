use super::draw_style_icon;
use crate::chart::style::ChartStyle;
use egui::{Color32, Rect};

/// The chart-style menu's icons preview what the chart draws: the installed market colours,
/// never the old candle mint and rose.
#[test]
fn the_style_icons_follow_the_market_colours() {
    use vike_ui_theme::appearance::{ThemeAppearance, apply};
    use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
    let context = egui::Context::default();
    apply(
        &context,
        &ThemeAppearance { market: ThemeMarketColorsId::ColourBlind, ..ThemeAppearance::default() },
    );
    let r = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(20.0, 20.0));
    let mut out = context.run_ui(egui::RawInput::default(), |ui| {
        draw_style_icon(ui.painter(), r, ChartStyle::Candles);
    });
    out.textures_delta.clear(); // egui 0.36 panics on dropping unapplied deltas
    let fills: Vec<Color32> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(rect) => Some(rect.fill),
            _ => None,
        })
        .collect();
    let cb = ThemeMarketColors::of(ThemeMarketColorsId::ColourBlind);
    assert!(fills.contains(&cb.up) && fills.contains(&cb.down), "{fills:?}");
    for old in [Color32::from_rgb(91, 190, 145), Color32::from_rgb(217, 84, 88)] {
        assert!(!fills.contains(&old), "the old candle colour {old:?} is still an icon's");
    }
}
