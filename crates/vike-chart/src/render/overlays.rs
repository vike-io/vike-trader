//! Price-pane indicator overlays: run-split lines, the visible window, pattern markers, autofit.

use crate::indicators::{Active, Category, LineDash, OutputStyle};
use crate::model::Bar;
use egui::Color32;
use egui_plot::{Line, MarkerShape, PlotPoints, Points};
use vike_ui_theme::value::chart;

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
