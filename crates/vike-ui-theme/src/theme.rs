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
//! OVERRIDES equal to `crate::palette`'s constants, so the default look does not move. Its caption
//! grey and analysis line follow the rule. The caption grey is the one intended visible change:
//! 4.00:1 on the background before; after, 5.39:1 on the background and 4.65:1 on cards.

use egui::Color32;

/// One of the four built-in themes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeId {
    #[default]
    Graphite,
    Midnight,
    Dusk,
    Carbon,
}

impl ThemeId {
    /// Every theme, in the order the Appearance screen lists them.
    pub const ALL: [ThemeId; 4] =
        [ThemeId::Graphite, ThemeId::Midnight, ThemeId::Dusk, ThemeId::Carbon];

    /// The value the `preferences.theme` settings row stores for this theme.
    pub fn key(self) -> &'static str {
        match self {
            ThemeId::Graphite => "graphite",
            ThemeId::Midnight => "midnight",
            ThemeId::Dusk => "dusk",
            ThemeId::Carbon => "carbon",
        }
    }

    /// The theme a stored `preferences.theme` value names, or `None` for a value no theme has.
    pub fn from_key(key: &str) -> Option<ThemeId> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    /// The name the Appearance screen shows for this theme.
    pub fn label(self) -> &'static str {
        match self {
            ThemeId::Graphite => "Graphite",
            ThemeId::Midnight => "Midnight",
            ThemeId::Dusk => "Dusk",
            ThemeId::Carbon => "Carbon",
        }
    }
}

/// Every colour a theme supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Windows, title bars, dialogs, the chart canvas's bottom stop.
    pub bg: Color32,
    /// The chart canvas gradient's top stop.
    pub grad_top: Color32,
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
    pub text2: Color32,
    /// Captions, units, axis labels, disabled controls.
    pub text3: Color32,
    /// The neutral line between plus and minus: a depth chart's bid/ask boundary, a PnL zero line.
    pub analysis_line: Color32,
    /// The theme's accent — a SHAPE only (underline, fill, ring, edge), never a number's colour.
    pub accent: Color32,
}

/// The alpha of [`Theme::scrim`].
pub const SCRIM_ALPHA: u8 = 150;

impl Theme {
    /// The constants for one theme.
    pub fn of(id: ThemeId) -> &'static Theme {
        match id {
            ThemeId::Graphite => &GRAPHITE,
            ThemeId::Midnight => &MIDNIGHT,
            ThemeId::Dusk => &DUSK,
            ThemeId::Carbon => &CARBON,
        }
    }

    /// The background at [`SCRIM_ALPHA`] — laid over content that is on screen but must not read
    /// as live: the Trade window's stale ladder (`vike_panels::trade::ladder`). It replaced the
    /// deleted DOM ladder's own `(8, 11, 15, 150)`.
    pub fn scrim(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(self.bg.r(), self.bg.g(), self.bg.b(), SCRIM_ALPHA)
    }
}

// The four themes' colours: GENERATED from `crates/vike-ui-theme/ui-theme.toml`. Production reads these
// constants; the derivation rule and the contrast floors below are tests over them.
include!("theme_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::{contrast_ratio, lerp};
    use crate::palette;

    // THE RULE (design system spec §3.1). Each fraction moves one base colour toward another.
    const SURFACE_T: f32 = 0.045;
    const CARD_T: f32 = 0.07;
    const HOVER_T: f32 = 0.09;
    const BORDER_T: f32 = 0.155;
    const TEXT_UI_T: f32 = 0.015;
    const TEXT2_T: f32 = 0.33;
    const ANALYSIS_T: f32 = 0.35;
    /// The caption grey is the DARKEST hundredth-step from the text toward the background that still
    /// clears this on the CARD — the lightest ground captions sit on (owner, 2026-09-28). 4.5:1 plus a
    /// margin, so rounding cannot land it under the floor. Clearing it on the card clears it on the
    /// panel and the background too, because both are darker.
    const TEXT3_MIN_CONTRAST: f64 = 4.6;

    fn caption_grey(text: Color32, bg: Color32, card: Color32) -> Color32 {
        (0..=100u32)
            .rev()
            .map(|i| lerp(text, bg, i as f32 / 100.0))
            .find(|c| contrast_ratio(*c, card) >= TEXT3_MIN_CONTRAST)
            .expect("at step 0 the grey IS the text, which clears the floor on every theme")
    }

    /// Every token of `t` as the rule computes it from `t`'s four base colours.
    fn derived(t: &Theme) -> Theme {
        Theme {
            bg: t.bg,
            grad_top: t.grad_top,
            text: t.text,
            accent: t.accent,
            surface: lerp(t.bg, t.text, SURFACE_T),
            card: lerp(t.bg, t.text, CARD_T),
            hover: lerp(t.bg, t.text, HOVER_T),
            border: lerp(t.bg, t.text, BORDER_T),
            text_ui: lerp(t.text, t.bg, TEXT_UI_T),
            text2: lerp(t.text, t.bg, TEXT2_T),
            // The theme's own card, so Graphite's caption grey is measured on the card it really has.
            text3: caption_grey(t.text, t.bg, t.card),
            analysis_line: lerp(t.bg, t.text, ANALYSIS_T),
        }
    }

    #[test]
    fn every_theme_but_graphite_is_exactly_the_rule() {
        for id in [ThemeId::Midnight, ThemeId::Dusk, ThemeId::Carbon] {
            assert_eq!(*Theme::of(id), derived(Theme::of(id)), "{id:?} drifted from the rule");
        }
    }

    /// Graphite is today's look: its neutrals are OVERRIDES pinned to `palette` below, and only its
    /// caption grey and analysis line come from the rule.
    #[test]
    fn graphite_follows_the_rule_for_its_caption_grey_and_analysis_line() {
        let g = Theme::of(ThemeId::Graphite);
        let d = derived(g);
        assert_eq!(g.text3, d.text3);
        assert_eq!(g.analysis_line, d.analysis_line);
    }

    #[test]
    fn graphite_is_todays_palette() {
        let g = Theme::of(ThemeId::Graphite);
        assert_eq!(g.bg, palette::BG);
        assert_eq!(g.surface, palette::SURFACE);
        assert_eq!(g.card, palette::CARD);
        assert_eq!(g.hover, palette::HOVER);
        assert_eq!(g.border, palette::BORDER);
        assert_eq!(g.text, palette::TEXT);
        assert_eq!(g.text_ui, palette::TEXT_UI);
        assert_eq!(g.text2, palette::TEXT2);
        assert_eq!(g.text3, palette::TEXT3, "TEXT3 IS the rule's caption grey since 2026-09-28");
        assert_eq!(g.accent, palette::ACCENT);
    }

    #[test]
    fn every_theme_clears_the_contrast_floors() {
        for id in ThemeId::ALL {
            let t = Theme::of(id);
            let r = |c| contrast_ratio(c, t.bg);
            assert!(r(t.text) >= 7.0, "{id:?} body text {:.2}", r(t.text));
            assert!(r(t.text_ui) >= 7.0, "{id:?} UI text {:.2}", r(t.text_ui));
            assert!(r(t.text2) >= 4.5, "{id:?} secondary text {:.2}", r(t.text2));
            assert!(r(t.text3) >= 4.5, "{id:?} captions {:.2}", r(t.text3));
            // Captions sit on panels and cards as much as on the background (owner, 2026-09-28).
            assert!(contrast_ratio(t.text3, t.surface) >= 4.5, "{id:?} captions on a panel");
            assert!(contrast_ratio(t.text3, t.card) >= 4.5, "{id:?} captions on a card");
            assert!(r(t.accent) >= 3.0, "{id:?} accent {:.2}", r(t.accent));
        }
    }

    /// The measurement that made the caption grey the one intended change.
    #[test]
    fn todays_caption_grey_was_below_the_floor() {
        let before = Color32::from_rgb(107, 116, 128);
        let r = contrast_ratio(before, palette::BG);
        assert!((3.99..=4.01).contains(&r), "{r}");
    }

    #[test]
    fn the_accents_are_the_ruled_pairs() {
        let a = |id| Theme::of(id).accent;
        assert_eq!(a(ThemeId::Graphite), Color32::from_rgb(62, 224, 138));
        assert_eq!(a(ThemeId::Midnight), Color32::from_rgb(53, 201, 242));
        assert_eq!(a(ThemeId::Dusk), Color32::from_rgb(170, 130, 250));
        assert_eq!(
            a(ThemeId::Carbon),
            Color32::from_rgb(242, 181, 58),
            "amber, final (2026-09-28)"
        );
    }

    /// The scrim is the theme's own background at `SCRIM_ALPHA`: content that is on screen but must
    /// not read as live (the Trade window's stale ladder) is dimmed into the theme, not into a fixed
    /// black.
    #[test]
    fn the_scrim_is_each_themes_background_at_the_scrim_alpha() {
        for id in ThemeId::ALL {
            let t = Theme::of(id);
            let want = Color32::from_rgba_unmultiplied(t.bg.r(), t.bg.g(), t.bg.b(), SCRIM_ALPHA);
            assert_eq!(t.scrim(), want, "{id:?}");
        }
        assert_eq!(SCRIM_ALPHA, 150, "the DOM's stale overlay was (8, 11, 15) at 150");
    }

    #[test]
    fn keys_round_trip_and_unknown_keys_are_refused() {
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::from_key(id.key()), Some(id));
        }
        assert_eq!(ThemeId::from_key("solarized"), None);
        assert_eq!(ThemeId::default(), ThemeId::Graphite);
        let keys: Vec<_> = ThemeId::ALL.iter().map(|t| t.key()).collect();
        assert_eq!(keys, ["graphite", "midnight", "dusk", "carbon"]);
    }
}
