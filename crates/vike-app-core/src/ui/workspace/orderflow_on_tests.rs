use super::*;

#[test]
fn fresh_window_is_off() {
    // DEFAULT: a fresh chart has every orderflow input off → zero change (Task 7's
    // default-off invariant, mirrored from vike-chart's `ChartInputs` fields).
    assert!(!chart_window("BTCUSDT", "1m").orderflow_on());
}

#[test]
fn either_toggle_turns_it_on_regardless_of_interval() {
    // The literal formula (task-7-brief step 3, restated with explicit parens by the
    // orchestrator) does NOT Kline-gate `cvd_on`/`profile_on` — only the Footprint-style
    // arm is Kline-gated. Pin that asymmetry down explicitly, incl. on a tick interval.
    let mut w = chart_window("BTCUSDT", "1m");
    w.cvd_on = true;
    assert!(w.orderflow_on());

    let mut w = chart_window("BTCUSDT", "100t");
    w.profile_on = true;
    assert!(w.orderflow_on(), "toggles are on regardless of Kline vs tick/volume interval");
}

#[test]
fn footprint_style_needs_a_kline_interval() {
    let mut w = chart_window("BTCUSDT", "1m");
    w.style = vike_chart::chart::style::ChartStyle::Footprint;
    assert!(w.orderflow_on(), "Footprint style on a Kline interval turns orderflow on");

    let mut w = chart_window("BTCUSDT", "100t"); // tick interval — out of scope for the Footprint STYLE
    w.style = vike_chart::chart::style::ChartStyle::Footprint;
    assert!(!w.orderflow_on(), "Footprint style on a non-Kline interval must NOT turn it on");

    let mut w = chart_window("BTCUSDT", "10v"); // volume interval — same restriction
    w.style = vike_chart::chart::style::ChartStyle::Footprint;
    assert!(!w.orderflow_on());
}

#[test]
fn candles_style_on_a_kline_interval_stays_off() {
    assert!(!chart_window("BTCUSDT", "1m").orderflow_on()); // style defaults to Candles in `WinState::new`
}
