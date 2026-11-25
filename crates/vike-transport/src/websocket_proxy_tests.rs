use super::*;

#[test]
fn parses_socks5h_with_remote_dns() {
    let p = WebsocketProxy::parse("socks5h://127.0.0.1:1080").unwrap();
    assert_eq!(p.host, "127.0.0.1");
    assert_eq!(p.port, 1080);
    assert!(p.remote_dns, "socks5h MUST resolve remotely — that is what clears the geo-block");
    assert!(p.auth.is_none());
}

#[test]
fn parses_plain_socks5_as_local_dns() {
    let p = WebsocketProxy::parse("socks5://<host>:9050").unwrap();
    assert!(!p.remote_dns);
    assert_eq!((p.host.as_str(), p.port), ("<host>", 9050));
}

#[test]
fn defaults_the_port_to_1080() {
    assert_eq!(WebsocketProxy::parse("socks5h://tunnel.local").unwrap().port, 1080);
}

#[test]
fn tolerates_a_trailing_slash() {
    let p = WebsocketProxy::parse("socks5h://127.0.0.1:1080/").unwrap();
    assert_eq!((p.host.as_str(), p.port), ("127.0.0.1", 1080));
}

#[test]
fn parses_userinfo() {
    let p = WebsocketProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
    assert_eq!(p.auth, Some(("alice".to_string(), "s3cr3t".to_string())));
    assert_eq!(p.host, "127.0.0.1");
}

#[test]
fn parses_ipv6_literal() {
    let p = WebsocketProxy::parse("socks5h://[::1]:1080").unwrap();
    assert_eq!((p.host.as_str(), p.port), ("::1", 1080));
    assert_eq!(WebsocketProxy::parse("socks5h://[::1]").unwrap().port, 1080);
}

#[test]
fn rejects_a_missing_or_unsupported_scheme() {
    assert!(WebsocketProxy::parse("127.0.0.1:1080").is_err());
    assert!(WebsocketProxy::parse("http://127.0.0.1:8080").is_err());
    assert!(WebsocketProxy::parse("socks4://127.0.0.1:1080").is_err());
    assert!(WebsocketProxy::parse("").is_err());
}

#[test]
fn rejects_a_bad_port() {
    assert!(WebsocketProxy::parse("socks5h://127.0.0.1:notaport").is_err());
}

/// **A parse ERROR must not carry the URL's userinfo either.**
///
/// [`WebsocketProxy`]'s `Debug`/`Display` redact, but a REJECTED url never becomes a `WebsocketProxy` — it
/// comes back as a `String`, which `crates/bridges/polymarket/src/transport/mod.rs`'s `websocket_proxy_with`
/// logs at `error!`. Two arms interpolate the input, both reachable with credentials in it: no
/// scheme (`user:pass@host:port`, an authority copied out of another config) and an empty host.
///
/// Asserted on the credential strings, not the message format: it fails if the username or the
/// password appears in ANY spelling. The last two rows pin arms that echo no input, so a rewrite
/// of their error strings cannot open them.
#[test]
fn a_parse_error_never_carries_the_proxy_credentials() {
    for url in [
        "alice:s3cr3t@127.0.0.1:1080",         // no scheme  → the raw-url arm
        "socks5h://alice:s3cr3t@:1080",        // no host    → the other raw-url arm
        "socks4://alice:s3cr3t@<host>:1080", // unsupported scheme (already safe)
        "socks5h://alice:s3cr3t@h:notaport",   // bad port (already safe)
    ] {
        let err = WebsocketProxy::parse(url).unwrap_err();
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
/// scheme, host, port and path. A url with no userinfo passes through untouched.
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
    let p = WebsocketProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
    let dbg = format!("{p:?}");
    assert!(!dbg.contains("s3cr3t"), "password leaked into Debug: {dbg}");
    assert!(!dbg.contains("alice"), "username leaked into Debug: {dbg}");
    assert!(dbg.contains("auth=set"));
    assert_eq!(p.to_string(), "socks5h://127.0.0.1:1080");
}

#[test]
fn websocket_target_defaults_ports_by_scheme() {
    assert_eq!(
        websocket_target("wss://ws.example.com/ws/market").unwrap(),
        ("ws.example.com".into(), 443)
    );
    assert_eq!(websocket_target("ws://ws.example.com/x").unwrap(), ("ws.example.com".into(), 80));
    assert_eq!(
        websocket_target("wss://ws.example.com:9443/x").unwrap(),
        ("ws.example.com".into(), 9443)
    );
    assert!(websocket_target("not a url").is_err());
}

/// Run `connect_websocket(url, None, Some(300 ms))` on its own thread against a peer that ACCEPTS the TCP
/// connection and then says NOTHING, and require it to FAIL within 10 s, naming the timeout.
///
/// Still blocked after 10 s is the defect: a connect that waits on a silent peer for ever (`what`
/// names the unbounded read, `handshake` the handshake that cannot complete). The accepted connection
/// is held open until the function returns, so the peer is a half-open edge and not a refusal. The
/// blocked thread of a failing run is left to end with the process (its peer closes at return).
fn assert_a_silent_peer_times_out(scheme: &str, what: &str, handshake: &str) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("{scheme}://{}/", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ =
            tx.send(connect_websocket(&url, None, Some(Duration::from_millis(300))).map(|_| ()));
    });
    let (_held, _) = listener.accept().expect("connect_ws dials the listener");
    let outcome = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| {
        panic!(
            "connect_ws is still blocked 10 s after a 300 ms bound: the {what} read is unbounded"
        )
    });
    let Err(err) = outcome else { panic!("a silent peer cannot complete {handshake}") };
    assert!(err.contains("no answer within"), "the timeout is named, got: {err}");
}

/// **The handshake after the TCP phase is bounded.** A peer that accepts the connection and then says
/// nothing used to leave `connect_websocket` blocked in `tungstenite::client_tls` with no error logged and no
/// way to stop it (the order-WS connect of deribit, the market pumps' dials). Now the dialed stream has
/// `bound` as its read timeout while the handshake runs, so the connect FAILS after one window, in
/// words. `ws://`: the WebSocket upgrade's reply never comes.
#[test]
fn a_silent_peer_cannot_hold_the_plain_handshake_past_the_window() {
    assert_a_silent_peer_times_out("ws", "handshake", "a handshake");
}

/// …and the same for `wss://`: the TLS ServerHello never comes (the TLS handshake runs inside
/// `client_tls`, lazily, on the first write of the upgrade request).
#[test]
fn a_silent_peer_cannot_hold_the_tls_handshake_past_the_window() {
    assert_a_silent_peer_times_out("wss", "TLS", "a TLS handshake");
}

/// **The handshake window is GONE once the socket is up** (the control): a socket leaves `connect_websocket`
/// with no read timeout, exactly as before the window existed. A caller that never sets its own poll
/// (every one calls `configure_websocket_stream`, but none is made to) must not find a quiet, healthy feed
/// timing out after `bound`. The peer here completes a real WebSocket handshake with
/// `tungstenite::accept` and then says nothing.
#[test]
fn the_handshake_window_does_not_outlive_the_handshake() {
    assert_the_open_socket_has_no_window(|url| {
        connect_websocket(url, None, Some(Duration::from_secs(5)))
    });
}

/// …and the request form opens the same socket.
#[test]
fn the_request_form_opens_the_same_socket() {
    assert_the_open_socket_has_no_window(|url| {
        connect_websocket_request(with_a_venue_header(url), None, Some(Duration::from_secs(5)))
    });
}

/// A real upgrade with `tungstenite::accept`, then silence: `dial` must hand back a socket with no read
/// timeout on it.
fn assert_the_open_socket_has_no_window(dial: impl FnOnce(&str) -> Result<WsSocket, String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _websocket = tungstenite::accept(stream).expect("the server side of the upgrade");
        let _ = done_rx.recv_timeout(Duration::from_secs(20)); // hold the open socket
    });

    let socket = dial(&url).expect("a live peer connects");
    let timeout = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => s.read_timeout().expect("read the option"),
        other => panic!("a ws:// url dials a plain stream, got {other:?}"),
    };
    assert_eq!(timeout, None, "the 5 s handshake window must not stay on the open socket");
    drop(done_tx);
    drop(socket);
    server.join().unwrap();
}

/// The upgrade head `dial` sends to a loopback peer that reads it and hangs up, lower-cased. The dial then fails; the
/// head is what counts.
fn upgrade_head(dial: impl FnOnce(String) -> Result<WsSocket, String>) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let (mut head, mut buffer) = (Vec::new(), [0u8; 1024]);
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buffer[..n]),
            }
        }
        String::from_utf8_lossy(&head).to_ascii_lowercase()
    });
    let _ = dial(url);
    peer.join().unwrap()
}

/// **The upgrade names this client** on the bounded and the unbounded arm, and a `User-Agent` the caller set is the
/// only one sent.
#[test]
fn the_upgrade_names_this_client_unless_the_caller_named_one() {
    let ours = format!("user-agent: {}\r\n", crate::http::USER_AGENT.to_ascii_lowercase());
    for head in [
        upgrade_head(|url| connect_websocket(&url, None, Some(Duration::from_secs(5)))),
        upgrade_head(|url| connect_websocket(&url, None, None)),
    ] {
        assert!(head.contains(&ours), "{head}");
    }
    let own = upgrade_head(|url| {
        let mut request = url.into_client_request().expect("a ws url");
        request
            .headers_mut()
            .insert("User-Agent", tungstenite::http::HeaderValue::from_static("venue-asked/1"));
        connect_websocket_request(request, None, Some(Duration::from_secs(5)))
    });
    assert!(own.contains("user-agent: venue-asked/1\r\n"), "{own}");
    assert_eq!(own.matches("user-agent:").count(), 1, "{own}");
}

/// An upgrade request carrying a header of the caller's. Not a subprotocol: `tungstenite::accept`
/// answers none, and the client refuses a reply without the one it asked for.
fn with_a_venue_header(url: &str) -> Request {
    let mut request = url.into_client_request().expect("a ws url");
    request
        .headers_mut()
        .insert("X-Venue", tungstenite::http::HeaderValue::from_static("the-venue-asked"));
    request
}

// The request form against `vike_bridge_core::silent_peer`'s peer is that crate's own
// (`crates/vike-bridge-core/src/websocket_tests.rs`'s `the_request_form_sends_its_header_and_a_silent_peer_cannot_hold_it`).

/// The window the tunnel tests below spend, a short stand-in for the callers' `CONNECT_10S`, and how far past it a
/// loaded lane's scheduling may carry the answer before [`tunnelled`] calls the dial unbounded.
#[cfg(feature = "socks-proxy")]
const TUNNEL_WINDOW: Duration = Duration::from_millis(300);
#[cfg(feature = "socks-proxy")]
const SCHEDULING_SLACK: Duration = Duration::from_secs(10);
/// The window of a tunnel test whose proxy answers: long enough that no lane's scheduling cuts it.
#[cfg(feature = "socks-proxy")]
const ANSWERING_WINDOW: Duration = Duration::from_secs(5);

/// `connect_websocket(url, Some(proxy), window)` on its own thread, so a dial its window does not bound FAILS the test
/// here instead of hanging it (the blocked thread ends with the process).
#[cfg(feature = "socks-proxy")]
fn tunnelled(url: &str, proxy: &str, window: Option<Duration>) -> Result<WsSocket, String> {
    let proxy = WebsocketProxy::parse(proxy).expect("a proxy url");
    let url = url.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(connect_websocket(&url, Some(&proxy), window));
    });
    rx.recv_timeout(window.unwrap_or_default() + SCHEDULING_SLACK)
        .expect("the dial outlived its window: a tunnel holds the caller")
}

/// A SOCKS proxy double for ONE dial: it reads each `(expected, answer)` of `script` in turn, asserting the client
/// said exactly `expected`, writes `answer`, then hands the socket to `far_side`. A wrong byte panics its thread,
/// which the caller's `join` reports.
#[cfg(feature = "socks-proxy")]
fn scripted_proxy(
    script: Vec<(Vec<u8>, Vec<u8>)>,
    far_side: impl FnOnce(TcpStream) + Send + 'static,
) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let proxy = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("the dial");
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        for (expected, answer) in script {
            let mut said = vec![0; expected.len()];
            s.read_exact(&mut said).expect("the client's next message");
            assert_eq!(said, expected);
            s.write_all(&answer).expect("the proxy's answer");
        }
        far_side(s);
    });
    (address, proxy)
}

/// A far side that holds the tunnel open and silent until `done` is dropped.
#[cfg(feature = "socks-proxy")]
fn silent_until(done: std::sync::mpsc::Receiver<()>) -> impl FnOnce(TcpStream) + Send + 'static {
    move |s| {
        let _ = done.recv_timeout(Duration::from_secs(30));
        drop(s);
    }
}

/// A far side that completes the WebSocket upgrade, then waits for the client to hang up.
#[cfg(feature = "socks-proxy")]
fn answer_the_upgrade(s: TcpStream) {
    let mut websocket = tungstenite::accept(s).expect("the server side of the upgrade");
    let _ = websocket.read();
}

#[cfg(feature = "socks-proxy")]
const GREETING: [u8; 3] = [5, 1, 0];
#[cfg(feature = "socks-proxy")]
const NO_PASSWORD: [u8; 2] = [5, 0];
#[cfg(feature = "socks-proxy")]
const TUNNEL_UP: [u8; 10] = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0];
/// A target no local lookup can answer, so only a proxy that resolves it remotely can reach it.
#[cfg(feature = "socks-proxy")]
const NAMED_TARGET: &str = "ws://venue.invalid:8080/x";
/// [`NAMED_TARGET`]'s SOCKS5 CONNECT, by name (ATYP 3), port 8080.
#[cfg(feature = "socks-proxy")]
fn connect_by_name() -> Vec<u8> {
    [&[5, 1, 0, 3, 13][..], b"venue.invalid", &[0x1f, 0x90]].concat()
}

/// **A tunnel that answers is not cut, and leaves no window on the socket** (the control): it opens under a window,
/// and the socket leaves the dial with no read or write timeout on it, as the direct arm's does;
/// `configure_websocket_stream` sets the pump's poll.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_tunnel_that_answers_opens_with_no_window_left_on_the_socket() {
    let connect = [&[5, 1, 0, 3, 13][..], b"venue.example", &[0, 80]].concat();
    let (address, proxy) = scripted_proxy(
        vec![(GREETING.to_vec(), NO_PASSWORD.to_vec()), (connect, TUNNEL_UP.to_vec())],
        answer_the_upgrade,
    );
    let socket =
        tunnelled("ws://venue.example/", &format!("socks5h://{address}"), Some(ANSWERING_WINDOW))
            .expect("a tunnel that answers opens");
    let MaybeTlsStream::Plain(tcp) = socket.get_ref() else {
        panic!("a ws:// url dials a plain stream")
    };
    assert_eq!(tcp.read_timeout().expect("read the option"), None, "the read window stayed");
    assert_eq!(tcp.write_timeout().expect("read the option"), None, "the write window stayed");
    drop(socket);
    proxy.join().expect("the proxy's thread");
}

/// **A proxy that accepts and never answers the greeting cannot hold the dial.** The kernel takes the connection
/// and nobody reads it; the `socks` crate this arm used to speak waited on the reply for ever.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_proxy_that_never_answers_the_greeting_cannot_hold_the_dial() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("socks5h://{}", silent.local_addr().unwrap());
    let err = tunnelled(NAMED_TARGET, &proxy, Some(TUNNEL_WINDOW))
        .map(drop)
        .expect_err("a silent proxy opens nothing");
    assert!(err.starts_with(&format!("socks5 connect to venue.invalid:8080 via {proxy}")), "{err}");
    assert!(err.ends_with(&format!("no answer within {TUNNEL_WINDOW:?}")), "{err}");
}

/// **A proxy that answers the greeting and never the CONNECT cannot hold the dial** (a hung `ssh -D`, whose far side
/// is gone).
#[cfg(feature = "socks-proxy")]
#[test]
fn a_proxy_that_never_answers_the_connect_cannot_hold_the_dial() {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let (address, proxy) =
        scripted_proxy(vec![(GREETING.to_vec(), NO_PASSWORD.to_vec())], silent_until(done_rx));
    let err = tunnelled(NAMED_TARGET, &format!("socks5h://{address}"), Some(TUNNEL_WINDOW))
        .map(drop)
        .expect_err("a hung tunnel opens nothing");
    drop(done_tx);
    proxy.join().expect("the proxy's thread");
    assert!(err.starts_with("socks5 connect to venue.invalid:8080 via socks5h://"), "{err}");
    assert!(err.ends_with(&format!("no answer within {TUNNEL_WINDOW:?}")), "{err}");
}

/// **One deadline, not one per read**: a proxy that sends its CONNECT reply a byte at a time, each byte inside the
/// window and all ten far past it, fails the dial in the SOCKS step. A bound per read would let it finish, and the
/// words would then name the WebSocket handshake.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_proxy_answering_a_byte_at_a_time_spends_one_deadline_not_one_per_read() {
    use std::io::Write;
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let (address, proxy) = scripted_proxy(
        vec![(GREETING.to_vec(), NO_PASSWORD.to_vec()), (connect_by_name(), vec![])],
        move |mut s| {
            for byte in TUNNEL_UP {
                std::thread::sleep(TUNNEL_WINDOW / 2);
                if s.write_all(&[byte]).is_err() {
                    break;
                }
            }
            let _ = done_rx.recv_timeout(Duration::from_secs(30));
        },
    );
    let err = tunnelled(NAMED_TARGET, &format!("socks5h://{address}"), Some(TUNNEL_WINDOW))
        .map(drop)
        .expect_err("a dribbling proxy cannot stretch the window");
    drop(done_tx);
    proxy.join().expect("the proxy's thread");
    assert!(err.starts_with("socks5 connect to venue.invalid:8080 via socks5h://"), "{err}");
    assert!(err.ends_with(&format!("no answer within {TUNNEL_WINDOW:?}")), "{err}");
}

/// **A tunnel whose far side never sends the TLS ServerHello cannot hold the handshake**: the proxy answers, and the
/// venue behind it says nothing.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_tunnel_whose_far_side_never_answers_tls_cannot_hold_the_handshake() {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let (address, proxy) = scripted_proxy(
        vec![(GREETING.to_vec(), NO_PASSWORD.to_vec()), (connect_by_name(), TUNNEL_UP.to_vec())],
        silent_until(done_rx),
    );
    let err = tunnelled(
        "wss://venue.invalid:8080/x",
        &format!("socks5h://{address}"),
        Some(TUNNEL_WINDOW),
    )
    .map(drop)
    .expect_err("nobody answers the TLS handshake");
    drop(done_tx);
    proxy.join().expect("the proxy's thread");
    assert!(err.starts_with(&format!("via socks5h://{address}")), "{err}");
    assert!(err.contains("no answer within"), "the silent handshake is named: {err}");
}

/// **A black-holed proxy costs at most a window**: a reserved address (RFC 5737) no packet reaches costs the OS's SYN
/// ladder (~127 s on Linux defaults) to a dial with no bound, or fails at once on a box with no route.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_black_holed_proxy_costs_at_most_a_window() {
    let err = tunnelled(NAMED_TARGET, "socks5h://192.0.2.1:1080", Some(TUNNEL_WINDOW))
        .map(drop)
        .expect_err("nothing answers");
    assert!(
        err.starts_with("socks5 connect to venue.invalid:8080 via socks5h://192.0.2.1:1080"),
        "{err}"
    );
}

/// **A proxied dial with no window is refused by name and dials nothing**: the proxied arm has no unbounded twin.
#[cfg(feature = "socks-proxy")]
#[test]
fn a_tunnelled_dial_with_no_window_is_refused_and_dials_nothing() {
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let url = format!("socks5h://{}", proxy.local_addr().unwrap());
    let err = tunnelled(NAMED_TARGET, &url, None).map(drop).expect_err("no window, no dial");
    assert!(err.contains("a tunnelled dial takes a window, and none was given"), "{err}");
    assert!(proxy.accept().is_err(), "the proxy was dialled");
}

/// The bytes of each scheme a proxy URL may name, the HTTP lane's pin
/// (`crates/bridges/polymarket/src/transport/transport_tests.rs`'s `a_tunnel_speaks_each_scheme_and_carries_the_request`)
/// row for row, then a WebSocket upgrade through the tunnel. An IP-literal target goes as an address under
/// `socks5h` too (the `socks` crate sent it as text, and `[::1]` with its brackets).
#[cfg(feature = "socks-proxy")]
#[test]
fn a_tunnel_speaks_each_scheme_as_the_http_lane_does() {
    let by_address = vec![5, 1, 0, 1, 127, 0, 0, 1, 0x1f, 0x90];
    let hello = (GREETING.to_vec(), NO_PASSWORD.to_vec());
    let up = TUNNEL_UP.to_vec();
    let v6_bound = [&[5, 0, 0, 4][..], &[0; 18]].concat();
    let offer_password = vec![5, 2, 2, 0];
    let password = [&[1, 5][..], b"alice", &[6], b"s3cret"].concat();
    let ip_target = "ws://127.0.0.1:8080/x";
    for (row, scheme, url, script) in [
        (
            "socks5h sends the name",
            "socks5h://",
            NAMED_TARGET,
            vec![hello.clone(), (connect_by_name(), up.clone())],
        ),
        (
            "socks5h sends an address as an address",
            "socks5h://",
            ip_target,
            vec![hello.clone(), (by_address.clone(), up.clone())],
        ),
        (
            "socks5 sends the address this box resolved",
            "socks5://",
            ip_target,
            vec![hello.clone(), (by_address, up.clone())],
        ),
        (
            "a tunnel bound to a name",
            "socks5h://",
            NAMED_TARGET,
            vec![
                hello.clone(),
                (connect_by_name(), [&[5, 0, 0, 3, 3][..], b"pxy", &[0, 1]].concat()),
            ],
        ),
        (
            "a tunnel bound to an IPv6 address",
            "socks5h://",
            NAMED_TARGET,
            vec![hello.clone(), (connect_by_name(), v6_bound)],
        ),
        (
            "a password the proxy asks for",
            "socks5h://alice:s3cret@",
            NAMED_TARGET,
            vec![
                (offer_password.clone(), vec![5, 2]),
                (password, vec![1, 0]),
                (connect_by_name(), up.clone()),
            ],
        ),
        (
            "a password the proxy waives",
            "socks5h://alice:s3cret@",
            NAMED_TARGET,
            vec![(offer_password, NO_PASSWORD.to_vec()), (connect_by_name(), up)],
        ),
    ] {
        let (address, proxy) = scripted_proxy(script, answer_the_upgrade);
        let socket = tunnelled(url, &format!("{scheme}{address}"), Some(ANSWERING_WINDOW))
            .unwrap_or_else(|e| panic!("{row}: {e}"));
        drop(socket);
        proxy.join().expect(row);
    }
}

/// **A refused password ends the dial in words that never carry it.**
#[cfg(feature = "socks-proxy")]
#[test]
fn a_refused_password_is_never_in_the_words() {
    let password = [&[1, 5][..], b"alice", &[7], b"hunter2"].concat();
    let (address, proxy) =
        scripted_proxy(vec![(vec![5, 2, 2, 0], vec![5, 2]), (password, vec![1, 1])], drop);
    let err = tunnelled(
        NAMED_TARGET,
        &format!("socks5h://alice:hunter2@{address}"),
        Some(ANSWERING_WINDOW),
    )
    .map(drop)
    .expect_err("a refused password opens nothing");
    proxy.join().expect("the proxy's thread");
    assert!(err.starts_with("socks5 connect to venue.invalid:8080 via socks5h://"), "{err}");
    assert!(!err.contains("hunter2"), "{err}");
}

/// INERTNESS: `connect_websocket(url, None, _)` must never touch the proxy machinery — a malformed
/// URL fails in the bounded arm's pre-dial parse, with no socket opened and no proxy consulted.
#[test]
fn no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before() {
    let err = connect_websocket("not a url", None, Some(Duration::from_millis(50))).unwrap_err();
    assert!(err.starts_with("bad ws url"), "unexpected: {err}");
}

/// The `Some(proxy)` arm reaches the proxy path (and, without the feature, says so by name)
/// — never a silent fall-through to a direct dial.
#[test]
fn proxy_arm_never_silently_falls_back_to_direct() {
    let p = WebsocketProxy::parse("socks5h://127.0.0.1:1").unwrap();
    // Port 1 is reserved; nothing listens, so the SOCKS connect fails fast on every OS.
    let err = connect_websocket("wss://example.invalid/ws", Some(&p), Some(Duration::from_secs(5)))
        .unwrap_err();
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

    let p = WebsocketProxy::parse(&format!("socks5h://127.0.0.1:{port}")).unwrap();
    let err = connect_websocket(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
        Some(&p),
        Some(Duration::from_secs(5)),
    )
    .unwrap_err();
    let (atyp, domain, tport) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    server.join().unwrap();

    assert_eq!(atyp, 3, "ATYP must be DOMAINNAME (3) — socks5h resolves at the proxy");
    assert_eq!(domain, "ws-subscriptions-clob.polymarket.com");
    assert_eq!(tport, 443);
    // The tunnel was established; only the TLS/WS handshake against the dummy failed.
    assert!(err.starts_with("via socks5h://127.0.0.1:"), "unexpected: {err}");
}
