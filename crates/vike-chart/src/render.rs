//! Per-style series renderers + indicator overlay/oscillator painting + the
//! chart-STYLE menu icon, split out of chart.rs (chart-UX bundle T0). Pure
//! egui_plot painting — no interaction state, no layout; `chart::draw`
//! orchestrates these against the price/volume/oscillator panes it owns.

use crate::chart::ChartStyle;
use crate::indicators::{Active, Category, LineDash, OutputStyle};
use crate::interact::vis;
use crate::model::Bar;
use crate::options::{rgba, ChartOptions};
use crate::transforms;
use egui::{Align2, Color32, Rect, Stroke};
use egui_plot::{
    HLine, Line, MarkerShape, PlotBounds, PlotGeometry, PlotItem, PlotItemBase, PlotPoint,
    PlotPoints, PlotTransform, Points, Polygon, Text,
};

// The former per-style color consts (UP/DOWN/UP_S/DOWN_S/LINE/GRID/CROSS) are
// now `ChartOptions` fields (chart-UX bundle T6): their values live in
// `ChartOptions::default()`, and every painter below reads them off the
// `opts: &ChartOptions` threaded in by `chart::draw` (the effective options
// for the frame — the dialog's working copy while open, else the committed
// set). Alpha-FILL literals mechanically derived from a base color now derive
// from the corresponding option color via `options::rgba`.
const HALF_BODY: f64 = 0.34;

// --- Footprint style consts (SP2, T5) ------------------------------------------------------

/// Footprint cell half-width in the x (bar-index) axis — wider than the candle body's
/// `HALF_BODY` (cells read as an almost-contiguous grid), but shy of 0.5 so adjacent bars' cell
/// columns keep a thin visible gutter between them.
const CELL_HALF_W: f64 = 0.45;
/// Footprint cell background tint alpha — faint (like `draw_baseline`'s/`draw_hlc_area`'s
/// subtle-wash fills), so the buy/sell numbers stay legible drawn on top and the tint reads as
/// an imbalance wash, not an opaque bar (unlike `draw_columns`' solid alpha-150 fill).
const CELL_FILL_ALPHA: u8 = 40;
/// Footprint per-bar point-of-control cell outline — the same amber as the chart module's
/// private `PROFILE_POC_COLOR` (the volume-profile overlay's POC line), duplicated rather than
/// shared for the same reason that module's `CVD_COLOR`/`dom.rs`'s `LAST` are
/// (`crates/vike-chart/src/chart/consts.rs`'s `PROFILE_POC_COLOR`): a fixed single-purpose
/// color, not a rotation array. The two POCs never coincide on screen —
/// footprint's is the max-volume cell WITHIN one bar, the overlay's is over the whole visible
/// range — but share the palette's amber "point of control" meaning.
const AMBER: Color32 = Color32::from_rgb(240, 180, 41);

/// One candle's geometry in **data space** (bar-index x, mapped-price y) — the SINGLE source
/// both the egui painter ([`draw_candles`]) and the GPU seam ([`GpuCandleItem`]) derive from,
/// so a GPU-drawn candle can never diverge from an egui-drawn one (pixel-parity by
/// construction). Every field is exactly what `draw_candles` used to inline.
pub struct CandleGeom {
    /// Body quad corners, in the same order `draw_candles`' `Polygon` used:
    /// `[t-HALF_BODY, lo], [t+HALF_BODY, lo], [t+HALF_BODY, hi], [t-HALF_BODY, hi]`, where
    /// `(lo, hi)` are the mapped open/close edges (lower, higher in mapped-value space).
    pub body: [[f64; 2]; 4],
    /// Wick segment endpoints `[t, map(low)], [t, map(high)]`.
    pub wick: [[f64; 2]; 2],
    /// Body FILL color (up/down), chosen off the RAW (scale-mode-invariant)
    /// bull/bear test. This is the color the GPU seam ([`candle_instance`])
    /// carries — the GPU candle layer draws a single-color candle.
    pub color: Color32,
    /// Body BORDER (Polygon stroke) color — `border_up`/`border_down`, defaulting
    /// to the body color so an unedited chart is pixel-identical.
    pub border_color: Color32,
    /// Wick (Line) color — `wick_up`/`wick_down`, defaulting to the body color.
    pub wick_color: Color32,
    /// True iff this body is drawn UNFILLED — i.e. the exact `hollow && bull` predicate
    /// `draw_candles` feeds `fill_color` (a hollow-style bull candle). Not the style flag.
    pub hollow: bool,
}

/// Extracted VERBATIM from `draw_candles`' per-bar body: bull/bear color, the mapped
/// open/close body edges, and the mapped low/high wick — the ONE candle-geometry authority
/// (see [`CandleGeom`]). `hollow_style` is the chart's Hollow style flag; the returned
/// `CandleGeom::hollow` is `hollow_style && bull` (only hollow-style bull bodies are unfilled).
///
/// `prev_close` is the RAW close of the immediately preceding drawn bar (`None` for the first
/// bar in the slice). It only matters when `opts.color_bars_prev_close` is set (TV "Color bars
/// based on previous close"): then bull/bear tests `close >= prev_close` rather than
/// `close >= open`; the first-bar `None` falls back to `close >= open`. Body FILL, body BORDER,
/// and WICK take three independent up/down color pairs off `opts` — all default to the body
/// color, so an unedited chart is pixel-identical.
pub(crate) fn candle_geom(
    bar: &Bar,
    prev_close: Option<f64>,
    hollow_style: bool,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) -> CandleGeom {
    // bull/bear classification stays RAW: it's a semantic (color) decision
    // that must not depend on the active scale mode, not a plotted position.
    let bull = match (opts.color_bars_prev_close, prev_close) {
        (true, Some(pc)) => bar.c >= pc,
        _ => bar.c >= bar.o,
    };
    let color = if bull { opts.up_col() } else { opts.down_col() };
    let border_color = if bull { opts.border_up_col() } else { opts.border_down_col() };
    let wick_color = if bull { opts.wick_up_col() } else { opts.wick_down_col() };
    // Body low/high edges still key off the raw open/close geometry (not the
    // coloring test) — the candle's shape never changes, only its color.
    let (lo, hi) = if bar.c >= bar.o { (map(bar.o), map(bar.c)) } else { (map(bar.c), map(bar.o)) };
    CandleGeom {
        body: [
            [bar.t - HALF_BODY, lo],
            [bar.t + HALF_BODY, lo],
            [bar.t + HALF_BODY, hi],
            [bar.t - HALF_BODY, hi],
        ],
        wick: [[bar.t, map(bar.l)], [bar.t, map(bar.h)]],
        color,
        border_color,
        wick_color,
        hollow: hollow_style && bull,
    }
}

pub(crate) fn draw_candles(
    p: &mut egui_plot::PlotUi,
    draw: &[Bar],
    hollow: bool,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    // `prev_close` walks the RAW close of the preceding drawn bar so the optional
    // "color bars based on previous close" mode can key each candle off it
    // (the first bar's `None` falls back to close>=open inside `candle_geom`).
    let mut prev_close: Option<f64> = None;
    for bar in draw {
        // Same Line (wick) + Polygon (body), same coords, same push order as before — now
        // sourced from the shared `candle_geom` so the GPU path stays byte-identical.
        // Wick, body border, and body fill each take their own up/down color (default-equal
        // to the body color, so this is pixel-identical until the new rows are edited).
        let g = candle_geom(bar, prev_close, hollow, map, opts);
        p.line(
            Line::new("", PlotPoints::from(vec![g.wick[0], g.wick[1]]))
                .color(g.wick_color)
                .width(1.0)
                .allow_hover(false),
        );
        let poly = Polygon::new("", PlotPoints::from(g.body.to_vec()))
            .stroke(Stroke::new(1.0, g.border_color))
            .fill_color(if g.hollow { Color32::TRANSPARENT } else { g.color })
            .allow_hover(false);
        p.polygon(poly);
        prev_close = Some(bar.c);
    }
}

/// One candle in **screen space** (egui points), OWNED and `Copy` so a `Vec<CandleInstance>`
/// is `Send + Sync` for a GPU vertex/instance buffer. Produced by [`GpuCandleItem::shapes`],
/// which maps every [`candle_geom`] corner through the plot's current-frame [`PlotTransform`].
/// `body_top`/`wick_top` are the SMALLER screen y (egui screen-y grows downward, so the
/// higher price is the smaller y); `filled == 0` marks a hollow-style bull body (unfilled),
/// `1` a solid one — the same predicate `draw_candles` feeds `fill_color`. The Task-2 GPU
/// layer in vike-app consumes these; vike-chart itself never draws from them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CandleInstance {
    pub x_lo: f32,
    pub x_hi: f32,
    pub body_top: f32,
    pub body_bot: f32,
    pub wick_top: f32,
    pub wick_bot: f32,
    pub color: [f32; 4],
    pub filled: u32,
}

/// Map ONE [`CandleGeom`] (data space) through a [`PlotTransform`] into a screen-space
/// [`CandleInstance`]. Kept a free fn (not inlined into `shapes`) so it needs no `egui::Ui`
/// and the parity test can pin it directly against `candle_geom`'s corners mapped through the
/// SAME transform — the whole point of the shared-geometry seam.
pub(crate) fn candle_instance(g: &CandleGeom, transform: &PlotTransform) -> CandleInstance {
    let at = |xy: [f64; 2]| transform.position_from_point(&PlotPoint::new(xy[0], xy[1]));
    let lo_left = at(g.body[0]); // [t-HALF_BODY, lo]  → screen bottom-left
    let lo_right = at(g.body[1]); // [t+HALF_BODY, lo] → screen bottom-right
    let hi_left = at(g.body[3]); // [t-HALF_BODY, hi]  → screen top-left
    let low = at(g.wick[0]); // [t, map(low)]
    let high = at(g.wick[1]); // [t, map(high)]
    let c = g.color;
    CandleInstance {
        x_lo: lo_left.x,
        x_hi: lo_right.x,
        body_top: hi_left.y,
        body_bot: lo_left.y,
        wick_top: high.y,
        wick_bot: low.y,
        color: [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            c.a() as f32 / 255.0,
        ],
        filled: u32::from(!g.hollow),
    }
}

/// A custom [`egui_plot::PlotItem`] that emits ONE opaque `Shape` (a GPU paint callback built
/// by vike-app's `build` hook) at the exact z-slot a candle `Polygon` occupies today: it is
/// pushed into `plot_ui.items` at the `draw_candles` call site, so it lands above the plot
/// background+grid and below whatever the closure pushes afterward (indicator overlays,
/// last-price line) and below the post-`.show()` crosshair — preserving today's stacking
/// (see the GPU codemap §3).
///
/// vike-chart never names any wgpu/egui_wgpu type: `build` returns an already-formed
/// `egui::Shape` (in practice a `Shape::Callback` wrapping an `Arc<dyn Any + Send + Sync>`),
/// and this item only threads screen-space [`CandleInstance`]s + the plot `Rect` into it.
/// `shapes()` receives a CURRENT-FRAME-fresh transform (codemap §3), so instances carry zero
/// staleness. Every other `PlotItem` method is inert: the price plot is fully manual-bounds
/// (`auto_bounds(false)`) and has no legend, so `bounds()` = `NOTHING`, `geometry()` =
/// `None`, `color()` = transparent, and the `base`-backed `name`/`id`/`highlight` defaults
/// never matter.
pub struct GpuCandleItem<'a> {
    /// Per-bar candle geometry in data space, computed EAGERLY at construction via the shared
    /// [`candle_geom`] (the exact source the egui painter uses → parity by construction).
    /// OWNED, not a borrow of the chart's per-frame `bars`/`map`: those are locals of the
    /// `plot.show` build closure, which does NOT outlive the `PlotUi<'a>` this item is stored
    /// in (`plot_ui.add` defers `shapes()` until after the closure returns). A stored
    /// `PlotItem` may only borrow data living as long as `'a`, and only `build` qualifies.
    geom: Vec<CandleGeom>,
    /// vike-app's per-frame hook turning screen-space instances + the plot `Rect` into the
    /// opaque GPU paint `Shape`. Borrowed with the chart-inputs lifetime `'a` (it comes from
    /// `ChartInputs::gpu_candles`, which outlives the `plot.show` closure) — the ONLY borrow
    /// the stored item holds.
    build: &'a dyn Fn(Vec<CandleInstance>, Rect) -> egui::Shape,
    // egui_plot bookkeeping the trait's `base()`/`base_mut()` return; carried so the default
    // name/id/highlight/allow_hover methods compile without a panic. Empty name / no legend.
    base: PlotItemBase,
}

impl<'a> GpuCandleItem<'a> {
    /// Build a `GpuCandleItem` from this frame's (LOD) bar slice. `bars`/`map`/`opts` are
    /// borrowed ONLY for the duration of this call — their [`candle_geom`] output is stored
    /// OWNED — so they may safely be the `plot.show` closure's short-lived locals; only
    /// `build` (lifetime `'a`, from `ChartInputs::gpu_candles`) is retained by reference.
    pub fn new(
        bars: &[Bar],
        hollow: bool,
        map: &dyn Fn(f64) -> f64,
        opts: &ChartOptions,
        build: &'a dyn Fn(Vec<CandleInstance>, Rect) -> egui::Shape,
    ) -> Self {
        // Track the preceding bar's raw close so the GPU path classifies bull/bear
        // identically to the egui painter under "color bars based on previous close".
        let mut prev_close: Option<f64> = None;
        let geom: Vec<CandleGeom> = bars
            .iter()
            .map(|bar| {
                let g = candle_geom(bar, prev_close, hollow, map, opts);
                prev_close = Some(bar.c);
                g
            })
            .collect();
        Self { geom, build, base: PlotItemBase::new(String::new()) }
    }
}

impl PlotItem for GpuCandleItem<'_> {
    fn shapes(&self, _ui: &egui::Ui, transform: &PlotTransform, shapes: &mut Vec<egui::Shape>) {
        // Each candle's screen geometry derives from the SAME `candle_geom` the egui painter
        // uses (captured at construction) → GPU/egui parity by construction (see the render.rs
        // parity test). The transform here is current-frame-fresh (codemap §3), so instances
        // carry zero staleness.
        let instances: Vec<CandleInstance> =
            self.geom.iter().map(|g| candle_instance(g, transform)).collect();
        shapes.push((self.build)(instances, *transform.frame()));
    }

    // Generated from x values only for function-plots — nothing to do for an explicit slice.
    fn initialize(&mut self, _x_range: std::ops::RangeInclusive<f64>) {}

    fn color(&self) -> Color32 {
        Color32::TRANSPARENT
    }

    fn geometry(&self) -> PlotGeometry<'_> {
        PlotGeometry::None
    }

    fn bounds(&self) -> PlotBounds {
        // The price plot is fully manual-bounds; this item never contributes to auto-fit.
        PlotBounds::NOTHING
    }

    fn base(&self) -> &PlotItemBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PlotItemBase {
        &mut self.base
    }
}

/// Candles whose body width is scaled by per-bar volume (vs the visible max).
///
/// `vmax` is the visible window's max volume, computed by the caller (chart.rs) via
/// [`crate::model::ChartState::visible_vol_max_shared`] (the CLOSED-bar cache)
/// `.max()`'d with the forming bar's volume when it's visible — matching what a
/// per-frame `visible_slice(bars, x0, x1).map(|b| b.v).fold(0.0, f64::max)` over
/// `bars` (closed + forming) used to compute inline here, but O(1) on a static frame
/// instead of O(visible) every frame (chart-perf T5).
pub(crate) fn draw_volume_candles(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
    vmax: f64,
) {
    // vmax/frac/hw are volume (x-axis-adjacent) quantities, not y-positions — unmapped.
    let vmax = vmax.max(1e-9);
    for bar in vis(bars, x0, x1) {
        let bull = bar.c >= bar.o; // RAW classification, see draw_candles
        let color = if bull { opts.up_col() } else { opts.down_col() };
        let (lo, hi) = if bull { (map(bar.o), map(bar.c)) } else { (map(bar.c), map(bar.o)) };
        let frac = (bar.v / vmax).clamp(0.0, 1.0).sqrt();
        let hw = 0.12 + frac * (0.42 - 0.12);
        p.line(
            Line::new("", PlotPoints::from(vec![[bar.t, map(bar.l)], [bar.t, map(bar.h)]]))
                .color(color)
                .width(1.0)
                .allow_hover(false),
        );
        let body = vec![[bar.t - hw, lo], [bar.t + hw, lo], [bar.t + hw, hi], [bar.t - hw, hi]];
        p.polygon(
            Polygon::new("", PlotPoints::from(body))
                .fill_color(color)
                .stroke(Stroke::new(1.0, color))
                .allow_hover(false),
        );
    }
}

pub(crate) fn draw_bars(
    p: &mut egui_plot::PlotUi,
    draw: &[Bar],
    hlc: bool,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    const TICK: f64 = 0.32;
    for bar in draw {
        let color = if bar.c >= bar.o { opts.up_col() } else { opts.down_col() }; // RAW classification
        p.line(
            Line::new("", PlotPoints::from(vec![[bar.t, map(bar.l)], [bar.t, map(bar.h)]]))
                .color(color)
                .width(1.0)
                .allow_hover(false),
        );
        if !hlc {
            p.line(
                Line::new(
                    "",
                    PlotPoints::from(vec![[bar.t - TICK, map(bar.o)], [bar.t, map(bar.o)]]),
                )
                .color(color)
                .width(1.0)
                .allow_hover(false),
            );
        }
        p.line(
            Line::new("", PlotPoints::from(vec![[bar.t, map(bar.c)], [bar.t + TICK, map(bar.c)]]))
                .color(color)
                .width(1.0)
                .allow_hover(false),
        );
    }
}

#[allow(clippy::too_many_arguments)] // one closure param added for T2 mapping tipped this over 7; each param is a distinct seam (style flags, cached floor, map)
pub(crate) fn draw_line(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    area: bool,
    markers: bool,
    y_floor: f64, // RAW series min low, precomputed by the caller (cached — no per-frame fold)
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    let pts: Vec<[f64; 2]> = vis(bars, x0, x1).map(|b| [b.t, map(b.c)]).collect();
    if pts.len() < 2 {
        return;
    }
    let line = opts.line_col();
    if area {
        let y_floor = map(y_floor);
        // TradingView-style GRADIENT fill: instead of one flat trapezoid per
        // segment, stack K bands from the line down to the floor with the alpha
        // fading to transparent — dense at the line, gone at the bottom. Each
        // band stays a convex quad (per-segment), so no non-convex fan artifact.
        const K: usize = 6;
        const TOP_A: f64 = 55.0; // alpha at the line
        for w in pts.windows(2) {
            let (a0, a1) = (w[0], w[1]);
            let lerp = |ly: f64, f: f64| ly + (y_floor - ly) * f;
            for k in 0..K {
                let (f0, f1) = (k as f64 / K as f64, (k + 1) as f64 / K as f64);
                let quad = vec![
                    [a0[0], lerp(a0[1], f0)],
                    [a1[0], lerp(a1[1], f0)],
                    [a1[0], lerp(a1[1], f1)],
                    [a0[0], lerp(a0[1], f1)],
                ];
                let alpha = (TOP_A * (1.0 - f0)).round() as u8;
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad))
                        .fill_color(rgba(opts.line, alpha))
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
        }
    }
    p.line(Line::new("", PlotPoints::from(pts.clone())).color(line).width(1.5).allow_hover(false));
    if markers {
        p.points(Points::new("", PlotPoints::from(pts)).radius(2.0).color(line).filled(true));
    }
}

pub(crate) fn draw_step(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    let mut pts = Vec::new();
    for b in vis(bars, x0, x1) {
        let c = map(b.c);
        pts.push([b.t - 0.5, c]);
        pts.push([b.t + 0.5, c]);
    }
    if pts.len() >= 2 {
        p.line(
            Line::new("", PlotPoints::from(pts))
                .color(opts.line_col())
                .width(1.5)
                .allow_hover(false),
        );
    }
}

/// Draw a vike-style mini icon for a chart style into `r` (menu rows + title-bar brand).
pub fn draw_style_icon(p: &egui::Painter, r: Rect, style: ChartStyle) {
    use ChartStyle::*;
    let up = Color32::from_rgb(91, 190, 145);
    let dn = Color32::from_rgb(217, 84, 88);
    let ln = Color32::from_rgb(168, 176, 188);
    let pt = |nx: f32, ny: f32| egui::pos2(r.left() + nx * r.width(), r.top() + ny * r.height());
    let vline = |x: f32, y0: f32, y1: f32, col: Color32, w: f32| {
        p.vline(pt(x, 0.0).x, pt(0.0, y0).y..=pt(0.0, y1).y, Stroke::new(w, col));
    };
    let hline = |x0: f32, x1: f32, y: f32, col: Color32, w: f32| {
        p.hline(pt(x0, 0.0).x..=pt(x1, 0.0).x, pt(0.0, y).y, Stroke::new(w, col));
    };
    let body = |xc: f32, y0: f32, y1: f32, col: Color32, hollow: bool| {
        let rr = Rect::from_min_max(pt(xc - 0.12, y0), pt(xc + 0.12, y1));
        if hollow {
            p.rect_stroke(rr, 0.0, Stroke::new(1.2, col), egui::StrokeKind::Middle);
        } else {
            p.rect_filled(rr, 0.0, col);
        }
    };
    let line = |pts: Vec<egui::Pos2>, col: Color32, w: f32| {
        p.add(egui::Shape::line(pts, Stroke::new(w, col)));
    };
    match style {
        Candles | HeikinAshi | VolumeCandles => {
            vline(0.33, 0.06, 0.94, up, 1.1);
            body(0.33, 0.26, 0.64, up, false);
            vline(0.67, 0.2, 0.84, dn, 1.1);
            body(0.67, 0.36, 0.74, dn, false);
        }
        Hollow => {
            vline(0.33, 0.06, 0.94, up, 1.1);
            body(0.33, 0.26, 0.64, up, true);
            vline(0.67, 0.2, 0.84, dn, 1.1);
            body(0.67, 0.36, 0.74, dn, true);
        }
        Bars => {
            vline(0.33, 0.1, 0.9, up, 1.2);
            hline(0.16, 0.33, 0.34, up, 1.2);
            hline(0.33, 0.5, 0.62, up, 1.2);
            vline(0.7, 0.18, 0.82, dn, 1.2);
            hline(0.53, 0.7, 0.42, dn, 1.2);
            hline(0.7, 0.87, 0.7, dn, 1.2);
        }
        HlcBars => {
            vline(0.33, 0.1, 0.9, up, 1.2);
            hline(0.33, 0.52, 0.62, up, 1.2);
            vline(0.7, 0.18, 0.82, dn, 1.2);
            hline(0.7, 0.89, 0.7, dn, 1.2);
        }
        HighLow => {
            vline(0.34, 0.1, 0.9, ln, 1.4);
            vline(0.66, 0.22, 0.8, ln, 1.4);
        }
        Line | LineMarkers => {
            let path = vec![pt(0.08, 0.72), pt(0.34, 0.36), pt(0.56, 0.6), pt(0.92, 0.22)];
            line(path.clone(), ln, 1.5);
            if matches!(style, LineMarkers) {
                for q in &path {
                    p.circle_filled(*q, 1.5, ln);
                }
            }
        }
        StepLine => {
            line(
                vec![
                    pt(0.08, 0.66),
                    pt(0.35, 0.66),
                    pt(0.35, 0.4),
                    pt(0.62, 0.4),
                    pt(0.62, 0.64),
                    pt(0.92, 0.64),
                ],
                ln,
                1.5,
            );
        }
        Area | HlcArea => {
            let poly = vec![
                pt(0.08, 0.92),
                pt(0.08, 0.62),
                pt(0.4, 0.34),
                pt(0.64, 0.56),
                pt(0.92, 0.3),
                pt(0.92, 0.92),
            ];
            p.add(egui::Shape::convex_polygon(poly, up.gamma_multiply(0.35), Stroke::new(1.3, up)));
        }
        Baseline => {
            hline(0.08, 0.92, 0.52, Color32::from_gray(110), 1.0);
            line(vec![pt(0.08, 0.66), pt(0.4, 0.34), pt(0.62, 0.6), pt(0.92, 0.42)], up, 1.4);
        }
        Columns => {
            for (x, col) in [(0.22, up), (0.42, dn), (0.62, up), (0.82, dn)] {
                let h = if col == up { 0.32 } else { 0.5 };
                p.rect_filled(Rect::from_min_max(pt(x - 0.07, h), pt(x + 0.07, 0.9)), 0.0, col);
            }
        }
        Renko | Range | LineBreak => {
            p.rect_filled(Rect::from_min_max(pt(0.12, 0.5), pt(0.42, 0.72)), 0.0, up);
            p.rect_filled(Rect::from_min_max(pt(0.4, 0.28), pt(0.7, 0.5)), 0.0, up);
            p.rect_filled(Rect::from_min_max(pt(0.56, 0.56), pt(0.86, 0.78)), 0.0, dn);
        }
        Kagi => {
            line(
                vec![
                    pt(0.14, 0.72),
                    pt(0.14, 0.32),
                    pt(0.46, 0.32),
                    pt(0.46, 0.62),
                    pt(0.78, 0.62),
                    pt(0.78, 0.26),
                ],
                ln,
                1.6,
            );
        }
        PointFigure => {
            line(vec![pt(0.14, 0.3), pt(0.42, 0.62)], up, 1.4);
            line(vec![pt(0.42, 0.3), pt(0.14, 0.62)], up, 1.4);
            p.circle_stroke(pt(0.68, 0.46), 0.16 * r.height(), Stroke::new(1.4, dn));
        }
        Footprint => {
            // 2-column x 3-row cell grid (sell left / buy right per row) — a miniature
            // footprint ladder, visually distinct from Columns' single-column bars.
            for row in 0..3 {
                let y0 = 0.08 + row as f32 * 0.3;
                let y1 = y0 + 0.22;
                p.rect_filled(Rect::from_min_max(pt(0.08, y0), pt(0.47, y1)), 0.0, dn);
                p.rect_filled(Rect::from_min_max(pt(0.53, y0), pt(0.92, y1)), 0.0, up);
            }
        }
    }
}

pub(crate) fn draw_baseline(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    // vike baseline: green fill+line above the anchor, red below.
    let base_raw = bars.first().map(|b| b.c).unwrap_or(0.0);
    let base = map(base_raw);
    // (t, RAW close, MAPPED close) — the above/below classification vs the
    // baseline anchor stays RAW (semantic, scale-mode-invariant, same
    // reasoning as the candle bull/bear color); only the plotted points map.
    let pts: Vec<(f64, f64, f64)> = vis(bars, x0, x1).map(|b| (b.t, b.c, map(b.c))).collect();
    let up_fill = rgba(opts.up_s, 28); // semantic-up @ subtle alpha
    let dn_fill = rgba(opts.down_s, 28); // semantic-down @ subtle alpha
    for w in pts.windows(2) {
        let above = (w[0].1 + w[1].1) / 2.0 >= base_raw;
        let (col, fill) =
            if above { (opts.up_s_col(), up_fill) } else { (opts.down_s_col(), dn_fill) };
        let p0 = [w[0].0, w[0].2];
        let p1 = [w[1].0, w[1].2];
        let quad = vec![p0, p1, [p1[0], base], [p0[0], base]];
        p.polygon(
            Polygon::new("", PlotPoints::from(quad))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
        p.line(
            Line::new("", PlotPoints::from(vec![p0, p1])).color(col).width(1.5).allow_hover(false),
        );
    }
    p.hline(
        HLine::new("", base)
            .color(Color32::from_gray(90))
            .width(1.0)
            .style(egui_plot::LineStyle::dashed_loose())
            .allow_hover(false),
    );
}

pub(crate) fn draw_hlc_area(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    let v: Vec<&Bar> = vis(bars, x0, x1).collect();
    if v.len() < 2 {
        return;
    }
    let fill = rgba(opts.line, 30); // line color @ subtle alpha
                                    // per-segment band quads (high→low) — convex, so no fan artifact. h and l
                                    // are mapped INDEPENDENTLY (same principle as PnF's box edges): they are
                                    // two distinct raw values, not a single height to be scaled.
    for w in v.windows(2) {
        let quad = vec![
            [w[0].t, map(w[0].h)],
            [w[1].t, map(w[1].h)],
            [w[1].t, map(w[1].l)],
            [w[0].t, map(w[0].l)],
        ];
        p.polygon(
            Polygon::new("", PlotPoints::from(quad))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
    }
    let cpts: Vec<[f64; 2]> = v.iter().map(|b| [b.t, map(b.c)]).collect();
    p.line(
        Line::new("", PlotPoints::from(cpts)).color(opts.line_col()).width(1.2).allow_hover(false),
    );
}

pub(crate) fn draw_columns(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    ymin: f64, // RAW floor, precomputed by the caller
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    let ymin = map(ymin);
    for b in vis(bars, x0, x1) {
        let col = if b.c >= b.o { opts.up_col() } else { opts.down_col() }; // RAW classification
        let fill = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 150);
        let c = map(b.c);
        let body = vec![[b.t - 0.32, ymin], [b.t + 0.32, ymin], [b.t + 0.32, c], [b.t - 0.32, c]];
        p.polygon(
            Polygon::new("", PlotPoints::from(body))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
    }
}

/// Kagi line — connected vertices, thick (yang/UP) or thin (yin/DOWN) per segment.
/// Each vertex price is mapped individually (chart-UX bundle T2 §1).
pub(crate) fn draw_kagi(
    p: &mut egui_plot::PlotUi,
    k: &transforms::Kagi,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    let n = k.prices.len();
    if n < 2 {
        return;
    }
    for i in 0..n - 1 {
        let thick = k.thick.get(i).copied().unwrap_or(false);
        let col = if thick { opts.up_col() } else { opts.down_col() };
        let w = if thick { 2.4 } else { 1.2 };
        let y0 = map(k.prices[i]);
        let y1 = map(k.prices[i + 1]);
        // step: horizontal at the current level, then vertical to the next vertex
        let path = vec![[i as f64, y0], [(i + 1) as f64, y0], [(i + 1) as f64, y1]];
        p.line(Line::new("", PlotPoints::from(path)).color(col).width(w).allow_hover(false));
    }
}

/// Point & Figure — stacked X (up) / O (down) boxes per column. Each box's
/// bottom (`y`) and top (`y + box_`) edges are mapped INDEPENDENTLY (chart-UX
/// bundle T2 §1) — mapped box heights become non-uniform (e.g. compressed at
/// higher log-scale prices), matching TV's log-scale PnF rendering.
///
/// The O-column ellipse ring is traced with `libm::cos`/`libm::sin` rather than the `f64` METHODS.
/// IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `sin` or
/// `cos`, so the methods reach the platform's libm — glibc on the CI runners, the MSVC runtime on
/// the Windows dev box — which are each entitled to their own last bit; the `libm` crate's
/// pure-Rust FDLIBM answers identically everywhere. Fifteen ring vertices at fixed angles is about
/// as benign as a trig site gets, so this is hygiene, not a known defect.
///
/// ⚠ It is, however, the one libm consumer in this crate that NO golden covers.
/// `crates/vike-chart/tests/tessellation_goldens.rs` renders sixteen scenarios
/// (candles/line/HeikinAshi/Renko across the scale and chrome variants) and not one of them is
/// PointFigure, so `a_one_ulp_price_perturbation_leaves_every_record_unchanged` — the suite's
/// direct statement that a libm difference cannot rebaseline it — says nothing whatsoever about
/// this ring. Adding a PnF scenario is the way to change that; until then the conversion is the
/// only thing standing between this path and a per-platform pixel difference.
pub(crate) fn draw_pnf(
    p: &mut egui_plot::PlotUi,
    cols: &[transforms::PnFColumn],
    box_: f64,
    map: &dyn Fn(f64) -> f64,
    opts: &ChartOptions,
) {
    if box_ <= 0.0 {
        return;
    }
    let hw = 0.34;
    for (i, col) in cols.iter().enumerate() {
        let x = i as f64;
        let color = if col.up { opts.up_col() } else { opts.down_col() };
        let nboxes = (((col.top - col.bottom) / box_).round() as i64).max(1);
        for b in 0..nboxes {
            let y_raw = col.bottom + b as f64 * box_;
            let y = map(y_raw);
            let y_top = map(y_raw + box_);
            if col.up {
                p.line(
                    Line::new("", PlotPoints::from(vec![[x - hw, y], [x + hw, y_top]]))
                        .color(color)
                        .width(1.4)
                        .allow_hover(false),
                );
                p.line(
                    Line::new("", PlotPoints::from(vec![[x - hw, y_top], [x + hw, y]]))
                        .color(color)
                        .width(1.4)
                        .allow_hover(false),
                );
            } else {
                // ellipse spanning the MAPPED [y, y_top] band — half-height from
                // the mapped difference, not the raw box_ (non-uniform per box).
                let yc = (y + y_top) / 2.0;
                let half_h = (y_top - y).abs() / 2.0;
                let ring: Vec<[f64; 2]> = (0..=14)
                    .map(|kk| {
                        let a = kk as f64 / 14.0 * std::f64::consts::TAU;
                        [x + hw * libm::cos(a), yc + half_h * libm::sin(a)]
                    })
                    .collect();
                p.line(
                    Line::new("", PlotPoints::from(ring))
                        .color(color)
                        .width(1.2)
                        .allow_hover(false),
                );
            }
        }
    }
}

/// Footprint (SP2, T5): per bar, a column of per-price cells (sell|buy volume). Each cell is a
/// faint imbalance-tinted rect `[x±CELL_HALF_W, price±tick_size/2]`; the bar's point-of-control
/// cell (max buy+sell volume WITHIN that bar) gets an amber outline; buy/sell numbers are drawn
/// in plot space when `cell_px` clears the text-legibility gate (`orderflow::cell_text_legible`)
/// — below that the tinted rect alone carries the imbalance read (no delta-bar fallback: the
/// tint already IS the delta signal, unlike the brief's original two-tier sketch).
///
/// `bars`/`fps` are index-aligned 1:1 (`ChartInputs::footprint`'s contract) — the `zip` stops at
/// the shorter of the two, so a length mismatch degrades gracefully instead of panicking.
/// `tick_size`/`cell_px` are resolved by the caller (`chart::draw`, via `orderflow_tick_size`/
/// `cell_px_for`) so this style and the volume-profile overlay always agree on bucket width.
/// `i0`/`i1` (inclusive) are the visible bar-index bounds — SP3 Task B #3 (SP2 final-review
/// finding B): this used to iterate ALL of `bars`/`fps` regardless of what was on screen, which
/// went O(full history) per frame the moment backfill filled history. Callers pass the SAME
/// `(i0, i1)` `chart::orderflow_index_bounds` computes for the price closure's visible x-range —
/// shared with the volume-profile overlay, so both bucket an identical window. Clamped
/// independently against `bars.len()`/`fps.len()` here (not trusted from the caller): `fps` can
/// be shorter than `bars` (the aggregator hasn't caught up to a just-opened forming bar yet), so
/// a stale/out-of-range `i1` must degrade to "draw nothing" rather than panic the slice index.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_footprint(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    fps: &[vike_orderflow::FootprintBar],
    map: &dyn Fn(f64) -> f64,
    tick_size: f64,
    opts: &ChartOptions,
    cell_px: f32,
    i0: usize,
    i1: usize,
) {
    let legible = crate::orderflow::cell_text_legible(cell_px);
    let buy_fill = rgba(opts.up_s, CELL_FILL_ALPHA);
    let sell_fill = rgba(opts.down_s, CELL_FILL_ALPHA);
    let neutral_fill = rgba(opts.cross, CELL_FILL_ALPHA / 2);
    let end = i1.saturating_add(1).min(bars.len()).min(fps.len());
    let start = i0.min(end);
    for (bar, fp) in bars[start..end].iter().zip(fps[start..end].iter()) {
        let x = bar.t;
        // Per-bar POC: the max-total cell in THIS bar (distinct from the volume-profile
        // overlay's whole-visible-range POC line) — empty `cells` (a bar with no trades)
        // safely yields `None` via `max_by`, never an unwrap.
        let poc = fp
            .cells
            .iter()
            .max_by(|a, b| (a.buy_vol + a.sell_vol).total_cmp(&(b.buy_vol + b.sell_vol)))
            .map(|c| c.price);
        for c in &fp.cells {
            let y0 = map(c.price - tick_size / 2.0);
            let y1 = map(c.price + tick_size / 2.0);
            let imb = c.buy_vol - c.sell_vol;
            let bg = if imb > 0.0 {
                buy_fill
            } else if imb < 0.0 {
                sell_fill
            } else {
                neutral_fill
            };
            p.polygon(
                Polygon::new(
                    "",
                    PlotPoints::from(vec![
                        [x - CELL_HALF_W, y0],
                        [x + CELL_HALF_W, y0],
                        [x + CELL_HALF_W, y1],
                        [x - CELL_HALF_W, y1],
                    ]),
                )
                .fill_color(bg)
                .stroke(Stroke::NONE)
                .allow_hover(false),
            );
            if Some(c.price) == poc {
                p.polygon(
                    Polygon::new(
                        "",
                        PlotPoints::from(vec![
                            [x - CELL_HALF_W, y0],
                            [x + CELL_HALF_W, y0],
                            [x + CELL_HALF_W, y1],
                            [x - CELL_HALF_W, y1],
                        ]),
                    )
                    .fill_color(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, AMBER))
                    .allow_hover(false),
                );
            }
            if legible {
                let yc = (y0 + y1) / 2.0;
                p.text(
                    Text::new("", PlotPoint::new(x - 0.05, yc), format!("{:.0}", c.sell_vol))
                        .color(opts.down_s_col())
                        .anchor(Align2::RIGHT_CENTER),
                );
                p.text(
                    Text::new("", PlotPoint::new(x + 0.05, yc), format!("{:.0}", c.buy_vol))
                        .color(opts.up_s_col())
                        .anchor(Align2::LEFT_CENTER),
                );
            }
        }
    }
}

/// Split `vals` (index-aligned to bars starting at `base`; NaN = warm-up)
/// into contiguous runs and draw one line per run, so gaps aren't connected.
/// `base` is the absolute bar index of `vals[0]` — 0 for a full series
/// (`render_oscillator`, whose panes are NOT mapped/sliced), or the sliced
/// window's start index (`render_overlay`, chart-perf T4) so `vals` can be a
/// visible-only sub-slice while still drawing at the correct absolute x.
/// Map a per-plot [`LineDash`] onto an `egui_plot::LineStyle` (TradingView "Style"
/// line-style picker). `Solid` → the default `LineStyle::Solid` (pixel-identical to
/// the pre-field render path), `Dashed`/`Dotted` → the same loose-dash / dense-dot
/// presets the oscillator reference bands already use.
pub(crate) fn dash_style(d: LineDash) -> egui_plot::LineStyle {
    match d {
        LineDash::Solid => egui_plot::LineStyle::Solid,
        LineDash::Dashed => egui_plot::LineStyle::dashed_loose(),
        LineDash::Dotted => egui_plot::LineStyle::dotted_dense(),
    }
}

#[allow(clippy::too_many_arguments)] // +1 arg for the per-plot dash style (T8 depth)
pub(crate) fn seg_line(
    p: &mut egui_plot::PlotUi,
    vals: &[f64],
    color: Color32,
    width: f32,
    dash: egui_plot::LineStyle,
    x0: f64,
    x1: f64,
    base: usize,
) {
    let mut run: Vec<[f64; 2]> = Vec::new();
    for (i, &v) in vals.iter().enumerate() {
        let x = (base + i) as f64;
        if v.is_nan() || x < x0 - 2.0 || x > x1 + 2.0 {
            if run.len() >= 2 {
                p.line(
                    Line::new("", PlotPoints::from(std::mem::take(&mut run)))
                        .color(color)
                        .width(width)
                        .style(dash)
                        .allow_hover(false),
                );
            } else {
                run.clear();
            }
        } else {
            run.push([x, v]);
        }
    }
    if run.len() >= 2 {
        p.line(
            Line::new("", PlotPoints::from(run))
                .color(color)
                .width(width)
                .style(dash)
                .allow_hover(false),
        );
    }
}

/// Inclusive-start/exclusive-end index window of an N-length overlay series
/// that can be visible in `[x0, x1]`, under the SAME ±2-bar guard `seg_line`/
/// the Dots arm already filter by (`x < x0 - 2.0 || x > x1 + 2.0` is
/// discarded — kept points satisfy `x0 - 2.0 <= x <= x1 + 2.0`), clamped to
/// `[0, n]`. `lo` is the smallest integer index that can pass the guard
/// (`floor(x0 - 2.0)`); `hi` is one past the largest integer index that can
/// pass it (`floor(x1 + 2.0) + 1`) — a conservative superset is fine: the
/// per-point `>= x0 - 2.0 && <= x1 + 2.0` filters in `render_overlay`'s Dots
/// arm and in `seg_line` still run on the sliced window, so pixel output is
/// unchanged (chart-perf T4).
pub(crate) fn overlay_visible_range(n: usize, x0: f64, x1: f64) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let lo = (x0 - 2.0).floor().clamp(0.0, n as f64) as usize;
    let hi_f = (x1 + 2.0).floor() + 1.0;
    let hi = hi_f.clamp(0.0, n as f64) as usize;
    (lo, hi.max(lo))
}

/// Overlay indicator lines (moving averages, bands, …) live on the PRICE
/// scale, so every output value is mapped — via a pre-mapped copy of the
/// VISIBLE window only (chart-perf T4: `overlay_visible_range` — the old code
/// mapped+allocated the whole `line.series` every frame; `Dots`/`seg_line`
/// then filtered to `[x0, x1]` anyway), so the shared `seg_line`/Dots code
/// (also used unmapped by `render_oscillator`, whose panes are NOT mapped,
/// own value domains) stays untouched. Consumers are offset by `lo` so the
/// sliced sub-series still draws at the same absolute x positions as before.
/// Split a candlestick pattern's per-bar SIGNAL series (`+100` bullish / `-100` bearish /
/// `0` none) into bull/bear marker positions, each anchored to the flagged bar's own
/// extreme — bullish at its low, bearish at its high. Anchoring to the bar (rather than
/// plotting the raw ±100) is what keeps the glyph on the candle in every price scale:
/// `map` is applied to a real price, so log/percent modes land correctly. `0` (no pattern)
/// and NaN (warm-up) are not markers. Pure — no egui — so placement is unit-testable;
/// [`render_overlay`] only paints the result.
pub(crate) fn pattern_marker_points(
    series: &[f64],
    bars: &[Bar],
    lo: usize,
    hi: usize,
    map: &dyn Fn(f64) -> f64,
) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    let (mut bull, mut bear) = (Vec::new(), Vec::new());
    let end = hi.min(series.len());
    for (i, &v) in series.iter().enumerate().take(end).skip(lo) {
        if !v.is_finite() || v == 0.0 {
            continue;
        }
        let Some(b) = bars.get(i) else { continue };
        if v > 0.0 {
            bull.push([i as f64, map(b.l)]);
        } else {
            bear.push([i as f64, map(b.h)]);
        }
    }
    (bull, bear)
}

/// Fold the visible price-pane overlays' series into the autofit extent `(lo, hi)`.
///
/// [`Category::Pattern`] series are per-bar SIGNALS (`+100`/`-100`/`0`), NOT prices, so they
/// are excluded: folding them drags the price axis down to ~0 and squashes real candles into
/// a strip at the top of the pane. Their markers anchor to bar extremes (see
/// [`pattern_marker_points`]), which the caller's own bar extent already covers — so skipping
/// them loses no visible geometry. Every other overlay (MAs, bands, VWAP…) is a real price
/// series and still folds in. Pure, so the exclusion is unit-testable.
pub(crate) fn fold_overlay_extent(
    indicators: &[Active],
    i0: usize,
    take: usize,
    lo: f64,
    hi: f64,
) -> (f64, f64) {
    let (mut lo, mut hi) = (lo, hi);
    for a in indicators
        .iter()
        .filter(|a| a.visible && a.is_overlay() && a.spec.category != Category::Pattern)
    {
        for line in &a.outputs {
            for v in line.series.iter().skip(i0).take(take) {
                if !v.is_nan() {
                    lo = lo.min(*v);
                    hi = hi.max(*v);
                }
            }
        }
    }
    (lo, hi)
}

pub(crate) fn render_overlay(
    p: &mut egui_plot::PlotUi,
    a: &Active,
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    bars: &[Bar],
) {
    for line in &a.outputs {
        if !line.visible {
            continue; // per-plot show/hide (T8 Style tab)
        }
        let (lo, hi) = overlay_visible_range(line.series.len(), x0, x1);
        // Candlestick patterns emit a per-bar SIGNAL (+100/-100/0), not a price — mapping
        // that onto the price axis draws a meaningless line pinned near zero (the whole
        // `Pattern` catalogue rendered as garbage before this arm existed). Anchor a glyph
        // to the flagged bar instead. Structure markers (zigzag / williams_fractal) DO
        // carry prices, so they keep the price-space path below.
        if a.spec.category == Category::Pattern {
            let (bull, bear) = pattern_marker_points(&line.series, bars, lo, hi, map);
            for (pts, shape) in [(bull, MarkerShape::Up), (bear, MarkerShape::Down)] {
                if !pts.is_empty() {
                    p.points(
                        Points::new("", PlotPoints::from(pts))
                            .radius(4.0)
                            .color(line.color)
                            .filled(true)
                            .shape(shape),
                    );
                }
            }
            continue;
        }
        // NaN (warm-up) needs no special-casing: map(NaN) is NaN in every mode
        // (identity / log10 / percent arithmetic all propagate NaN), and the
        // consumers below already skip NaN points.
        let mapped: Vec<f64> = line.series[lo..hi].iter().map(|&v| map(v)).collect();
        match line.style {
            OutputStyle::Dots => {
                let pts: Vec<[f64; 2]> = mapped
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| {
                        !v.is_nan()
                            && ((lo + *i) as f64) >= x0 - 2.0
                            && ((lo + *i) as f64) <= x1 + 2.0
                    })
                    .map(|(i, &v)| [(lo + i) as f64, v])
                    .collect();
                if !pts.is_empty() {
                    p.points(
                        Points::new("", PlotPoints::from(pts))
                            .radius(2.0)
                            .color(line.color)
                            .filled(true),
                    );
                }
            }
            // Line/Band both stroke at the line's own width (T8): Band's default
            // stays 1.0 and Line's 1.5, so this is pixel-identical until edited.
            // `line_style` (Solid default) drives the dash pattern (T8 depth).
            _ => seg_line(
                p,
                &mapped,
                line.color,
                line.width,
                dash_style(line.line_style),
                x0,
                x1,
                lo,
            ),
        }
    }
}

pub(crate) fn render_oscillator(
    p: &mut egui_plot::PlotUi,
    a: &Active,
    x0: f64,
    x1: f64,
    opts: &ChartOptions,
) {
    render_oscillator_parts(
        p,
        OscPaint {
            show_bands: a.show_bands,
            show_ob_os_fill: a.show_ob_os_fill,
            bands: &a.bands,
            ob_fill: a.ob_fill,
            os_fill: a.os_fill,
            outputs: &a.outputs,
        },
        x0,
        x1,
        opts,
    );
}

/// The oscillator-pane paint surface, borrowed from whatever owns it: an indicator
/// [`Active`] or a tick-driven [`crate::studies::ActiveStudy`]. Both carry the same
/// bar-indexed `OutputLine` series + band/fill state, so both render through the ONE
/// [`render_oscillator_parts`] body below — a study pane is pixel-identical in
/// machinery to an indicator pane.
#[derive(Clone, Copy)]
pub(crate) struct OscPaint<'a> {
    pub show_bands: bool,
    pub show_ob_os_fill: bool,
    pub bands: &'a [crate::indicators::BandLevel],
    pub ob_fill: egui::Color32,
    pub os_fill: egui::Color32,
    pub outputs: &'a [crate::indicators::OutputLine],
}

/// Draw one oscillator sub-pane's bands, overbought/oversold fills and output lines.
/// This is `render_oscillator`'s former body verbatim, reading through [`OscPaint`]
/// instead of `&Active` — byte-identical render for the indicator path.
pub(crate) fn render_oscillator_parts(
    p: &mut egui_plot::PlotUi,
    a: OscPaint<'_>,
    x0: f64,
    x1: f64,
    opts: &ChartOptions,
) {
    if a.show_bands {
        // Overbought/oversold translucent fills (TradingView RSI "Style" parity):
        // green above the highest band level, red below the lowest. Drawn FIRST so
        // the band hlines + oscillator series paint on top. Off by default
        // (`show_ob_os_fill`), so the pane is pixel-identical until enabled. The
        // pane's y-bounds were set by `subpanes.rs`; we fill within them (clipped to
        // the plot) across the full visible x-range.
        if a.show_ob_os_fill && !a.bands.is_empty() {
            let bounds = p.plot_bounds();
            let (xmin, xmax) = (bounds.min()[0], bounds.max()[0]);
            let (ymin, ymax) = (bounds.min()[1], bounds.max()[1]);
            let top = a.bands.iter().map(|b| b.value).fold(f64::NEG_INFINITY, f64::max);
            let bottom = a.bands.iter().map(|b| b.value).fold(f64::INFINITY, f64::min);
            let quad = |lo: f64, hi: f64| vec![[xmin, lo], [xmax, lo], [xmax, hi], [xmin, hi]];
            if top < ymax {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(top, ymax)))
                        .fill_color(a.ob_fill)
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
            if bottom > ymin {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(ymin, bottom)))
                        .fill_color(a.os_fill)
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
        }
        // Per-level band hlines (TradingView RSI "Style"): each level draws in its
        // OWN editable colour + show toggle; the default gray + shown keeps these
        // hlines pixel-identical to the pre-per-level single-`band_color` path.
        for band in a.bands {
            if !band.show {
                continue;
            }
            p.hline(
                HLine::new("", band.value)
                    .color(band.color)
                    .width(1.0)
                    .style(egui_plot::LineStyle::dashed_loose())
                    .allow_hover(false),
            );
        }
    }
    for line in a.outputs {
        if !line.visible {
            continue; // per-plot show/hide (T8 Style tab)
        }
        match line.style {
            OutputStyle::Histogram => {
                let vals = &line.series;
                let bars: Vec<egui_plot::Bar> = vals
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| {
                        !v.is_nan() && (*i as f64) >= x0 - 2.0 && (*i as f64) <= x1 + 2.0
                    })
                    .map(|(i, &v)| {
                        let rising = i > 0 && !vals[i - 1].is_nan() && v > vals[i - 1];
                        let col = if rising { opts.up_s_col() } else { opts.down_s_col() };
                        egui_plot::Bar::new(i as f64, v).fill(col).width(0.8)
                    })
                    .collect();
                if !bars.is_empty() {
                    p.bar_chart(egui_plot::BarChart::new("", bars));
                }
            }
            // Full series, unsliced (oscillator panes are NOT windowed by T4 — own value
            // domains, x0/x1 filtering happens inside seg_line itself): base = 0.
            _ => seg_line(
                p,
                &line.series,
                line.color,
                line.width,
                dash_style(line.line_style),
                x0,
                x1,
                0,
            ),
        }
    }
}

/// Draw one tick-driven microstructure study ([`crate::studies::ActiveStudy`]) into an
/// oscillator sub-pane, through the SAME [`render_oscillator_parts`] body indicator
/// oscillators use — the study's series is already bar-indexed by
/// `ActiveStudy::sync` (see `studies.rs`'s tick->bar mapping contract), so nothing here
/// is study-specific. A gated study (`ActiveStudy::is_empty` — a book study with no L2
/// feed) has an EMPTY series, so this paints only the reference bands: no fabricated
/// values ever reach the pane.
pub fn render_study(
    p: &mut egui_plot::PlotUi,
    s: &crate::studies::ActiveStudy,
    x0: f64,
    x1: f64,
    opts: &ChartOptions,
) {
    render_oscillator_parts(
        p,
        OscPaint {
            show_bands: s.show_bands,
            show_ob_os_fill: s.show_ob_os_fill,
            bands: &s.bands,
            ob_fill: s.ob_fill,
            os_fill: s.os_fill,
            outputs: &s.outputs,
        },
        x0,
        x1,
        opts,
    );
}

/// For each PRIMARY bar index in `[lo, hi)`, find the overlay bar whose open-time
/// is nearest-at-or-before that primary bar's `ot`, returning that overlay bar's
/// index (or None where the overlay has no bar at/before that primary time — a
/// leading gap). Both slices are open-time-ascending. This maps a 2nd symbol onto
/// the primary's positional x-index. Reuses sync::nearest_index_by_ts's ascending-scan
/// technique, not its nearest-rounding — this is strict floor/at-or-before.
pub fn reindex_by_ot(primary: &[Bar], overlay: &[Bar], lo: usize, hi: usize) -> Vec<Option<usize>> {
    let lo = lo.min(primary.len());
    let hi = hi.min(primary.len());
    if lo >= hi {
        return Vec::new();
    }
    if overlay.is_empty() {
        return vec![None; hi - lo];
    }
    let mut out = Vec::with_capacity(hi - lo);
    // Forward two-pointer walk: both `primary[lo..hi]` and `overlay` are ot-ascending, so `j`
    // only ever advances — O(hi - lo + overlay.len()) total, no per-bar binary search. This is
    // a strict floor/asof lookup (unlike sync::nearest_index_by_ts, which can round UP to a
    // closer LATER bar): an overlay candle from the future must never get mapped onto a past
    // primary bar, so `j` only advances while the NEXT overlay bar is still <= the primary ot.
    let mut j = 0usize;
    for bar in &primary[lo..hi] {
        while j + 1 < overlay.len() && overlay[j + 1].ot <= bar.ot {
            j += 1;
        }
        out.push(if overlay[j].ot <= bar.ot { Some(j) } else { None });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact acceptance tuples from the chart-perf T4 brief: `(lo, hi)` is the
    /// inclusive-start/exclusive-end index window that can be visible in
    /// `[x0, x1]` under the same ±2-bar guard `seg_line`/Dots already apply
    /// (`x < x0 - 2.0 || x > x1 + 2.0` is discarded), clamped to `[0, n]`.
    #[test]
    fn overlay_visible_range_clamps_and_covers() {
        assert_eq!(overlay_visible_range(1000, 400.5, 610.2), (398, 613));
        assert_eq!(overlay_visible_range(1000, -5.0, 5.0), (0, 8));
        assert_eq!(overlay_visible_range(1000, 990.0, 2000.0), (988, 1000));
        assert_eq!(overlay_visible_range(0, 0.0, 0.0), (0, 0));
    }

    /// A 4-bar fixture at realistic prices: the ±100 signal a pattern emits is nowhere
    /// near these, which is exactly why plotting the raw series on the price axis was
    /// meaningless — the markers must come from the BARS, not the signal's magnitude.
    fn marker_bars() -> Vec<Bar> {
        (0..4)
            .map(|i| {
                let base = 64_000.0 + i as f64 * 100.0;
                Bar { t: i as f64, ot: 0, o: base, h: base + 50.0, l: base - 50.0, c: base, v: 1.0 }
            })
            .collect()
    }

    /// The regression this arm exists for: candlestick patterns emit a per-bar SIGNAL
    /// (`+100` bullish / `-100` bearish / `0` none), never a price. Every marker must
    /// land on the flagged bar's own extreme — bullish at its low, bearish at its high —
    /// and never at the raw ±100 (which on a 64k chart pins to the bottom of the pane).
    #[test]
    fn pattern_markers_anchor_to_the_flagged_bar_extreme() {
        let bars = marker_bars();
        let series = [0.0, -100.0, 0.0, 100.0];
        let (bull, bear) = pattern_marker_points(&series, &bars, 0, 4, &|v| v);
        assert_eq!(bull, vec![[3.0, bars[3].l]], "bullish marker sits at the bar's low");
        assert_eq!(bear, vec![[1.0, bars[1].h]], "bearish marker sits at the bar's high");
        // ...and never at the signal's own magnitude.
        assert!(bull.iter().chain(&bear).all(|p| p[1] > 1_000.0), "markers are in price space");
    }

    /// `0` means "no pattern on this bar" and NaN is warm-up — neither is a marker.
    /// (`0` is the value 63 of the 64 bars carry on a typical chart, so emitting it
    /// would carpet the pane.)
    #[test]
    fn pattern_markers_skip_zero_and_nan() {
        let bars = marker_bars();
        let series = [0.0, f64::NAN, 0.0, f64::NAN];
        let (bull, bear) = pattern_marker_points(&series, &bars, 0, 4, &|v| v);
        assert!(bull.is_empty() && bear.is_empty(), "0 / NaN emit no markers");
    }

    /// Catalogue-wide guard, not just hammer. Both fixes key off the CATEGORY, so every
    /// `Pattern` must be an Overlay whose outputs are all `Marker` — otherwise a pattern
    /// would slip back onto the price-space path that plotted its ±100 signal as a line.
    ///
    /// `Marker` is also pattern-EXCLUSIVE, which is what lets the style mean exactly one
    /// thing (a ±100/0 signal). A series that carries real prices must say so instead —
    /// `zigzag` is a `Line`, `williams_fractal` is `Dots`. A non-pattern `Marker` appearing
    /// here would silently inherit the pattern glyph path and the autofit exclusion, so it
    /// should be a deliberate decision, not a fallthrough.
    #[test]
    fn marker_is_pattern_exclusive_and_every_pattern_is_an_overlay_marker() {
        use crate::indicators::RenderKind;
        let mut non_pattern_markers = Vec::new();
        let mut patterns = 0usize;
        for m in crate::indicators::registry() {
            let any_marker = m.outputs.iter().any(|o| o.style == OutputStyle::Marker);
            if m.category == Category::Pattern {
                patterns += 1;
                assert_eq!(
                    m.kind,
                    RenderKind::Overlay,
                    "{}: pattern must be a price overlay",
                    m.name
                );
                assert!(
                    m.outputs.iter().all(|o| o.style == OutputStyle::Marker),
                    "{}: every pattern output must be a Marker",
                    m.name
                );
            } else if any_marker {
                non_pattern_markers.push(m.name);
            }
        }
        assert!(patterns >= 60, "the whole pattern catalogue is covered, got {patterns}");
        assert_eq!(
            non_pattern_markers,
            Vec::<&str>::new(),
            "Marker means a pattern signal — a price-carrying series must be Line/Dots"
        );
    }

    /// The structure indicators emit real PRICES (NaN between), so each says what it is:
    /// zigzag draws the connecting line through its pivots, williams_fractal drops a point
    /// on each fractal bar instead of joining unrelated bars into a line.
    #[test]
    fn structure_price_series_declare_their_true_style() {
        let style_of = |name: &str| {
            crate::indicators::get(name)
                .unwrap()
                .outputs
                .iter()
                .map(|o| o.style)
                .collect::<Vec<_>>()
        };
        assert_eq!(style_of("zigzag"), [OutputStyle::Line]);
        assert_eq!(style_of("williams_fractal"), [OutputStyle::Dots, OutputStyle::Dots]);
    }

    /// The autofit half of the same root cause — and the one only an eyeball caught: the
    /// marker placement above was already correct, yet the rendered chart was still ruined
    /// because the price pane folded the pattern's `±100`/`0` SIGNAL into its PRICE extent,
    /// dragging the axis to ~0 and squashing 64k candles into a strip. A pattern must not
    /// move the extent at all; real price overlays (MAs) must still fold in.
    #[test]
    fn autofit_excludes_pattern_signals_but_keeps_price_overlays() {
        let bars = marker_bars();
        let (lo, hi) = (63_950.0, 64_350.0); // the candles' own extent

        // `hammer` cannot fire on this doji-bodied fixture, so its series is all `0.0` —
        // precisely the value that used to drag `lo` to zero.
        let hammer = Active::new(1, crate::indicators::get("hammer").unwrap(), &bars);
        assert!(hammer.outputs[0].series.iter().all(|v| *v == 0.0), "fixture: all-zero signal");
        assert_eq!(
            fold_overlay_extent(&[hammer], 0, bars.len(), lo, hi),
            (lo, hi),
            "a pattern's signal series must not touch the price extent"
        );

        // A real price overlay still folds: seeding with ±inf leaves only its own values.
        let mut sma = Active::new(2, crate::indicators::get("sma").unwrap(), &bars);
        sma.set_params(vec![2.0], &bars);
        let (slo, shi) =
            fold_overlay_extent(&[sma], 0, bars.len(), f64::INFINITY, f64::NEG_INFINITY);
        assert!(slo > 60_000.0 && shi > 60_000.0, "price overlays are still folded: {slo}..{shi}");
    }

    /// Only the visible `[lo, hi)` window is emitted (same O(visible) contract the rest
    /// of `render_overlay` honours), and `map` is applied to the BAR PRICE — so log /
    /// percent scales land the glyph on the candle instead of off-pane.
    #[test]
    fn pattern_markers_respect_window_and_map() {
        let bars = marker_bars();
        let series = [100.0, 100.0, 100.0, 100.0];
        let (bull, bear) = pattern_marker_points(&series, &bars, 1, 3, &|v| v * 2.0);
        assert_eq!(bull, vec![[1.0, bars[1].l * 2.0], [2.0, bars[2].l * 2.0]]);
        assert!(bear.is_empty());
        // hi beyond the series is clamped, not panicking.
        let (b2, _) = pattern_marker_points(&series, &bars, 0, 99, &|v| v);
        assert_eq!(b2.len(), 4);
    }

    /// `render_overlay` must map only the [lo, hi) window, not the whole
    /// series (the T4 regression: the old `map`+`collect` in
    /// `crates/vike-chart/src/render.rs`'s `render_overlay`, over the FULL
    /// `line.series` every frame). `overlay_visible_range`'s
    /// returned width IS the number of `map` calls `render_overlay` performs
    /// (it slices `line.series[lo..hi]` before mapping), so this pure-fn
    /// assertion is equivalent to counting a spy `map` through the real draw
    /// path without needing an `egui::Context`/`PlotUi`.
    #[test]
    fn overlay_maps_visible_only() {
        let n = 10_000;
        let (lo, hi) = overlay_visible_range(n, 100.0, 200.0);
        let mapped_count = hi - lo;
        assert_ne!(mapped_count, n, "must not map the whole 10_000-pt series");
        assert!(
            mapped_count < 200,
            "expected ~104 mapped points (visible window + ±2 guard), got {mapped_count}"
        );
        assert_eq!(mapped_count, 105, "exact count per the ±2-bar guard formula");
    }

    fn bar(ot: i64) -> Bar {
        Bar { t: 0.0, ot, o: 1.0, h: 2.0, l: 1.0, c: 1.5, v: 1.0 }
    }

    #[test]
    fn reindex_aligns_overlay_by_open_time() {
        let primary = vec![bar(100), bar(200), bar(300), bar(400)];
        let overlay = vec![bar(150), bar(250), bar(410)]; // different grid + leading gap
        let idx = reindex_by_ot(&primary, &overlay, 0, 4);
        // ot=100 -> no overlay bar at/before 100 -> None; 200 -> overlay[0] (150);
        // 300 -> overlay[1] (250); 400 -> overlay[1] (250, 410 is after)
        assert_eq!(idx, vec![None, Some(0), Some(1), Some(1)]);
    }

    #[test]
    fn reindex_empty_overlay_is_all_none() {
        let primary = vec![bar(100), bar(200), bar(300)];
        let overlay: Vec<Bar> = Vec::new();
        let idx = reindex_by_ot(&primary, &overlay, 0, 3);
        assert_eq!(idx, vec![None, None, None]);
    }

    #[test]
    fn reindex_single_bar_overlay() {
        let primary = vec![bar(100), bar(200), bar(300), bar(400)];
        let overlay = vec![bar(250)];
        let idx = reindex_by_ot(&primary, &overlay, 0, 4);
        // Before 250 there's no floor -> None; at/after 250 it's always overlay[0].
        assert_eq!(idx, vec![None, None, Some(0), Some(0)]);
    }

    #[test]
    fn reindex_clamps_lo_hi_and_empty_range() {
        let primary = vec![bar(100), bar(200), bar(300)];
        let overlay = vec![bar(150)];
        // lo == hi -> empty.
        assert_eq!(reindex_by_ot(&primary, &overlay, 1, 1), Vec::<Option<usize>>::new());
        // hi past primary.len() clamps down; still covers the trailing bars.
        assert_eq!(reindex_by_ot(&primary, &overlay, 1, 100), vec![Some(0), Some(0)]);
    }

    /// GPU/egui parity by construction: the `CandleInstance`s `GpuCandleItem` emits (through
    /// its `build` hook) must have screen coords EQUAL to `candle_geom`'s body/wick corners
    /// mapped through the SAME `PlotTransform`. Both paths derive from `candle_geom`, so this
    /// pins the wiring — corner indices, screen top/bot orientation, color, and the
    /// hollow-bull `filled` flag — proving a GPU-drawn candle lands exactly where the egui
    /// painter would draw one.
    #[test]
    fn gpu_candle_instance_matches_candle_geom_through_transform() {
        // A fixed, deterministic transform: known screen rect + plot bounds (x = bar-index
        // space, y = price space), so the data→screen mapping is fully reproducible.
        let frame = Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(410.0, 320.0));
        let bounds = PlotBounds::from_min_max([-1.0, 90.0], [50.0, 130.0]);
        let transform = PlotTransform::new(frame, bounds, false);

        // One bull + one bear bar, drawn Hollow so the bull is the unfilled case.
        let bars = vec![
            Bar { t: 5.0, ot: 0, o: 100.0, h: 110.0, l: 95.0, c: 108.0, v: 3.0 }, // bull
            Bar { t: 6.0, ot: 0, o: 108.0, h: 112.0, l: 101.0, c: 102.0, v: 2.0 }, // bear
        ];
        let opts = ChartOptions::default();
        let map = |y: f64| y; // Linear/identity — the SAME map fed to both paths.

        // Capture the exact Vec<CandleInstance> GpuCandleItem hands to its build hook.
        let captured: std::cell::RefCell<Vec<CandleInstance>> = std::cell::RefCell::new(Vec::new());
        let build = |insts: Vec<CandleInstance>, _rect: Rect| -> egui::Shape {
            *captured.borrow_mut() = insts;
            egui::Shape::Noop
        };
        let item = GpuCandleItem::new(&bars, true, &map, &opts, &build);

        // Drive the REAL PlotItem::shapes through a headless egui Ui; it must push exactly one
        // Shape (the opaque GPU callback) at the candle z-slot.
        let n_shapes = std::cell::Cell::new(usize::MAX);
        egui::__run_test_ui(|ui| {
            let mut out: Vec<egui::Shape> = Vec::new();
            PlotItem::shapes(&item, ui, &transform, &mut out);
            n_shapes.set(out.len());
        });
        assert_eq!(n_shapes.get(), 1, "GpuCandleItem emits exactly one Shape (the GPU callback)");

        let insts = captured.borrow();
        assert_eq!(insts.len(), bars.len(), "one CandleInstance per bar");

        // color_bars_prev_close defaults false → prev_close is ignored, so recomputing
        // with `None` matches the GpuCandleItem path's per-bar prev tracking exactly.
        for (bar, inst) in bars.iter().zip(insts.iter()) {
            let g = candle_geom(bar, None, true, &map, &opts);
            let at = |xy: [f64; 2]| transform.position_from_point(&PlotPoint::new(xy[0], xy[1]));
            let lo_left = at(g.body[0]); // [t-HALF_BODY, lo]
            let lo_right = at(g.body[1]); // [t+HALF_BODY, lo]
            let hi_left = at(g.body[3]); // [t-HALF_BODY, hi]
            let low = at(g.wick[0]); // [t, map(low)]
            let high = at(g.wick[1]); // [t, map(high)]
            assert_eq!(inst.x_lo, lo_left.x, "left body edge x");
            assert_eq!(inst.x_hi, lo_right.x, "right body edge x");
            assert_eq!(inst.body_bot, lo_left.y, "body bottom (lower price) screen y");
            assert_eq!(inst.body_top, hi_left.y, "body top (higher price) screen y");
            assert_eq!(inst.wick_bot, low.y, "wick bottom (low) screen y");
            assert_eq!(inst.wick_top, high.y, "wick top (high) screen y");
            let c = g.color;
            assert_eq!(
                inst.color,
                [
                    c.r() as f32 / 255.0,
                    c.g() as f32 / 255.0,
                    c.b() as f32 / 255.0,
                    c.a() as f32 / 255.0,
                ],
                "instance color mirrors candle_geom color",
            );
            let bull = bar.c >= bar.o;
            assert_eq!(inst.filled, u32::from(!bull), "hollow-style bull body is unfilled");
        }
    }
}
