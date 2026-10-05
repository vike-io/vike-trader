use super::{
    BACKFILL_MAX_RETRIES, BackfillReport, BackfillRetries, RETRY_MAX, should_spawn_backfill,
};
use std::collections::HashSet;

const SYM: &str = "BTCUSDT";
const HOURS: f64 = 2.0;

/// One frame's worth of the real gate.
fn gate(bf_spawned: &mut HashSet<String>, retries: &mut BackfillRetries) -> bool {
    should_spawn_backfill(HOURS, bf_spawned, retries, SYM)
}

/// Drive the first spawn, then feed back whatever the worker reported.
fn first_walk_then(report: BackfillReport) -> (HashSet<String>, BackfillRetries) {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(gate(&mut bf_spawned, &mut retries), "the first walk always spawns");
    retries.note_report(&report);
    (bf_spawned, retries)
}

/// **The reported bug.** A walk that stopped on a REST error having delivered nothing is
/// re-walked: the run-once guard alone said no forever, and #952 made the difference
/// detectable without anything acting on it. The re-walk is not immediate — it waits out the
/// cooldown, which is what keeps a failing endpoint from being re-asked every frame.
#[test]
fn an_errored_walk_that_delivered_nothing_is_re_walked_after_its_cooldown() {
    let (mut bf_spawned, mut retries) =
        first_walk_then(BackfillReport::failed(SYM, 0, "429 Too Many Requests"));
    assert!(retries.is_pending(SYM), "the failure is recorded, so the loss is visible");
    assert_eq!(retries.attempts(SYM), 1);

    assert!(
        !gate(&mut bf_spawned, &mut retries),
        "a frame inside the cooldown must NOT re-walk — that is the per-frame abuse guard"
    );

    retries.expire_all();
    assert!(gate(&mut bf_spawned, &mut retries), "once the cooldown elapses, the walk re-runs");
    assert!(
        bf_spawned.contains(SYM),
        "the run-once guard itself is never cleared — the retry record is what reopens it"
    );
}

/// A claimed re-walk is IN FLIGHT: the per-frame call site must not launch a second 300-page
/// walk on the very next frame (or on any of the thousands of frames the walk can take).
/// Nothing reopens the gate again until that walk reports.
#[test]
fn a_claimed_re_walk_is_not_claimed_again_while_it_is_still_running() {
    let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::failed(SYM, 0, "boom"));
    retries.expire_all();
    assert!(gate(&mut bf_spawned, &mut retries), "the re-walk is claimed once");

    for _ in 0..1_000 {
        retries.expire_all(); // even a fully elapsed clock may not claim a second walk
        assert!(!gate(&mut bf_spawned, &mut retries), "an in-flight walk must never be re-claimed");
    }
    assert_eq!(retries.attempts(SYM), 1, "an in-flight claim is not itself a failure");
}

/// **A `Capped` truncation is NOT retryable** (#952's own rule: the venue served every page it
/// was asked for, so an identical re-run caps at the same page). It must leave the guard shut,
/// exactly like a clean walk — the cure there is a bigger `max_pages`, not more requests.
#[test]
fn a_capped_walk_is_not_re_walked_and_records_nothing() {
    let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::finished(SYM, 300));
    assert!(retries.is_empty(), "a capped walk is complete-as-asked, not a failure to track");
    for _ in 0..10 {
        retries.expire_all();
        assert!(!gate(&mut bf_spawned, &mut retries), "a cap must never reopen the guard");
    }
}

/// A walk that simply succeeded is the same story with no truncation at all: one spawn, ever.
#[test]
fn a_successful_walk_is_not_re_walked() {
    let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::finished(SYM, 12));
    assert!(retries.is_empty());
    for _ in 0..10 {
        retries.expire_all();
        assert!(!gate(&mut bf_spawned, &mut retries), "success must never re-run the walk");
    }
}

/// **The safety rule.** A walk that delivered pages and THEN errored is not re-walked, however
/// retryable its stop reason: those pages are already folded into an `OrderflowAgg`, which has
/// no per-trade dedup, and this seam carries no aggTrade id to resume below them — so a second
/// walk would re-deliver and double-count them. The loss is recorded and warned, not retried.
#[test]
fn an_errored_walk_that_already_delivered_pages_is_never_re_walked() {
    let (mut bf_spawned, mut retries) =
        first_walk_then(BackfillReport::failed(SYM, 7, "connection reset"));
    assert!(retries.is_abandoned(SYM), "the chain ends, visibly");
    for _ in 0..10 {
        retries.expire_all();
        assert!(
            !gate(&mut bf_spawned, &mut retries),
            "re-walking a partially delivered walk would double-count its pages"
        );
    }
}

/// The invariant the rule above buys, stated directly: across a whole retry chain, **at most
/// one walk ever delivers ticks**. Every re-walk is claimed only after a report with
/// `pages == 0`, and the first report that carries pages ends the chain either way (success
/// clears it, failure abandons it).
#[test]
fn at_most_one_walk_in_a_chain_ever_delivers_ticks() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    let mut delivered = 0u32;
    let mut walks = 0u32;

    // Four walks that deliver nothing, then one that delivers and succeeds.
    let script = [0u32, 0, 0, 0, 9];
    for (i, pages) in script.iter().enumerate() {
        retries.expire_all();
        assert!(gate(&mut bf_spawned, &mut retries), "walk {i} must be claimable");
        walks += 1;
        delivered += *pages;
        if *pages == 0 {
            retries.note_report(&BackfillReport::failed(SYM, 0, "timeout"));
        } else {
            retries.note_report(&BackfillReport::finished(SYM, *pages));
        }
    }
    assert_eq!(walks, 5);
    assert_eq!(delivered, 9, "exactly one walk in the chain delivered anything");
    assert!(retries.is_empty(), "the successful walk closed the chain");
    retries.expire_all();
    assert!(!gate(&mut bf_spawned, &mut retries), "and nothing re-opens it afterwards");
}

/// **The bound.** A permanently broken endpoint gets `BACKFILL_MAX_RETRIES` re-walks and then
/// stops for the session — it does not spin forever. What stops it is the attempt count; what
/// is logged is one `warn!` naming the count and the last error (the `Abandoned` state below is
/// that line's observable twin).
#[test]
fn a_permanently_failing_walk_stops_after_the_bound() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(gate(&mut bf_spawned, &mut retries), "the first walk");
    retries.note_report(&BackfillReport::failed(SYM, 0, "500 Internal Server Error"));

    let mut re_walks = 0u32;
    for _ in 0..(BACKFILL_MAX_RETRIES + 5) {
        retries.expire_all();
        if !gate(&mut bf_spawned, &mut retries) {
            break;
        }
        re_walks += 1;
        retries.note_report(&BackfillReport::failed(SYM, 0, "500 Internal Server Error"));
    }
    assert_eq!(re_walks, BACKFILL_MAX_RETRIES, "exactly the bounded number of re-walks");
    assert!(retries.is_abandoned(SYM), "and then the chain is over, visibly");

    for _ in 0..10 {
        retries.expire_all();
        assert!(!gate(&mut bf_spawned, &mut retries), "an abandoned chain never re-opens");
    }
}

/// The bound is the ladder's own strictly-increasing run: the last scheduled re-walk is the
/// last one shorter than the ceiling, which is exactly where [`FeedRetries`]' unbounded lane
/// would flatten into a fixed-rate poller. Pins the constant to that argument rather than to a
/// number someone picked.
#[test]
fn the_bound_is_where_the_shared_ladder_stops_growing() {
    assert!(
        super::retry_backoff(BACKFILL_MAX_RETRIES) < RETRY_MAX,
        "the last re-walk must still be on the growing part of the ladder"
    );
    assert_eq!(
        super::retry_backoff(BACKFILL_MAX_RETRIES + 1),
        RETRY_MAX,
        "and the first attempt past the bound is the one the ceiling would flatten"
    );
}

/// A blip that heals: three failures, then the fourth walk completes. The record clears
/// entirely (that is the one `info!` recovery line), so a LATER unrelated failure on the same
/// symbol starts a fresh ladder rather than inheriting a stale, inflated attempt count.
#[test]
fn a_recovery_clears_the_record_so_a_later_failure_starts_a_fresh_ladder() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(gate(&mut bf_spawned, &mut retries));
    for _ in 0..3 {
        retries.note_report(&BackfillReport::failed(SYM, 0, "socket"));
        retries.expire_all();
        assert!(gate(&mut bf_spawned, &mut retries));
    }
    assert_eq!(retries.attempts(SYM), 3);

    retries.note_report(&BackfillReport::finished(SYM, 4));
    assert!(retries.is_empty(), "a completed walk forgets the whole chain");
    assert_eq!(retries.attempts(SYM), 0);
}

/// Two symbols fail independently: one symbol's exhausted chain must not close the other's,
/// and one symbol's recovery must not clear the other's record.
#[test]
fn symbols_are_independent() {
    let mut retries = BackfillRetries::default();
    retries.note_report(&BackfillReport::failed("BTCUSDT", 0, "a"));
    retries.note_report(&BackfillReport::failed("ETHUSDT", 3, "b"));
    assert_eq!(retries.len(), 2);
    assert!(!retries.is_abandoned("BTCUSDT"));
    assert!(retries.is_abandoned("ETHUSDT"));

    retries.note_report(&BackfillReport::finished("BTCUSDT", 1));
    assert!(!retries.is_pending("BTCUSDT"));
    assert!(retries.is_pending("ETHUSDT"), "one symbol's recovery is not another's");
}

/// A report for a symbol the gate never recorded — the shell's failed `thread::Builder::spawn`
/// path, which reported a zero-page failure so the symbol it had just burned in `bf_spawned` was
/// not lost for the session. (That spawn was `vike-app`'s, and it is a tombstone since the `fat`
/// build went — `crates/vike-desktop/src/app_methods.rs`'s `maybe_spawn_backfill`; the policy it
/// fed is still pinned here.) It must open a normal ladder.
#[test]
fn a_failure_reported_for_an_unrecorded_symbol_opens_a_normal_ladder() {
    let mut bf_spawned: HashSet<String> = HashSet::new();
    let mut retries = BackfillRetries::default();
    assert!(gate(&mut bf_spawned, &mut retries), "the first walk records bf_spawned");
    retries.note_report(&BackfillReport::failed(SYM, 0, "thread spawn failed: too many open"));
    assert_eq!(retries.attempts(SYM), 1);
    retries.expire_all();
    assert!(gate(&mut bf_spawned, &mut retries), "so the spawn is retried, not lost");
}
