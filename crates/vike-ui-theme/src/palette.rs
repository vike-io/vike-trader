//! The one canonical color palette — vike `theme.py` ported to egui [`Color32`]. Every window,
//! panel, tool arm and the chart canvas reads its colors from here (via `vike-app`'s `theme`
//! re-export, or directly), and `vike-app`'s `install_visuals` builds the egui `Visuals` from the
//! same constants, so each color exists in EXACTLY one place.
//!
//! Before this crate the palette was copy-pasted as ~54 per-function local `const` blocks (and a
//! handful of cross-crate copies), and the drift that invited already shipped a real bug: the News
//! and Calendar arms spelled the accent `(62,224,137)` instead of the canonical `(62,224,138)` — a
//! 1-bit green drift on a color that is supposed to be identical everywhere. Reference [`ACCENT`]
//! and it can never drift again; [`palette_values_match`](tests) pins every value.
//!
//! Hue notes (from `theme.py`): [`BG`] hsl(215,28,7), [`ACCENT`] hsl(148,72,56),
//! [`UP`] hsl(128,49,49), [`DOWN`] hsl(3,93,63).
//!
//! Two submodules sit below those consts, and they are opposites — read which is which before
//! adding a third:
//!
//! * [`trading`] is a SECOND, deliberately separate palette — the dark trading-terminal look
//!   vike-cockpit and vike-panels render in. Its values are NOT these consts (trading UP is
//!   `(46,189,133)`, the app [`UP`] is `(64,186,80)`) and must never be folded into them; the
//!   `trading_palette_is_deliberately_not_the_app_palette` test guards that boundary.
//! * [`status`] is NOT a second palette — it is THIS palette under live-link status names, and two
//!   of its four constants are literally [`ACCENT`] and [`DOWN`]. It exists because two surfaces
//!   painting one `ConnectionState` had each grown a private near-copy of the others' colours; its
//!   own doc carries the table of what differed and the rule that chose the survivors.

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
/// Tertiary text (muted captions, disabled controls).
pub const TEXT3: Color32 = Color32::from_rgb(107, 116, 128);
/// Up / long / bid / beat green. hsl(128,49,49).
pub const UP: Color32 = Color32::from_rgb(64, 186, 80);
/// Down / short / ask / miss red. hsl(3,93,63).
pub const DOWN: Color32 = Color32::from_rgb(248, 82, 73);
/// Accent green — live badges, active borders, ATM row, focused pills. hsl(148,72,56). This is the
/// CANONICAL value; the News/Calendar arms had drifted to `(62,224,137)` (the bug this crate ends).
pub const ACCENT: Color32 = Color32::from_rgb(62, 224, 138);
/// Warning amber — missing-price badge, high-impact calendar glyph.
pub const WARN: Color32 = Color32::from_rgb(255, 174, 0);
/// Info / series blue — the options-chain CALLS side, the CVD line, the first default indicator
/// color. `vike-chart/src/options_chain.rs` referenced `theme::BLUE` in a comment before this crate
/// existed (theme.rs never actually defined it); this is that value made real.
pub const BLUE: Color32 = Color32::from_rgb(87, 165, 255);

/// The LIVE-LINK status palette — the four colours a connection state is painted in, shared by the
/// bottom status strip's dots (`crates/vike-app-core/src/status_dot.rs`'s `dot_color_for`) and the
/// Connections tool's Status column (`crates/vike-connections/src/view.rs`'s
/// `connection_state_label_color`).
///
/// ⚠ **This is NOT a third palette.** It is the app palette above under status names plus the two
/// colours that palette had no name for: [`CONNECTED`] IS [`ACCENT`] and [`ERROR`] IS [`DOWN`], held
/// equal by `status_palette_is_the_app_palette_under_status_names`. Only the amber and the muted grey
/// are values of their own here, and only because nothing above could stand in for either — [`WARN`]
/// is a far more saturated orange (a missing-price badge, not a link in transition) and [`TEXT3`] is
/// a blue-tinted text grey.
///
/// # Why it exists — two surfaces, one state, near-identical-but-different colours
///
/// The two call sites above classify the SAME
/// `crates/vike-model/src/feed_status.rs`'s `ConnectionState` — since #1569 through the same shared
/// parser — and then painted it from two private colour sets that differed by a few points each:
///
/// | state | the strip carried | the Connections tool carried |
/// |---|---|---|
/// | `Connected` | [`ACCENT`] | [`ACCENT`] — the one they already agreed on |
/// | `Connecting` | `(230,180,40)` | `(224,176,62)` |
/// | `Error` | [`DOWN`] `(248,82,73)` | `(224,96,96)` |
/// | `Disconnected` / `Unknown` | `from_gray(120)` | `from_gray(110)` |
///
/// That is the drift shape this whole module exists to end — the same one that shipped the 1-bit
/// accent bug — and it was visible to an operator: one feed, two panels, two ambers.
///
/// **The values are the STRIP's, and the rule that chose them is not taste.** Two of the strip's
/// four colours were already canonical palette constants, so adopting its set keeps [`ACCENT`] and
/// [`DOWN`] in play and folds three private near-copies away; adopting the tool's set would have
/// replaced the canonical [`DOWN`] with a private near-copy of it, which is the drift running
/// backwards. The Connections tool therefore repaints — its amber, its red and its grey each move to
/// the column above — and `the_superseded_connections_colours_are_gone` pins that those three
/// literals are not what these constants hold.
pub mod status {
    use egui::Color32;

    /// A healthy live link. THE SAME VALUE as the app [`ACCENT`](super::ACCENT) — named here so a
    /// status surface reads its whole set from one place, not so a second green can exist.
    pub const CONNECTED: Color32 = super::ACCENT;
    /// A link in transition — dialling, subscribing, reconnecting.
    ///
    /// ⚠ It has a SECOND, non-state use in each consumer, which is why it is a colour role rather
    /// than a state name: the strip paints the armed remote-control segment in it
    /// (`crates/vike-app-core/src/status_dot.rs`'s `control_dot_color` — loud on purpose, because an
    /// observer with an armed channel can place REAL orders), and the Connections tool paints the
    /// provisional `(new)` account chip in it. Both mean "not settled yet"; neither is `Connecting`.
    pub const CONNECTING: Color32 = Color32::from_rgb(230, 180, 40);
    /// A faulted link. THE SAME VALUE as the app [`DOWN`](super::DOWN) — see [`CONNECTED`].
    ///
    /// Its second use is the Connections tool's inline error text.
    pub const ERROR: Color32 = super::DOWN;
    /// Nothing live to show — `Disconnected` and `Unknown` share it deliberately in BOTH consumers
    /// (only a label can tell those two apart, and the strip has no label).
    ///
    /// Its second use is the Connections tool's "not set" credential glyph, which is the same claim
    /// about a different subject.
    pub const MUTED: Color32 = Color32::from_gray(120);
}

/// The dark trading-terminal palette — the SECOND named palette in this crate, shared by the
/// Polymarket scalp-cockpit widgets (`vike-cockpit`: chain rail / PTB header / probability ladder /
/// one-click ticket) and the DOM ladder (`vike-panels`, which reads `UP`/`DOWN`/`ACCENT` under its
/// own names BID/ASK/LAST). Before this module those FIVE files each hand-copied the identical
/// `const` block, plus two copies of the [`trading::dim`] alpha helper (`up_dim`/`down_dim` in the
/// ladder, `bid_dim`/`ask_dim` in the DOM) — exactly the copy-shape whose drift shipped the 1-bit
/// accent bug recorded in this module's doc.
///
/// DELIBERATELY a second palette, NOT the app consts above: the terminal reads in Polymarket's
/// colour language on a deeper panel stack, and the values differ on purpose — trading `UP`
/// `(46,189,133)` is not the app `UP` `(64,186,80)`. Do not fold the two together; the pin tests
/// below (`trading_palette_values_match`, `trading_palette_is_deliberately_not_the_app_palette`)
/// hold both the exact values and that boundary.
pub mod trading {
    use egui::Color32;

    /// YES / Up / bid green — Polymarket's green (the DOM names it BID).
    pub const UP: Color32 = Color32::from_rgb(46, 189, 133);
    /// NO / Down / ask red — Polymarket's red (the DOM names it ASK).
    pub const DOWN: Color32 = Color32::from_rgb(246, 70, 93);
    /// Amber highlight — current-window border, big countdown, selection/spread band, the DOM's
    /// LAST-trade row.
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

    /// `c` at alpha `a` (unmultiplied) — the one home for the hand-copied `up_dim`/`down_dim`
    /// (probability ladder) and `bid_dim`/`ask_dim` (DOM) helpers: depth bars, inside-market
    /// tints, the position row. Byte-identical to those helpers for every (opaque) palette
    /// colour — pinned by `trading_dim_matches_the_hand_copied_helpers`.
    pub fn dim(c: Color32, a: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
    }
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
        assert_eq!(rgb(TEXT3), (107, 116, 128), "TEXT3");
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

    /// Pin every status-palette value to its exact rgb tuple, like the two palettes around it.
    #[test]
    fn status_palette_values_match() {
        assert_eq!(rgb(status::CONNECTED), (62, 224, 138), "status CONNECTED");
        assert_eq!(rgb(status::CONNECTING), (230, 180, 40), "status CONNECTING");
        assert_eq!(rgb(status::ERROR), (248, 82, 73), "status ERROR");
        assert_eq!(rgb(status::MUTED), (120, 120, 120), "status MUTED");
        for c in [status::CONNECTED, status::CONNECTING, status::ERROR, status::MUTED] {
            assert_eq!(c.to_array()[3], 255, "status palette colors must be opaque");
        }
    }

    /// The half of [`status`]'s contract that a value pin cannot express: two of its four constants
    /// are ALIASES, not new colours. A "tidy-up" that respelled either as its own literal would keep
    /// `status_palette_values_match` green while re-creating exactly the drift this module ends.
    #[test]
    fn status_palette_is_the_app_palette_under_status_names() {
        assert_eq!(status::CONNECTED, ACCENT, "status CONNECTED IS the app ACCENT");
        assert_eq!(status::ERROR, DOWN, "status ERROR IS the app DOWN");
        // ...and the two that are NOT aliases must stay distinct from the app consts that are
        // closest to them, or the module would be claiming an equality it does not have.
        assert_ne!(status::CONNECTING, WARN, "the transition amber is not the WARN orange");
        assert_ne!(status::MUTED, TEXT3, "the status grey is not the blue-tinted text grey");
    }

    /// The Connections tool carried three private near-copies of the strip's colours until this
    /// module existed (its own doc has the table). They are gone; this test is what keeps them gone,
    /// because "somebody re-adds the old amber" and "somebody edits the new one" are the same diff.
    #[test]
    fn the_superseded_connections_colours_are_gone() {
        assert_ne!(
            rgb(status::CONNECTING),
            (224, 176, 62),
            "the Connections tool's old amber must not be the shared one"
        );
        assert_ne!(
            rgb(status::ERROR),
            (224, 96, 96),
            "the Connections tool's old red must not be the shared one — it was a near-copy of DOWN"
        );
        assert_ne!(rgb(status::MUTED), (110, 110, 110), "the Connections tool's old grey");
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
    /// makes [`trading::dim`]'s `r()`/`g()`/`b()` reads exactly the raw rgb the hand-copied
    /// helpers spelled out.
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
    /// palette. A future "dedup" folding trading::UP into the app UP would repaint every cockpit
    /// and DOM surface — this test makes that a red build instead of a silent pixel change.
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

    /// [`trading::dim`] must be byte-identical to the deleted hand-copied helpers
    /// (`up_dim`/`bid_dim` = rgba(46,189,133,a), `down_dim`/`ask_dim` = rgba(246,70,93,a)) at
    /// every alpha the widgets actually paint (DOM 28/46/48; ladder tint 30 + the 30..=150 depth
    /// heatmap ramp) plus the u8 edges.
    #[test]
    fn trading_dim_matches_the_hand_copied_helpers() {
        for a in [0u8, 8, 28, 30, 46, 48, 90, 150, 255] {
            assert_eq!(
                trading::dim(trading::UP, a).to_array(),
                Color32::from_rgba_unmultiplied(46, 189, 133, a).to_array(),
                "up_dim/bid_dim({a})"
            );
            assert_eq!(
                trading::dim(trading::DOWN, a).to_array(),
                Color32::from_rgba_unmultiplied(246, 70, 93, a).to_array(),
                "down_dim/ask_dim({a})"
            );
        }
    }
}
