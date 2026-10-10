use super::*;
use egui::{Rect, pos2, vec2};

fn chart_win(symbol: &str) -> WinState {
    let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
    WinState::new("t", symbol, "1m", WinKind::Chart, r)
}

#[test]
fn compare_add_dedups_and_orders() {
    let mut w = WinState::default();
    assert!(w.compare.is_empty(), "a fresh window has no compare overlays");
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    w.add_compare("ETHUSDT"); // duplicate → no-op
    assert_eq!(w.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
    w.remove_compare("ETHUSDT");
    assert_eq!(w.compare, vec!["SOLUSDT".to_string()]);
}

#[test]
fn add_compare_ignores_the_windows_own_symbol() {
    let mut w = chart_win("BTCUSDT");
    w.add_compare("BTCUSDT"); // exact self
    w.add_compare("btcusdt"); // case-insensitive self
    assert!(w.compare.is_empty(), "a chart must never overlay its own symbol");
    w.add_compare("ETHUSDT");
    assert_eq!(w.compare, vec!["ETHUSDT".to_string()]);
}

#[test]
fn first_compare_forces_percent_scale_second_does_not_reflip() {
    // Task 3 gates overlay render on Percent mode, so the FIRST overlay must
    // auto-switch a Linear window to Percent (otherwise the overlay is invisible).
    let mut w = chart_win("BTCUSDT");
    assert_eq!(w.scale, vike_chart::ScaleMode::Linear);
    w.add_compare("ETHUSDT");
    assert_eq!(w.scale, vike_chart::ScaleMode::Percent, "first overlay auto-switches to Percent");
    // A subsequent add must NOT re-touch the scale — the user may switch back manually.
    w.scale = vike_chart::ScaleMode::Log;
    w.add_compare("SOLUSDT");
    assert_eq!(w.scale, vike_chart::ScaleMode::Log, "second overlay must not re-flip the scale");
}

#[test]
fn ignored_self_add_does_not_count_as_first_overlay() {
    // An add that's ignored (own symbol) must NOT trigger the first-overlay
    // auto-Percent flip — the overlay list is still empty afterward.
    let mut w = chart_win("BTCUSDT");
    w.add_compare("BTCUSDT");
    assert_eq!(w.scale, vike_chart::ScaleMode::Linear, "ignored add must not flip scale");
    assert!(w.compare.is_empty());
}

/// C2 tidy FIX 1: switching a window's PRIMARY symbol onto one that is already a Compare
/// overlay must drop the now-redundant chip. This is the `WinState`-level primitive the
/// fix relies on (`main.rs`'s title-bar `new_symbol` handler calls `remove_compare` right
/// before reassigning `w.symbol` — that call site is UI-wired egui code, not unit-testable
/// without a full `App`/`eframe::CreationContext`, so this test exercises the same sequence
/// directly against `WinState`, standing in for the handler).
#[test]
fn switching_primary_to_an_already_compared_symbol_drops_the_stale_chip() {
    let mut w = chart_win("BTCUSDT");
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    assert_eq!(w.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);

    // Mirror main.rs's `new_symbol` handler: remove_compare(&new_symbol) BEFORE reassigning
    // `w.symbol`, so the (now-redundant) chip is dropped instead of lingering.
    let new_symbol = "ETHUSDT".to_string();
    w.remove_compare(&new_symbol);
    w.symbol = new_symbol;

    assert_eq!(w.compare, vec!["SOLUSDT".to_string()], "the matching chip must be gone");
    assert!(!w.compare.iter().any(|s| s == &w.symbol), "no chip may match the new primary");
}
