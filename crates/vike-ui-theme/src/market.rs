//! The four market-colour sets (design system spec §3.2): what "up" and "down" look like on every
//! candle, depth bar and P&L figure. A setting (`preferences.market_colors`), independent of the
//! theme.
//!
//! A set is two ruled colours and three things derived from them:
//! - **text** — a P&L, a signed change, a side word. The ruled colour wherever that already reads
//!   as small text on every theme's CARD, the lightest ground a P&L sits on; otherwise the ruled
//!   colour lightened toward white by the smallest hundredth-step that does. The rule runs in this
//!   module's tests (`text_colour` there): it needs a luminance power, which decision 0032 keeps
//!   out of production code, so production reads the constants below.
//! - **depth** — depth-bar fills: the ruled colour at the DOM's alphas.
//! - **volume** — volume-bar fills: the ruled colour at the volume pane's strength.

use egui::Color32;

/// Depth-bar alpha for "up". `crates/vike-panels/src/dom.rs`'s `draw` drew bid bars at 48 and ask
/// bars at 46 before it read these tokens, and now draws `MarketColors::up_depth`/`down_depth`.
/// Why the two differ is not recorded, so the token does not decide it.
pub const DEPTH_UP_ALPHA: u8 = 48;
/// Depth-bar alpha for "down". See [`DEPTH_UP_ALPHA`].
pub const DEPTH_DOWN_ALPHA: u8 = 46;
/// Volume-bar strength: the chart's volume pane (`crates/vike-chart/src/chart/subpanes.rs`'s
/// `draw_volume_pane`) fills with `up_volume`/`down_volume`, or with a colour the user picked at
/// this strength.
pub const VOLUME_FACTOR: f32 = 0.7;

/// One of the four market-colour sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MarketId {
    #[default]
    Classic,
    TradingView,
    Exchange,
    ColourBlind,
}

impl MarketId {
    /// Every set, in the order the Appearance screen lists them.
    pub const ALL: [MarketId; 4] =
        [MarketId::Classic, MarketId::TradingView, MarketId::Exchange, MarketId::ColourBlind];

    /// The value the `preferences.market_colors` settings row stores for this set.
    pub fn key(self) -> &'static str {
        match self {
            MarketId::Classic => "classic",
            MarketId::TradingView => "tradingview",
            MarketId::Exchange => "exchange",
            MarketId::ColourBlind => "colorblind",
        }
    }

    /// The set a stored `preferences.market_colors` value names, or `None`.
    pub fn from_key(key: &str) -> Option<MarketId> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }

    /// The name the Appearance screen shows for this set.
    pub fn label(self) -> &'static str {
        match self {
            MarketId::Classic => "Classic",
            MarketId::TradingView => "TradingView",
            MarketId::Exchange => "Exchange",
            MarketId::ColourBlind => "Colour-blind",
        }
    }
}

/// "Up" and "down" for one set, and what each derives into (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketColors {
    /// "Up" as a graphic: candles, markers, the BUY fill. Held to 3:1 on every background.
    pub up: Color32,
    /// "Down" as a graphic.
    pub down: Color32,
    /// "Up" as text: a P&L, a signed change, a side word. Held to 4.5:1 on every card.
    pub up_text: Color32,
    /// "Down" as text.
    pub down_text: Color32,
    /// "Up" depth-bar fill.
    pub up_depth: Color32,
    /// "Down" depth-bar fill.
    pub down_depth: Color32,
    /// "Up" volume-bar fill.
    pub up_volume: Color32,
    /// "Down" volume-bar fill.
    pub down_volume: Color32,
}

impl MarketColors {
    pub fn of(id: MarketId) -> MarketColors {
        let rgb = Color32::from_rgb;
        // (up, down, up as text, down as text)
        let (up, down, up_text, down_text) = match id {
            MarketId::Classic => {
                (rgb(64, 186, 80), rgb(248, 82, 73), rgb(64, 186, 80), rgb(248, 82, 73))
            }
            // TradingView's CURRENT chart default; the older `#26A69A`/`#EF5350` is what
            // Lightweight Charts still ships. Both text colours are lifted: down measured 4.19:1
            // on Graphite's card, and up 4.57:1, inside the rule's margin.
            MarketId::TradingView => {
                (rgb(8, 153, 129), rgb(242, 54, 69), rgb(10, 154, 130), rgb(243, 74, 88))
            }
            MarketId::Exchange => {
                (rgb(46, 189, 133), rgb(246, 70, 93), rgb(46, 189, 133), rgb(246, 70, 93))
            }
            // Up measured 4.44:1 on Graphite's card.
            MarketId::ColourBlind => {
                (rgb(59, 130, 246), rgb(245, 158, 11), rgb(65, 134, 246), rgb(245, 158, 11))
            }
        };
        let depth = crate::color::with_alpha;
        MarketColors {
            up,
            down,
            up_text,
            down_text,
            up_depth: depth(up, DEPTH_UP_ALPHA),
            down_depth: depth(down, DEPTH_DOWN_ALPHA),
            up_volume: crate::color::faded(up, VOLUME_FACTOR),
            down_volume: crate::color::faded(down, VOLUME_FACTOR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::{contrast_ratio, lerp};
    use crate::palette;
    use crate::theme::{Theme, ThemeId};

    #[test]
    fn classic_is_todays_app_up_and_down() {
        let m = MarketColors::of(MarketId::Classic);
        assert_eq!((m.up, m.down), (palette::UP, palette::DOWN));
    }

    #[test]
    fn exchange_is_todays_trading_terminal_pair() {
        let m = MarketColors::of(MarketId::Exchange);
        assert_eq!((m.up, m.down), (palette::trading::UP, palette::trading::DOWN));
    }

    #[test]
    fn tradingview_and_colour_blind_are_the_ruled_values() {
        let tv = MarketColors::of(MarketId::TradingView);
        assert_eq!(
            (tv.up, tv.down),
            (Color32::from_rgb(8, 153, 129), Color32::from_rgb(242, 54, 69))
        );
        let cb = MarketColors::of(MarketId::ColourBlind);
        assert_eq!(
            (cb.up, cb.down),
            (Color32::from_rgb(59, 130, 246), Color32::from_rgb(245, 158, 11))
        );
    }

    /// Up and down are graphics (candles, depth bars), so 3:1 is their floor — on every theme.
    #[test]
    fn every_set_reads_on_every_theme() {
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            for t in ThemeId::ALL {
                let bg = Theme::of(t).bg;
                assert!(contrast_ratio(c.up, bg) >= 3.0, "{m:?} up on {t:?}");
                assert!(contrast_ratio(c.down, bg) >= 3.0, "{m:?} down on {t:?}");
            }
        }
    }

    #[test]
    fn keys_round_trip() {
        for m in MarketId::ALL {
            assert_eq!(MarketId::from_key(m.key()), Some(m));
        }
        assert_eq!(MarketId::from_key("rainbow"), None);
        assert_eq!(MarketId::default(), MarketId::Classic);
        let keys: Vec<_> = MarketId::ALL.iter().map(|m| m.key()).collect();
        assert_eq!(keys, ["classic", "tradingview", "exchange", "colorblind"]);
    }

    /// THE TEXT RULE: the smallest hundredth-step from the ruled colour toward white that clears
    /// this on EVERY theme's card — the lightest ground a P&L is painted on (the Trade window's
    /// cards). The same margin the theme's caption grey uses, for the same reason.
    const TEXT_MIN_CONTRAST: f64 = 4.6;

    fn text_colour(c: Color32) -> Color32 {
        (0..=100u32)
            .map(|i| lerp(c, Color32::WHITE, i as f32 / 100.0))
            .find(|t| {
                ThemeId::ALL
                    .iter()
                    .all(|id| contrast_ratio(*t, Theme::of(*id).card) >= TEXT_MIN_CONTRAST)
            })
            .expect("white clears the floor on every dark card")
    }

    #[test]
    fn the_text_colours_follow_the_rule() {
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            assert_eq!(c.up_text, text_colour(c.up), "{m:?} up");
            assert_eq!(c.down_text, text_colour(c.down), "{m:?} down");
        }
    }

    /// The finding this closes (PR 2's final review): two ruled colours are below the small-text
    /// floor on Graphite's card, so text cannot use them as they are.
    #[test]
    fn two_ruled_colours_are_below_the_text_floor_on_a_card() {
        let card = Theme::of(ThemeId::Graphite).card;
        let tv = contrast_ratio(MarketColors::of(MarketId::TradingView).down, card);
        let cb = contrast_ratio(MarketColors::of(MarketId::ColourBlind).up, card);
        assert!((4.18..=4.20).contains(&tv), "{tv}");
        assert!((4.43..=4.45).contains(&cb), "{cb}");
    }

    /// P&L text reads on every ground it is painted on, in every theme and every set.
    #[test]
    fn pnl_text_reads_on_every_card_panel_and_background() {
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            for id in ThemeId::ALL {
                let t = Theme::of(id);
                for (ground, g) in [("card", t.card), ("panel", t.surface), ("background", t.bg)] {
                    assert!(contrast_ratio(c.up_text, g) >= 4.5, "{m:?} up on {id:?}'s {ground}");
                    assert!(
                        contrast_ratio(c.down_text, g) >= 4.5,
                        "{m:?} down on {id:?}'s {ground}"
                    );
                }
            }
        }
    }

    /// Wherever the ruled colour already reads, text IS the ruled colour: Classic (the default)
    /// and Exchange do not move.
    #[test]
    fn classic_and_exchange_text_is_the_ruled_colour() {
        for m in [MarketId::Classic, MarketId::Exchange] {
            let c = MarketColors::of(m);
            assert_eq!((c.up_text, c.down_text), (c.up, c.down), "{m:?}");
        }
    }

    /// The depth fills are today's DOM bars and the volume fills today's volume pane.
    #[test]
    fn the_fills_are_todays_depth_and_volume_bars() {
        let ex = MarketColors::of(MarketId::Exchange);
        assert_eq!(ex.up_depth, crate::color::with_alpha(palette::trading::UP, 48));
        assert_eq!(ex.down_depth, crate::color::with_alpha(palette::trading::DOWN, 46));
        let cl = MarketColors::of(MarketId::Classic);
        assert_eq!(cl.up_volume, palette::UP.gamma_multiply(0.7));
        assert_eq!(cl.down_volume, palette::DOWN.gamma_multiply(0.7));
    }

    #[test]
    fn every_set_has_translucent_fills() {
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            assert_eq!(
                (c.up_depth.a(), c.down_depth.a()),
                (DEPTH_UP_ALPHA, DEPTH_DOWN_ALPHA),
                "{m:?}"
            );
            assert!(c.up_volume.a() < 255 && c.up_volume.a() == c.down_volume.a(), "{m:?}");
        }
    }
}
