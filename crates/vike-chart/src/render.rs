//! Per-style series renderers + indicator overlay/oscillator painting + the
//! chart-STYLE menu icon, split out of chart.rs (chart-UX bundle T0). Pure
//! egui_plot painting — no interaction state, no layout; `chart::draw`
//! orchestrates these against the price/volume/oscillator panes it owns.

use crate::chart::ChartStyle;
use crate::colors::ChartColors;
use crate::indicators::{Active, Category, LineDash, OutputStyle};
use crate::interact::vis;
use crate::model::Bar;
use crate::transforms;
use egui::{Align2, Color32, Rect, Stroke};
use egui_plot::{
    HLine, Line, MarkerShape, PlotBounds, PlotGeometry, PlotItem, PlotItemBase, PlotPoint,
    PlotPoints, PlotTransform, Points, Polygon, Text,
};
use vike_ui_theme::color::{faded, with_alpha};
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::side::Pair;
use vike_ui_theme::value::chart;

// The former per-style color consts (UP/DOWN/UP_S/DOWN_S/LINE/GRID/CROSS) became
// `ChartOptions` fields (chart-UX bundle T6), and every painter below now reads the frame's
// RESOLVED colours, `c: &ChartColors`, threaded in by `chart::draw` (`crate::colors`, design
// system spec §3.5). Alpha-FILL colours derive from a resolved colour through
// `vike_ui_theme::color::with_alpha`; the alphas, and the candle, footprint-cell, OHLC-tick and bar
// widths (in bar widths, the plot's x-unit), are the `chart` group of `ui-theme.toml`.

/// One candle's geometry in **data space** (bar-index x, mapped-price y) — the SINGLE source
/// both the egui painter ([`draw_candles`]) and the GPU seam ([`GpuCandleItem`]) derive from,
/// so a GPU-drawn candle can never diverge from an egui-drawn one (pixel-parity by
/// construction). Every field is exactly what `draw_candles` used to inline.
pub struct CandleGeom {
    /// Body quad corners, in the same order `draw_candles`' `Polygon` used:
    /// `[t-CANDLE_BODY_HALF_W, lo], [t+CANDLE_BODY_HALF_W, lo], [t+CANDLE_BODY_HALF_W, hi],
    /// [t-CANDLE_BODY_HALF_W, hi]`, where
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
/// bar in the slice). It only matters when `by_prev_close` (the window's
/// `ChartOptions::color_bars_prev_close`, TV "Color bars based on previous close") is set: then
/// bull/bear tests `close >= prev_close` rather than `close >= open`; the first-bar `None` falls
/// back to `close >= open`. Body FILL, body BORDER, and WICK take three independent up/down
/// color pairs off the frame's resolved colours `c`.
pub(crate) fn candle_geom(
    bar: &Bar,
    prev_close: Option<f64>,
    hollow_style: bool,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
    by_prev_close: bool,
) -> CandleGeom {
    // bull/bear classification stays RAW: it's a semantic (color) decision
    // that must not depend on the active scale mode, not a plotted position.
    let bull = match (by_prev_close, prev_close) {
        (true, Some(pc)) => bar.c >= pc,
        _ => bar.c >= bar.o,
    };
    let color = if bull { c.up } else { c.down };
    let border_color = if bull { c.border_up } else { c.border_down };
    let wick_color = if bull { c.wick_up } else { c.wick_down };
    // Body low/high edges still key off the raw open/close geometry (not the
    // coloring test) — the candle's shape never changes, only its color.
    let (lo, hi) = if bar.c >= bar.o { (map(bar.o), map(bar.c)) } else { (map(bar.c), map(bar.o)) };
    CandleGeom {
        body: [
            [bar.t - chart::CANDLE_BODY_HALF_W, lo],
            [bar.t + chart::CANDLE_BODY_HALF_W, lo],
            [bar.t + chart::CANDLE_BODY_HALF_W, hi],
            [bar.t - chart::CANDLE_BODY_HALF_W, hi],
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
    c: &ChartColors,
    by_prev_close: bool,
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
        let g = candle_geom(bar, prev_close, hollow, map, c, by_prev_close);
        p.line(
            Line::new("", PlotPoints::from(vec![g.wick[0], g.wick[1]]))
                .color(g.wick_color)
                .width(stroke::HAIRLINE)
                .allow_hover(false),
        );
        let poly = Polygon::new("", PlotPoints::from(g.body.to_vec()))
            .stroke(Stroke::new(stroke::HAIRLINE, g.border_color))
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
/// layer in vike-desktop consumes these; vike-chart itself never draws from them.
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
    let lo_left = at(g.body[0]); // [t-CANDLE_BODY_HALF_W, lo]  → screen bottom-left
    let lo_right = at(g.body[1]); // [t+CANDLE_BODY_HALF_W, lo] → screen bottom-right
    let hi_left = at(g.body[3]); // [t-CANDLE_BODY_HALF_W, hi]  → screen top-left
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
/// by vike-desktop's `build` hook) at the exact z-slot a candle `Polygon` occupies today: it is
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
    /// vike-desktop's per-frame hook turning screen-space instances + the plot `Rect` into the
    /// opaque GPU paint `Shape`. Borrowed with the chart-inputs lifetime `'a` (it comes from
    /// `ChartInputs::gpu_candles`, which outlives the `plot.show` closure) — the ONLY borrow
    /// the stored item holds.
    build: &'a dyn Fn(Vec<CandleInstance>, Rect) -> egui::Shape,
    // egui_plot bookkeeping the trait's `base()`/`base_mut()` return; carried so the default
    // name/id/highlight/allow_hover methods compile without a panic. Empty name / no legend.
    base: PlotItemBase,
}

impl<'a> GpuCandleItem<'a> {
    /// Build a `GpuCandleItem` from this frame's (LOD) bar slice. `bars`/`map`/`c` are
    /// borrowed ONLY for the duration of this call — their [`candle_geom`] output is stored
    /// OWNED — so they may safely be the `plot.show` closure's short-lived locals; only
    /// `build` (lifetime `'a`, from `ChartInputs::gpu_candles`) is retained by reference.
    pub fn new(
        bars: &[Bar],
        hollow: bool,
        map: &dyn Fn(f64) -> f64,
        c: &ChartColors,
        by_prev_close: bool,
        build: &'a dyn Fn(Vec<CandleInstance>, Rect) -> egui::Shape,
    ) -> Self {
        // Track the preceding bar's raw close so the GPU path classifies bull/bear
        // identically to the egui painter under "color bars based on previous close".
        let mut prev_close: Option<f64> = None;
        let geom: Vec<CandleGeom> = bars
            .iter()
            .map(|bar| {
                let g = candle_geom(bar, prev_close, hollow, map, c, by_prev_close);
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
    c: &ChartColors,
    vmax: f64,
) {
    // vmax/frac/hw are volume (x-axis-adjacent) quantities, not y-positions — unmapped.
    let vmax = vmax.max(1e-9);
    for bar in vis(bars, x0, x1) {
        let bull = bar.c >= bar.o; // RAW classification, see draw_candles
        let color = if bull { c.up } else { c.down };
        let (lo, hi) = if bull { (map(bar.o), map(bar.c)) } else { (map(bar.c), map(bar.o)) };
        let frac = (bar.v / vmax).clamp(0.0, 1.0).sqrt();
        let hw = 0.12 + frac * (0.42 - 0.12);
        p.line(
            Line::new("", PlotPoints::from(vec![[bar.t, map(bar.l)], [bar.t, map(bar.h)]]))
                .color(color)
                .width(stroke::HAIRLINE)
                .allow_hover(false),
        );
        let body = vec![[bar.t - hw, lo], [bar.t + hw, lo], [bar.t + hw, hi], [bar.t - hw, hi]];
        p.polygon(
            Polygon::new("", PlotPoints::from(body))
                .fill_color(color)
                .stroke(Stroke::new(stroke::HAIRLINE, color))
                .allow_hover(false),
        );
    }
}

pub(crate) fn draw_bars(
    p: &mut egui_plot::PlotUi,
    draw: &[Bar],
    hlc: bool,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    for bar in draw {
        let color = if bar.c >= bar.o { c.up } else { c.down }; // RAW classification
        p.line(
            Line::new("", PlotPoints::from(vec![[bar.t, map(bar.l)], [bar.t, map(bar.h)]]))
                .color(color)
                .width(stroke::HAIRLINE)
                .allow_hover(false),
        );
        if !hlc {
            p.line(
                Line::new(
                    "",
                    PlotPoints::from(vec![
                        [bar.t - chart::OHLC_TICK_W, map(bar.o)],
                        [bar.t, map(bar.o)],
                    ]),
                )
                .color(color)
                .width(stroke::HAIRLINE)
                .allow_hover(false),
            );
        }
        p.line(
            Line::new(
                "",
                PlotPoints::from(vec![
                    [bar.t, map(bar.c)],
                    [bar.t + chart::OHLC_TICK_W, map(bar.c)],
                ]),
            )
            .color(color)
            .width(stroke::HAIRLINE)
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
    c: &ChartColors,
) {
    let pts: Vec<[f64; 2]> = vis(bars, x0, x1).map(|b| [b.t, map(b.c)]).collect();
    if pts.len() < 2 {
        return;
    }
    let line = c.line;
    if area {
        let y_floor = map(y_floor);
        // TradingView-style GRADIENT fill: instead of one flat trapezoid per
        // segment, stack K bands from the line down to the floor with the alpha
        // fading to transparent — dense at the line, gone at the bottom. Each
        // band stays a convex quad (per-segment), so no non-convex fan artifact.
        const K: usize = 6;
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
                let alpha = (f64::from(chart::AREA_TOP_ALPHA) * (1.0 - f0)).round() as u8;
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad))
                        .fill_color(with_alpha(c.line, alpha))
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
        }
    }
    p.line(
        Line::new("", PlotPoints::from(pts.clone()))
            .color(line)
            .width(stroke::LINE)
            .allow_hover(false),
    );
    if markers {
        p.points(
            Points::new("", PlotPoints::from(pts))
                .radius(chart::LINE_MARKER_RADIUS)
                .color(line)
                .filled(true),
        );
    }
}

pub(crate) fn draw_step(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let mut pts = Vec::new();
    for b in vis(bars, x0, x1) {
        let y = map(b.c);
        pts.push([b.t - 0.5, y]);
        pts.push([b.t + 0.5, y]);
    }
    if pts.len() >= 2 {
        p.line(
            Line::new("", PlotPoints::from(pts))
                .color(c.line)
                .width(stroke::LINE)
                .allow_hover(false),
        );
    }
}

/// Draw a vike-style mini icon for a chart style into `r` (menu rows + title-bar brand).
pub fn draw_style_icon(p: &egui::Painter, r: Rect, style: ChartStyle) {
    use ChartStyle::*;
    // The menu icon previews what the chart will draw: the installed market colours, and the
    // theme's secondary text for the styles that have no direction (spec §3.5).
    let t = Tokens::of(p.ctx());
    let (up, dn) = Pair::RiseFall.colours(&t);
    let ln = t.theme.text2;
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
            p.rect_stroke(rr, 0.0, Stroke::new(stroke::HAIRLINE, col), egui::StrokeKind::Middle);
        } else {
            p.rect_filled(rr, 0.0, col);
        }
    };
    let line = |pts: Vec<egui::Pos2>, col: Color32, w: f32| {
        p.add(egui::Shape::line(pts, Stroke::new(w, col)));
    };
    match style {
        Candles | HeikinAshi | VolumeCandles => {
            vline(0.33, 0.06, 0.94, up, stroke::HAIRLINE);
            body(0.33, 0.26, 0.64, up, false);
            vline(0.67, 0.2, 0.84, dn, stroke::HAIRLINE);
            body(0.67, 0.36, 0.74, dn, false);
        }
        Hollow => {
            vline(0.33, 0.06, 0.94, up, stroke::HAIRLINE);
            body(0.33, 0.26, 0.64, up, true);
            vline(0.67, 0.2, 0.84, dn, stroke::HAIRLINE);
            body(0.67, 0.36, 0.74, dn, true);
        }
        Bars => {
            vline(0.33, 0.1, 0.9, up, stroke::HAIRLINE);
            hline(0.16, 0.33, 0.34, up, stroke::HAIRLINE);
            hline(0.33, 0.5, 0.62, up, stroke::HAIRLINE);
            vline(0.7, 0.18, 0.82, dn, stroke::HAIRLINE);
            hline(0.53, 0.7, 0.42, dn, stroke::HAIRLINE);
            hline(0.7, 0.87, 0.7, dn, stroke::HAIRLINE);
        }
        HlcBars => {
            vline(0.33, 0.1, 0.9, up, stroke::HAIRLINE);
            hline(0.33, 0.52, 0.62, up, stroke::HAIRLINE);
            vline(0.7, 0.18, 0.82, dn, stroke::HAIRLINE);
            hline(0.7, 0.89, 0.7, dn, stroke::HAIRLINE);
        }
        HighLow => {
            vline(0.34, 0.1, 0.9, ln, stroke::LINE);
            vline(0.66, 0.22, 0.8, ln, stroke::LINE);
        }
        Line | LineMarkers => {
            let path = vec![pt(0.08, 0.72), pt(0.34, 0.36), pt(0.56, 0.6), pt(0.92, 0.22)];
            line(path.clone(), ln, stroke::LINE);
            if matches!(style, LineMarkers) {
                for q in &path {
                    p.circle_filled(*q, chart::ICON_DOT_RADIUS, ln);
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
                stroke::LINE,
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
            p.add(egui::Shape::convex_polygon(
                poly,
                faded(up, chart::AREA_ICON_FILL),
                Stroke::new(stroke::LINE, up),
            ));
        }
        Baseline => {
            hline(0.08, 0.92, 0.52, t.theme.analysis_line, stroke::HAIRLINE);
            line(
                vec![pt(0.08, 0.66), pt(0.4, 0.34), pt(0.62, 0.6), pt(0.92, 0.42)],
                up,
                stroke::LINE,
            );
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
                stroke::LINE,
            );
        }
        PointFigure => {
            line(vec![pt(0.14, 0.3), pt(0.42, 0.62)], up, stroke::LINE);
            line(vec![pt(0.42, 0.3), pt(0.14, 0.62)], up, stroke::LINE);
            p.circle_stroke(pt(0.68, 0.46), 0.16 * r.height(), Stroke::new(stroke::LINE, dn));
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
    c: &ChartColors,
) {
    // vike baseline: green fill+line above the anchor, red below.
    let base_raw = bars.first().map(|b| b.c).unwrap_or(0.0);
    let base = map(base_raw);
    // (t, RAW close, MAPPED close) — the above/below classification vs the
    // baseline anchor stays RAW (semantic, scale-mode-invariant, same
    // reasoning as the candle bull/bear color); only the plotted points map.
    let pts: Vec<(f64, f64, f64)> = vis(bars, x0, x1).map(|b| (b.t, b.c, map(b.c))).collect();
    let up_fill = with_alpha(c.up_s, chart::BASELINE_WASH_ALPHA);
    let dn_fill = with_alpha(c.down_s, chart::BASELINE_WASH_ALPHA);
    for w in pts.windows(2) {
        let above = (w[0].1 + w[1].1) / 2.0 >= base_raw;
        let (col, fill) = if above { (c.up_s, up_fill) } else { (c.down_s, dn_fill) };
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
            Line::new("", PlotPoints::from(vec![p0, p1]))
                .color(col)
                .width(stroke::LINE)
                .allow_hover(false),
        );
    }
    p.hline(
        HLine::new("", base)
            .color(c.zero_line)
            .width(stroke::HAIRLINE)
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
    c: &ChartColors,
) {
    let v: Vec<&Bar> = vis(bars, x0, x1).collect();
    if v.len() < 2 {
        return;
    }
    let fill = with_alpha(c.line, chart::HLC_FILL_ALPHA);
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
        Line::new("", PlotPoints::from(cpts))
            .color(c.line)
            .width(stroke::HAIRLINE)
            .allow_hover(false),
    );
}

pub(crate) fn draw_columns(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    ymin: f64, // RAW floor, precomputed by the caller
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let ymin = map(ymin);
    for b in vis(bars, x0, x1) {
        let col = if b.c >= b.o { c.up } else { c.down }; // RAW classification
        let fill = with_alpha(col, chart::COLUMN_FILL_ALPHA);
        let y = map(b.c);
        let body = vec![[b.t - 0.32, ymin], [b.t + 0.32, ymin], [b.t + 0.32, y], [b.t - 0.32, y]];
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
    c: &ChartColors,
) {
    let n = k.prices.len();
    if n < 2 {
        return;
    }
    for i in 0..n - 1 {
        let thick = k.thick.get(i).copied().unwrap_or(false);
        let col = if thick { c.up } else { c.down };
        let w = if thick { stroke::EDGE } else { stroke::HAIRLINE };
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
    c: &ChartColors,
) {
    if box_ <= 0.0 {
        return;
    }
    let hw = 0.34;
    for (i, col) in cols.iter().enumerate() {
        let x = i as f64;
        let color = if col.up { c.up } else { c.down };
        let nboxes = (((col.top - col.bottom) / box_).round() as i64).max(1);
        for b in 0..nboxes {
            let y_raw = col.bottom + b as f64 * box_;
            let y = map(y_raw);
            let y_top = map(y_raw + box_);
            if col.up {
                p.line(
                    Line::new("", PlotPoints::from(vec![[x - hw, y], [x + hw, y_top]]))
                        .color(color)
                        .width(stroke::LINE)
                        .allow_hover(false),
                );
                p.line(
                    Line::new("", PlotPoints::from(vec![[x - hw, y_top], [x + hw, y]]))
                        .color(color)
                        .width(stroke::LINE)
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
                        .width(stroke::HAIRLINE)
                        .allow_hover(false),
                );
            }
        }
    }
}

/// Footprint (SP2, T5): per bar, a column of per-price cells (sell|buy volume). Each cell is a
/// faint imbalance-tinted rect `[x±FOOTPRINT_CELL_HALF_W, price±tick_size/2]`; the bar's
/// point-of-control cell (max buy+sell volume WITHIN that bar) gets an amber outline; buy/sell
/// numbers are drawn in plot space when `cell_px` clears the text-legibility gate
/// (`orderflow::cell_text_legible`) — below that the tinted rect alone carries the imbalance read
/// (no delta-bar fallback: the tint already IS the delta signal, unlike the brief's original
/// two-tier sketch).
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
    c: &ChartColors,
    cell_px: f32,
    i0: usize,
    i1: usize,
) {
    let legible = crate::orderflow::cell_text_legible(cell_px);
    let buy_fill = with_alpha(c.up_s, chart::CELL_FILL_ALPHA);
    let sell_fill = with_alpha(c.down_s, chart::CELL_FILL_ALPHA);
    let neutral_fill = with_alpha(c.cross, chart::CELL_FILL_ALPHA / 2);
    // Read out here: the cell loop below binds `c` to each cell. TEXT colour, never the graphic
    // `up_s`/`down_s` pair `buy_fill`/`sell_fill` above uses for the cell background — final-review
    // finding (Important #1): these numbers used to read the graphic pair, which on TradingView
    // sits below the text contrast floor. `crate::colors::ChartColors`' own doc draws this split.
    let (buy_ink, sell_ink) = (c.up_text, c.down_text);
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
                        [x - chart::FOOTPRINT_CELL_HALF_W, y0],
                        [x + chart::FOOTPRINT_CELL_HALF_W, y0],
                        [x + chart::FOOTPRINT_CELL_HALF_W, y1],
                        [x - chart::FOOTPRINT_CELL_HALF_W, y1],
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
                            [x - chart::FOOTPRINT_CELL_HALF_W, y0],
                            [x + chart::FOOTPRINT_CELL_HALF_W, y0],
                            [x + chart::FOOTPRINT_CELL_HALF_W, y1],
                            [x - chart::FOOTPRINT_CELL_HALF_W, y1],
                        ]),
                    )
                    .fill_color(Color32::TRANSPARENT)
                    .stroke(Stroke::new(stroke::HAIRLINE, chart::POC_AMBER))
                    .allow_hover(false),
                );
            }
            if legible {
                let yc = (y0 + y1) / 2.0;
                p.text(
                    Text::new("", PlotPoint::new(x - 0.05, yc), format!("{:.0}", c.sell_vol))
                        .color(sell_ink)
                        .anchor(Align2::RIGHT_CENTER),
                );
                p.text(
                    Text::new("", PlotPoint::new(x + 0.05, yc), format!("{:.0}", c.buy_vol))
                        .color(buy_ink)
                        .anchor(Align2::LEFT_CENTER),
                );
            }
        }
    }
}

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

/// Split `vals` (index-aligned to bars starting at `base`; NaN = warm-up)
/// into contiguous runs and draw one line per run, so gaps aren't connected.
/// `base` is the absolute bar index of `vals[0]` — 0 for a full series
/// (`render_oscillator`, whose panes are NOT mapped/sliced), or the sliced
/// window's start index (`render_overlay`, chart-perf T4) so `vals` can be a
/// visible-only sub-slice while still drawing at the correct absolute x.
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
                            .radius(chart::PATTERN_MARKER_RADIUS)
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
                            .radius(chart::DOT_MARKER_RADIUS)
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
    c: &ChartColors,
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
        c,
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
    c: &ChartColors,
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
        // OWN editable colour + show toggle; a level nobody coloured (`None`) draws in
        // the theme's level line, resolved here so it follows a theme change.
        for band in a.bands {
            if !band.show {
                continue;
            }
            p.hline(
                HLine::new("", band.value)
                    .color(band.color.unwrap_or(c.level_line))
                    .width(stroke::HAIRLINE)
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
                        let col = if rising { c.up_s } else { c.down_s };
                        egui_plot::Bar::new(i as f64, v).fill(col).width(chart::HISTOGRAM_BAR_W)
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
    c: &ChartColors,
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
        c,
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

#[path = "render_tests.rs"]
#[cfg(test)]
mod render_tests;
