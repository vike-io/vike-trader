//! The flush retry policy (`append_with_retry`), exercised without a store.

use super::*;
use crate::store::hist::DataError;

// ---- the flush retry policy (pure — no store needed, so the failure modes are exact) --------

#[test]
fn append_retry_does_not_re_run_a_success() {
    let mut calls = 0u32;
    // A long retry delay proves the success path never sleeps: a test that reached the sleep
    // would hang for 30s rather than finish instantly.
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            Ok(3)
        },
        Duration::from_secs(30),
        Duration::from_secs(1),
    );
    assert_eq!(res.expect("ok"), 3);
    assert_eq!(attempts, 1);
    assert_eq!(calls, 1, "a successful append must not be repeated");
}

#[test]
fn append_retry_recovers_a_transient_fast_failure() {
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            if calls == 1 { Err(DataError::Io("transient".into())) } else { Ok(7) }
        },
        Duration::ZERO,
        Duration::from_secs(1),
    );
    assert_eq!(res.expect("recovered"), 7, "the retry's rows are NOT lost");
    assert_eq!(attempts, 2);
    assert_eq!(calls, 2, "one failure, one retry, done");
}

#[test]
fn append_retry_gives_up_after_exactly_one_extra_attempt() {
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            Err(DataError::Io("boom".into()))
        },
        Duration::ZERO,
        Duration::from_secs(1),
    );
    assert!(res.is_err(), "a permanent failure still fails");
    assert_eq!(attempts, 2, "bounded: ONE retry, never an unbounded loop");
    assert_eq!(calls, 2, "the append ran exactly twice, then the caller discards");
}

#[test]
fn append_retry_skips_a_slow_failure() {
    // A failure that took `slow_attempt` or longer has already burned the store's own retry
    // budget (`SeriesLock::acquire` spins ~4s on a contended lock). Retrying it would mostly
    // double this single writer thread's stall — and every stalled millisecond is more rows
    // dropped at the channel behind it — so it is deliberately not retried.
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            thread::sleep(Duration::from_millis(30));
            Err(DataError::Io("timeout acquiring series lock".into()))
        },
        Duration::ZERO,
        Duration::from_millis(10), // tiny threshold keeps the test fast
    );
    assert!(res.is_err());
    assert_eq!(attempts, 1, "a slow failure is not retried");
    assert_eq!(calls, 1);
}
