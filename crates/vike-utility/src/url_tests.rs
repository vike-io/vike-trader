use super::{is_loopback_url, url_authority, websocket_url_from_http};

/// The authority is what sits between the scheme and the first `/`; no scheme reads from the start.
#[test]
fn the_authority_is_host_and_port_without_scheme_or_path() {
    assert_eq!(url_authority("wss://127.0.0.1:5000/v1/api/ws"), "127.0.0.1:5000");
    assert_eq!(url_authority("https://[::1]:5000"), "[::1]:5000");
    assert_eq!(url_authority("localhost:5000/v1"), "localhost:5000");
    assert_eq!(url_authority(""), "");
}

#[test]
fn loopback_hosts_only() {
    assert!(is_loopback_url("https://127.0.0.1:5000"));
    assert!(is_loopback_url("https://localhost:5000/v1/api"));
    assert!(is_loopback_url("https://127.0.0.5:5000"));
    assert!(is_loopback_url("https://[::1]:5000"));
    assert!(!is_loopback_url("https://api.ibkr.com/v1/api"));
    assert!(!is_loopback_url("https://<host>:5000"));
    assert!(!is_loopback_url("https://127evil.com:5000")); // not a parseable IP
    // The security case: a loopback-LOOKING hostname must NOT pass (it would enable MITM under
    // the cert-bypass agent). Only real loopback IPs / literal "localhost" do.
    assert!(!is_loopback_url("https://127.0.0.1.evil.com:5000"));
    assert!(!is_loopback_url("https://localhost.evil.com:5000"));
    // A `user@` part puts the real host after the `@`: each of these dials `evil.com`.
    assert!(!is_loopback_url("https://127.0.0.1:1@evil.com"));
    assert!(!is_loopback_url("https://localhost:5000@evil.com:5000"));
    assert!(!is_loopback_url("https://[::1]@evil.com"));
    assert!(!is_loopback_url("https://user@127.0.0.1:5000"));
}

/// An HTTP base becomes the WebSocket url of a path on its host: `http` → `ws`, anything else → `wss`.
#[test]
fn an_http_base_becomes_the_websocket_url_of_a_path_on_its_host() {
    let ws = websocket_url_from_http;
    assert_eq!(ws("https://127.0.0.1:5000", "/v1/api/ws"), "wss://127.0.0.1:5000/v1/api/ws");
    assert_eq!(ws("https://h.example:443/", "/ls"), "wss://h.example:443/ls");
    assert_eq!(ws("http://local:8080", "/ls"), "ws://local:8080/ls");
    assert_eq!(ws("push.example", "/ls"), "wss://push.example/ls");
}
