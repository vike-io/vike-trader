//! [`ReconnectBackoff`]'s start-over rules, rows, refusal wait and modes, with no clock (moved with the type from
//! `vike-bridge-core` on 2026-11-22).
use super::*;

const S: Duration = Duration::from_secs(1);

/// **`from_row` runs a declared row's ladder** (connection plan B3), one wait per failure: cTrader's actor 1 s
/// doubling to 5 s, dukascopy's sidecar 5 s to 60 s, IG's trade stream 1 s to 60 s, and a fixed row (IBKR's cpapi
/// redial) the same at every step. The numbers are the rows' (decision 0160), so a retune moves them here too.
#[test]
fn from_row_runs_the_rows_ladder() {
    use crate::venue_timings::{CTRADER, DUKASCOPY, IBKR_CPAPI, IG};
    let secs = |row: VenueTimingsReconnect, failures: usize| -> Vec<u64> {
        let mut b = ReconnectBackoff::from_row(&row).without_jitter();
        (0..failures).map(|_| b.failed().as_secs()).collect()
    };
    assert_eq!(secs(CTRADER.exec().reconnect(), 6), [1, 2, 4, 5, 5, 5]);
    assert_eq!(secs(DUKASCOPY.exec().reconnect(), 6), [5, 10, 20, 40, 60, 60]);
    assert_eq!(secs(IG.exec().reconnect(), 8), [1, 2, 4, 8, 16, 32, 60, 60]);
    assert_eq!(secs(IBKR_CPAPI.exec().reconnect(), 3), [2, 2, 2]);
}

/// **`reset` is a loop's own start-over** (the gatewayhub supervisor's 2 s to 8 s): the next wait is the first one
/// again, however far the ladder had climbed, and it doubles again from there.
#[test]
fn reset_starts_the_ladder_over() {
    let mut b = ReconnectBackoff::new(2 * S, 8 * S);
    let waits: Vec<_> = (0..5).map(|_| b.failed()).collect();
    assert_eq!(waits, [2, 4, 8, 8, 8].map(|s| s * S));
    b.reset();
    assert_eq!([b.failed(), b.failed()], [2 * S, 4 * S]);
}

/// The pump's own ladder, 1 s doubling to 30 s: a failed open never resets it, and a session resets it
/// only once it lived twice the ceiling.
#[test]
fn the_ladder_doubles_to_its_ceiling_and_resets_only_after_a_long_session() {
    let mut b = ReconnectBackoff::new(S, 30 * S);
    let waits: Vec<_> = (0..7).map(|_| b.failed()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30].map(|s| s * S));
    b.opened();
    assert_eq!(b.ended(59 * S), 30 * S, "a session under 2 x max keeps the ladder where it was");
    assert_eq!(b.ended(60 * S), S, "a session of 2 x max starts the ladder over");
    assert_eq!(b.failed(), 2 * S, "and it doubles again from there");
}

/// An announced close reopens at once after a session that lived twice the ceiling, leaving the fresh ladder's first
/// step unspent; after a younger session it waits what an unannounced end waits, so a notice on every new session climbs.
#[test]
fn an_announced_close_reopens_at_once_only_after_a_long_session() {
    let mut b = ReconnectBackoff::new(S, 30 * S);
    assert_eq!(b.announced(60 * S), Duration::ZERO, "a session of 2 x max: at once");
    assert_eq!(b.failed(), S, "the fresh ladder's first step is still the next wait");
    let young: Vec<_> = (0..4).map(|_| b.announced(S)).collect();
    assert_eq!(
        young,
        [2, 4, 8, 16].map(|s| s * S),
        "a notice on every young session climbs the ladder"
    );
    let mut every = ReconnectBackoff::new(S, 30 * S).with_reset_after(Duration::ZERO);
    assert_eq!(
        [every.announced(S), every.failed()],
        [Duration::ZERO, S],
        "reset after every session"
    );
}

/// The first wait is `initial` even above `max` (the pump's behaviour under a short test ceiling); a
/// reset lands on the smaller one.
#[test]
fn a_ceiling_under_the_first_wait_capabilities_every_later_wait() {
    let mut b = ReconnectBackoff::new(S, Duration::from_millis(5));
    assert_eq!(b.failed(), S);
    assert_eq!(b.failed(), Duration::from_millis(5));
    assert_eq!(b.ended(Duration::from_millis(10)), Duration::from_millis(5));
}

/// From the `after`-th refusal in a row the venue's wait applies; another failure does not end the run,
/// an open does.
#[test]
fn a_run_of_refusals_waits_the_venues_wait_until_an_open_works() {
    let mut b = ReconnectBackoff::new(S, S).with_refusal_wait(3, 30 * S);
    assert_eq!([b.refused(), b.refused()], [S, S], "inside the run: the ladder's wait");
    assert_eq!(b.failed(), S);
    assert_eq!(b.refused(), 30 * S, "the third refusal in a row, a failure between them or not");
    assert_eq!(b.refused(), 30 * S);
    b.opened();
    assert_eq!(b.refused(), S, "an open that worked ends the run");
    assert_eq!(ReconnectBackoff::new(S, S).refused(), S, "no refusal wait: a refusal is a failure");
}

/// **A run of refusals is spared while the venue's refusal mode holds** (`refusal_spared`, what the pump reads a refused
/// open by): short of a refusal wait's `after`-th refusal and inside a refusal budget, never with neither mode, and an
/// open that works starts a new run.
#[test]
fn a_refusal_is_spared_inside_the_venues_mode_and_never_without_one() {
    let spared_run = |b: &mut ReconnectBackoff, n: usize| -> Vec<bool> {
        (0..n)
            .map(|_| {
                b.refused();
                b.refusal_spared()
            })
            .collect()
    };
    let mut wait = ReconnectBackoff::new(S, S).with_refusal_wait(3, 30 * S);
    assert_eq!(spared_run(&mut wait, 4), [true, true, false, false]);
    wait.opened();
    assert_eq!(spared_run(&mut wait, 1), [true], "an open that worked starts a new run");
    let mut budget = ReconnectBackoff::new(S, 8 * S).with_refusal_budget(2);
    assert_eq!(spared_run(&mut budget, 3), [true, true, false]);
    assert_eq!(spared_run(&mut ReconnectBackoff::new(S, S), 1), [false], "no mode: a failed open");
}

/// **A silent session is spared only in the mode a venue opts into, and the mode moves no wait**: the same ladder with
/// and without it hands out the same waits for a failed open, a refusal, a session's end and an announced one.
#[test]
fn a_silence_is_spared_only_in_its_mode_and_the_mode_moves_no_wait() {
    let waits = |mut b: ReconnectBackoff| {
        [b.failed(), b.refused(), b.ended(Duration::ZERO), b.announced(3600 * S), b.ended(S)]
    };
    let plain = ReconnectBackoff::new(S, 8 * S).with_refusal_budget(1);
    assert!(!plain.silence_spared(), "no mode: a silence is a stall");
    let spared = plain.clone().with_silence_spared();
    assert!(spared.silence_spared());
    assert_eq!(waits(spared), waits(plain));
}

/// A flat end wait (IBKR's cpapi pump: 1 s after a drop, its row's fixed 2 s after a failed dial) is the same however
/// long the session lived, and leaves the ladder where it was: a growing one here, so a start-over would show.
#[test]
fn an_end_wait_is_flat_and_leaves_the_ladder_alone() {
    let mut b = ReconnectBackoff::new(2 * S, 8 * S).with_end_wait(S);
    assert_eq!(b.failed(), 2 * S);
    assert_eq!(b.ended(Duration::ZERO), S);
    assert_eq!(b.ended(3600 * S), S, "a session that lived past twice the ceiling too");
    assert_eq!(b.failed(), 4 * S, "the ladder did not start over");
    assert_eq!(
        b.announced(3600 * S),
        Duration::ZERO,
        "an announced end of a long session: at once"
    );
    assert_eq!(b.failed(), 8 * S, "and the ladder still where it was");
}

/// IG's trade stream, 1 s to 60 s with a budget of three re-login refusals and a reset on every session: inside the
/// budget a refusal waits the flat first step, past it the ladder climbs from it; each kind of failure is its own
/// run, and a session that opened starts the ladder over with its end waiting the first step.
#[test]
fn the_refusal_budget_waits_flat_then_climbs_and_each_kind_is_its_own_run() {
    let ig =
        || ReconnectBackoff::new(S, 60 * S).with_refusal_budget(3).with_reset_after(Duration::ZERO);
    let mut b = ig();
    let refusals: Vec<_> = (0..10).map(|_| b.refused()).collect();
    assert_eq!(refusals, [1, 1, 1, 1, 2, 4, 8, 16, 32, 60].map(|s| s * S));
    assert_eq!(b.refused(), 60 * S, "capped");
    assert_eq!(
        [b.failed(), b.failed(), b.failed()],
        [S, 2 * S, 4 * S],
        "a failed open: its own run"
    );
    assert_eq!(b.refused(), S, "and a refusal after it starts its own, inside the budget");
    assert_eq!([b.failed(), b.failed()], [S, 2 * S]);

    assert_eq!(b.ended(Duration::ZERO), S, "a session that opened waits the first step");
    assert_eq!(b.failed(), S, "and leaves the ladder at its start");
    assert_eq!(b.ended(Duration::ZERO), S, "a drop after a drop waits flat too");
    for _ in 0..3 {
        assert_eq!(b.refused(), S, "a session ended the refusal run: the budget is whole again");
    }
    assert_eq!(b.refused(), S, "the first refusal past the budget is the ladder's first step");
    assert_eq!(b.refused(), 2 * S);

    let mut long = ReconnectBackoff::new(S, 60 * S).with_reset_after(10 * S);
    assert_eq!([long.failed(), long.failed()], [S, 2 * S]);
    assert_eq!(long.ended(9 * S), 4 * S, "a session shorter than the reset keeps climbing");
    assert_eq!(long.ended(10 * S), S, "one that lived it starts over");
    assert_eq!(long.failed(), S);
}

/// The unspread schedule and every wait a seeded ladder hands out over `turns` of `turn`, side by side.
fn spread_walk(
    initial: Duration,
    max: Duration,
    seed: u64,
    turns: usize,
    turn: fn(&mut ReconnectBackoff) -> Duration,
) -> Vec<(Duration, Duration)> {
    let mut exact = ReconnectBackoff::new(initial, max);
    let mut spread = ReconnectBackoff::new(initial, max).with_jitter_seed(seed);
    (0..turns).map(|_| (turn(&mut exact), turn(&mut spread))).collect()
}

/// **A row's ladder spreads each step inside the schedule's bounds** (the module doc): never sooner than the step,
/// never more than half again, never past the ceiling, and at the ceiling not at all; the spread never moves the
/// schedule itself, so the steps after a spread wait are the unspread ones. The same seed draws the same spread.
#[test]
fn a_spread_step_stays_between_its_rung_and_half_again_and_never_passes_the_ceiling() {
    let (initial, max) = (S, 30 * S);
    for seed in [0, 1, 7, u64::MAX] {
        let walk = spread_walk(initial, max, seed, 12, ReconnectBackoff::failed);
        let rungs: Vec<Duration> = walk.iter().map(|(rung, _)| *rung).collect();
        assert_eq!(rungs, [1, 2, 4, 8, 16, 30, 30, 30, 30, 30, 30, 30].map(|s| s * S));
        for (rung, wait) in &walk {
            assert!(*rung <= *wait, "seed {seed}: {wait:?} is sooner than its step {rung:?}");
            assert!(
                *wait <= (*rung + *rung / 2).min(max),
                "seed {seed}: {wait:?} past {rung:?}'s room"
            );
        }
        assert!(
            walk[5..].iter().all(|(rung, wait)| rung == wait),
            "seed {seed}: the ceiling is not spread"
        );
        assert_eq!(
            walk,
            spread_walk(initial, max, seed, 12, ReconnectBackoff::failed),
            "seed {seed} again"
        );
    }
    let spread_any = (0..16).any(|seed| {
        spread_walk(initial, max, seed, 5, ReconnectBackoff::failed)
            .iter()
            .any(|(rung, wait)| rung != wait)
    });
    assert!(spread_any, "no seed spread any step under the ceiling");
}

/// **Two row ladders draw apart** (each `from_row` seeds its own source), so two connections that fail together do
/// not dial again together; a `new` ladder and a `without_jitter` one hand out the schedule exactly.
#[test]
fn two_row_ladders_draw_apart_and_an_exact_one_hands_out_its_schedule() {
    let row = VenueTimingsReconnect::new(S, 30 * S, "a test's 1 s -> 30 s row");
    let draws = |mut b: ReconnectBackoff| -> Vec<Duration> { (0..5).map(|_| b.failed()).collect() };
    assert_ne!(
        draws(ReconnectBackoff::from_row(&row)),
        draws(ReconnectBackoff::from_row(&row)),
        "two row ladders drew the same five waits"
    );
    let schedule = [1, 2, 4, 8, 16].map(|s| s * S).to_vec();
    assert_eq!(draws(ReconnectBackoff::new(S, 30 * S)), schedule);
    assert_eq!(draws(ReconnectBackoff::from_row(&row).without_jitter()), schedule);
}

/// **What is never spread**: a fixed row, and a venue's declared flat waits (the refusal wait, the refusal budget's
/// flat first step, the end wait, the first step after a session that lived its reset); a session's end is a step,
/// and is.
#[test]
fn a_fixed_row_and_the_declared_flat_waits_are_never_spread() {
    let mut fixed = ReconnectBackoff::from_row(&VenueTimingsReconnect::fixed(2 * S, "a fixed row"));
    assert_eq!([fixed.failed(), fixed.refused(), fixed.ended(S)], [2 * S; 3]);

    let mut refusal =
        ReconnectBackoff::new(S, 30 * S).with_refusal_wait(1, 10 * S).with_jitter_seed(3);
    assert_eq!([refusal.refused(), refusal.refused()], [10 * S; 2], "the refusal wait");

    let mut budget = ReconnectBackoff::new(S, 30 * S).with_refusal_budget(2).with_jitter_seed(3);
    assert_eq!([budget.refused(), budget.refused()], [S; 2], "inside the budget");

    let mut end = ReconnectBackoff::new(S, 30 * S).with_end_wait(S).with_jitter_seed(3);
    assert_eq!([end.ended(Duration::ZERO), end.ended(3600 * S)], [S; 2], "the end wait");

    let mut reset = ReconnectBackoff::new(S, 30 * S).with_reset_after(10 * S).with_jitter_seed(3);
    assert_eq!(reset.ended(10 * S), S, "the first step after a session that lived its reset");

    for (rung, wait) in spread_walk(4 * S, 30 * S, 5, 4, |b| b.ended(Duration::ZERO)) {
        assert!(
            rung <= wait && wait <= rung + rung / 2,
            "a session's end is a step: {wait:?} for {rung:?}"
        );
    }
}

/// **An announced close leaves the fresh ladder's first step unspent, spread or not**: the wait after it is a spread
/// of the first step, and the step after that a spread of the second.
#[test]
fn an_announced_close_gives_back_the_unspread_first_step() {
    let mut b = ReconnectBackoff::new(S, 30 * S).with_jitter_seed(11);
    b.failed();
    b.failed();
    assert_eq!(b.announced(60 * S), Duration::ZERO);
    let first = b.failed();
    assert!((S..=S + S / 2).contains(&first), "the fresh ladder's first step: {first:?}");
    let second = b.failed();
    assert!((2 * S..=3 * S).contains(&second), "then its second: {second:?}");
}
