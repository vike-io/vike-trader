//! The agents' guarantees over loopback servers, no network: which redirects each follows, the resolver, the connect
//! failure, the body head and the headers form of the public read.

use super::*;
use std::io::{Error, ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn only_a_connect_that_never_completed_is_a_connect_failure() {
    assert!(is_connect_failure(&ureq::Error::Timeout(ureq::Timeout::Connect)));
    assert!(is_connect_failure(&ureq::Error::Io(Error::new(ErrorKind::ConnectionRefused, "x"))));
    assert!(!is_connect_failure(&ureq::Error::Timeout(ureq::Timeout::Global)));
    assert!(!is_connect_failure(&ureq::Error::Timeout(ureq::Timeout::RecvResponse)));
    assert!(!is_connect_failure(&ureq::Error::Io(Error::new(ErrorKind::ConnectionReset, "x"))));
}

#[test]
fn every_timeout_phase_is_a_timeout_and_nothing_else_is() {
    assert!(is_timeout(&ureq::Error::Timeout(ureq::Timeout::Connect)));
    assert!(is_timeout(&ureq::Error::Timeout(ureq::Timeout::Global)));
    assert!(is_timeout(&ureq::Error::Timeout(ureq::Timeout::RecvResponse)));
    assert!(!is_timeout(&ureq::Error::Io(Error::new(ErrorKind::TimedOut, "timeout"))));
    assert!(!is_timeout(&ureq::Error::StatusCode(504)));
}

#[test]
fn truncates_to_200_chars_and_passes_short_bodies_through() {
    assert_eq!(body_head("short"), "short");
    assert_eq!(body_head(&"x".repeat(500)).len(), 200);
    // chars(), not bytes — a multi-byte body must not split a char boundary/panic.
    assert_eq!(body_head(&"é".repeat(500)).chars().count(), 200);
}

/// A server that answers every request with `reply` and counts the requests it served.
fn serve(reply: String) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let n = hits.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buffer = [0u8; 2048];
            let _ = s.read(&mut buffer);
            n.fetch_add(1, Ordering::SeqCst);
            let _ = s.write_all(reply.as_bytes());
        }
    });
    (format!("http://{addr}"), hits)
}

/// The loopback agent shares the venue agents' two guarantees: a 3xx is the answer (a request is
/// never replayed to its `Location`, which for a 301/302/303 would turn a POST or DELETE into a
/// bodyless GET carrying every header but `Authorization`), and a failed DNS lookup is the
/// definite `HostNotFound`.
#[test]
fn the_loopback_agent_follows_no_redirect() {
    let (target, target_hits) =
        serve("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".into());
    let (base, _) = serve(format!(
        "HTTP/1.1 302 Found\r\nlocation: {target}/y\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    ));
    let agent = blocking_agent_loopback_insecure();
    for response in
        [agent.post(&format!("{base}/x")).send_empty(), agent.delete(&format!("{base}/x")).call()]
    {
        assert_eq!(response.expect("the 302 is a response").status().as_u16(), 302);
    }
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the Location was never requested");
}

/// The resolver is what makes a DNS failure definite, for the built agents and for a builder ended in a finisher.
#[test]
fn a_dns_failure_through_the_loopback_agent_or_a_finisher_is_host_not_found() {
    let raw = blocking_agent_loopback_insecure().post("http://no-such-host.invalid/x").send_empty();
    assert!(matches!(raw, Err(ureq::Error::HostNotFound)), "{raw:?}");
    for agent in [
        agent_no_redirect(ureq::Agent::config_builder()),
        agent_public_get(ureq::Agent::config_builder()),
    ] {
        let raw = agent.get("http://no-such-host.invalid/x").call();
        assert!(matches!(raw, Err(ureq::Error::HostNotFound)), "{raw:?}");
    }
}

const OK_JSON: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}";

/// A redirect to `location` (a 302), and nothing else.
fn redirect_to(location: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    )
}

/// A server that answers EVERY request with a redirect to itself (a loop), counting the requests.
fn serve_a_loop() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let (n, reply) = (hits.clone(), redirect_to(&format!("{base}/again")));
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buffer = [0u8; 2048];
            let _ = s.read(&mut buffer);
            n.fetch_add(1, Ordering::SeqCst);
            let _ = s.write_all(reply.as_bytes());
        }
    });
    (base, hits)
}

/// **Redirects are a decision, per kind of call** (A50): an UNSIGNED public GET follows a few on purpose, every other
/// builder follows none. THE POINT: a catalog read whose host moved the endpoint behind a redirect is still read.
#[test]
fn get_json_follows_a_redirect_to_the_moved_endpoint() {
    let (target, target_hits) = serve(OK_JSON.to_string());
    let (base, _) = serve(redirect_to(&format!("{target}/moved")));
    let json = get_json(&format!("{base}/catalog")).expect("the redirect is followed");
    assert_eq!(json, serde_json::json!({"ok": true}));
    assert_eq!(target_hits.load(Ordering::SeqCst), 1, "the Location was read once");
}

/// …and the follow is BOUNDED: a loop fails after exactly the stated number of hops (the first
/// request plus [`PUBLIC_GET_MAX_REDIRECTS`] followed redirects), in words, instead of ureq's ten.
/// The public-GET finisher states the same figure.
#[test]
fn a_redirect_loop_stops_at_the_stated_limit() {
    let (base, hits) = serve_a_loop();
    let err = get_json(&format!("{base}/loop")).expect_err("a loop cannot be read");
    assert!(err.contains("too many redirects"), "{err}");
    assert_eq!(
        hits.load(Ordering::SeqCst) as u32,
        PUBLIC_GET_MAX_REDIRECTS + 1,
        "one request and {PUBLIC_GET_MAX_REDIRECTS} followed redirects"
    );
    for agent in [public_get_agent(), agent_public_get(ureq::Agent::config_builder())] {
        assert_eq!(agent.config().max_redirects(), PUBLIC_GET_MAX_REDIRECTS);
    }
    assert_eq!(PUBLIC_GET_MAX_REDIRECTS, 5, "the figure the datahub and backfill agents take too");
    let stated = agent_following(ureq::Agent::config_builder(), 3);
    assert_eq!(stated.config().max_redirects(), 3, "the caller's own figure, never ureq's ten");
}

/// THE OTHER SIDE, which must not move: every venue builder and the no-redirect finisher follow none, so a signed
/// query (or any request carrying a credential) is never replayed to a `Location`. The 302 is the answer and the
/// target is never asked.
#[test]
fn every_venue_builder_still_follows_no_redirect() {
    let (target, target_hits) = serve(OK_JSON.to_string());
    let (base, _) = serve(redirect_to(&format!("{target}/moved")));
    for (name, agent) in [
        ("blocking_agent", blocking_agent()),
        ("blocking_agent_with_timeout", blocking_agent_with_timeout(Duration::from_secs(5))),
        (
            "blocking_agent_with_connect_timeout",
            blocking_agent_with_connect_timeout(Duration::from_secs(5), Duration::from_secs(5)),
        ),
        ("blocking_agent_for_lanes", blocking_agent_for_lanes(8)),
        ("blocking_agent_loopback_insecure", blocking_agent_loopback_insecure()),
        (
            "agent_no_redirect",
            agent_no_redirect(ureq::Agent::config_builder().http_status_as_error(false)),
        ),
    ] {
        assert_eq!(agent.config().max_redirects(), 0, "{name}");
        let response = agent.get(&format!("{base}/x")).call().expect("the 302 is a response");
        assert_eq!(response.status().as_u16(), 302, "{name}");
    }
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "no venue agent asked the Location");
}

/// **The timed no-redirect agent's four settings, which vike-alerting's webhook transport, `vike-ai`'s provider
/// clients and the tradehub's Telegram long poll are built with** (each was its own copy, then vike-alerting's
/// `no_redirect_agent`, until decision 0299): 4xx/5xx as responses, no redirect, the caller's global timeout,
/// [`USER_AGENT`]. MUTATION PROOF: drop any one setting from [`blocking_agent_with_timeout`] or its finisher and this
/// fails.
#[test]
fn the_timed_agent_carries_its_four_settings() {
    for secs in [5, 10, 900] {
        let agent = blocking_agent_with_timeout(Duration::from_secs(secs));
        let config = agent.config();
        assert!(!config.http_status_as_error(), "a 4xx/5xx is a response");
        assert_eq!(config.max_redirects(), 0, "no redirect is followed");
        assert_eq!(config.timeouts().global, Some(Duration::from_secs(secs)));
        assert!(
            matches!(config.user_agent(), ureq::config::AutoHeaderValue::Provided(ua) if ua.as_str() == USER_AGENT),
            "{:?}",
            config.user_agent()
        );
    }
}

/// The headers form is [`get_json`] plus its headers: each reaches the wire, and the answer and the
/// error words are the plain read's (okx's demo-network read spelled the same words by hand).
#[test]
fn each_header_reaches_the_wire_and_the_answer_is_the_plain_reads() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/instruments", l.local_addr().unwrap());
    let request = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let (mut req, mut buffer) = (Vec::new(), [0u8; 1024]);
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => req.extend_from_slice(&buffer[..n]),
            }
        }
        s.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        )
        .unwrap();
        String::from_utf8_lossy(&req).to_ascii_lowercase()
    });
    let json = get_json_with_headers(&url, &[("x-simulated-trading", "1")]).expect("read");
    assert_eq!(json, serde_json::json!({"ok": true}));
    let request = request.join().unwrap();
    assert!(request.contains("x-simulated-trading: 1"), "{request}");
}

#[test]
fn a_refusal_reads_in_the_plain_reads_words() {
    let (base, _) = serve(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy"
            .into(),
    );
    let url = format!("{base}/instruments");
    let words = Err(format!("{url} HTTP 503: busy"));
    assert_eq!(get_json_with_headers(&url, &[("x-simulated-trading", "1")]), words);
    assert_eq!(get_json(&url), words);
}

/// **A finisher's agent never keeps the proxy its builder carried** (ureq's default config takes one from
/// `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`, so that is every builder's): it dials direct.
/// `tests/proxy_environment.rs` proves the same with the variables really set.
#[test]
fn a_finisher_drops_any_proxy_its_builder_carried() {
    let carrying = || {
        ureq::Agent::config_builder().proxy(Some(ureq::Proxy::new("http://127.0.0.1:9").unwrap()))
    };
    for agent in [
        agent_no_redirect(carrying()),
        agent_public_get(carrying()),
        agent_following(carrying(), 2),
    ] {
        assert!(agent.config().proxy().is_none(), "{:?}", agent.config().proxy());
    }
}

/// An HTTP CONNECT proxy on loopback that serves one tunnel at a time: it sends each CONNECT head it reads to the
/// receiver, opens the tunnel and answers the tunnelled request itself, 200 `ok`.
fn connect_proxy() -> (u16, std::sync::mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let _ = tx.send(read_head(&mut s));
            let _ = s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
            let _ = read_head(&mut s);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    (port, rx)
}

/// One request head off `s`, lower-cased.
fn read_head(s: &mut std::net::TcpStream) -> String {
    let (mut head, mut buffer) = (Vec::new(), [0u8; 1024]);
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buffer[..n]),
        }
    }
    String::from_utf8_lossy(&head).to_ascii_lowercase()
}

/// **A request sent through [`request_no_redirect`] stops at the 3xx, whatever its agent follows**, and the control:
/// the same GET without it follows. ureq strips only `Authorization` (and `Cookie`) on a redirect.
#[test]
fn request_no_redirect_stops_a_following_agent_at_the_3xx() {
    let (target, target_hits) = serve(OK_JSON.to_string());
    let (base, _) = serve(redirect_to(&format!("{target}/stolen")));
    let agent = public_get_agent();
    let keyed = agent.get(&format!("{base}/keyed")).header("x-api-key", "k");
    let stopped = request_no_redirect(keyed).call().expect("the 302 is a response");
    assert_eq!(stopped.status().as_u16(), 302);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the Location was never asked");
    let followed = agent.get(&format!("{base}/keyless")).call().expect("followed");
    assert_eq!(followed.status().as_u16(), 200);
    assert_eq!(target_hits.load(Ordering::SeqCst), 1);
    assert_eq!(agent.config().redirect_auth_headers(), ureq::config::RedirectAuthHeaders::Never);
}

#[test]
fn the_user_agent_is_this_client_and_its_version_never_a_browser() {
    assert_eq!(USER_AGENT, format!("vike-trader-rust/{}", env!("CARGO_PKG_VERSION")));
    assert!(!USER_AGENT.contains("Mozilla"), "{USER_AGENT}");
}

/// The head of the one request `send` makes to a loopback server, lower-cased (the server answers 200 `{}`).
fn head_of(send: impl FnOnce(&str)) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let head = read_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        head
    });
    send(&url);
    server.join().unwrap()
}

/// **Every agent names this client, the finisher's name over its builder's, and a request's own header still wins**
/// (ureq 3.5.2 sends the agent's only when the request has none): SEC's declared contact string is the one SEC sees.
#[test]
fn every_agent_names_this_client_and_a_request_header_still_wins() {
    let ours = format!("user-agent: {USER_AGENT}\r\n").to_ascii_lowercase();
    let seconds = Duration::from_secs(5);
    let builder_said_another =
        agent_no_redirect(ureq::Agent::config_builder().user_agent("someone-else/1"));
    for (name, agent) in [
        ("blocking_agent", blocking_agent()),
        ("public_get_agent", public_get_agent()),
        ("stream_agent", stream_agent(seconds, None, seconds, StatusPolicy::Error)),
        (
            "public_get_agent_bounded",
            public_get_agent_bounded(None, CallBound::Total(seconds), StatusPolicy::Error),
        ),
        ("a builder that named another", builder_said_another.clone()),
    ] {
        let head = head_of(|url| drop(agent.get(url).call()));
        assert!(head.contains(&ours), "{name}: {head}");
    }
    let declared = head_of(|url| {
        drop(
            builder_said_another
                .get(url)
                .header("User-Agent", "Declared contact@example.com")
                .call(),
        )
    });
    assert!(declared.contains("user-agent: declared contact@example.com\r\n"), "{declared}");
    assert_eq!(declared.matches("user-agent:").count(), 1, "{declared}");
}

/// **The stream agent's bounds and status policy are its parameters**, and it has neither a total nor a whole-body
/// budget (either would end a healthy stream). MUTATION PROOF: hard-code any one knob and a row here fails.
#[test]
fn the_stream_agent_takes_its_bounds_and_status_policy_as_parameters() {
    let (connect, head, idle) =
        (Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(20));
    for (response, status) in [(Some(head), StatusPolicy::Response), (None, StatusPolicy::Error)] {
        let agent = stream_agent(connect, response, idle, status);
        let config = agent.config();
        assert_eq!(config.timeouts().connect, Some(connect));
        assert_eq!(config.timeouts().recv_response, response);
        assert_eq!(config.timeout_per_read(), Some(idle));
        assert_eq!(config.timeouts().global, None, "the request is the stream: no total");
        assert_eq!(config.timeouts().recv_body, None, "never a budget on the whole body");
        assert_eq!(config.http_status_as_error(), status == StatusPolicy::Error, "{status:?}");
        assert_eq!(config.max_redirects(), 0, "the open carries a credential");
    }
}

/// The status policy on the wire: under `Response` a 503 is the answer, under `Error` it is `StatusCode(503)`.
#[test]
fn the_status_policy_decides_what_a_5xx_is() {
    let (base, _) = serve(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy"
            .into(),
    );
    let seconds = Duration::from_secs(5);
    let call = |agent: ureq::Agent| agent.get(&format!("{base}/x")).call();
    for status in [StatusPolicy::Response, StatusPolicy::Error] {
        let stream = call(stream_agent(seconds, None, seconds, status));
        let public = call(public_get_agent_bounded(None, CallBound::Total(seconds), status));
        for answer in [stream, public] {
            match status {
                StatusPolicy::Response => {
                    assert_eq!(answer.expect("a response").status().as_u16(), 503)
                }
                StatusPolicy::Error => {
                    assert!(matches!(answer, Err(ureq::Error::StatusCode(503))), "{answer:?}")
                }
            }
        }
    }
}

/// **A host that takes the stream's request and never answers fails after `read_idle`**: the per-read bound caps
/// the wait for the head too, with no response bound given.
#[test]
fn a_silent_host_fails_the_stream_after_its_idle_bound() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", l.local_addr().unwrap());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = read_head(&mut s);
        let _ = done_rx.recv_timeout(Duration::from_secs(30)); // hold the socket, say nothing
    });
    let agent =
        stream_agent(Duration::from_secs(5), None, Duration::from_millis(200), StatusPolicy::Error);
    let started = std::time::Instant::now();
    let answer = agent.get(&url).call();
    assert!(matches!(answer, Err(ureq::Error::Timeout(_))), "{answer:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    drop(done_tx);
}

/// **The public GET builder's bounds and status policy are its parameters**; it follows the stated public-GET limit.
#[test]
fn the_public_get_builder_takes_its_bounds_and_status_policy_as_parameters() {
    let (connect, bound) = (Duration::from_secs(5), Duration::from_secs(30));
    let total =
        public_get_agent_bounded(Some(connect), CallBound::Total(bound), StatusPolicy::Response);
    let config = total.config();
    assert_eq!(config.timeouts().connect, Some(connect));
    assert_eq!(config.timeouts().global, Some(bound));
    assert_eq!(config.timeout_per_read(), None);
    assert!(!config.http_status_as_error());
    assert_eq!(config.max_redirects(), PUBLIC_GET_MAX_REDIRECTS);
    let idle = public_get_agent_bounded(None, CallBound::Idle(bound), StatusPolicy::Error);
    let config = idle.config();
    assert_eq!(config.timeouts().connect, None);
    assert_eq!(config.timeouts().global, None, "an idle bound sets no ceiling");
    assert_eq!(config.timeout_per_read(), Some(bound));
    assert!(config.http_status_as_error());
    assert_eq!(config.max_redirects(), PUBLIC_GET_MAX_REDIRECTS);
}

/// **One arm per failure, in the fixed vocabulary, and the error itself is never formatted**: a URL the error
/// carries (here with a key in its query) is in none of the words.
#[test]
fn a_failure_reads_in_the_fixed_vocabulary_one_arm_each() {
    for (error, words) in [
        (ureq::Error::StatusCode(429), "HTTP 429"),
        (ureq::Error::Timeout(ureq::Timeout::Global), "timed out"),
        (ureq::Error::Timeout(ureq::Timeout::Connect), "timed out"),
        (
            ureq::Error::Io(Error::new(ErrorKind::ConnectionRefused, "x")),
            "I/O error (ConnectionRefused)",
        ),
        (ureq::Error::HostNotFound, "host not found"),
        (ureq::Error::ConnectionFailed, "connection failed"),
        (ureq::Error::TooManyRedirects, "redirect failed"),
        (ureq::Error::RedirectFailed, "redirect failed"),
        (ureq::Error::BodyExceedsLimit(10), "the body was too large"),
        (ureq::Error::BadUri("https://x.example/?apikey=s3cret".into()), "request failed"),
    ] {
        assert_eq!(describe_error(&error), words, "{error:?}");
    }
}

/// **The proxied agent dials the caller's proxy with the base settings, and an unusable one refuses every request
/// and dials nothing**: an `http://` proxy is tunnelled through (this client's name on the CONNECT), a SOCKS one is
/// dialled by the bounded `SocksDial` (its own words), an `Err` is refused before a socket exists.
#[cfg(feature = "socks-proxy")]
#[test]
fn the_proxied_agent_dials_the_callers_proxy_and_an_unusable_one_refuses_every_request() {
    let total = Duration::from_secs(5);
    let (port, heads) = connect_proxy();
    let agent = proxied_agent(total, ureq::Proxy::new(&format!("http://127.0.0.1:{port}")));
    let config = agent.config();
    assert!(!config.http_status_as_error(), "a 4xx/5xx is a response");
    assert_eq!(config.max_redirects(), 0, "the calls carry credentials");
    assert_eq!(config.timeouts().global, Some(total));
    let mut response = agent.get("http://vike-tunnel.invalid/x").call().expect("tunnelled");
    assert_eq!(response.body_mut().read_to_string().unwrap(), "ok");
    let head = heads.recv_timeout(total).expect("the proxy was dialled");
    assert!(head.starts_with("connect vike-tunnel.invalid:80 "), "{head}");
    assert!(head.contains(&format!("user-agent: {USER_AGENT}").to_ascii_lowercase()), "{head}");

    let socks = proxied_agent(total, ureq::Proxy::new("socks5h://127.0.0.1:1"));
    let err = socks.get("http://vike-tunnel.invalid/x").call().expect_err("nothing listens");
    assert!(
        matches!(&err, ureq::Error::ConnectProxyFailed(why) if why.starts_with("no tunnel through the SOCKS proxy")),
        "{err:?}"
    );

    let refusing = proxied_agent(total, Err(ureq::Error::InvalidProxyUrl));
    let refused = refusing.get("http://vike-tunnel.invalid/x").call();
    assert!(matches!(refused, Err(ureq::Error::InvalidProxyUrl)), "{refused:?}");
    assert!(heads.try_recv().is_err(), "an unusable proxy dialled something");
}
