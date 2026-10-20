use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::*;

/// A one-shot loopback HTTP/1.1 server that answers the first request with a `200` carrying a
/// `Content-Length` of `bytes`, then DRIBBLES the body one byte at a time with `gap` between
/// bytes — and, when `stall` is given, one gap of that length before byte `stall.0` instead.
///
/// The request head is drained byte-wise and never parsed: the test is about the BODY clock,
/// and `ureq` puts the whole GET on the wire in one write. A write failure ends the thread
/// quietly — in the stall test the client has already given up on the socket, which is the
/// point. Returns the URL to fetch and the server thread's handle.
fn dribbling_server(
    bytes: usize,
    gap: Duration,
    stall: Option<(usize, Duration)>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/part", listener.local_addr().expect("local_addr"));
    let handle = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        // Bounded so a test that never sends a request cannot park this thread forever.
        sock.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match sock.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return,
            }
        }
        let headers =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n");
        if sock.write_all(headers.as_bytes()).is_err() {
            return;
        }
        for i in 0..bytes {
            let pause = match stall {
                Some((at, long)) if at == i => long,
                _ => gap,
            };
            thread::sleep(pause);
            if sock.write_all(b"x").is_err() {
                return;
            }
        }
    });
    (url, handle)
}

/// The ILLUSTRATION of the idle semantics, as behaviour: a body that streams for longer than
/// BOTH armed bounds completes, because neither is a ceiling on the body — [`BODY_IDLE_TIMEOUT`]
/// is per read and [`CONNECT_TIMEOUT`] never reaches the body phase (module doc). Scaled down
/// through [`build_agent`]: a 300 ms connect bound and a 3 s idle bound against a body that
/// takes ~5 s to arrive — a 50 ms cadence with ONE 2 s gap late in the body.
///
/// ⚠ This test is NOT the gate against the `timeout_recv_response` regression the module doc
/// describes; [`no_ceiling_reaches_the_headers_or_the_body`]'s field pin is. Measured by
/// mutation: with a 500 ms `timeout_recv_response` planted in [`build_agent`] the first version
/// of this test — a smooth 50 ms dribble — still PASSED, because past an inherited wall `ureq`
/// degrades the remainder to a 1 s socket read timeout (`NextTimeout::not_zero`) and a steady
/// dribble never leaves a 1 s hole for it to fire in. The 2 s gap is what lets this test SEE
/// that class at all (a wall past by then trips on it; the 3 s idle bound does not), but it
/// catches a wall only when the gap outruns the degraded timeout — a field pin catches a wall
/// of ANY value. The elapsed-time assertion is what makes the pass MEAN something — a server
/// that finished inside the bounds would pass the read for the wrong reason.
#[test]
fn a_body_streaming_longer_than_both_bounds_completes_when_no_gap_exceeds_the_idle_bound() {
    let connect = Duration::from_millis(300);
    let idle = Duration::from_secs(3);
    let gap = Duration::from_millis(50);
    let bytes = 60;
    // Over `ureq`'s 1 s degraded read timeout (so an inherited wall would trip on it), a full
    // second under `idle` (so a scheduler stall on a loaded runner does not fire the bound
    // early), and late enough that a short wall is already past when it lands.
    let late_gap = Duration::from_secs(2);
    let (url, server) = dribbling_server(bytes, gap, Some((bytes - 10, late_gap)));

    let started = Instant::now();
    let mut resp = build_agent(connect, idle).get(&url).call().expect("headers arrive");
    let mut body = Vec::new();
    resp.body_mut().as_reader().read_to_end(&mut body).expect(
        "a slow but PROGRESSING body must complete — neither bound is a body ceiling.\n\
                 ⚠ IF THIS FAILED AT ~3s, READ THIS BEFORE CALLING IT A FLAKE: that is the idle \
                 bound itself firing, and the likeliest cause is a ureq BUMP. 3.4.1 \
                 (algesten/ureq#1194) stopped the active phase's deadline restarting on every \
                 lookup — and that restart WAS this module's per-read semantics, so \
                 `timeout_recv_body` becomes a TOTAL body ceiling and a 5s body dies on a 3s \
                 bound. MEASURED: 3.4.0 passes at ~5.0s, 3.4.1 fails at 3.01s, three for three. \
                 A loaded-runner stall looks different — it varies. The root Cargo.toml's ureq \
                 pin carries the rest.",
    );
    let elapsed = started.elapsed();

    assert_eq!(body.len(), bytes, "the whole body arrived");
    assert!(
        elapsed > idle && elapsed > connect,
        "the body must have streamed for LONGER than both bounds for this pass to prove \
             anything: took {elapsed:?} against idle {idle:?} / connect {connect:?}"
    );
    server.join().expect("server thread");
}

/// ...and the other half of the idle semantics: ONE gap past [`BODY_IDLE_TIMEOUT`] fails the
/// read with `ureq`'s `timeout: receive body` — promptly, from the bound, not from the server
/// eventually resuming — rather than parking the collector on a peer that went silent with the
/// socket open. The bytes delivered before the stall are already in the caller's buffer, which
/// is what lets [`get_to_file`] leave a legibly truncated file rather than nothing.
#[test]
fn one_gap_past_the_idle_bound_fails_with_receive_body_rather_than_hanging() {
    // A full second, not the few hundred milliseconds the assertions strictly need: the server
    // dribbles at 10 ms, and a scheduler stall longer than `idle` between two dribbled bytes on
    // a loaded runner would fire the bound EARLY and fail `body.len() == before_stall`. Every
    // assertion below holds unchanged at this value (`elapsed < stall` has 4 s of room).
    let idle = Duration::from_secs(1);
    let before_stall = 10;
    let stall = Duration::from_secs(5);
    // The server thread is deliberately not joined: it sleeps out the stall, finds the socket
    // gone and returns on its own, and nextest runs each test in its own process anyway.
    let (url, _server) =
        dribbling_server(20, Duration::from_millis(10), Some((before_stall, stall)));

    let mut resp =
        build_agent(Duration::from_secs(5), idle).get(&url).call().expect("headers arrive");
    let mut body = Vec::new();
    let started = Instant::now();
    let err = resp
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .expect_err("a body that stalls past the idle bound must FAIL, not hang");
    let elapsed = started.elapsed();

    assert!(err.to_string().contains("timeout: receive body"), "the idle bound fired: {err}");
    assert!(
        elapsed < stall,
        "failed by the bound ({idle:?}), not by the server resuming after {stall:?}: {elapsed:?}"
    );
    assert_eq!(body.len(), before_stall, "the bytes before the stall were delivered");
}

/// The absences, as field pins — an absence cannot be read from the code, so each ceiling that
/// would reach the headers or the body is asserted unarmed here and fails the moment somebody
/// "fixes" the missing timeout. The module doc carries the argument per knob; the messages
/// carry the one-line version.
///
/// `await_100` and `send_body` are deliberately NOT asserted: `ureq` defaults `await_100` to
/// one second, both belong to request-BODY phases these bodyless GETs never enter, and neither
/// sits in `RecvResponse`'s or `RecvBody`'s preceding set, so neither can reach a response.
#[test]
fn no_ceiling_reaches_the_headers_or_the_body() {
    let timeouts = collector_agent().config().timeouts();
    assert_eq!(timeouts.global, None, "a global ceiling would abort a large healthy download");
    assert_eq!(timeouts.per_call, None, "per_call is a whole-call ceiling by another name");
    assert_eq!(
        timeouts.recv_response, None,
        "recv_response is inherited by EVERY body read as an absolute wall headers_time + X \
             (module doc) — a whole-body ceiling wearing a headers-only name"
    );
    assert_eq!(
        timeouts.send_request, None,
        "send_request would bound time-to-first-byte only through an undocumented `preceeding` \
             quirk that a future ureq could move onto the body — declared unarmed, see module doc"
    );
    assert_eq!(timeouts.resolve, None, "name resolution is a DECLARED residual, not a bound");
}

/// ...and the two that ARE armed, by value: the production constants reach the builder
/// unchanged. The behavioural tests above prove what the two knobs MEAN; this proves which
/// numbers the collectors actually run with.
#[test]
fn the_connect_and_the_body_idle_bounds_are_armed() {
    let timeouts = collector_agent().config().timeouts();
    assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));
    assert_eq!(timeouts.recv_body, Some(BODY_IDLE_TIMEOUT));
}

/// The status policy the whole module depends on: 4xx/5xx must come back as RESPONSES, because
/// every body sink here reads the status itself (a 404 is a "not published" skip, not an error).
/// Bundled with the timeout pins because they share one builder — a rewrite that arms a timeout
/// while dropping this would turn every 404 skip into a hard `CollectError::Fetch`.
#[test]
fn statuses_are_returned_as_responses_not_errors() {
    assert!(!collector_agent().config().http_status_as_error());
}
