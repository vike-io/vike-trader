use super::*;

/// A probe over scripted hosts with an explicit debounce threshold.
fn probe(hosts: &[&str], failures_before_down: u32) -> NetProbe {
    NetProbe::new(NetProbeConfig {
        hosts: hosts.iter().map(|h| (*h).to_string()).collect(),
        interval: DEFAULT_PROBE_INTERVAL,
        failures_before_down,
    })
    .expect("non-empty host list")
}

// ---- initial state: optimistic, and honest about not having measured yet ---------------------

/// Before any round, the flag reads up (no phantom outage for an early reader) but `has_probed`
/// discloses that this is the initial value, not a measurement.
#[test]
fn starts_optimistically_up_and_unprobed() {
    let p = NetProbe::with_defaults();
    let h = p.handle();
    assert!(h.internet_up(), "an early reader must not see a phantom outage");
    assert!(!h.has_probed(), "nothing has been measured yet");
    assert_eq!(h.checks(), 0);
    assert_eq!(h.transitions(), 0);
    assert_eq!(h.last_check_ms(), 0);
    assert_eq!(h.downtime_ms(1_000), None, "up ⇒ no downtime");
}

/// Constructing a probe spawns nothing and touches no network (the OFF-by-default contract).
#[test]
fn construction_alone_probes_nothing() {
    let p = NetProbe::with_defaults();
    assert_eq!(p.handle().checks(), 0, "no round runs without an explicit call");
    assert_eq!(p.hosts().len(), DEFAULT_PROBE_HOSTS.len());
}

// ---- the debounce: slow to condemn ----------------------------------------------------------

/// One failed round below the threshold must NOT flip the flag — a single DNS hiccup is not an
/// outage.
#[test]
fn a_single_failure_does_not_flip_the_flag() {
    let p = probe(&["a", "b"], 2);
    assert_eq!(p.probe_once_with(|_| false, 1_000), None, "no flip on the first failure");
    let h = p.handle();
    assert!(h.internet_up(), "still up — the debounce has not been reached");
    assert!(h.has_probed(), "but a round DID complete");
    assert_eq!(h.checks(), 1);
    assert_eq!(h.transitions(), 0);
    assert_eq!(h.last_check_ms(), 1_000);
}

/// The Nth consecutive failure flips it down and stamps the transition timestamp.
#[test]
fn consecutive_failures_reaching_the_threshold_flip_it_down() {
    let p = probe(&["a", "b"], 2);
    assert_eq!(p.probe_once_with(|_| false, 1_000), None);
    assert_eq!(
        p.probe_once_with(|_| false, 2_000),
        Some(NetTransition::WentDown { at_ms: 2_000 }),
        "the second consecutive failure trips it"
    );
    let h = p.handle();
    assert!(!h.internet_up());
    assert_eq!(h.last_down_ms(), 2_000, "the transition timestamp is stamped");
    assert_eq!(h.transitions(), 1);
    assert_eq!(h.downtime_ms(6_000), Some(4_000), "down for 4s as of t=6000");
}

/// Staying down emits no further transitions — the edge fires once, not every round.
#[test]
fn staying_down_emits_no_further_transitions() {
    let p = probe(&["a"], 1);
    assert_eq!(p.probe_once_with(|_| false, 1_000), Some(NetTransition::WentDown { at_ms: 1_000 }));
    assert_eq!(p.probe_once_with(|_| false, 2_000), None, "already down");
    assert_eq!(p.probe_once_with(|_| false, 3_000), None, "still already down");
    let h = p.handle();
    assert_eq!(h.transitions(), 1, "exactly one flip");
    assert_eq!(h.last_down_ms(), 1_000, "the ORIGINAL down time, not the latest round");
    assert_eq!(h.checks(), 3, "but all three rounds counted");
}

/// An interrupted failure streak resets the debounce: fail, succeed, fail must NOT flip down
/// with a threshold of 2 (the streak restarted).
#[test]
fn a_success_resets_the_failure_streak() {
    let p = probe(&["a"], 2);
    assert_eq!(p.probe_once_with(|_| false, 1_000), None);
    assert_eq!(p.probe_once_with(|_| true, 2_000), None, "up→up is not a transition");
    assert_eq!(p.probe_once_with(|_| false, 3_000), None, "streak restarted, so no flip yet");
    assert!(p.handle().internet_up());
    assert_eq!(
        p.probe_once_with(|_| false, 4_000),
        Some(NetTransition::WentDown { at_ms: 4_000 }),
        "now two in a row"
    );
}

/// A threshold of 0 is normalised to 1 rather than meaning "never flip down".
#[test]
fn a_zero_threshold_is_treated_as_one() {
    let p = probe(&["a"], 0);
    assert_eq!(
        p.probe_once_with(|_| false, 500),
        Some(NetTransition::WentDown { at_ms: 500 }),
        "a 0 threshold must not disable the flag"
    );
}

// ---- recovery: quick to forgive -------------------------------------------------------------

/// One success recovers immediately — recovery is never debounced.
#[test]
fn one_success_recovers_immediately() {
    let p = probe(&["a"], 2);
    let _ = p.probe_once_with(|_| false, 1_000);
    let _ = p.probe_once_with(|_| false, 2_000);
    assert!(!p.handle().internet_up());
    assert_eq!(
        p.probe_once_with(|_| true, 3_000),
        Some(NetTransition::CameUp { at_ms: 3_000 }),
        "the very first success recovers"
    );
    let h = p.handle();
    assert!(h.internet_up());
    assert_eq!(h.last_up_ms(), 3_000);
    assert_eq!(h.last_down_ms(), 2_000, "the prior down stamp is retained");
    assert_eq!(h.transitions(), 2, "down + up");
    assert_eq!(h.downtime_ms(9_000), None, "recovered ⇒ no downtime");
}

/// A full down→up→down cycle counts three transitions and keeps both stamps current — the
/// flapping signature a consumer needs to distinguish from one clean outage.
#[test]
fn a_flapping_link_is_visible_in_the_transition_count() {
    let p = probe(&["a"], 1);
    let _ = p.probe_once_with(|_| false, 1_000); // down
    let _ = p.probe_once_with(|_| true, 2_000); // up
    let _ = p.probe_once_with(|_| false, 3_000); // down again
    let h = p.handle();
    assert_eq!(h.transitions(), 3);
    assert_eq!(h.last_down_ms(), 3_000, "latest down");
    assert_eq!(h.last_up_ms(), 2_000, "latest up");
    assert!(!h.internet_up());
}

// ---- the host fold: ANY host resolving means up ---------------------------------------------

/// One reachable host is enough — one operator's DNS outage must not read as our internet dying.
#[test]
fn any_single_resolving_host_keeps_it_up() {
    let p = probe(&["a", "b", "c"], 1);
    assert_eq!(p.probe_once_with(|h| h == "c", 1_000), None, "the last host alone suffices");
    assert!(p.handle().internet_up());
}

/// A down verdict requires EVERY host to fail to resolve.
#[test]
fn down_requires_every_host_to_fail() {
    let p = probe(&["a", "b", "c"], 1);
    let mut tried = Vec::new();
    let flip = p.probe_once_with(
        |h| {
            tried.push(h.to_string());
            false
        },
        1_000,
    );
    assert_eq!(flip, Some(NetTransition::WentDown { at_ms: 1_000 }));
    assert_eq!(tried, ["a", "b", "c"], "all hosts were tried before condemning");
}

/// The healthy path short-circuits on the first resolving host (one lookup, not N).
#[test]
fn the_healthy_path_short_circuits_on_the_first_host() {
    let p = probe(&["a", "b", "c"], 1);
    let mut tried = 0usize;
    let _ = p.probe_once_with(
        |_| {
            tried += 1;
            true
        },
        1_000,
    );
    assert_eq!(tried, 1, "any() stops at the first success");
}

// ---- construction invariants ----------------------------------------------------------------

/// An empty host list is rejected: it could only ever fold to "unreachable".
#[test]
fn new_rejects_an_empty_host_list() {
    let cfg = NetProbeConfig { hosts: vec![], ..NetProbeConfig::default() };
    assert!(NetProbe::new(cfg).is_none());
}

/// The defaults use MORE THAN ONE independently-operated host (no single point of failure).
#[test]
fn defaults_have_no_single_point_of_failure() {
    assert!(DEFAULT_PROBE_HOSTS.len() > 1);
    assert_eq!(NetProbe::with_defaults().hosts().len(), DEFAULT_PROBE_HOSTS.len());
    // Names, not literal IPs — resolving them is what exercises the local resolver.
    for h in DEFAULT_PROBE_HOSTS {
        assert!(h.parse::<std::net::IpAddr>().is_err(), "{h} must be a NAME, not a literal IP");
    }
}

/// Handles share one state: a flip observed through the probe is visible to every reader.
#[test]
fn handles_share_the_same_state() {
    let p = probe(&["a"], 1);
    let (h1, h2) = (p.handle(), p.handle().clone());
    let _ = p.probe_once_with(|_| false, 7_000);
    assert!(!h1.internet_up());
    assert!(!h2.internet_up(), "a cloned handle sees the same state");
    assert_eq!(h2.last_down_ms(), 7_000);
}

// ---- the REAL resolver path, offline-deterministic -------------------------------------------

/// `dns_resolves` is wired to real name resolution. Asserted with a LITERAL IP, which
/// `to_socket_addrs` parses without any DNS traffic — so this is deterministic on any box,
/// online or not. (The NXDOMAIN-negative is deliberately NOT asserted against the real resolver:
/// resolvers that hijack NXDOMAIN would answer even for a reserved `.invalid` name, making it
/// environment-dependent. The failure fold is fully covered by the injected-resolver tests.)
#[test]
fn dns_resolves_is_true_for_a_literal_address() {
    assert!(dns_resolves("127.0.0.1"), "a literal IP resolves without DNS");
}

/// The wall clock is the only real clock and returns a sane epoch-ms value.
#[test]
fn wall_clock_ms_is_a_plausible_epoch_ms() {
    // 2020-01-01 in epoch-ms; any real clock is well past it.
    assert!(wall_clock_ms() > 1_577_836_800_000);
}

// ---- the spawned thread: opt-in lifecycle, no real network needed ----------------------------

/// `spawn` starts the monitor and `join` tears it down deterministically. The probe resolves the
/// literal-IP host, so the round needs no network. A short interval keeps the test quick.
#[test]
fn spawn_runs_rounds_and_join_stops_it() {
    let p = NetProbe::new(NetProbeConfig {
        hosts: vec!["127.0.0.1".to_string()],
        interval: Duration::from_millis(50),
        failures_before_down: DEFAULT_FAILURES_BEFORE_DOWN,
    })
    .expect("one host");
    let thread = p.spawn();
    let h = thread.handle();
    // Wait (bounded) for the first round rather than sleeping a fixed span.
    let start = std::time::Instant::now();
    while !h.has_probed() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.has_probed(), "the spawned thread ran at least one round");
    assert!(h.internet_up(), "the literal-IP host resolves, so it stays up");
    thread.join();
}

/// Dropping the handle signals stop without joining (never blocks an unrelated teardown).
#[test]
fn dropping_the_thread_handle_signals_stop() {
    let p = NetProbe::new(NetProbeConfig {
        hosts: vec!["127.0.0.1".to_string()],
        interval: Duration::from_millis(50),
        failures_before_down: DEFAULT_FAILURES_BEFORE_DOWN,
    })
    .expect("one host");
    let state = Arc::clone(&p.state);
    drop(p.spawn());
    assert!(state.stop.load(Ordering::Relaxed), "Drop set the stop flag");
}
