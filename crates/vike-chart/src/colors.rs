//! What the chart canvas paints with THIS frame (design system spec §3.5): resolved once per
//! frame in `crate::chart::draw` and handed to every painter, so no painter names a colour of its
//! own and a theme or market-colour change reaches an open chart on its next frame.
//!
//! Two sources:
//! - the window's own colours, `crate::options::ChartOptions`;
//! - the installed appearance (`vike_ui_theme::appearance::current`): the theme's neutrals for what
//!   a user does not edit per chart — axis labels, crosshair tags, pane names and dividers, the
//!   zero line — and the kit's on-fill black for text on a coloured chip.
//!
//! Each colour of the window's `ChartOptions` is an OVERRIDE: `None` follows the market colours
//! (candles, histograms, volume, direction text, the line series) or the theme (canvas, grid,
//! crosshair); `Some` is the user's pick for this chart. Unset borders and wicks follow the
//! resolved body.

use egui::Color32;
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::color::{faded, rgb};
use vike_ui_theme::components::ON_FILL;
use vike_ui_theme::market::{MarketColors, VOLUME_FACTOR};
use vike_ui_theme::theme::Theme;

use crate::options::ChartOptions;

/// Below this Rec. 601 luma a chip's fill takes light text instead of [`ON_FILL`]. The darkest
/// fill the design has is TradingView's "up" at 106.9 (`every_design_fill_takes_on_fill_text`
/// lists them all), so every design fill keeps the kit's black, and only a colour a user picked
/// darker than that turns its text light. Luma, not a contrast ratio: decision 0032 keeps the
/// luminance power a ratio needs out of production code.
pub const DARK_FILL_LUMA: f32 = 100.0;

/// Every colour the canvas paints this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartColors {
    /// Candle bodies, and every mark a bar's direction colours: bars, columns, Kagi, PnF.
    pub up: Color32,
    pub down: Color32,
    /// Candle body borders.
    pub border_up: Color32,
    pub border_down: Color32,
    /// Candle wicks.
    pub wick_up: Color32,
    pub wick_down: Color32,
    /// Up and down as a GRAPHIC away from the candles: histograms, markers, the baseline style,
    /// the last-price line, footprint tints.
    pub up_s: Color32,
    pub down_s: Color32,
    /// Up and down as TEXT: the OHLC legend's numbers and change.
    pub up_text: Color32,
    pub down_text: Color32,
    /// Volume bars.
    pub up_volume: Color32,
    pub down_volume: Color32,
    /// Line, area and step series.
    pub line: Color32,
    pub grid: Color32,
    /// The crosshair's lines.
    pub cross: Color32,
    /// The canvas: `bg` flat, or the gradient from `bg_top` down to `bg`.
    pub bg: Color32,
    pub bg_top: Color32,
    /// Words on the canvas: the OHLC legend's letters, the crosshair tags' text.
    pub text: Color32,
    /// Pane names.
    pub text2: Color32,
    /// Axis labels.
    pub axis: Color32,
    /// The crosshair tags' fill.
    pub tag_bg: Color32,
    /// The line between two panes, and that line under the pointer.
    pub divider: Color32,
    pub divider_hover: Color32,
    /// The line between plus and minus: the CVD zero line, the baseline style's anchor.
    pub zero_line: Color32,
    /// Text on a chip filled with a market or series colour.
    pub on_fill: Color32,
}

impl ChartColors {
    /// The colours `o` paints under `a`.
    pub fn resolve(o: &ChartOptions, a: &Appearance) -> ChartColors {
        let t = Theme::of(a.theme);
        let m = MarketColors::of(a.market);
        let pick = |c: Option<[u8; 3]>, unset: Color32| c.map(rgb).unwrap_or(unset);
        let (up, down) = (pick(o.up, m.up), pick(o.down, m.down));
        ChartColors {
            up,
            down,
            border_up: pick(o.border_up, up),
            border_down: pick(o.border_down, down),
            wick_up: pick(o.wick_up, up),
            wick_down: pick(o.wick_down, down),
            up_s: pick(o.up_s, m.up),
            down_s: pick(o.down_s, m.down),
            up_text: pick(o.up_s, m.up_text),
            down_text: pick(o.down_s, m.down_text),
            up_volume: o.up_s.map_or(m.up_volume, |c| faded(rgb(c), VOLUME_FACTOR)),
            down_volume: o.down_s.map_or(m.down_volume, |c| faded(rgb(c), VOLUME_FACTOR)),
            line: pick(o.line, m.up),
            grid: pick(o.grid, t.hover),
            cross: pick(o.cross, t.text2),
            bg: pick(o.bg, t.bg),
            bg_top: pick(o.bg_top, t.grad_top),
            text: t.text,
            text2: t.text2,
            axis: t.text3,
            tag_bg: t.border,
            divider: t.border,
            divider_hover: t.analysis_line,
            zero_line: t.analysis_line,
            on_fill: ON_FILL,
        }
    }

    /// The text colour for a chip filled with `fill`: the kit's black on every fill the design
    /// has, the theme's text on a fill a user picked darker than [`DARK_FILL_LUMA`].
    pub fn text_on(&self, fill: Color32) -> Color32 {
        let [r, g, b, _] = fill.to_array();
        let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
        if luma >= DARK_FILL_LUMA { self.on_fill } else { self.text }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_ui_theme::components::Status;
    use vike_ui_theme::market::MarketId;
    use vike_ui_theme::theme::ThemeId;

    /// What a user does not edit per chart follows the theme — in every theme.
    #[test]
    fn the_canvas_neutrals_are_the_installed_themes() {
        for id in ThemeId::ALL {
            let a = Appearance { theme: id, ..Appearance::default() };
            let c = ChartColors::resolve(&ChartOptions::default(), &a);
            let t = Theme::of(id);
            assert_eq!(
                (c.text, c.text2, c.axis, c.tag_bg),
                (t.text, t.text2, t.text3, t.border),
                "{id:?}"
            );
            assert_eq!(
                (c.divider, c.divider_hover, c.zero_line),
                (t.border, t.analysis_line, t.analysis_line),
                "{id:?}"
            );
            assert_eq!(c.on_fill, ON_FILL);
        }
    }

    /// Every fill the design has — the four accents, the eight market colours, the status red and
    /// the six indicator rotation colours — takes the kit's black. The darkest is TradingView's up
    /// at luma 106.9.
    #[test]
    fn every_design_fill_takes_on_fill_text() {
        let c = ChartColors::resolve(&ChartOptions::default(), &Appearance::default());
        let mut fills: Vec<Color32> = ThemeId::ALL.iter().map(|id| Theme::of(*id).accent).collect();
        for m in MarketId::ALL {
            let mc = MarketColors::of(m);
            fills.extend([mc.up, mc.down]);
        }
        fills.push(Status::Error.color());
        // `crate::indicators`' private rotation — the series palette (decision 5), spelled here
        // because the test must see every fill a value tag can carry.
        fills.extend(
            [
                (87, 165, 255),
                (168, 85, 247),
                (38, 198, 218),
                (102, 187, 106),
                (236, 64, 122),
                (245, 166, 35),
            ]
            .map(|(r, g, b)| Color32::from_rgb(r, g, b)),
        );
        for f in fills {
            assert_eq!(c.text_on(f), ON_FILL, "{f:?}");
        }
    }

    /// Unset, every colour follows the installed appearance — in every theme and every set.
    #[test]
    fn every_unset_colour_follows_the_installed_appearance() {
        for theme in ThemeId::ALL {
            for market in MarketId::ALL {
                let a = Appearance { theme, market, ..Appearance::default() };
                let c = ChartColors::resolve(&ChartOptions::default(), &a);
                let (t, m) = (Theme::of(theme), MarketColors::of(market));
                let at = format!("{theme:?} {market:?}");
                assert_eq!((c.up, c.down, c.up_s, c.down_s), (m.up, m.down, m.up, m.down), "{at}");
                assert_eq!(
                    (c.border_up, c.border_down, c.wick_up, c.wick_down),
                    (m.up, m.down, m.up, m.down),
                    "{at}"
                );
                assert_eq!((c.up_text, c.down_text), (m.up_text, m.down_text), "{at}");
                assert_eq!((c.up_volume, c.down_volume), (m.up_volume, m.down_volume), "{at}");
                assert_eq!(c.line, m.up, "{at}");
                assert_eq!(
                    (c.bg, c.bg_top, c.grid, c.cross),
                    (t.bg, t.grad_top, t.hover, t.text2),
                    "{at}"
                );
            }
        }
    }

    /// An edited colour is the user's in every theme and set; its unset neighbours still follow.
    #[test]
    fn an_edited_colour_wins_over_every_theme_and_set() {
        let o =
            ChartOptions { up: Some([1, 2, 3]), bg: Some([4, 5, 6]), ..ChartOptions::default() };
        for theme in ThemeId::ALL {
            for market in MarketId::ALL {
                let c = ChartColors::resolve(
                    &o,
                    &Appearance { theme, market, ..Appearance::default() },
                );
                assert_eq!((c.up, c.bg), (Color32::from_rgb(1, 2, 3), Color32::from_rgb(4, 5, 6)));
                assert_eq!(c.down, MarketColors::of(market).down, "{theme:?} {market:?}");
            }
        }
    }

    /// Unset borders and wicks follow the BODY, edited or not — an edited body is not outlined in
    /// the theme's colour.
    #[test]
    fn unset_borders_and_wicks_follow_the_body() {
        let o = ChartOptions { up: Some([1, 2, 3]), ..ChartOptions::default() };
        let c = ChartColors::resolve(&o, &Appearance::default());
        assert_eq!((c.border_up, c.wick_up), (c.up, c.up));
    }

    /// An edited semantic colour also colours direction text and volume: the user chose "up".
    #[test]
    fn an_edited_up_s_colours_direction_text_and_volume() {
        let o = ChartOptions { up_s: Some([1, 200, 3]), ..ChartOptions::default() };
        let c = ChartColors::resolve(&o, &Appearance::default());
        let picked = Color32::from_rgb(1, 200, 3);
        assert_eq!((c.up_s, c.up_text), (picked, picked));
        assert_eq!(c.up_volume, picked.gamma_multiply(VOLUME_FACTOR));
    }

    /// The default appearance's chart, spelled out: Classic on Graphite.
    #[test]
    fn the_default_appearance_is_the_classic_graphite_chart() {
        let c = ChartColors::resolve(&ChartOptions::default(), &Appearance::default());
        assert_eq!(
            (c.up, c.down),
            (Color32::from_rgb(64, 186, 80), Color32::from_rgb(248, 82, 73))
        );
        assert_eq!(
            (c.bg, c.bg_top),
            (Color32::from_rgb(13, 17, 23), Color32::from_rgb(22, 27, 36))
        );
        assert_eq!(
            (c.grid, c.cross),
            (Color32::from_rgb(33, 37, 44), Color32::from_rgb(154, 164, 177))
        );
    }

    /// A fill a user picked darker than every design fill gets the theme's light text.
    #[test]
    fn a_fill_picked_darker_than_the_design_takes_the_themes_text() {
        let c = ChartColors::resolve(&ChartOptions::default(), &Appearance::default());
        for f in [Color32::from_rgb(20, 20, 60), Color32::from_rgb(60, 60, 60), Color32::BLACK] {
            assert_eq!(c.text_on(f), c.text, "{f:?}");
        }
    }
}
