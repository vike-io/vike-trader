use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use super::{OptionsLink, Wake, wait_for_next_poll};

#[test]
fn a_refresh_wakes_the_thread_before_the_cadence() {
    let (tx, rx) = channel::<()>();
    OptionsLink::new(tx).refresh(); // the Refresh pill
    let t0 = Instant::now();
    assert_eq!(wait_for_next_poll(&rx, Duration::from_secs(30)), Wake::Refresh);
    assert!(t0.elapsed() < Duration::from_secs(1), "woke on the signal, not the timeout");
}

#[test]
fn a_burst_of_refreshes_coalesces_to_one_pass() {
    let (tx, rx) = channel::<()>();
    let link = OptionsLink::new(tx);
    for _ in 0..5 {
        link.refresh();
    }
    assert_eq!(wait_for_next_poll(&rx, Duration::from_secs(30)), Wake::Refresh);
    assert!(rx.try_recv().is_err(), "the four queued wakes were drained with the first");
}

#[test]
fn with_no_signal_the_cadence_elapses() {
    let (_tx, rx) = channel::<()>();
    let t0 = Instant::now();
    assert_eq!(wait_for_next_poll(&rx, Duration::from_millis(20)), Wake::Cadence);
    assert!(t0.elapsed() >= Duration::from_millis(20));
}

#[test]
fn a_closed_channel_sleeps_the_cadence_instead_of_spinning() {
    let (tx, rx) = channel::<()>();
    drop(tx);
    let t0 = Instant::now();
    assert_eq!(wait_for_next_poll(&rx, Duration::from_millis(50)), Wake::Closed);
    assert!(t0.elapsed() >= Duration::from_millis(50), "a disconnected wait must not busy-loop");
}

/// A pick the thread was not already asking for wakes it, so a click on a new expiry is asked for
/// in one round trip; a pick renewed every frame does not wake it again.
#[test]
fn a_new_pick_wakes_the_thread_and_a_renewed_one_does_not() {
    let (tx, rx) = channel::<()>();
    let link = OptionsLink::new(tx);
    link.want(Some("BTC"), Some("2026-06-26"));
    assert_eq!(wait_for_next_poll(&rx, Duration::from_millis(1)), Wake::Refresh);
    link.want(Some("BTC"), Some("2026-06-26"));
    assert_eq!(wait_for_next_poll(&rx, Duration::from_millis(1)), Wake::Cadence);
}
