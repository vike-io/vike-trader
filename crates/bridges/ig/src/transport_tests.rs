use super::*;

/// A REAL `GET /session/encryptionKey` body, captured from `demo-api.ig.com` on 2026-08-09.
/// The `encryptionKey` value — the RSA public key IG returns for password encryption, which
/// this parser never reads — is replaced by a placeholder, the same redact-the-leaf-keep-the-
/// shape idiom as `vike_bridge_core::capture`'s `capture_frame`.
const CAPTURED: &str = include_str!("../tests/fixtures/session_encryption_key.json");

#[test]
fn the_captured_encryption_key_body_yields_igs_clock() {
    let body: serde_json::Value =
        serde_json::from_str(CAPTURED).expect("the capture is valid JSON");
    assert_eq!(parse_server_time_ms(&body, 200), Ok(1_786_242_762_192));
}

/// The UNIT trap, at the one venue whose stamp is a bare integer with no sibling to compare
/// against: a seconds-valued reading is off by a factor of a thousand and still looks like a
/// plausible epoch. The captured stamp is asserted to be MILLISECONDS by its magnitude.
#[test]
fn the_captured_stamp_is_milliseconds_not_seconds() {
    let body: serde_json::Value = serde_json::from_str(CAPTURED).expect("valid JSON");
    let ms = parse_server_time_ms(&body, 200).expect("the capture carries timeStamp");
    // 2001-09-09T01:46:40Z in ms; any seconds-valued stamp for this century is far below it.
    assert!(ms > 1_000_000_000_000, "a seconds-valued stamp would be a thousandfold out: {ms}");
    assert!(ms < 10_000_000_000_000, "a µs/ns-valued stamp would be far above this: {ms}");
}

// The 401 ladder's own properties (one retry, one login, the sibling guard, a transport error never
// a trigger) are the shared ladder's: `crates/vike-bridge-core/src/transport/auth_tests.rs`, written from this
// file's tests.

/// A 2xx body that carries no stamp is an ERROR naming the field, never a zero.
#[test]
fn a_body_without_the_stamp_is_an_error_naming_the_field() {
    let e = parse_server_time_ms(&serde_json::json!({"encryptionKey": "x"}), 200)
        .expect_err("no timeStamp");
    assert_eq!(e.status, 200);
    assert!(e.message.contains("timeStamp"), "{}", e.message);
}

/// **A logout to a gateway that never answers is over in seconds, not thirty.** The history lane logs its
/// session out at a clean shutdown, and the gatewayhub's unit gives its stop 35 seconds: the shared
/// agent's 30 second ceiling would eat nearly all of it. The logout runs on its own short agent.
#[test]
fn a_logout_to_a_server_that_never_answers_is_over_in_seconds() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("its address").port();
    // Accepts the connection and then says nothing, for longer than the bound under test.
    let hold = std::thread::spawn(move || {
        let _held = listener.accept().expect("the logout connects");
        std::thread::sleep(std::time::Duration::from_secs(12));
    });
    let session = IgSession {
        agent: IgSession::agent(),
        confirm_agent: IgSession::confirm_agent(),
        api_key: "k".to_string(),
        base: format!("http://127.0.0.1:{port}"),
        config: crate::config::IgConfig {
            api_key: vike_model::secret::Secret::new("k".to_string()),
            identifier: vike_model::secret::Secret::new("i".to_string()),
            password: vike_model::secret::Secret::new("p".to_string()),
            rest_base: format!("http://127.0.0.1:{port}"),
            account_pin: None,
            fx_clock: None,
        },
        tokens: TokenCell::new(("c".to_string(), "x".to_string())),
        account_id: "ABC12".to_string(),
        lightstreamer_endpoint: String::new(),
        account_currency: None,
        accounts: None,
        link: std::sync::OnceLock::new(),
        relogin_failed: std::sync::atomic::AtomicBool::new(false),
    };
    let started = std::time::Instant::now();

    session.logout();

    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(6), "the logout waited {took:?}");
    assert!(
        took >= std::time::Duration::from_secs(2),
        "it gave the server its few seconds: {took:?}"
    );
    drop(hold);
}

/// A send failure PROVABLY before the write (DNS, connect) is `status 0` = a clean `OrderRejected`
/// upstream; a timeout may have followed the write and is the unknown-outcome status.
#[test]
fn an_order_send_failure_before_the_write_is_definite_and_a_timeout_is_not() {
    let before = IgSession::order_send_err(ureq::Error::HostNotFound);
    assert_eq!(before.status, 0);
    assert!(!before.outcome_unknown());
    let after = IgSession::order_send_err(ureq::Error::Timeout(ureq::Timeout::Global));
    assert_eq!(after.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
    assert!(after.outcome_unknown());
}

/// **A 3xx, a 5xx or an unreadable 2xx to an order write is no verdict on the order** (owner rulings B-3, B-1 and
/// B-2): no IG agent follows a redirect, nothing says IG did not act before a 5xx, and a 2xx says IG took something,
/// so the answer may stand in front of an order IG took; it reads outcome-unknown, so a submit asks `/confirms` and
/// never rejects. A read keeps the status.
#[test]
fn a_redirect_a_5xx_or_an_unreadable_2xx_on_an_order_write_is_outcome_unknown() {
    let finish_call = |status, text, write| IgSession::finish_call(status, text, write, &[]);
    let moved = finish_call(302, "", true).expect_err("a 3xx is no answer");
    assert_eq!(moved.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
    assert!(moved.outcome_unknown());
    assert_eq!(finish_call(302, "", false).expect_err("a read").status, 302);
    let failed = finish_call(500, r#"{"errorCode":"error.x"}"#, true).expect_err("a 500");
    assert_eq!(failed.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "{failed}");
    assert_eq!(failed.message, "http 500, outcome unknown: error.x");
    let read = finish_call(500, r#"{"errorCode":"error.x"}"#, false).expect_err("a read");
    assert_eq!((read.status, read.message.as_str()), (500, "error.x"));
    let unread = finish_call(200, "not json", true).expect_err("bad json");
    assert_eq!(unread.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "{unread}");
    assert!(unread.message.starts_with("bad json: "), "{unread}");
    assert_eq!(finish_call(200, "not json", false).expect_err("a read").status, 200);
    // The session's own values are redacted from IG's words, the rest of them kept.
    let echoed = IgSession::finish_call(
        400,
        r#"{"errorCode":"error.request.invalid CST=cst-token-1"}"#,
        false,
        &["cst-token-1"],
    );
    assert_eq!(echoed.expect_err("a 400").message, "error.request.invalid CST=[redacted]");
}

/// A session whose calls go to `base` and whose (re-)login goes to `login_base`, holding a pair already.
fn session_at(base: String, login_base: String) -> IgSession {
    IgSession {
        agent: IgSession::agent(),
        confirm_agent: IgSession::confirm_agent(),
        api_key: "k".to_string(),
        base,
        config: crate::config::IgConfig {
            api_key: vike_model::secret::Secret::new("k".to_string()),
            identifier: vike_model::secret::Secret::new("i".to_string()),
            password: vike_model::secret::Secret::new("p".to_string()),
            rest_base: login_base,
            account_pin: None,
            fx_clock: None,
        },
        tokens: TokenCell::new(("c".to_string(), "x".to_string())),
        account_id: "ABC12".to_string(),
        lightstreamer_endpoint: String::new(),
        account_currency: None,
        accounts: None,
        link: std::sync::OnceLock::new(),
        relogin_failed: std::sync::atomic::AtomicBool::new(false),
    }
}

/// A loopback server that reads ONE request's head, writes `reply` and closes: its base URL, and the server thread,
/// which answers the request line it read.
fn answer_once(reply: &'static [u8]) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let base = format!("http://{}", listener.local_addr().expect("its address"));
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the call arrives");
        let (mut head, mut chunk) = (Vec::new(), [0_u8; 1024]);
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            head.extend_from_slice(&chunk[..n]);
        }
        let _ = sock.write_all(reply);
        String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string()
    });
    (base, server)
}

/// A cancel whose reply headers arrived and whose body broke off read as `status 0`, "provably never sent", while IG
/// had received it (T-D5): the outcome is unknown, as alpaca's DELETE says.
#[test]
fn a_cancel_whose_reply_body_breaks_off_is_an_unknown_outcome() {
    let (base, server) =
        answer_once(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n{\"dealReference");
    let e = session_at(base.clone(), base)
        .delete("/workingorders/otc/DIAAAA1", "2")
        .expect_err("the body breaks off");
    assert!(server.join().unwrap().starts_with("DELETE "), "the cancel was sent");
    assert!(e.outcome_unknown(), "{e}");
    assert!(e.message.starts_with("network error reading the reply"), "{e}");
}

/// A 401 whose re-login FAILS answers the LOGIN's error (T-D6), as alpaca's mint does: a login host that never
/// answered is a network failure, and it used to come back as IG's 401, which reads as refused keys. Nothing is sent
/// again.
#[test]
fn a_401_whose_re_login_fails_answers_the_logins_own_error() {
    let (base, server) =
        answer_once(b"HTTP/1.1 401 X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    // A port nothing listens on: the login is never answered.
    let login_base = {
        let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        format!("http://{}", gone.local_addr().expect("its address"))
    };
    let session = session_at(base, login_base);
    let (link, cell) = (
        crate::account::venue_link::SessionLink::default(),
        vike_bridge_core::venue_link::VenueLink::default(),
    );
    link.claim(&cell);
    session.report_to(link);
    let e = session.get("/accounts", "1", "").expect_err("no session");
    assert!(server.join().unwrap().starts_with("GET "), "the call was sent once");
    assert_eq!(e.status, 0, "the login's own error, not the 401: {e}");
    assert!(e.message.starts_with("network error: "), "{e}");
    // The status line says the LOGIN failed, by its kind (IG answered the call, with the 401) …
    let vike_bridge_core::venue_link::VenueLinkState::Down(down) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert!(
        down.why.starts_with(
            "the order session: IG's login could not be reached for a re-login (network error: "
        ),
        "{}",
        down.why
    );
    // … and the history cache's lease sees a session that cannot recover on its own.
    assert!(session.relogin_failed(), "a failed re-login of any kind marks the session");
}
