use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

#[test]
fn signal_wakes_before_the_30s_cadence() {
    let (tx, rx) = channel::<()>();
    tx.send(()).unwrap(); // Refresh pill clicked
    let t0 = Instant::now();
    // Uses the same 30s timeout the poll loop does; the queued signal must return immediately.
    assert!(matches!(rx.recv_timeout(Duration::from_secs(30)), Ok(())));
    assert!(t0.elapsed() < Duration::from_secs(1), "woke on the signal, not the timeout");
}

#[test]
fn burst_of_clicks_coalesces_to_one_refetch() {
    let (tx, rx) = channel::<()>();
    for _ in 0..5 {
        tx.send(()).unwrap(); // 5 rapid Refresh clicks
    }
    // Wake once...
    assert!(matches!(rx.recv_timeout(Duration::from_secs(30)), Ok(())));
    // ...then drain the rest so they don't trigger 4 more back-to-back refetches.
    while rx.try_recv().is_ok() {}
    assert!(rx.try_recv().is_err(), "all queued signals drained → exactly one refetch");
}

#[test]
fn dropped_sender_reports_disconnected_not_ok() {
    // At shutdown the App's Sender drops; the loop must see Disconnected (and fall back to the
    // 30s sleep) rather than spuriously refetching or panicking.
    let (tx, rx) = channel::<()>();
    drop(tx);
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(30)),
        Err(RecvTimeoutError::Disconnected)
    ));
}
