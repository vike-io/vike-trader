//! The Studio follows the installed appearance (design system spec §2, §3.1–§3.3): the theme's
//! accent marks its shapes, the market-colour set paints its money, and none of the colours the
//! Studio used to keep for itself (`crates/vike-studio/src/theme.rs`, which this migration deletes)
//! is painted any more — the violet survives only as Dusk's own accent.
//!
//! The last frame is TESSELLATED and its vertex colours read. A vertex carries exactly the colour
//! its shape was painted in — a glyph's vertices carry its text colour — so "is this colour in the
//! set" means "did anything paint it". The frame schedule and viewport are
//! `crates/vike-studio/tests/tessellation_goldens.rs`'s; the appearance is the app's WHOLE
//! appearance (`vike_ui_theme::appearance::install`), not only its type, because the colours are
//! the point.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{RefusingCatalogStore, seeded_store, state};
use egui::epaint::{ClippedShape, Primitive};
use vike_studio::{ChatApiKeys, CompareRow, RightTab, SavedPane, StoreHandle, StrategySource};
use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::TextSize;

/// The Studio's own accent. It is Dusk's accent too, which is the one theme allowed to paint it.
const VIOLET: [u8; 4] = [170, 130, 250, 255];

/// Colours the Studio painted for itself before this migration, as the committed goldens record
/// them (premultiplied RGBA). None of them may come back.
const RETIRED: [[u8; 4]; 8] = [
    VIOLET,
    [78, 201, 78, 255],       // its ok
    [224, 168, 48, 255],      // its warn
    [217, 78, 78, 255],       // its error
    [0x1f, 0x17, 0x2d, 0x2e], // the rail's selected fill: the violet at 0.18
    [0x4d, 0x3b, 0x71, 0x73], // the empty-state art: the violet at 0.45
    [0x0c, 0x1e, 0x0c, 0x26], // the compile chip's tint: its ok at 0.15
    [0x5a, 0x44, 0x9e, 255],  // its primary fill
];

/// Three frames of `draw` with `a` installed, on the goldens' viewport and clock. Returns the
/// context (the font atlas lives there) and the last frame's shapes and scale.
fn last_frame(
    a: Appearance,
    mut draw: impl FnMut(&mut egui::Ui),
) -> (egui::Context, Vec<ClippedShape>, f32) {
    let ctx = egui::Context::default();
    appearance::install(&ctx, &a);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 960.0));
    let mut last = (Vec::new(), 1.0);
    for f in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| draw(ui));
        // egui 0.36 panics on dropping texture deltas nobody applied.
        out.textures_delta.clear();
        last = (std::mem::take(&mut out.shapes), out.pixels_per_point);
    }
    (ctx, last.0, last.1)
}

/// Every colour a vertex of the last frame carries.
fn colours_of(a: Appearance, draw: impl FnMut(&mut egui::Ui)) -> BTreeSet<[u8; 4]> {
    let (ctx, shapes, ppp) = last_frame(a, draw);
    ctx.tessellate(shapes, ppp)
        .iter()
        .filter_map(|p| match &p.primitive {
            Primitive::Mesh(m) => Some(m),
            _ => None,
        })
        .flat_map(|m| m.vertices.iter().map(|v| v.color.to_array()))
        .collect()
}

/// The goldens' `sweep_seeded_store` (a store with data: the getting-started panel, `Run` armed)
/// or `sweep_refused_scan` (the refusal in the central panel), painted under `a`.
fn shell_colours(refused: bool, a: Appearance) -> BTreeSet<[u8; 4]> {
    let (dir, store): (tempfile::TempDir, StoreHandle) = if refused {
        (tempfile::tempdir().expect("temp dir"), Arc::new(RefusingCatalogStore))
    } else {
        seeded_store()
    };
    let mut st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
    colours_of(a, |ui| st.ui(ui))
}

#[test]
fn the_shell_paints_each_themes_accent_and_none_of_the_studios_own_colours() {
    for refused in [false, true] {
        let pose = if refused { "refused" } else { "seeded" };
        for id in ThemeId::ALL {
            let c = shell_colours(refused, Appearance { theme: id, ..Appearance::default() });
            assert!(c.len() > 20, "{pose} {id:?}: only {} colours — nothing was painted", c.len());
            let accent = Theme::of(id).accent.to_array();
            assert!(c.contains(&accent), "{pose} {id:?}: no shape carries the theme's accent");
            for r in RETIRED {
                let dusks_own = id == ThemeId::Dusk && r == VIOLET;
                assert!(
                    dusks_own || !c.contains(&r),
                    "{pose} {id:?}: the Studio still paints {r:?}"
                );
            }
        }
    }
}

/// Two compare rows, one up and one down, so every money colour has something to paint.
fn compare_pane() -> SavedPane {
    let row = |name: &str, sharpe: f64, max_dd: f64, equity: Vec<f64>| CompareRow {
        name: name.to_string(),
        source: StrategySource::Rhai,
        sharpe,
        final_equity: equity[equity.len() - 1],
        n_trades: 3,
        max_dd,
        equity,
        error: None,
    };
    SavedPane {
        compare_rows: Some(vec![
            row("winner", 1.5, 0.02, vec![100.0, 104.0, 110.0]),
            row("loser", -0.7, 0.12, vec![100.0, 97.0, 90.0]),
        ]),
        ..SavedPane::default()
    }
}

#[test]
fn money_follows_the_market_colour_set() {
    for m in MarketId::ALL {
        let want = MarketColors::of(m);
        let mut pane = compare_pane();
        let c = colours_of(Appearance { market: m, ..Appearance::default() }, |ui| {
            pane.ui(ui, None);
        });
        for (what, colour) in [
            ("a positive Sharpe", want.up_text),
            ("a negative Sharpe and a drawdown", want.down_text),
            ("a rising sparkline", want.up),
            ("a falling sparkline", want.down),
        ] {
            assert!(c.contains(&colour.to_array()), "{m:?}: {what} is not painted {colour:?}");
        }
        for r in [RETIRED[1], RETIRED[3]] {
            assert!(!c.contains(&r), "{m:?}: money is still the Studio's own {r:?}");
        }
    }
}

/// Every text shape of the last frame of `draw` under `a`, with the size its first section is
/// laid out at.
fn text_sizes(a: Appearance, draw: impl FnMut(&mut egui::Ui)) -> Vec<(String, f32)> {
    let (_ctx, shapes, _ppp) = last_frame(a, draw);
    let mut flat = Vec::new();
    for c in shapes {
        flatten(c.shape, &mut flat);
    }
    flat.into_iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => {
                let size = t.galley.job.sections.first()?.format.font_id.size;
                Some((t.galley.text().to_string(), size))
            }
            _ => None,
        })
        .collect()
}

fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
    match shape {
        egui::Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// Text follows the text-size setting (spec §3.3, §5) — headings and a caption on the Sweep pose:
/// the toolbar's "Studio" and the Sweep pane's own title are Titles, the Sweep pane's "Template"
/// is Body. Each must be painted exactly once, at its role's size on each scale.
#[test]
fn text_follows_the_text_size_setting() {
    for (text, small, standard, large) in [
        ("Studio", 14.0, 15.0, 17.0),
        ("Template", 11.0, 12.0, 14.0),
        ("Sweep & Validate", 14.0, 15.0, 17.0),
    ] {
        for (size, want) in
            [(TextSize::Small, small), (TextSize::Standard, standard), (TextSize::Large, large)]
        {
            let (dir, store) = seeded_store();
            let mut st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
            let a = Appearance { text_size: size, ..Appearance::default() };
            let got: Vec<f32> = text_sizes(a, |ui| st.ui(ui))
                .into_iter()
                .filter(|(t, _)| t == text)
                .map(|(_, px)| px)
                .collect();
            assert_eq!(got, [want], "{text:?} at {size:?}");
        }
    }
}
