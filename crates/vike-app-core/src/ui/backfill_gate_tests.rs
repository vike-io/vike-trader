use super::{BackfillRetries, should_spawn_backfill};
use std::collections::HashSet;

/// `of_backfill_hours <= 0.0` (the SP2-identical default-off floor) must never spawn, for
/// ANY symbol, and must never even touch `bf_spawned` — `global-constraints.md`'s "default =
/// SP2 behavior: `of_backfill_hours = 0.0` ⇒ no backfill thread spawned ⇒ byte-identical to
/// SP2". The retry lane must not weaken that: the off-switch returns before either set is read.
#[test]
fn zero_or_negative_backfill_hours_never_spawns_and_never_touches_bf_spawned() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(!should_spawn_backfill(0.0, &mut bf_spawned, &mut retries, "BTCUSDT"));
    assert!(!should_spawn_backfill(-1.0, &mut bf_spawned, &mut retries, "ETHUSDT"));
    assert!(bf_spawned.is_empty(), "hours <= 0.0 must not record ANY symbol into bf_spawned");
    assert!(retries.is_empty(), "hours <= 0.0 must not record ANY symbol into the retry lane");
}

/// NaN/±inf must also hit the off-switch — a bare `<= 0.0` alone lets NaN slip through
/// (every `<=` comparison against NaN is false), which would have wrongly spawned a
/// backfill thread for a corrupt/hand-edited `workspace.json`'s `of_backfill_hours`. Covers
/// the SP3-review NaN-guard follow-up.
#[test]
fn non_finite_backfill_hours_never_spawns_and_never_touches_bf_spawned() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(!should_spawn_backfill(f64::NAN, &mut bf_spawned, &mut retries, "BTCUSDT"));
    assert!(!should_spawn_backfill(f64::INFINITY, &mut bf_spawned, &mut retries, "ETHUSDT"));
    assert!(!should_spawn_backfill(f64::NEG_INFINITY, &mut bf_spawned, &mut retries, "SOLUSDT"));
    assert!(bf_spawned.is_empty(), "non-finite hours must not record ANY symbol into bf_spawned");
    assert!(retries.is_empty(), "non-finite hours must not record ANY symbol into the retry lane");
}

/// A positive `of_backfill_hours` spawns exactly once per symbol (the run-once gate): the
/// first call for a symbol returns `true` and records it; every later call for that SAME
/// symbol returns `false` (re-enabling orderflow on an already-spawned symbol does not
/// refetch); a DIFFERENT symbol is still independent. Unchanged by the retry lane, which is
/// only ever consulted once `bf_spawned` has already said no AND a worker has reported a
/// failure — neither of which happens here.
#[test]
fn positive_backfill_hours_spawns_exactly_once_per_symbol() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "BTCUSDT"));
    assert!(bf_spawned.contains("BTCUSDT"));
    assert!(
        !should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "BTCUSDT"),
        "a second call for the same symbol must not re-spawn"
    );
    assert!(
        should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "ETHUSDT"),
        "a different symbol must still spawn independently"
    );
    assert_eq!(bf_spawned.len(), 2);
    assert!(retries.is_empty(), "a healthy run-once spawn records nothing in the retry lane");
}
