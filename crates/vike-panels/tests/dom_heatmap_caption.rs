//! The Elite heatmap's empty-state caption stays inside its strip.
//!
//! Until the heatmap has a column to paint — `draw` pushes the first one on its sixth frame, so for the
//! first five after a DOM opens or is switched to Elite — the strip paints a caption in the middle of
//! itself. It was laid out on ONE line and painted unclipped, so wherever the strip is narrower than
//! that line it ran out of the strip on both sides, and on the right it ran over the ladder. The
//! launcher opens a DOM 320 pt wide, which leaves the strip 127 pt: the caption is a good deal wider.
//!
//! The caption is painted text, not a widget, so it is not in the accessibility tree `dom_fit.rs`
//! reads; this reads the shape list instead, which is still pure CPU, so a GPU-less runner proves it.
//! `crates/vike-panels/src/dom.rs`'s `paint_heatmap` fills the strip and then paints the caption on
//! it, so the strip is read off the fill painted just before the caption rather than restated here
//! from the layout's own arithmetic.

use std::collections::BTreeMap;

use egui::epaint::{ClippedShape, Shape};
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::{DomInputs, DomMode, DomState};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::type_scale::TextSize;

/// Half a point: the tolerance for the rounding egui does to a rect's edges.
const EPS: f32 = 0.6;

/// The caption's two halves. It is painted on one line where the strip holds it and one half above
/// the other where it does not, so a half is what is looked for and every shape carrying one is
/// measured.
const CAPTION: [&str; 2] = ["liquidity heatmap", "time × price"];

fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

/// A context under the look `a`, primed: `appearance_ready` installs the look on its first ask and
/// answers `false` for that frame (fonts take effect at the start of the NEXT pass), so one empty
/// frame leaves the context ready for a frame that draws.
fn primed(a: &Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let _ = vike_ui_theme::harness::appearance_ready(ui.ctx(), a);
    });
    // epaint panics on a `TexturesDelta` dropped with entries nobody applied.
    out.textures_delta.clear();
    ctx
}

/// The shapes of the FIRST frame an Elite DOM `width` pt wide draws. The state is fresh, so `draw`'s
/// own frame counter has not reached the frame that pushes the first heatmap column.
fn first_frame(ctx: &egui::Context, width: f32) -> Vec<ClippedShape> {
    let book = book();
    let inputs = DomInputs {
        book: &book,
        last: Some(65_432.1),
        orders: &[],
        position: None,
        stale: false,
        paper: true,
        caps: VenueCaps::UNSUPPORTED,
        source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
        absence: None,
    };
    let mut state = DomState { mode: DomMode::Elite, ..DomState::default() };
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 560.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        // The workspace's tool body keeps 8 pt each side, and the strips fill what is left.
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
            let _ = vike_panels::dom::draw(ui, &mut state, &inputs);
        });
    });
    out.textures_delta.clear();
    out.shapes
}

/// Where the caption was painted, and the strip it sits on.
struct Placed {
    look: String,
    width: f32,
    text: egui::Rect,
    strip: egui::Rect,
}

impl Placed {
    /// How far the caption lies past the strip, on the side it lies furthest past; 0 when inside.
    fn past(&self) -> f32 {
        [
            self.strip.min.x - self.text.min.x,
            self.text.max.x - self.strip.max.x,
            self.strip.min.y - self.text.min.y,
            self.text.max.y - self.strip.max.y,
            0.0,
        ]
        .into_iter()
        .fold(f32::MIN, f32::max)
    }
}

fn placed(look: &str, width: f32, shapes: &[ClippedShape]) -> Placed {
    let is_caption = |cs: &ClippedShape| matches!(&cs.shape, Shape::Text(t) if CAPTION.iter().any(|half| t.galley.text().contains(half)));
    let first = shapes.iter().position(is_caption).unwrap_or_else(|| {
        panic!("{look} at {width}: an Elite DOM with no heatmap history paints its caption")
    });
    let Shape::Rect(fill) = &shapes[first - 1].shape else {
        panic!(
            "{look} at {width}: the strip's fill is painted just before its caption, got {:?}",
            shapes[first - 1].shape
        )
    };
    // Every text shape carrying a half of the caption, together: the caption is as wide as its
    // widest line and as tall as all of them.
    let text = shapes[first..]
        .iter()
        .filter(|cs| is_caption(cs))
        .filter_map(|cs| match &cs.shape {
            Shape::Text(t) => Some(t.visual_bounding_rect()),
            _ => None,
        })
        .reduce(|a, b| a.union(b))
        .expect("the first caption shape is among them");
    Placed { look: look.to_string(), width, text, strip: fill.rect }
}

/// Fails, if any caption lies outside its strip, with one line per look: how many widths and which,
/// and the worst.
fn assert_inside(what: &str, all: &[Placed]) {
    let out: Vec<&Placed> = all.iter().filter(|p| p.past() > EPS).collect();
    let mut looks: BTreeMap<&str, Vec<&Placed>> = BTreeMap::new();
    for p in &out {
        looks.entry(p.look.as_str()).or_default().push(p);
    }
    let lines: Vec<String> = looks
        .iter()
        .map(|(look, ps)| {
            let lo = ps.iter().map(|p| p.width).fold(f32::MAX, f32::min);
            let hi = ps.iter().map(|p| p.width).fold(f32::MIN, f32::max);
            let worst = ps.iter().max_by(|a, b| a.past().total_cmp(&b.past())).expect("one or more");
            format!(
                "{look}: outside its strip at {} of {} width(s), {lo}..{hi}; worst at {}: caption \
                 x {:.1}..{:.1} y {:.1}..{:.1} on a strip x {:.1}..{:.1} y {:.1}..{:.1} ({:.1} pt past)",
                ps.len(),
                all.iter().filter(|p| p.look == *look).count(),
                worst.width,
                worst.text.min.x,
                worst.text.max.x,
                worst.text.min.y,
                worst.text.max.y,
                worst.strip.min.x,
                worst.strip.max.x,
                worst.strip.min.y,
                worst.strip.max.y,
                worst.past(),
            )
        })
        .collect();
    assert!(
        out.is_empty(),
        "{what}: {} caption(s) outside the strip:\n{}",
        out.len(),
        lines.join("\n")
    );
}

/// The default look, from below the launcher's own width (296 leaves 24 pt for whatever margin the
/// workspace's window frame takes) up to a wide window.
#[test]
fn the_caption_stays_inside_its_strip_at_every_width_from_the_launchers_up() {
    let ctx = primed(&Appearance::default());
    let all: Vec<Placed> = (296..=900)
        .step_by(2)
        .map(|w| w as f32)
        .map(|w| placed("Graphite / Normal / Standard", w, &first_frame(&ctx, w)))
        .collect();
    assert_inside("the default appearance", &all);
}

/// Denser, looser and larger looks set the caption in a different face size, and so on a different
/// line; the strip is the same width. From 320 up, as `dom_fit.rs` does for the same reason.
#[test]
fn every_density_and_text_size_keeps_it_inside_too() {
    let mut all = Vec::new();
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let look = format!("{density:?} / {text_size:?}");
            let ctx = primed(&Appearance { density, text_size, ..Appearance::default() });
            for width in (320..=900).step_by(8) {
                let width = width as f32;
                all.push(placed(&look, width, &first_frame(&ctx, width)));
            }
        }
    }
    assert_inside("every density and text size", &all);
}
