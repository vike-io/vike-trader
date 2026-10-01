//! **A SELECTION is marked in the accent; only a STATE is marked in the status green.**
//!
//! The design system binds the accent to the theme and keeps the status colours the same in every
//! theme (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §2 and §3.2). This panel
//! paints both kinds. The selected venue's name (in the two-column rail, and in the one-column
//! arm's chips) and the selected account's chip say "this is what you are looking at": a selection.
//! A tier's `configured` mark says something about the store: a state.
//!
//! Until 2026-09-28 the status green WAS the accent, by alias, so a selection spelled with the
//! status constant looked right and nothing could tell the two apart. They are different colours
//! now, and this suite reads the colour each text was actually PAINTED in, off the real frame's
//! shapes, so a selection cannot quietly turn into a state again.

use std::collections::HashMap;
use std::path::Path;

use egui::Color32;
use egui_kittest::Harness;
use vike_connections::{AccountGrids, CredentialWrite, StoreHealth, connections_ui};
use vike_model::feed_status::ConnectionState;
use vike_ui_theme::palette;

/// A store path this suite never writes — same idiom, and the same reason, as
/// `connections_layout.rs`'s `NEVER_WRITTEN_STORE`.
const NEVER_WRITTEN_STORE: &str = "connections-selection-tests-never-save-to-this.env";

fn test_proc() -> &'static vike_model::change_journal::Proc {
    static PROC: std::sync::OnceLock<vike_model::change_journal::Proc> = std::sync::OnceLock::new();
    PROC.get_or_init(|| vike_model::change_journal::Proc::new("vike-test", 0, "0"))
}

/// The REAL panel over an EMPTY store at `width`, settled. The panel selects the roster's first
/// venue (`binance`) and the only account an empty store has (`default`).
fn panel(width: f32) -> Harness<'static, ()> {
    let grids = AccountGrids::from_vars(&HashMap::new());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite {
        store: Path::new(NEVER_WRITTEN_STORE),
        journal: None,
        proc: test_proc(),
        now_ms: 0,
    };
    let mut h = Harness::builder().with_size(egui::vec2(width, 900.0)).build_ui(move |ui| {
        // The panel draws icons, whose family only the app's type binds.
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
        }
    });
    h.run();
    h
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

/// The two-column rail (`rail_row`) and the one-column chips (`rail_chips`) draw the selected
/// venue in different code, so both widths are driven.
#[test]
fn the_selected_venue_is_named_in_the_accent_in_both_arms() {
    for width in [560.0, 1300.0] {
        let texts = painted_texts(&panel(width));
        let binance = colours_of(&texts, "binance");
        assert!(
            binance.contains(&palette::ACCENT),
            "at {width}pt the selected venue `binance` was painted in {binance:?}, not the accent"
        );
        assert!(
            !binance.contains(&vike_ui_theme::status::OK),
            "at {width}pt a selection is painted in the status green, which is a STATE colour"
        );
    }
}

#[test]
fn the_selected_account_chip_is_the_accent() {
    let texts = painted_texts(&panel(1300.0));
    let chip = colours_of(&texts, "\u{25CF} default");
    assert!(!chip.is_empty(), "no selected-account chip `\u{25CF} default` was painted");
    assert!(chip.iter().all(|c| *c == palette::ACCENT), "the selected account chip is {chip:?}");
}

/// The other half: the fix must not turn STATES into the accent too. With an empty store no tier
/// is configured, so the legend's `configured` entry is the witness.
#[test]
fn a_configured_state_is_still_the_status_green() {
    let texts = painted_texts(&panel(1300.0));
    let legend: Vec<Color32> =
        texts.iter().filter(|(t, _)| t.ends_with("= configured")).map(|(_, c)| *c).collect();
    assert!(!legend.is_empty(), "no `= configured` legend entry was painted");
    assert!(
        legend.iter().all(|c| *c == vike_ui_theme::status::OK),
        "the configured legend entry is {legend:?}"
    );
}
