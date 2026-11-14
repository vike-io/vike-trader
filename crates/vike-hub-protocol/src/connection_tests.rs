use std::net::TcpListener;

use serde_json::{Value, json};

use super::*;
use crate::plane::DATA_PLANE_SENTINEL;

const PROTO_VERSION: u32 = 7;

/// The refusal names the daemon that answered, what a Write session would have opened there, and where
/// the wanted daemon is configured, and never a key.
#[test]
fn the_wrong_welcome_refusal_says_which_daemon_answered_and_where_to_point() {
    let data = vec![DATA_PLANE_SENTINEL.to_string()];
    let msg = wrong_welcome_message(
        "127.0.0.1:7878",
        Plane::Backtest,
        Some(Plane::Data),
        Scope::Write,
        &data,
    );
    assert!(msg.contains("DATA daemon"), "names what answered: {msg}");
    assert!(msg.contains("DeleteSeries"), "says what the key would have opened: {msg}");
    assert!(msg.contains("config.backtest_addr"), "says where to point: {msg}");
    assert!(msg.contains("Nothing was sent"), "says nothing leaked: {msg}");

    let unknown = wrong_welcome_message("h:1", Plane::Backtest, None, Scope::Write, &[]);
    assert!(unknown.contains("no single plane"), "{unknown}");
}

#[test]
fn check_proto_version_accepts_a_matching_server() {
    assert!(check_proto_version("hub", PROTO_VERSION, PROTO_VERSION).is_ok());
}

/// A mismatched server version yields an error naming BOTH the client's and the server's number, so a
/// stale binary reports something a human can act on.
#[test]
fn check_proto_version_flags_a_mismatch_naming_both() {
    let server_version = PROTO_VERSION + 1;
    let err = check_proto_version("hub", PROTO_VERSION, server_version).unwrap_err();
    assert!(err.contains(&PROTO_VERSION.to_string()), "names the client version: {err}");
    assert!(err.contains(&server_version.to_string()), "names the server version: {err}");
}

/// `arm_request_timeouts` performs the phase swap: the handshake's 10 s read bound must NOT survive it
/// (it would abort a legitimate long `RunParamscanProfile`), because each verb arms its own in
/// `exchange_frame`.
#[test]
fn arm_request_timeouts_clears_the_read_and_bounds_the_write() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    let stream = connect_bounded(addr).expect("connect to a bound loopback listener");
    let client = HubConnection::on_stream(stream);

    client.arm_request_timeouts().expect("arm the request-phase timeouts");

    assert_eq!(
        client.stream.read_timeout().expect("read_timeout"),
        None,
        "the handshake's bound must not leak into the request phase; each verb arms its own"
    );
    assert_eq!(client.stream.write_timeout().expect("write_timeout"), Some(REQUEST_WRITE_TIMEOUT));
}

/// The default table follows the class: a server-bounded verb gets [`REPLY_IDLE_TIMEOUT`], a
/// client-sized one none, a handshake frame the handshake's bound, and a caller's `set_reply_timeout`
/// replaces the table for EVERY verb until it is cleared. The gatewayhub's two classes no data-plane
/// verb takes: a lookup is server-bounded here, a progress-framed reply unbounded.
#[test]
fn the_reply_bound_is_the_verb_class_until_the_caller_names_one() {
    assert_eq!(ReplyWait::Bounded.default_bound(), Some(REPLY_IDLE_TIMEOUT));
    assert_eq!(ReplyWait::Unbounded.default_bound(), None, "a client-sized verb stays unbounded");
    assert_eq!(ReplyWait::Handshake.default_bound(), Some(CLIENT_HANDSHAKE_DEADLINE));
    assert_eq!(ReplyWait::Lookup.default_bound(), Some(REPLY_IDLE_TIMEOUT));
    assert_eq!(ReplyWait::Progress.default_bound(), None);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let stream = connect_bounded(listener.local_addr().expect("addr")).expect("connect");
    let mut client = HubConnection::on_stream(stream);
    let short = Duration::from_millis(250);
    client.set_reply_timeout(Some(short));
    assert_eq!(client.reply_bound(ReplyWait::Bounded.default_bound()), Some(short));
    assert_eq!(
        client.reply_bound(ReplyWait::Unbounded.default_bound()),
        Some(short),
        "the override covers all"
    );
    client.set_reply_timeout(None);
    assert_eq!(
        client.reply_bound(ReplyWait::Unbounded.default_bound()),
        None,
        "and clearing it restores the table"
    );
}

/// The exchange ARMS the verb's bound on the socket before it writes, so the bound a verb gets is the
/// one [`HubConnection::reply_bound`] reports, not whatever the last verb left.
#[test]
fn an_exchange_arms_its_verbs_bound_on_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let stream = connect_bounded(listener.local_addr().expect("addr")).expect("connect");
    let mut client = HubConnection::on_stream(stream);
    client.arm_request_timeouts().expect("request phase");
    let (mut server_side, _) = listener.accept().expect("accept");
    let answer = std::thread::spawn(move || {
        let _: Value = read_frame(&mut server_side).expect("the Ping arrives");
        write_frame(&mut server_side, &json!("Pong")).expect("answer it");
        server_side
    });
    let pong: Value = client
        .exchange_frame("Ping", ReplyWait::Bounded.default_bound(), &json!("Ping"))
        .expect("answered");
    assert_eq!(pong, json!("Pong"));
    let _server_side = answer.join().expect("server thread");
    assert_eq!(client.stream.read_timeout().expect("read_timeout"), Some(REPLY_IDLE_TIMEOUT));
    assert!(client.is_usable(), "an answered exchange leaves the client in step");
}

/// `cancel_handle` closes the connection from ANOTHER thread while the client is blocked in a verb's
/// reply read: the verb returns an error instead of waiting forever, and the server reads end-of-stream,
/// the "client gone" a backtesthub watches for. A second `shutdown` is harmless.
#[test]
fn a_cancel_handle_unblocks_the_verb_and_the_server_reads_end_of_stream() {
    use std::io::Read;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    let stream = connect_bounded(addr).expect("connect to a bound loopback listener");
    let mut client = HubConnection::on_stream(stream);
    client.arm_request_timeouts().expect("arm the request-phase timeouts");
    let (mut server_side, _) = listener.accept().expect("accept");
    let handle = client.cancel_handle().expect("clone the socket");

    // The verb writes its frame and then blocks: this server never answers.
    let blocked = std::thread::spawn(move || {
        client.exchange_frame::<_, Value>("Ping", None, &json!("Ping")).map(|_| ())
    });
    let mut frame = [0u8; 4];
    server_side.read_exact(&mut frame).expect("the Ping frame's length prefix arrives");

    handle.shutdown();
    let outcome = blocked.join().expect("the verb thread does not panic");
    assert!(outcome.is_err(), "the blocked verb fails once the connection is shut: {outcome:?}");

    let mut rest = Vec::new();
    server_side.read_to_end(&mut rest).expect("the server reads to end-of-stream");
    handle.shutdown();
}

/// A peer that answers the `Hello` with some other verb's reply is named by its wire tag, a word the
/// handshake's own enum does not carry; garbage stays `InvalidData`.
#[test]
fn a_reply_that_is_not_the_welcome_is_named_by_its_tag() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    let peer = std::thread::spawn(move || {
        for reply in [json!({"Error": "no"}), json!("Pong")] {
            let (mut s, _) = listener.accept().expect("accept");
            let _: Value = read_frame(&mut s).expect("the Hello");
            write_frame(&mut s, &reply).expect("answer");
        }
    });
    for got in ["Error", "Pong"] {
        let err = HubConnection::connect(addr, PROTO_VERSION).expect_err("not a Welcome");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), format!("hub handshake: expected Welcome, got {got}"));
    }
    peer.join().expect("peer thread");
}

/// **Every error names the hub whose `Welcome` it read**, whichever client dialled: the plane sentinel
/// is the only input, over a real socket. A `Welcome` naming no single plane is "hub".
#[test]
fn every_error_names_the_hub_its_welcome_stated() {
    const NONCE: [u8; 32] = [7; 32];
    for (sentinel, hub) in [
        (crate::plane::BACKTEST_PLANE_SENTINEL, "backtesthub"),
        (DATA_PLANE_SENTINEL, "datahub"),
        ("neither", "hub"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        // Each dial below in turn: a keyed Welcome, one under another version, a denied Auth, and a
        // key-less one that then answers nothing.
        let peer = std::thread::spawn(move || {
            let keyed = json!({"Welcome": {
                "proto_version": PROTO_VERSION, "features": [sentinel, "auth"], "nonce": NONCE,
            }});
            let skewed = json!({"Welcome": {
                "proto_version": PROTO_VERSION + 1, "features": [sentinel],
            }});
            let keyless =
                json!({"Welcome": { "proto_version": PROTO_VERSION, "features": [sentinel] }});
            for welcome in [&keyed, &skewed, &keyed, &keyless] {
                let (mut s, _) = listener.accept().expect("accept");
                let _: Value = read_frame(&mut s).expect("the Hello");
                write_frame(&mut s, welcome).expect("the Welcome");
                if std::ptr::eq(welcome, &keyed) {
                    // Past the plain dial's refusal this read ends on the client's close.
                    if read_frame::<_, Value>(&mut s).is_ok() {
                        write_frame(&mut s, &json!({"AuthDenied": {"reason": "no"}}))
                            .expect("deny");
                    }
                } else if std::ptr::eq(welcome, &keyless) {
                    // Hold the socket open and silent until the client's two verbs are over.
                    let _ = read_frame::<_, Value>(&mut s);
                    let _ = read_frame::<_, Value>(&mut s);
                }
            }
        });

        let no_key = HubConnection::connect(addr, PROTO_VERSION).expect_err("a keyed hub");
        assert!(no_key.to_string().starts_with(&format!("this {hub} REQUIRES")), "{no_key}");
        assert_eq!(
            no_key.to_string().contains("the pair it verifies"),
            hub != "datahub",
            "{no_key}"
        );

        let skew = HubConnection::connect(addr, PROTO_VERSION).expect_err("another version");
        assert!(skew.to_string().starts_with(&format!("{hub} protocol version")), "{skew}");

        let keys = HubKeys::new(b"o".to_vec(), b"c".to_vec());
        let denied = HubConnection::connect_authed(addr, &keys, Scope::Read, PROTO_VERSION)
            .expect_err("a denied key");
        assert_eq!(denied.to_string(), format!("{hub} auth denied (Read): no"));

        let mut client = HubConnection::connect(addr, PROTO_VERSION).expect("a key-less hub");
        let silent = Some(Duration::from_millis(50));
        let stall = client.exchange_frame::<_, Value>("Ping", silent, &"Ping").unwrap_err();
        assert!(stall.to_string().starts_with(&format!("{hub} Ping: no reply byte")), "{stall}");
        let later = client.exchange_frame::<_, Value>("Ping", silent, &"Ping").unwrap_err();
        assert!(later.to_string().starts_with(&format!("{hub} Ping: not sent")), "{later}");
        drop(client);
        peer.join().expect("peer thread");
    }
}
