use super::*;
use std::assert_matches;

/// Replays the reconnect loop's DECISION against a synthetic clock, turn for turn as
/// [`stream_trades`] drives it: the dial costs `dial_cost`, then [`RetryState::plan`] is
/// consulted with the resulting reading, then the planned sleep runs.
///
/// ⚠ **This is the whole reason the decision was extracted.** The dial itself goes through
/// `ureq` to `LsParams::endpoint`, so driving `stream_trades` end-to-end would need an HTTP
/// server standing in for IG's Lightstreamer gateway — and the thing that needs proving is not
/// the transport but the LOGGING POLICY over a day of wall time, which no server can be made to
/// demonstrate quickly. Against a synthetic clock a day costs microseconds.
struct Replay {
    state: RetryState,
    now: Duration,
    dial_cost: Duration,
    /// `(when the plan was made, the plan)` for every turn, in order.
    turns: Vec<(Duration, RetryPlan)>,
}

impl Replay {
    fn new(dial_cost: Duration) -> Self {
        Self { state: RetryState::default(), now: Duration::ZERO, dial_cost, turns: Vec::new() }
    }

    fn dial(&mut self, outcome: DialOutcome) -> RetryPlan {
        self.now += self.dial_cost;
        let plan = self.state.plan(outcome, self.now);
        self.turns.push((self.now, plan));
        self.now += plan.sleep;
        plan
    }

    fn dial_n(&mut self, n: usize, outcome: DialOutcome) {
        for _ in 0..n {
            self.dial(outcome);
        }
    }

    fn says(&self) -> Vec<Say> {
        self.turns.iter().map(|(_, p)| p.say).collect()
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.turns.iter().map(|(_, p)| p.sleep).collect()
    }

    fn relogins(&self) -> Vec<bool> {
        self.turns.iter().map(|(_, p)| p.relogin).collect()
    }

    /// Every turn that would print a line at all, with the clock reading it printed at.
    fn lines(&self, want: fn(&Say) -> bool) -> Vec<(Duration, Say)> {
        self.turns.iter().filter(|(_, p)| want(&p.say)).map(|(t, p)| (*t, p.say)).collect()
    }

    /// Every turn that would print an `error` line — the permanent-refused lane.
    fn error_lines(&self) -> Vec<(Duration, Say)> {
        self.lines(|s| matches!(s, Say::FillLaneDown { .. } | Say::FillLaneStillDown { .. }))
    }

    /// Every turn that would print a cannot-open-a-session `warn`.
    fn dial_lines(&self) -> Vec<(Duration, Say)> {
        self.lines(|s| matches!(s, Say::DialFailed { .. } | Say::DialStillFailing { .. }))
    }
}

/// Both persistent lanes must obey the same two rules, so they are asserted by one helper
/// rather than two hand copies that can drift: never two lines closer than the FIRST interval
/// (the floor of the decay), and never a silence longer than the CAPPED one, which is the
/// promise that stops a decaying cadence from fading into apparent recovery.
fn assert_paced(lines: &[(Duration, Say)], least: usize) {
    assert!(lines.len() >= least, "only {} lines — not enough to prove a rate at all", lines.len());
    for pair in lines.windows(2) {
        let gap = pair[1].0.saturating_sub(pair[0].0);
        assert!(gap >= DOWN_HEARTBEAT, "two lines {gap:?} apart — the rate limit leaked");
        // The loop can only speak on a turn, so the worst case is one whole backoff late.
        assert!(
            gap <= MAX_DOWN_HEARTBEAT + MAX_RECONNECT_BACKOFF,
            "the lane went quiet for {gap:?} — past the capped interval, which is exactly the \
                 silence an operator reads as recovery"
        );
    }
}

/// A dial against a gateway that refuses instantly. ~100 ms is what the measured flood implies:
/// 53,160 lines a day at a flat 1 s sleep leaves ~0.6 s per turn for the round trip and the
/// loop, and the exact figure changes nothing here — the backoff dominates within ten turns.
const FAST_REFUSAL: Duration = Duration::from_millis(100);

/// The transition is a TRANSITION. The defect being fixed is precisely that this `error` line
/// was emitted on every turn of a one-second loop while its own text says the condition is
/// permanent, so "exactly one" is the assertion that matters, not "at least one".
#[test]
fn a_refusal_streak_logs_the_fill_lane_down_exactly_once_on_the_transition() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(43, DialOutcome::Refused);
    let says = r.says();

    assert_eq!(
        says[..3],
        [
            Say::RefusedRetryingLogin { attempt: 1 },
            Say::RefusedRetryingLogin { attempt: 2 },
            Say::RefusedRetryingLogin { attempt: 3 },
        ],
        "the bounded re-login path is unchanged: MAX_REFUSED_HANDSHAKES attempts, each at warn"
    );
    assert_eq!(
        says[usize::try_from(MAX_REFUSED_HANDSHAKES).unwrap()],
        Say::FillLaneDown { streak: MAX_REFUSED_HANDSHAKES + 1, next_in: DOWN_HEARTBEAT },
        "the FIRST turn past the budget is the transition line"
    );

    let onsets = says.iter().filter(|s| matches!(s, Say::FillLaneDown { .. })).count();
    assert_eq!(
        onsets, 1,
        "the transition fired {onsets} times — it is a transition, not a per-turn line"
    );

    let heartbeats = says.iter().filter(|s| matches!(s, Say::FillLaneStillDown { .. })).count();
    assert!(heartbeats > 0, "an operator must still learn the lane is down; got {heartbeats}");
    assert!(
        heartbeats < 43 - 4,
        "{heartbeats} heartbeats over 39 permanent-refused turns is the flood again"
    );
    assert_eq!(
        says.iter().filter(|s| matches!(s, Say::Dropped)).count(),
        0,
        "a refusal is never a transport drop"
    );
}

/// The rate limit, asserted as a property of the emitted timeline rather than of a counter:
/// no two `error` lines closer together than the heartbeat, and no gap so long the operator
/// would conclude the outage ended.
#[test]
fn the_permanent_refused_heartbeat_is_rate_limited_to_one_line_per_interval() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(200, DialOutcome::Refused);
    let lines = r.error_lines();

    assert_matches!(
        lines[0].1,
        Say::FillLaneDown { .. },
        "the first error line is the transition, not a heartbeat"
    );
    assert!(
        lines[1..].iter().all(|(_, s)| matches!(s, Say::FillLaneStillDown { .. })),
        "every line after the transition is a heartbeat"
    );
    assert_paced(&lines, 5);
}

/// The backoff itself: exponential from the flat delay, capped, monotonic, and TOTAL. A streak
/// large enough to overflow the shift must answer the CEILING — an arithmetic wrap there would
/// hand back a one-second retry and with it the whole flood.
#[test]
fn the_refused_backoff_doubles_from_the_flat_delay_and_caps_at_the_ceiling() {
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 1), Duration::from_secs(1));
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 2), Duration::from_secs(2));
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 3), Duration::from_secs(4));
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 4), Duration::from_secs(8));
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 5), Duration::from_secs(16));
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 6), Duration::from_secs(32));
    assert_eq!(
        refused_backoff(MAX_REFUSED_HANDSHAKES + 7),
        MAX_RECONNECT_BACKOFF,
        "64s is above the ceiling and must clamp to it"
    );
    assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 40), MAX_RECONNECT_BACKOFF);
    assert_eq!(refused_backoff(u32::MAX), MAX_RECONNECT_BACKOFF, "the shift must not wrap");

    let mut prev = Duration::ZERO;
    let mut checked = 0u32;
    for streak in (MAX_REFUSED_HANDSHAKES + 1)..(MAX_REFUSED_HANDSHAKES + 200) {
        let d = refused_backoff(streak);
        assert!(d >= prev, "backoff shrank at streak {streak}: {d:?} < {prev:?}");
        assert!(d >= RECONNECT_DELAY, "backoff dipped below the flat delay at streak {streak}");
        assert!(d <= MAX_RECONNECT_BACKOFF, "backoff exceeded the ceiling at streak {streak}");
        prev = d;
        checked += 1;
    }
    assert_eq!(checked, 199, "the monotonicity sweep must actually have run");
}

/// The two paths' sleeps, read off the PLAN rather than the helper — the re-login path keeps
/// the flat one-second delay it has always had, and only the permanent path backs off.
#[test]
fn only_the_permanent_refused_path_backs_off_and_only_it_stops_re_logging_in() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(10, DialOutcome::Refused);

    let sleeps = r.sleeps();
    assert_eq!(sleeps[..3], [RECONNECT_DELAY; 3], "the bounded re-login path is unchanged");
    assert_eq!(
        sleeps[3..],
        [1u64, 2, 4, 8, 16, 32, 60].map(Duration::from_secs),
        "the permanent path doubles from the flat delay and clamps at the ceiling"
    );

    let relogins = r.relogins();
    assert_eq!(relogins[..3], [true; 3], "the first MAX_REFUSED_HANDSHAKES turns re-authenticate");
    assert_eq!(
        relogins[3..],
        [false; 7],
        "past the budget the driver keeps dialling but stops spending the account's login budget"
    );
}

/// A session that RAN clears everything: the streak, the backoff and the heartbeat. Without
/// this a lane that recovered would keep dialling once a minute and stay silent about the next
/// outage — the opposite defect to the one being fixed.
#[test]
fn a_clean_session_resets_the_streak_the_backoff_and_the_heartbeat() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(12, DialOutcome::Refused);
    assert_eq!(r.sleeps()[11], MAX_RECONNECT_BACKOFF, "precondition: deep in the backoff");

    assert_eq!(
        r.dial(DialOutcome::Ok),
        RetryPlan { relogin: false, say: Say::Nothing, sleep: RECONNECT_DELAY },
        "a session that ran says nothing and dials again promptly"
    );
    assert_eq!(
        r.dial(DialOutcome::Refused),
        RetryPlan {
            relogin: true,
            say: Say::RefusedRetryingLogin { attempt: 1 },
            sleep: RECONNECT_DELAY
        },
        "the next refusal is attempt 1 of a NEW streak, not a resumption"
    );

    r.dial_n(2, DialOutcome::Refused);
    assert_eq!(
        r.dial(DialOutcome::Refused).say,
        Say::FillLaneDown { streak: MAX_REFUSED_HANDSHAKES + 1, next_in: DOWN_HEARTBEAT },
        "re-entering the permanent state is a NEW outage and logs its own transition"
    );
}

/// A session that STREAMED and then dropped keeps the original behaviour exactly: a warn per
/// drop, no backoff, no login spent, both streaks reset. Reaching here costs a full handshake
/// and a subscription, so the loop cannot spin at dial speed — that is the whole argument, and
/// it is why this path must NOT be rate-limited into silence along with the other two.
#[test]
fn a_session_that_ran_and_dropped_is_still_logged_every_time() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(12, DialOutcome::Refused);
    r.dial_n(5, DialOutcome::Dropped);

    assert_eq!(r.says()[12..], [Say::Dropped; 5], "every drop is news");
    assert_eq!(r.sleeps()[12..], [RECONNECT_DELAY; 5], "a drop never backs off");
    assert_eq!(r.relogins()[12..], [false; 5], "a drop never spends a login");
    assert_eq!(
        r.dial(DialOutcome::Refused).say,
        Say::RefusedRetryingLogin { attempt: 1 },
        "a drop resets the refusal streak, exactly as it always did"
    );
}

/// ⚠ **The second half of the flood, and the one that was left in place by the first fix.**
/// Most `Transient` values were never mid-stream drops at all — `create_session failed`,
/// `create_session: empty response`, `create_session read`, a non-`CONOK` first line and
/// `subscribe failed` all leave the loop having established NOTHING. Nothing is spent, so
/// nothing self-limits: a hard-down endpoint fails in milliseconds forever, which at the flat
/// delay is a `warn` a second. This path now gets the same three treatments the refusal path
/// got — its own streak, exponential backoff, and one line on the transition then a heartbeat.
#[test]
fn a_dial_that_never_opens_a_session_backs_off_and_warns_on_a_heartbeat() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(200, DialOutcome::NeverEstablished);

    let says = r.says();
    assert_eq!(
        says[0],
        Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
        "the FIRST failed dial is news and says so immediately"
    );
    assert_eq!(
        says.iter().filter(|s| matches!(s, Say::DialFailed { .. })).count(),
        1,
        "…and exactly once: this is a transition, not a per-turn line"
    );
    assert_eq!(
        says.iter().filter(|s| matches!(s, Say::Dropped)).count(),
        0,
        "a dial that never opened a session is NOT a drop and must not borrow its unlimited rate"
    );

    let sleeps = r.sleeps();
    assert_eq!(
        sleeps[..7],
        [1u64, 2, 4, 8, 16, 32, 60].map(Duration::from_secs),
        "the first retry keeps the flat delay, then it doubles to the ceiling"
    );
    assert!(
        sleeps[7..].iter().all(|s| *s == MAX_RECONNECT_BACKOFF),
        "and stays at the ceiling — it never stops dialling, a hard-down venue does recover"
    );
    assert_eq!(r.relogins(), vec![false; 200], "a failed dial never spends a login");

    assert_paced(&r.dial_lines(), 5);
}

/// The two persistent conditions clear each other, because each is POSITIVE evidence against
/// the other: a `CONERR` is the gateway answering (so it is reachable), and a failed dial says
/// nothing at all about the credentials. Without this, whichever condition ran first would
/// suppress the other's transition line and an operator would never learn the failure changed.
#[test]
fn a_refusal_and_a_failed_dial_clear_each_others_streaks() {
    let mut r = Replay::new(FAST_REFUSAL);
    r.dial_n(10, DialOutcome::NeverEstablished);
    assert_eq!(r.sleeps()[9], MAX_RECONNECT_BACKOFF, "precondition: deep in the dial backoff");

    assert_eq!(
        r.dial(DialOutcome::Refused),
        RetryPlan {
            relogin: true,
            say: Say::RefusedRetryingLogin { attempt: 1 },
            sleep: RECONNECT_DELAY
        },
        "the gateway answered, so the unreachable condition is over and the budget is whole"
    );

    r.dial_n(10, DialOutcome::Refused);
    assert_eq!(
        r.dial(DialOutcome::NeverEstablished),
        RetryPlan {
            relogin: false,
            say: Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
            sleep: RECONNECT_DELAY
        },
        "and back the other way: a failed dial is a NEW outage with its own transition line"
    );
}

/// A session that actually ran clears the dial-failure condition too — the same reset `Ok` has
/// always performed for the refusal streak, extended to the condition added beside it.
#[test]
fn a_session_that_ran_clears_the_dial_failure_streak() {
    for recovery in [DialOutcome::Ok, DialOutcome::Dropped] {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(10, DialOutcome::NeverEstablished);
        let plan = r.dial(recovery);
        assert_eq!(plan.sleep, RECONNECT_DELAY, "{recovery:?} must drop the backoff");

        assert_eq!(
            r.dial(DialOutcome::NeverEstablished),
            RetryPlan {
                relogin: false,
                say: Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
                sleep: RECONNECT_DELAY
            },
            "after {recovery:?} the next failed dial is attempt 1 of a NEW outage"
        );
    }
}

/// The ladder itself: geometric from the first interval, capped, monotonic and TOTAL. The cap
/// is the load-bearing half — an interval that kept growing would eventually be silence, and
/// silence is what the heartbeat exists to NOT produce.
#[test]
fn the_heartbeat_interval_decays_geometrically_and_caps() {
    assert_eq!(heartbeat_interval(0), DOWN_HEARTBEAT, "the first quiet period is responsive");
    assert_eq!(heartbeat_interval(1), Duration::from_secs(900), "5m -> 15m");
    assert_eq!(heartbeat_interval(2), Duration::from_secs(2700), "15m -> 45m");
    assert_eq!(
        heartbeat_interval(3),
        MAX_DOWN_HEARTBEAT,
        "135m is past the ceiling and must clamp to it"
    );

    let mut prev = Duration::ZERO;
    let mut checked = 0u32;
    for steps in 0..200 {
        let d = heartbeat_interval(steps);
        assert!(d >= prev, "the interval shrank at step {steps}: {d:?} < {prev:?}");
        assert!(d >= DOWN_HEARTBEAT, "the interval dipped below the floor at step {steps}");
        assert!(d <= MAX_DOWN_HEARTBEAT, "the interval passed the ceiling at step {steps}");
        prev = d;
        checked += 1;
    }
    assert_eq!(checked, 200, "the monotonicity sweep must actually have run");

    // ⚠ TOTAL: an outage long enough to overflow the power must answer the CEILING. A wrap here
    // would hand back a five-minute cadence forever, which is the volume this decay removes.
    assert_eq!(heartbeat_interval(u32::MAX), MAX_DOWN_HEARTBEAT, "the power must not wrap");
    assert_eq!(heartbeat_interval(1_000_000), MAX_DOWN_HEARTBEAT);
}

/// The rate limiter itself, asserted once rather than twice — both persistent lanes share it,
/// which is the point of it being a type. Covers the whole contract: the interval is measured
/// from the LAST LINE (measuring from onset would fire every turn once the outage outlived one
/// interval — the flood wearing a rate limit), it DECAYS, and a clear resets the decay.
#[test]
fn the_shared_rate_limiter_decays_from_the_last_line_and_resets_on_recovery() {
    let t0 = Duration::from_secs(7);
    let mut p = Persistent::default();

    assert_eq!(p.tick(t0), Speak::Onset { next_in: DOWN_HEARTBEAT });
    assert_eq!(p.tick(t0), Speak::Quiet);
    assert_eq!(
        p.tick(t0 + DOWN_HEARTBEAT - Duration::from_nanos(1)),
        Speak::Quiet,
        "one nanosecond short of the interval is still silent"
    );
    assert_eq!(
        p.tick(t0 + DOWN_HEARTBEAT),
        Speak::Heartbeat { down_for: DOWN_HEARTBEAT, next_in: heartbeat_interval(1) },
        "down_for is measured from ONSET; next_in is the DECAYED interval that follows"
    );

    // ⚠ The next line is due one DECAYED interval after the last one — not one base interval,
    // and not at a multiple of the outage age.
    let second = t0 + DOWN_HEARTBEAT;
    assert_eq!(
        p.tick(second + DOWN_HEARTBEAT),
        Speak::Quiet,
        "the flat interval has passed but the decayed one has not"
    );
    assert_eq!(
        p.tick(second + heartbeat_interval(1)),
        Speak::Heartbeat {
            down_for: DOWN_HEARTBEAT + heartbeat_interval(1),
            next_in: heartbeat_interval(2),
        }
    );

    p.clear();
    assert_eq!(
        p.tick(Duration::from_secs(9)),
        Speak::Onset { next_in: DOWN_HEARTBEAT },
        "a recovery resets the DECAY too — a second outage starts responsive rather than \
             inheriting an hour-long interval from the first"
    );
}

/// The cap holds forever: once the interval reaches the ceiling it stays there, so a lane down
/// for days keeps saying so hourly. Driven through the real `plan`, not the limiter alone.
#[test]
fn a_capped_heartbeat_keeps_speaking_hourly_forever() {
    let mut r = Replay::new(FAST_REFUSAL);
    // A week of a permanently refused lane.
    let week = Duration::from_secs(7 * 24 * 60 * 60);
    while r.now < week {
        r.dial(DialOutcome::Refused);
    }
    let lines = r.error_lines();

    // Every gap after the ladder tops out is the capped interval, to within one dial turn.
    let tail = &lines[6..];
    assert!(tail.len() > 100, "not enough capped gaps to prove the cap: {}", tail.len());
    for pair in tail.windows(2) {
        let gap = pair[1].0.saturating_sub(pair[0].0);
        assert!(
            gap >= MAX_DOWN_HEARTBEAT && gap <= MAX_DOWN_HEARTBEAT + MAX_RECONNECT_BACKOFF,
            "a capped gap of {gap:?} — the cadence must settle at the ceiling, not drift"
        );
    }
}

/// EITHER streak running long enough to saturate must stay where it is. A wrap to zero on the
/// refusal side would resume re-logging-in and point a login storm at the one endpoint IG
/// meters per account; on the dial side it would drop the backoff to one second and restore the
/// flood. Both are `saturating_add`, and both are asserted, because a `+= 1` added to only one
/// of them later would otherwise pass.
#[test]
fn a_saturated_streak_neither_wraps_nor_leaves_its_state() {
    let pinned =
        Persistent { since: Some(Duration::ZERO), last_said: Some(Duration::ZERO), steps: 0 };

    let mut state =
        RetryState { refused_streak: u32::MAX, refused_down: pinned, ..RetryState::default() };
    let plan = state.plan(DialOutcome::Refused, Duration::from_secs(10));
    assert_eq!(state.refused_streak, u32::MAX, "the streak saturates rather than wrapping");
    assert!(!plan.relogin, "a wrapped streak would resume re-logging-in forever");
    assert_eq!(plan.sleep, MAX_RECONNECT_BACKOFF);
    assert_eq!(plan.say, Say::Nothing, "and the heartbeat still holds");

    let mut state =
        RetryState { dial_streak: u32::MAX, dial_down: pinned, ..RetryState::default() };
    let plan = state.plan(DialOutcome::NeverEstablished, Duration::from_secs(10));
    assert_eq!(state.dial_streak, u32::MAX, "the dial streak saturates too");
    assert!(!plan.relogin);
    assert_eq!(plan.sleep, MAX_RECONNECT_BACKOFF, "a wrap here would restore the flood");
    assert_eq!(plan.say, Say::Nothing);
}

/// **The other half of the flood, replayed.** A hard-down / DNS-broken / TLS-broken gateway
/// fails before a session exists, in milliseconds, forever. At the flat delay that is a `warn`
/// on every turn — ~86,400 lines a day, and the CI box's file level is `warn`, so every one
/// of them is written to disk. It was never measured on a live box because the day that WAS
/// measured failed as a refusal; the rate is arithmetic, not an estimate.
#[test]
fn a_hard_down_endpoint_no_longer_writes_a_warn_a_second_all_day() {
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    let mut r = Replay::new(FAST_REFUSAL);
    while r.now < DAY {
        r.dial(DialOutcome::NeverEstablished);
    }

    let dials = r.turns.len();
    let warns = r.dial_lines().len();

    // What the OLD loop did on the same day: one turn per `RECONNECT_DELAY` + dial, warning on
    // every one of them.
    let old = DAY.as_millis() / (RECONNECT_DELAY + FAST_REFUSAL).as_millis();
    assert!(old > 78_000, "sanity: the old rate really is a warn a second ({old})");

    // MEASURED by replaying this exact state machine. The bounds are a band rather than
    // equalities, so re-tuning a constant is a deliberate re-argument and not a rebaseline.
    // MEASURED: 78,545 -> 26 lines, over 1,443 dials. (It was 288 under the flat heartbeat that
    // first bounded this; the decay is what took it the rest of the way.)
    assert!(warns >= 20, "an operator must still see the lane is down all day: {warns} lines");
    assert!(warns < 40, "{warns} lines a day is more than a known-permanent state needs");
    assert!(dials < 1_600, "{dials} dials a day — the ceiling must bound the retry rate too");
    assert!(
        u128::try_from(warns).unwrap() * 2_000 < old,
        "only a {}x reduction, from {old} to {warns}",
        old / u128::try_from(warns.max(1)).unwrap()
    );
}

/// **The defect, replayed.** Measured on the CI box 2026-08-23: this one loop wrote **53,160 `error`
/// lines / 23 MB in one day**, dead flat at ~3,290/hour for 17 hours — 99.5% of the box's entire
/// error volume — because a permanent refusal was logged on every turn of a one-second retry.
/// Same day, same permanent refusal, through the new decision.
#[test]
fn a_permanently_refused_day_no_longer_floods_the_log() {
    /// What the box actually wrote in 24h, and roughly how wide each line was (23 MB / 53,160).
    const MEASURED_LINES_PER_DAY: usize = 53_160;
    const MEASURED_BYTES_PER_LINE: usize = 433;
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    let mut r = Replay::new(FAST_REFUSAL);
    while r.now < DAY {
        r.dial(DialOutcome::Refused);
    }

    let dials = r.turns.len();
    let errors = r.error_lines().len();

    // MEASURED by replaying this exact state machine, against the 24 hours that produced
    // 53,160. The bounds are a band rather than equalities, so re-tuning a constant is a
    // deliberate re-argument and not a rebaseline.
    // MEASURED: 53,160 -> 26 lines (23 MB -> ~11 KB), over 1,446 dials. It was 288 under the
    // flat heartbeat that first bounded this; the decay took it the rest of the way.
    assert!(errors >= 20, "an operator must still see the lane is down all day: {errors} lines");
    assert!(errors < 40, "{errors} lines a day is more than a known-permanent state needs");
    assert!(
        dials < 1_600,
        "{dials} dials a day — the backoff ceiling is supposed to bound the retry rate too"
    );
    assert!(
        errors * 1_500 < MEASURED_LINES_PER_DAY,
        "only a {}x reduction, from {MEASURED_LINES_PER_DAY} to {errors}",
        MEASURED_LINES_PER_DAY / errors.max(1)
    );
    assert!(
        errors * MEASURED_BYTES_PER_LINE < 20_000,
        "{} KB/day from one refused venue is more than this needs",
        errors * MEASURED_BYTES_PER_LINE / 1024
    );
}

#[test]
fn conok_parses_session_and_control_link() {
    let (sid, link) = parse_conok("CONOK,S1a2b3c4d5e6f,50000,5000,myhost.ig.com").unwrap();
    assert_eq!(sid, "S1a2b3c4d5e6f");
    assert_eq!(link, "myhost.ig.com");
    let (sid2, link2) = parse_conok("CONOK,S9,50000,5000,*").unwrap();
    assert_eq!(sid2, "S9");
    assert_eq!(link2, "*");
    assert!(parse_conok("CONERR,2,Wrong credentials").is_none());
}

/// The classification the reconnect loop's recovery hangs off: a `CONERR` first line is the
/// server REFUSING our credentials (re-authenticate), everything else that is not a `CONOK` is
/// a protocol surprise (retry with the same pair). Getting this backwards is invisible at
/// runtime — it just means an expired session never recovers, which is the defect this whole
/// path exists to close.
#[test]
fn conerr_is_classified_as_a_refusal_and_nothing_else_is() {
    assert!(is_conerr("CONERR,2,Wrong credentials"));
    assert!(is_conerr("CONERR,1,Requested Adapter Set not available"));
    // Real TLCP frames can carry leading whitespace after a chunked read.
    assert!(is_conerr("  CONERR,2,x"));
    assert!(!is_conerr("CONOK,S1,50000,5000,*"));
    assert!(!is_conerr("PROBE"));
    assert!(!is_conerr("LOOP,0"));
    assert!(!is_conerr(""));
    // ⚠ Not a substring test: a CONFIRMS payload mentioning the word must not read as a refusal.
    assert!(!is_conerr(r#"U,1,1|{"reason":"CONERR"}"#));
}

/// ⚠ **The `LS_cid` this lane sends is a LICENSING field, not a name for us**, and it is why
/// this stream had almost certainly never worked on any account. TLCP 2.1.0's `create_session`
/// mandates one special string "for all custom developed clients"; anything else is answered
/// `CONERR,71 License not valid for this Client type`, which is a refusal of the CLIENT and has
/// nothing to do with the account, the credentials or the subscription. `stream.rs` sent
/// `"vike-trader-rust"` — a made-up identifier — while `lightstreamer.rs`, the market-data lane
/// that works, sent the mandated one from its own `LS_CID`.
///
/// This test is the twin of `lightstreamer.rs`'s `request_builders_pin_the_wire_bytes`, and it
/// asserts the ENCODED body: the mandated string contains a space, so the wire form carries
/// `%20` (`form_percent_encodes_group_and_schema` below pins that encoding independently).
#[test]
fn the_trade_stream_sends_the_licensed_client_id_the_spec_mandates() {
    let body = create_session_body("ABC123", "CST-tok|XST-tok");

    assert!(
        body.contains("LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"),
        "the create_session body must carry the SPEC's client id; got: {body}"
    );
    assert!(
        !body.contains("vike-trader-rust"),
        "a made-up client id is refused with CONERR,71 whatever the account says; got: {body}"
    );
    // The value is the shared constant, not a second copy that can drift from it again.
    assert!(body.contains(&format!("LS_cid={}", crate::lightstreamer::urlencode(LS_CID))));
}

/// The whole body, verbatim, both requests — the market-data lane pins its two the same way.
/// A verbatim pin is what makes an accidental edit to a licensing or protocol field a test
/// failure rather than a silent `CONERR` nobody can attribute.
#[test]
fn trade_stream_request_bodies_pin_the_wire_bytes() {
    assert_eq!(
        create_session_body("ABC123", "CST-abc|XST-def"),
        "LS_adapter_set=DEFAULT&LS_user=ABC123&LS_password=CST-abc%7CXST-def\
             &LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"
    );
    assert_eq!(
        subscribe_body("S1a2b3", "ABC123"),
        "LS_session=S1a2b3&LS_reqId=1&LS_op=add&LS_subId=1&LS_data_adapter=DEFAULT\
             &LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU&LS_mode=DISTINCT\
             &LS_snapshot=true"
    );
}

/// One handshake, one spelling. Both lanes speak TLCP 2.1.0 to the same vendor's server, and
/// `stream.rs` re-spelling the protocol version (and `form`/`urlencode`) as its own local copies
/// is the duplication that let the `LS_cid` above diverge in the first place — the wrong cid was
/// the COST, the duplication was the defect.
#[test]
fn the_two_tlcp_lanes_agree_on_the_protocol_version() {
    assert_eq!(LS_PROTOCOL, crate::lightstreamer::LS_PROTOCOL);
    assert_eq!(LS_CID, crate::lightstreamer::LS_CID);
    assert_eq!(
        crate::lightstreamer::LS_WS_SUBPROTOCOL,
        format!("{LS_PROTOCOL}.lightstreamer.com"),
        "the WS subprotocol is the same version wearing the vendor's suffix"
    );
}

#[test]
fn form_percent_encodes_group_and_schema() {
    let body = form(&[("LS_group", "TRADE:ABC123"), ("LS_schema", "CONFIRMS OPU WOU")]);
    assert_eq!(body, "LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU");
}

/// Both directions, from ONE write. The `coid -> dealReference` half is what makes
/// `ExecutionClient::confirm` answerable on this venue at all: `GET /confirms/{ref}` is IG's
/// only per-order status read, and before this map existed nothing in the process could turn a
/// coid back into the reference it needs.
#[test]
fn deal_refs_answers_in_both_directions_from_one_write() {
    let mut refs = DealRefs::default();
    refs.record("coid-market", "REF-A", true);
    refs.record("coid-working", "REF-B", false);

    assert_eq!(refs.coid_for("REF-A").as_deref(), Some("coid-market"));
    assert_eq!(refs.coid_for("REF-B").as_deref(), Some("coid-working"));
    assert_eq!(refs.coid_for("REF-UNKNOWN"), None, "a foreign confirm is not ours to route");

    // `market` travels with the reference: the re-confirm needs it to decide whether the
    // confirm it fetches carries a fill or only an accept.
    assert_eq!(
        refs.pending_for("coid-market"),
        Some(Pending { deal_ref: "REF-A".into(), market: true })
    );
    assert_eq!(
        refs.pending_for("coid-working"),
        Some(Pending { deal_ref: "REF-B".into(), market: false })
    );
    assert_eq!(
        refs.pending_for("coid-never-submitted"),
        None,
        "no reference means IG never acknowledged the deal — there is nothing to ask about"
    );
}

#[test]
fn rebase_host_keeps_scheme() {
    assert_eq!(
        rebase_host("https://demo-apd.marketdatasystems.com", "push.ig.com"),
        "https://push.ig.com"
    );
}
