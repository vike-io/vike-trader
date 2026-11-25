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

// The depth and volume strengths and each set's four ruled colours (`base`): GENERATED from
// `crates/vike-ui-theme/ui-theme.toml` (`[market]`, `[market.*]`), each with its doc comment.
include!("generated/market_tokens.rs");

/// One of the four market-colour sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeMarketColorsId {
    #[default]
    Classic,
    TradingView,
    Exchange,
    ColourBlind,
}

impl ThemeMarketColorsId {
    /// Every set, in the order the Appearance screen lists them.
    pub const ALL: [ThemeMarketColorsId; 4] = [
        ThemeMarketColorsId::Classic,
        ThemeMarketColorsId::TradingView,
        ThemeMarketColorsId::Exchange,
        ThemeMarketColorsId::ColourBlind,
    ];

    /// The value the `preferences.market_colors` settings row stores for this set: its word in
    /// `vike_model::appearance::MARKET_COLOR_SETS`, at its place in [`ThemeMarketColorsId::ALL`] (the
    /// declaration order).
    pub fn key(self) -> &'static str {
        vike_model::appearance::MARKET_COLOR_SETS[self as usize]
    }

    /// The set a stored `preferences.market_colors` value names, or `None`.
    pub fn from_key(key: &str) -> Option<ThemeMarketColorsId> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }

    /// The name the Appearance screen shows for this set.
    pub fn label(self) -> &'static str {
        match self {
            ThemeMarketColorsId::Classic => "Classic",
            ThemeMarketColorsId::TradingView => "TradingView",
            ThemeMarketColorsId::Exchange => "Exchange",
            ThemeMarketColorsId::ColourBlind => "Colour-blind",
        }
    }
}

/// "Up" and "down" for one set, and what each derives into (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeMarketColors {
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

impl ThemeMarketColors {
    pub fn of(id: ThemeMarketColorsId) -> ThemeMarketColors {
        let (up, down, up_text, down_text) = base(id);
        let depth = crate::color::with_alpha;
        ThemeMarketColors {
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
    use crate::theme::{ThemePalette, ThemePaletteId};

    /// The app's up and down before the market sets, as the retired `palette` module held them.
    const APP_UP: Color32 = Color32::from_rgb(64, 186, 80);
    const APP_DOWN: Color32 = Color32::from_rgb(248, 82, 73);
    /// The trading terminal's pair, as the retired `palette::trading` held it.
    const TERMINAL_UP: Color32 = Color32::from_rgb(46, 189, 133);
    const TERMINAL_DOWN: Color32 = Color32::from_rgb(246, 70, 93);

    #[test]
    fn classic_is_todays_app_up_and_down() {
        let m = ThemeMarketColors::of(ThemeMarketColorsId::Classic);
        assert_eq!((m.up, m.down), (APP_UP, APP_DOWN));
    }

    #[test]
    fn exchange_is_todays_trading_terminal_pair() {
        let m = ThemeMarketColors::of(ThemeMarketColorsId::Exchange);
        assert_eq!((m.up, m.down), (TERMINAL_UP, TERMINAL_DOWN));
    }

    #[test]
    fn tradingview_and_colour_blind_are_the_ruled_values() {
        let tv = ThemeMarketColors::of(ThemeMarketColorsId::TradingView);
        assert_eq!(
            (tv.up, tv.down),
            (Color32::from_rgb(8, 153, 129), Color32::from_rgb(242, 54, 69))
        );
        let cb = ThemeMarketColors::of(ThemeMarketColorsId::ColourBlind);
        assert_eq!(
            (cb.up, cb.down),
            (Color32::from_rgb(59, 130, 246), Color32::from_rgb(245, 158, 11))
        );
    }

    /// Up and down are graphics (candles, depth bars), so 3:1 is their floor — on every theme.
    #[test]
    fn every_set_reads_on_every_theme() {
        for m in ThemeMarketColorsId::ALL {
            let c = ThemeMarketColors::of(m);
            for t in ThemePaletteId::ALL {
                let bg = ThemePalette::of(t).background;
                assert!(contrast_ratio(c.up, bg) >= 3.0, "{m:?} up on {t:?}");
                assert!(contrast_ratio(c.down, bg) >= 3.0, "{m:?} down on {t:?}");
            }
        }
    }

    #[test]
    fn keys_round_trip() {
        for m in ThemeMarketColorsId::ALL {
            assert_eq!(ThemeMarketColorsId::from_key(m.key()), Some(m));
        }
        assert_eq!(ThemeMarketColorsId::from_key("rainbow"), None);
        assert_eq!(ThemeMarketColorsId::default(), ThemeMarketColorsId::Classic);
        let keys: Vec<_> = ThemeMarketColorsId::ALL.iter().map(|m| m.key()).collect();
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
                ThemePaletteId::ALL
                    .iter()
                    .all(|id| contrast_ratio(*t, ThemePalette::of(*id).card) >= TEXT_MIN_CONTRAST)
            })
            .expect("white clears the floor on every dark card")
    }

    #[test]
    fn the_text_colours_follow_the_rule() {
        for m in ThemeMarketColorsId::ALL {
            let c = ThemeMarketColors::of(m);
            assert_eq!(c.up_text, text_colour(c.up), "{m:?} up");
            assert_eq!(c.down_text, text_colour(c.down), "{m:?} down");
        }
    }

    /// The finding this closes (PR 2's final review): two ruled colours are below the small-text
    /// floor on Graphite's card, so text cannot use them as they are.
    #[test]
    fn two_ruled_colours_are_below_the_text_floor_on_a_card() {
        let card = ThemePalette::of(ThemePaletteId::Graphite).card;
        let tv = contrast_ratio(ThemeMarketColors::of(ThemeMarketColorsId::TradingView).down, card);
        let cb = contrast_ratio(ThemeMarketColors::of(ThemeMarketColorsId::ColourBlind).up, card);
        assert!((4.18..=4.20).contains(&tv), "{tv}");
        assert!((4.43..=4.45).contains(&cb), "{cb}");
    }

    /// P&L text reads on every ground it is painted on, in every theme and every set.
    #[test]
    fn pnl_text_reads_on_every_card_panel_and_background() {
        for m in ThemeMarketColorsId::ALL {
            let c = ThemeMarketColors::of(m);
            for id in ThemePaletteId::ALL {
                let t = ThemePalette::of(id);
                for (ground, g) in
                    [("card", t.card), ("panel", t.surface), ("background", t.background)]
                {
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
        for m in [ThemeMarketColorsId::Classic, ThemeMarketColorsId::Exchange] {
            let c = ThemeMarketColors::of(m);
            assert_eq!((c.up_text, c.down_text), (c.up, c.down), "{m:?}");
        }
    }

    /// The depth fills are today's DOM bars and the volume fills today's volume pane.
    #[test]
    fn the_fills_are_todays_depth_and_volume_bars() {
        let ex = ThemeMarketColors::of(ThemeMarketColorsId::Exchange);
        assert_eq!(ex.up_depth, crate::color::with_alpha(TERMINAL_UP, 48));
        assert_eq!(ex.down_depth, crate::color::with_alpha(TERMINAL_DOWN, 46));
        let cl = ThemeMarketColors::of(ThemeMarketColorsId::Classic);
        assert_eq!(cl.up_volume, APP_UP.gamma_multiply(0.7));
        assert_eq!(cl.down_volume, APP_DOWN.gamma_multiply(0.7));
    }

    #[test]
    fn every_set_has_translucent_fills() {
        for m in ThemeMarketColorsId::ALL {
            let c = ThemeMarketColors::of(m);
            assert_eq!(
                (c.up_depth.a(), c.down_depth.a()),
                (DEPTH_UP_ALPHA, DEPTH_DOWN_ALPHA),
                "{m:?}"
            );
            assert!(c.up_volume.a() < 255 && c.up_volume.a() == c.down_volume.a(), "{m:?}");
        }
    }
}
