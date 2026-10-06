//! **A Write key aimed at the wrong daemon is refused BEFORE it is used** —
//! `DatahubClient::connect_authed_on`, driven over real sockets.
//!
//! One node pair authenticates both daemons, so a CONTROL key signed toward the data daemon does
//! not fail: it opens a `VerbScope::Write` session there, and that scope carries `Backfill` and
//! `DeleteSeries`. `connect_authed_on` reads the pre-auth `Welcome` and refuses unless it names the
//! plane the caller asked for — between the `Welcome` and the first byte of `Auth`.
//!
//! Three kinds of proof, each doing what the others cannot:
//!
//! 1. **BYTES** — a scripted server that records every byte the client writes after its `Hello`.
//!    Refused ⇒ zero bytes. That is the claim "nothing was sent", measured on the wire rather than
//!    inferred from an `Err`.
//! 2. **The REAL data daemon** (`vike_datahub::serve_authed`, keyed). The UNGUARDED
//!    `connect_authed` there gets `Some(Write)` — the hazard, reproduced against the production
//!    server rather than asserted — and `connect_authed_on(Plane::Compute)` against the same
//!    listener is refused. `connect_authed_on(Plane::Data)` SUCCEEDS there, which is also the proof
//!    that the real server's `served_features` classifies as the data plane: a sentinel that drifted
//!    away from `vike_datahub_client::DATA_PLANE_SENTINEL` reddens this file.
//! 3. **The positive control on the other plane** — a scripted COMPUTE `Welcome` is let through,
//!    and the scripted server sees the `Auth` frame arrive. Without it, a guard that refused every
//!    keyed connect would pass (1) and (2).
//!
//! The compute daemon's own `served_features` is held to its sentinel one crate up, in
//! `crates/vike-backtest/src/compute_server.rs`'s tests: this crate cannot link that server.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::{
    COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, DatahubClient, FEATURE_AUTH, PROTO_VERSION,
    STUDIO_RUNNER_SENTINEL,
    proto::{Plane, Request, Response, write_frame},
    read_frame,
};
use vike_node_proto::auth::{NodeKeys, Scope};

/// A key pair both sides of every case share. Placeholder bytes — never a credential.
fn keys() -> NodeKeys {
    NodeKeys::new(b"observe-placeholder".to_vec(), b"control-placeholder".to_vec())
}

fn features(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_string()).collect()
}

/// A one-connection scripted server: read the `Hello`, answer a `Welcome` advertising `features`
/// with a nonce (a KEYED server's shape), then RECORD every byte the client writes afterwards until
/// it hangs up. The join handle yields those bytes.
///
/// The read after the `Welcome` is bounded, so a client that neither writes nor closes cannot hang
/// the test — it shows up as a test failure naming the bytes, not as a stall.
fn spawn_recorder(features: Vec<String>) -> (SocketAddr, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("the scripted server expected Hello first, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: Some([7u8; 32]) },
        )
        .expect("write the Welcome");
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
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("the scripted server expected Hello first, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: Some([7u8; 32]) },
        )
        .expect("write the Welcome");
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("bound the Auth read");
        let asked = match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Auth { scope, .. }) => scope,
            Ok(other) => panic!("the scripted server expected Auth, got {other:?}"),
            Err(_) => return None,
        };
        write_frame(&mut stream, &Response::AuthOk { scope: asked }).expect("write AuthOk");
        // Hold the socket until the client drops it, so the AuthOk is read before a close races it.
        let _ = read_frame::<_, Request>(&mut stream);
        Some(asked)
    });
    (addr, handle)
}

/// The REAL data daemon, KEYED with [`keys`], over an in-memory store.
fn spawn_keyed_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, Some(keys()), None, None, None);
    });
    addr
}

/// ⚠ **THE KILL PROOF, on the wire.** A keyed server whose `Welcome` is the DATA plane's: the
/// guarded constructor refuses, and the server receives NOTHING after the `Hello` — no `Auth`, no
/// mac, not one byte. A guard that checked AFTER signing would pass an `is_err()` assertion and
/// fail this one.
#[test]
fn a_write_key_asked_for_the_compute_plane_sends_nothing_to_a_data_plane_welcome() {
    let (addr, server) = spawn_recorder(features(&[DATA_PLANE_SENTINEL, FEATURE_AUTH]));

    let err = DatahubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Compute)
        .expect_err("a DATA-plane Welcome must be refused for a compute-plane Write key");
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
            features(&[COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, FEATURE_AUTH]),
        ),
        ("an unknown peer", features(&[FEATURE_AUTH])),
    ] {
        let (addr, server) = spawn_recorder(advertised);
        let err = DatahubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Compute)
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
fn a_compute_plane_welcome_is_authenticated_as_asked() {
    let (addr, server) =
        spawn_granting(features(&[COMPUTE_PLANE_SENTINEL, STUDIO_RUNNER_SENTINEL, FEATURE_AUTH]));
    let client = DatahubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Compute)
        .expect("the compute plane is the one asked for");
    assert_eq!(client.authenticated_scope(), Some(Scope::Write));
    drop(client);
    assert_eq!(server.join().expect("the scripted server finished"), Some(Scope::Write));
}

/// ⚠ **The hazard, reproduced against the REAL data daemon rather than asserted.** The unguarded
/// constructor, handed a Write key and pointed at the keyed datahub, AUTHENTICATES — a Write session
/// on the plane that carries `Backfill` and `DeleteSeries`. The guarded one, at the same address,
/// is refused. And asked for the DATA plane it succeeds, which is the proof that the real server's
/// `Welcome` classifies as that plane (so its sentinel is the one this crate compares).
#[test]
fn against_the_real_keyed_datahub_only_the_guard_stands_between_a_write_key_and_a_store_session() {
    let addr = spawn_keyed_datahub();

    let unguarded = DatahubClient::connect_authed(addr, &keys(), Scope::Write)
        .expect("the unguarded constructor authenticates wherever the key verifies");
    assert_eq!(
        unguarded.authenticated_scope(),
        Some(Scope::Write),
        "the premise: without the guard, a Write key opens a Write session on the DATA daemon"
    );
    drop(unguarded);

    let err = DatahubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Compute)
        .expect_err("the guard refuses the data daemon for a compute-plane key");
    assert!(err.to_string().contains("DATA daemon"), "{err}");

    let data = DatahubClient::connect_authed_on(addr, &keys(), Scope::Read, Plane::Data)
        .expect("the real datahub's Welcome must classify as the DATA plane");
    assert_eq!(data.authenticated_scope(), Some(Scope::Read));
}
