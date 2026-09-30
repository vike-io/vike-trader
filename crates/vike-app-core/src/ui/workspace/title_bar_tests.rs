use super::*;

fn bar(width: f32) -> Rect {
    Rect::from_min_size(Pos2::new(100.0, 50.0), egui::vec2(width, BAR_H))
}

/// ⚠ **The chips' BOTTOM is the bar's bottom**, at every width and whatever the title did.
/// That is the whole continuity effect: the selected chip's fourth edge is the gap it makes in
/// the hairline at [`TitleTabSlot::hairline_y`], which is that same edge. Reddens on a slot
/// centred in the bar, or one seated on the bar's TOP — both of which look plausible and both
/// of which put the chip's bottom edge somewhere the hairline is not.
#[test]
fn the_tab_slot_is_seated_on_the_bars_bottom_edge() {
    for w in [400.0_f32, 560.0, 1200.0] {
        let b = bar(w);
        let slot = tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0);
        assert_eq!(slot.bottom(), b.bottom(), "width {w}: the chips sit ON the hairline");
        assert!(slot.top() >= b.top(), "width {w}: …and inside the bar: {slot:?}");
    }
    let b = bar(1200.0);
    let slot = TitleTabSlot {
        bar: b,
        tabs: tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0),
        pad: PAD_WIDE,
        carries_tabs: true,
    };
    // The hairline is the chips' own bottom edge, on the pixel grid.
    assert!((slot.hairline_y() - (b.bottom().round() + 0.5)).abs() < f32::EPSILON);
    assert!((slot.hairline_y() - slot.tabs.bottom()).abs() <= 1.0, "{slot:?}");
}

/// ⚠ **The chips lose to the window controls, never the other way round.** A bar too narrow
/// for both must not push `✕` off its own title bar — a window that cannot be closed is a
/// worse failure than a clipped tab. Reddens on an unclamped `from_min_max`, which yields an
/// INVERTED rect that egui then paints backwards.
#[test]
fn a_bar_too_narrow_for_both_clamps_rather_than_inverting() {
    let b = bar(120.0);
    // The controls take 96pt of a 120pt bar; the title has already eaten what is left.
    let slot = tab_slot_rect(b, b.left() + 80.0, b.right() - 96.0);
    assert!(slot.width() >= 0.0, "never inverted: {slot:?}");
    assert!(slot.right() <= b.right() - 96.0 + 0.001, "the controls keep their room: {slot:?}");
}

/// **`egui-wgpu-0.36.1/src/renderer.rs`'s `ScissorRect::new`, restated as the physical ROW
/// RANGE a render pass will actually touch.** Rounded per edge, `[min, max)` — the exclusive
/// upper bound is the whole of finding 1.
fn scissor_rows(clip: Rect, ppp: f32) -> std::ops::Range<i64> {
    let min = (ppp * clip.min.y).round() as i64;
    let max = ((ppp * clip.max.y).round() as i64).max(min);
    min..max
}

/// The physical row a 1pt stroke centred on `y` lands in — its centre's row, which is the row
/// that has to survive for ANY of the stroke to be seen.
fn stroke_row(y: f32, ppp: f32) -> i64 {
    (ppp * y).floor() as i64
}

/// ⚠⚠ **THE HAIRLINE IS THE WHOLE DESIGN, AND CLIPPED TO THE BAR IT RENDERS NOTHING.**
///
/// The selected chip is filled with `palette::BG` — the same ground the title bar stands on —
/// and draws only its left, top and right edges. Its missing FOURTH edge is the gap it makes
/// in this line, and that gap is the only thing marking the selection. So a hairline that is
/// scissored away does not cost a divider: it costs the selection marker, and the frame reads
/// as two inert words.
///
/// This is arithmetic rather than a picture because it has to be: no CI runner has a GPU, and
/// a clipped line reaches no accessibility tree. The model is `ScissorRect::new`'s own
/// rounding ([`scissor_rows`]), driven over fractional bar bottoms (a window sits wherever the
/// user dragged it) and over the `pixels_per_point` values a real display hands egui.
///
/// Both directions are asserted on purpose. The witness half is what stops the survival half
/// from passing against a clip that never needed widening.
///
/// ⚠ **The witness is stated at two strengths, because only one of them is universal and the
/// first draft of this test claimed the stronger one everywhere and went red.** At
/// `pixels_per_point == 1` — the ordinary case, and the one the reviewers argued — the bar's
/// clip excludes the stroke's row OUTRIGHT, at every bar bottom: the bound is `round(bottom)`
/// and the row is `floor(round(bottom) + 0.5)`, which is the same integer, and the range is
/// half-open. At fractional scales the rounding sometimes admits that row by luck (bottom
/// `80.4` at `1.25x`: bound `round(100.5) = 101`, row `100`), so "nothing renders" is not true
/// there — what IS true at every scale is that the bar's bound falls strictly INSIDE the
/// stroke, so part of the line is always cut. The defect is therefore "invisible at 1x and
/// unreliable everywhere else", which is worse than a clean always-broken, not better: it is
/// the shape that survives a glance at one machine.
#[test]
fn the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip() {
    for offset in [0.0_f32, 0.1, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9] {
        let b = Rect::from_min_size(Pos2::new(100.0, 50.0 + offset), egui::vec2(800.0, BAR_H));
        let slot = TitleTabSlot {
            bar: b,
            tabs: tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0),
            pad: PAD_WIDE,
            carries_tabs: true,
        };
        // The stroke's own bottom edge, in points — where the pass has to reach for the whole
        // 1pt line to be drawn.
        let stroke_bottom = slot.hairline_y() + 0.5;
        for ppp in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
            let row = stroke_row(slot.hairline_y(), ppp);
            // The UNIVERSAL witness: the bar's scissor bound cuts into the stroke at every
            // scale, so the hairline is never drawn whole under it.
            assert!(
                (scissor_rows(slot.bar, ppp).end as f32) < ppp * stroke_bottom,
                "bar bottom {} @ {ppp}x: clipping to the BAR must CUT the stroke — its bound \
                     {:?} has to fall inside the line's own extent (…{}). An assertion that \
                     stopped failing here would mean the survival check below proves nothing",
                slot.bar.bottom(),
                scissor_rows(slot.bar, ppp),
                ppp * stroke_bottom,
            );
            // …and at 1x it takes the whole thing: the centre row is outside the range.
            if ppp == 1.0 {
                assert!(
                    !scissor_rows(slot.bar, ppp).contains(&row),
                    "bar bottom {} @ 1x: clipping to the BAR scissors row {row} away entirely \
                         (rows {:?}) — nothing of the hairline reaches a pixel, which is the \
                         shipped bug",
                    slot.bar.bottom(),
                    scissor_rows(slot.bar, ppp),
                );
            }
            assert!(
                scissor_rows(slot.hairline_clip(), ppp).contains(&row),
                "bar bottom {} @ {ppp}x: the hairline's own clip must RENDER row {row} \
                     (rows {:?}, clip {:?})",
                slot.bar.bottom(),
                scissor_rows(slot.hairline_clip(), ppp),
                slot.hairline_clip(),
            );
        }
        // …and the widening is exactly one stroke, not a clip that spills into the panel: it
        // reaches the line's bottom edge and stops.
        assert!(
            (slot.hairline_clip().max.y - (slot.hairline_y() + 0.5)).abs() < f32::EPSILON,
            "{slot:?}"
        );
        assert_eq!(slot.hairline_clip().min, slot.bar.min, "the bar's own top-left: {slot:?}");
        assert!(
            slot.hairline_clip().max.y - slot.bar.max.y <= 1.5,
            "at most the stroke's own row below the bar: {slot:?}"
        );
    }
}

/// The responsive rule: a tabbed window drops its TITLE below the breakpoint and tightens its
/// chips; a window with no tabs keeps its title at every width, because nothing is competing
/// for the row.
#[test]
fn the_title_drops_only_for_a_tabbed_kind_and_only_below_the_breakpoint() {
    assert_eq!(title_bar_plan(WinKind::Connections, TITLE_DROP_W), (true, PAD_WIDE));
    assert_eq!(title_bar_plan(WinKind::Connections, TITLE_DROP_W - 1.0), (false, PAD_TIGHT));
    assert_eq!(title_bar_plan(WinKind::Connections, 400.0), (false, PAD_TIGHT));
    // …and a kind that seats no tabs is unaffected at the same widths.
    assert!(title_bar_plan(WinKind::News, 400.0).0);
    assert!(title_bar_plan(WinKind::Chart, 120.0).0);
}

/// The header gradient reaches every tool window's title bar: one mesh inside the bar when it is
/// on, nothing when it is off — today's look, the window fill showing through.
#[test]
fn the_title_bar_paints_the_header_gradient_only_when_it_is_on() {
    use vike_ui_theme::appearance::{Appearance, install};
    let meshes = |on: bool| {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance { header_gradient: on, ..Appearance::default() });
        let mut bar = Rect::NOTHING;
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let (_, _, slot) = tool_title_bar(ui, WinKind::News, false);
            bar = slot.bar;
        });
        let inside = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Mesh(m) => Some(m.calc_bounds()),
                _ => None,
            })
            .filter(|b| bar.expand(1.0).contains_rect(*b))
            .count();
        out.drop_without_applying_deltas();
        inside
    };
    assert_eq!(meshes(false), 0);
    assert_eq!(meshes(true), 1);
}

/// The window's name and its three controls are the Title role of the text size in force — a
/// chart window's title role too — and the name is the theme's UI text, on every theme.
#[test]
fn the_title_and_controls_are_the_title_role_in_the_themes_ui_text() {
    use vike_ui_theme::appearance::{Appearance, install};
    use vike_ui_theme::theme::{Theme, ThemeId};
    use vike_ui_theme::type_scale::{TextRole, TextSize};
    for theme in ThemeId::ALL {
        for size in TextSize::ALL {
            let ctx = egui::Context::default();
            install(&ctx, &Appearance { theme, text_size: size, ..Appearance::default() });
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                tool_title_bar(ui, WinKind::News, false);
            });
            let texts: Vec<(String, f32, egui::Color32)> = std::mem::take(&mut out.shapes)
                .into_iter()
                .filter_map(|c| match c.shape {
                    egui::Shape::Text(t) => {
                        let f = t.galley.job.sections[0].format.clone();
                        Some((t.galley.text().to_string(), f.font_id.size, f.color))
                    }
                    _ => None,
                })
                .collect();
            out.drop_without_applying_deltas();
            let want = size.px(TextRole::Title);
            assert!(texts.iter().all(|(_, px, _)| *px == want), "{theme:?} {size:?}: {texts:?}");
            let name =
                texts.iter().find(|(s, _, _)| s == WinKind::News.label()).expect("the title");
            assert_eq!(name.2, Theme::of(theme).text_ui, "{theme:?}");
        }
    }
}
