//! Session lifecycle: no-I/O subscribe, a mid-stream Bye, convergence, teardown and the fake's floor.

use super::*;

// ── 6. unsubscribe does no I/O ───────────────────────────────────────────────────────────────────

/// **`unsubscribe` returns without touching the network**, and neither does `subscribe_*`: the
/// frame thread only ever mutates the desired set and pokes a `Condvar`. Proven by TIME, because the
/// reconciler is free to start dialling the moment the first call pokes it — what this pins is that
/// no call WAITS for that, which is the property a frozen GUI would violate. (Against a session with
/// no address the old spelling could prove it by the ABSENCE of a connection; a mounted feed means
/// an address and a probe, so that proof is gone and `an_empty_desired_set_opens_no_stream` carries
/// the half of it that still holds.)
#[test]
fn subscribe_and_unsubscribe_do_no_io_at_all() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.attach(&fake);
    let started = Instant::now();
    let id = h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    h.feed("binance").unsubscribe(id);
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "a frame-thread call took {:?} — that is a frozen GUI",
        started.elapsed()
    );
}

/// **An empty desired set opens no STREAM, even with the advertisement known and an address set**:
/// opening a connection to say "nothing, thanks" spends one of the server's stream slots for no
/// reason. The probe that learned the advertisement is the one connection there should be, and it
/// sent nothing after the handshake.
#[test]
fn an_empty_desired_set_opens_no_stream() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.attach(&fake);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        1,
        "an empty desired set must open no connection beyond the probe"
    );
    assert_eq!(
        fake.seen.recv_timeout(SETTLE).expect("the fake reports what the probe sent"),
        vec!["Hello".to_string()],
        "the probe sent a frame after the handshake"
    );
}

// ── 8. a mid-stream Bye ──────────────────────────────────────────────────────────────────────────

/// **A `Bye` ends the reader and the session re-dials** rather than wedging. The book held on the
/// dead connection is FORGOTTEN on the way out (§7.3 rule 4's opening bracket, synthesized locally
/// because the server that would have sent it is the one that went away) — never resumed across a
/// socket.
#[test]
fn a_mid_stream_bye_drops_the_books_and_the_session_redials() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1_700_000_000_000),
                    MdFrame::Bye(MdBye::TooSlow { lapses: 9 }),
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the book to arrive", || h.books.get("binance", "BTCUSDT").is_some());
    until("the Bye to drop it again", || h.books.get("binance", "BTCUSDT").is_none());
    // ⚠ **WAITED FOR, not asserted straight after the `until` above — and the difference is a CI
    // flake this test carried.** Every other `until`/`assert!` pair in this file waits on the same
    // observable it then asserts about. This one did not: the `until` returns the instant the BOOK
    // is gone, and the STATUS transition is a separate observation that lands a beat later, so the
    // assertion was measuring how busy the box was rather than what the session did.
    //
    // It failed on `fix/bug-b-reaches-the-backfill-bins` (2026-09-18) — a branch touching only
    // `vike-backfill`, `vike-backtest` and `vike-datahub`, nothing this test can see — under seven
    // concurrent CI runs, and reddened the whole merge gate for it. `main` was green across the
    // same window.
    //
    // The property is unchanged, and the timeout still fails a session that never transitions:
    // `until` panics at `SETTLE` exactly as an assertion would, so this is not a weakening.
    until("the status to SAY the link went away rather than show a silently empty ladder", || {
        h.status("binance").contains("reconnecting")
    });
}

// ── 9. the reconciler CONVERGES against a clamping server ────────────────────────────────────────

/// **THE HOT-LOOP KILL PROOF.** A server that CLAMPS the depth must still leave the desired set and
/// the served set comparing EQUAL, or the reconciler re-sends `MdUpdate` for ever.
///
/// This is the sharpest footgun in the client half and it is invisible to every other test here,
/// because the obvious fake — `ScriptedAnswer::accept_all`, which echoes the request verbatim —
/// makes the two sets equal by ANY comparison. The real server does not echo: `MdSpec::depth_levels`
/// is `None` on the wire out (the client asks for the server's own default rather than inflating
/// every subscriber's frame) and `MdSubscribedInfo::accepted` comes back carrying the server's
/// AUTHORITATIVE spec with the depth RESOLVED — `Some(MD_DEPTH_LEVELS_DEFAULT)`. `MdSpec` derives
/// `Eq`/`Hash` over all four fields, so a set diff written the natural way differs on every key for
/// ever and hammers the server with one dial per pass, at the backoff floor, with nothing visibly
/// broken at either end. `MdSpec::key()` — which deliberately omits the depth — is the answer, and
/// this test is what makes removing it fail.
///
/// The proof is the CONNECTION COUNT. The stream socket goes write-only at `MdSubscribe`, so every
/// `MdUpdate` must dial its own short-lived connection: the probe plus the stream are TWO accepts
/// and a converged session stays there, while a diverging one climbs without bound.
#[test]
fn a_clamping_server_converges_and_does_not_hot_loop_md_update() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            // Exactly what the real server does: resolve `None` to its own default and echo the
            // RESOLVED spec back as authoritative.
            let clamped: Vec<MdSpec> = specs
                .iter()
                .map(|s| MdSpec { depth_levels: Some(s.resolved_depth()), ..s.clone() })
                .collect();
            ScriptedAnswer { accepted: clamped, refused: Vec::new(), push: Vec::new() }
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the stream to be accepted", || fake.accepted.load(Ordering::SeqCst) >= 2);
    // Long enough for many passes of a diverging reconciler: `retry_backoff`'s floor is 1 s, and a
    // set that never converges is re-diffed on every poke as well as on every park expiry.
    thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        2,
        "the reconciler kept re-sending MdUpdate against a server that had already accepted the \
         key — the desired/served diff is comparing the CLAMPED depth instead of MdSpec::key()"
    );
}

// ── teardown ─────────────────────────────────────────────────────────────────────────────────────

/// ⚠ **`shutdown` is called ONCE PER VENUE, in PARALLEL, on ONE session** —
/// `App::run_bounded_teardown` fans it over every `App::feeds` entry at once. Unguarded, five of
/// the six would panic on a double `JoinHandle::join`. This drives the real fan-out shape and
/// requires it to complete well inside that teardown's 1500 ms budget.
#[test]
fn a_parallel_shutdown_across_every_feed_is_safe_and_fast() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    until("the stream to be live", || h.books.get("binance", "BTCUSDT").is_some());

    let started = Instant::now();
    let handles: Vec<_> = std::mem::take(&mut h.mount.feeds)
        .into_values()
        .map(|mut f| thread::spawn(move || f.shutdown()))
        .collect();
    for handle in handles {
        handle.join().expect("a concurrent shutdown panicked — the double join is unguarded");
    }
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "teardown took {:?}, past App::run_bounded_teardown's whole budget — the reader is being \
         waited out at its 45s read deadline instead of woken by TcpStream::shutdown",
        started.elapsed()
    );
}

/// **With NO feed mounted the session is still stoppable.** The feeds are mounted from the datahub's
/// handshake, so before it lands (or against a server that advertises nothing) `App::feeds` is
/// empty and no `DataClient::shutdown` exists to stop the reconciler; `App::run_bounded_teardown`
/// therefore stops the session itself, and it must do so in time and a second time over (the feeds'
/// own `shutdown`s are the same call, in parallel).
#[test]
fn a_session_with_no_feed_stops_on_its_own() {
    let fake = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    assert!(h.mount.feeds.is_empty(), "a plane-less server mounts no feed");

    let started = Instant::now();
    let session = h.mount.session.clone();
    let twice: Vec<_> = (0..2)
        .map(|_| {
            let s = session.clone();
            thread::spawn(move || s.stop_and_join())
        })
        .collect();
    for t in twice {
        t.join().expect("a concurrent stop panicked");
    }
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "stopping a feed-less session took {:?}",
        started.elapsed()
    );
}

/// The fake's own floor: it really does speak the protocol, so a green above is not a green over a
/// server that answered nothing. Without this, every "nothing was sent" assertion could pass
/// against a listener that never completed a handshake.
#[test]
fn the_fake_datahub_really_serves_the_handshake() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut stream = TcpStream::connect(fake.addr).expect("connect to the fake");
    stream.set_read_timeout(Some(SETTLE)).expect("arm a deadline");
    write_frame(&mut stream, &Request::Hello { proto_version: PROTO_VERSION }).expect("send Hello");
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Welcome { features, .. }) => {
            assert!(features.iter().any(|f| f == FEATURE_MARKET_DATA))
        }
        other => panic!("the fake did not answer a Welcome: {other:?}"),
    }
    // A verb it does not serve is a clean `Response::Error` on a LIVE connection — leg (3) of the
    // capability contract, and the reason `md_subscribe` hands the client back intact.
    write_frame(&mut stream, &Request::ListSeries).expect("send an unserved verb");
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Error(_)) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            panic!("the fake stopped answering — the connection was not kept positional")
        }
        other => panic!("expected a clean Error, got {other:?}"),
    }
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
}

/// The unused-field guard: `Harness::wakes` and `Fake::seen` are read by the tests above, but a
/// future edit that stops reading one would leave a dead field rather than a failure. Named here so
/// the omission is loud. A fresh session mounts NOTHING — the feed set is the server's word, and no
/// server has spoken.
#[test]
fn the_harness_reports_what_the_tests_assert_on() {
    let h = Harness::new();
    assert_eq!(h.wakes.load(Ordering::SeqCst), 0, "a fresh session wakes nobody");
    assert!(h.mount.feeds.is_empty(), "no server has advertised anything, so no feed exists");
    assert!(h.mount.feed_statuses.is_empty(), "...and no status line either");
}
