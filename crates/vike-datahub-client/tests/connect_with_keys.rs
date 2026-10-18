//! `DatahubClient::connect_with_keys` — the ONE spelling of "authenticated when the caller holds
//! keys, plain when it does not" — pinned against the REAL data daemon, keyed and key-less.
//!
//! It replaced the same hand-written `match keys { Some(k) => connect_authed(addr, k, scope),
//! None => connect(addr) }` in vike-app-core, vike-cli, vike-studio and this crate's own
//! `RemoteHistStore`, so each case below holds what those copies did:
//!
//! - **keyed server, no keys** — the error `connect` returns, word for word (`PermissionDenied`,
//!   naming the missing keys): the door adds nothing and loses nothing on that arm;
//! - **keyed server, keys** — authenticated at EXACTLY the scope asked for, Read or Write;
//! - **keyed server, WRONG keys** — `connect_authed`'s refusal, never a quiet unauthenticated dial
//!   (there is no third branch);
//! - **key-less server, keys** — a clean unauthenticated connect (`connect_authed`'s contract);
//! - **key-less server, no keys** — a plain connect.

mod common;

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::DatahubClient;
use vike_hub_proto::auth::{HubKeys, Scope};

/// The pair the keyed server holds. Placeholder bytes — never a credential.
fn keys() -> HubKeys {
    HubKeys::new(b"observe-placeholder".to_vec(), b"control-placeholder".to_vec())
}

fn store() -> Arc<dyn HistStore + Send + Sync> {
    Arc::new(MemHistStore::new())
}

/// The REAL data daemon, KEYED with [`keys`], over an in-memory store.
fn spawn_keyed_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = store();
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, Some(keys()), None, None, None);
    });
    addr
}

#[test]
fn no_keys_against_a_keyed_server_is_connects_own_refusal_word_for_word() {
    let addr = spawn_keyed_datahub();
    let plain = DatahubClient::connect(addr).expect_err("a keyed server refuses a key-less dial");
    for scope in [Scope::Read, Scope::Write] {
        let door = DatahubClient::connect_with_keys(addr, None, scope)
            .expect_err("no keys against a keyed server must fail at the handshake");
        assert_eq!(door.kind(), ErrorKind::PermissionDenied);
        assert_eq!(door.kind(), plain.kind());
        assert_eq!(door.to_string(), plain.to_string(), "the `connect` arm, unchanged ({scope:?})");
    }
}

#[test]
fn keys_against_a_keyed_server_authenticate_at_exactly_the_scope_asked() {
    let addr = spawn_keyed_datahub();
    let k = keys();
    for scope in [Scope::Read, Scope::Write] {
        let client = DatahubClient::connect_with_keys(addr, Some(&k), scope)
            .unwrap_or_else(|e| panic!("the right keys authenticate ({scope:?}): {e}"));
        assert_eq!(client.authenticated_scope(), Some(scope));
    }
}

#[test]
fn wrong_keys_against_a_keyed_server_are_refused_never_dialled_plain() {
    let addr = spawn_keyed_datahub();
    let wrong = HubKeys::new(b"not-the-observe-key".to_vec(), b"not-the-control-key".to_vec());
    let err = DatahubClient::connect_with_keys(addr, Some(&wrong), Scope::Read)
        .expect_err("a wrong key must be refused");
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    let msg = err.to_string();
    assert!(msg.contains("datahub auth denied"), "connect_authed's refusal, not connect's: {msg}");
}

#[test]
fn a_key_less_server_answers_both_arms_unauthenticated() {
    let addr = common::spawn_server(store());
    let k = keys();
    for scope in [Scope::Read, Scope::Write] {
        let keyed = DatahubClient::connect_with_keys(addr, Some(&k), scope)
            .unwrap_or_else(|e| panic!("keys against a key-less server still connect: {e}"));
        assert_eq!(keyed.authenticated_scope(), None, "nothing to authenticate against");
        assert!(!keyed.requires_auth());
        let plain = DatahubClient::connect_with_keys(addr, None, scope)
            .unwrap_or_else(|e| panic!("a key-less dial to a key-less server connects: {e}"));
        assert_eq!(plain.authenticated_scope(), None);
    }
}
