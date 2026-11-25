use super::*;

/// The overlaid symbols, in add order (== colour order).
fn symbols(w: &WindowState) -> Vec<&str> {
    w.compare.iter().map(|c| c.symbol.as_str()).collect()
}

#[test]
fn compare_add_dedups_and_orders() {
    let mut w = WindowState::default();
    assert!(w.compare.is_empty(), "a fresh window has no compare overlays");
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    w.add_compare("ETHUSDT"); // duplicate → no-op
    assert_eq!(symbols(&w), ["ETHUSDT", "SOLUSDT"]);
    w.remove_compare("ETHUSDT");
    assert_eq!(symbols(&w), ["SOLUSDT"]);
}

#[test]
fn add_compare_ignores_the_windows_own_symbol() {
    let mut w = chart_window("BTCUSDT", "1m");
    w.add_compare("BTCUSDT"); // exact self
    w.add_compare("btcusdt"); // case-insensitive self
    assert!(w.compare.is_empty(), "a chart must never overlay its own symbol");
    w.add_compare("ETHUSDT");
    assert_eq!(symbols(&w), ["ETHUSDT"]);
}

#[test]
fn first_compare_forces_percent_scale_second_does_not_reflip() {
    // Task 3 gates overlay render on Percent mode, so the FIRST overlay must
    // auto-switch a Linear window to Percent (otherwise the overlay is invisible).
    let mut w = chart_window("BTCUSDT", "1m");
    assert_eq!(w.scale, vike_chart::ChartScaleMode::Linear);
    w.add_compare("ETHUSDT");
    assert_eq!(
        w.scale,
        vike_chart::ChartScaleMode::Percent,
        "first overlay auto-switches to Percent"
    );
    // A subsequent add must NOT re-touch the scale — the user may switch back manually.
    w.scale = vike_chart::ChartScaleMode::Logarithmic;
    w.add_compare("SOLUSDT");
    assert_eq!(
        w.scale,
        vike_chart::ChartScaleMode::Logarithmic,
        "second overlay must not re-flip the scale"
    );
}

#[test]
fn ignored_self_add_does_not_count_as_first_overlay() {
    // An add that's ignored (own symbol) must NOT trigger the first-overlay
    // auto-Percent flip — the overlay list is still empty afterward.
    let mut w = chart_window("BTCUSDT", "1m");
    w.add_compare("BTCUSDT");
    assert_eq!(w.scale, vike_chart::ChartScaleMode::Linear, "ignored add must not flip scale");
    assert!(w.compare.is_empty());
}

/// C2 tidy FIX 1: switching a window's PRIMARY symbol onto one that is already a Compare
/// overlay must drop the now-redundant chip. This is the `WindowState`-level primitive the
/// fix relies on (`main.rs`'s title-bar `new_symbol` handler calls `remove_compare` right
/// before reassigning `w.symbol` — that call site is UI-wired egui code, not unit-testable
/// without a full `App`/`eframe::CreationContext`, so this test exercises the same sequence
/// directly against `WindowState`, standing in for the handler).
#[test]
fn switching_primary_to_an_already_compared_symbol_drops_the_stale_chip() {
    let mut w = chart_window("BTCUSDT", "1m");
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    assert_eq!(symbols(&w), ["ETHUSDT", "SOLUSDT"]);

    // Mirror main.rs's `new_symbol` handler: remove_compare(&new_symbol) BEFORE reassigning
    // `w.symbol`, so the (now-redundant) chip is dropped instead of lingering.
    let new_symbol = "ETHUSDT".to_string();
    w.remove_compare(&new_symbol);
    w.symbol = new_symbol;

    assert_eq!(symbols(&w), ["SOLUSDT"], "the matching chip must be gone");
    assert!(!w.compare.iter().any(|c| c.symbol == w.symbol), "no chip may match the new primary");
}

/// A NEW overlay is on the chart's own venue, and it KEEPS that venue: moving the primary to
/// another venue does not move an overlay already drawn (its feed and its key stay its own).
#[test]
fn a_new_overlay_takes_the_charts_venue_and_keeps_it() {
    let mut w = chart_window("BTCUSDT", "1m");
    w.venue = "bybit".into();
    w.add_compare("ETHUSDT");
    let bybit_eth = ChartCompareSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() };
    assert_eq!(w.compare, std::slice::from_ref(&bybit_eth));
    w.venue = "okx".into();
    w.add_compare("SOLUSDT");
    let okx_sol = ChartCompareSymbol { venue: "okx".into(), symbol: "SOLUSDT".into() };
    assert_eq!(w.compare, [bybit_eth, okx_sol], "each overlay keeps the venue it was added on");
}
