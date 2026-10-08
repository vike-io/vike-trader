//! Screen-space overlay paints drawn AFTER the price plot lays out (chart refactor
//! PR-5): the local + ghost crosshair, the right-axis last-price chip, the OHLC
//! legend, the sticky-fallback hint, and the price/time crosshair tags. Each was
//! inline in `draw()`; the bodies here are moved verbatim — pure paints that read
//! the frame/hover/scale locals and draw directly via `ui.painter()`, returning
//! nothing — so the render stays byte-identical. See `chart/mod.rs`'s `draw`.

use super::fmt::{fmt_datetime, fmt_scaled_view, fmt_thousands, fmt_thousands_prec};
use crate::colors::ChartColors;
use crate::model::ChartState;
use crate::scale::{ScaleMode, ScaleView};
use crate::tz::DisplayTz;
use egui::{Color32, FontId, Rect, Stroke, Vec2};
use egui_plot::PlotTransform;
use vike_ui_theme::color::{faded, with_alpha};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::{TextRole, TextSize};
use vike_ui_theme::value::chart;

/// crosshair — vike: 1px TEXT2 dashed, Qt.DashLine = 4px dash / 2px gap. Painted in screen
/// space so the dash pattern is EXACT (egui_plot's preset locks the gap to the golden ratio).
pub(crate) fn paint_crosshair(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    hover_y: Option<f64>,
    hover_xt: Option<(f64, i64)>,
    cross_color: Color32,
) {
    if let Some(py) = hover_y {
        let stroke = Stroke::new(stroke::HAIRLINE, cross_color);
        let cp = ui.painter().with_clip_rect(frame); // never bleed into the title/caption
        if let Some((bt, _)) = hover_xt {
            let cx = tr.position_from_point(&egui_plot::PlotPoint::new(bt, py)).x;
            cp.extend(egui::Shape::dashed_line(
                &[egui::pos2(cx, frame.top()), egui::pos2(cx, frame.bottom())],
                stroke,
                4.0,
                2.0,
            ));
        }
        let cy = tr.position_from_point(&egui_plot::PlotPoint::new(0.0, py)).y;
        cp.extend(egui::Shape::dashed_line(
            &[egui::pos2(frame.left(), cy), egui::pos2(frame.right(), cy)],
            stroke,
            4.0,
            2.0,
        ));
    }
}

/// Sync seam (task B7) behavior 4: the ghost crosshair's vertical dashed line, at
/// [`chart::GHOST_STRENGTH`] of the crosshair colour (`vike_ui_theme::color::faded` — the same
/// gamma-space fade the volume bars take): y isn't synced, so no horizontal line and no price
/// tag, just the x position. `ghost_xt` is resolved in `draw` (stays inline there — it reads
/// `sync`/`series`/`nearest_index_by_ts`) and is passed in here already computed.
pub(crate) fn paint_ghost_crosshair(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    c: &ChartColors,
    ghost_xt: Option<(f64, i64)>,
) {
    if let Some((gt, _)) = ghost_xt {
        let ghost_stroke = Stroke::new(stroke::HAIRLINE, faded(c.cross, chart::GHOST_STRENGTH));
        let cp = ui.painter().with_clip_rect(frame);
        let cx = tr.position_from_point_x(gt);
        cp.extend(egui::Shape::dashed_line(
            &[egui::pos2(cx, frame.top()), egui::pos2(cx, frame.bottom())],
            ghost_stroke,
            4.0,
            2.0,
        ));
    }
}

/// right-axis last-price chip: filled in the candle direction's colour, its text the colour
/// that reads on that fill (`ChartColors::text_on` — the kit's on-fill black on every design
/// fill), JetBrains Mono at the Caption role — a price is read character by character (spec
/// §3.3). Gated with the dashed last-price LINE by `show_last_price` (T6) — they are one "last
/// price" feature (line + its axis label).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_last_price_chip(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    show_last_price: bool,
    state: &ChartState,
    view: ScaleView,
    anchor: f64,
    precision: Option<u8>,
    c: &ChartColors,
    ts: TextSize,
) {
    if let (true, Some(lastbar)) = (show_last_price, state.bars.last()) {
        // `view.map` carries the invert flip, so the chip's y position tracks the
        // (possibly flipped) last-price line exactly.
        let mapped_c = view.map(lastbar.c, anchor);
        let y = tr.position_from_point_y(mapped_c);
        let col = if lastbar.c >= lastbar.o { c.up } else { c.down }; // CANDLE colors
        let ink = c.text_on(col);
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            fmt_scaled_view(view, mapped_c, anchor, precision),
            FontId::monospace(ts.px(TextRole::Caption)),
            ink,
        );
        let pad = Vec2::new(space::MD, space::XS);
        let size = galley.size() + pad * 2.0;
        let chip =
            Rect::from_min_size(egui::pos2(frame.right() + space::HAIR, y - size.y / 2.0), size);
        painter.rect_filled(chip, chart::AXIS_TAG_RADIUS, col);
        painter.galley(chip.min + pad, galley, ink);
    }
}

/// OHLC legend overlay (top-left, inside the plot — vike overlays it ~10px in). Painted,
/// NOT a layout row, so the window can shrink into narrow tiles (no min-width forced).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_ohlc_legend(
    ui: &egui::Ui,
    frame: Rect,
    state: &ChartState,
    hovered: Option<[f64; 4]>,
    colors: &ChartColors,
    eff_mode: ScaleMode,
    precision: Option<u8>,
    overlay_legend: &[(Color32, String, f64, bool)],
    ts: TextSize,
) {
    if !state.bars.is_empty() {
        let (o, h, l, c) = hovered
            .map(|[o, h, l, c]| (o, h, l, c))
            .or_else(|| state.bars.last().map(|b| (b.o, b.h, b.l, b.c)))
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let chg = c - o;
        let pct = if o != 0.0 { chg / o * 100.0 } else { 0.0 };
        // Direction is TEXT here, so it takes the market set's text colours (spec §3.2).
        let dir = if c >= o { colors.up_text } else { colors.down_text };
        let txt = colors.text;
        let f = FontId::monospace(ts.px(TextRole::Title)); // the OHLC legend: Title, in mono
        let tf = |col: Color32| egui::TextFormat {
            font_id: f.clone(),
            color: col,
            ..Default::default()
        };
        // OHLC values are always RAW prices (never percent/index); the §3
        // precision override applies for Linear/Log, but the rebased modes
        // (Percent/Indexed) keep the default 2dp grouping here (spec §1 —
        // precision is Linear/Log only). Invert never affects this (raw prices).
        let ohlc_fmt = |v: f64| match (!matches!(eff_mode, ScaleMode::Percent | ScaleMode::Indexed))
            .then_some(precision)
            .flatten()
        {
            Some(p) => fmt_thousands_prec(v, p as usize),
            None => fmt_thousands(v),
        };
        let mut job = egui::text::LayoutJob::default();
        job.append("O ", 0.0, tf(txt));
        job.append(&ohlc_fmt(o), 0.0, tf(dir));
        job.append("  H ", 0.0, tf(txt));
        job.append(&ohlc_fmt(h), 0.0, tf(dir));
        job.append("  L ", 0.0, tf(txt));
        job.append(&ohlc_fmt(l), 0.0, tf(dir));
        job.append("  C ", 0.0, tf(txt));
        job.append(&ohlc_fmt(c), 0.0, tf(dir));
        job.append(&format!("   {:+.2} ({:+.2}%)", chg, pct), 0.0, tf(dir));
        // C2a/C2b: append each compare overlay's symbol + last-visible readout, colored
        // per series (uses `s.symbol`, not the write-only `ChartState.symbol`). A
        // shared-% overlay (`is_pct`) shows `+d.dd%`; a Pin-to-Right overlay shows its
        // ABSOLUTE close via `fmt_thousands` (thousands-grouped, app-standard 2dp, no
        // `%` — Task 7 review Minor-2). NOT `ohlc_fmt`: that carries the PRIMARY symbol's
        // price precision, which shouldn't dictate an unrelated-magnitude compare's
        // readout — this stays in lock-step with the compare's own secondary axis above.
        // EMPTY `overlay_legend` (no overlays) ⇒ the legend is unchanged.
        for (color, sym, pv, is_pct) in overlay_legend {
            let entry = if *is_pct {
                format!("   {}  {:+.2}%", sym, pv)
            } else {
                format!("   {}  {}", sym, fmt_thousands(*pv))
            };
            job.append(&entry, 0.0, tf(*color));
        }
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(frame.left_top() + Vec2::new(8.0, 11.0), galley, txt);
        // +5px → matches Py title→OHLC gap (~35 logical)
    }
}

/// Sticky-fallback hint (chart-UX bundle T3, carry-over Minor from the T2
/// review): drawn directly in-chart — small, semi-transparent, top-center,
/// under the OHLC legend — rather than plumbed through vike-desktop's status
/// line (rebuilt every frame from feeds; the chart is the honest surface
/// for a chart-local degrade). Only visible while `scale_fallback` is Some
/// (the sticky latch is engaged).
pub(crate) fn paint_scale_fallback_hint(
    ui: &egui::Ui,
    frame: Rect,
    scale_fallback: Option<&'static str>,
    c: &ChartColors,
    ts: TextSize,
) {
    if let Some(hint) = scale_fallback {
        let col = with_alpha(c.text2, chart::HINT_ALPHA);
        // Words, so Inter (spec §3.3), at the Body role.
        let galley = ui.painter().layout_no_wrap(
            hint.to_string(),
            FontId::proportional(ts.px(TextRole::Body)),
            col,
        );
        let pos = egui::pos2(
            frame.center().x - galley.size().x / 2.0,
            frame.top() + chart::SCALE_HINT_FROM_TOP,
        );
        ui.painter().with_clip_rect(frame).galley(pos, galley, col);
    }
}

/// crosshair price tag (right axis, at the cursor's y) — on the theme's border fill, like vike.
/// `py` is already in MAPPED plot space (from plot_ui.pointer_coordinate()); the
/// label unmaps it back to a raw price (or shows it directly for Percent, whose
/// mapped value already IS the percent number).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_price_tag(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    hover_y: Option<f64>,
    view: ScaleView,
    anchor: f64,
    precision: Option<u8>,
    c: &ChartColors,
    ts: TextSize,
) {
    if let Some(py) = hover_y {
        let y = tr.position_from_point_y(py);
        // Don't render the price tag in the very top strip of the pane — that's
        // where the window title-bar controls (─ □ ✕) sit, and the tag would
        // flash next to the maximize button when the cursor is up there.
        if y < frame.top() + chart::PRICE_TAG_HIDDEN_STRIP_H {
            return;
        }
        let painter = ui.painter();
        // `py` is in MAPPED (possibly inverted) plot-space; `fmt_scaled_view`
        // un-flips it before reading out the raw price / percent / index.
        let galley = painter.layout_no_wrap(
            fmt_scaled_view(view, py, anchor, precision),
            FontId::monospace(ts.px(TextRole::Caption)),
            c.text,
        );
        let pad = Vec2::new(space::MD, space::XS);
        let size = galley.size() + pad * 2.0;
        let chip =
            Rect::from_min_size(egui::pos2(frame.right() + space::HAIR, y - size.y / 2.0), size);
        painter.rect_filled(chip, chart::AXIS_TAG_RADIUS, c.tag_bg);
        painter.galley(chip.min + pad, galley, c.text);
    }
}

/// Right-edge current-value tag for a LINEAR sub-pane (oscillator / CVD / volume —
/// TradingView parity). The sub-pane twin of [`paint_last_price_chip`]: a persistent
/// COLORED chip pinned to the right axis at a data value, so every oscillator pane
/// shows its live reading the way the price pane shows its last price. Sub-panes are
/// ALWAYS linear (no `ScaleMode`/anchor mapping — see `sub_pane_plot`), so `value` is
/// already a raw plot-space y and its pixel is `tr.position_from_point_y(value)`
/// directly; `label` formats it EXACTLY as that pane's own y-axis does, so the tag
/// reads out in the axis's own units (compact for volume/CVD, `{:.1}` for studies).
/// The chip is filled in the labeled series' own `color`, its text the colour that reads on
/// that fill (`ChartColors::text_on`). A `None`/non-finite `value` paints NOTHING —
/// byte-identical no-tag when the pane has no current reading (empty slice / indicator
/// warm-up NaN).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_value_tag(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    value: Option<f64>,
    color: Color32,
    c: &ChartColors,
    ts: TextSize,
    label: impl FnOnce(f64) -> String,
) {
    let Some(v) = value.filter(|v| v.is_finite()) else {
        return;
    };
    let txt = c.text_on(color);
    let painter = ui.painter();
    // The Caption role in mono, matching the crosshair price tag.
    let galley = painter.layout_no_wrap(label(v), FontId::monospace(ts.px(TextRole::Caption)), txt);
    let pad = Vec2::new(space::MD, space::XS);
    let size = galley.size() + pad * 2.0;
    // The value sits inside the pane's own y-autofit bounds, so its pixel is inside
    // `frame`; clamp the chip's center anyway so a value hard against the top/bottom
    // (or a scrolled-off LAST value beyond the visible slice's fit) stays fully
    // visible. Guarded against a pane shorter than the chip (clamp would panic).
    let lo = frame.top() + size.y / 2.0;
    let hi = frame.bottom() - size.y / 2.0;
    let cy = tr.position_from_point_y(v);
    let cy = if lo <= hi { cy.clamp(lo, hi) } else { cy };
    let chip =
        Rect::from_min_size(egui::pos2(frame.right() + space::HAIR, cy - size.y / 2.0), size);
    painter.rect_filled(chip, chart::AXIS_TAG_RADIUS, color);
    painter.galley(chip.min + pad, galley, txt);
}

/// crosshair time tag (bottom axis) — on the theme's border fill, like vike. Falls back
/// to the ghost bar (sync seam, task B7 behavior 4) when there's no local
/// hover — mutually exclusive with `hover_xt` by construction (`ghost_xt`
/// is `None` whenever `hover_xt` is `Some`), so this never double-paints;
/// deliberately the SAME tag style for both — no price tag pairs with the
/// ghost case (y isn't synced), matched by the price-tag block above
/// staying gated on `hover_y` alone, untouched by `ghost_xt`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_time_tag(
    ui: &egui::Ui,
    frame: Rect,
    tr: &PlotTransform,
    tz: DisplayTz,
    hover_xt: Option<(f64, i64)>,
    ghost_xt: Option<(f64, i64)>,
    c: &ChartColors,
    ts: TextSize,
) {
    if let Some((t, ot)) = hover_xt.or(ghost_xt) {
        let x = tr.position_from_point_x(t);
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            fmt_datetime(ot, tz),
            FontId::monospace(ts.px(TextRole::Caption)),
            c.text,
        );
        let pad = Vec2::new(space::MD, space::XS);
        let size = galley.size() + pad * 2.0;
        let chip =
            Rect::from_min_size(egui::pos2(x - size.x / 2.0, frame.bottom() + space::HAIR), size);
        painter.rect_filled(chip, chart::AXIS_TAG_RADIUS, c.tag_bg);
        painter.galley(chip.min + pad, galley, c.text);
    }
}
