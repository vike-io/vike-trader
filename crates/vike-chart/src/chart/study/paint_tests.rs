//! The study painters' pure halves: the overlay window and autofit fold, the pattern markers,
//! and the oscillator's open-time reindex.

use super::ChartStudy;
use super::oscillator::reindex_by_ot;
use super::overlays::{fold_overlay_extent, overlay_visible_range, pattern_marker_points};
use crate::chart::state::bar::ChartBar;
use vike_indicators::{Category, OutputStyle};

/// Exact acceptance tuples from the chart-perf T4 brief: `(lo, hi)` is the
/// inclusive-start/exclusive-end index window that can be visible in
/// `[x0, x1]` under the same ±2-bar guard `segment_line`/Dots already apply
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
fn marker_bars() -> Vec<ChartBar> {
    (0..4)
        .map(|i| {
            let base = 64_000.0 + i as f64 * 100.0;
            ChartBar {
                t: i as f64,
                ot: 0,
                o: base,
                h: base + 50.0,
                l: base - 50.0,
                c: base,
                v: 1.0,
            }
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
    use vike_indicators::RenderKind;
    let mut non_pattern_markers = Vec::new();
    let mut patterns = 0usize;
    for m in vike_indicators::registry() {
        let any_marker = m.outputs.iter().any(|o| o.style == OutputStyle::Marker);
        if m.category == Category::Pattern {
            patterns += 1;
            assert_eq!(m.kind, RenderKind::Overlay, "{}: pattern must be a price overlay", m.name);
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
        vike_indicators::get(name).unwrap().outputs.iter().map(|o| o.style).collect::<Vec<_>>()
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
    let hammer = ChartStudy::new(1, vike_indicators::get("hammer").unwrap(), &bars);
    assert!(hammer.outputs[0].series.iter().all(|v| *v == 0.0), "fixture: all-zero signal");
    assert_eq!(
        fold_overlay_extent(&[hammer], 0, bars.len(), lo, hi),
        (lo, hi),
        "a pattern's signal series must not touch the price extent"
    );

    // A real price overlay still folds: seeding with ±inf leaves only its own values.
    let mut sma = ChartStudy::new(2, vike_indicators::get("sma").unwrap(), &bars);
    sma.set_params(vec![2.0], &bars);
    let (slo, shi) = fold_overlay_extent(&[sma], 0, bars.len(), f64::INFINITY, f64::NEG_INFINITY);
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
/// `crates/vike-chart/src/chart/study/overlays.rs`'s `render_overlay`, over the FULL
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

fn bar(ot: i64) -> ChartBar {
    ChartBar { t: 0.0, ot, o: 1.0, h: 2.0, l: 1.0, c: 1.5, v: 1.0 }
}

#[test]
fn reindex_aligns_overlay_by_open_time() {
    let primary = vec![bar(100), bar(200), bar(300), bar(400)];
    let overlay = vec![bar(150), bar(250), bar(410)]; // different grid + leading gap
    let index = reindex_by_ot(&primary, &overlay, 0, 4);
    // ot=100 -> no overlay bar at/before 100 -> None; 200 -> overlay[0] (150);
    // 300 -> overlay[1] (250); 400 -> overlay[1] (250, 410 is after)
    assert_eq!(index, vec![None, Some(0), Some(1), Some(1)]);
}

#[test]
fn reindex_empty_overlay_is_all_none() {
    let primary = vec![bar(100), bar(200), bar(300)];
    let overlay: Vec<ChartBar> = Vec::new();
    let index = reindex_by_ot(&primary, &overlay, 0, 3);
    assert_eq!(index, vec![None, None, None]);
}

#[test]
fn reindex_single_bar_overlay() {
    let primary = vec![bar(100), bar(200), bar(300), bar(400)];
    let overlay = vec![bar(250)];
    let index = reindex_by_ot(&primary, &overlay, 0, 4);
    // Before 250 there's no floor -> None; at/after 250 it's always overlay[0].
    assert_eq!(index, vec![None, None, Some(0), Some(0)]);
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
