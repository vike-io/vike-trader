//! The rebuild schedule, the operator line and the connect turn, on a FAKE clock (`Instant` plus an offset: the
//! pure halves take `now`) and, for the turn, real threads over a short gap.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Both callers of the data lane: the yield and refusal rules hold for each.
const LANES: [DataLane; 2] = [DataLane::Lookup, DataLane::Redial];

/// A schedule armed at `t` (the loss), no jitter.
fn armed(t: Instant) -> Heal {
    let mut h = Heal::default();
    h.lost(t, t, 0);
    h
}

#[test]
fn an_unarmed_gate_lets_every_attempt_through() {
    let t = Instant::now();
    let mut h = Heal::default();
    assert!(!h.is_armed());
    for n in 0..20 {
        assert_eq!(h.permit(t + s(n)), Ok(()), "a rebuild after a daily logoff is not spaced");
    }
}

/// THE SCHEDULE: the first rebuild no sooner than 60 s after the loss, then 2 and 5 minutes (the cap).
#[test]
fn the_first_rebuild_waits_a_minute_and_the_waits_grow_to_the_cap() {
    let t = Instant::now();
    let mut h = armed(t);
    assert_eq!(h.permit(t), Err(Refusal::Spaced { left: s(60) }), "not at once");
    assert_eq!(h.permit(t + s(59)), Err(Refusal::Spaced { left: s(1) }));
    assert_eq!(h.permit(t + s(60)), Ok(()));
    let mut now = t + s(60);
    // Six attempts in all (60, 180, 480, 780, 1080, 1380 s after the loss).
    for wait in [120, 300, 300, 300, 300] {
        h.attempt_failed(now, false, 0);
        assert!(
            matches!(h.permit(now + s(wait - 1)), Err(Refusal::Spaced { .. })),
            "one second early, wait {wait}"
        );
        now += s(wait);
        assert_eq!(h.permit(now), Ok(()), "wait {wait}");
    }
    // The seventh would be on the schedule (another 300 s) but is the hour's seventh: the hard cap holds.
    h.attempt_failed(now, false, 0);
    assert_eq!(h.permit(now + s(300)), Err(Refusal::Capped { left: s(3600 + 60 - 1680) }));
}

#[test]
fn the_jitter_spreads_a_wait_by_a_tenth_at_most() {
    assert_eq!(Heal::wait_for(1, 0), s(60));
    assert_eq!(Heal::wait_for(1, 100), Duration::from_millis(66_000));
    assert_eq!(Heal::wait_for(1, -100), Duration::from_millis(54_000));
    assert_eq!(Heal::wait_for(1, 5_000), Duration::from_millis(66_000), "clamped");
    assert_eq!(Heal::wait_for(40, 0), s(300), "the cap");
}

/// The hard cap: six admitted attempts in an hour, whatever the schedule says (sessions that connect and
/// die again do not back the schedule off, the cap is what holds them).
#[test]
fn at_most_six_attempts_an_hour_whatever_the_schedule() {
    let t = Instant::now();
    let mut h = armed(t);
    let first = t + s(60);
    for i in 0..MAX_ATTEMPTS_PER_HOUR as u64 {
        assert_eq!(h.permit(first + s(i * 10)), Ok(()), "attempt {i}");
    }
    let seventh = first + s(60);
    assert_eq!(h.permit(seventh), Err(Refusal::Capped { left: s(3600 - 60) }));
    assert_eq!(h.permit(first + s(3600)), Ok(()), "the oldest left the hour");
}

#[test]
fn a_second_loss_inside_the_same_wait_is_the_same_outage() {
    let t = Instant::now();
    let mut h = armed(t);
    h.lost(t, t + s(5), 0);
    assert_eq!(h.failures(), 1, "two accounts on one gateway lose the link once");
    h.lost(t, t + s(61), 0);
    assert_eq!(h.failures(), 2, "a loss after the wait is a failed rebuild");
}

#[test]
fn only_a_timeout_arms_the_schedule_a_refusal_does_not() {
    let t = Instant::now();
    let mut h = Heal::default();
    h.attempt_failed(t, false, 0);
    assert!(!h.is_armed(), "a closed port is a restart, not a lost link");
    h.attempt_failed(t, true, 0);
    assert!(h.is_armed(), "a gateway that took the connection and never answered has no link");
    assert_eq!(h.failures(), 1);
    assert_eq!(h.permit(t + s(1)), Err(Refusal::Spaced { left: s(59) }));
}

#[test]
fn a_healthy_link_forgets_the_outage() {
    let t = Instant::now();
    let mut h = armed(t);
    h.attempt_failed(t + s(60), false, 0);
    h.healthy();
    assert!(!h.is_armed());
    assert_eq!(h.failures(), 0);
    assert_eq!(h.permit(t + s(61)), Ok(()));
}

/// THE OPERATOR LINE: loud, only after N failed rebuilds, never from the code restarting anything, and at
/// most once per 30 minutes.
#[test]
fn the_operator_is_asked_after_three_failures_and_at_most_every_half_hour() {
    let t = Instant::now();
    let mut h = armed(t);
    assert_eq!(h.operator_note(t), None, "a first loss alone is not the operator's yet");
    let mut now = t + s(60);
    h.attempt_failed(now, false, 0);
    assert_eq!(h.operator_note(now), None);
    now += s(120);
    h.attempt_failed(now, false, 0);
    assert_eq!(h.failures(), OPERATOR_AFTER_FAILURES);
    let said = h.operator_note(now).expect("three failures ask the operator");
    assert!(said.contains("IB Gateway has no link to IBKR for 3 min"), "{said}");
    assert!(said.contains("restart vike-ibkr-gateway (operator)"), "{said}");
    assert!(said.contains("never restarts the gateway itself"), "{said}");
    assert_eq!(h.operator_note(now + s(1)), None, "said once");
    assert_eq!(h.operator_note(now + OPERATOR_REPEAT - s(1)), None);
    let again = h.operator_note(now + OPERATOR_REPEAT).expect("said again after 30 minutes");
    assert!(again.contains("for 33 min"), "{again}");
    let mut quiet = Heal::default();
    assert_eq!(quiet.operator_note(now), None, "an unarmed gate has nothing to say");
}

#[test]
fn a_gateway_restart_is_a_port_that_closed_and_opened_again() {
    let mut w = PortWatch::default();
    assert!(!w.observe(true), "never closed");
    assert!(!w.observe(false));
    assert!(!w.observe(false));
    assert!(w.observe(true), "open again after two closed samples: the gateway restarted");
    assert!(!w.observe(true), "said once");
}

/// A single refused sample is a busy gateway, not a restart.
#[test]
fn one_closed_sample_is_not_a_restart() {
    let mut w = PortWatch::default();
    assert!(!w.observe(false));
    assert!(!w.observe(true), "closed once, then open: a blink");
    assert!(!w.observe(false));
    assert!(!w.observe(true), "the run of closed samples restarted from zero");
    assert!(!w.observe(false) && !w.observe(false));
    assert!(w.observe(true));
}

fn test_gate(gap_ms: u64) -> GatewayGate {
    GatewayGate::new("127.0.0.1:9", Duration::from_millis(gap_ms), false)
}

/// An armed gate dials NOTHING and says why, in words an operator reads in the not-running reason.
#[test]
fn an_armed_gate_refuses_an_exec_connect_without_dialling() {
    let gate = test_gate(10);
    assert!(gate.permit_exec().is_ok());
    let t = Instant::now();
    gate.heal().lost(t, t, 0);
    let Err(IbkrError::Connect(words)) = gate.permit_exec() else { panic!("armed: refused") };
    assert!(words.contains("link watchdog"), "{words}");
    assert!(words.contains("nothing was dialled"), "{words}");
    assert!(words.contains("allowed in"), "{words}");
}

/// The data lane is refused while the schedule is armed, dials nothing, and spends none of the exec's hourly attempts.
#[test]
fn an_armed_gate_refuses_a_data_connect_and_counts_no_attempt() {
    let gate = test_gate(10);
    assert!(gate.permit_data().is_ok(), "unarmed: the data lane dials");
    let t = Instant::now();
    gate.heal().lost(t, t, 0);
    for _ in 0..(MAX_ATTEMPTS_PER_HOUR + 2) {
        let Err(IbkrError::Connect(words)) = gate.permit_data() else { panic!("armed: refused") };
        assert!(
            words.contains("link watchdog") && words.contains("nothing was dialled"),
            "{words}"
        );
    }
    assert!(gate.heal().attempts.is_empty(), "a data refusal is not an exec attempt");
}

/// ONE DIAL AT A TIME, SPACED: four threads on one gateway never overlap, and each starts at least the gap after
/// the previous one ended.
#[test]
fn dials_on_one_gateway_never_overlap_and_are_spaced() {
    let gap = Duration::from_millis(40);
    let gate = Arc::new(test_gate(40));
    let spans = Arc::new(Mutex::new(Vec::<(Instant, Instant)>::new()));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (gate, spans) = (Arc::clone(&gate), Arc::clone(&spans));
            thread::spawn(move || {
                let log = Arc::clone(&spans);
                let (done, timed_out) = gate.dial("step", s(5), move || {
                    let began = Instant::now();
                    thread::sleep(Duration::from_millis(30));
                    log.lock().unwrap().push((began, Instant::now()));
                    Ok(())
                });
                assert!(done.is_ok() && !timed_out);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let mut spans = spans.lock().unwrap().clone();
    spans.sort();
    assert_eq!(spans.len(), 4);
    for pair in spans.windows(2) {
        assert!(pair[1].0 >= pair[0].1 + gap, "overlap or no gap: {pair:?}");
    }
}

#[test]
fn a_dial_that_fails_is_not_a_timeout() {
    let gate = test_gate(0);
    let (done, timed_out) =
        gate.dial::<()>("step", s(5), || Err(IbkrError::Connect("refused".into())));
    assert!(matches!(done, Err(IbkrError::Connect(_))));
    assert!(!timed_out, "the gateway answered, with a refusal");
}

/// An open the deadline abandoned still holds its client id until it returns: the NEXT dial on that gateway
/// waits for it (error 326 otherwise), and a timeout is flagged as one.
#[test]
fn the_next_dial_waits_for_an_abandoned_open_to_return() {
    // Deterministic: the abandoned open blocks on a channel the test controls, and the order of events is what is
    // asserted, never a wall-clock margin.
    let gate = Arc::new(test_gate(5));
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (release, released) = std::sync::mpsc::channel::<()>();
    let abandoned_log = Arc::clone(&events);
    let (done, timed_out) = gate.dial::<()>("slow", Duration::from_millis(50), move || {
        let _ = released.recv();
        abandoned_log.lock().unwrap().push("abandoned open returned");
        Ok(())
    });
    assert!(done.is_err() && timed_out, "the deadline won");
    assert_eq!(gate.settling_now(), 1, "the abandoned thread is still out");

    let next_log = Arc::clone(&events);
    let next_gate = Arc::clone(&gate);
    let next = thread::spawn(move || {
        next_gate.dial("next", s(30), move || {
            next_log.lock().unwrap().push("next dial ran");
            Ok(7)
        })
    });
    // The next dial must not run while the abandoned open is out. (This wait can only miss a bug, never invent one.)
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        *events.lock().unwrap(),
        Vec::<&str>::new(),
        "the next dial ran before the abandoned open returned"
    );

    release.send(()).unwrap();
    let (next, _) = next.join().unwrap();
    assert_eq!(next, Ok(7));
    assert_eq!(*events.lock().unwrap(), ["abandoned open returned", "next dial ran"]);
    // The dial returns when its result is RECEIVED; its worker lets go of the count a moment later.
    let deadline = Instant::now() + s(30);
    while gate.settling_now() != 0 {
        assert!(Instant::now() < deadline, "the count never went back to zero");
        thread::sleep(Duration::from_millis(5));
    }
}

// --- the data lane never delays the order lane ----------------------------------------------------------------

/// The data lane's backoff on a fake clock: a dial the deadline won spaces the next one 60 s, 2 and 5 min; an answer
/// clears it.
#[test]
fn a_data_dial_the_deadline_won_backs_the_next_off_and_an_answer_clears_it() {
    let t = Instant::now();
    let mut d = DataDials::default();
    assert_eq!(d.check(t, DataLane::Lookup), Ok(()));
    d.admit(t, DataLane::Lookup);
    d.timed_out(t, 0);
    assert_eq!(d.check(t + s(59), DataLane::Lookup), Err(Refusal::Spaced { left: s(1) }));
    assert_eq!(d.check(t + s(60), DataLane::Lookup), Ok(()));
    d.timed_out(t + s(60), 0);
    let early = d.check(t + s(179), DataLane::Redial);
    assert_eq!(
        early,
        Err(Refusal::Spaced { left: s(1) }),
        "the 2 minute wait, whichever lane asks"
    );
    d.timed_out(t + s(180), 0);
    assert_eq!(d.check(t + s(479), DataLane::Lookup), Err(Refusal::Spaced { left: s(1) }), "5 min");
    d.timed_out(t + s(480), 0);
    let capped = d.check(t + s(779), DataLane::Lookup);
    assert_eq!(capped, Err(Refusal::Spaced { left: s(1) }), "the cap repeats");
    d.answered();
    assert_eq!(d.check(t + s(481), DataLane::Lookup), Ok(()), "an answered dial ends the backoff");
}

/// The data lanes spend ONE hourly budget of six: a refused check counts nothing, an answer refunds nothing.
#[test]
fn data_dials_are_counted_against_an_hourly_budget() {
    let t = Instant::now();
    let mut d = DataDials::default();
    // Four lookups and two re-dials fill the six without either lane passing its own cap.
    let (l, r) = (DataLane::Lookup, DataLane::Redial);
    for (i, lane) in [l, r, l, r, l, l].into_iter().enumerate() {
        let at = t + s(i as u64);
        assert_eq!(d.check(at, lane), Ok(()), "attempt {i}");
        d.admit(at, lane);
    }
    d.answered();
    let seventh = t + s(60);
    for lane in [DataLane::Lookup, DataLane::Redial] {
        assert_eq!(
            d.check(seventh, lane),
            Err(Refusal::Capped { left: s(3600 - 60) }),
            "an answer refunds nothing, and a refusal counts nothing"
        );
    }
    assert_eq!(d.check(t + s(3600), DataLane::Lookup), Ok(()), "the oldest left the hour");
}

/// **The policy that shares the budget**: neither lane can take the last `LANE_RESERVE` from the other. Lookups
/// alone stop at four of the six, so a re-dial still has two; re-dials alone stop at four, so a lookup still has two;
/// a lane that is capped waits for ITS OWN oldest attempt to leave the hour, not for the other lane's.
#[test]
fn neither_data_lane_can_starve_the_other() {
    let own = MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE;
    assert_eq!(own, 4, "the policy in numbers: four of six each, two kept for the other");
    for (hog, victim) in
        [(DataLane::Lookup, DataLane::Redial), (DataLane::Redial, DataLane::Lookup)]
    {
        let t = Instant::now();
        let mut d = DataDials::default();
        for i in 0..own as u64 {
            assert_eq!(d.check(t + s(i), hog), Ok(()), "{hog:?} attempt {i}");
            d.admit(t + s(i), hog);
        }
        let now = t + s(10);
        assert_eq!(
            d.check(now, hog),
            Err(Refusal::Capped { left: s(3600 - 10) }),
            "{hog:?} is held to its four; it frees when ITS oldest leaves the hour"
        );
        for i in 0..LANE_RESERVE as u64 {
            assert_eq!(d.check(now + s(i), victim), Ok(()), "{victim:?} keeps its reserve {i}");
            d.admit(now + s(i), victim);
        }
        let later = now + s(LANE_RESERVE as u64);
        assert!(d.check(later, victim).is_err(), "all six are spent");
        // The hog's oldest leaves the hour: a slot is free for the lane whose attempt it was.
        assert_eq!(d.check(t + s(3600), hog), Ok(()), "{hog:?} gets its oldest slot back");
    }
}

/// The lane a dial names is the share it spends: four re-dials spend the re-dial lane's share, the fifth is refused
/// without dialling and says the cap, and a lookup still dials.
#[test]
fn a_data_dial_spends_the_share_of_the_lane_it_names() {
    let gate = test_gate(0);
    for n in 0..(MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE) {
        assert_eq!(gate.dial_data(DataLane::Redial, "data", s(5), move || Ok(n)), Ok(n));
    }
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let Err(IbkrError::Connect(words)) =
        gate.dial_data(DataLane::Redial, "data", s(5), move || {
            seen.store(true, Ordering::SeqCst);
            Ok(())
        })
    else {
        panic!("the lane's share is spent")
    };
    assert!(words.contains("hourly cap") && words.contains("nothing was dialled"), "{words}");
    assert!(!ran.load(Ordering::SeqCst), "the refused dial never opened");
    let lookup = gate.dial_data(DataLane::Lookup, "data", s(5), || Ok(1));
    assert_eq!(lookup, Ok(1), "a lookup still has its reserve");
}

/// `data_ready` is the answer a retrying caller asks BEFORE it dials, and spends nothing: ready on a quiet gateway,
/// not while the rebuild schedule is armed, an order-lane dial waits, a dial holds the turn, or the lane's budget is
/// spent (and then it says how long).
#[test]
fn data_ready_answers_without_spending_anything() {
    let gate = Arc::new(test_gate(10));
    for _ in 0..3 {
        assert_eq!(gate.data_ready(DataLane::Redial), Ok(()), "asking is free, however often");
    }
    assert!(gate.data.lock().unwrap().attempts.is_empty(), "nothing was spent by asking");

    // A lane's budget: the reply carries the wait.
    {
        let mut d = gate.data.lock().unwrap();
        for _ in 0..(MAX_ATTEMPTS_PER_HOUR - LANE_RESERVE) {
            d.admit(Instant::now(), DataLane::Redial);
        }
    }
    let Err(left) = gate.data_ready(DataLane::Redial) else { panic!("the lane's four are spent") };
    assert!(left > s(3500) && left <= s(3600), "{left:?}");
    assert_eq!(gate.data_ready(DataLane::Lookup), Ok(()), "the other lane still has its reserve");

    // The armed schedule refuses the data lane outright.
    let armed = test_gate(10);
    let t = Instant::now();
    armed.heal().lost(t, t, 0);
    assert!(armed.data_ready(DataLane::Redial).is_err());
}

/// While an order-lane dial holds the turn, a data dial is not READY: the caller waits instead of being refused.
#[test]
fn data_is_not_ready_while_an_order_dial_holds_the_turn() {
    let gate = Arc::new(test_gate(10));
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let order_gate = Arc::clone(&gate);
    let order = thread::spawn(move || {
        order_gate.dial("order", s(30), move || {
            entered_tx.send(()).unwrap();
            let _ = released.recv();
            Ok(())
        })
    });
    entered.recv().unwrap();
    assert_eq!(gate.data_ready(DataLane::Redial), Err(RECHECK));
    release.send(()).unwrap();
    assert!(order.join().unwrap().0.is_ok());
}

/// An order-lane dial that arrives while a data open holds the turn is NOT delayed by it: the data dial gives way
/// at once, the order dial runs while the abandoned data open is still blocked (it is counted apart, so the order
/// lane never waits for it) and owes no gap after it, and the data lane is not backed off for having yielded. For BOTH
/// data lanes: the supervised re-dial (`DataLane::Redial`) never delays the order lane's reconnect either.
#[test]
fn an_order_lane_dial_never_waits_for_a_data_dial() {
    for lane in LANES {
        an_order_lane_dial_never_waits_for(lane);
    }
}

fn an_order_lane_dial_never_waits_for(lane: DataLane) {
    let gate = Arc::new(test_gate(5_000));
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let data_gate = Arc::clone(&gate);
    let data = thread::spawn(move || {
        data_gate.dial_data::<()>(lane, "data", s(30), move || {
            entered_tx.send(()).unwrap();
            let _ = released.recv();
            Ok(())
        })
    });
    entered.recv().unwrap(); // the data open is in flight and holds the turn

    let began = Instant::now();
    let (order, timed_out) = gate.dial("order", s(5), || Ok(7));
    assert_eq!(order, Ok(7));
    assert!(!timed_out);
    assert!(
        began.elapsed() < s(3),
        "the order dial waited {:?} (the data open's bound is 30 s, the gap 5 s)",
        began.elapsed()
    );

    let Err(IbkrError::Connect(words)) = data.join().unwrap() else {
        panic!("the data dial yielded")
    };
    assert!(words.contains("gave way to an order-lane connect"), "{words}");
    assert_eq!(
        gate.data_settling.load(Ordering::SeqCst),
        1,
        "the abandoned data open is still out, counted apart"
    );
    // The order dial returns when its result is RECEIVED; its worker lets go of the order lane's count a moment later.
    let deadline = Instant::now() + s(30);
    while gate.settling_now() != 0 {
        assert!(Instant::now() < deadline, "...the order lane's own count never went back to zero");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        gate.data_settling.load(Ordering::SeqCst),
        1,
        "...and the order lane's count never touched the data open's"
    );
    assert_eq!(
        gate.data.lock().unwrap().check(Instant::now(), lane),
        Ok(()),
        "yielding is not a failure"
    );
    release.send(()).unwrap();
}

/// A data dial never QUEUES for the turn: while an order-lane dial holds it, the data dial is refused at once and its
/// open is never started (either data lane).
#[test]
fn a_data_dial_is_refused_while_an_order_dial_holds_the_turn() {
    for lane in LANES {
        a_data_dial_is_refused_while_an_order_dial_holds_the(lane);
    }
}

fn a_data_dial_is_refused_while_an_order_dial_holds_the(lane: DataLane) {
    let gate = Arc::new(test_gate(10));
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let order_gate = Arc::clone(&gate);
    let order = thread::spawn(move || {
        order_gate.dial("order", s(30), move || {
            entered_tx.send(()).unwrap();
            let _ = released.recv();
            Ok(())
        })
    });
    entered.recv().unwrap();

    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let began = Instant::now();
    let Err(IbkrError::Connect(words)) = gate.dial_data(lane, "data", s(30), move || {
        seen.store(true, Ordering::SeqCst);
        Ok(())
    }) else {
        panic!("the turn is held")
    };
    assert!(
        words.contains("another connect holds the gateway's turn")
            && words.contains("nothing was dialled"),
        "{words}"
    );
    assert!(!ran.load(Ordering::SeqCst), "the data open never started");
    assert!(began.elapsed() < s(3), "refused at once, not queued: {:?}", began.elapsed());
    release.send(()).unwrap();
    assert!(order.join().unwrap().0.is_ok());
}

/// A data dial still waiting out the gap after the previous dial gives way to an order-lane dial that arrives: its
/// open never starts (either data lane).
#[test]
fn a_data_dial_waiting_out_the_gap_gives_way_to_an_order_dial() {
    for lane in LANES {
        a_data_dial_waiting_out_the_gap_gives_way(lane);
    }
}

fn a_data_dial_waiting_out_the_gap_gives_way(lane: DataLane) {
    let gate = Arc::new(test_gate(1_000));
    assert!(gate.dial("first", s(5), || Ok(())).0.is_ok());
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let data_gate = Arc::clone(&gate);
    let data = thread::spawn(move || {
        data_gate.dial_data(lane, "data", s(30), move || {
            seen.store(true, Ordering::SeqCst);
            Ok(())
        })
    });
    // Whichever of the two reaches the turn first, the data dial ends in a refusal and its open never runs.
    assert!(gate.dial("second", s(5), || Ok(())).0.is_ok());
    assert!(matches!(data.join().unwrap(), Err(IbkrError::Connect(_))));
    assert!(!ran.load(Ordering::SeqCst), "the data open never started");
}

/// A data dial the deadline won arms the DATA backoff (not the order lane's schedule): the next data dial is refused
/// without dialling, and the order lane is unaffected.
#[test]
fn a_timed_out_data_dial_arms_a_data_backoff_and_nothing_else() {
    let gate = test_gate(0);
    let (release, released) = std::sync::mpsc::channel::<()>();
    let Err(IbkrError::Connect(words)) =
        gate.dial_data::<()>(DataLane::Lookup, "data", Duration::from_millis(60), move || {
            let _ = released.recv();
            Ok(())
        })
    else {
        panic!("the deadline won")
    };
    assert!(words.contains("did not answer"), "{words}");
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let Err(IbkrError::Connect(words)) =
        gate.dial_data(DataLane::Lookup, "data", s(5), move || {
            seen.store(true, Ordering::SeqCst);
            Ok(())
        })
    else {
        panic!("backed off")
    };
    assert!(
        words.contains("data connects")
            && words.contains("allowed in")
            && words.contains("nothing was dialled"),
        "{words}"
    );
    assert!(!ran.load(Ordering::SeqCst));
    assert!(!gate.is_armed(), "the order lane's schedule is not armed by a data timeout");
    assert!(gate.permit_exec().is_ok());
    assert_eq!(gate.dial("order", s(5), || Ok(1)).0, Ok(1), "the order lane dials at once");
    release.send(()).unwrap();
}

/// What a data dial under an ask's deadline reported: whether its open ran, how many hourly dials it spent, how long
/// it took and the words of the refusal (if it was one).
struct Tried {
    ran: bool,
    spent: usize,
    took: Duration,
    refusal: Option<String>,
}

/// A data dial of `lane` on `gate`, `within` for the open step, under an ask that has `total` in all.
fn try_under(gate: &GatewayGate, lane: DataLane, within: Duration, total: Duration) -> Tried {
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let began = Instant::now();
    let got =
        gate.dial_data_by(lane, "data", within, Some(AskDeadline::within(total)), move || {
            seen.store(true, Ordering::SeqCst);
            Ok(())
        });
    Tried {
        ran: ran.load(Ordering::SeqCst),
        spent: gate.data_dials_spent(),
        took: began.elapsed(),
        refusal: got.err().map(|e| e.to_string()),
    }
}

/// **An ask's deadline is TOTAL** (finding 4 of the second IBKR currency review): the connect gap that follows the
/// previous dial is waited out only while the ask has the time. A gap longer than the ask ends the wait AT the
/// ask's deadline, with nothing dialled and none of the lane's hourly dials spent (either data lane); without a
/// deadline the same dial waits the gap out and dials, as it always did.
#[test]
fn an_ask_does_not_wait_out_a_connect_gap_longer_than_its_time() {
    for lane in LANES {
        let gate = test_gate(2_000);
        assert!(gate.dial("first", s(5), || Ok(())).0.is_ok());
        let tried = try_under(&gate, lane, s(5), Duration::from_millis(400));
        assert!(!tried.ran, "{lane:?}: the open never started");
        assert_eq!(tried.spent, 0, "{lane:?}: no hourly dial was spent");
        let words = tried.refusal.expect("refused");
        assert!(words.contains("ran out") && words.contains("nothing was dialled"), "{words}");
        assert!(
            tried.took < Duration::from_millis(1_200),
            "{lane:?}: the wait ended at the ask's deadline, not at the gap's: {:?}",
            tried.took
        );
    }
    let gate = test_gate(300);
    assert!(gate.dial("first", s(5), || Ok(())).0.is_ok());
    assert_eq!(
        gate.dial_data(DataLane::Lookup, "data", s(5), || Ok(7)),
        Ok(7),
        "no deadline: it waits"
    );
    assert_eq!(gate.data_dials_spent(), 1);
}

/// **A dial that would leave the ask less than a quarter of its time is not made**: the gap ate the time, so the
/// connect and the read after it could not finish, and the dial would spend one of the six hourly ones for
/// nothing. With more than the quarter left the dial is admitted and its open step is held to what is left.
#[test]
fn a_dial_that_leaves_the_ask_too_little_time_is_not_spent() {
    let gate = test_gate(700);
    assert!(gate.dial("first", s(5), || Ok(())).0.is_ok());
    // 800 ms in all, 700 ms of gap: about 100 ms are left, under the 200 ms (a quarter) a dial is worth.
    let tried = try_under(&gate, DataLane::Lookup, s(5), Duration::from_millis(800));
    assert!(!tried.ran && tried.spent == 0, "refused before it was admitted");
    // (Either refusal: a box so loaded that the gap overshot the whole 800 ms says "ran out" instead.)
    let words = tried.refusal.expect("refused");
    assert!(words.contains("nothing was dialled"), "{words}");

    // The same gap with room to spare: admitted, spent, and bounded by the ask (not by the 30 s asked for).
    let gate = test_gate(100);
    assert!(gate.dial("first", s(5), || Ok(())).0.is_ok());
    let (release, released) = std::sync::mpsc::channel::<()>();
    let began = Instant::now();
    let got = gate.dial_data_by::<()>(
        DataLane::Lookup,
        "data",
        s(30),
        Some(AskDeadline::within(Duration::from_millis(900))),
        move || {
            let _ = released.recv();
            Ok(())
        },
    );
    assert!(got.is_err(), "the open step was bounded by the ask's time, not by its own 30 s");
    assert!(began.elapsed() < s(3), "{:?}", began.elapsed());
    assert_eq!(gate.data_dials_spent(), 1, "that dial was worth it and was spent");
    release.send(()).unwrap();
}
