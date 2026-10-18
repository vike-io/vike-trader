//! The rebuild schedule, the operator line and the connect turn, on a FAKE clock (`Instant` plus an offset: the
//! pure halves take `now`) and, for the turn, real threads over a short gap.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

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
