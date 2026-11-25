//! The invitation's registration door (decision 0288 item 7.3) over a real store: a loopback peer with the secret is
//! granted ONE row once, and every other case is `invitation refused` with nothing spent.

use std::net::SocketAddr;

use super::register::{INVITATION_REFUSED, register};
use vike_hub_protocol::access::{DeviceKey, invite_auth_secret, invite_proof};
use vike_hub_protocol::handshake::Registration;
use vike_tradehub_client::proto::NODE_PROTO_VERSION;

const NONCE: [u8; 32] = [3u8; 32];
const INVITE: &str = "0a1b2c3d";
const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBeY8sZr3o6R0x8T1v9Kc2QmN4yXh7pJwLfGd5uVbE0a";

fn loopback() -> Option<SocketAddr> {
    Some("127.0.0.1:50000".parse().unwrap())
}

/// A store holding one `observe` invitation for `alice` whose secret is `s3cret`, alive until `expires_ms`.
fn store_with_invite(expires_ms: i64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 temp path")))
        .expect("an empty settings database");
    let added = vike_secrets::access::add_access_invite(
        dir.path(),
        INVITE,
        "alice",
        "observe",
        &invite_auth_secret("s3cret"),
        expires_ms,
    )
    .expect("the invitation is written");
    assert!(added);
    dir
}

fn registration(device: &str, key: &DeviceKey, secret: &str) -> Registration {
    Registration {
        invite: INVITE.into(),
        proof: invite_proof(secret, &NONCE, NODE_PROTO_VERSION),
        device: device.into(),
        public_key: key.public_key().to_vec(),
        ssh_key: SSH_KEY.into(),
    }
}

#[test]
fn a_loopback_registration_with_the_secret_grants_one_row_and_only_once() {
    let dir = store_with_invite(i64::MAX);
    let (key, _) = DeviceKey::generate().unwrap();
    let granted =
        register(dir.path(), loopback(), registration("macbook", &key, "s3cret"), &NONCE, 1_000)
            .expect("granted");
    assert_eq!(
        (granted.person.as_str(), granted.device.as_str(), granted.role.as_str()),
        ("alice", "macbook", "observe")
    );
    assert_eq!(granted.key_id, key.key_id(), "the id is the door's, from the public bytes");
    let row = vike_secrets::access::find_access_key(dir.path(), &key.key_id())
        .expect("the store reads")
        .expect("the row is in");
    assert_eq!(
        (row.person.as_str(), row.device.as_str(), row.role.as_str()),
        ("alice", "macbook", "observe")
    );
    assert_eq!(row.public_key, key.public_key().to_vec());
    assert_eq!(row.revoked_ms, None);

    let (another, _) = DeviceKey::generate().unwrap();
    let again =
        register(dir.path(), loopback(), registration("laptop", &another, "s3cret"), &NONCE, 1_001);
    assert_eq!(again, Err(INVITATION_REFUSED), "an invitation registers ONE device");
    assert!(
        vike_secrets::access::find_access_key(dir.path(), &another.key_id()).unwrap().is_none()
    );

    // The grant is ONE `access` record in the change journal beside the store (decision 0288 item 5); the refusal
    // is none.
    let changes = dir.path().join("state").join("changes");
    let records: Vec<serde_json::Value> = std::fs::read_dir(&changes)
        .expect("the change journal exists")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .expect("read")
                .lines()
                .map(|l| serde_json::from_str(l).expect("one JSON object"))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(records.len(), 1, "{records:#?}");
    let target = &records[0]["target"];
    assert_eq!(records[0]["kind"], "access");
    assert_eq!(target["verb"], "register");
    assert_eq!(target["person"], "alice");
    assert_eq!(target["device"], "macbook");
    assert_eq!(target["role"], "observe");
    assert_eq!(target["key_id"], key.key_id().as_str());
    assert_eq!(records[0]["actor"]["origin"], "wire");
    assert_eq!(records[0]["actor"]["person"], "alice");
}

#[test]
fn every_other_registration_is_refused_and_spends_nothing() {
    let dir = store_with_invite(10_000);
    let (key, _) = DeviceKey::generate().unwrap();
    let door =
        |peer, registration, now_ms| register(dir.path(), peer, registration, &NONCE, now_ms);

    // Off loopback, or with no peer at all: the hub wire is plaintext, only the tunnel may carry it.
    for peer in [
        Some("<host>:50000".parse().unwrap()),
        Some("[::ffff:<host>]:1".parse().unwrap()),
        None,
    ] {
        assert_eq!(door(peer, registration("pc", &key, "s3cret"), 1), Err(INVITATION_REFUSED));
    }
    // The shapes, checked here before the store is asked.
    let shaped = |edit: fn(&mut Registration)| {
        let mut r = registration("pc", &key, "s3cret");
        edit(&mut r);
        r
    };
    for bad in [
        shaped(|r| r.device = "PC".into()),
        shaped(|r| r.device = "a b".into()),
        shaped(|r| r.ssh_key = format!("{SSH_KEY} guest@pc")),
        shaped(|r| r.ssh_key = format!("command=\"/bin/sh\" {SSH_KEY}")),
        shaped(|r| r.public_key.truncate(31)),
        shaped(|r| r.invite = "0A1B2C3D".into()),
        shaped(|r| r.invite = "zz".into()),
    ] {
        assert_eq!(door(loopback(), bad, 1), Err(INVITATION_REFUSED));
    }
    // The wrong secret, and the right one once the invitation has expired (checked at use).
    assert_eq!(door(loopback(), registration("pc", &key, "wrong"), 1), Err(INVITATION_REFUSED));
    assert_eq!(
        door(loopback(), registration("pc", &key, "s3cret"), 10_000),
        Err(INVITATION_REFUSED)
    );

    // None of it spent the invitation: inside its life, with its secret, it still grants.
    assert!(door(loopback(), registration("pc", &key, "s3cret"), 9_999).is_ok());
}
