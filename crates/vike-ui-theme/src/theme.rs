//! The four dark themes (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §3.1).
//!
//! A theme is a background, a gradient top, a text colour and an ACCENT. The accent belongs to the
//! theme; there is no separate accent setting (ruled 2026-09-28). Everything else a screen paints —
//! panel, card, hover, border, UI text, secondary text, captions, the analysis line — is DERIVED
//! from those four by one rule and written out below as constants.
//!
//! The rule runs in this module's tests (`derived` there), which recompute every derived value and
//! compare. It does not run at runtime: it needs a luminance power, which decision 0032 keeps out of
//! production code (see `crate::color_math`). A user-made theme (deferred) is the day it moves to
//! runtime, through the `libm` crate.
//!
//! Graphite is today's look. Its surface, card, hover, border, UI text and secondary text are
//! OVERRIDES equal to the constants the retired `palette` module held, so the default look does not move. Its caption
//! grey and analysis line follow the rule. The caption grey is the one intended visible change:
//! 4.00:1 on the background before; after, 5.39:1 on the background and 4.65:1 on cards.

use egui::Color32;

/// One of the four built-in themes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemePaletteId {
    #[default]
    Graphite,
    Midnight,
    Dusk,
    Carbon,
}

impl ThemePaletteId {
    /// Every theme, in the order the Appearance screen lists them.
    pub const ALL: [ThemePaletteId; 4] = [
        ThemePaletteId::Graphite,
        ThemePaletteId::Midnight,
        ThemePaletteId::Dusk,
        ThemePaletteId::Carbon,
    ];

    /// The value the `preferences.theme` settings row stores for this theme: its word in
    /// `vike_model::appearance::THEMES`, at its place in [`ThemePaletteId::ALL`] (the declaration order).
    pub fn key(self) -> &'static str {
        vike_model::appearance::THEMES[self as usize]
    }

    /// The theme a stored `preferences.theme` value names, or `None` for a value no theme has.
    pub fn from_key(key: &str) -> Option<ThemePaletteId> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    /// The name the Appearance screen shows for this theme.
    pub fn label(self) -> &'static str {
        match self {
            ThemePaletteId::Graphite => "Graphite",
            ThemePaletteId::Midnight => "Midnight",
            ThemePaletteId::Dusk => "Dusk",
            ThemePaletteId::Carbon => "Carbon",
        }
    }
}

/// Every colour a theme supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    /// Windows, title bars, dialogs, the chart canvas's bottom stop.
    pub background: Color32,
    /// The chart canvas gradient's top stop.
    pub gradient_top: Color32,
    /// Panels, tables, menus.
    pub surface: Color32,
    /// Raised cards.
    pub card: Color32,
    /// Hover and selection fills.
    pub hover: Color32,
    /// One-pixel separators and widget borders.
    pub border: Color32,
    /// Primary text in content.
    pub text: Color32,
    /// Primary text in egui's own chrome (menus, widget labels).
    pub text_ui: Color32,
    /// Secondary text.
    pub text_secondary: Color32,
    /// Captions, units, axis labels, disabled controls.
    pub text_tertiary: Color32,
    /// The neutral line between plus and minus: a depth chart's bid/ask boundary, a PnL zero line.
    pub analysis_line: Color32,
    /// The theme's accent — a SHAPE only (underline, fill, ring, edge), never a number's colour.
    pub accent: Color32,
}

/// The alpha of [`ThemePalette::scrim`].
pub(crate) const SCRIM_ALPHA: u8 = 150;

impl ThemePalette {
    /// The constants for one theme.
    pub fn of(id: ThemePaletteId) -> &'static ThemePalette {
        match id {
            ThemePaletteId::Graphite => &GRAPHITE,
            ThemePaletteId::Midnight => &MIDNIGHT,
            ThemePaletteId::Dusk => &DUSK,
            ThemePaletteId::Carbon => &CARBON,
        }
    }

    /// The background at `SCRIM_ALPHA` — laid over content that is on screen but must not read
    /// as live: the Trade window's stale ladder (`vike_order_entry::trade_window::ladder`). It replaced the
    /// deleted DOM ladder's own `(8, 11, 15, 150)`.
    pub fn scrim(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(
            self.background.r(),
            self.background.g(),
            self.background.b(),
            SCRIM_ALPHA,
        )
    }
}

// The four themes' colours: GENERATED from `crates/vike-ui-theme/ui-theme.toml`. Production reads these
// constants; the derivation rule and the contrast floors below are tests over them.
include!("generated/theme_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::{contrast_ratio, lerp};

    // THE RULE (design system spec §3.1). Each fraction moves one base colour toward another.
    const SURFACE_T: f32 = 0.045;
    const CARD_T: f32 = 0.07;
    const HOVER_T: f32 = 0.09;
    const BORDER_T: f32 = 0.155;
    const TEXT_UI_T: f32 = 0.015;
    const TEXT_SECONDARY_T: f32 = 0.33;
    const ANALYSIS_T: f32 = 0.35;
    /// The caption grey is the DARKEST hundredth-step from the text toward the background that still
    /// clears this on the CARD — the lightest ground captions sit on (owner, 2026-09-28). 4.5:1 plus a
    /// margin, so rounding cannot land it under the floor. Clearing it on the card clears it on the
    /// panel and the background too, because both are darker.
    const TEXT_TERTIARY_MIN_CONTRAST: f64 = 4.6;

    fn caption_grey(text: Color32, bg: Color32, card: Color32) -> Color32 {
        (0..=100u32)
            .rev()
            .map(|i| lerp(text, bg, i as f32 / 100.0))
            .find(|c| contrast_ratio(*c, card) >= TEXT_TERTIARY_MIN_CONTRAST)
            .expect("at step 0 the grey IS the text, which clears the floor on every theme")
    }

    /// Every token of `t` as the rule computes it from `t`'s four base colours.
    fn derived(t: &ThemePalette) -> ThemePalette {
        ThemePalette {
            background: t.background,
            gradient_top: t.gradient_top,
            text: t.text,
            accent: t.accent,
            surface: lerp(t.background, t.text, SURFACE_T),
            card: lerp(t.background, t.text, CARD_T),
            hover: lerp(t.background, t.text, HOVER_T),
            border: lerp(t.background, t.text, BORDER_T),
            text_ui: lerp(t.text, t.background, TEXT_UI_T),
            text_secondary: lerp(t.text, t.background, TEXT_SECONDARY_T),
            // The theme's own card, so Graphite's caption grey is measured on the card it really has.
            text_tertiary: caption_grey(t.text, t.background, t.card),
            analysis_line: lerp(t.background, t.text, ANALYSIS_T),
        }
    }

    #[test]
    fn every_theme_but_graphite_is_exactly_the_rule() {
        for id in [ThemePaletteId::Midnight, ThemePaletteId::Dusk, ThemePaletteId::Carbon] {
            assert_eq!(
                *ThemePalette::of(id),
                derived(ThemePalette::of(id)),
                "{id:?} drifted from the rule"
            );
        }
    }

    /// Graphite is today's look: its neutrals are OVERRIDES pinned below, and only its
    /// caption grey and analysis line come from the rule.
    #[test]
    fn graphite_follows_the_rule_for_its_caption_grey_and_analysis_line() {
        let g = ThemePalette::of(ThemePaletteId::Graphite);
        let d = derived(g);
        assert_eq!(g.text_tertiary, d.text_tertiary);
        assert_eq!(g.analysis_line, d.analysis_line);
    }

    /// Every Graphite value, pinned to the exact rgb tuple the retired `palette` module held, so a
    /// future edit cannot silently drift one: the accent is the canonical `(62,224,138)`, never the
    /// historically shipped `(62,224,137)`.
    #[test]
    fn graphite_is_todays_palette() {
        let g = ThemePalette::of(ThemePaletteId::Graphite);
        let rgb = Color32::from_rgb;
        assert_eq!(g.background, rgb(13, 17, 23));
        assert_eq!(g.surface, rgb(22, 27, 34));
        assert_eq!(g.card, rgb(28, 32, 40));
        assert_eq!(g.hover, rgb(33, 37, 44));
        assert_eq!(g.border, rgb(47, 52, 60));
        assert_eq!(g.text, rgb(231, 236, 244));
        assert_eq!(g.text_ui, rgb(228, 233, 240));
        assert_eq!(g.text_secondary, rgb(154, 164, 177));
        assert_eq!(
            g.text_tertiary,
            rgb(133, 137, 145),
            "TEXT3 IS the rule's caption grey since 2026-09-28"
        );
        assert_eq!(g.accent, rgb(62, 224, 138));
    }

    #[test]
    fn every_theme_clears_the_contrast_floors() {
        for id in ThemePaletteId::ALL {
            let t = ThemePalette::of(id);
            let r = |c| contrast_ratio(c, t.background);
            assert!(r(t.text) >= 7.0, "{id:?} body text {:.2}", r(t.text));
            assert!(r(t.text_ui) >= 7.0, "{id:?} UI text {:.2}", r(t.text_ui));
            assert!(r(t.text_secondary) >= 4.5, "{id:?} secondary text {:.2}", r(t.text_secondary));
            assert!(r(t.text_tertiary) >= 4.5, "{id:?} captions {:.2}", r(t.text_tertiary));
            // Captions sit on panels and cards as much as on the background (owner, 2026-09-28).
            assert!(
                contrast_ratio(t.text_tertiary, t.surface) >= 4.5,
                "{id:?} captions on a panel"
            );
            assert!(contrast_ratio(t.text_tertiary, t.card) >= 4.5, "{id:?} captions on a card");
            assert!(r(t.accent) >= 3.0, "{id:?} accent {:.2}", r(t.accent));
        }
    }

    /// The measurement that made the caption grey the one intended change.
    #[test]
    fn todays_caption_grey_was_below_the_floor() {
        let before = Color32::from_rgb(107, 116, 128);
        let r = contrast_ratio(before, ThemePalette::of(ThemePaletteId::Graphite).background);
        assert!((3.99..=4.01).contains(&r), "{r}");
    }

    #[test]
    fn the_accents_are_the_ruled_pairs() {
        let a = |id| ThemePalette::of(id).accent;
        assert_eq!(a(ThemePaletteId::Graphite), Color32::from_rgb(62, 224, 138));
        assert_eq!(a(ThemePaletteId::Midnight), Color32::from_rgb(53, 201, 242));
        assert_eq!(a(ThemePaletteId::Dusk), Color32::from_rgb(170, 130, 250));
        assert_eq!(
            a(ThemePaletteId::Carbon),
            Color32::from_rgb(242, 181, 58),
            "amber, final (2026-09-28)"
        );
    }

    /// The scrim is the theme's own background at `SCRIM_ALPHA`: content that is on screen but must
    /// not read as live (the Trade window's stale ladder) is dimmed into the theme, not into a fixed
    /// black.
    #[test]
    fn the_scrim_is_each_themes_background_at_the_scrim_alpha() {
        for id in ThemePaletteId::ALL {
            let t = ThemePalette::of(id);
            let want = Color32::from_rgba_unmultiplied(
                t.background.r(),
                t.background.g(),
                t.background.b(),
                SCRIM_ALPHA,
            );
            assert_eq!(t.scrim(), want, "{id:?}");
        }
        assert_eq!(SCRIM_ALPHA, 150, "the DOM's stale overlay was (8, 11, 15) at 150");
    }

    #[test]
    fn keys_round_trip_and_unknown_keys_are_refused() {
        for id in ThemePaletteId::ALL {
            assert_eq!(ThemePaletteId::from_key(id.key()), Some(id));
        }
        assert_eq!(ThemePaletteId::from_key("solarized"), None);
        assert_eq!(ThemePaletteId::default(), ThemePaletteId::Graphite);
        let keys: Vec<_> = ThemePaletteId::ALL.iter().map(|t| t.key()).collect();
        assert_eq!(keys, ["graphite", "midnight", "dusk", "carbon"]);
    }
}
