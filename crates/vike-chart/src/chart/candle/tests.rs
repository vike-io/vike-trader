//! The GPU candle seam against the egui painter's own geometry.

use super::{ChartCandleGpuItem, ChartCandleInstance, candle_geom};
use crate::chart::state::bar::ChartBar;
use egui::{Color32, Rect};
use egui_plot::{PlotBounds, PlotItem, PlotPoint, PlotTransform};

/// GPU/egui parity by construction: the `ChartCandleInstance`s `ChartCandleGpuItem` emits (through
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
        ChartBar { t: 5.0, ot: 0, o: 100.0, h: 110.0, l: 95.0, c: 108.0, v: 3.0 }, // bull
        ChartBar { t: 6.0, ot: 0, o: 108.0, h: 112.0, l: 101.0, c: 102.0, v: 2.0 }, // bear
    ];
    let options = crate::chart::settings::ChartSettings::default();
    // `colors`, not `c`: the loop below binds `c` to each candle's colour.
    let colors = crate::chart::colors::ChartColors::resolve(
        &options,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let map = |y: f64| y; // Linear/identity — the SAME map fed to both paths.

    // Capture the exact Vec<ChartCandleInstance> ChartCandleGpuItem hands to its build hook.
    let captured: std::cell::RefCell<Vec<ChartCandleInstance>> =
        std::cell::RefCell::new(Vec::new());
    let build = |instruments: Vec<ChartCandleInstance>, _rect: Rect| -> egui::Shape {
        *captured.borrow_mut() = instruments;
        egui::Shape::Noop
    };
    let item = ChartCandleGpuItem::new(&bars, true, &map, &colors, false, &build);

    // Drive the REAL PlotItem::shapes through a headless egui Ui; it must push exactly one
    // Shape (the opaque GPU callback) at the candle z-slot.
    let n_shapes = std::cell::Cell::new(usize::MAX);
    egui::__run_test_ui(|ui| {
        let mut out: Vec<egui::Shape> = Vec::new();
        PlotItem::shapes(&item, ui, &transform, &mut out);
        n_shapes.set(out.len());
    });
    assert_eq!(n_shapes.get(), 1, "ChartCandleGpuItem emits exactly one Shape (the GPU callback)");

    let instruments = captured.borrow();
    assert_eq!(instruments.len(), bars.len(), "one ChartCandleInstance per bar");

    // color_bars_previous_close defaults false → previous_close is ignored, so recomputing
    // with `None` matches the ChartCandleGpuItem path's per-bar prev tracking exactly.
    for (bar, instrument) in bars.iter().zip(instruments.iter()) {
        let g = candle_geom(bar, None, true, &map, &colors, false);
        let at = |xy: [f64; 2]| transform.position_from_point(&PlotPoint::new(xy[0], xy[1]));
        let lo_left = at(g.body[0]); // [t-CANDLE_BODY_HALF_WIDTH, lo]
        let lo_right = at(g.body[1]); // [t+CANDLE_BODY_HALF_WIDTH, lo]
        let hi_left = at(g.body[3]); // [t-CANDLE_BODY_HALF_WIDTH, hi]
        let low = at(g.wick[0]); // [t, map(low)]
        let high = at(g.wick[1]); // [t, map(high)]
        assert_eq!(instrument.x_lo, lo_left.x, "left body edge x");
        assert_eq!(instrument.x_hi, lo_right.x, "right body edge x");
        assert_eq!(instrument.body_bot, lo_left.y, "body bottom (lower price) screen y");
        assert_eq!(instrument.body_top, hi_left.y, "body top (higher price) screen y");
        assert_eq!(instrument.wick_bot, low.y, "wick bottom (low) screen y");
        assert_eq!(instrument.wick_top, high.y, "wick top (high) screen y");
        let c = g.color;
        assert_eq!(
            instrument.color,
            [
                c.r() as f32 / 255.0,
                c.g() as f32 / 255.0,
                c.b() as f32 / 255.0,
                c.a() as f32 / 255.0,
            ],
            "instance color mirrors candle_geom color",
        );
        let bull = bar.c >= bar.o;
        assert_eq!(instrument.filled, u32::from(!bull), "hollow-style bull body is unfilled");
    }
}

/// Review Focus 4, the GPU half: the instances vike-desktop's candle layer draws carry the
/// resolved market colours — the same `candle_geom` the egui path paints from.
#[test]
fn gpu_instances_carry_the_resolved_market_colours() {
    use vike_ui_theme::appearance::ThemeAppearance;
    use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
    let frame = Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(410.0, 320.0));
    let bounds = PlotBounds::from_min_max([-1.0, 90.0], [50.0, 130.0]);
    let transform = PlotTransform::new(frame, bounds, false);
    let bars = vec![
        ChartBar { t: 5.0, ot: 0, o: 100.0, h: 110.0, l: 95.0, c: 108.0, v: 3.0 }, // bull
        ChartBar { t: 6.0, ot: 0, o: 108.0, h: 112.0, l: 101.0, c: 102.0, v: 2.0 }, // bear
    ];
    let a =
        ThemeAppearance { market: ThemeMarketColorsId::ColourBlind, ..ThemeAppearance::default() };
    let colors = crate::chart::colors::ChartColors::resolve(
        &crate::chart::settings::ChartSettings::default(),
        &a,
    );
    let map = |y: f64| y;
    let captured: std::cell::RefCell<Vec<ChartCandleInstance>> =
        std::cell::RefCell::new(Vec::new());
    let build = |instruments: Vec<ChartCandleInstance>, _rect: Rect| -> egui::Shape {
        *captured.borrow_mut() = instruments;
        egui::Shape::Noop
    };
    let item = ChartCandleGpuItem::new(&bars, false, &map, &colors, false, &build);
    egui::__run_test_ui(|ui| {
        let mut out: Vec<egui::Shape> = Vec::new();
        PlotItem::shapes(&item, ui, &transform, &mut out);
    });
    let rgba = |x: Color32| [x.r(), x.g(), x.b(), x.a()].map(|v| f32::from(v) / 255.0);
    let cb = ThemeMarketColors::of(ThemeMarketColorsId::ColourBlind);
    let got: Vec<[f32; 4]> = captured.borrow().iter().map(|i| i.color).collect();
    assert_eq!(got, vec![rgba(cb.up), rgba(cb.down)], "bull then bear, in the Colour-blind set");
}
