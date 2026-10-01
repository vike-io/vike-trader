//! [`assert_frame_sane`] — the ONE shared geometry assertion every headless egui frame test calls.
//!
//! WHY THIS EXISTS: across the GUI crates ~598 tests run, 32 of them render a real
//! `egui::Context` frame, 9 look at the frame's output at all — and, before this module, ZERO
//! asserted a position, a size or a paint order. A test that renders a frame and then reads back
//! one string or one `ChartActions` field proves the frame did not PANIC; it does not prove the
//! frame is DRAWABLE. A shape carrying a `NaN` coordinate is a silent corruption: epaint
//! tessellates it into degenerate triangles, the GPU discards them, and the only symptom is
//! something missing on screen — which no headless test in this repo could see, and which the
//! rasterizing tests cannot see either because they run on the dev box and not in CI.
//!
//! Calling this from the harnesses that already render turns all 32 into geometry tests, and every
//! FUTURE frame test into one for free. It lives HERE — `vike-ui-theme`, layer 70, whose only
//! dependency is `egui` — because every GUI crate already sits above it, so one copy serves all of
//! them. A law spelled twice is the defect class this repo spends its review budget removing: do
//! not copy this function into a crate, add the dev-dependency.
//!
//! It is behind the `test-support` feature (the workspace convention: "one owned double behind a
//! `test-support` feature"), so a default build compiles none of it.
//!
//! # What it asserts
//!
//! Both invariants are checked on the STORED geometry of each shape variant
//! (`CircleShape::center`, `PathShape::points`, `TextShape::pos`, mesh vertex positions, …) and
//! never on `Shape::visual_bounding_rect()`, which returns `Rect::NOTHING` — i.e. ±infinity — for
//! a `Noop` or an empty-stroke shape and would report every healthy frame as broken.
//!
//! 1. **No `NaN` anywhere** — in any emitted shape coordinate, or in any clip rect. `NaN` has no
//!    legitimate producer in egui geometry: it is arithmetic that divided by a zero-width range or
//!    folded an empty extent, and it paints nothing while raising nothing.
//! 2. **No infinite or absurd SHAPE coordinate** (see [`ABSURD_COORD`]). Infinity IS legitimate in
//!    a CLIP rect — `Rect::EVERYTHING` (`[-inf, inf]²`) and `Rect::NOTHING` are egui's own
//!    sentinels and appear on real frames — so clip rects are checked for `NaN` only.
//!
//! ⚠ Worth knowing about the clip-rect half, because a planted defect that should have reddened
//! did not: `egui::Painter::with_clip_rect` INTERSECTS the requested rect with the parent's, and
//! Rust's `f32::max`/`f32::min` return the NON-`NaN` operand — so a `NaN` handed to
//! `with_clip_rect` is silently erased and never reaches the frame. The check bites the
//! direct-assignment APIs (`Ui::set_clip_rect` / `Painter::set_clip_rect`), and `epaint`'s
//! `PaintList::add` stores whatever it is given without filtering, so a `NaN` clip rect from those
//! DOES reach `FullOutput` — verified by planting one in `stored_catalog_grid`, which reddened all
//! seven of `vike-data-manager`'s frame tests. Today this workspace only ever calls
//! `with_clip_rect`; the check is there for the day it does not.
//!
//! # What was proposed, measured, and DROPPED
//!
//! Three further invariants were proposed. Each was run against all 32 existing frames BEFORE
//! being written down as a rule, on the standard that an invariant which fires on known-good
//! frames is a wrong invariant and not a bug found. All three were dropped, for three different
//! reasons, and the measurements are recorded here so the next person to propose one reads the
//! result instead of re-running the experiment.
//!
//! **DROPPED: "every `Shape::Text` galley's rect lies within its own clip rect."** It fires on
//! healthy frames, because egui centres a label on its anchor and lets the clip rect trim the
//! sub-pixel overhang. Measured on `vike-chart`'s own passing tests: the options-chain expiry pill
//! paints `"01 Aug"` across `x ∈ [920.0, 960.4]` under a clip ending at `960.0`, and a study
//! pane's y-axis tick `"1"` paints across `y ∈ [-1.8, 13.2]` under a clip starting at `0.0`. Both
//! galleys report `elided == false`, so this is not even the truncation case — it is ordinary
//! centred text. Deliberate clipping (a truncated label, a scrolled `ScrollArea` row, a table cell
//! narrower than its content) produces the same signal as a real layout bug, and no threshold
//! separates them. That verdict stands — [`assert_frame_sane`] still never compares text against
//! clip. The defect CLASS is covered instead by [`clipped_text_shapes`], an OPT-IN per-harness
//! check whose discriminators are the section below.
//!
//! **DROPPED: "nothing is painted entirely outside `screen`."** It fires hardest of the three.
//! `vike-chart`'s `case2_pointer_outside_plot_no_hover` deliberately places the pointer at
//! `(5000, 5000)` on a 960×620 screen — that is the whole point of the test — and `egui_plot`
//! duly paints the crosshair and its tick geometry there: **150 of that frame's 332 shapes** lie
//! entirely off-screen, on a test that passes and should. Culling is the renderer's job, not the
//! layout's; an emitted off-screen shape carries no information about correctness.
//!
//! **DROPPED: "no interactive/allocated rect of zero area."** This one is not merely wrong, it is
//! VACUOUS — a gate that cannot fail. It is unreachable from `FullOutput`, which carries shapes
//! and not widget rects; the only public way to widget rects in egui 0.36 is
//! `egui::Context::interactive_rects_last_pass`, and that accessor already filters
//! `rect.is_positive() && rect.is_finite()` before returning, so a zero-area rect can never appear
//! in its output. Confirmed empirically as well as by reading it: a probe frame containing
//! `ui.allocate_response(Vec2::ZERO, Sense::click())` (egui's Id-reservation idiom), a separator,
//! an empty `CollapsingHeader` and a `ScrollArea` with 40 rows in a 20px slot reported
//! `total=7, zero_area=0`. Were the accessor not filtering, the idiom above would have made this
//! fire on correct code anyway.
//!
//! # The opt-in truncation check
//!
//! [`clipped_text_shapes`] / [`assert_no_clipped_text`] cover the class the dropped text-in-clip
//! rule was aimed at — a label silently cut by its clip — WITHOUT overturning the measurement that
//! dropped it. The motivating defect: the 2026-08-21 GPU contact sheet showed Studio's
//! centre-panel empty state rendering its primary button as `?un backtest`
//! (`crates/vike-studio/src/studio.rs`'s `empty_state` allocated a fixed 280px row that a narrower
//! panel's clip cut on the left). Every CPU rung was blind to it: the coordinates were finite (this
//! module), the button present and enabled (the a11y harness), the click still fired (the
//! characterization suites) — only pixels showed it. This check is what a CPU rung CAN see of that
//! class, given three discriminators the global rule lacked:
//!
//! 1. **HORIZONTAL axis only.** The worst measured healthy HORIZONTAL overhang is the 0.4px
//!    expiry-pill trim above, while the worst healthy VERTICAL one is 1.8px — and vertical
//!    clipping is what a scrolled `ScrollArea` (the overwhelmingly common scroll) does
//!    deliberately, row after row. Checking x and never y is what keeps every vertically-scrolled
//!    frame quiet even at a call site that wired the check in wrongly.
//! 2. **PARTIALLY VISIBLE only.** A galley wholly outside its clip is emitted-then-culled
//!    geometry (epaint's per-row culling in `tessellate_text` discards it) — the dropped
//!    "nothing off-screen" rule stays dropped.
//! 3. **A tolerance**, [`CLIPPED_TEXT_TOL`], sitting well above the healthy centred-overhang band
//!    and well below one cut glyph.
//!
//! And ONE structural decision: it is NEVER called from [`assert_frame_sane`]. egui marks scrolled
//! content with nothing but a plain clip rect — `ScrollArea` sets `content_ui.set_clip_rect(..)`
//! exactly the way `CentralPanel` does — so shape data alone cannot tell an honest mid-scroll cut
//! from a truncated label; the discrimination can only come from the CALLER knowing its frame
//! contains no scrolled content. Harnesses opt in where that is true.
//!
//! Declared residuals: a label cut top/bottom is invisible (the vertical axis is deliberately
//! unchecked); an overhang at or under the tolerance — a sub-half-glyph cut — is invisible; a
//! label pushed ENTIRELY outside its clip is invisible (deliberate, per discriminator 2); a frame
//! holding mid-HORIZONTAL-scroll content, or a deliberately wider-than-viewport scroll row, WILL
//! trip it — that is the opt-in contract, not a bug; and egui's own `…` elision (`Galley::elided`)
//! is never flagged — that is egui truncating BY DESIGN, visibly, not a clip lying silently. A
//! false positive at an opted-in call site is a loud finding naming the label; the remedy is that
//! caller passing a larger tolerance, never widening [`CLIPPED_TEXT_TOL`] to make a test pass.

use egui::epaint::{ClippedShape, Shape};
use egui::{Pos2, Rect};

/// The largest coordinate magnitude a healthy frame is allowed to carry.
///
/// NOT a screen bound — "on screen" is not assertable, see the module docs. This is *finiteness
/// with a margin*: a coordinate at `f32::MAX` scale is as unpaintable as infinity (the
/// tessellator's `expand`/`intersect` arithmetic overflows straight to infinity from one), and it
/// passes an `is_finite` check, so the NaN/infinity rule alone would let that whole class through.
/// The threshold sits far above any plausible layout or virtual-scroll extent, so it can only trip
/// on arithmetic that has already gone wrong: the widest coordinate any of the 32 existing frames
/// emits is `5008.0`, six orders of magnitude below it.
pub const ABSURD_COORD: f32 = 1.0e9;

/// What one frame's geometry looked like — the measurement [`assert_frame_sane`] gates on.
///
/// Exposed so a caller can inspect a frame without panicking.
#[derive(Debug, Default, Clone)]
pub struct FrameReport {
    /// Top-level `ClippedShape`s in `FullOutput::shapes`.
    pub clipped_shapes: usize,
    /// Leaf shapes after flattening every `Shape::Vec` group.
    pub leaves: usize,
    /// Shape coordinates that are `NaN`, infinite, or beyond [`ABSURD_COORD`] — one per finding.
    pub bad_shape_coords: Vec<String>,
    /// Clip rects carrying a `NaN`. Infinities are NOT collected: `Rect::EVERYTHING` and
    /// `Rect::NOTHING` are egui's own sentinels and appear on healthy frames.
    pub nan_clip_rects: Vec<String>,
}

impl FrameReport {
    /// Whether every invariant held.
    #[must_use]
    pub fn is_sane(&self) -> bool {
        self.bad_shape_coords.is_empty() && self.nan_clip_rects.is_empty()
    }
}

impl std::fmt::Display for FrameReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} clipped shapes / {} leaves | bad shape coords {} | NaN clip rects {}",
            self.clipped_shapes,
            self.leaves,
            self.bad_shape_coords.len(),
            self.nan_clip_rects.len(),
        )
    }
}

/// Assert one rendered frame is geometrically sane. Call it from every headless-frame harness.
///
/// ⚠ Call it AFTER `out.textures_delta.clear()` where a harness does that. egui 0.36 panics on
/// dropping a `TexturesDelta` that still holds unapplied deltas, so a frame asserted BEFORE the
/// clear panics a SECOND time in its destructor while this assertion is unwinding — a
/// non-unwinding double panic, which ABORTS the process. Measured, not theorised: a planted NaN
/// in `chart::draw` killed all 18 `draw_characterization` scenarios with `SIGABRT` / "panic in a
/// destructor during cleanup", printing a backtrace and never the offending coordinate. Clearing
/// first is safe because `clear` touches `textures_delta` and never `shapes` — it cannot hide
/// anything this function reads.
///
/// # Panics
///
/// If any emitted shape coordinate is `NaN`, infinite or beyond [`ABSURD_COORD`], or any clip rect
/// carries a `NaN`. The message names the offending shapes, capped so a frame that is wrong
/// everywhere still prints a readable failure.
#[track_caller]
pub fn assert_frame_sane(out: &egui::FullOutput) {
    let report = frame_report(out);
    assert!(
        report.is_sane(),
        "frame geometry is not paintable ({report})\n  bad shape coords: {:#?}\n  NaN clip rects: {:#?}",
        capped(&report.bad_shape_coords),
        capped(&report.nan_clip_rects),
    );
}

/// The same measurement [`assert_frame_sane`] gates on, returned instead of asserted.
#[must_use]
pub fn frame_report(out: &egui::FullOutput) -> FrameReport {
    let mut r = FrameReport { clipped_shapes: out.shapes.len(), ..Default::default() };
    for cs in &out.shapes {
        check_clip_rect(cs, &mut r);
        walk(&cs.shape, &mut r);
    }
    r
}

// ============================ internals ============================

fn capped(v: &[String]) -> Vec<&str> {
    v.iter().take(8).map(String::as_str).collect()
}

/// A clip rect may legitimately be `Rect::EVERYTHING` / `Rect::NOTHING` (both infinite), so only
/// `NaN` is a defect here.
fn check_clip_rect(cs: &ClippedShape, r: &mut FrameReport) {
    let c = cs.clip_rect;
    if c.min.x.is_nan() || c.min.y.is_nan() || c.max.x.is_nan() || c.max.y.is_nan() {
        r.nan_clip_rects.push(format!("{c:?} around {}", shape_name(&cs.shape)));
    }
}

fn shape_name(s: &Shape) -> &'static str {
    match s {
        Shape::Noop => "Noop",
        Shape::Vec(_) => "Vec",
        Shape::Circle(_) => "Circle",
        Shape::Ellipse(_) => "Ellipse",
        Shape::LineSegment { .. } => "LineSegment",
        Shape::Path(_) => "Path",
        Shape::Rect(_) => "Rect",
        Shape::Text(_) => "Text",
        Shape::Mesh(_) => "Mesh",
        Shape::QuadraticBezier(_) => "QuadraticBezier",
        Shape::CubicBezier(_) => "CubicBezier",
        Shape::Callback(_) => "Callback",
    }
}

/// Recurse into `Shape::Vec` groups, checking every leaf's STORED geometry.
fn walk(shape: &Shape, r: &mut FrameReport) {
    if let Shape::Vec(group) = shape {
        for s in group {
            walk(s, r);
        }
        return;
    }
    r.leaves += 1;

    let name = shape_name(shape);
    let mut bad = |what: &str, v: String| r.bad_shape_coords.push(format!("{name}.{what} = {v}"));

    match shape {
        Shape::Noop | Shape::Vec(_) => {}
        Shape::Circle(c) => {
            check_pos(c.center, "center", &mut bad);
            check_f32(c.radius, "radius", &mut bad);
        }
        Shape::Ellipse(e) => {
            check_pos(e.center, "center", &mut bad);
            check_f32(e.radius.x, "radius.x", &mut bad);
            check_f32(e.radius.y, "radius.y", &mut bad);
        }
        Shape::LineSegment { points, .. } => check_points(points, &mut bad),
        Shape::Path(p) => check_points(&p.points, &mut bad),
        Shape::Rect(rect) => check_rect(rect.rect, "rect", &mut bad),
        Shape::Text(t) => {
            check_pos(t.pos, "pos", &mut bad);
            check_f32(t.angle, "angle", &mut bad);
            // The galley's own laid-out extent: a text layout that produced a NaN or runaway width
            // paints nothing, and `pos` alone would not show it.
            check_rect(t.galley.rect, "galley.rect", &mut bad);
        }
        Shape::Mesh(m) => {
            for (i, v) in m.vertices.iter().enumerate() {
                check_pos(v.pos, &format!("vertices[{i}].pos"), &mut bad);
            }
        }
        Shape::QuadraticBezier(b) => check_points(&b.points, &mut bad),
        Shape::CubicBezier(b) => check_points(&b.points, &mut bad),
        Shape::Callback(c) => check_rect(c.rect, "rect", &mut bad),
    }
}

fn check_points(points: &[Pos2], bad: &mut impl FnMut(&str, String)) {
    for (i, p) in points.iter().enumerate() {
        check_pos(*p, &format!("points[{i}]"), bad);
    }
}

fn check_pos(p: Pos2, what: &str, bad: &mut impl FnMut(&str, String)) {
    check_f32(p.x, &format!("{what}.x"), bad);
    check_f32(p.y, &format!("{what}.y"), bad);
}

fn check_rect(rect: Rect, what: &str, bad: &mut impl FnMut(&str, String)) {
    check_pos(rect.min, &format!("{what}.min"), bad);
    check_pos(rect.max, &format!("{what}.max"), bad);
}

fn check_f32(v: f32, what: &str, bad: &mut impl FnMut(&str, String)) {
    if v.is_nan() {
        bad(what, "NaN".to_owned());
    } else if v.is_infinite() {
        bad(what, format!("{v}"));
    } else if v.abs() > ABSURD_COORD {
        bad(what, format!("{v} (beyond ±{ABSURD_COORD:e})"));
    }
}

// ============================ the opt-in truncation check ============================

/// The horizontal overhang (px) a text shape may paint beyond its clip rect before
/// [`clipped_text_shapes`] reports it.
///
/// 5× the worst measured healthy HORIZONTAL overhang (the 0.4px expiry-pill trim in the module
/// docs), and well under one glyph advance at egui's default text styles (~7px) — so a cut that
/// removes even half a character clears it while ordinary centred sub-pixel overhang never does. A
/// call site with a legitimately larger centred overhang passes a bigger tolerance of its own;
/// this constant is never widened to make a test pass.
pub const CLIPPED_TEXT_TOL: f32 = 2.0;

/// One text shape painting beyond its clip rect on the horizontal axis — a truncated label. The
/// finding [`clipped_text_shapes`] collects.
#[derive(Debug, Clone)]
pub struct ClippedText {
    /// The label's laid-out text, so a finding names the widget it is about.
    pub text: String,
    /// The painted extent (`TextShape::visual_bounding_rect`: the galley's `mesh_bounds` with the
    /// shape's rotation folded in, translated to its `pos`).
    pub painted: Rect,
    /// The clip rect that cuts it.
    pub clip: Rect,
    /// Pixels of text beyond the clip on the worse of the two horizontal sides.
    pub overhang: f32,
}

impl std::fmt::Display for ClippedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} painted x [{:.1} {:.1}] under clip x [{:.1} {:.1}] — {:.1}px cut",
            self.text,
            self.painted.min.x,
            self.painted.max.x,
            self.clip.min.x,
            self.clip.max.x,
            self.overhang,
        )
    }
}

/// Every `Shape::Text` in `out` that paints beyond its clip rect HORIZONTALLY by more than
/// `tolerance` while still intersecting it — the truncated-label class.
///
/// OPT-IN: call it from a harness that KNOWS its frame contains no scrolled content — the module
/// docs carry the discriminators, the declared residuals, and why [`assert_frame_sane`] must never
/// call it. [`CLIPPED_TEXT_TOL`] is the default tolerance to pass.
#[must_use]
pub fn clipped_text_shapes(out: &egui::FullOutput, tolerance: f32) -> Vec<ClippedText> {
    let mut findings = Vec::new();
    for cs in &out.shapes {
        collect_clipped_text(&cs.shape, cs.clip_rect, tolerance, &mut findings);
    }
    findings
}

/// Assert [`clipped_text_shapes`] finds nothing. The failure names each cut label, capped the same
/// way [`assert_frame_sane`]'s own message is.
///
/// # Panics
///
/// If any text shape overhangs its clip rect horizontally by more than `tolerance`.
#[track_caller]
pub fn assert_no_clipped_text(out: &egui::FullOutput, tolerance: f32) {
    let findings = clipped_text_shapes(out, tolerance);
    let rendered: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert!(
        findings.is_empty(),
        "text painted beyond its clip rect (a truncated label): {:#?}",
        capped(&rendered),
    );
}

/// Recurse into `Shape::Vec` groups the way `walk` does, carrying the group's clip rect down to
/// every text leaf.
fn collect_clipped_text(shape: &Shape, clip: Rect, tolerance: f32, out: &mut Vec<ClippedText>) {
    match shape {
        Shape::Vec(group) => {
            for s in group {
                collect_clipped_text(s, clip, tolerance, out);
            }
        }
        Shape::Text(t) => {
            let painted = t.visual_bounding_rect();
            if !painted.is_finite() {
                // An empty galley (`Rect::NOTHING`) paints nothing; a NaN is `walk`'s finding.
                return;
            }
            if !clip.intersects(painted) {
                // Fully scissored = culled geometry — the dropped off-screen rule stays dropped.
                // (A NaN clip also fails `intersects`; it reddens `assert_frame_sane` on its own.)
                return;
            }
            // `Rect::EVERYTHING` clips pass `intersects` and dismiss themselves here: both sides
            // come out -inf, and -inf is never above the tolerance.
            let overhang = (clip.min.x - painted.min.x).max(painted.max.x - clip.max.x);
            if overhang > tolerance {
                out.push(ClippedText { text: t.galley.text().to_owned(), painted, clip, overhang });
            }
        }
        _ => {}
    }
}
