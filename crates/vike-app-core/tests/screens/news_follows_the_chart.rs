//! **The News window's Follow chart follows the focused chart**: the chart window highest in egui's
//! window order (`vike_app_core::ui::window::focused_chart`, which `show_window` feeds), handed
//! to the window by `WindowToolContext::news_inputs` beside the catalog. How the window reads the chart's
//! asset through that catalog, once per chart, and which headlines name it are `vike-news`'s own
//! tests (`crates/vike-news/src/follow_tests.rs`).

use egui::{Context, LayerId, Order, RawInput, Rect, Vec2, pos2, vec2};
use vike_app_core::ui::window::{WindowKind, WindowState, show_window};
use vike_execution::CoreSnapshot;

use crate::tool_context::Owned;

fn screen() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0))
}

fn chart(id: &str, venue: &str, symbol: &str) -> WindowState {
    let rect = Rect::from_min_size(pos2(40.0, 40.0), vec2(600.0, 400.0));
    let mut w = WindowState::new(id, symbol, "1m", WindowKind::Chart, rect);
    w.venue = venue.to_string();
    w
}

/// One frame drawing `windows` in order: a window shown for the first time opens on top.
fn frame(context: &Context, windows: &mut [WindowState]) {
    context.begin_pass(RawInput { screen_rect: Some(screen()), ..Default::default() });
    for w in windows.iter_mut() {
        show_window(context, w, screen(), |_, _| Vec2::ZERO);
    }
    context.end_pass().drop_without_applying_deltas();
}

/// The chart on top is followed; a click on another chart moves the follow there, a window that
/// is not a chart never takes it, and a closed chart drops out.
#[test]
fn follow_chart_follows_the_chart_window_on_top() {
    let owned = Owned::new(CoreSnapshot::empty("", ""));
    let followed = |context: &Context| owned.context().news_inputs(context).chart;
    let pair = |venue: &str, symbol: &str| Some((venue.to_string(), symbol.to_string()));
    let context = Context::default();
    assert_eq!(followed(&context), None, "no chart on screen: nothing to follow");

    let news = Rect::from_min_size(pos2(700.0, 40.0), vec2(500.0, 400.0));
    let mut windows = [
        chart("btc", "binance", "BTCUSDT"),
        chart("eth", "bybit", "ETHUSDT"),
        WindowState::tool("news", WindowKind::News, news),
    ];
    frame(&context, &mut windows);
    assert_eq!(
        followed(&context),
        pair("bybit", "ETHUSDT"),
        "the top chart; the News window above it does not count"
    );

    context.move_to_top(LayerId::new(Order::Middle, windows[0].id));
    frame(&context, &mut windows);
    assert_eq!(followed(&context), pair("binance", "BTCUSDT"), "a click brings the other chart up");

    windows[0].symbol = "SOLUSDT".to_string();
    frame(&context, &mut windows);
    assert_eq!(
        followed(&context),
        pair("binance", "SOLUSDT"),
        "the chart's new symbol, next frame"
    );

    // The first chart closes: the shell stops drawing it.
    frame(&context, &mut windows[1..]);
    frame(&context, &mut windows[1..]);
    assert_eq!(
        followed(&context),
        pair("bybit", "ETHUSDT"),
        "a closed chart is followed no longer"
    );
}
