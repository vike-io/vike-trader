//! The kill proofs for [`vike_ui_theme::frame_sanity::assert_frame_sane`] — the shared
//! headless-frame geometry invariant this crate's three render harnesses
//! (`draw_characterization.rs`, `options_chain_characterization.rs`, `study_pane_render.rs`) and
//! `vike-data-manager`'s `view.rs` test module all call.
//!
//! WHY IT LIVES IN A CONSUMER CRATE rather than beside the helper: the helper is compiled only
//! behind `vike-ui-theme`'s `test-support` feature, and `vike-ui-theme` has no CI feature lane of
//! its own. A `#[cfg(test)]` module inside that crate would compile only under a
//! `cargo test -p vike-ui-theme --features test-support` invocation that nothing runs — the
//! definition of a gate that is decoration. `vike-chart`'s dev-dependency turns the feature on,
//! and `vike-chart` is in the DERIVED CI roster (`xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`), so these proofs run in
//! the ordinary `just test` / CI fast lane with no new lane bought.
//!
//! WHY THEY EXIST AT ALL: this repo's standard is that a gate which has not been shown to fail on
//! the defect it exists for is decoration. Every invariant `assert_frame_sane` ships is provoked
//! here, synthetically, so the proof ships with the gate and keeps working — rather than living in
//! a reviewer's memory of a planted edit that was reverted.
//!
//! A synthetic `FullOutput` proves the FUNCTION reddens; it cannot prove the harnesses REACH it.
//! That half was measured once, by hand, with 18 planted lines across four real render entry
//! points — `chart::draw` (NaN coordinate), `options_chain::draw` (infinite coordinate),
//! `render::render_study` (NaN coordinate), `view::bulk_action_bar` (absurd-but-finite
//! coordinate) and `view::stored_catalog_grid` (NaN clip rect via `Ui::set_clip_rect`). Result:
//! **32 of 312 tests failed, and the 32 were exactly the 32 that render a frame** — every test in
//! `draw_characterization`, `options_chain_characterization`, `study_pane_render` and the seven
//! frame tests in `vike-data-manager`'s `view::tests`. Nothing else moved.
//!
//! The last three tests are the mirror image: they pin the three invariants that were proposed,
//! MEASURED against all 32 existing frames, and dropped for firing on healthy code. Each asserts
//! the helper ACCEPTS the frame shape that would have tripped the dropped rule, so re-adding one
//! reddens here instead of reddening scattered render tests whose authors have no idea why.
//! `vike_ui_theme::frame_sanity`'s module docs carry the measurements.

use egui::epaint::{ClippedShape, Shape};
use vike_ui_theme::frame_sanity::{ABSURD_COORD, assert_frame_sane, frame_report};

const SCREEN: egui::Rect =
    egui::Rect { min: egui::Pos2 { x: 0.0, y: 0.0 }, max: egui::Pos2 { x: 960.0, y: 620.0 } };

/// A `FullOutput` carrying exactly the shapes handed in — the synthetic frame every proof below
/// plants its defect into.
fn frame(shapes: Vec<ClippedShape>) -> egui::FullOutput {
    egui::FullOutput { shapes, pixels_per_point: 1.0, ..Default::default() }
}

/// One healthy rect, so a "this frame is fine" baseline is never an EMPTY frame — an empty frame
/// passes every invariant trivially and would prove nothing about the walk reaching leaves.
fn healthy_rect() -> ClippedShape {
    ClippedShape {
        clip_rect: SCREEN,
        shape: Shape::rect_filled(
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 40.0)),
            0.0,
            egui::Color32::RED,
        ),
    }
}

fn clipped(shape: Shape) -> ClippedShape {
    ClippedShape { clip_rect: SCREEN, shape }
}

/// A REAL laid-out galley, so the text proofs below carry the same `TextShape` a render emits
/// rather than a hand-built stand-in. `Context::fonts` hands out a `&FontsView` and laying out
/// needs `&mut`, so it goes through `Painter::layout_no_wrap` inside a pass — which is also the
/// only place egui has fonts at all ("No fonts available until first call to Context::run()").
fn galley(ctx: &egui::Context, text: &str) -> std::sync::Arc<egui::Galley> {
    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(0.0), ..Default::default() };
    let mut g = None;
    let mut out = ctx.run_ui(raw, |ui| {
        g = Some(ui.painter().layout_no_wrap(
            text.to_owned(),
            egui::FontId::default(),
            egui::Color32::WHITE,
        ));
    });
    out.textures_delta.clear();
    g.expect("the pass runs its closure exactly once")
}

// ============================ the baseline ============================

#[test]
fn a_healthy_frame_passes_and_the_walk_actually_reaches_its_leaves() {
    // Guards the failure mode that makes every other test in this file meaningless: a walk that
    // silently visits nothing would pass all of them. The leaf count is asserted, and the nested
    // `Shape::Vec` proves the recursion happens rather than the group being counted as one.
    let out = frame(vec![
        healthy_rect(),
        clipped(Shape::Vec(vec![
            Shape::circle_filled(egui::pos2(5.0, 5.0), 2.0, egui::Color32::BLUE),
            Shape::Vec(vec![Shape::line_segment(
                [egui::pos2(0.0, 0.0), egui::pos2(9.0, 9.0)],
                (1.0, egui::Color32::WHITE),
            )]),
        ])),
    ]);
    let report = frame_report(&out);
    assert!(report.is_sane(), "a healthy frame must pass: {report}");
    assert_eq!(report.clipped_shapes, 2);
    assert_eq!(report.leaves, 3, "the Vec groups must be flattened, not counted as one leaf");
    assert_frame_sane(&out);
}

// ============================ invariant 1: no NaN ============================

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn a_nan_shape_coordinate_reddens() {
    let mut bad = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(30.0, 30.0));
    bad.min.x = f32::NAN;
    assert_frame_sane(&frame(vec![
        healthy_rect(),
        clipped(Shape::rect_filled(bad, 0.0, egui::Color32::RED)),
    ]));
}

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn a_nan_inside_a_nested_vec_group_reddens() {
    // The recursion is load-bearing: `chart::draw` emits most of its geometry inside `Shape::Vec`
    // groups (`egui_plot` batches a series that way), so a walk that stopped at the top level
    // would be blind to nearly every coordinate this repo actually paints.
    assert_frame_sane(&frame(vec![clipped(Shape::Vec(vec![Shape::Vec(vec![
        Shape::circle_filled(egui::pos2(f32::NAN, 5.0), 2.0, egui::Color32::BLUE),
    ])]))]));
}

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn a_nan_mesh_vertex_reddens() {
    // The gradient background is a `Shape::Mesh`, and `gradient_background_emits_mesh_with_both_stops`
    // reads its vertex COLOURS — nothing checked their positions until this gate.
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(egui::pos2(0.0, 0.0), egui::Color32::RED);
    mesh.colored_vertex(egui::pos2(f32::NAN, 10.0), egui::Color32::RED);
    mesh.colored_vertex(egui::pos2(10.0, 10.0), egui::Color32::RED);
    mesh.add_triangle(0, 1, 2);
    assert_frame_sane(&frame(vec![clipped(Shape::Mesh(mesh.into()))]));
}

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn a_nan_clip_rect_reddens() {
    let mut clip = SCREEN;
    clip.max.y = f32::NAN;
    assert_frame_sane(&frame(vec![ClippedShape { clip_rect: clip, shape: healthy_rect().shape }]));
}

// ============================ invariant 2: no infinite / absurd shape coord ============

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn an_infinite_shape_coordinate_reddens() {
    assert_frame_sane(&frame(vec![clipped(Shape::line_segment(
        [egui::pos2(0.0, 0.0), egui::pos2(f32::INFINITY, 10.0)],
        (1.0, egui::Color32::WHITE),
    ))]));
}

#[test]
#[should_panic(expected = "frame geometry is not paintable")]
fn an_absurd_but_finite_shape_coordinate_reddens() {
    // The class `is_finite()` alone would wave through: arithmetic that has already blown up but
    // has not yet reached infinity. Tessellating this overflows to infinity anyway.
    assert_frame_sane(&frame(vec![clipped(Shape::circle_filled(
        egui::pos2(ABSURD_COORD * 10.0, 0.0),
        2.0,
        egui::Color32::BLUE,
    ))]));
}

// ============================ the three DROPPED proposals ============================

#[test]
fn a_far_offscreen_shape_is_accepted() {
    // The proposal was "nothing is painted entirely outside `screen`". `case2_pointer_outside_plot_no_hover`
    // parks the pointer at (5000, 5000) on a 960×620 screen ON PURPOSE, and 150 of that frame's
    // 332 shapes land entirely off-screen as a result. A text shape at (-5000, 0) is the same
    // legitimate case, so the helper must accept it — and, being finite, it must also NOT be
    // caught by the absurd-coordinate rule, which is what separates "off-screen" from "broken".
    let ctx = egui::Context::default();
    let text = galley(&ctx, "offscreen");
    let out = frame(vec![
        healthy_rect(),
        clipped(Shape::galley(egui::pos2(-5000.0, 0.0), text, egui::Color32::WHITE)),
    ]);
    assert!(frame_report(&out).is_sane(), "an off-screen shape is not a defect");
    assert_frame_sane(&out);
}

#[test]
fn text_hanging_outside_its_own_clip_rect_is_accepted() {
    // The proposal was "every `Shape::Text` galley's rect lies within its own clip rect". Measured
    // on passing tests, egui centres a label on its anchor and lets the clip trim the sub-pixel
    // overhang: `"01 Aug"` spans x ∈ [920.0, 960.4] under a clip ending at 960.0, `elided == false`.
    let ctx = egui::Context::default();
    let text = galley(&ctx, "01 Aug");
    let out = frame(vec![ClippedShape {
        // A clip rect far narrower than the text it holds — the extreme of the same shape.
        clip_rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(4.0, 620.0)),
        shape: Shape::galley(egui::pos2(0.0, 0.0), text, egui::Color32::WHITE),
    }]);
    assert!(frame_report(&out).is_sane(), "clipped text is the mechanism working, not failing");
    assert_frame_sane(&out);
}

#[test]
fn a_zero_area_interactive_rect_is_unreachable_not_merely_tolerated() {
    // The proposal was "no interactive/allocated rect of zero area". It is VACUOUS: the only
    // public route to widget rects, `Context::interactive_rects_last_pass`, filters
    // `rect.is_positive() && rect.is_finite()` itself, so the count is zero by construction. This
    // test pins that property of the egui version we are pinned to — if a future egui stops
    // filtering, the proposal becomes live again and this test says so by failing.
    let ctx = egui::Context::default();
    for f in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(SCREEN),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            // egui's Id-reservation idiom: a zero-size allocation that still senses clicks.
            let _ = ui.allocate_response(egui::Vec2::ZERO, egui::Sense::click());
            let _ = ui.button("real");
            egui::CollapsingHeader::new("empty").show(ui, |_ui| {});
            egui::ScrollArea::vertical().max_height(20.0).show(ui, |ui| {
                for i in 0..40 {
                    let _ = ui.button(format!("row {i}"));
                }
            });
        });
        out.textures_delta.clear(); // before the assertion — see `assert_frame_sane`'s own doc
        assert_frame_sane(&out);
    }
    let rects = ctx.interactive_rects_last_pass();
    assert!(!rects.is_empty(), "the probe frame must register interactive widgets at all");
    assert!(
        rects.iter().all(|r| r.width() > 0.0 && r.height() > 0.0),
        "egui filters non-positive interact rects itself: {rects:?}"
    );
}
