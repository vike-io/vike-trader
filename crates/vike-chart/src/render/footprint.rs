//! The Footprint style painter: per-bar columns of sell|buy volume cells.

use crate::colors::ChartColors;
use crate::model::Bar;
use egui::{Align2, Color32, Stroke};
use egui_plot::{PlotPoint, PlotPoints, Polygon, Text};
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

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
