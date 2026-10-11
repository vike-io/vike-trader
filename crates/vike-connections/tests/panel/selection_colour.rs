//! **A SELECTION is marked by an accent SHAPE; only a STATE is marked in the status green.**
//!
//! The design system binds the accent to the theme and keeps the status colours the same in every
//! theme (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §2 and §3.2). This panel
//! paints both kinds. The selected venue's name (in the two-column rail, and in the one-column
//! arm's chips) and the selected account's chip say "this is what you are looking at": a selection.
//! A tier's `configured` mark says something about the store: a state.
//!
//! The accent is a SHAPE, never the colour of a word (§2, §4.3; `ui-theme.toml`'s
//! `accent-is-a-shape`): the selected word keeps the theme's text ink and the accent is the
//! `stroke::EDGE` line drawn directly under it. Until 2026-10-05 the word itself was painted in
//! the accent and this suite asserted exactly that; it now asserts the rule instead.
//!
//! Until 2026-09-28 the status green WAS the accent, by alias, so a selection spelled with the
//! status constant looked right and nothing could tell the two apart. They are different colours
//! now, and this suite reads the colour each text and each mark was actually PAINTED in, off the
//! real frame's shapes, so a selection cannot quietly turn into a state again.
//!
//! The accent it expects is read off the INSTALLED theme (`Tokens::of(ctx).theme.accent`), and every
//! theme is driven: a selection that spelled the Graphite accent as a constant would pass under
//! Graphite alone and stay green while the other three themes painted it wrong.

use std::collections::HashMap;

use egui::Color32;
use egui_kittest::Harness;
use vike_connections::AccountGrids;
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::theme::ThemeId;

use crate::support::PanelHarness;

/// The REAL panel over an EMPTY store at `width`, under `theme`, settled. The panel selects the
/// roster's first venue (`binance`) and the only account an empty store has (`default`).
fn panel(width: f32, theme: ThemeId) -> Harness<'static, ()> {
    let look = Appearance { theme, ..Appearance::default() };
    PanelHarness {
        size: egui::vec2(width, 900.0),
        // The panel draws icons, whose family only the app's type binds; installing the appearance
        // binds it AND hands the panel the theme it is to read its accent from.
        appearance: Some(look),
        ..PanelHarness::new(AccountGrids::from_vars(&HashMap::new()))
    }
    .build()
}

/// Every run of text the last frame painted, with the colour it was painted in.
fn painted_texts(h: &Harness<'_, ()>) -> Vec<(String, Color32)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(String, Color32)>) {
        match shape {
            egui::Shape::Text(t) => {
                let job = &t.galley.job;
                for s in &job.sections {
                    let text = job.text[s.byte_range.start.0..s.byte_range.end.0].to_string();
                    out.push((text, t.override_text_color.unwrap_or(s.format.color)));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &h.output().shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

fn colours_of(texts: &[(String, Color32)], want: &str) -> Vec<Color32> {
    texts.iter().filter(|(t, _)| t == want).map(|(_, c)| *c).collect()
}

/// The colours of the `stroke::EDGE` horizontal lines the last frame drew directly UNDER a text run
/// reading exactly `want`: a line spanning the run's own width, within a few points below its
/// bottom. That is the shape a selection is marked with, so asking for it by position rather than
/// by colour alone keeps another accent line elsewhere in the panel from satisfying the test.
fn underlines_under(h: &Harness<'_, ()>, want: &str) -> Vec<Color32> {
    type Line = ([egui::Pos2; 2], egui::Stroke);
    fn walk(shape: &egui::Shape, runs: &mut Vec<(String, egui::Rect)>, lines: &mut Vec<Line>) {
        match shape {
            egui::Shape::Text(t) => {
                // The first row WITHOUT its leading space: a label in a wrapping row (the account
                // strip's) is laid out with the cursor's indentation as leading space, and the
                // widget's own rect — what the underline spans — starts after it.
                let row = t.galley.rows.first().map(|r| r.rect_without_leading_space());
                let rect = row.unwrap_or(t.galley.rect).translate(t.pos.to_vec2());
                runs.push((t.galley.text().to_string(), rect));
            }
            egui::Shape::LineSegment { points, stroke } => lines.push((*points, *stroke)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, runs, lines)),
            _ => {}
        }
    }
    let (mut runs, mut lines) = (Vec::new(), Vec::new());
    for clipped in &h.output().shapes {
        walk(&clipped.shape, &mut runs, &mut lines);
    }
    let mut out = Vec::new();
    for (_, rect) in runs.iter().filter(|(t, _)| t == want) {
        for ([a, b], line) in &lines {
            let horizontal = (a.y - b.y).abs() < 0.01;
            let spans_the_word =
                (a.x - rect.left()).abs() < 1.0 && (b.x - rect.right()).abs() < 1.0;
            let just_under = a.y >= rect.bottom() - 1.0 && a.y <= rect.bottom() + 3.0;
            if horizontal && spans_the_word && just_under && line.width == stroke::EDGE {
                out.push(line.color);
            }
        }
    }
    out
}

/// The two-column rail (`rail_row`) and the one-column chips (`rail_chips`) draw the selected
/// venue in different code, so both widths are driven. The WORD keeps the theme's text ink and the
/// accent is the line under it.
#[test]
fn the_selected_venue_is_underlined_in_the_accent_in_both_arms() {
    for theme in ThemeId::ALL {
        for width in [560.0, 1300.0] {
            let h = panel(width, theme);
            let tokens = Tokens::of(&h.ctx);
            let (accent, ink) = (tokens.theme.accent, tokens.theme.text);
            let binance = colours_of(&painted_texts(&h), "binance");
            assert!(
                binance.contains(&ink),
                "at {width}pt under {theme:?} the selected venue `binance` was painted in \
                 {binance:?}, not the theme's text ink {ink:?}"
            );
            assert!(
                !binance.contains(&accent),
                "at {width}pt under {theme:?} the selected venue `binance` is a WORD painted in \
                 the accent {accent:?}: the accent is a shape"
            );
            assert!(
                !binance.contains(&vike_ui_theme::status::OK),
                "at {width}pt under {theme:?} a selection is painted in the status green, which \
                 is a STATE colour"
            );
            let marks = underlines_under(&h, "binance");
            assert!(
                marks.contains(&accent),
                "at {width}pt under {theme:?} the selected venue `binance` carries no accent \
                 underline; the lines under it are {marks:?}"
            );
        }
    }
}

#[test]
fn the_selected_account_chip_is_underlined_in_the_accent() {
    for theme in ThemeId::ALL {
        let h = panel(1300.0, theme);
        let tokens = Tokens::of(&h.ctx);
        let (accent, ink) = (tokens.theme.accent, tokens.theme.text);
        let chip = colours_of(&painted_texts(&h), "\u{25CF} default");
        assert!(!chip.is_empty(), "no selected-account chip `\u{25CF} default` was painted");
        assert!(
            chip.iter().all(|c| *c == ink),
            "under {theme:?} the selected account chip is {chip:?}, not the text ink {ink:?} (the \
             accent {accent:?} is a shape, never a word's colour)"
        );
        let marks = underlines_under(&h, "\u{25CF} default");
        assert!(
            marks.contains(&accent),
            "under {theme:?} the selected account chip carries no accent underline; the lines \
             under it are {marks:?}"
        );
    }
}

/// The selection FOLLOWS the theme the trader picked: the accent is not one fixed green. Graphite
/// and Midnight carry different accents, and each underlines in its own — so a constant spelled as
/// either cannot satisfy both.
#[test]
fn the_selection_follows_the_installed_theme() {
    let selected_in = |theme: ThemeId| -> (Color32, Vec<Color32>) {
        let h = panel(1300.0, theme);
        let accent = Tokens::of(&h.ctx).theme.accent;
        (accent, underlines_under(&h, "binance"))
    };
    let (graphite, on_graphite) = selected_in(ThemeId::Graphite);
    let (midnight, on_midnight) = selected_in(ThemeId::Midnight);
    assert_ne!(graphite, midnight, "the two themes share an accent, so this proves nothing");
    assert!(on_graphite.contains(&graphite), "Graphite underlined `binance` in {on_graphite:?}");
    assert!(on_midnight.contains(&midnight), "Midnight underlined `binance` in {on_midnight:?}");
    assert!(
        !on_midnight.contains(&graphite),
        "under Midnight the selection was still underlined in Graphite's accent: {on_midnight:?}"
    );
}

/// The other half: the fix must not turn STATES into the accent too. With an empty store no tier
/// is configured, so the legend's `configured` entry is the witness — and a status colour is the
/// same in every theme, so every theme is driven.
#[test]
fn a_configured_state_is_still_the_status_green() {
    for theme in ThemeId::ALL {
        let texts = painted_texts(&panel(1300.0, theme));
        let legend: Vec<Color32> =
            texts.iter().filter(|(t, _)| t.ends_with("= configured")).map(|(_, c)| *c).collect();
        assert!(!legend.is_empty(), "no `= configured` legend entry was painted under {theme:?}");
        assert!(
            legend.iter().all(|c| *c == vike_ui_theme::status::OK),
            "under {theme:?} the configured legend entry is {legend:?}"
        );
    }
}
