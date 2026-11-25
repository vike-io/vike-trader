//! **A Write key aimed at the wrong daemon is refused BEFORE it is used** —
//! `BacktesthubClient::connect_authed_on`, driven over real sockets.
//!
//! One key is admitted by both daemons, so a CONTROL key aimed at the data daemon is a session that
//! carries `Backfill` and `DeleteSeries` away. Its signature no longer opens one there (each daemon
//! signs under its own separator since decision 0288's amendment), and `connect_authed_on` refuses
//! even to send it: it reads the pre-auth `Welcome` and refuses unless it names the plane the caller
//! asked for — between the `Welcome` and the first byte of `Auth`.
//!
//! Two kinds of proof, each doing what the other cannot:
//!
//! 1. **BYTES** — a scripted server records every byte the client writes after its `Hello`.
//!    Refused ⇒ zero bytes: "nothing was sent", measured on the wire rather than inferred from an
//!    `Err`.
//! 2. **The positive control on the other plane** — a scripted COMPUTE `Welcome` is let through,
//!    and the scripted server sees the `Auth` frame arrive. Without it, a guard that refused every
//!    keyed connect would pass (1).
//!
//! The REAL daemons are held to their sentinels in their own suites:
//! `crates/vike-backtesthub/tests/backtest_plane.rs`'s
//! `a_keyed_backtesthub_is_the_plane_a_backtest_key_may_sign_toward` and
//! `crates/vike-datahub/tests/default_build/plane_split.rs`'s
//! `against_the_real_keyed_datahub_a_backtesthub_login_opens_no_store_session`.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::Duration;

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{PROTO_VERSION, Request, STUDIO_RUNNER_SENTINEL};
use vike_hub_protocol::auth::{FEATURE_AUTH, HubKeys, Scope};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_hub_protocol::plane::{BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL, Plane};

/// A key pair both sides of every case share. Placeholder bytes — never a credential.
fn keys() -> HubKeys {
    HubKeys::new(b"observe-placeholder".to_vec(), b"control-placeholder".to_vec())
}

fn features(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_string()).collect()
}

/// Read the client's `Hello` and answer a KEYED server's `Welcome` (a nonce) advertising `features`.
fn welcome(stream: &mut std::net::TcpStream, features: Vec<String>) {
    match read_frame::<_, HandshakeFrame>(stream) {
        Ok(HandshakeFrame::Hello { .. }) => {}
        other => panic!("the scripted server expected Hello first, got {other:?}"),
    }
    let welcome =
        HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: Some([7u8; 32]) };
    write_frame(stream, &welcome).expect("write the Welcome");
}

/// A one-connection scripted server: read the `Hello`, answer a `Welcome` advertising `features`
/// with a nonce (a KEYED server's shape), then RECORD every byte the client writes afterwards until
/// it hangs up. The join handle yields those bytes. The recording read is bounded, so a client that
/// neither writes nor closes fails the test instead of stalling it.
fn spawn_recorder(features: Vec<String>) -> (SocketAddr, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        welcome(&mut stream, features);
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("bound the recording read");
        let mut after_hello = Vec::new();
        let _ = stream.read_to_end(&mut after_hello);
        after_hello
    });
    (addr, handle)
}

/// A one-connection scripted server that GRANTS whatever `Auth` it is sent, and reports whether one
/// arrived. The mac is not checked — this is the positive control for the CLIENT's decision to
/// send, not a test of verification.
fn spawn_granting(features: Vec<String>) -> (SocketAddr, thread::JoinHandle<Option<Scope>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        welcome(&mut stream, features);
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("bound the Auth read");
        let asked = match read_frame::<_, HandshakeFrame>(&mut stream) {
            Ok(HandshakeFrame::Auth { scope, .. }) => scope,
            Ok(other) => panic!("the scripted server expected Auth, got {other:?}"),
            Err(_) => return None,
        };
        let granted = HandshakeReply::AuthOk { scope: asked, admitted: None };
        write_frame(&mut stream, &granted).expect("write AuthOk");
        // Hold the socket until the client drops it, so the AuthOk is read before a close races it.
        let _ = read_frame::<_, Request>(&mut stream);
        Some(asked)
    });
    (addr, handle)
}

/// ⚠ **THE KILL PROOF, on the wire.** A keyed server whose `Welcome` is the DATA plane's: the
/// guarded constructor refuses, and the server receives NOTHING after the `Hello` — no `Auth`, no
/// mac, not one byte. A guard that checked AFTER signing would pass an `is_err()` assertion and
/// fail this one.
#[test]
fn a_write_key_asked_for_the_backtest_plane_sends_nothing_to_a_data_plane_welcome() {
    let (addr, server) = spawn_recorder(features(&[DATA_PLANE_SENTINEL, FEATURE_AUTH]));

    let err = BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
        .expect_err("a DATA-plane Welcome must be refused for a backtest-plane Write key");
    let msg = err.to_string();
    assert!(msg.contains("DATA daemon"), "the refusal names what answered: {msg}");
    assert!(msg.contains("config.backtest_addr"), "…and where the wanted daemon is set: {msg}");

    let after_hello = server.join().expect("the scripted server finished");
    assert!(
        after_hello.is_empty(),
        "the client wrote {} byte(s) after its Hello to a DATA-plane server — the guard ran after \
         something had already been sent",
        after_hello.len()
    );
}

/// A PRE-SPLIT daemon — both planes in one process — is refused too: it serves `Backfill` and
/// `DeleteSeries` beside the run verbs, so a Write session there is the thing being prevented.
/// An unknown peer (neither sentinel) likewise. Both send nothing.
#[test]
fn a_pre_split_daemon_and_an_unknown_peer_are_refused_before_anything_is_sent() {
    for (what, advertised) in [
        (
            "a pre-split daemon",
            features(&[BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL, FEATURE_AUTH]),
        ),
        ("an unknown peer", features(&[FEATURE_AUTH])),
    ] {
        let (addr, server) = spawn_recorder(advertised);
        let err =
            BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
                .expect_err(what);
        assert!(err.to_string().contains("no single plane"), "{what}: {err}");
        let after_hello = server.join().expect("the scripted server finished");
        assert!(after_hello.is_empty(), "{what}: {} byte(s) sent after Hello", after_hello.len());
    }
}

/// ⚠ **The positive control.** A COMPUTE-plane `Welcome` is let through and the `Auth` frame
/// ARRIVES — so the refusals above are the plane check firing, not a guard that refuses every keyed
/// connect (which would pass both tests above and break every Studio Run).
#[test]
fn a_backtest_plane_welcome_is_authenticated_as_asked() {
    let (addr, server) =
        spawn_granting(features(&[BACKTEST_PLANE_SENTINEL, STUDIO_RUNNER_SENTINEL, FEATURE_AUTH]));
    let client = BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
        .expect("the backtest plane is the one asked for");
    assert_eq!(client.authenticated_scope(), Some(Scope::Write));
    drop(client);
    assert_eq!(server.join().expect("the scripted server finished"), Some(Scope::Write));
}
