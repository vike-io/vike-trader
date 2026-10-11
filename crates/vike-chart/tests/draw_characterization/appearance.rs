//! What the canvas paints: the solid/gradient background, the appearance's colours, the type roles.

use crate::common::{Case, Section, flat_bars, make_state, text_sections, wave_bars};
use vike_chart::chart::{PaneKey, style::ChartStyle};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::{TextRole, TextSize};

// ==================== background: solid vs gradient (render output) ====================

/// Every mesh-vertex color emitted this frame, recursing into `Shape::Vec` groups. The
/// gradient background is painted as a `Shape::Mesh`, so its stop colors show up here — this
/// is how the two tests below prove the paint actually reached the render output (not just the
/// config), without a GPU.
fn mesh_vertex_colors(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Color32> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Mesh(m) => out.extend(m.vertices.iter().map(|v| v.color)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for cs in shapes {
        walk(&cs.shape, &mut out);
    }
    out
}

#[test]
fn gradient_background_emits_mesh_with_both_stops() {
    // Sentinel stop colors that nothing else in the chart paints, so finding them among the
    // frame's mesh vertices unambiguously proves the gradient background mesh was emitted with
    // the top stop (`bg_top`) at its top and the bottom stop (`bg`) at its bottom.
    let state = make_state(wave_bars(60));
    let mut case = Case::new(&state);
    case.options.bg_gradient = true;
    case.options.bg_top = Some([1, 2, 3]);
    case.options.bg = Some([6, 7, 8]);

    let (_acts, out) = case.run_frames();
    let colors = mesh_vertex_colors(&out.shapes);
    assert!(
        colors.contains(&egui::Color32::from_rgb(1, 2, 3)),
        "top stop reached the render output"
    );
    assert!(
        colors.contains(&egui::Color32::from_rgb(6, 7, 8)),
        "bottom stop reached the render output"
    );
}

#[test]
fn solid_background_never_paints_bg_top() {
    // Gradient OFF (today's default): `bg_top` must be inert. A sentinel `bg_top` that never
    // shows up as a painted vertex proves the solid path is byte-identical to before the feature.
    let state = make_state(wave_bars(60));
    let mut case = Case::new(&state);
    case.options.bg_gradient = false;
    case.options.bg_top = Some([1, 2, 3]);

    let (_acts, out) = case.run_frames();
    let colors = mesh_vertex_colors(&out.shapes);
    assert!(
        !colors.contains(&egui::Color32::from_rgb(1, 2, 3)),
        "solid path ignores bg_top entirely"
    );
}

// ==================== the canvas follows the appearance (design system step 7) ====================

/// Every OPAQUE vertex colour of the scenario's last frame, tessellated — what a renderer fills.
/// Opaque only: the faded grid and axis variants are premultiplied, one per alpha, and every
/// colour these tests look for is an opaque token.
fn opaque_colours(case: &Case<'_>) -> std::collections::BTreeSet<[u8; 4]> {
    let (_ppp, prims) = case.run_tessellated();
    let mut out = std::collections::BTreeSet::new();
    for p in &prims {
        if let egui::epaint::Primitive::Mesh(m) = &p.primitive {
            out.extend(
                m.vertices.iter().filter(|v| v.color.a() == 255).map(|v| v.color.to_array()),
            );
        }
    }
    out
}

/// The neutrals follow the installed theme: Midnight's captions and border are painted, and
/// Graphite's are not.
#[test]
fn the_canvas_neutrals_follow_the_installed_theme() {
    let st = make_state(wave_bars(60));
    let mut case = Case::new(&st);
    case.sub_panes = &[PaneKey::Volume];
    case.appearance = Some(Appearance { theme: ThemeId::Midnight, ..Appearance::default() });
    let got = opaque_colours(&case);
    let (m, g) = (Theme::of(ThemeId::Midnight), Theme::of(ThemeId::Graphite));
    for (what, c) in
        [("axis labels", m.text3), ("pane divider", m.border), ("legend letters", m.text)]
    {
        assert!(got.contains(&c.to_array()), "Midnight's {what} {c:?} is not on the canvas");
    }
    for (what, c) in [("captions", g.text3), ("border", g.border), ("text", g.text)] {
        assert!(
            !got.contains(&c.to_array()),
            "Graphite's {what} {c:?} survived on a Midnight canvas"
        );
    }
}

/// Text on a filled chip — the last-price chip, the volume tag — is the kit's black. No text is
/// painted in the background colour any more (the chip's old text).
#[test]
fn every_chip_on_the_canvas_writes_on_fill_text() {
    let st = make_state(wave_bars(40));
    let mut case = Case::new(&st);
    case.sub_panes = &[PaneKey::Volume];
    let (_acts, out) = case.run_frames();
    let texts = text_sections(&out);
    let on_fill =
        texts.iter().filter(|(_, _, _, c)| *c == vike_ui_theme::components::ON_FILL).count();
    assert!(on_fill >= 2, "the last-price chip and the volume tag: {on_fill} black sections");
    let bg = Theme::of(ThemeId::Graphite).bg;
    assert!(
        !texts.iter().any(|(_, _, _, c)| *c == bg),
        "a chip still writes in the background colour"
    );
}

/// Review Focus 2: under Midnight and Colour-blind, the canvas paints their tokens, and no
/// Graphite-only, Classic-only or old-candle value survives anywhere on it.
#[test]
fn the_canvas_follows_the_installed_theme_and_market_colours() {
    let st = make_state(wave_bars(60));
    let mut case = Case::new(&st);
    case.sub_panes = &[PaneKey::Volume];
    case.appearance = Some(Appearance {
        theme: ThemeId::Midnight,
        market: MarketId::ColourBlind,
        ..Appearance::default()
    });
    let got = opaque_colours(&case);
    let (t, m) = (Theme::of(ThemeId::Midnight), MarketColors::of(MarketId::ColourBlind));
    for (what, c) in [
        ("up candle", m.up),
        ("down candle", m.down),
        ("gradient top", t.grad_top),
        ("gradient bottom", t.bg),
        ("axis labels", t.text3),
        ("pane divider", t.border),
    ] {
        assert!(got.contains(&c.to_array()), "{what} {c:?} is not on the canvas");
    }
    let (g, cl) = (Theme::of(ThemeId::Graphite), MarketColors::of(MarketId::Classic));
    for (what, c) in [
        ("Graphite background", g.bg),
        ("Graphite gradient top", g.grad_top),
        ("Graphite captions", g.text3),
        ("Classic up", cl.up),
        ("Classic down", cl.down),
        ("the old candle mint", egui::Color32::from_rgb(91, 190, 145)),
        ("the old candle rose", egui::Color32::from_rgb(217, 84, 88)),
    ] {
        assert!(
            !got.contains(&c.to_array()),
            "{what} {c:?} survived under Midnight + Colour-blind"
        );
    }
}

/// Review Focus 3: an edited colour survives a theme and market change; the unedited ones follow.
#[test]
fn an_edited_colour_survives_a_theme_and_market_change() {
    let st = make_state(wave_bars(60));
    let mut case = Case::new(&st);
    case.options.show_volume = false;
    case.options.up = Some([1, 2, 3]);
    case.options.bg_gradient = false;
    case.options.bg = Some([4, 5, 6]);
    case.appearance = Some(Appearance {
        theme: ThemeId::Dusk,
        market: MarketId::TradingView,
        ..Appearance::default()
    });
    let got = opaque_colours(&case);
    assert!(got.contains(&[1, 2, 3, 255]), "the edited up candle lost its colour");
    assert!(got.contains(&[4, 5, 6, 255]), "the edited background lost its colour");
    let tv = MarketColors::of(MarketId::TradingView);
    assert!(got.contains(&tv.down.to_array()), "the unedited down candle did not follow the set");
}

/// Review Focus 4: a Settings change reaches an open chart on the very next frame.
#[test]
fn a_live_appearance_change_reaches_an_open_chart_on_the_next_frame() {
    let st = make_state(wave_bars(40));
    let mut case = Case::new(&st);
    case.options.show_volume = false;
    case.frames = 3;
    case.switch_before_last =
        Some(Appearance { market: MarketId::ColourBlind, ..Appearance::default() });
    let got = opaque_colours(&case);
    let (cb, cl) = (MarketColors::of(MarketId::ColourBlind), MarketColors::of(MarketId::Classic));
    assert!(got.contains(&cb.up.to_array()) && got.contains(&cb.down.to_array()));
    assert!(!got.contains(&cl.up.to_array()), "the frame after the change still painted Classic");
}

/// Review Focus 5: the OHLC legend writes direction in the set's TEXT colour, never its graphic
/// one — TradingView's graphic down is below the text floor on a card.
#[test]
fn the_ohlc_legend_writes_direction_in_the_market_text_colour() {
    let st = make_state(wave_bars(40));
    let mut case = Case::new(&st);
    case.options.show_volume = false;
    case.appearance = Some(Appearance { market: MarketId::TradingView, ..Appearance::default() });
    let (_acts, out) = case.run_frames();
    let colours: Vec<egui::Color32> =
        text_sections(&out).into_iter().map(|(_, _, _, c)| c).collect();
    let tv = MarketColors::of(MarketId::TradingView);
    assert!(colours.contains(&tv.up_text) || colours.contains(&tv.down_text), "{colours:?}");
    assert!(
        !colours.contains(&tv.up) && !colours.contains(&tv.down),
        "a graphic colour wrote text"
    );
}

/// Final-review finding (Important #1): `draw_footprint`'s per-cell buy/sell volume numbers must
/// use the market set's TEXT colour, the same rule Review Focus 5 pins for the OHLC legend — never
/// the GRAPHIC pair (`up_s`/`down_s`), which also colours the cell's background fill and, on
/// TradingView, sits below the text contrast floor. Before the fix these numbers resolved to
/// `tv.up`/`tv.down` (the graphic pair, read straight off `ChartColors` with no text/graphic split).
#[test]
fn the_footprint_cells_write_volume_in_the_market_text_colour() {
    let bars = flat_bars(10);
    let footprint: Vec<vike_orderflow::FootprintBar> = (0..bars.len())
        .map(|i| vike_orderflow::FootprintBar {
            bar_index: i as u64,
            cells: vec![vike_orderflow::PriceBin { price: 1.0, buy_vol: 7.0, sell_vol: 3.0 }],
        })
        .collect();
    let st = make_state(bars);
    let mut case = Case::new(&st);
    case.style = ChartStyle::Footprint;
    case.options.show_volume = false;
    case.footprint = Some(&footprint);
    // Pinned well above the ~11px legibility floor (`orderflow::cell_text_legible`) regardless of
    // the price pane's exact pixel height — this fixture's whole visible range is only 1.5 (h=2.0,
    // l=0.5), so a 1.0 tick is already 2/3 of it.
    case.of_tick_size = 1.0;
    case.appearance = Some(Appearance { market: MarketId::TradingView, ..Appearance::default() });
    let (_acts, out) = case.run_frames();
    let colours: Vec<egui::Color32> =
        text_sections(&out).into_iter().map(|(_, _, _, c)| c).collect();
    let tv = MarketColors::of(MarketId::TradingView);
    assert!(colours.contains(&tv.up_text) || colours.contains(&tv.down_text), "{colours:?}");
    assert!(
        !colours.contains(&tv.up) && !colours.contains(&tv.down),
        "a graphic colour wrote footprint cell text"
    );
}

/// Spec §3.5: the chart's own text follows the text-size setting through the roles — every mono
/// text is the Caption role except the OHLC legend (Title), and the pane name is Strong semibold.
#[test]
fn the_chart_text_follows_the_type_roles_at_both_sizes() {
    for size in TextSize::ALL {
        let st = make_state(wave_bars(40));
        let mut case = Case::new(&st);
        case.sub_panes = &[PaneKey::Volume];
        case.appearance = Some(Appearance { text_size: size, ..Appearance::default() });
        let (_acts, out) = case.run_frames();
        let texts = text_sections(&out);
        // The OHLC legend is ONE galley: letters and numbers in several sections. Its first
        // section is `O `; every section of that galley is the Title role.
        let legend = texts
            .iter()
            .find(|(_, s, _, _)| s.starts_with("O "))
            .map(|(g, _, _, _)| *g)
            .expect("the OHLC legend: its first section is `O `");
        let mono: Vec<&Section> =
            texts.iter().filter(|(_, _, f, _)| f.family == egui::FontFamily::Monospace).collect();
        assert!(
            mono.len() >= 5,
            "{size:?}: only {} mono sections — the scan saw nothing",
            mono.len()
        );
        for (g, s, f, _) in mono {
            let role = if *g == legend { TextRole::Title } else { TextRole::Caption };
            assert_eq!(f.size, size.px(role), "{size:?}: {s:?} is {f:?}, not {role:?}");
        }
        let semibold = egui::FontFamily::Name(vike_ui_theme::fonts::SEMIBOLD.into());
        let pane = texts.iter().find(|(_, s, _, _)| s == "Volume").expect("the volume pane's name");
        assert_eq!(pane.2, egui::FontId::new(size.px(TextRole::Strong), semibold), "{size:?}");
    }
}
