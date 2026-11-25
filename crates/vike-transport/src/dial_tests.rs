//! The bounded dial's one deadline, offline: the deadline helper, every address tried, resolution debited from the
//! window, the whole phase bounded however many addresses resolve, and the public dial against a loopback listener.

use super::*;

/// A listening loopback socket, and a loopback port with certainly nothing on it (port 1: below
/// every ephemeral range, so no test's listener can be handed it, as a port bound and released
/// can be) — the two ends every dial test below needs, with no network.
///
/// A closed loopback port answers RST immediately on every OS, which is what makes
/// "unreachable address" deterministic here rather than a timing gamble.
fn live_and_dead() -> (std::net::TcpListener, SocketAddr, SocketAddr) {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let live_address = live.local_addr().unwrap();
    (live, live_address, SocketAddr::from(([127, 0, 0, 1], 1)))
}

/// **The resolver cannot outlive the bound.** `getaddrinfo` has no timeout in its API, so an
/// unguarded resolution hangs a "bounded" dial for as long as libc likes.
///
/// The mechanism, not the DNS, is what is testable offline — so this drives
/// [`call_with_deadline`] with a call that would block for a minute and asserts the wait ends at
/// the deadline. Restore a `join()` (or any wait on the call itself) and this test hangs until
/// the closure finishes.
#[test]
fn a_blocking_call_cannot_outlive_its_deadline() {
    let started = Instant::now();
    let out = call_with_deadline(Duration::from_millis(120), || {
        std::thread::sleep(Duration::from_secs(60));
        "the resolver eventually answered"
    });
    let elapsed = started.elapsed();
    assert!(out.is_none(), "the deadline must win, got {out:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the bound did not bound: waited {elapsed:?} for a 120 ms deadline"
    );
}

/// …and the deadline does not COST anything when the call answers: a prompt call still returns
/// its value. Without this, "return `None` immediately" would pass the test above.
#[test]
fn a_prompt_call_still_returns_its_value() {
    assert_eq!(call_with_deadline(Duration::from_secs(30), || 7 + 1), Some(8));
}

/// **Every resolved address is tried, not just the first**, as the `tungstenite::connect` arm
/// does: a venue edge name resolves to several addresses, and first-only fails a connect the
/// unbounded arm would complete.
///
/// Deterministic and offline: the first address is a closed loopback port (instant RST), the
/// second a live listener. Reinstate first-only and the dial fails.
#[test]
fn every_resolved_address_is_tried_not_just_the_first() {
    let (_live, live_address, dead_address) = live_and_dead();
    let tcp = dial_bounded_with(
        "venue.example",
        443,
        Duration::from_secs(10),
        None,
        &|_: &str, _: u16, _: Duration| Ok(vec![dead_address, live_address]),
        &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
    )
    .expect("the second resolved address is reachable, so the dial must succeed");
    assert_eq!(
        tcp.peer_addr().unwrap(),
        live_address,
        "the dial must have fallen through to the reachable address"
    );
}

/// **Resolution is DEBITED from the window, not added to it.** The bound is a ceiling on
/// the whole TCP phase, which only holds if the time resolution spent is gone from what the
/// connects may spend. Asserted on the timeout the connect step is actually HANDED, so it
/// cannot be satisfied by luck.
///
/// Hand the attempts the full `bound` again and the recorded value jumps back to it — a phase
/// ceiling of `resolve + n × bound`, which is what the callers' budget arithmetic denies.
#[test]
fn resolution_time_is_spent_from_the_same_window_as_the_connects() {
    let bound = Duration::from_millis(600);
    let resolve_cost = Duration::from_millis(250);
    let handed = std::sync::Mutex::new(Vec::<Duration>::new());
    let (_live, _live_address, dead_address) = live_and_dead();

    let err = dial_bounded_with(
        "venue.example",
        443,
        bound,
        None,
        &|_: &str, _: u16, _: Duration| {
            std::thread::sleep(resolve_cost);
            Ok(vec![dead_address])
        },
        &|addr, timeout| {
            handed.lock().unwrap().push(timeout);
            TcpStream::connect_timeout(addr, timeout)
        },
    )
    .expect_err("a closed port cannot connect");

    let handed = handed.lock().unwrap().clone();
    assert_eq!(handed.len(), 1, "one address, one attempt");
    assert!(
        handed[0] < bound - resolve_cost + Duration::from_millis(150),
        "the connect was handed {:?} of a {bound:?} window after resolution had already spent \
             {resolve_cost:?} — resolution is not being debited, so `bound` bounds no phase",
        handed[0]
    );
    assert!(err.to_string().contains("venue.example:443"), "unexpected: {err}");
}

/// **The whole phase is bounded HOWEVER MANY addresses resolve.** Trying every address is only
/// safe because they share one deadline; per-attempt bounds would make the real ceiling
/// `n × bound` for an `n` the venue's DNS picks. Five black-holing addresses under a 300 ms window
/// must still cost ~300 ms, not 1.5 s; each is handed its share, so all five get their turn, and the
/// error is a timeout (`TimedOut`), which a probe reads as "no answer yet", not "refused".
#[test]
fn the_whole_tcp_phase_is_bounded_however_many_addresses_resolve() {
    let bound = Duration::from_millis(300);
    let addrs: Vec<SocketAddr> =
        (1..=5u8).map(|i| SocketAddr::from(([192, 0, 2, i], 443))).collect();
    let attempts = std::sync::atomic::AtomicUsize::new(0);

    let started = Instant::now();
    let err = dial_bounded_with(
        "venue.example",
        443,
        bound,
        None,
        &|_: &str, _: u16, _: Duration| Ok(addrs.clone()),
        // A black hole: consume exactly the window handed over, then fail like a real timeout.
        &|_, timeout| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(timeout);
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timed out"))
        },
    )
    .expect_err("every address black-holes");
    let elapsed = started.elapsed();

    assert!(
        elapsed < bound * 2,
        "5 black-holed addresses under a {bound:?} phase bound took {elapsed:?} — the bound is \
             per-attempt, not per-phase"
    );
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "a window spent on black holes: {err}");
    // At least two, not exactly five: a loaded box's oversleep may spend the last share (the exact shares are
    // pinned with no sleep by `each_address_is_handed_an_equal_share_of_what_is_left`).
    assert!(
        attempts.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "an equal share each: the first black hole may not starve the addresses behind it"
    );
}

/// A resolver that answers with NOTHING is its own outcome, not a silent success, spelled
/// `no address for host:port`.
#[test]
fn an_empty_resolution_is_reported_as_such() {
    let err = dial_bounded_with(
        "venue.example",
        443,
        Duration::from_secs(1),
        None,
        &|_: &str, _: u16, _: Duration| Ok(vec![]),
        &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
    )
    .expect_err("no addresses, no dial");
    assert_eq!(err.to_string(), "no address for venue.example:443");
}

/// ctrader's `attempt_share` table, ported with its cap (`CONNECT_TIMEOUT`, 4 s): an equal share of what is left,
/// never more than the cap, and the last address gets all that remains. With no cap the last one takes it all.
#[test]
fn a_connect_attempt_gets_an_equal_share_of_what_is_left() {
    let secs = Duration::from_secs;
    let cap = Some(secs(4));
    assert_eq!(
        attempt_share(secs(5), 2, cap),
        Duration::from_millis(2500),
        "the account-currency ask"
    );
    assert_eq!(attempt_share(secs(5), 1, cap), secs(4), "the last address: the cap, not 5 s");
    assert_eq!(attempt_share(secs(12), 3, cap), secs(4), "a roomy ask: the cap");
    assert_eq!(attempt_share(secs(1), 2, cap), Duration::from_millis(500));
    assert_eq!(attempt_share(Duration::ZERO, 2, cap), Duration::ZERO);
    assert_eq!(attempt_share(secs(5), 0, cap), secs(4), "no addresses left reads as one");
    assert_eq!(
        attempt_share(secs(5), 1, None),
        secs(5),
        "uncapped: the last takes all that is left"
    );
    assert_eq!(attempt_share(secs(5), 2, None), Duration::from_millis(2500));
}

/// The timeouts each connect is HANDED, through the injected steps (no network, no sleep): three addresses that
/// fail fast under 30 s get 10 s, 15 s and 30 s, because a fast failure passes its unused time on; under a 12 s
/// cap, 10 s, 12 s and 12 s. Every address failing is the LAST attempt's kind, so a refusal reads as one.
#[test]
fn each_address_is_handed_an_equal_share_of_what_is_left() {
    let addrs: Vec<SocketAddr> =
        (1..=3u8).map(|i| SocketAddr::from(([192, 0, 2, i], 443))).collect();
    let secs = Duration::from_secs;
    for (cap, expected) in [(None, [10, 15, 30]), (Some(secs(12)), [10, 12, 12])] {
        let handed = std::sync::Mutex::new(Vec::<Duration>::new());
        let err = dial_bounded_with(
            "venue.example",
            443,
            secs(30),
            cap,
            &|_: &str, _: u16, _: Duration| Ok(addrs.clone()),
            &|_, timeout| {
                handed.lock().unwrap().push(timeout);
                Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"))
            },
        )
        .expect_err("every address refuses");
        let handed = handed.lock().unwrap().clone();
        assert_eq!(handed.len(), 3, "every address is tried");
        for (got, want) in handed.iter().zip(expected.map(secs)) {
            // Microseconds pass between attempts; a second of slack still tells the rule from `n x bound`.
            assert!(*got <= want && *got > want - secs(1), "cap {cap:?}: handed {handed:?}");
        }
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused, "{err}");
        assert!(err.to_string().contains("all 3 resolved address(es) failed"), "{err}");
    }
}

/// **A black-holed first address cannot eat the window the working one needs** (ctrader's five-second
/// account-currency ask; the CI box's 2026-11-09 dukascopy false alarm): the first is handed only its share, 2.5 s, and
/// the reachable second address behind it is dialled with the 4 s cap.
#[test]
fn a_black_holed_first_address_leaves_the_rest_for_the_working_one() {
    let (_live, live_address, _dead_address) = live_and_dead();
    let black_hole = SocketAddr::from(([192, 0, 2, 1], 443));
    let handed = std::sync::Mutex::new(Vec::<Duration>::new());
    let tcp = dial_bounded_with(
        "venue.example",
        443,
        Duration::from_secs(5),
        Some(Duration::from_secs(4)),
        &|_: &str, _: u16, _: Duration| Ok(vec![black_hole, live_address]),
        &|addr, timeout| {
            handed.lock().unwrap().push(timeout);
            if *addr == black_hole {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"));
            }
            TcpStream::connect_timeout(addr, timeout)
        },
    )
    .expect("the second address answers");
    assert_eq!(tcp.peer_addr().unwrap(), live_address);
    let handed = handed.lock().unwrap().clone();
    assert!(
        handed[0] <= Duration::from_millis(2500) && handed[0] > Duration::from_millis(1500),
        "the black hole's share: {handed:?}"
    );
    assert_eq!(handed[1], Duration::from_secs(4), "the working address: the cap");
}

/// A window with nothing left (a zero bound, or a zero cap) dials NOTHING and says the window ran out, as
/// `TimedOut`: `TcpStream::connect_timeout` would refuse a zero timeout as a bad argument instead.
#[test]
fn a_spent_window_dials_nothing_and_says_so() {
    let addrs =
        vec![SocketAddr::from(([192, 0, 2, 1], 443)), SocketAddr::from(([192, 0, 2, 2], 443))];
    for (bound, cap) in [(Duration::ZERO, None), (Duration::from_secs(5), Some(Duration::ZERO))] {
        let err = dial_bounded_with(
            "venue.example",
            443,
            bound,
            cap,
            &|_: &str, _: u16, _: Duration| Ok(addrs.clone()),
            &|_, _| panic!("a spent window dials nothing"),
        )
        .expect_err("nothing left to dial with");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(err.to_string().contains("spent after 0 of 2 address(es)"), "{err}");
    }
}

/// **The public dial, end to end on loopback**: a real lookup of a literal, the live listener reached, and a closed
/// port's refusal reported as every address failing, inside the one window.
#[test]
fn the_public_dial_reaches_a_live_port_and_names_a_refused_one() {
    let (_live, live_address, dead_address) = live_and_dead();
    let tcp =
        dial_bounded("127.0.0.1", live_address.port(), Duration::from_secs(5), None).expect("live");
    assert_eq!(tcp.peer_addr().unwrap(), live_address);
    let started = Instant::now();
    let err = dial_bounded("127.0.0.1", dead_address.port(), Duration::from_secs(5), None)
        .expect_err("nothing listens on port 1")
        .to_string();
    assert!(err.starts_with("connect 127.0.0.1:1: all 1 resolved address(es) failed"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "a refusal costs no window");
}

/// The authority split the proxy URL and the WebSocket target share: a name, an IPv4 and a bracketed IPv6 literal,
/// with and without a port, and the two refusals.
#[test]
fn the_authority_split_reads_every_shape_and_refuses_a_bad_port() {
    assert_eq!(split_host_port("venue.example:8443", 1080), Ok(("venue.example".into(), 8443)));
    assert_eq!(split_host_port("venue.example", 1080), Ok(("venue.example".into(), 1080)));
    assert_eq!(split_host_port("<host>:1", 1080), Ok(("<host>".into(), 1)));
    assert_eq!(split_host_port("[::1]:9050", 1080), Ok(("::1".into(), 9050)));
    assert_eq!(split_host_port("[::1]", 1080), Ok(("::1".into(), 1080)));
    assert!(split_host_port("venue.example:99999", 1080).unwrap_err().contains("bad port"));
    assert!(split_host_port("[::1", 1080).unwrap_err().contains("unterminated IPv6"));
}
