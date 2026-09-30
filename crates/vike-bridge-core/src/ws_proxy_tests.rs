use super::*;

#[test]
fn parses_socks5h_with_remote_dns() {
    let p = WsProxy::parse("socks5h://127.0.0.1:1080").unwrap();
    assert_eq!(p.host, "127.0.0.1");
    assert_eq!(p.port, 1080);
    assert!(p.remote_dns, "socks5h MUST resolve remotely — that is what clears the geo-block");
    assert!(p.auth.is_none());
}

#[test]
fn parses_plain_socks5_as_local_dns() {
    let p = WsProxy::parse("socks5://<host>:9050").unwrap();
    assert!(!p.remote_dns);
    assert_eq!((p.host.as_str(), p.port), ("<host>", 9050));
}

#[test]
fn defaults_the_port_to_1080() {
    assert_eq!(WsProxy::parse("socks5h://tunnel.local").unwrap().port, 1080);
}

#[test]
fn tolerates_a_trailing_slash() {
    let p = WsProxy::parse("socks5h://127.0.0.1:1080/").unwrap();
    assert_eq!((p.host.as_str(), p.port), ("127.0.0.1", 1080));
}

#[test]
fn parses_userinfo() {
    let p = WsProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
    assert_eq!(p.auth, Some(("alice".to_string(), "s3cr3t".to_string())));
    assert_eq!(p.host, "127.0.0.1");
}

#[test]
fn parses_ipv6_literal() {
    let p = WsProxy::parse("socks5h://[::1]:1080").unwrap();
    assert_eq!((p.host.as_str(), p.port), ("::1", 1080));
    assert_eq!(WsProxy::parse("socks5h://[::1]").unwrap().port, 1080);
}

#[test]
fn rejects_a_missing_or_unsupported_scheme() {
    assert!(WsProxy::parse("127.0.0.1:1080").is_err());
    assert!(WsProxy::parse("http://127.0.0.1:8080").is_err());
    assert!(WsProxy::parse("socks4://127.0.0.1:1080").is_err());
    assert!(WsProxy::parse("").is_err());
}

#[test]
fn rejects_a_bad_port() {
    assert!(WsProxy::parse("socks5h://127.0.0.1:notaport").is_err());
}

/// **A parse ERROR must not carry the URL's userinfo either.**
///
/// [`WsProxy`]'s `Debug`/`Display` redact, but a REJECTED url never becomes a `WsProxy` — it
/// comes back as a `String`, and `crates/bridges/polymarket/src/egress.rs`'s `ws_proxy_with`
/// logs that string at `error!` under a comment asserting "the message carries the parse
/// error, never the URL's userinfo". Two arms interpolate the RAW input, and both are
/// reachable with credentials in it: a url with no scheme (`user:pass@host:port` — the shape
/// somebody writes when they copy an authority out of another config) and one whose authority
/// has an empty host.
///
/// The assertion is on the credential strings, not on the message format: it fails if the
/// username or the password appears in ANY spelling. The last two rows are already safe and
/// are pinned here so a future rewrite of the error strings cannot open them.
#[test]
fn a_parse_error_never_carries_the_proxy_credentials() {
    for url in [
        "alice:s3cr3t@127.0.0.1:1080",         // no scheme  → the raw-url arm
        "socks5h://alice:s3cr3t@:1080",        // no host    → the other raw-url arm
        "socks4://alice:s3cr3t@<host>:1080", // unsupported scheme (already safe)
        "socks5h://alice:s3cr3t@h:notaport",   // bad port (already safe)
    ] {
        let err = WsProxy::parse(url).unwrap_err();
        assert!(
            !err.contains("s3cr3t"),
            "proxy password leaked into the parse error for {url:?}: {err}"
        );
        assert!(
            !err.contains("alice"),
            "proxy username leaked into the parse error for {url:?}: {err}"
        );
    }
}

/// …and what the redaction KEEPS, which is what makes those errors still actionable: the
/// scheme, host, port and path. A url with no userinfo passes through untouched, so the
/// messages for the overwhelmingly common case are byte-identical to before.
#[test]
fn redact_userinfo_keeps_everything_that_is_not_a_credential() {
    assert_eq!(redact_userinfo("socks5h://127.0.0.1:1080"), "socks5h://127.0.0.1:1080");
    assert_eq!(redact_userinfo("socks5h://tunnel.local/x?y"), "socks5h://tunnel.local/x?y");
    assert_eq!(
        redact_userinfo("socks5h://alice:s3cr3t@127.0.0.1:1080"),
        "socks5h://<redacted>@127.0.0.1:1080"
    );
    // No scheme at all — the arm that cannot lean on `parse` having split anything.
    assert_eq!(redact_userinfo("alice:s3cr3t@127.0.0.1:1080"), "<redacted>@127.0.0.1:1080");
    // A '@' inside the password must not shift the cut (the LAST '@' separates userinfo).
    assert_eq!(
        redact_userinfo("socks5h://alice:p@ss@<host>:1080/x"),
        "socks5h://<redacted>@<host>:1080/x"
    );
    // A '@' in the PATH is not userinfo — the authority ends at the first '/'.
    assert_eq!(redact_userinfo("socks5h://h:1080/a@b"), "socks5h://h:1080/a@b");
}

#[test]
fn debug_redacts_the_password_and_display_omits_credentials() {
    let p = WsProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
    let dbg = format!("{p:?}");
    assert!(!dbg.contains("s3cr3t"), "password leaked into Debug: {dbg}");
    assert!(!dbg.contains("alice"), "username leaked into Debug: {dbg}");
    assert!(dbg.contains("auth=set"));
    assert_eq!(p.to_string(), "socks5h://127.0.0.1:1080");
}

#[test]
fn ws_target_defaults_ports_by_scheme() {
    assert_eq!(
        ws_target("wss://ws.example.com/ws/market").unwrap(),
        ("ws.example.com".into(), 443)
    );
    assert_eq!(ws_target("ws://ws.example.com/x").unwrap(), ("ws.example.com".into(), 80));
    assert_eq!(ws_target("wss://ws.example.com:9443/x").unwrap(), ("ws.example.com".into(), 9443));
    assert!(ws_target("not a url").is_err());
}

/// A listening loopback socket, and a loopback port with certainly nothing on it (bound, its
/// port read, then dropped) — the two ends every dial test below needs, with no network.
///
/// A closed loopback port answers RST immediately on every OS, which is what makes
/// "unreachable address" deterministic here rather than a timing gamble.
fn live_and_dead() -> (std::net::TcpListener, SocketAddr, SocketAddr) {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let live_addr = live.local_addr().unwrap();
    let dead_addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    (live, live_addr, dead_addr)
}

/// **F1 — the resolver cannot outlive the bound.** The bounded arm used to call
/// `to_socket_addrs()` in front of `TcpStream::connect_timeout`: a blocking `getaddrinfo` with
/// no timeout in its API, so a dead resolver hung the "bounded" dial for as long as libc liked
/// and every teardown budget derived from `connect_timeout` was derived from half the operation.
///
/// The mechanism, not the DNS, is what is testable offline — so this drives
/// [`call_with_deadline`] with a call that would block for a minute and asserts the wait ends at
/// the deadline. Restore a `join()` (or any wait on the call itself) and this test hangs until
/// the closure finishes, which is the defect reproduced.
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

/// **F2 — every resolved address is tried, not just the first.** The bounded arm did
/// `to_socket_addrs()?.next()`, while the `tungstenite::connect` arm beside it walks them all.
/// MEASURED from the CI box on 2026-08-08, `stream.binance.com` resolves to 6 addresses and
/// `fstream.binance.com` to 8 — so on the venue this bound was introduced for, one unreachable
/// edge failed a connect the unbounded arm would have completed. That is an availability
/// regression traded for boundedness, and it was neither necessary nor disclosed.
///
/// Deterministic and offline: the first address is a closed loopback port (instant RST), the
/// second a live listener. Reinstate first-only and the dial fails.
#[test]
fn every_resolved_address_is_tried_not_just_the_first() {
    let (_live, live_addr, dead_addr) = live_and_dead();
    let tcp = dial_bounded_with(
        "venue.example",
        443,
        Duration::from_secs(10),
        &|_: &str, _: u16, _: Duration| Ok(vec![dead_addr, live_addr]),
        &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
    )
    .expect("the second resolved address is reachable, so the dial must succeed");
    assert_eq!(
        tcp.peer_addr().unwrap(),
        live_addr,
        "the dial must have fallen through to the reachable address"
    );
}

/// **F1 — resolution is DEBITED from the window, not added to it.** The bound is a ceiling on
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
    let (_live, _live_addr, dead_addr) = live_and_dead();

    let err = dial_bounded_with(
        "venue.example",
        443,
        bound,
        &|_: &str, _: u16, _: Duration| {
            std::thread::sleep(resolve_cost);
            Ok(vec![dead_addr])
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
    assert!(err.contains("venue.example:443"), "unexpected: {err}");
}

/// **F1 + F2 together — the whole phase is bounded HOWEVER MANY addresses resolve.** Trying
/// every address (F2) is only safe because they share one deadline (F1); per-attempt bounds
/// would make the real ceiling `n × bound` for an `n` the venue's DNS picks, and binance's is 6
/// and 8. Five black-holing addresses under a 300 ms window must still cost ~300 ms, not 1.5 s,
/// and the error must say the WINDOW ran out rather than blaming the last address.
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
    assert!(
        err.contains("dial bound"),
        "an exhausted window must report itself, not the last address's error: {err}"
    );
    assert!(
        attempts.load(std::sync::atomic::Ordering::Relaxed) >= 1,
        "the loop must actually attempt something"
    );
}

/// A resolver that answers with NOTHING is its own outcome, not a silent success — the arm this
/// replaced spelled it `no address for host:port` and callers' logs still say so.
#[test]
fn an_empty_resolution_is_reported_as_such() {
    let err = dial_bounded_with(
        "venue.example",
        443,
        Duration::from_secs(1),
        &|_: &str, _: u16, _: Duration| Ok(vec![]),
        &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
    )
    .expect_err("no addresses, no dial");
    assert_eq!(err, "no address for venue.example:443");
}

/// INERTNESS: `connect_ws(url, None, _)` must never touch the proxy machinery — a malformed
/// URL fails in the SAME pre-dial parse the pre-existing bounded arm used, with no socket
/// opened and no proxy consulted.
#[test]
fn no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before() {
    let err = connect_ws("not a url", None, Some(Duration::from_millis(50))).unwrap_err();
    assert!(err.starts_with("bad ws url"), "unexpected: {err}");
}

/// The `Some(proxy)` arm reaches the proxy path (and, without the feature, says so by name)
/// — never a silent fall-through to a direct dial.
#[test]
fn proxy_arm_never_silently_falls_back_to_direct() {
    let p = WsProxy::parse("socks5h://127.0.0.1:1").unwrap();
    // Port 1 is reserved; nothing listens, so the SOCKS connect fails fast on every OS.
    let err = connect_ws("wss://example.invalid/ws", Some(&p), None).unwrap_err();
    #[cfg(feature = "socks-proxy")]
    assert!(err.contains("socks5 connect to example.invalid:443"), "unexpected: {err}");
    #[cfg(not(feature = "socks-proxy"))]
    assert!(err.contains("`socks-proxy` feature"), "unexpected: {err}");
    assert!(!err.contains("s3cr3t"));
}

/// End-to-end proof of the SOCKS5 wire exchange against a hand-rolled one-shot proxy: the
/// greeting is answered no-auth, the CONNECT request is asserted to carry the **domain**
/// (ATYP=3 — i.e. `socks5h` remote DNS, NOT a locally-resolved IP), and a success reply is
/// returned. The subsequent TLS handshake then fails against the dummy server, which is the
/// point: reaching a TLS error proves the tunnel was established.
#[cfg(feature = "socks-proxy")]
#[test]
fn socks5h_sends_the_domain_to_the_proxy() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<(u8, String, u16)>();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut greet = [0u8; 3];
        s.read_exact(&mut greet).unwrap();
        assert_eq!(greet[0], 5, "socks version");
        s.write_all(&[5, 0]).unwrap(); // version 5, no-auth selected
        let mut head = [0u8; 4];
        s.read_exact(&mut head).unwrap();
        assert_eq!(&head[..2], &[5, 1], "CONNECT command");
        let atyp = head[3];
        let mut len = [0u8; 1];
        s.read_exact(&mut len).unwrap();
        let mut dom = vec![0u8; len[0] as usize];
        s.read_exact(&mut dom).unwrap();
        let mut pbuf = [0u8; 2];
        s.read_exact(&mut pbuf).unwrap();
        let tport = u16::from_be_bytes(pbuf);
        // success, bound to 0.0.0.0:0
        s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        tx.send((atyp, String::from_utf8_lossy(&dom).to_string(), tport)).unwrap();
        // Let the client's TLS ClientHello land, then drop — the client sees a TLS failure.
        let mut sink = [0u8; 512];
        let _ = s.read(&mut sink);
    });

    let p = WsProxy::parse(&format!("socks5h://127.0.0.1:{port}")).unwrap();
    let err = connect_ws("wss://ws-subscriptions-clob.polymarket.com/ws/market", Some(&p), None)
        .unwrap_err();
    let (atyp, domain, tport) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    server.join().unwrap();

    assert_eq!(atyp, 3, "ATYP must be DOMAINNAME (3) — socks5h resolves at the proxy");
    assert_eq!(domain, "ws-subscriptions-clob.polymarket.com");
    assert_eq!(tport, 443);
    // The tunnel was established; only the TLS/WS handshake against the dummy failed.
    assert!(err.starts_with("ws handshake via socks5h://127.0.0.1:"), "unexpected: {err}");
}
