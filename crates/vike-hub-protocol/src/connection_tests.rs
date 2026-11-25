use std::net::TcpListener;

use serde_json::{Value, json};
use vike_utility::errors::FailureOutcome;

use super::*;
use crate::frame::read_frame;
use crate::plane::DATA_PLANE_SENTINEL;

const PROTO_VERSION: u32 = 7;

/// The refusal names the daemon that answered, what a Write session would have opened there, and where
/// the wanted daemon is configured, and never a key.
#[test]
fn the_wrong_welcome_refusal_says_which_daemon_answered_and_where_to_point() {
    let data = vec![DATA_PLANE_SENTINEL.to_string()];
    let msg = wrong_welcome_message(
        "127.0.0.1:17878",
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

/// A listener answering every dial's `Hello` with a `Welcome` of `version` naming `features`, then
/// recording whatever the client sent after it: the bytes after the Hello, one entry per dial.
fn welcome_server(
    version: u32,
    features: Vec<String>,
    dials: usize,
) -> (std::net::SocketAddr, std::thread::JoinHandle<Vec<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    let peer = std::thread::spawn(move || {
        (0..dials)
            .map(|_| {
                let (mut s, _) = listener.accept().expect("accept");
                let _: Value = read_frame(&mut s).expect("the Hello");
                let nonce = [7u8; 32];
                let welcome = json!({"Welcome": {
                    "proto_version": version, "features": features, "nonce": nonce,
                }});
                write_frame(&mut s, &welcome).expect("the Welcome");
                s.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
                let mut after = Vec::new();
                let _ = std::io::Read::read_to_end(&mut s, &mut after);
                after.len()
            })
            .collect()
    });
    (addr, peer)
}

/// ⚠ **The daemon is identified BEFORE the version is compared.** A client of the backtest plane
/// meeting the DATA daemon at another version reads which daemon answered and where to point, as a
/// [`WrongDaemon`] the caller can act on, never a bare version skew; the guarded keyed dial reads its
/// plane refusal first too. Nothing is sent after the Hello on either.
#[test]
fn a_client_meeting_the_other_planes_daemon_reads_which_daemon_answered_first() {
    let data = vec![DATA_PLANE_SENTINEL.to_string(), FEATURE_AUTH.to_string()];
    let (addr, peer) = welcome_server(PROTO_VERSION - 1, data, 3);

    let plain = HubConnection::connect(addr, Plane::Backtest, PROTO_VERSION).expect_err("wrong");
    assert!(matches!(plain, HubCallError::Protocol(_)), "{plain:?}");
    let wrong = WrongDaemon::of(&plain).expect("a WrongDaemon rides the error");
    assert_eq!((wrong.found, wrong.wanted), (Plane::Data, Plane::Backtest));
    let words = plain.to_string();
    assert!(words.contains("DATA daemon") && words.contains("config.backtest_addr"), "{words}");
    assert!(!words.contains("version mismatch"), "the daemon first: {words}");

    let keys = HubKeys::new(b"o".to_vec(), b"c".to_vec());
    let unguarded = HubConnection::connect_authed(
        addr,
        &keys,
        Scope::Write,
        crate::auth::BACKTESTHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect_err("its domain says which daemon it signs for");
    assert!(WrongDaemon::of(&unguarded).is_some(), "{unguarded}");

    let guarded = HubConnection::connect_authed_on(
        addr,
        &keys,
        Scope::Write,
        Plane::Backtest,
        crate::auth::BACKTESTHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect_err("the guard refuses the DATA daemon");
    let words = guarded.to_string();
    assert!(words.contains("refusing to authenticate") && words.contains("DATA daemon"), "{words}");
    assert!(!words.contains("version mismatch"), "the plane first: {words}");

    assert_eq!(peer.join().expect("peer thread"), [0, 0, 0], "nothing after any Hello");
}

/// A client meeting its OWN plane's daemon at another version still reads the version skew, naming
/// both numbers; no `WrongDaemon` rides it.
#[test]
fn a_client_meeting_its_own_daemon_at_another_version_reads_the_skew() {
    let (addr, peer) = welcome_server(PROTO_VERSION - 1, vec![DATA_PLANE_SENTINEL.to_string()], 1);
    let err = HubConnection::connect(addr, Plane::Data, PROTO_VERSION).expect_err("a skew");
    assert!(WrongDaemon::of(&err).is_none());
    assert!(err.to_string().starts_with("datahub protocol version mismatch"), "{err}");
    peer.join().expect("peer thread");
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
/// client-sized one none, and a caller's `set_reply_timeout`
/// replaces the table for EVERY verb until it is cleared. The gatewayhub's two classes no data-plane
/// verb takes: a lookup is server-bounded here, a progress-framed reply unbounded.
#[test]
fn the_reply_bound_is_the_verb_class_until_the_caller_names_one() {
    assert_eq!(ReplyWait::Bounded.default_bound(), Some(REPLY_IDLE_TIMEOUT));
    assert_eq!(ReplyWait::Unbounded.default_bound(), None, "a client-sized verb stays unbounded");
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
        let err =
            HubConnection::connect(addr, Plane::Data, PROTO_VERSION).expect_err("not a Welcome");
        assert!(matches!(err, HubCallError::Protocol(_)), "{err:?}");
        assert_eq!(err.to_string(), format!("hub handshake: expected Welcome, got {got}"));
    }
    peer.join().expect("peer thread");
}

/// **Every error names the hub whose `Welcome` it read**, whichever client dialled: the plane sentinel
/// is the only input, over a real socket. A `Welcome` naming no single plane is "hub".
#[test]
fn every_error_names_the_hub_its_welcome_stated() {
    const NONCE: [u8; 32] = [7; 32];
    // Each dial is a client of the plane its Welcome names, so a skew reads as a skew.
    for (sentinel, hub, plane) in [
        (crate::plane::BACKTEST_PLANE_SENTINEL, "backtesthub", Plane::Backtest),
        (DATA_PLANE_SENTINEL, "datahub", Plane::Data),
        ("neither", "hub", Plane::Data),
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

        let no_key = HubConnection::connect(addr, plane, PROTO_VERSION).expect_err("a keyed hub");
        assert!(matches!(no_key, HubCallError::AccessDenied(_)), "{no_key:?}");
        assert!(
            no_key
                .to_string()
                .starts_with(&format!("error [hub.access.denied]: this {hub} REQUIRES")),
            "{no_key}"
        );
        // A loopback hub: this box's own first, `mint-missing` before the tunnel's `connect`.
        let said = no_key.to_string();
        let (mint, connect) = (said.find("backend mint-missing"), said.find("backend connect"));
        assert!(mint.is_some() && mint < connect, "{no_key}");

        let skew = HubConnection::connect(addr, plane, PROTO_VERSION).expect_err("another version");
        assert!(matches!(skew, HubCallError::Protocol(_)), "{skew:?}");
        assert!(skew.to_string().starts_with(&format!("{hub} protocol version")), "{skew}");

        let keys = HubKeys::new(b"o".to_vec(), b"c".to_vec());
        let denied = HubConnection::connect_authed(
            addr,
            &keys,
            Scope::Read,
            crate::auth::DATAHUB_DOMAIN,
            PROTO_VERSION,
        )
        .expect_err("a denied key");
        assert!(matches!(denied, HubCallError::AccessDenied(_)), "{denied:?}");
        assert_eq!(
            denied.to_string(),
            format!("error [hub.access.denied]: {hub} auth denied (Read): no"),
            "a real AuthDenied, shown under the client's one id"
        );

        let mut client =
            HubConnection::connect(addr, plane, PROTO_VERSION).expect("a key-less hub");
        let silent = Some(Duration::from_millis(50));
        let stall = client.exchange_frame::<_, Value>("Ping", silent, &"Ping").unwrap_err();
        assert!(matches!(stall, HubCallError::OutcomeUnknown(_)), "{stall:?}");
        assert!(stall.to_string().starts_with(&format!("{hub} Ping: no reply byte")), "{stall}");
        let later = client.exchange_frame::<_, Value>("Ping", silent, &"Ping").unwrap_err();
        assert!(matches!(later, HubCallError::NotSent(_)), "{later:?}");
        assert!(later.to_string().starts_with(&format!("{hub} Ping: not sent")), "{later}");
        drop(client);
        peer.join().expect("peer thread");
    }
}

/// **A dial nobody accepted never left**, and a request this client will not encode is refused before its
/// first byte and keeps the connection: the next verb on it is answered.
#[test]
fn a_refused_dial_is_not_sent_and_an_unencodable_request_keeps_the_connection() {
    let gone = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = gone.local_addr().expect("local_addr");
    drop(gone);
    let refused = HubConnection::connect(addr, Plane::Data, PROTO_VERSION).expect_err("nobody");
    assert!(matches!(refused, HubCallError::NotSent(_)), "{refused:?}");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let stream = connect_bounded(listener.local_addr().expect("addr")).expect("connect");
    let mut client = HubConnection::on_stream(stream);
    client.arm_request_timeouts().expect("request phase");
    let (mut server_side, _) = listener.accept().expect("accept");
    // JSON has no map key but a string, so this body never serializes.
    let unencodable = std::collections::BTreeMap::from([(vec![1u8], 1u8)]);
    let err = client.exchange_frame::<_, Value>("Odd", None, &unencodable).unwrap_err();
    assert!(matches!(err, HubCallError::Protocol(_)), "{err:?}");
    assert!(client.is_usable(), "nothing reached the wire: {err}");
    let answer = std::thread::spawn(move || {
        let _: Value = read_frame(&mut server_side).expect("the Ping is the first frame");
        write_frame(&mut server_side, &json!("Pong")).expect("answer it");
    });
    let pong: Value = client.exchange_frame("Ping", None, &json!("Ping")).expect("answered");
    assert_eq!(pong, json!("Pong"));
    answer.join().expect("server thread");
}

/// **The no-key cure follows where the hub is**: a loopback hub names `backend mint-missing` (this
/// box's own hubs) before `backend connect` (a tunnel), and a remote one names `backend connect` alone.
#[test]
fn the_no_key_cure_names_mint_missing_first_on_loopback_and_connect_alone_off_box() {
    let local = no_device_key_cure(true);
    let (mint, connect) = (local.find("backend mint-missing"), local.find("backend connect"));
    assert!(mint.is_some() && mint < connect, "{local}");
    let remote = no_device_key_cure(false);
    assert!(remote.contains("backend connect") && !remote.contains("mint-missing"), "{remote}");
    for cure in [local, remote] {
        assert!(cure.contains("vike-cli access ls"), "both name the list: {cure}");
    }
}

/// An address names THIS box when it is a loopback IP (both families), the unspecified address (a dial
/// reaches it as this box), or a well-known loopback name; any other name or IP is another box, and
/// nothing is resolved to decide it.
#[test]
fn an_address_names_this_box_by_its_text_alone() {
    for here in [
        "127.0.0.1:9301",
        "127.9.9.9:1",
        "[::1]:9302",
        "0.0.0.0:9304",
        "[::]:9305",
        "localhost:9303",
        "LOCALHOST:1",
        "localhost.localdomain:9306",
        "ip6-localhost:9307",
        "ip6-loopback:9308",
    ] {
        assert!(names_this_box(here), "{here}");
    }
    for there in ["<host>:7879", "[2001:db8::1]:7879", "the CI box.lan:7879", "localhost", ""] {
        assert!(!names_this_box(there), "{there}");
    }
}

/// **A hub that ANSWERED is `Refused`**, and an older hub a device key cannot answer is one of them:
/// `auth_answer` names the feature it lacks AFTER that hub's `Welcome` (no `person-auth`), which the yes/no
/// this replaced read as an unreachable hub (`data catalog venues` showed NOT REACHED, `research study`
/// exited "retry"). Through `io::Result` it keeps its `Unsupported`.
#[test]
fn an_unsupported_auth_answer_after_the_welcome_reads_as_refused() {
    let (device, _) = crate::access::DeviceKey::generate().expect("generate");
    let older_hub = auth_answer(
        Signer::Device(&device),
        DATAHUB_DOMAIN,
        &[0; 32],
        PROTO_VERSION,
        Scope::Read,
        &[FEATURE_AUTH.to_string()],
    )
    .expect_err("an older hub cannot take a device key");
    assert!(
        matches!(&older_hub, HubCallError::NotOffered { feature, .. } if feature == FEATURE_PERSON_AUTH),
        "{older_hub:?}"
    );
    assert_eq!(older_hub.failure_outcome(), FailureOutcome::Refused, "{older_hub}");
    let older_hub = io::Error::from(older_hub);
    assert_eq!(older_hub.kind(), io::ErrorKind::Unsupported, "{older_hub}");
}
