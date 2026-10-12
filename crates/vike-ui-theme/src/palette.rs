//! The one canonical color palette — vike `theme.py` ported to egui [`Color32`]. Every window,
//! panel, tool arm and the chart canvas reads its colors from here (directly — `vike-app` also
//! re-exported it as `theme`, a second name `vike-desktop` does not carry), and
//! [`crate::appearance::install`] builds the egui `Visuals` from the same values, so each color
//! exists in EXACTLY one place.
//!
//! Before this crate the palette was copy-pasted as ~54 per-function local `const` blocks (and a
//! handful of cross-crate copies), and the drift that invited already shipped a real bug: the News
//! and Calendar arms spelled the accent `(62,224,137)` instead of the canonical `(62,224,138)` — a
//! 1-bit green drift on a color that is supposed to be identical everywhere. Reference [`ACCENT`]
//! and it can never drift again; `tests::palette_values_match` pins every value.
//!
//! Hue notes (from `theme.py`): [`BG`] hsl(215,28,7), [`ACCENT`] hsl(148,72,56),
//! [`UP`] hsl(128,49,49), [`DOWN`] hsl(3,93,63).
//!
//! One submodule sits below those consts: [`trading`], a SECOND, deliberately separate palette —
//! the dark trading-terminal look vike-cockpit renders in (vike-panels moved onto the design
//! system in step 7). Its values are NOT these consts (trading UP is `(46,189,133)`, the app
//! [`UP`] is `(64,186,80)`) and must never be folded into them; the
//! `trading_palette_is_deliberately_not_the_app_palette` test guards that boundary.
//!
//! The status colours are not here: they are the same in every theme, so they are not Graphite's
//! to hold. They live in [`crate::status`] (design-system step 7, which deleted the `status` module
//! this file carried, with no alias left behind).

use egui::Color32;

/// App background — the plot canvas, windows, title bars, rail, dialogs. hsl(215,28,7).
pub const BG: Color32 = Color32::from_rgb(13, 17, 23);
/// Panels / tables / menus / pills — one step up from [`BG`].
pub const SURFACE: Color32 = Color32::from_rgb(22, 27, 34);
/// Raised cards (equity / trade panels) — between [`SURFACE`] and [`HOVER`].
pub const CARD: Color32 = Color32::from_rgb(28, 32, 40);
/// Hover / selection fill — also the Options centre-strike "spine" band.
pub const HOVER: Color32 = Color32::from_rgb(33, 37, 44);
/// 1px separators / widget borders.
pub const BORDER: Color32 = Color32::from_rgb(47, 52, 60);
/// Primary text in tool content (chains / news / calendar / data tables).
pub const TEXT: Color32 = Color32::from_rgb(231, 236, 244);
/// Primary text in the egui `Visuals` chrome (menus / widgets / title labels) — very slightly
/// dimmer than [`TEXT`]. Kept distinct because it is a pre-existing, deliberate value; folding it
/// into `TEXT` would be an unintended pixel change.
pub const TEXT_UI: Color32 = Color32::from_rgb(228, 233, 240);
/// Secondary text (labels, sublabels).
pub const TEXT2: Color32 = Color32::from_rgb(154, 164, 177);
/// Tertiary text (muted captions, disabled controls). Lightened 2026-09-28 from `(107, 116, 128)`,
/// which measured 4.0:1 on [`BG`], to clear the 4.5:1 floor small text needs on every ground a
/// caption sits on, cards included.
pub const TEXT3: Color32 = Color32::from_rgb(133, 137, 145);
/// Up / long / bid / beat green. hsl(128,49,49).
pub const UP: Color32 = Color32::from_rgb(64, 186, 80);
/// Down / short / ask / miss red. hsl(3,93,63).
pub const DOWN: Color32 = Color32::from_rgb(248, 82, 73);
/// Accent green — live badges, active borders, ATM row, focused pills. hsl(148,72,56). This is the
/// CANONICAL value; the News/Calendar arms had drifted to `(62,224,137)` (the bug this crate ends).
pub const ACCENT: Color32 = Color32::from_rgb(62, 224, 138);
/// Warning amber — missing-price badge, high-impact calendar glyph.
pub const WARN: Color32 = Color32::from_rgb(255, 174, 0);
/// Series blue — read by NO production code now. The options chain's CALLS side
/// (`crates/vike-chart/src/options_chain.rs`) reads the `callput` map's `CALL` row, whose role is
/// `info` because that status holds exactly this value today (the chain's own
/// `the_callput_rows_are_todays_colours` pins it); the Stored view's `covered` swatch had already left
/// `palette`. A status is fixed in every theme (`crate::status::INFO`), while a series colour belongs
/// to its chart. Its other old use, egui's hyperlink colour, is `crate::status::INFO` too. It goes with
/// `palette` (step 7).
pub const BLUE: Color32 = Color32::from_rgb(87, 165, 255);

/// The dark trading-terminal palette — the SECOND named palette in this crate, once shared by the
/// Polymarket scalp-cockpit widgets (`vike-cockpit`: chain rail / PTB header / probability ladder /
/// one-click ticket) and the DOM ladder (`vike-panels`, deleted since with the DOM window, which
/// read `UP`/`DOWN`/`ACCENT` under its own names BID/ASK/LAST). Before this module those FIVE
/// files each hand-copied the identical `const` block, plus two copies of an alpha helper
/// (`up_dim`/`down_dim` in the ladder, `bid_dim`/`ask_dim` in the DOM) — exactly the copy-shape
/// whose drift shipped the 1-bit accent bug recorded in this module's doc. (Its alpha helper is
/// `crate::color::with_alpha` now.)
///
/// DELIBERATELY a second palette, NOT the app consts above: the terminal reads in Polymarket's
/// colour language on a deeper panel stack, and the values differ on purpose — trading `UP`
/// `(46,189,133)` is not the app `UP` `(64,186,80)`. Do not fold the two together; the pin tests
/// below (`trading_palette_values_match`, `trading_palette_is_deliberately_not_the_app_palette`)
/// hold both the exact values and that boundary.
pub mod trading {
    use egui::Color32;

    /// YES / Up / bid green — Polymarket's green (the deleted DOM named it BID).
    pub const UP: Color32 = Color32::from_rgb(46, 189, 133);
    /// NO / Down / ask red — Polymarket's red (the deleted DOM named it ASK).
    pub const DOWN: Color32 = Color32::from_rgb(246, 70, 93);
    /// Amber highlight — current-window border, big countdown, selection/spread band, and the
    /// deleted DOM's LAST-trade row.
    pub const ACCENT: Color32 = Color32::from_rgb(240, 180, 41);
    /// Final-seconds countdown red — deliberately the SAME rgb as [`DOWN`] (the urgency red IS the
    /// down red), named separately where a countdown escalates (chain rail, PTB header).
    pub const URGENT: Color32 = DOWN;
    /// Primary text.
    pub const TXT: Color32 = Color32::from_rgb(210, 214, 220);
    /// Secondary text (labels, non-current cards, price rungs).
    pub const MUTED: Color32 = Color32::from_rgb(140, 149, 160);
    /// Tertiary text (faint captions, volume tags, inside-market readouts).
    pub const FAINT: Color32 = Color32::from_rgb(88, 99, 115);
    /// Ladder / current-card background — one step up from [`PANEL2`].
    pub const PANEL: Color32 = Color32::from_rgb(15, 19, 27);
    /// Deepest background — headers, toolbars, footers, the rail strip, zebra rows.
    pub const PANEL2: Color32 = Color32::from_rgb(12, 16, 23);
    /// 1px separators / card borders.
    pub const RULE: Color32 = Color32::from_rgb(29, 36, 45);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color32) -> (u8, u8, u8) {
        let [r, g, b, _a] = c.to_array();
        (r, g, b)
    }

    /// Pin EVERY palette value to its exact rgb tuple so a future edit can't silently drift one.
    /// This is the machine-enforced version of the doc's warning: the accent MUST be `(62,224,138)`,
    /// never the historically-shipped `(62,224,137)`.
    #[test]
    fn palette_values_match() {
        assert_eq!(rgb(BG), (13, 17, 23), "BG");
        assert_eq!(rgb(SURFACE), (22, 27, 34), "SURFACE");
        assert_eq!(rgb(CARD), (28, 32, 40), "CARD");
        assert_eq!(rgb(HOVER), (33, 37, 44), "HOVER");
        assert_eq!(rgb(BORDER), (47, 52, 60), "BORDER");
        assert_eq!(rgb(TEXT), (231, 236, 244), "TEXT");
        assert_eq!(rgb(TEXT_UI), (228, 233, 240), "TEXT_UI");
        assert_eq!(rgb(TEXT2), (154, 164, 177), "TEXT2");
        assert_eq!(rgb(TEXT3), (133, 137, 145), "TEXT3");
        assert_eq!(rgb(UP), (64, 186, 80), "UP");
        assert_eq!(rgb(DOWN), (248, 82, 73), "DOWN");
        assert_eq!(
            rgb(ACCENT),
            (62, 224, 138),
            "ACCENT must be the canonical (62,224,138), NEVER the drifted (62,224,137)"
        );
        assert_eq!(rgb(WARN), (255, 174, 0), "WARN");
        assert_eq!(rgb(BLUE), (87, 165, 255), "BLUE");
    }

    /// Every palette color is fully opaque (alpha 255) — `from_rgb` guarantees it, pinned here so a
    /// switch to `from_rgba_*` at any site would be caught.
    #[test]
    fn palette_is_opaque() {
        for c in [
            BG, SURFACE, CARD, HOVER, BORDER, TEXT, TEXT_UI, TEXT2, TEXT3, UP, DOWN, ACCENT, WARN,
            BLUE,
        ] {
            assert_eq!(c.to_array()[3], 255, "palette colors must be opaque");
        }
    }

    /// Pin EVERY trading-palette value to its exact rgb tuple — the same machine-enforcement the
    /// app palette gets. These are the values the five deleted hand-copied blocks (cockpit
    /// chain/header/ladder/ticket + panels dom) carried; rendering stays byte-identical only while
    /// every one of them holds.
    #[test]
    fn trading_palette_values_match() {
        assert_eq!(rgb(trading::UP), (46, 189, 133), "trading UP (dom BID)");
        assert_eq!(rgb(trading::DOWN), (246, 70, 93), "trading DOWN (dom ASK)");
        assert_eq!(rgb(trading::ACCENT), (240, 180, 41), "trading ACCENT (dom LAST)");
        assert_eq!(rgb(trading::URGENT), (246, 70, 93), "trading URGENT");
        assert_eq!(trading::URGENT, trading::DOWN, "URGENT is the down red BY DESIGN");
        assert_eq!(rgb(trading::TXT), (210, 214, 220), "trading TXT");
        assert_eq!(rgb(trading::MUTED), (140, 149, 160), "trading MUTED");
        assert_eq!(rgb(trading::FAINT), (88, 99, 115), "trading FAINT");
        assert_eq!(rgb(trading::PANEL), (15, 19, 27), "trading PANEL");
        assert_eq!(rgb(trading::PANEL2), (12, 16, 23), "trading PANEL2");
        assert_eq!(rgb(trading::RULE), (29, 36, 45), "trading RULE");
    }

    /// Every trading-palette color is fully opaque, like the app palette's — which is also what
    /// makes `crate::color::with_alpha`'s `r()`/`g()`/`b()` reads exactly the raw rgb the
    /// hand-copied helpers spelled out.
    #[test]
    fn trading_palette_is_opaque() {
        for c in [
            trading::UP,
            trading::DOWN,
            trading::ACCENT,
            trading::URGENT,
            trading::TXT,
            trading::MUTED,
            trading::FAINT,
            trading::PANEL,
            trading::PANEL2,
            trading::RULE,
        ] {
            assert_eq!(c.to_array()[3], 255, "trading palette colors must be opaque");
        }
    }

    /// The audit's DO-NOT, machine-enforced: the trading palette is DELIBERATELY not the app
    /// palette. A future "dedup" folding trading::UP into the app UP would repaint every surface
    /// still reading it — this test makes that a red build instead of a silent pixel change.
    #[test]
    fn trading_palette_is_deliberately_not_the_app_palette() {
        assert_ne!(trading::UP, UP, "cockpit green (46,189,133) is NOT the app UP (64,186,80)");
        assert_ne!(trading::DOWN, DOWN, "cockpit red (246,70,93) is NOT the app DOWN (248,82,73)");
        assert_ne!(
            trading::ACCENT,
            ACCENT,
            "cockpit amber (240,180,41) is NOT the app ACCENT green (62,224,138)"
        );
        assert_ne!(trading::TXT, TEXT, "terminal text (210,214,220) is NOT the app TEXT");
    }
}
