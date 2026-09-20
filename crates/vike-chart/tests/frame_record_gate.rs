//! The kill proofs for [`vike_ui_theme::frame_record`] — and, in the same breath, the argument for
//! why that module exists at all.
//!
//! Every test below plants ONE defect into an otherwise identical frame, and asserts two things:
//!
//! 1. [`vike_ui_theme::frame_sanity::assert_frame_sane`] — the rung below — stays GREEN. It is
//!    called on both frames and never fires. That is not incidental; it is the whole point. A
//!    z-order inversion, a collapsed pane, a swapped colour token and a narrowed clip rect all emit
//!    perfectly finite, perfectly sane coordinates, so the existing invariant is structurally blind
//!    to all four.
//! 2. The frame RECORD changes, in the specific FIELD that names the defect — not merely
//!    "something differs". A gate proven only by `assert_ne!` has been proven to notice noise.
//!
//! WHY THEY LIVE IN A CONSUMER CRATE rather than beside the recorder: identical to
//! `frame_sanity_gate.rs`'s reason. `vike-ui-theme`'s `test-support` feature has no CI lane of its
//! own, so a `#[cfg(test)]` module inside that crate would compile only under an invocation nothing
//! runs. `vike-chart` turns the feature on as a dev-dependency and IS in the derived CI roster, so
//! these proofs ride the ordinary fast lane and buy no new lane.
//!
//! The four defect classes here were also planted once, by hand, into the REAL `chart::draw` render
//! path and measured against the whole suite; the result is recorded in the branch's report. These
//! synthetic twins are what keeps the proof working after that plant was reverted, rather than
//! leaving it in a reviewer's memory.
//!
//! Defect 5 is the same discipline pointed one rung sideways: its check lives back in
//! `vike_ui_theme::frame_sanity` (the OPT-IN `clipped_text_shapes`, which `assert_frame_sane`
//! deliberately never calls — that module's docs argue why), not in the recorder — but its kill
//! proofs live HERE for the same consumer-crate/CI-lane reason as everything else in this file.

use std::path::PathBuf;

use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Rect, pos2, vec2};
use vike_ui_theme::frame_record::{
    MAX_COLOR_RUNS, NEAR_EDGE_EPS, RECORD_VERSION, assert_frame_golden, record_frame,
    rounding_margin,
};
use vike_ui_theme::frame_sanity::{
    CLIPPED_TEXT_TOL, assert_frame_sane, assert_no_clipped_text, clipped_text_shapes,
};

const SCREEN: Rect =
    Rect { min: egui::Pos2 { x: 0.0, y: 0.0 }, max: egui::Pos2 { x: 400.0, y: 300.0 } };

/// Two sentinel colours nothing else paints, so a run list is unambiguous about which shape is
/// which. Opaque, so premultiplied alpha cannot merge them.
const A: Color32 = Color32::from_rgb(0x11, 0x22, 0x33);
const B: Color32 = Color32::from_rgb(0xaa, 0xbb, 0xcc);

/// Paint one frame, ASSERT IT IS SANE, then record it. Every proof below goes through here, which
/// is what makes "stage 1 stayed green" a property of the test rather than a claim in a comment:
/// if `assert_frame_sane` ever fired on one of these frames the test would panic here instead of
/// reaching its assertion.
fn record(name: &str, mut paint: impl FnMut(&egui::Painter)) -> String {
    let ctx = egui::Context::default();
    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(0.0), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| paint(ui.painter()));
    // Before the assertion — egui 0.36 aborts the process if a `TexturesDelta` with unapplied
    // deltas is dropped while an assertion is unwinding (see `assert_frame_sane`'s own doc).
    out.textures_delta.clear();
    assert_frame_sane(&out);
    let ppp = out.pixels_per_point;
    record_frame(name, ppp, &ctx.tessellate(out.shapes, ppp))
}

fn rect_at(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

/// `Painter::rect_filled` returns a `ShapeIdx`; every paint closure below wants `()`. One helper
/// rather than a brace-and-semicolon on each of them.
fn fill(p: &egui::Painter, r: Rect, c: Color32) {
    p.rect_filled(r, 0.0, c);
}

/// One real widget frame at `size`, cleared and sanity-checked, its `FullOutput` handed back. The
/// defect-5 twins render PANELS, BUTTONS and a SCROLL AREA — widget code, where `record`'s
/// closures take a bare `Painter` — so they need a `Ui` pass of their own.
fn ui_frame(size: egui::Vec2, build: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
        time: Some(0.0),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, build);
    // Before the assertion — the same abort-in-destructor hazard `record` documents.
    out.textures_delta.clear();
    assert_frame_sane(&out);
    out
}

/// A `FullOutput` carrying exactly the shapes handed in — the synthetic frames the defect-5
/// tolerance and intersects proofs plant their text into. A deliberate mirror of
/// `frame_sanity_gate.rs`'s `frame` helper: integration-test binaries can only share code through
/// `tests/common`, which is the chart-draw harness and no home for a shape-list constructor.
fn shapes_frame(shapes: Vec<ClippedShape>) -> egui::FullOutput {
    egui::FullOutput { shapes, pixels_per_point: 1.0, ..Default::default() }
}

/// A REAL laid-out galley — the same deliberate mirror of `frame_sanity_gate.rs`'s `galley` helper
/// (and for the same reason as `shapes_frame` above). Laying out needs `&mut` fonts, which egui
/// only hands out inside a pass.
fn galley(ctx: &egui::Context, text: &str) -> std::sync::Arc<egui::Galley> {
    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(0.0), ..Default::default() };
    let mut g = None;
    let mut out = ctx.run_ui(raw, |ui| {
        g = Some(ui.painter().layout_no_wrap(
            text.to_owned(),
            egui::FontId::default(),
            Color32::WHITE,
        ));
    });
    out.textures_delta.clear();
    g.expect("the pass runs its closure exactly once")
}

/// A hand-built `ClippedPrimitive` carrying exactly the vertex positions given — used where the
/// property under test is arithmetic over a coordinate list rather than anything epaint does.
fn mesh_primitive(points: &[egui::Pos2]) -> egui::epaint::ClippedPrimitive {
    let mut mesh = egui::epaint::Mesh::default();
    for p in points {
        mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: egui::epaint::WHITE_UV, color: A });
    }
    egui::epaint::ClippedPrimitive {
        clip_rect: SCREEN,
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }
}

// ============================ the positive control ============================

#[test]
fn the_same_frame_records_identically_twice() {
    // Guards the failure mode that would make every `assert_ne!` below meaningless: a record that
    // differs run-to-run would "detect" all four planted defects while detecting nothing.
    let paint = |p: &egui::Painter| {
        fill(p, rect_at(10.0, 10.0, 100.0, 40.0), A);
        fill(p, rect_at(20.0, 60.0, 80.0, 30.0), B);
    };
    assert_eq!(record("control", paint), record("control", paint));
}

#[test]
fn the_record_carries_its_version_and_scenario_name() {
    // The two header lines exist so a stale or mis-copied golden fails on line 1 or 2 with an
    // obvious message, instead of producing a thousand-line diff.
    let r = record("named_scenario", |p| fill(p, rect_at(1.0, 1.0, 2.0, 2.0), A));
    let mut lines = r.lines();
    assert!(
        lines.next().is_some_and(|l| l.contains(RECORD_VERSION)),
        "the first line must carry the grammar version"
    );
    assert_eq!(lines.next(), Some("# scenario: named_scenario"));
}

// ============================ defect 1: z-order inversion ============================

#[test]
fn a_z_order_inversion_reddens_while_frame_sanity_stays_green() {
    // "Draw the overlay before the thing it should sit above." Both frames paint the SAME two
    // rects, at the SAME coordinates, in the SAME colours, under the SAME clip — only the order
    // differs. Every coordinate is finite in both, so `assert_frame_sane` (called inside `record`)
    // passes on both; and because the two rects share a clip rect and a texture, epaint batches
    // them into ONE primitive, so the bounding box, the vertex count and the colour SET are all
    // identical too. The ORDERED colour runs are the only field that moves — which is exactly why
    // the record carries runs in order rather than a colour set.
    let over = rect_at(20.0, 20.0, 60.0, 60.0);
    let under = rect_at(40.0, 40.0, 60.0, 60.0);

    let correct = record("z", |p| {
        fill(p, under, A);
        fill(p, over, B); // B on top
    });
    let inverted = record("z", |p| {
        fill(p, over, B);
        fill(p, under, A); // A on top — the inversion
    });

    assert_ne!(correct, inverted, "a z-order inversion must move the record");
    // …and it must move the RUN ORDER specifically, not the counts or the box.
    let runs = |r: &str| {
        r.lines()
            .find(|l| l.contains("runs "))
            .map(|l| l.split("runs ").nth(1).unwrap_or_default().to_owned())
            .expect("the frame has at least one mesh primitive")
    };
    let (c, i) = (runs(&correct), runs(&inverted));
    assert_ne!(c, i, "the ordered colour runs must differ");
    assert!(
        c.contains("#112233ff") && c.contains("#aabbccff"),
        "both sentinels are painted in both frames — the SET is unchanged: {c}"
    );
    assert!(
        c.find("#112233ff") < c.find("#aabbccff") && i.find("#112233ff") > i.find("#aabbccff"),
        "the sentinels must appear in opposite order:\n  correct {c}\n  inverted {i}"
    );
}

// ============================ defect 2: a pane collapsing ============================

#[test]
fn a_pane_collapsing_to_a_wrong_height_reddens_while_frame_sanity_stays_green() {
    // A pane rendered at a quarter of its height is the archetypal layout regression: nothing is
    // NaN, nothing is off-screen, nothing is even mis-coloured. The bounding box is the field that
    // sees it.
    let full = record("pane", |p| fill(p, rect_at(0.0, 0.0, 400.0, 200.0), A));
    let collapsed = record("pane", |p| fill(p, rect_at(0.0, 0.0, 400.0, 50.0), A));

    assert_ne!(full, collapsed);
    // The box is the rect grown by epaint's 1px anti-alias feathering (0.5 each way, so 0 → -1 and
    // 200 → 201 after rounding), which is why these are not the numbers passed to `rect_at`. The
    // HEIGHT is what the assertion is about, and it is unambiguous: 202 vs 52.
    assert!(full.contains("solid [-1 -1 401 201]"), "the healthy pane's box: {full}");
    assert!(collapsed.contains("solid [-1 -1 401 51]"), "the collapsed pane's box: {collapsed}");
}

// ============================ defect 3: a swapped colour token ============================

#[test]
fn a_swapped_colour_token_reddens_while_frame_sanity_stays_green() {
    // Identical geometry, one paint call reading the wrong palette entry. Geometry-only invariants
    // are blind to this by construction — the record's colour runs are not.
    let r = rect_at(10.0, 10.0, 100.0, 40.0);
    let right = record("colour", |p| fill(p, r, A));
    let wrong = record("colour", |p| fill(p, r, B));

    assert_ne!(right, wrong);
    assert!(right.contains("#112233ff"), "{right}");
    assert!(wrong.contains("#aabbccff"), "{wrong}");
    // And ONLY the colour moved: the geometry field is byte-identical, which is what proves the
    // record separates "moved" from "recoloured" instead of collapsing both into one blob.
    let solid = |s: &str| {
        s.lines()
            .find_map(|l| {
                l.split("solid ").nth(1).map(|t| t.split("  ").next().unwrap().to_owned())
            })
            .expect("a mesh primitive")
    };
    assert_eq!(solid(&right), solid(&wrong), "the geometry must be unchanged");
}

// ============================ defect 4: a narrowed clip rect ============================

#[test]
fn a_narrowed_clip_rect_reddens_while_frame_sanity_stays_green() {
    // Content silently cut. The SHAPES are identical in both frames — clipping happens downstream —
    // so a shape-level invariant cannot see it at all. `frame_sanity` explicitly does not check
    // clip rects for anything but `NaN`, because `Rect::EVERYTHING` is legitimate. The record's
    // `clip` field is the only place a narrowed scissor rect shows up.
    let content = rect_at(0.0, 0.0, 400.0, 100.0);
    let wide = record("clip", |p| {
        fill(&p.with_clip_rect(rect_at(0.0, 0.0, 400.0, 300.0)), content, A);
    });
    let narrow = record("clip", |p| {
        fill(&p.with_clip_rect(rect_at(0.0, 0.0, 200.0, 300.0)), content, A);
    });

    assert_ne!(wide, narrow);
    assert!(wide.contains("clip [0 0 400 300]"), "{wide}");
    assert!(narrow.contains("clip [0 0 200 300]"), "{narrow}");
}

// ========================== defect 5: a label cut by its clip (opt-in) ==========================

#[test]
fn a_truncated_label_reddens_the_clip_check_while_frame_sanity_stays_green() {
    // The faithful twin of the 2026-08-21 contact-sheet bug: `crates/vike-studio/src/studio.rs`'s
    // `empty_state` allocated a FIXED `vec2(280.0, 28.0)` button row, `vertical_centered` centred
    // that block into a CentralPanel narrower than 280, and the panel's own clip
    // (`CentralPanel::show` clips at its outer rect) cut the first button's left edge into
    // "?un backtest". Coordinates finite, so `assert_frame_sane` (run inside `ui_frame`) stays
    // green — the whole reason the opt-in rung exists. The 220px screen stands in for the squeezed
    // panel: centring 280 into it puts the block's left edge ~30px LEFT of the clip (the panel's
    // outer rect), while the row's actual content ends well inside the right edge.
    let out = ui_frame(vec2(220.0, 300.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(280.0, 28.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let _ = ui.button("Run backtest");
                        let _ = ui.button("Load a template");
                    },
                );
            });
        });
    });

    let findings = clipped_text_shapes(&out, CLIPPED_TEXT_TOL);
    let rendered: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert_eq!(findings.len(), 1, "exactly the one cut label, nothing else: {rendered:?}");
    let f = &findings[0];
    assert_eq!(f.text, "Run backtest", "the finding names the widget it is about: {f}");
    // The record moves in the FIELD that names the defect: a LEFT cut, the contact sheet's
    // direction — and only the left, because the row's content fits the panel; the fixed
    // allocation is what does not.
    assert!(f.painted.min.x < f.clip.min.x, "the cut is on the LEFT ('?un backtest'): {f}");
    assert!(f.painted.max.x < f.clip.max.x, "…and ONLY the left: {f}");

    // The assert wrapper fires on the same frame, naming the label — the shape a harness failure
    // will actually wear.
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_no_clipped_text(&out, CLIPPED_TEXT_TOL);
    }))
    .expect_err("the assert wrapper must fire on the frame the collector flagged");
    let msg = err.downcast_ref::<String>().expect("panic payload is a String").clone();
    assert!(msg.contains("Run backtest"), "the panic names the cut label: {msg}");
}

#[test]
fn the_fixed_empty_state_layout_is_green_at_the_widths_that_broke_it() {
    // The POST-fix twin of `empty_state`'s button row: clamp the allocation to the width that
    // actually exists and let the row wrap. At 220px the clamped row holds both buttons; at 150px
    // it cannot, and egui wraps (the second button to a new row, or its label within the button)
    // instead of clipping. The assertion is the same either way — nothing paints beyond the clip —
    // which is what makes this test robust to font-metric drift rather than pinned to one wrap
    // outcome.
    for width in [220.0_f32, 150.0] {
        let out = ui_frame(vec2(width, 300.0), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let row_w = ui.available_width().min(280.0);
                    ui.allocate_ui_with_layout(
                        vec2(row_w, 28.0),
                        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                        |ui| {
                            let _ = ui.button("Run backtest");
                            let _ = ui.button("Load a template");
                        },
                    );
                });
            });
        });
        let findings = clipped_text_shapes(&out, CLIPPED_TEXT_TOL);
        let rendered: Vec<String> = findings.iter().map(ToString::to_string).collect();
        assert!(rendered.is_empty(), "at {width}px the clamped row must not clip: {rendered:?}");
    }
}

#[test]
fn an_honestly_scrolled_frame_does_not_trip_the_clip_check() {
    // The control: a REAL mid-scroll `ScrollArea` — the false-positive class that killed the
    // global text-in-clip rule (`frame_sanity`'s module docs) — must produce ZERO findings.
    let out = ui_frame(vec2(220.0, 300.0), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(60.0).vertical_scroll_offset(10.0).show(
                ui,
                |ui| {
                    for i in 0..12 {
                        ui.label(format!("scrolled row {i}"));
                    }
                },
            );
        });
    });
    let findings = clipped_text_shapes(&out, CLIPPED_TEXT_TOL);
    let rendered: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert!(rendered.is_empty(), "an honest scroll cut is not a truncated label: {rendered:?}");

    // The positive control that keeps this from being the vacuous kind of pass: the frame must
    // GENUINELY contain text cut beyond the tolerance — vertically, by the 10px scroll offset (the
    // first row's glyphs start above the clip even after their cap-height inset) and the 60px
    // viewport's bottom edge — while still intersecting its clip. An any-axis rule WOULD have
    // fired here; the horizontal-only discriminator is the thing that spares it.
    fn vertically_cut(shape: &Shape, clip: Rect, hit: &mut bool) {
        match shape {
            Shape::Vec(group) => {
                for s in group {
                    vertically_cut(s, clip, hit);
                }
            }
            Shape::Text(t) => {
                let painted = t.visual_bounding_rect();
                if painted.is_finite()
                    && clip.intersects(painted)
                    && (painted.min.y < clip.min.y - CLIPPED_TEXT_TOL
                        || painted.max.y > clip.max.y + CLIPPED_TEXT_TOL)
                {
                    *hit = true;
                }
            }
            _ => {}
        }
    }
    let mut cut = false;
    for cs in &out.shapes {
        vertically_cut(&cs.shape, cs.clip_rect, &mut cut);
    }
    assert!(
        cut,
        "the control must contain a real vertical cut beyond {CLIPPED_TEXT_TOL}px — otherwise it \
         proves nothing about the discriminator"
    );
}

#[test]
fn a_sub_tolerance_overhang_is_not_a_finding_and_its_measured_size_is_exact() {
    // The tolerance boundary, both sides, on a SYNTHETIC frame: the clip is constructed FROM the
    // measured galley, so the planted overhang is an exact number rather than a font-metric
    // estimate.
    let ctx = egui::Context::default();
    let shape = Shape::galley(pos2(60.0, 50.0), galley(&ctx, "Run backtest"), Color32::WHITE);
    let painted = match &shape {
        Shape::Text(t) => t.visual_bounding_rect(),
        _ => unreachable!("Shape::galley builds a Shape::Text"),
    };
    let cut_by = |dx: f32| ClippedShape {
        clip_rect: Rect::from_min_max(pos2(painted.min.x + dx, 0.0), pos2(400.0, 300.0)),
        shape: shape.clone(),
    };

    let under = shapes_frame(vec![cut_by(1.0)]);
    assert!(
        clipped_text_shapes(&under, CLIPPED_TEXT_TOL).is_empty(),
        "a 1.0px cut sits under the {CLIPPED_TEXT_TOL}px tolerance — sub-half-glyph trims are the \
         healthy centred-text case, not a finding"
    );

    let over = shapes_frame(vec![cut_by(6.0)]);
    let findings = clipped_text_shapes(&over, CLIPPED_TEXT_TOL);
    assert_eq!(findings.len(), 1, "exactly the one planted cut");
    let f = &findings[0];
    assert_eq!(f.text, "Run backtest", "{f}");
    // The finding moves in the FIELD that names the defect: the measured size of the cut.
    assert!((f.overhang - 6.0).abs() < 1e-3, "the overhang is the planted 6.0px: {f}");
}

#[test]
fn text_pushed_entirely_outside_its_clip_is_not_a_finding() {
    // Pins the intersects-filter: a galley WHOLLY outside its clip is emitted-then-culled geometry
    // (the crosshair-off-plot poses park text exactly like this), and the dropped "nothing
    // off-screen" rule stays dropped — the mirror of `frame_sanity_gate.rs`'s
    // `a_far_offscreen_shape_is_accepted`, holding for this check too.
    let ctx = egui::Context::default();
    let out = shapes_frame(vec![ClippedShape {
        clip_rect: rect_at(0.0, 0.0, 400.0, 300.0),
        shape: Shape::galley(pos2(-5000.0, 50.0), galley(&ctx, "offscreen label"), Color32::WHITE),
    }]);
    assert!(
        clipped_text_shapes(&out, CLIPPED_TEXT_TOL).is_empty(),
        "a fully-scissored galley is culled geometry, not a truncated label"
    );
    assert_frame_sane(&out); // …and the rung below accepts it too, as its own gate already pins
}

// ============================ the record's own properties ============================

#[test]
fn a_coordinate_sitting_on_an_exact_half_flips_under_any_nudge_at_all() {
    // ⚠ THE LIMIT OF THE SCHEME, measured rather than assumed — and written down here because the
    // obvious test ("sub-pixel jitter below the rounding grid cannot move the record") was written
    // first, and it is FALSE.
    //
    // A circle of radius 20 centred on y = 60 puts its topmost feathered vertex at exactly 39.5.
    // Rounding is discontinuous there by construction, so a nudge of 1e-4 — or of one ULP, or of
    // anything at all downward — moves that edge from 40 to 39. No amount of care in the rounding
    // rule fixes this; every rule has a boundary and exact layout arithmetic lands on it constantly
    // (epaint's 0.5 feathering guarantees it).
    //
    // The consequence is the shape of this whole suite. A golden's robustness cannot come from
    // rounding being forgiving; it comes from the INPUTS being exact, so the coordinate that lands
    // on 39.5 lands on 39.5 everywhere. That is why the gate is
    // `a_one_ulp_price_perturbation_leaves_every_record_unchanged` over the real scenarios — which
    // passes — and not a tolerance claim, and why `rounding_margin` counts exact halves separately
    // instead of flagging them.
    //
    // A CIRCLE, deliberately, not a rect: epaint snaps `RectShape`s to the pixel grid (see the test
    // below), so a rect could not demonstrate this at all.
    let a = record("jitter", |p| {
        p.circle_filled(pos2(60.0, 60.0), 20.0, A);
    });
    let b = record("jitter", |p| {
        p.circle_filled(pos2(60.0, 60.0 - 1e-4), 20.0, A);
    });
    assert!(a.contains("solid [40 40 81 81]"), "the unnudged circle's box: {a}");
    assert!(b.contains("solid [40 39 81 80]"), "one ten-thousandth lower, one pixel up: {b}");
}

#[test]
fn epaint_snaps_rects_and_galleys_to_the_pixel_grid() {
    // Not our behaviour, but load-bearing for ours, so it is pinned rather than assumed: epaint
    // 0.36 rounds `RectShape` geometry and galley positions to the pixel grid before tessellating
    // (`Tessellator::tessellate_rect`'s `round_to_pixels`, and the `galley_pos.round_to_pixels`
    // in `tessellate_text`). That is a second, independent layer of the protection the record's own
    // integer rounding provides — and if a future egui turns it off, THIS is the test that says so,
    // instead of sixteen goldens moving for no visible reason.
    let a = record("snap", |p| fill(p, rect_at(10.0, 10.0, 100.0, 40.0), A));
    let b = record("snap", |p| fill(p, rect_at(10.3, 10.3, 100.0, 40.0), A));
    assert_eq!(a, b, "epaint no longer snaps rects to the pixel grid");
}

#[test]
fn a_whole_pixel_move_does_move_the_record() {
    // The control for the snapping test below: pixel-grid snapping must not have flattened
    // everything into noise.
    let a = record("move", |p| fill(p, rect_at(10.0, 10.0, 100.0, 40.0), A));
    let b = record("move", |p| fill(p, rect_at(13.0, 10.0, 100.0, 40.0), A));
    assert_ne!(a, b, "a 3px move must move the record");
}

#[test]
fn the_colour_run_list_is_capped_but_the_count_and_digest_are_not() {
    // Readability cap: a batch with hundreds of alternating colours must still produce a reviewable
    // line. The exact run COUNT is printed regardless, and the digest covers what is elided — so an
    // eighteenth run changing is still caught even though it is not printed.
    let many = |shift: u8| {
        move |p: &egui::Painter| {
            for i in 0..(MAX_COLOR_RUNS + 8) {
                let c = Color32::from_rgb(i as u8 + shift, 0x40, 0x40);
                fill(p, rect_at(i as f32 * 4.0, 10.0, 3.0, 20.0), c);
            }
        }
    };
    let r = record("runs", many(0));
    assert!(r.contains("+.."), "a long run list must be elided: {r}");
    assert!(
        r.contains(&format!("runs {}", MAX_COLOR_RUNS + 8)),
        "the exact count must survive: {r}"
    );
    // Change only a colour that falls BEYOND the printed cap: the visible prefix is identical and
    // the digest is what reddens.
    let shifted = record("runs", many(1));
    assert_ne!(r, shifted, "a change past the print cap must still move the record");
}

#[test]
fn rounding_margin_separates_exact_halves_from_a_coordinate_near_a_boundary() {
    // The diagnostic, proven to actually diagnose. Driven from HAND-BUILT primitives rather than a
    // rendered frame, because the property under test is arithmetic on a coordinate list and
    // routing it through the painter would only re-test epaint's pixel snapping (see
    // `epaint_snaps_rects_and_galleys_to_the_pixel_grid`).
    //
    // An exact half-integer is the SAFE case and must be counted as such rather than reported as a
    // near-miss: it is where epaint's 0.5 feathering puts a whole-pixel layout, and a half reached
    // by exact arithmetic is bit-identical everywhere.
    let clean = rounding_margin(&[mesh_primitive(&[
        pos2(10.5, 20.5),
        pos2(110.5, 20.5),
        pos2(110.5, 60.5),
    ])]);
    assert_eq!(clean.samples, 10, "6 vertex coords + 4 clip-rect edges");
    assert_eq!(clean.exact_halves, 6, "every vertex coordinate is an exact half: {clean}");
    assert_eq!(clean.near_edge, 0, "…and an exact half is NOT counted as near-edge: {clean}");

    // A tenth of the reporting threshold off a half: near the boundary, but not ON it — the state
    // the report exists to surface, and the one an exact half must not be conflated with.
    let off = 10.5_f32 + NEAR_EDGE_EPS * 0.1;
    let edgy = rounding_margin(&[mesh_primitive(&[pos2(off, 20.5), pos2(110.5, 60.5)])]);
    assert_eq!(edgy.near_edge, 1, "exactly the one off-half coordinate: {edgy}");
    assert!(edgy.min_margin > 0.0, "an exact half would be the safe case, not the reported one");
    assert!(edgy.min_margin < NEAR_EDGE_EPS, "…and it is the tightest margin carried: {edgy}");
    assert_eq!(edgy.worst, off, "the report names the offending coordinate: {edgy}");

    // A real frame, so the walk is also proven to reach a rendered mesh and not just a fabricated
    // one — the failure mode that would make every assertion above vacuous in practice.
    let ctx = egui::Context::default();
    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(0.0), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| fill(ui.painter(), rect_at(10.0, 10.0, 100.0, 40.0), A));
    out.textures_delta.clear();
    let ppp = out.pixels_per_point;
    let real = rounding_margin(&ctx.tessellate(out.shapes, ppp));
    assert!(real.samples > 8, "the walk must reach a rendered frame's vertices: {real}");
    assert!(real.exact_halves > 0, "whole-pixel layout + 0.5 feathering lands on halves: {real}");
}

// ============================ the golden-file mechanics ============================

/// A scratch golden path in a directory owned by THIS process.
///
/// ⚠ It must be a real tempdir, not `temp_dir().join(<fixed name>)`. A fixed name under a SHARED
/// `/tmp` is owned by whoever created it first: on the CI runners that is a different user from a
/// previous job, `remove_dir_all` then fails (its error was discarded), and the write lands on
/// somebody else's directory — `Permission denied (os error 13)`. That is not hypothetical, it is
/// how this file first went red in CI while passing on every developer box, where the same user
/// owns the leftover. Two concurrent jobs on one runner collide the same way.
///
/// ⚠ Bind the returned `TempDir`, do not discard it: `let (_, path) = …` drops it IMMEDIATELY and
/// deletes the directory out from under the test. `_dir` keeps it alive to end of scope, which is
/// also what cleans it up — no litter, unlike the fixed path this replaced.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("scratch tempdir");
    let path = dir.path().join("golden.txt");
    (dir, path)
}

const HINT: &str = "<the regeneration command>";

#[test]
fn a_missing_golden_fails_with_the_regeneration_command() {
    let (_dir, path) = scratch();
    let err = std::panic::catch_unwind(|| assert_frame_golden("x\n", &path, false, HINT))
        .expect_err("a missing golden must fail");
    let msg = err.downcast_ref::<String>().expect("panic payload is a String").clone();
    assert!(msg.contains("missing golden"), "{msg}");
    assert!(msg.contains(HINT), "the message must say how to create it: {msg}");
}

#[test]
fn update_mode_writes_the_file_and_the_written_file_then_compares_equal() {
    let (_dir, path) = scratch();
    let record = "# vike frame record v1\nline one\nline two\n";
    assert_frame_golden(record, &path, true, HINT);
    assert!(path.exists(), "update mode must create the golden and its directory");
    assert_frame_golden(record, &path, false, HINT); // must not panic
}

#[test]
fn a_mismatch_names_the_first_differing_line() {
    let (_dir, path) = scratch();
    assert_frame_golden("alpha\nbeta\ngamma\n", &path, true, HINT);
    let err = std::panic::catch_unwind(|| {
        assert_frame_golden("alpha\nBETA\ngamma\n", &path, false, HINT);
    })
    .expect_err("a differing record must fail");
    let msg = err.downcast_ref::<String>().expect("panic payload is a String").clone();
    assert!(msg.contains("line 2"), "the first differing line must be named: {msg}");
    assert!(msg.contains("- beta") && msg.contains("+ BETA"), "{msg}");
    assert!(!msg.contains("line 1"), "identical lines must not be printed: {msg}");
}

#[test]
fn crlf_line_endings_in_the_golden_do_not_redden_it() {
    // `.gitattributes` pins `tests/goldens/*.txt` to `eol=lf`, but a working copy checked out with
    // `core.autocrlf=true` before that rule existed would otherwise fail every golden at once with
    // a diff in which no line looks different — the worst possible first impression of the suite.
    let (_dir, path) = scratch();
    std::fs::create_dir_all(path.parent().expect("scratch path has a parent")).expect("mkdir");
    std::fs::write(&path, "alpha\r\nbeta\r\n").expect("write");
    assert_frame_golden("alpha\nbeta\n", &path, false, HINT); // must not panic
}
