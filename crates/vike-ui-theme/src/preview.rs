//! Painted previews of the appearance options — what the Settings window's Appearance section
//! shows for each theme and each market-colour set (design system spec §5). Each is painted from
//! the option's OWN tokens, so a person sees an option before choosing it; the tests read the
//! painted shapes back and hold every preview to that.

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::market::{ThemeMarketColors, ThemeMarketColorsId};
use crate::metrics::RADIUS;
use crate::theme::{ThemePalette, ThemePaletteId};
use crate::type_scale::{ThemeTextRole, ThemeTextSize};

/// The size of one preview; the Appearance section lays four in a row.
pub const PREVIEW_SIZE: egui::Vec2 = vec2(132.0, 84.0);

/// Theme `id` as a miniature window: its background, a header (the header gradient when
/// `gradient` is on), a tab row whose selected tab is underlined in the theme's accent — the
/// accent as a shape, spec §2 — and a card holding a price and its caption.
pub fn paint_theme_preview(painter: &Painter, rect: Rect, id: ThemePaletteId, gradient: bool) {
    let t = ThemePalette::of(id);
    let cr = CornerRadius::same(RADIUS);
    painter.rect_filled(rect, cr, t.background);
    if gradient {
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), 14.0));
        let ppp = painter.ctx().pixels_per_point();
        painter.add(egui::Shape::mesh(crate::components::window::background::gradient_mesh(
            header,
            t.gradient_top,
            t.background,
            ppp,
        )));
    }
    let caption = FontId::proportional(ThemeTextSize::Small.px(ThemeTextRole::Caption));
    let tab_y = rect.top() + 22.0;
    let chart = painter.text(
        pos2(rect.left() + 8.0, tab_y),
        Align2::LEFT_CENTER,
        "Chart",
        caption.clone(),
        t.text,
    );
    painter.text(
        pos2(chart.right() + 10.0, tab_y),
        Align2::LEFT_CENTER,
        "Trade",
        caption.clone(),
        t.text_secondary,
    );
    painter.hline(chart.x_range(), chart.bottom() + 2.0, Stroke::new(2.0, t.accent));
    let card = Rect::from_min_max(
        pos2(rect.left() + 8.0, rect.top() + 34.0),
        pos2(rect.right() - 8.0, rect.bottom() - 8.0),
    );
    painter.rect_filled(card, cr, t.card);
    let body = FontId::monospace(ThemeTextSize::Small.px(ThemeTextRole::Body));
    painter.text(
        pos2(card.left() + 6.0, card.top() + 11.0),
        Align2::LEFT_CENTER,
        "64,213.50",
        body,
        t.text,
    );
    painter.text(
        pos2(card.left() + 6.0, card.bottom() - 9.0),
        Align2::LEFT_CENTER,
        "BTCUSDT",
        caption,
        t.text_tertiary,
    );
    painter.rect_stroke(rect, cr, Stroke::new(1.0, t.border), StrokeKind::Inside);
}

/// Market-colour set `id` on theme `on`'s card: three candles, their volume bars in the volume
/// fills, a bid and an ask depth bar in the depth fills, and a P&L pair in the text colours.
pub fn paint_market_preview(
    painter: &Painter,
    rect: Rect,
    id: ThemeMarketColorsId,
    on: ThemePaletteId,
) {
    let t = ThemePalette::of(on);
    let m = ThemeMarketColors::of(id);
    let cr = CornerRadius::same(RADIUS);
    painter.rect_filled(rect, cr, t.card);
    // Up, down, up: (is up, body top, body bottom, volume height), in points from the top.
    let candles = [(true, 18.0, 36.0, 12.0), (false, 14.0, 30.0, 8.0), (true, 12.0, 26.0, 14.0)];
    for (i, (up, top, bottom, vol)) in candles.into_iter().enumerate() {
        let x = rect.left() + 16.0 + i as f32 * 14.0;
        let (c, v) = if up { (m.up, m.up_volume) } else { (m.down, m.down_volume) };
        painter.line_segment(
            [pos2(x, rect.top() + 8.0), pos2(x, rect.top() + 42.0)],
            Stroke::new(1.0, c),
        );
        let body =
            Rect::from_min_max(pos2(x - 4.0, rect.top() + top), pos2(x + 4.0, rect.top() + bottom));
        painter.rect_filled(body, 0.0, c);
        let bar = Rect::from_min_max(
            pos2(x - 4.0, rect.top() + 62.0 - vol),
            pos2(x + 4.0, rect.top() + 62.0),
        );
        painter.rect_filled(bar, 0.0, v);
    }
    let left = rect.left() + 70.0;
    let bid =
        Rect::from_min_max(pos2(left, rect.top() + 14.0), pos2(left + 48.0, rect.top() + 24.0));
    let ask =
        Rect::from_min_max(pos2(left, rect.top() + 30.0), pos2(left + 34.0, rect.top() + 40.0));
    painter.rect_filled(bid, 0.0, m.up_depth);
    painter.rect_filled(ask, 0.0, m.down_depth);
    let caption = FontId::monospace(ThemeTextSize::Small.px(ThemeTextRole::Caption));
    let y = rect.bottom() - 10.0;
    painter.text(
        pos2(rect.left() + 8.0, y),
        Align2::LEFT_CENTER,
        "+1,234.56",
        caption.clone(),
        m.up_text,
    );
    painter.text(
        pos2(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        "-567.89",
        caption,
        m.down_text,
    );
    painter.rect_stroke(rect, cr, Stroke::new(1.0, t.border), StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ThemeDensity;
    use egui::Color32;
    use std::collections::BTreeSet;

    const RECT: Rect =
        Rect { min: egui::Pos2 { x: 20.0, y: 20.0 }, max: egui::Pos2 { x: 152.0, y: 104.0 } };

    /// One frame of `paint`, on a context with the app's type; the frame's shapes.
    fn frame(paint: impl Fn(&Painter)) -> Vec<egui::epaint::ClippedShape> {
        let context = egui::Context::default();
        crate::appearance::install_type(&context, ThemeTextSize::Small);
        let out = context.run_ui(egui::RawInput::default(), |ui| paint(ui.painter()));
        let shapes = out.shapes.clone();
        out.drop_without_applying_deltas();
        shapes
    }

    fn collect(shape: &egui::Shape, got: &mut BTreeSet<[u8; 4]>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, got)),
            egui::Shape::Rect(r) => {
                got.insert(r.fill.to_array());
                got.insert(r.stroke.color.to_array());
            }
            egui::Shape::LineSegment { stroke, .. } => {
                got.insert(stroke.color.to_array());
            }
            egui::Shape::Text(t) => {
                for s in &t.galley.job.sections {
                    got.insert(s.format.color.to_array());
                }
            }
            egui::Shape::Mesh(m) => {
                for v in &m.vertices {
                    got.insert(v.color.to_array());
                }
            }
            _ => {}
        }
    }

    /// Every colour `paint` puts on screen, transparent excepted.
    fn painted(paint: impl Fn(&Painter)) -> BTreeSet<[u8; 4]> {
        let mut got = BTreeSet::new();
        for c in frame(paint) {
            collect(&c.shape, &mut got);
        }
        got.remove(&Color32::TRANSPARENT.to_array());
        got
    }

    fn set(colours: &[Color32]) -> BTreeSet<[u8; 4]> {
        colours.iter().map(|c| c.to_array()).collect()
    }

    /// A theme's preview paints that theme and nothing else — never the current theme's colours,
    /// which is the mistake a preview is most likely to make.
    #[test]
    fn a_theme_preview_paints_that_theme_and_nothing_else() {
        for id in ThemePaletteId::ALL {
            let t = ThemePalette::of(id);
            let got = painted(|p| paint_theme_preview(p, RECT, id, false));
            let allowed = set(&[
                t.background,
                t.card,
                t.border,
                t.text,
                t.text_secondary,
                t.text_tertiary,
                t.accent,
            ]);
            let extra: Vec<_> = got.difference(&allowed).collect();
            assert!(extra.is_empty(), "{id:?} painted {extra:?}");
            for must in [t.background, t.card, t.accent] {
                assert!(got.contains(&must.to_array()), "{id:?}: {must:?} not painted");
            }
        }
    }

    #[test]
    fn a_theme_preview_shows_the_header_gradient_only_when_it_is_on() {
        let meshes = |on: bool| {
            frame(|p| paint_theme_preview(p, RECT, ThemePaletteId::Midnight, on))
                .iter()
                .filter(|c| matches!(c.shape, egui::Shape::Mesh(_)))
                .count()
        };
        assert_eq!((meshes(false), meshes(true)), (0, 1));
    }

    /// A market preview paints its whole set — graphics, text, depth and volume fills — on the
    /// given theme's card, and nothing else.
    #[test]
    fn a_market_preview_paints_its_whole_set_on_the_themes_card() {
        for m in ThemeMarketColorsId::ALL {
            for on in ThemePaletteId::ALL {
                let c = ThemeMarketColors::of(m);
                let t = ThemePalette::of(on);
                let whole = [
                    c.up,
                    c.down,
                    c.up_text,
                    c.down_text,
                    c.up_depth,
                    c.down_depth,
                    c.up_volume,
                    c.down_volume,
                ];
                let got = painted(|p| paint_market_preview(p, RECT, m, on));
                let mut allowed = set(&whole);
                allowed.extend(set(&[t.card, t.border]));
                let extra: Vec<_> = got.difference(&allowed).collect();
                assert!(extra.is_empty(), "{m:?} on {on:?} painted {extra:?}");
                for must in whole {
                    assert!(got.contains(&must.to_array()), "{m:?} on {on:?}: {must:?} missing");
                }
            }
        }
    }

    /// The Settings window's tests find each control by these names, so no two may be equal.
    #[test]
    fn every_option_has_its_own_name() {
        assert_eq!(
            ThemePaletteId::ALL.map(ThemePaletteId::label),
            ["Graphite", "Midnight", "Dusk", "Carbon"]
        );
        assert_eq!(
            ThemeMarketColorsId::ALL.map(ThemeMarketColorsId::label),
            ["Classic", "TradingView", "Exchange", "Colour-blind"]
        );
        assert_eq!(
            ThemeDensity::ALL.map(ThemeDensity::label),
            ["Compact", "Normal", "Comfortable"]
        );
        assert_eq!(ThemeTextSize::ALL.map(ThemeTextSize::label), ["Small", "Standard", "Large"]);
        let mut all: Vec<&str> = ThemePaletteId::ALL.map(ThemePaletteId::label).to_vec();
        all.extend(ThemeMarketColorsId::ALL.map(ThemeMarketColorsId::label));
        all.extend(ThemeDensity::ALL.map(ThemeDensity::label));
        all.extend(ThemeTextSize::ALL.map(ThemeTextSize::label));
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
    }
}
