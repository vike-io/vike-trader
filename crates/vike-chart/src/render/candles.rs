//! Candle-family painters and the GPU candle seam, both derived from one shared `CandleGeom`.

use crate::colors::ChartColors;
use crate::interact::vis;
use crate::model::Bar;
use egui::{Color32, Rect, Stroke};
use egui_plot::{
    Line, PlotBounds, PlotGeometry, PlotItem, PlotItemBase, PlotPoint, PlotPoints, PlotTransform,
    Polygon,
};
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

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
