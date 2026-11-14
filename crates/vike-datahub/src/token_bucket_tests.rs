//! Golden timelines for [`Bucket`]: the exact admit/refuse sequence the seed lane's and the catalog
//! lane's former private copies produced, each under its own lane's constants, plus the edges.

use super::*;

use crate::catalog::{CATALOG_VENUE_BURST, CATALOG_VENUE_REFILL};
use crate::seed::{SEED_VENUE_BURST, SEED_VENUE_REFILL};

/// Run `take` at each `ms` offset from `t0` and collect the answers.
fn timeline(bucket: &mut Bucket, t0: Instant, offsets_ms: &[u64]) -> Vec<bool> {
    offsets_ms.iter().map(|&ms| bucket.take(t0 + Duration::from_millis(ms))).collect()
}

#[test]
fn the_seed_lane_timeline_is_unchanged() {
    // 8 back-to-back, then one per 5 s, remainder carried across a late poll (12 s → the 10 s
    // period boundary, so 15 s already holds the next token).
    let t0 = Instant::now();
    let mut b = Bucket::fresh(t0, SEED_VENUE_BURST, SEED_VENUE_REFILL);
    let mut offsets = vec![0u64; 8];
    offsets.extend([0, 4_999, 5_000, 5_000, 12_000, 14_999, 15_000]);
    let mut expected = vec![true; 8];
    expected.extend([false, false, true, false, true, false, true]);
    assert_eq!((SEED_VENUE_BURST, SEED_VENUE_REFILL), (8, Duration::from_secs(5)));
    assert_eq!(timeline(&mut b, t0, &offsets), expected);
    assert_eq!(b.last_refill, t0 + Duration::from_millis(15_000));
    assert_eq!(b.tokens, 0);
}

#[test]
fn the_catalog_lane_timeline_is_unchanged() {
    let t0 = Instant::now();
    let mut b = Bucket::fresh(t0, CATALOG_VENUE_BURST, CATALOG_VENUE_REFILL);
    let offsets = [0, 0, 0, 59_999, 60_000, 60_000, 150_000, 179_999, 180_000];
    let expected = [true, true, false, false, true, false, true, false, true];
    assert_eq!((CATALOG_VENUE_BURST, CATALOG_VENUE_REFILL), (2, Duration::from_secs(60)));
    assert_eq!(timeline(&mut b, t0, &offsets), expected);
    assert_eq!(b.last_refill, t0 + Duration::from_millis(180_000));
}

#[test]
fn a_poll_faster_than_the_refill_keeps_the_remainder() {
    // The classic bucket bug, pinned on the clock itself: advancing to `now` at 1.2 periods would
    // leave 0.8 of a period at 2.0 and refuse; advancing by whole periods leaves exactly one.
    let t0 = Instant::now();
    let p = Duration::from_secs(5);
    let mut b = Bucket::fresh(t0, 1, p);
    assert!(b.take(t0));
    assert!(!b.take(t0 + Duration::from_millis(3_000)));
    assert!(b.take(t0 + Duration::from_millis(6_000)));
    assert_eq!(b.last_refill, t0 + p, "advanced by WHOLE periods, not to `now`");
    assert!(b.take(t0 + p * 2));
    assert!(!b.take(t0 + p * 2));
}

#[test]
fn a_long_idle_refills_to_the_burst_and_no_further() {
    let t0 = Instant::now();
    let p = Duration::from_secs(5);
    let mut b = Bucket::fresh(t0, 3, p);
    for _ in 0..3 {
        assert!(b.take(t0));
    }
    assert!(!b.take(t0));
    let later = t0 + p * 1_000 + Duration::from_millis(1_234);
    assert_eq!(timeline(&mut b, later, &[0, 0, 0, 0]), [true, true, true, false]);
    assert_eq!(b.last_refill, t0 + p * 1_000, "the 1.234 s remainder is kept");
}

#[test]
fn a_clock_reading_before_the_last_refill_accrues_nothing_and_spends_the_balance() {
    // `saturating_duration_since`: an earlier `now` is zero elapsed, never a panic or a refill.
    let t0 = Instant::now();
    let start = t0 + Duration::from_secs(10);
    let mut b = Bucket::fresh(start, 2, Duration::from_secs(5));
    assert_eq!(timeline(&mut b, t0, &[0, 0, 0]), [true, true, false]);
    assert_eq!(b.last_refill, start);
}
