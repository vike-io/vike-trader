use std::time::Duration;

use super::*;

#[test]
fn reconnect_backoff_doubles_and_caps() {
    let (base, max) = (Duration::from_millis(250), Duration::from_secs(5));
    let delays: Vec<Duration> = (0..8).map(|n| reconnect_delay(base, max, n)).collect();
    assert_eq!(delays[0], base);
    assert_eq!(delays[1], Duration::from_millis(500));
    assert_eq!(delays[2], Duration::from_secs(1));
    assert!(delays.iter().all(|d| *d <= max));
    assert_eq!(*delays.last().unwrap(), max);
    assert_eq!(reconnect_delay(base, max, u32::MAX), max, "an absurd attempt count saturates");
}
