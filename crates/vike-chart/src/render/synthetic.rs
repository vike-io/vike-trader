//! Kagi and Point & Figure painters: the two synthetic styles drawn from their own types.

use crate::colors::ChartColors;
use crate::transforms;
use egui_plot::{Line, PlotPoints};
use vike_ui_theme::metrics::stroke;

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
