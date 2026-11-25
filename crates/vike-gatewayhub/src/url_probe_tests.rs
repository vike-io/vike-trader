//! The probe's rule, one test per class, against loopback listeners: a RESOURCE (a `.jnlp`) is up on
//! 2xx/3xx only; every other rest, stream and ws row is a TCP connect plus a TLS handshake that judges
//! no HTTP answer (FXCM's `Hosts.jsp` among them); a socket is a TCP connect; and a row no active
//! account dials is NOT IN USE, probed but never down.

use std::io::{Read, Write};
use std::net::TcpListener;

use vike_venues::venue_urls::UrlChannel::{All, History, Trade, User};
use vike_venues::venue_urls::UrlProtocol::{Rest, Socket, Stream, Websocket};
use vike_venues::venue_urls::UrlType::{Demo, Live};
use vike_venues::venue_urls::{UrlChannel, UrlType};

use super::*;

/// A listener that answers every connection with `status`, and its `http://127.0.0.1:<port>` base.
fn answering(status: u16) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nLocation: /x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    format!("http://{addr}")
}

/// An address nothing listens on.
fn refused() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    addr.to_string()
}

fn key(
    venue: &str,
    ty: UrlType,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &str,
) -> UrlKey {
    UrlKey { venue: venue.into(), ty, channel, protocol, product: product.into() }
}

#[test]
fn a_resource_is_a_jnlp_and_hosts_jsp_is_a_base_host() {
    assert!(is_resource("jnlp", "https://platform.dukascopy.com/demo/jforex.jnlp"));
    assert!(is_resource("", "https://example.com/client/jforex.jnlp"), "the path alone says so");
    assert!(is_resource("jnlp", "https://example.com"), "the product alone says so");
    for base in [
        "https://www.fxcorporate.com/Hosts.jsp",
        "https://api.binance.com",
        "wss://stream.binance.com:9443",
        "https://api.ig.com/gateway/deal",
        "https://datafeed.dukascopy.com/datafeed",
        "https://127.0.0.1:5000",
    ] {
        assert!(!is_resource("", base), "{base} is a base host");
    }
    assert!(!is_resource("Demo", "https://www.fxcorporate.com/Hosts.jsp"), "fxcm's product word");
}

#[test]
fn a_resource_is_up_on_2xx_or_3xx_only() {
    let file = |status| format!("{}/demo/jforex.jnlp", answering(status));
    assert_eq!(probe(Rest, "jnlp", &file(200)), (true, "200".to_string()));
    assert!(probe(Rest, "jnlp", &file(302)).0, "a redirect is an answer");
    assert_eq!(probe(Rest, "jnlp", &file(404)), (false, "404".to_string()));
    // A 429 is a refusal for now (a rate limit), worded so, never as a dead endpoint.
    let limited = (false, format!("{}: 429", vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW));
    assert_eq!(probe(Rest, "jnlp", &file(429)), limited);
    let (ok, words) = probe(Rest, "jnlp", &format!("http://{}/jforex.jnlp", refused()));
    assert!(!ok && words.starts_with("no answer"), "{words}");
}

/// the CI box, 2026-11-09: the old rule read FXCM's 503 as down while FXCM ran with its link PASS.
#[test]
fn hosts_jsp_answering_503_to_a_bare_request_is_up() {
    let hosts = format!("{}/Hosts.jsp", answering(503));
    assert_eq!(probe(Rest, "Demo", &hosts), (true, "connected".to_string()));
}

#[test]
fn a_base_host_judges_the_connect_and_no_http_answer() {
    for status in [200, 404, 503] {
        assert!(probe(Rest, "", &answering(status)).0, "{status}");
        assert!(probe(Stream, "", &answering(status)).0, "{status}");
    }
    let (ok, words) = probe(Rest, "", &format!("http://{}", refused()));
    assert!(!ok && words.starts_with("connect failed"), "{words}");
    // The IBKR Client Portal gateway's self-signed certificate: a loopback https row is its connect.
    let loopback = answering(200).replacen("http://", "https://", 1);
    let judged = (true, "connected (loopback: TLS not judged)".to_string());
    assert_eq!(probe(Rest, "cpapi", &loopback), judged);
}

#[test]
fn a_websocket_row_is_a_base_host() {
    let ws = answering(404).replacen("http://", "ws://", 1);
    assert_eq!(probe(Websocket, "", &ws), (true, "connected".to_string()));
    let (ok, words) = probe(Websocket, "", &format!("wss://{}", refused()));
    assert!(!ok && words.starts_with("connect failed"), "{words}");
}

/// The handshake itself, past the loopback shortcut: an HTTP answer (which the old GET took for up)
/// and a peer that hangs up are both refused.
#[test]
fn the_tls_handshake_refuses_an_answer_that_is_not_tls() {
    let http = answering(503);
    let tcp = TcpStream::connect(http.trim_start_matches("http://")).expect("connect");
    let words = handshake(tcp, "localhost").expect_err("an HTTP answer is not TLS");
    assert!(words.starts_with("TLS handshake failed"), "{words}");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || listener.incoming().flatten().for_each(drop));
    let tcp = TcpStream::connect(addr).expect("connect");
    assert!(handshake(tcp, "localhost").is_err(), "a hang-up is no handshake");
}

#[test]
fn an_address_is_its_host_and_its_port() {
    let host = |tls: bool, host: &str, port: u16| Some((tls, host.to_string(), port));
    assert_eq!(host_port("https://api.binance.com"), host(true, "api.binance.com", 443));
    assert_eq!(host_port("wss://stream.binance.com:9443"), host(true, "stream.binance.com", 9443));
    let dukascopy = host(true, "datafeed.dukascopy.com", 443);
    assert_eq!(host_port("https://datafeed.dukascopy.com/datafeed"), dukascopy);
    // An IPv6 literal comes back bare, as the lookup and the TLS server name read it.
    assert_eq!(host_port("ws://[::1]"), host(false, "::1", 80));
    assert_eq!(host_port("https://[::1]:5000"), host(true, "::1", 5000));
    assert_eq!(host_port("https://venue.example:http"), None, "a port that is no number");
    assert_eq!(host_port("127.0.0.1:4102"), None, "a socket row has no scheme");
}

#[test]
fn a_socket_is_a_tcp_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let open = listener.local_addr().expect("addr").to_string();
    assert_eq!(probe(Socket, "gateway", &open), (true, "connected".to_string()));
    let (ok, words) = probe(Socket, "gateway", &refused());
    assert!(!ok && words.starts_with("connect failed"), "{words}");
    // The network layer's words follow, so the address dialled is named.
    assert!(words.contains("127.0.0.1"), "{words}");
}

/// **The dukascopy case** (the CI box, 2026-11-09: a dual-stack host whose FIRST address this box cannot reach, the
/// second answering). Under the probe's window and cap, the network layer's rule (each address an equal share of
/// what is LEFT, at most the cap: `crates/vike-transport/src/dial.rs`'s `dial_bounded`, its own black-hole test
/// through the injected steps) hands the black-holed first address [`PROBE_TIMEOUT`] and STILL a full
/// [`PROBE_TIMEOUT`] to the second, even after a lookup that took a whole [`PROBE_TIMEOUT`]. A window of two
/// timeouts would short the second address by whatever the lookup spent.
#[test]
fn a_black_holed_first_address_leaves_the_second_a_full_probe_timeout() {
    let share = |left: Duration, addresses_left: u32| (left / addresses_left).min(PROBE_TIMEOUT);
    for lookup in [Duration::ZERO, Duration::from_millis(10), PROBE_TIMEOUT] {
        let left = PROBE_DIAL_BOUND - lookup;
        let black_hole = share(left, 2);
        assert_eq!(black_hole, PROBE_TIMEOUT, "the first address after a {lookup:?} lookup");
        let working = share(left - black_hole, 1);
        assert_eq!(working, PROBE_TIMEOUT, "the second address after a {lookup:?} lookup");
    }
}

/// A row no active account dials is recorded NOT IN USE with what was found, never down; a row in
/// use keeps its verdict.
#[test]
fn a_row_no_active_account_dials_is_recorded_not_in_use_and_never_down() {
    let found = (false, "no answer: timeout".to_string());
    let recorded =
        (None, "not in use (no active aster demo account); no answer: timeout".to_string());
    assert_eq!(verdict(Some("no active aster demo account".to_string()), found.clone()), recorded);
    assert_eq!(verdict(None, found), (Some(false), "no answer: timeout".to_string()));
}

/// the CI box on 2026-11-09, over a planted store: aster LIVE and ibkr DEMO active, no backend row (the
/// socket). The rows it warned on that no account dials are unused, a row in use is judged, and a
/// tier's `backend` row reaches the rule (which rows are which is `vike_venues`', tested there).
#[test]
fn what_is_in_use_is_read_from_the_account_rows_and_the_settings() {
    let dir = tempfile::tempdir().expect("temp");
    vike_bridge_core::credential_fixture::write_store(
        dir.path(),
        &[
            ("aster", AccountTier::Live, None, Vec::new()),
            ("ibkr", AccountTier::Demo, None, Vec::new()),
        ],
    );
    let store = CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        vike_secrets::CredentialScope::Full,
    );
    let mut read = InUse::read(&store, dir.path()).expect("a readable store");
    let words = |read: &InUse, k: UrlKey| read.unused(&k);
    let none_at_demo = Some("no active aster demo account".to_string());
    assert_eq!(words(&read, key("aster", Demo, User, Websocket, "spot")), none_at_demo);
    assert_eq!(
        words(&read, key("aster", Live, All, Websocket, "spot")),
        None,
        "the data lane dials it"
    );
    assert_eq!(words(&read, key("dukascopy", Live, History, Rest, "")), None);
    assert_eq!(words(&read, key("ibkr", Demo, Trade, Socket, "gateway")), None);
    let on_socket = Some("the ibkr demo backend is socket".to_string());
    assert_eq!(words(&read, key("ibkr", Demo, Trade, Rest, "cpapi")), on_socket);
    let at_demo = Some("the ibkr session is demo".to_string());
    assert_eq!(words(&read, key("ibkr", Live, Trade, Socket, "gateway")), at_demo);
    // A padded `backend` row is read as every reader reads it: on cpapi the cpapi row is dialled.
    let cpapi = vike_bridge_core::venue_adapter_fixture::venue_settings(
        "ibkr",
        "demo",
        &[("backend", " cpapi ")],
    );
    read.settings.insert("ibkr".to_string(), cpapi);
    assert_eq!(words(&read, key("ibkr", Demo, Trade, Rest, "cpapi")), None);
    assert!(words(&read, key("ibkr", Demo, Trade, Socket, "gateway")).is_some());
}

/// **The rows the CI box's probe flagged on 2026-11-09, dialled for real** (`#[ignore]`d: it reaches public
/// venue hosts, no account and no credential). Run on a lane:
/// `cargo test -p vike-gatewayhub --lib -- --ignored url_probe::tests::the_rows_prod2_flagged`.
#[test]
#[ignore = "dials public venue hosts"]
fn the_rows_prod2_flagged_answer_the_handshake() {
    for (protocol, product, url) in [
        (Rest, "Demo", "https://www.fxcorporate.com/Hosts.jsp"),
        (Websocket, "spot", "wss://sstream.asterdex-testnet.com"),
        (Websocket, "usdm", "wss://fstream.asterdex-testnet.com"),
        (Websocket, "spot", "wss://sstream.asterdex.com"),
        (Rest, "", "https://datafeed.dukascopy.com/datafeed"),
    ] {
        assert_eq!(probe(protocol, product, url), (true, "TLS ok".to_string()), "{url}");
    }
}
