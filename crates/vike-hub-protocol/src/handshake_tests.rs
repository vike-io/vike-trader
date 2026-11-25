//! `run_keyed_handshake` over a real loopback socket, one refusal at a time: the bytes the client
//! reads back, the notes the daemon is told, and the outcome. The datahub's, the backtesthub's and the
//! gatewayhub's copies were this function three times; a mutant that reorders a refusal, changes a
//! reason or drops a note goes red here. Each [`HandshakeSpec`] term a daemon sets (the domain, the
//! plane, the reply ceiling) and the served-scope check have a test of their own, and the last test
//! holds the phase deadline against a peer that trickles.

use std::io::Write as _;
use std::net::TcpListener;

use super::*;
use crate::auth::{BACKTESTHUB_DOMAIN, DATAHUB_DOMAIN, sign};
use crate::frame::{MAX_FRAME_LEN, read_frame};

const READ_KEY: &[u8] = b"handshake-test-read-key";
const WRITE_KEY: &[u8] = b"handshake-test-write-key";
const PROTO_VERSION: u32 = 7;
/// Production's deadline: every client here answers at once, so it never fires.
const DEADLINE: Duration = crate::limits::SERVER_HANDSHAKE_DEADLINE;
/// A domain separator that is not the data plane's, for the daemon on no data plane.
const OTHER_DOMAIN: Domain = Domain::new(b"vike-handshake-test-auth\0");

/// The terms a test daemon handshakes under: [`DATA_DAEMON`] unless the test says otherwise.
#[derive(Clone, Copy)]
struct Daemon {
    domain: Domain,
    plane: Option<Plane>,
    reply_ceiling: u32,
    admission: fn(Scope) -> Result<(), &'static str>,
}

fn every_scope(_: Scope) -> Result<(), &'static str> {
    Ok(())
}

/// The datahub's terms: its domain and plane, the frame codec's ceiling, every scope it has a key for.
const DATA_DAEMON: Daemon = Daemon {
    domain: DATAHUB_DOMAIN,
    plane: Some(Plane::Data),
    reply_ceiling: MAX_FRAME_LEN,
    admission: every_scope,
};

fn keys() -> HubKeys {
    HubKeys::new(READ_KEY.to_vec(), WRITE_KEY.to_vec())
}

/// A daemon's own request enum, as far as this suite needs one: one verb.
#[derive(Debug, Deserialize)]
enum OwnVerb {
    Ping,
}

fn verb_name(_: &OwnVerb) -> &'static str {
    "Ping"
}

/// `body` as one frame: its big-endian `u32` length, then the bytes (decodable or not).
fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

fn json(frame: &HandshakeFrame) -> Vec<u8> {
    serde_json::to_vec(frame).unwrap()
}

const PING: &[u8] = br#""Ping""#;

/// [`handshake_with`] the datahub's terms.
fn handshake(
    first: Vec<u8>,
    second: impl FnOnce([u8; 32]) -> Option<Vec<u8>> + Send + 'static,
) -> (HandshakeOutcome, Vec<HandshakeNote>, Vec<HandshakeReply>) {
    handshake_with(DATA_DAEMON, first, second)
}

/// One handshake with `daemon`: a client thread sends `first`, answers the server's `Welcome` (given
/// its nonce) with `second` when that is `Some`, and reads every reply until the server hangs up.
fn handshake_with(
    daemon: Daemon,
    first: Vec<u8>,
    second: impl FnOnce([u8; 32]) -> Option<Vec<u8>> + Send + 'static,
) -> (HandshakeOutcome, Vec<HandshakeNote>, Vec<HandshakeReply>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::thread::Builder::new()
        .name("handshake-test-client".into())
        .spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(&framed(&first)).unwrap();
            let mut second = Some(second);
            let mut replies = Vec::new();
            while let Ok(reply) = read_frame::<_, HandshakeReply>(&mut stream) {
                if let HandshakeReply::Welcome { nonce: Some(nonce), .. } = &reply
                    && let Some(body) = second.take().and_then(|answer| answer(*nonce))
                {
                    stream.write_all(&framed(&body)).unwrap();
                }
                replies.push(reply);
            }
            replies
        })
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let features = ["f".to_string()];
    let spec = HandshakeSpec {
        domain: daemon.domain,
        proto_version: PROTO_VERSION,
        plane: daemon.plane,
        features: &features,
        deadline: DEADLINE,
        reply_ceiling: daemon.reply_ceiling,
    };
    let mut notes = Vec::new();
    let outcome =
        run_keyed_handshake(&mut server, &keys(), spec, daemon.admission, None, verb_name, |n| {
            notes.push(n)
        });
    drop(server);
    (outcome, notes, client.join().unwrap())
}

fn hello(proto_version: u32) -> Vec<u8> {
    json(&HandshakeFrame::Hello { proto_version })
}

fn auth(scope: Scope, mac: Vec<u8>) -> Option<Vec<u8>> {
    Some(json(&HandshakeFrame::Auth { scope, mac }))
}

/// The refusal a reply carries, `None` for any other reply.
fn denied(reply: &HandshakeReply) -> Option<&str> {
    match reply {
        HandshakeReply::AuthDenied { reason } => Some(reason),
        _ => None,
    }
}

fn is_welcome(reply: &HandshakeReply) -> bool {
    matches!(
        reply,
        HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: Some(_) }
            if features == &["f".to_string()]
    )
}

#[test]
fn a_verb_before_hello_is_refused_by_name_and_noted() {
    let (outcome, notes, replies) = handshake(PING.to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("Ping")]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

/// ⚠ A verb of the OTHER plane gets the same words as one of this daemon's, and is noted under its own
/// wire tag, although this daemon's enum cannot decode it.
#[test]
fn a_verb_of_the_other_plane_before_hello_is_refused_the_same_way() {
    let (outcome, notes, replies) = handshake(br#"{"RunBacktest":"x"}"#.to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("RunBacktest")]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

#[test]
fn an_auth_before_hello_is_refused_as_a_verb() {
    let (outcome, notes, replies) = handshake(auth(Scope::Read, vec![0; 32]).unwrap(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("Auth")]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

#[test]
fn an_undecodable_first_frame_is_refused_unnoted() {
    let (outcome, notes, replies) = handshake(b"not json".to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("expected Hello (undecodable request)")]
    );
}

#[test]
fn a_second_frame_that_is_not_auth_is_refused_after_the_welcome() {
    for (second, reason) in [
        (PING.to_vec(), "expected Auth after Welcome"),
        (hello(PROTO_VERSION), "expected Auth after Welcome"),
        (br#"{"RunBacktest":"x"}"#.to_vec(), "expected Auth after Welcome"),
        (b"not json".to_vec(), "expected Auth (undecodable request)"),
    ] {
        let (outcome, notes, replies) = handshake(hello(PROTO_VERSION), move |_| Some(second));
        assert!(matches!(outcome, HandshakeOutcome::Closed));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(is_welcome(&replies[0]), "{replies:?}");
        assert_eq!(denied(&replies[1]), Some(reason));
    }
}

#[test]
fn a_scope_with_no_key_gets_the_bad_mac_reason_and_the_daemon_the_truth() {
    let (outcome, notes, replies) =
        handshake(hello(PROTO_VERSION), |_| auth(Scope::Trade, vec![0; 32]));
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::NoKey(Scope::Trade)]);
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert_eq!(denied(&replies[1]), Some("bad mac"));
}

#[test]
fn a_bad_mac_is_refused_and_a_version_skew_is_noted_first() {
    let (outcome, notes, replies) =
        handshake(hello(PROTO_VERSION + 1), |_| auth(Scope::Read, vec![0; 32]));
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(
        notes,
        [HandshakeNote::VersionDiffers(PROTO_VERSION + 1), HandshakeNote::BadMac(Scope::Read)]
    );
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert_eq!(denied(&replies[1]), Some("bad mac"));
}

#[test]
fn a_mac_signed_over_the_nonce_under_the_datahub_domain_authenticates() {
    let (outcome, notes, replies) = handshake(hello(PROTO_VERSION), |nonce| {
        auth(Scope::Write, sign(DATAHUB_DOMAIN, WRITE_KEY, &nonce, PROTO_VERSION, Scope::Write))
    });
    // The outcome carries the fingerprint of the key that verified, for the daemon's audit trail.
    let expected_id = keys().key_id(Scope::Write);
    assert!(expected_id.is_some());
    assert!(
        matches!(&outcome, HandshakeOutcome::Authed { scope: Scope::Write, key_id, person: None } if *key_id == expected_id),
        "{outcome:?}"
    );
    assert_eq!(notes, [HandshakeNote::Authenticated(Scope::Write)]);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert!(
        matches!(replies[1], HandshakeReply::AuthOk { scope: Scope::Write, .. }),
        "{replies:?}"
    );
}

/// A scope the daemon's ADMISSION refuses is refused before any key is consulted, in the daemon's own text: a
/// data plane's coarse `bad mac`, or the tradehub's sentence naming the scope (decision 0288: one handshake, the
/// texts a value of the daemon). A mac that WOULD verify does not matter.
#[test]
fn a_scope_the_daemon_does_not_admit_is_refused_in_its_own_words_although_its_mac_verifies() {
    for (admission, text) in [
        (
            (|scope| if scope == Scope::Read { Ok(()) } else { Err("bad mac") })
                as fn(Scope) -> Result<(), &'static str>,
            "bad mac",
        ),
        (
            |scope| {
                if scope == Scope::Write { Err("control disabled on this node") } else { Ok(()) }
            },
            "control disabled on this node",
        ),
    ] {
        let daemon = Daemon { admission, ..DATA_DAEMON };
        let (outcome, notes, replies) = handshake_with(daemon, hello(PROTO_VERSION), |nonce| {
            auth(Scope::Write, sign(DATAHUB_DOMAIN, WRITE_KEY, &nonce, PROTO_VERSION, Scope::Write))
        });
        assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
        assert_eq!(notes, [HandshakeNote::Refused(Scope::Write, text)]);
        assert!(is_welcome(&replies[0]), "{replies:?}");
        assert_eq!(denied(&replies[1]), Some(text));
    }
}

/// `Register` reaches the daemon's door with this connection's nonce and its answer goes out as it came; a
/// daemon with no door refuses it as any unexpected second frame (decision 0288 item 7.3).
#[test]
fn a_registration_reaches_only_a_door_and_its_answer_goes_out_verbatim() {
    let registration = Registration {
        invite: "0a1b2c3d".into(),
        proof: vec![1; 32],
        device: "macbook".into(),
        public_key: vec![2; 32],
        ssh_key: "ssh-ed25519 AAAA".into(),
    };
    let frame = json(&HandshakeFrame::Register(registration.clone()));
    let (outcome, _, replies) = handshake(hello(PROTO_VERSION), {
        let frame = frame.clone();
        move |_| Some(frame)
    });
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(denied(&replies[1]), Some("expected Auth after Welcome"));

    for answer in [
        Ok(Registered {
            person: "alice".into(),
            device: "macbook".into(),
            role: "observe".into(),
            key_id: "nk-0123456789abcdef".into(),
        }),
        Err("invitation refused"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = frame.clone();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(&framed(&hello(PROTO_VERSION))).unwrap();
            let welcome = read_frame::<_, HandshakeReply>(&mut stream).unwrap();
            let HandshakeReply::Welcome { nonce: Some(nonce), .. } = welcome else { panic!() };
            stream.write_all(&framed(&body)).unwrap();
            (nonce, read_frame::<_, HandshakeReply>(&mut stream).unwrap())
        });
        let (mut server, _) = listener.accept().unwrap();
        let seen = std::sync::Mutex::new(None);
        let door = |got: Registration, nonce: &[u8; 32]| {
            *seen.lock().unwrap() = Some((got, *nonce));
            answer.clone()
        };
        let spec = HandshakeSpec {
            domain: DATAHUB_DOMAIN,
            proto_version: PROTO_VERSION,
            plane: None,
            features: &[],
            deadline: DEADLINE,
            reply_ceiling: MAX_FRAME_LEN,
        };
        let outcome = run_keyed_handshake(
            &mut server,
            &keys(),
            spec,
            every_scope,
            Some(&door),
            verb_name,
            |_| {},
        );
        assert!(matches!(outcome, HandshakeOutcome::Closed));
        let (nonce, reply) = client.join().unwrap();
        let (got, door_nonce) = seen.lock().unwrap().take().expect("the door was asked");
        assert_eq!((got, door_nonce), (registration.clone(), nonce));
        match (answer, reply) {
            (Ok(granted), HandshakeReply::Registered(sent)) => assert_eq!(granted, sent),
            (Err(text), reply) => assert_eq!(denied(&reply), Some(text)),
            (_, reply) => panic!("{reply:?}"),
        }
    }
}

/// The mac is verified under the DAEMON's domain: a datahub-domain mac does not verify at a daemon
/// with another one, and one signed under that daemon's own does.
#[test]
fn the_mac_is_verified_under_the_daemons_own_domain() {
    let other = Daemon { domain: OTHER_DOMAIN, ..DATA_DAEMON };
    let (outcome, notes, _) = handshake_with(other, hello(PROTO_VERSION), |nonce| {
        auth(Scope::Read, sign(DATAHUB_DOMAIN, READ_KEY, &nonce, PROTO_VERSION, Scope::Read))
    });
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert_eq!(notes, [HandshakeNote::BadMac(Scope::Read)]);

    let (outcome, _, _) = handshake_with(other, hello(PROTO_VERSION), |nonce| {
        auth(Scope::Read, sign(OTHER_DOMAIN, READ_KEY, &nonce, PROTO_VERSION, Scope::Read))
    });
    assert!(matches!(outcome, HandshakeOutcome::Authed { scope: Scope::Read, .. }), "{outcome:?}");
}

/// A daemon on NO data plane (the gatewayhub) does not name a data-plane verb it cannot decode: the
/// frame is undecodable, before `Hello` and in place of `Auth` alike.
#[test]
fn a_daemon_on_no_data_plane_calls_a_data_plane_verb_undecodable() {
    let no_plane = Daemon { plane: None, ..DATA_DAEMON };
    let load_bars = br#"{"LoadBars":"x"}"#.to_vec();
    let (outcome, notes, replies) = handshake_with(no_plane, load_bars.clone(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("expected Hello (undecodable request)")]
    );

    let (_, notes, replies) =
        handshake_with(no_plane, hello(PROTO_VERSION), move |_| Some(load_bars));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(denied(&replies[1]), Some("expected Auth (undecodable request)"));
}

/// A pre-auth reply over the daemon's reply ceiling is not written at all: the `Welcome` here is
/// longer than 8 bytes, so the client reads nothing and the daemon closes.
#[test]
fn a_reply_over_the_reply_ceiling_is_not_written() {
    let tiny = Daemon { reply_ceiling: 8, ..DATA_DAEMON };
    let (outcome, notes, replies) = handshake_with(tiny, hello(PROTO_VERSION), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert!(notes.is_empty(), "{notes:?}");
    assert!(replies.is_empty(), "{replies:?}");
}

/// **The deadline is for the PHASE, not for each read.** A peer that sends a legal length prefix and
/// then one byte every 50 ms (well inside any per-read timeout) would hold a per-read bound for the
/// 50 s its 1,000 bytes take; it is cut off at the 300 ms deadline instead.
#[test]
fn a_peer_trickling_inside_every_read_timeout_is_cut_off_at_the_phase_deadline() {
    let deadline = Duration::from_millis(300);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let trickler = std::thread::Builder::new()
        .name("handshake-test-trickle".into())
        .spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            if stream.write_all(&1_000_u32.to_be_bytes()).is_err() {
                return;
            }
            for _ in 0..1_000 {
                if stream.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let spec = HandshakeSpec {
        domain: DATAHUB_DOMAIN,
        proto_version: PROTO_VERSION,
        plane: Some(Plane::Data),
        features: &[],
        deadline,
        reply_ceiling: MAX_FRAME_LEN,
    };
    let began = Instant::now();
    let outcome =
        run_keyed_handshake(&mut server, &keys(), spec, every_scope, None, verb_name, |_| {});
    let held = began.elapsed();
    drop(server);
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert!(
        held >= deadline / 2 && held < deadline * 5,
        "a peer trickling a byte per 50 ms was held {held:?}, not the {deadline:?} phase deadline"
    );
    trickler.join().unwrap();
}

/// A device signs `PersonAuth` and the access list decides (decision 0288): a listed device under its role is
/// authenticated as its person, a scope the daemon does not admit is refused in its words before the list is read, a
/// revoked row is `access denied`, and a hub that holds no list advertises no `person-auth`, so a device-only client
/// refuses locally having answered nothing.
#[test]
fn a_device_authenticates_by_the_access_list_and_is_refused_unadmitted_or_once_revoked() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::access::{AccessEntry, AccessList, DeviceKey, FEATURE_PERSON_AUTH};
    use crate::auth::FEATURE_AUTH;
    use crate::conn::HubConnection;

    let (device, _) = DeviceKey::generate().unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let listed = {
        let (device, revoked) = (device.clone(), Arc::clone(&revoked));
        AccessList::new(move |key_id| {
            (key_id == device.key_id()).then(|| AccessEntry {
                person: "alice".into(),
                device: "pc".into(),
                role: "control".into(),
                public_key: device.public_key().to_vec(),
                revoked: revoked.load(Ordering::SeqCst),
            })
        })
    };
    let serve_once = |server_keys: HubKeys| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let features = [FEATURE_AUTH.to_string()];
            let spec = HandshakeSpec {
                domain: DATAHUB_DOMAIN,
                proto_version: PROTO_VERSION,
                plane: None,
                features: &features,
                deadline: DEADLINE,
                reply_ceiling: MAX_FRAME_LEN,
            };
            let admission = |scope| match scope {
                Scope::Account => Err("bad mac"),
                _ => Ok(()),
            };
            run_keyed_handshake(&mut stream, &server_keys, spec, admission, None, verb_name, |_| {})
        });
        (addr, server)
    };
    let client_keys = HubKeys::of_device(device.clone());

    let (addr, server) = serve_once(keys().with_access(listed.clone()));
    let client = HubConnection::connect_authed(
        addr,
        &client_keys,
        Scope::Write,
        DATAHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect("a listed device authenticates under its role");
    assert_eq!(client.authenticated_scope(), Some(Scope::Write));
    assert!(client.features().iter().any(|f| f == FEATURE_PERSON_AUTH), "{:?}", client.features());
    match server.join().unwrap() {
        HandshakeOutcome::Authed { scope: Scope::Write, key_id, person: Some(person) } => {
            assert_eq!(key_id.as_deref(), Some(device.key_id().as_str()));
            assert_eq!((person.person.as_str(), person.device.as_str()), ("alice", "pc"));
        }
        other => panic!("{other:?}"),
    }

    // The data plane's admission refuses Account in its own words; a revoked row is `access denied`.
    for (scope, revoke, text) in
        [(Scope::Account, false, "bad mac"), (Scope::Read, true, "access denied")]
    {
        revoked.store(revoke, Ordering::SeqCst);
        let (addr, server) = serve_once(keys().with_access(listed.clone()));
        let err =
            HubConnection::connect_authed(addr, &client_keys, scope, DATAHUB_DOMAIN, PROTO_VERSION)
                .expect_err("not admitted, or revoked");
        assert!(matches!(err, crate::call::HubCallError::AccessDenied(_)), "{err:?}");
        assert!(err.to_string().contains(text), "{err}");
        assert!(matches!(server.join().unwrap(), HandshakeOutcome::Closed));
    }

    // A listed device's login signed for ANOTHER hub is refused here (decision 0288's amendment): the client signs
    // under the domain it is handed, the server verifies under its own.
    revoked.store(false, Ordering::SeqCst);
    let (addr, server) = serve_once(keys().with_access(listed.clone()));
    let err = HubConnection::connect_authed(
        addr,
        &client_keys,
        Scope::Write,
        BACKTESTHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect_err("a login signed for the backtesthub");
    assert!(err.to_string().contains("access denied"), "{err}");
    assert!(matches!(server.join().unwrap(), HandshakeOutcome::Closed));

    let (addr, server) = serve_once(keys());
    let err = HubConnection::connect_authed(
        addr,
        &client_keys,
        Scope::Read,
        DATAHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect_err("no list, no person-auth, no shared key");
    assert!(matches!(err, crate::call::HubCallError::NotOffered { .. }), "{err:?}");
    assert!(matches!(server.join().unwrap(), HandshakeOutcome::Closed));

    // An EMPTY list admits nobody (decision 0288's cutover: a hub always authenticates), and says how a box gets
    // its first device — even for a device that a lookup would have found.
    revoked.store(false, Ordering::SeqCst);
    let (addr, server) = serve_once(keys().with_access(listed.clone().with_held(|| false)));
    let err = HubConnection::connect_authed(
        addr,
        &client_keys,
        Scope::Read,
        DATAHUB_DOMAIN,
        PROTO_VERSION,
    )
    .expect_err("an empty list admits nobody");
    assert!(matches!(err, crate::call::HubCallError::AccessDenied(_)), "{err:?}");
    assert!(err.to_string().contains(crate::access::NO_DEVICE_REGISTERED), "{err}");
    assert!(matches!(server.join().unwrap(), HandshakeOutcome::Closed));
}

/// **A device is told the role its row holds** (the owner: "i dont see admin despite i am admin"): under each
/// hub's domain an admin, a control and an observe device each get their OWN row back in `AuthOk`, which the
/// client keeps ([`crate::conn::HubConnection::admitted`]); a shared key gets none.
#[test]
fn each_device_is_told_its_own_role_and_a_shared_key_nothing() {
    use crate::access::{AccessEntry, AccessList, DeviceKey};
    use crate::auth::FEATURE_AUTH;
    use crate::conn::HubConnection;

    let devices: Vec<(DeviceKey, &str)> = ["admin", "control", "observe"]
        .into_iter()
        .map(|role| (DeviceKey::generate().unwrap().0, role))
        .collect();
    let rows: Vec<(String, AccessEntry)> = devices
        .iter()
        .map(|(device, role)| {
            let entry = AccessEntry {
                person: "alice".into(),
                device: format!("pc-{role}"),
                role: (*role).into(),
                public_key: device.public_key().to_vec(),
                revoked: false,
            };
            (device.key_id(), entry)
        })
        .collect();
    let list = AccessList::new(move |key_id| {
        rows.iter().find(|(id, _)| id == key_id).map(|(_, entry)| entry.clone())
    });
    let serve_once = |domain: Domain, server_keys: HubKeys| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let features = [FEATURE_AUTH.to_string()];
            let spec = HandshakeSpec {
                domain,
                proto_version: PROTO_VERSION,
                plane: None,
                features: &features,
                deadline: DEADLINE,
                reply_ceiling: MAX_FRAME_LEN,
            };
            run_keyed_handshake(
                &mut stream,
                &server_keys,
                spec,
                every_scope,
                None,
                verb_name,
                |_| {},
            )
        });
        (addr, server)
    };
    for domain in [DATAHUB_DOMAIN, BACKTESTHUB_DOMAIN, OTHER_DOMAIN] {
        for (device, role) in &devices {
            let (addr, server) = serve_once(domain, keys().with_access(list.clone()));
            let client_keys = HubKeys::of_device(device.clone());
            let client = HubConnection::connect_authed(
                addr,
                &client_keys,
                Scope::Read,
                domain,
                PROTO_VERSION,
            )
            .expect("a listed device authenticates");
            let want = Admitted {
                person: "alice".into(),
                device: format!("pc-{role}"),
                role: (*role).into(),
            };
            assert_eq!(client.admitted(), Some(&want), "{role}");
            assert!(matches!(server.join().unwrap(), HandshakeOutcome::Authed { .. }));
        }
        let (addr, server) = serve_once(domain, keys());
        let client =
            HubConnection::connect_authed(addr, &keys(), Scope::Read, domain, PROTO_VERSION)
                .expect("a shared key authenticates");
        assert_eq!(client.admitted(), None, "a shared key is nobody's row");
        assert!(matches!(server.join().unwrap(), HandshakeOutcome::Authed { person: None, .. }));
    }
}

/// An OLD node's `AuthOk` has no `admitted` and still decodes (to `None`), and an answer without one is the old
/// bytes exactly, so neither side of a mixed pair breaks.
#[test]
fn an_old_answer_without_admitted_decodes_and_keeps_its_bytes() {
    let old =
        serde_json::to_vec(&serde_json::json!({ "AuthOk": { "scope": Scope::Read } })).unwrap();
    let decoded: HandshakeReply = serde_json::from_slice(&old).unwrap();
    assert!(matches!(decoded, HandshakeReply::AuthOk { scope: Scope::Read, admitted: None }));
    let new =
        serde_json::to_vec(&HandshakeReply::AuthOk { scope: Scope::Read, admitted: None }).unwrap();
    assert_eq!(new, old, "a shared-key answer is the old bytes");
}

/// **Every handshake frame's JSON bytes, pinned literally**: the frames the datahub's, the backtesthub's and the
/// gatewayhub's protocols carried as copies of these until decision 0288's amendment of 2026-11-21 deleted them. Each
/// encodes to exactly the bytes below and decodes back from them, so the deletion moved no byte: a `Welcome` without
/// a nonce (a key-less data-plane daemon's, and an old server's) carries no `nonce` key at all, and one with a nonce
/// carries its 32 bytes as an array; an `AuthOk` without `admitted` is the bytes every old client reads.
#[test]
fn every_handshake_frame_is_pinned_to_its_json_bytes() {
    fn pinned<T: Serialize + DeserializeOwned>(frame: &T, bytes: &str) {
        assert_eq!(serde_json::to_string(frame).unwrap(), bytes);
        let back: T = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes, "decodes from its own bytes");
    }
    let features = vec!["load_bars".to_string(), "auth".to_string()];
    let nonce = format!("[{}]", ["7"; 32].join(","));
    pinned(&HandshakeFrame::Hello { proto_version: 8 }, r#"{"Hello":{"proto_version":8}}"#);
    pinned(
        &HandshakeFrame::Auth { scope: Scope::Write, mac: vec![9, 0, 255] },
        r#"{"Auth":{"scope":"Write","mac":[9,0,255]}}"#,
    );
    pinned(
        &HandshakeReply::Welcome { proto_version: 9, features: features.clone(), nonce: None },
        r#"{"Welcome":{"proto_version":9,"features":["load_bars","auth"]}}"#,
    );
    pinned(
        &HandshakeReply::Welcome { proto_version: 9, features, nonce: Some([7u8; 32]) },
        &format!(
            r#"{{"Welcome":{{"proto_version":9,"features":["load_bars","auth"],"nonce":{nonce}}}}}"#
        ),
    );
    pinned(
        &HandshakeReply::AuthOk { scope: Scope::Trade, admitted: None },
        r#"{"AuthOk":{"scope":"Trade"}}"#,
    );
    pinned(
        &HandshakeReply::AuthDenied { reason: "bad mac".to_string() },
        r#"{"AuthDenied":{"reason":"bad mac"}}"#,
    );
    // ...and the frames of decision 0288 the copies never carried: a device's answer, an invitation's
    // registration and its grant, and an `AuthOk` naming who was admitted.
    pinned(
        &HandshakeFrame::PersonAuth {
            key_id: "nk-0123456789abcdef".to_string(),
            scope: Scope::Read,
            sig: vec![1, 2, 3],
        },
        r#"{"PersonAuth":{"key_id":"nk-0123456789abcdef","scope":"Read","sig":[1,2,3]}}"#,
    );
    pinned(
        &HandshakeFrame::Register(Registration {
            invite: "inv-1".to_string(),
            proof: vec![4, 5],
            device: "laptop".to_string(),
            public_key: vec![6, 7],
            ssh_key: "ssh-ed25519 AAAA guest".to_string(),
        }),
        r#"{"Register":{"invite":"inv-1","proof":[4,5],"device":"laptop","public_key":[6,7],"ssh_key":"ssh-ed25519 AAAA guest"}}"#,
    );
    pinned(
        &HandshakeReply::Registered(Registered {
            person: "alice".to_string(),
            device: "laptop".to_string(),
            role: "observe".to_string(),
            key_id: "nk-0123456789abcdef".to_string(),
        }),
        r#"{"Registered":{"person":"alice","device":"laptop","role":"observe","key_id":"nk-0123456789abcdef"}}"#,
    );
    pinned(
        &HandshakeReply::AuthOk {
            scope: Scope::Write,
            admitted: Some(Admitted {
                person: "alice".to_string(),
                device: "laptop".to_string(),
                role: "control".to_string(),
            }),
        },
        r#"{"AuthOk":{"scope":"Write","admitted":{"person":"alice","device":"laptop","role":"control"}}}"#,
    );
    // ...and a client's word on what it reads (the refusal frame, owner's Q8), sent only to a hub advertising it.
    pinned(
        &HandshakeFrame::Reads { features: vec!["refusal_frame".to_string()] },
        r#"{"Reads":{"features":["refusal_frame"]}}"#,
    );
    // A handshake frame read after the handshake is read as JSON alone: a msgpack or LZ4 body is a verb's.
    let hello = HandshakeFrame::Hello { proto_version: 8 };
    assert!(matches!(
        handshake_frame_of(&serde_json::to_vec(&hello).unwrap()),
        Some(HandshakeFrame::Hello { proto_version: 8 })
    ));
    for codec in [crate::frame::BodyCodec::Msgpack, crate::frame::BodyCodec::MsgpackLz4] {
        let body = crate::frame::encode_frame(codec, &hello).unwrap().split_off(4);
        assert!(handshake_frame_of(&body).is_none(), "{codec:?}");
    }
    assert!(handshake_frame_of(br#""Ping""#).is_none());
    assert_eq!(
        already_authenticated(&HandshakeFrame::Hello { proto_version: 8 }),
        "Hello is a handshake frame; this connection is already authenticated"
    );
}

/// **The refusal frame's legs (decision 0112, owner's Q8)**: a client says it reads the frame only to a hub that
/// advertised `refusal_frame` (an older hub is sent nothing), and a hub reads that word, and only that word, off it.
#[test]
fn a_client_says_it_reads_the_refusal_frame_only_to_a_hub_that_advertised_it() {
    let advertised = vec!["auth".to_string(), FEATURE_REFUSAL_FRAME.to_string()];
    let said = reads_answer(&advertised).expect("a hub that advertised the frame is told");
    assert_eq!(reads_refusal_frame(&said), Some(true));
    assert!(reads_answer(&["auth".to_string()]).is_none(), "an older hub is sent nothing");
    assert_eq!(reads_refusal_frame(&HandshakeFrame::Reads { features: Vec::new() }), Some(false));
    assert_eq!(reads_refusal_frame(&HandshakeFrame::Hello { proto_version: 8 }), None);
}
