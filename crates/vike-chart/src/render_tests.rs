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
        crate::indicators::get(name).unwrap().outputs.iter().map(|o| o.style).collect::<Vec<_>>()
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
    let opts = crate::options::ChartOptions::default();
    // `colors`, not `c`: the loop below binds `c` to each candle's colour.
    let colors = crate::colors::ChartColors::resolve(
        &opts,
        &vike_ui_theme::appearance::Appearance::default(),
    );
    let map = |y: f64| y; // Linear/identity — the SAME map fed to both paths.

    // Capture the exact Vec<CandleInstance> GpuCandleItem hands to its build hook.
    let captured: std::cell::RefCell<Vec<CandleInstance>> = std::cell::RefCell::new(Vec::new());
    let build = |insts: Vec<CandleInstance>, _rect: Rect| -> egui::Shape {
        *captured.borrow_mut() = insts;
        egui::Shape::Noop
    };
    let item = GpuCandleItem::new(&bars, true, &map, &colors, false, &build);

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
        let g = candle_geom(bar, None, true, &map, &colors, false);
        let at = |xy: [f64; 2]| transform.position_from_point(&PlotPoint::new(xy[0], xy[1]));
        let lo_left = at(g.body[0]); // [t-CANDLE_BODY_HALF_W, lo]
        let lo_right = at(g.body[1]); // [t+CANDLE_BODY_HALF_W, lo]
        let hi_left = at(g.body[3]); // [t-CANDLE_BODY_HALF_W, hi]
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

/// Review Focus 4, the GPU half: the instances vike-desktop's candle layer draws carry the
/// resolved market colours — the same `candle_geom` the egui path paints from.
#[test]
fn gpu_instances_carry_the_resolved_market_colours() {
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::market::{MarketColors, MarketId};
    let frame = Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(410.0, 320.0));
    let bounds = PlotBounds::from_min_max([-1.0, 90.0], [50.0, 130.0]);
    let transform = PlotTransform::new(frame, bounds, false);
    let bars = vec![
        Bar { t: 5.0, ot: 0, o: 100.0, h: 110.0, l: 95.0, c: 108.0, v: 3.0 }, // bull
        Bar { t: 6.0, ot: 0, o: 108.0, h: 112.0, l: 101.0, c: 102.0, v: 2.0 }, // bear
    ];
    let a = Appearance { market: MarketId::ColourBlind, ..Appearance::default() };
    let colors = crate::colors::ChartColors::resolve(&crate::options::ChartOptions::default(), &a);
    let map = |y: f64| y;
    let captured: std::cell::RefCell<Vec<CandleInstance>> = std::cell::RefCell::new(Vec::new());
    let build = |insts: Vec<CandleInstance>, _rect: Rect| -> egui::Shape {
        *captured.borrow_mut() = insts;
        egui::Shape::Noop
    };
    let item = GpuCandleItem::new(&bars, false, &map, &colors, false, &build);
    egui::__run_test_ui(|ui| {
        let mut out: Vec<egui::Shape> = Vec::new();
        PlotItem::shapes(&item, ui, &transform, &mut out);
    });
    let rgba = |x: Color32| [x.r(), x.g(), x.b(), x.a()].map(|v| f32::from(v) / 255.0);
    let cb = MarketColors::of(MarketId::ColourBlind);
    let got: Vec<[f32; 4]> = captured.borrow().iter().map(|i| i.color).collect();
    assert_eq!(got, vec![rgba(cb.up), rgba(cb.down)], "bull then bear, in the Colour-blind set");
}

/// The chart-style menu's icons preview what the chart draws: the installed market colours,
/// never the old candle mint and rose.
#[test]
fn the_style_icons_follow_the_market_colours() {
    use vike_ui_theme::appearance::{Appearance, apply};
    use vike_ui_theme::market::{MarketColors, MarketId};
    let ctx = egui::Context::default();
    apply(&ctx, &Appearance { market: MarketId::ColourBlind, ..Appearance::default() });
    let r = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(20.0, 20.0));
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        draw_style_icon(ui.painter(), r, ChartStyle::Candles);
    });
    out.textures_delta.clear(); // egui 0.36 panics on dropping unapplied deltas
    let fills: Vec<Color32> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(rect) => Some(rect.fill),
            _ => None,
        })
        .collect();
    let cb = MarketColors::of(MarketId::ColourBlind);
    assert!(fills.contains(&cb.up) && fills.contains(&cb.down), "{fills:?}");
    for old in [Color32::from_rgb(91, 190, 145), Color32::from_rgb(217, 84, 88)] {
        assert!(!fills.contains(&old), "the old candle colour {old:?} is still an icon's");
    }
}
