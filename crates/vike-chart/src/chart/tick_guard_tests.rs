use super::*;
use egui::{Pos2, RawInput, vec2};

/// Run `f` with a [`Probe`] from a real (headless) frame: the metrics are the bundled font's, so
/// nothing below hard-codes a glyph size.
fn with_probe(f: impl FnOnce(&Probe)) {
    let ctx = egui::Context::default();
    crate::chart::harness::bind_chart_font_families(&ctx);
    let ts = vike_ui_theme::appearance::Appearance::default().text_size;
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
        ..Default::default()
    };
    let mut f = Some(f);
    let mut out = ctx.run_ui(raw, |ui| {
        if let Some(f) = f.take() {
            f(&Probe::new(ui, ts));
        }
    });
    // egui 0.36 panics on dropping unapplied texture deltas; nothing here is rendered.
    out.textures_delta.clear();
}

/// A y axis over `0..=20` at 10 pixels per unit, i.e. a 200pt pane.
fn pane(probe: &Probe) -> (TickGuard, RangeInclusive<f64>) {
    let guard = TickGuard::y(probe);
    // egui_plot hands the spacer `value-per-pixel x grid_spacing().min`: 0.1 x 8 here.
    guard.note_spacing(&GridInput { bounds: (0.0, 20.0), base_step_size: 0.8 });
    (guard, 0.0..=20.0)
}

#[test]
fn a_tick_label_whose_glyphs_stick_out_of_its_pane_is_not_drawn() {
    with_probe(|probe| {
        let (guard, range) = pane(probe);
        let (size, glyphs) = probe.lay("0");
        let (top, bottom) = (glyphs.min.y - size.y / 2.0, glyphs.max.y - size.y / 2.0);
        assert!(
            top < -1.0 && bottom > 1.0,
            "a digit spans both sides of its mark: {top}..{bottom}"
        );
        // Volume's 0 sits on the bottom edge, its glyphs half below the pane; the top mark likewise.
        assert!(guard.hides(0.0, &range, "0"));
        assert!(guard.hides(20.0, &range, "20"));
        // A label wholly inside stays...
        assert!(!guard.hides(10.0, &range, "10"));
        // ...and the glyph extent, not the taller layout row, decides: put the glyphs' bottom
        // `over` points past the bottom edge (mark at 200 + over - bottom pixels down).
        let at_bottom = |over: f32| 20.0 - f64::from(200.0 + over - bottom) / 10.0;
        assert!(!guard.hides(at_bottom(0.4), &range, "0"), "within the half-point slack");
        assert!(guard.hides(at_bottom(0.7), &range, "0"));
        // Likewise at the top: glyph tops `over` points above the top edge.
        let at_top = |over: f32| 20.0 - f64::from(-over - top) / 10.0;
        assert!(!guard.hides(at_top(0.4), &range, "0"));
        assert!(guard.hides(at_top(0.7), &range, "0"));
    });
}

#[test]
fn a_time_label_cut_by_the_left_edge_is_not_drawn() {
    with_probe(|probe| {
        let guard = TickGuard::x(probe, 1000.0);
        // 10 pixels per bar over bars 0..=100.
        guard.note_spacing(&GridInput { bounds: (0.0, 100.0), base_step_size: 0.8 });
        let range = 0.0..=100.0;
        let (size, glyphs) = probe.lay("15 Nov");
        let (left, right) = (glyphs.min.x - size.x / 2.0, glyphs.max.x - size.x / 2.0);
        assert!(
            left < -1.0 && right > 1.0,
            "a label spans both sides of its mark: {left}..{right}"
        );
        // The mark in `value` bars from the edge puts the glyphs' left side `glyph_left` pixels right of it.
        let at = |glyph_left: f32| f64::from(glyph_left - left) / 10.0;
        // Centred on the edge, the label is cut: not drawn. Starting on or inside it, or by no more
        // than the half-point slack outside, it stays.
        assert!(guard.hides(0.0, &range, "15 Nov"));
        assert!(!guard.hides(at(0.0), &range, "15 Nov"));
        assert!(!guard.hides(at(-0.4), &range, "15 Nov"));
        assert!(guard.hides(at(-0.7), &range, "15 Nov"));
        // One wholly past the edge is clipped away unseen, and left alone.
        assert!(!guard.hides(at(-(right - left) - 5.0), &range, "15 Nov"));
        // The right edge is not a cut: the gutter is there.
        assert!(!guard.hides(100.0, &range, "15 Nov"));
    });
}

#[test]
fn the_tick_label_under_a_chip_is_not_drawn_but_its_neighbours_are() {
    with_probe(|probe| {
        let (guard, range) = pane(probe);
        let chip = guard.y_chip(10.0, Fit::Inside);
        guard.set_chips([Some(chip), None]);
        // The label at the chip's own value is covered, and so is one 3pt off it.
        assert!(guard.hides(10.0, &range, "10"));
        assert!(guard.hides(10.3, &range, "10.3"));
        // A label above the chip whose mark is `delta` points over the chip's top edge: its glyphs
        // reach `far` below the mark, so they overlap the chip by `far - delta`, and the label is
        // drawn once that is no more than the half-point slack.
        let (size, glyphs) = probe.lay("11");
        let far = glyphs.max.y - size.y / 2.0;
        let above = |delta: f32| 10.0 + f64::from(chip.len / 2.0 + delta) / 10.0;
        assert!(guard.hides(above(far - 0.5 - 0.2), &range, "11"));
        assert!(!guard.hides(above(far - 0.5 + 0.2), &range, "11"));
        // Without the chip nothing hides.
        guard.set_chips([None, None]);
        assert!(!guard.hides(10.0, &range, "10"));
    });
}

#[test]
fn a_chip_is_where_its_painter_puts_it() {
    with_probe(|probe| {
        let (guard, range) = pane(probe);
        // Pinned: the last-price chip of a view whose close is ABOVE the pane (value 25 > 20) sits
        // flush against the top edge, so it covers the label at 19 (10pt down) and not the one at 15.
        guard.set_chips([Some(guard.y_chip(25.0, Fit::PinOutside)), None]);
        assert!(guard.hides(19.0, &range, "19"));
        assert!(!guard.hides(15.0, &range, "15"));
        // Hidden: a crosshair price tag in the top 26pt strip is not painted, so it covers nothing,
        // while one below the strip is painted and covers its label.
        guard.set_chips([None, Some(guard.y_chip(19.5, Fit::Hide { strip: 26.0 }))]);
        assert!(!guard.hides(19.0, &range, "19"));
        guard.set_chips([None, Some(guard.y_chip(15.0, Fit::Hide { strip: 26.0 }))]);
        assert!(guard.hides(15.0, &range, "15"));
        assert!(!guard.hides(10.0, &range, "10"));
    });
}

#[test]
fn the_time_tag_covers_the_time_labels_under_it_and_is_kept_inside_the_window() {
    with_probe(|probe| {
        // 10 pixels per bar over bars 0..=100 in a 1000pt-wide chart.
        let guard = TickGuard::x(probe, 1000.0);
        guard.note_spacing(&GridInput { bounds: (0.0, 100.0), base_step_size: 0.8 });
        let range = 0.0..=100.0;
        let tag = guard.time_chip(50.0, "11-15 00:00");
        guard.set_chips([Some(tag), None]);
        assert!(guard.hides(50.0, &range, "15 Nov"));
        assert!(guard.hides(52.0, &range, "15 Nov"));
        assert!(!guard.hides(50.0 + f64::from(tag.len) / 10.0, &range, "15 Nov"));
        // A pointer on the first bar: the tag is shifted right to stay in the window, so it covers
        // the labels from the edge to its far side, not the ones that sat under its unshifted half.
        guard.set_chips([Some(guard.time_chip(0.0, "11-15 00:00")), None]);
        assert!(guard.hides(f64::from(tag.len / 10.0 - 2.0), &range, "15 Nov"));
        assert!(!guard.hides(f64::from(tag.len / 10.0 + 6.0), &range, "15 Nov"));
    });
}

#[test]
fn a_guard_that_has_not_seen_the_spacer_or_has_no_text_hides_nothing() {
    with_probe(|probe| {
        let guard = TickGuard::y(probe);
        assert!(!guard.hides(0.0, &(0.0..=20.0), "0"));
        let (guard, range) = pane(probe);
        assert!(!guard.hides(0.0, &range, ""));
        // A degenerate spacer call (zero base step) leaves the scale unusable, not infinite.
        guard.note_spacing(&GridInput { bounds: (0.0, 0.0), base_step_size: 0.0 });
        assert!(!guard.hides(0.0, &range, "0"));
    });
}
