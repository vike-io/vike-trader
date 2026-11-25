//! **A chart under another window takes none of that window's input.** The owner scrolled in
//! Studio's editor, a tool window floating over a BYBIT:BTCUSDT.P chart, and the chart behind it
//! zoomed and panned with every wheel turn (2026-11-17): its wheel zoom asked whether the pointer was
//! inside the chart's rect, never whether the chart's window was the one under the pointer.
//!
//! Both windows here are `Order::Middle` `egui::Window`s, as every workspace window is
//! (`crates/vike-app-core/src/ui/window/state/chrome.rs`'s `show_window`), the chart's drawn FIRST
//! (a window's place in the app's list is not its place on screen) and the tool window on top.

use egui::{Context, Event, Id, LayerId, Order, PointerButton, Pos2, Rect, pos2, vec2};
use vike_chart::chart::{
    draw,
    harness::{ChartHeadless, bind_chart_font_families},
    style::ChartStyle,
};
use vike_chart::test_support::make_state;
use vike_chart::{ChartInputs, ChartNavigation, ChartSettings};

use crate::common::wave_bars;

/// The chart window: the whole left of the screen.
const CHART: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(900.0, 620.0));
/// The tool window, over the chart's right half (its price pane included).
const TOOL: Rect = Rect::from_min_max(pos2(450.0, 60.0), pos2(850.0, 460.0));

/// What one scenario saw: the chart's visible bars before and after, whether the chart reported
/// an interaction on any frame, and how far the tool window's own list scrolled.
struct Seen {
    before: Option<(i64, i64)>,
    after: Option<(i64, i64)>,
    interacted: bool,
    tool_scrolled: f32,
}

/// Settle both windows with the pointer at `at`, turn the wheel there, then drag from there.
fn wheel_and_drag_at(at: Pos2) -> Seen {
    let context = Context::default();
    bind_chart_font_families(&context);
    let state = make_state(wave_bars(400));
    let options = ChartSettings::default();
    let mut chart = ChartHeadless::default();
    let (chart_id, tool_id) = (Id::unique("chart"), Id::unique("tool"));
    let tool_layer = LayerId::new(Order::Middle, tool_id);
    // Down the editor, as the owner scrolled it (over the chart that is a zoom OUT).
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    };
    let press = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    // Frames 0-2 seed the full-series view, 3 zooms in (so a zoom out shows), 4-5 settle (the view
    // before), 6 turns the wheel, 7-26 let the smoothed scroll land, 27-30 drag 120pt left, 31-34
    // settle again.
    let mut seen = Seen { before: None, after: None, interacted: false, tool_scrolled: 0.0 };
    for frame in 0..35u32 {
        let mut events = vec![Event::PointerMoved(at)];
        match frame {
            6 => events.push(wheel.clone()),
            27 => events.push(press(true)),
            28 | 29 => {
                let x = at.x - 60.0 * (frame - 27) as f32;
                events = vec![Event::PointerMoved(pos2(x, at.y))];
            }
            30 => events = vec![Event::PointerMoved(pos2(at.x - 120.0, at.y)), press(false)],
            _ => {}
        }
        let navigation = (frame == 3).then_some(ChartNavigation::ZoomIn);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
            time: Some(f64::from(frame) / 60.0),
            events,
            ..Default::default()
        };
        let mut out = context.run_ui(raw, |ui| {
            let context = ui.ctx().clone();
            let acts = egui::Window::new("chart")
                .id(chart_id)
                .title_bar(false)
                .movable(false)
                .resizable(false)
                .fixed_pos(CHART.min)
                .fixed_size(CHART.size())
                .show(&context, |ui| {
                    let inputs = chart.inputs(&state, ChartStyle::Candles, &[], &options);
                    draw(ui, ChartInputs { navigation, ..inputs })
                })
                .and_then(|w| w.inner)
                .expect("the chart window is open");
            let tool = egui::Window::new("tool")
                .id(tool_id)
                .title_bar(false)
                .movable(false)
                .resizable(false)
                .fixed_pos(TOOL.min)
                .fixed_size(TOOL.size())
                .show(&context, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for line in 0..200 {
                            ui.label(format!("fn on_bar(strategy) // line {line}"));
                        }
                    })
                })
                .and_then(|w| w.inner)
                .expect("the tool window is open");
            context.move_to_top(tool_layer);
            seen.tool_scrolled = tool.state.clamped_offset().y;
            seen.interacted |= frame >= 6 && acts.interacted;
            if frame == 5 {
                seen.before = acts.visible_timestamp;
            }
            seen.after = acts.visible_timestamp;
        });
        out.textures_delta.clear();
        if frame == 5 {
            let over_tool = context.layer_id_at(at) == Some(tool_layer);
            assert_eq!(over_tool, TOOL.contains(at), "the tool window is on top where it lies");
        }
    }
    let first_bar = state.bars[0].ot;
    assert!(
        seen.before.is_some_and(|(first, _)| first > first_bar),
        "the chart is zoomed in before the wheel, so a zoom out would show: {:?}",
        seen.before
    );
    seen
}

#[test]
fn a_wheel_and_a_drag_over_a_window_above_the_chart_leave_the_chart_alone() {
    // Over the tool window, which covers the chart's price pane there.
    let at = pos2(600.0, 200.0);
    assert!(CHART.contains(at) && TOOL.contains(at), "the fixture point is on both windows");
    let seen = wheel_and_drag_at(at);
    assert!(seen.tool_scrolled > 0.0, "the wheel scrolls the window under the pointer");
    assert!(!seen.interacted, "the chart under it must report no interaction");
    assert_eq!(seen.after, seen.before, "the chart under it must not zoom or pan");
}

#[test]
fn the_same_wheel_and_drag_on_the_uncovered_chart_move_it() {
    // NON-VACUITY: the same input where nothing covers the chart's price pane moves the chart, so
    // the test above holds because the window above took the input, not because none arrived.
    let at = pos2(200.0, 200.0);
    assert!(CHART.contains(at) && !TOOL.contains(at));
    let seen = wheel_and_drag_at(at);
    assert_eq!(seen.tool_scrolled, 0.0, "the tool window is not under the pointer");
    assert!(seen.interacted, "the chart under the pointer reports the interaction");
    assert_ne!(seen.after, seen.before, "the chart under the pointer zooms and pans");
}
