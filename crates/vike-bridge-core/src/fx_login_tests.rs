use std::time::Duration;

use super::*;

/// Epoch ms of a November 2026 UTC instant (the 13th is a Friday, the 15th the Sunday the FX week opens).
fn nov(day: u32, hour: u32, minute: u32) -> i64 {
    vike_model::time::parse_utc_timestamp_ms(&format!("2026-11-{day:02}T{hour:02}:{minute:02}:00Z"))
        .expect("a real instant")
}

/// **A retry ladder never lands a login inside the closed window, and waits ONCE to the open.** A pause that ends
/// while the market is open is untouched; one that ends after Friday 22:00 (a drop at 21:58, a 10-minute pause), or
/// that starts inside the window (a drop at Friday 22:30), becomes the one wait to Sunday 21:00 UTC. A crypto venue's
/// pause is never held.
#[test]
fn a_pause_that_would_land_a_login_in_the_closed_window_waits_for_the_open() {
    let open = nov(15, 21, 0);
    let ten_minutes = Duration::from_secs(600);
    let wait_to_open =
        |from: i64| Duration::from_millis(u64::try_from(open - from).expect("before the open"));
    assert_eq!(held_pause_at("fxcm", ten_minutes, nov(11, 12, 0)), ten_minutes, "a Wednesday");
    assert_eq!(
        held_pause_at("fxcm", ten_minutes, nov(13, 21, 40)),
        ten_minutes,
        "it lands at 21:50, still open"
    );
    assert_eq!(
        held_pause_at("ctrader", ten_minutes, nov(13, 21, 58)),
        wait_to_open(nov(13, 21, 58))
    );
    assert_eq!(
        held_pause_at("ig", Duration::from_secs(1), nov(13, 22, 30)),
        wait_to_open(nov(13, 22, 30))
    );
    assert_eq!(
        held_pause_at("dukascopy", Duration::ZERO, nov(14, 12, 0)),
        wait_to_open(nov(14, 12, 0))
    );
    assert_eq!(
        held_pause_at("oanda", ten_minutes, nov(15, 20, 55)),
        ten_minutes,
        "it lands at 21:05, open"
    );
    assert_eq!(
        held_pause_at("binance", ten_minutes, nov(14, 12, 0)),
        ten_minutes,
        "crypto is never held"
    );
}

/// **The gate is OFF in a process that never armed it**: no venue is held and a ladder keeps its own pause, whatever the
/// day, so a test or a tool behaves as it always did. (No test in this binary arms it.)
#[test]
fn an_unarmed_gate_holds_nothing() {
    assert_eq!(closed_until("fxcm"), None);
    assert_eq!(closed_words("dukascopy"), None);
    assert_eq!(held_pause("ig", Duration::from_secs(3)), Duration::from_secs(3));
}
