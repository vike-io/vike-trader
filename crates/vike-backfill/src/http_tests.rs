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
/// ⚠ This test is NOT the gate against a body ceiling;
/// [`no_ceiling_reaches_the_headers_or_the_body`]'s field pin is (module doc): a ceiling longer
/// than this ~5 s body passes here and still truncates a large part. The late 2 s gap is the one
/// long pause a healthy body may take, a full second under the idle bound. The elapsed-time
/// assertion is what makes the pass MEAN something — a server that finished inside the bounds
/// would pass the read for the wrong reason.
#[test]
fn a_body_streaming_longer_than_both_bounds_completes_when_no_gap_exceeds_the_idle_bound() {
    let connect = Duration::from_millis(300);
    let idle = Duration::from_secs(3);
    let gap = Duration::from_millis(50);
    let bytes = 60;
    // A real pause, a full second under `idle` (so a scheduler stall on a loaded runner does not
    // fire the bound early), and late enough that a short ceiling is already past when it lands.
    let late_gap = Duration::from_secs(2);
    let (url, server) = dribbling_server(bytes, gap, Some((bytes - 10, late_gap)));

    let started = Instant::now();
    let mut response = build_agent(connect, idle).get(&url).call().expect("headers arrive");
    let mut body = Vec::new();
    response.body_mut().as_reader().read_to_end(&mut body).expect(
        "a slow but PROGRESSING body must complete — neither bound is a body ceiling.\n\
                 ⚠ IF THIS FAILED AT ~3s, READ THIS BEFORE CALLING IT A FLAKE: that is the idle \
                 bound firing as a TOTAL body ceiling, i.e. the agent arms `timeout_recv_body` \
                 where it needs `timeout_per_read`. Since ureq 3.4.1 (algesten/ureq#1194) \
                 `recv_body` is a total budget: 3.4.1 failed this at 3.01s, three for three. \
                 A loaded-runner stall looks different — it varies. The root Cargo.toml's ureq \
                 note carries the rest.",
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
/// read with `ureq`'s `timeout: per read` — promptly, from the bound, not from the server
/// eventually resuming — rather than parking the collector on a peer that went silent with the
/// socket open. The bytes delivered before the stall are already in the caller's buffer, which
/// is what lets [`get_to_file`] leave a legibly truncated file rather than nothing.
#[test]
fn one_gap_past_the_idle_bound_fails_per_read_rather_than_hanging() {
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

    let mut response =
        build_agent(Duration::from_secs(5), idle).get(&url).call().expect("headers arrive");
    let mut body = Vec::new();
    let started = Instant::now();
    let err = response
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .expect_err("a body that stalls past the idle bound must FAIL, not hang");
    let elapsed = started.elapsed();

    assert!(err.to_string().contains("timeout: per read"), "the idle bound fired: {err}");
    assert!(
        elapsed < stall,
        "failed by the bound ({idle:?}), not by the server resuming after {stall:?}: {elapsed:?}"
    );
    assert_eq!(body.len(), before_stall, "the bytes before the stall were delivered");
}

/// **A vendor that takes the request and never answers fails after the idle bound**, not after the
/// TCP stack gives up: ureq 3.5 caps every transport read at `timeout_per_read`, the one that waits
/// for the response head included (the module doc's time-to-first-byte claim). Scaled down through
/// [`build_agent`] to a 500 ms idle bound.
///
/// MUTATION PROOF: pass `CallBound::Total` in [`build_agent`] in place of `CallBound::Idle` — the call
/// then fails on `Timeout::Global`, not `PerRead`.
#[test]
fn a_vendor_that_never_answers_fails_per_read_rather_than_hanging() {
    let idle = Duration::from_millis(500);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/part", listener.local_addr().expect("local_addr"));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut request = [0u8; 2048];
        let _ = sock.read(&mut request); // the request arrives...
        let _ = held.recv(); // ...and no answer ever does
    });
    let started = Instant::now();
    let outcome = build_agent(Duration::from_secs(5), idle).get(&url).call();
    let waited = started.elapsed();
    drop(release);
    server.join().expect("the silent vendor exits once released");
    let err = outcome.err();
    assert!(matches!(err, Some(ureq::Error::Timeout(ureq::Timeout::PerRead))), "{err:?}");
    assert!(
        waited >= idle && waited < idle * 6,
        "gave up after {waited:?} against a {idle:?} idle bound"
    );
}

/// The absences, as field pins — an absence cannot be read from the code, so each ceiling that
/// would reach the headers or the body is asserted unarmed here and fails the moment somebody
/// "fixes" the missing timeout. The module doc carries the argument per knob; the messages
/// carry the one-line version.
///
/// The phase budgets before the body (`send_request`, `await_100`, `send_body`, `recv_response`)
/// are deliberately NOT asserted: in ureq 3.5 a phase budget stops when its phase ends, so none
/// can reach the body (`ureq` defaults `await_100` to one second, harmlessly).
#[test]
fn no_ceiling_reaches_the_headers_or_the_body() {
    let timeouts = collector_agent().config().timeouts();
    assert_eq!(timeouts.global, None, "a global ceiling would abort a large healthy download");
    assert_eq!(timeouts.per_call, None, "per_call is a whole-call ceiling by another name");
    assert_eq!(
        timeouts.recv_body, None,
        "recv_body is a TOTAL body budget since ureq 3.4.1 — a whole-body ceiling wearing an \
             idle-sounding name (module doc); the idle bound is timeout_per_read"
    );
    assert_eq!(timeouts.resolve, None, "name resolution is a DECLARED residual, not a bound");
}

/// ...and the two that ARE armed, by value: the production constants reach the builder
/// unchanged. The behavioural tests above prove what the two knobs MEAN; this proves which
/// numbers the collectors actually run with.
#[test]
fn the_connect_and_the_body_idle_bounds_are_armed() {
    let agent = collector_agent();
    assert_eq!(agent.config().timeouts().connect, Some(CONNECT_TIMEOUT));
    assert_eq!(agent.config().timeout_per_read(), Some(BODY_IDLE_TIMEOUT));
}

/// A loopback server that answers `/hop/<n>` with a 302 to `/hop/<n+1>` until `/hop/<end>`, which it
/// answers with a 200 `ok`; counts the requests it served. `end` redirects make a chain of `end`.
fn hop_server(end: usize) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local_addr"));
    let served = Arc::new(AtomicUsize::new(0));
    let (count, prefix) = (Arc::clone(&served), base.clone());
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut sock) = conn else { continue };
            let mut buffer = [0u8; 4096];
            let n = sock.read(&mut buffer).unwrap_or(0);
            let head = String::from_utf8_lossy(&buffer[..n]).into_owned();
            count.fetch_add(1, Ordering::SeqCst);
            let hop: usize = head
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.strip_prefix("/hop/"))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let reply = if hop >= end {
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string()
            } else {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {prefix}/hop/{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    hop + 1
                )
            };
            let _ = sock.write_all(reply.as_bytes());
        }
    });
    (base, served)
}

/// A loopback server that answers every request with `status` over a kept-alive socket, recording
/// each request head and counting the connections it accepted.
fn recording_server(
    status: u16,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local_addr"));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (seen, count) = (Arc::clone(&heads), Arc::clone(&accepted));
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut sock) = conn else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            let seen = Arc::clone(&seen);
            thread::spawn(move || {
                loop {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match sock.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    seen.lock().unwrap().push(String::from_utf8_lossy(&head).to_lowercase());
                    let reply = format!("HTTP/1.1 {status} X\r\nContent-Length: 4\r\n\r\nbody");
                    if sock.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (base, heads, accepted)
}

/// The caller's agent is the point of [`get_to_string_on`]: a client paging one host keeps its one
/// connection, and its `X-API-KEY` reaches the wire through [`GetOptions::headers`].
#[test]
fn a_callers_agent_keeps_its_connection_and_carries_its_header() {
    use std::sync::atomic::Ordering;
    let (base, heads, accepted) = recording_server(200);
    let agent = collector_agent();
    let opts = GetOptions { headers: &[("X-API-Key", "the-key")], ..GetOptions::default() };
    for _ in 0..2 {
        let body = get_to_string_on(&agent, &format!("{base}/x"), &opts, "test").expect("served");
        assert_eq!(body, "body");
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "both requests rode one pooled connection");
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 2);
    assert!(heads.iter().all(|h| h.contains("x-api-key: the-key")), "{heads:?}");
}

/// The Yahoo chart GET names this client in its `User-Agent`, never a browser (the owner's
/// honest-client rule): the collector agent's, since the request sets none, and only one. The
/// loopback's `body` is no chart, so the fetch fails after the request.
#[test]
fn the_yahoo_chart_request_sends_this_clients_own_user_agent() {
    use crate::eod::EodSource;
    let (base, heads, _) = recording_server(200);
    assert!(crate::eod::yahoo::YahooSource.fetch(&base, "SPY", 1).is_err());
    let heads = heads.lock().unwrap();
    let user_agent = format!("user-agent: {}\r\n", vike_transport::http::USER_AGENT);
    assert!(heads[0].contains(&user_agent), "{heads:?}");
    assert_eq!(heads[0].matches("user-agent:").count(), 1, "{heads:?}");
    assert!(!heads[0].contains("mozilla"), "{heads:?}");
}

/// Yahoo's `404` for a ticker it does not list is a VARIANT the datahub's share-class retry decides on (decision
/// 0301), in the words every refused GET says; any other status is a plain failure.
#[test]
fn a_yahoo_404_is_an_unlisted_ticker_and_any_other_status_a_failure() {
    use crate::eod::yahoo::{YahooSplitsError, fetch_splits};
    let (base, _, _) = recording_server(404);
    let unlisted = fetch_splits(&base, "BRKB").expect_err("a 404");
    assert!(matches!(unlisted, YahooSplitsError::Unlisted(_)), "{unlisted:?}");
    assert_eq!(
        unlisted.to_string(),
        format!(
            "venue fetch: eod HTTP 404 from {base}/v8/finance/chart/BRKB?interval=3mo&range=max&events=split: body"
        )
    );
    let (base, _, _) = recording_server(500);
    let failed = fetch_splits(&base, "BRKB").expect_err("a 500");
    assert!(matches!(failed, YahooSplitsError::Failed(_)), "{failed:?}");
}

/// The two pooled clients through the one skeleton: the header is on the wire and a refused GET says
/// the skeleton's sentence, under each client's own prefix.
#[cfg(feature = "vike-archive")]
#[test]
fn the_pooled_clients_send_their_key_and_say_the_one_refusal() {
    let (base, heads, _) = recording_server(401);
    let archive = crate::vike_archive::client::ArchiveClient::new(base.clone(), Some("k1".into()));
    let e = archive.fetch_manifest().expect_err("refused").to_string();
    assert!(e.contains("vike-archive HTTP 401 from ") && e.ends_with(": body"), "{e}");
    let events = crate::events_api::EventsClient::new(base, Some("k2".to_string()));
    let e = events.fetch_all_single("TOK", 0, 1, 1).expect_err("refused").to_string();
    assert!(e.contains("events-api HTTP 401 from ") && e.ends_with(": body"), "{e}");
    let heads = heads.lock().unwrap();
    assert!(heads[0].contains("x-api-key: k1") && heads[1].contains("x-api-key: k2"), "{heads:?}");
}

/// A client with no key sends no header, rather than an empty one.
#[cfg(feature = "vike-archive")]
#[test]
fn a_pooled_client_with_no_key_sends_no_key_header() {
    let (base, heads, _) = recording_server(401);
    let archive = crate::vike_archive::client::ArchiveClient::new(base, None);
    assert!(archive.fetch_manifest().is_err());
    assert!(!heads.lock().unwrap()[0].contains("x-api-key"));
}

/// **The redirect limit is stated, not ureq's default** (A50). A collector GET that moves behind
/// five redirects is still read (six requests); a chain of six fails with ureq's `too many
/// redirects` having asked exactly six times (the default of ten would ask eleven and succeed). The
/// builder's stated figure is read back from the agent too, so the constant and the behaviour cannot
/// drift apart.
#[test]
fn the_redirect_limit_is_stated_and_a_longer_chain_fails() {
    use std::sync::atomic::Ordering;
    let stated = vike_transport::http::PUBLIC_GET_MAX_REDIRECTS;
    assert_eq!(collector_agent().config().max_redirects(), stated);
    assert_eq!(stated, 5, "the figure the collectors have always followed");

    let (base, served) = hop_server(5);
    let body = get_to_string(&format!("{base}/hop/0"), &GetOptions::default(), "test")
        .expect("five redirects are followed");
    assert_eq!(body, "ok");
    assert_eq!(served.load(Ordering::SeqCst), 6);

    let (base, served) = hop_server(6);
    let err = get_to_string(&format!("{base}/hop/0"), &GetOptions::default(), "test")
        .expect_err("a sixth redirect is refused");
    assert!(err.to_string().contains("too many redirects"), "{err}");
    assert_eq!(
        served.load(Ordering::SeqCst),
        6,
        "the request that would follow the sixth was never sent"
    );
}

/// A loopback server answering its one request with a 302 to `location`.
fn redirect_once(location: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local_addr"));
    thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else { return };
        let _ = sock.read(&mut [0u8; 4096]);
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = sock.write_all(reply.as_bytes());
    });
    base
}

/// **A keyed GET follows no redirect, so its key never reaches another host**: ureq replays a custom header to any
/// `Location`. The 302 is the failure, and the host it named saw nothing. A keyless GET still follows
/// (`the_redirect_limit_is_stated_and_a_longer_chain_fails`).
#[test]
fn a_keyed_get_follows_no_redirect_and_its_key_never_reaches_another_host() {
    let (target, heads, _) = recording_server(200);
    let origin = redirect_once(format!("{target}/stolen"));
    let opts = GetOptions { headers: &[("X-API-Key", "the-key")], ..GetOptions::default() };
    let err = get_to_string(&format!("{origin}/x"), &opts, "test").expect_err("a 302 is refused");
    assert!(err.to_string().contains("test HTTP 302 from "), "{err}");
    assert!(
        heads.lock().unwrap().is_empty(),
        "the Location was asked: {:?}",
        heads.lock().unwrap()
    );
}

/// The status policy the whole module depends on: 4xx/5xx must come back as RESPONSES, because
/// every body sink here reads the status itself (a 404 is a "not published" skip, not an error).
/// Bundled with the timeout pins because they share one builder — a rewrite that arms a timeout
/// while dropping this would turn every 404 skip into a hard `CollectError::Fetch`.
#[test]
fn statuses_are_returned_as_responses_not_errors() {
    assert!(!collector_agent().config().http_status_as_error());
}
