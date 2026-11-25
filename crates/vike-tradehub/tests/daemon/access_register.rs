//! An invitation end to end over the REAL tradehub server and the REAL client verbs (decision 0288): a node whose keys
//! carry the access list advertises `person-auth` and `access-register`; a guest's app registers its device over
//! loopback with the invitation's secret; that device then authenticates with `PersonAuth` under the invitation's
//! role, is refused above it, and the spent invitation registers nothing more.

use vike_hub_protocol::access::{AccessEntry, AccessList, DeviceKey, invite_auth_secret};
use vike_hub_protocol::auth::HubKeys;
use vike_tradehub::server::settings::SettingsShowSource;
use vike_tradehub_client::{TradehubControlHandle, register_device, snapshot_once};

use crate::support::spawn_maker_hub_with_keys;

const TOKEN: &str = "ACCESS_REGISTER_TOKEN";
const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBeY8sZr3o6R0x8T1v9Kc2QmN4yXh7pJwLfGd5uVbE0a";

#[test]
fn an_invitation_registers_a_device_that_then_authenticates_under_its_role_only() {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 temp path")))
        .expect("an empty settings database");
    let added = vike_secrets::access::add_access_invite(
        dir.path(),
        "0a1b2c3d",
        "alice",
        "observe",
        &invite_auth_secret("s3cret"),
        i64::MAX,
    )
    .expect("the invitation is written");
    assert!(added);

    // The node's list reads the store at every call, as a daemon's does.
    let store = dir.path().to_path_buf();
    let list = AccessList::new(move |key_id| {
        let row = vike_secrets::access::find_access_key(&store, key_id).ok().flatten()?;
        Some(AccessEntry {
            person: row.person,
            device: row.device,
            role: row.role,
            public_key: row.public_key,
            revoked: row.revoked_ms.is_some(),
        })
    });
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec()).with_access(list);
    let settings = SettingsShowSource { settings_dir: Some(dir.path().to_path_buf()), hot: None };
    let (_mount, addr) = spawn_maker_hub_with_keys(TOKEN, keys, true, Some(settings));

    let (device, _) = DeviceKey::generate().expect("a device key");
    let granted = register_device(addr, "0a1b2c3d", "s3cret", "macbook", &device, SSH_KEY)
        .expect("the invitation registers the device");
    assert_eq!(
        (granted.person.as_str(), granted.device.as_str(), granted.role.as_str()),
        ("alice", "macbook", "observe")
    );
    assert_eq!(granted.key_id, device.key_id());

    // The device reads under its role, with no shared key at all.
    snapshot_once(addr, &device).expect("a listed observe device reads a snapshot");

    // Above its role: control is `access denied`, not a session.
    let err = TradehubControlHandle::connect(addr, &device)
        .err()
        .expect("an observe device is refused the control scope");
    assert!(matches!(err, vike_hub_protocol::call::HubCallError::AccessDenied(_)), "{err:?}");
    assert!(err.to_string().contains("access denied"), "{err}");

    // The invitation is spent: a second device registers nothing.
    let (another, _) = DeviceKey::generate().unwrap();
    let err = register_device(addr, "0a1b2c3d", "s3cret", "laptop", &another, SSH_KEY)
        .expect_err("an invitation registers ONE device");
    assert!(matches!(err, vike_hub_protocol::call::HubCallError::AccessDenied(_)), "{err:?}");
    assert!(err.to_string().contains("invitation refused"), "{err}");

    // A device nobody registered is `access denied` even for a read.
    let err = snapshot_once(addr, &another).expect_err("no row, no read");
    assert!(err.to_string().contains("access denied"), "{err}");
}

/// **One signature domain per hub** (decision 0288's amendment): a listed device's login signed for the datahub or
/// the backtesthub, over this node's own nonce and version, is refused here, so a client lured into signing a nonce
/// relayed from the tradehub cannot be replayed onto it. The same login signed for the tradehub is admitted.
#[test]
fn a_login_signed_for_another_hub_is_refused_and_one_for_the_tradehub_admitted() {
    use std::net::TcpStream;

    use vike_hub_protocol::access::ACCESS_DENIED;
    use vike_hub_protocol::auth::{BACKTESTHUB_DOMAIN, DATAHUB_DOMAIN, Domain, Scope};
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_tradehub_client::proto::NODE_PROTO_VERSION;

    let (device, _) = DeviceKey::generate().expect("a device key");
    let entry = AccessEntry {
        person: "alice".into(),
        device: "macbook".into(),
        role: "observe".into(),
        public_key: device.public_key().to_vec(),
        revoked: false,
    };
    let id = device.key_id();
    let list = AccessList::new(move |key_id| (key_id == id).then(|| entry.clone()));
    let keys = HubKeys::new(b"observe-key".to_vec(), Vec::new()).with_access(list);
    let (_mount, addr) = spawn_maker_hub_with_keys("ACCESS_DOMAIN_TOKEN", keys, false, None);
    let login = |domain: Domain| {
        let mut stream = TcpStream::connect(addr).expect("connect");
        write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
            .expect("hello");
        let nonce = match read_frame::<_, HandshakeReply>(&mut stream).expect("welcome") {
            HandshakeReply::Welcome { nonce: Some(nonce), .. } => nonce,
            other => panic!("expected a keyed Welcome, got {other:?}"),
        };
        let sig = device.sign(domain, &nonce, NODE_PROTO_VERSION, Scope::Read);
        let auth = HandshakeFrame::PersonAuth { key_id: device.key_id(), scope: Scope::Read, sig };
        write_frame(&mut stream, &auth).expect("person auth");
        read_frame::<_, HandshakeReply>(&mut stream).expect("the answer")
    };
    for other in [DATAHUB_DOMAIN, BACKTESTHUB_DOMAIN] {
        let answer = login(other);
        assert!(
            matches!(&answer, HandshakeReply::AuthDenied { reason } if reason == ACCESS_DENIED),
            "{other:?}: {answer:?}"
        );
    }
    let answer = login(vike_tradehub_client::auth::DOMAIN);
    assert!(matches!(answer, HandshakeReply::AuthOk { scope: Scope::Read, .. }), "{answer:?}");
}

/// The node tells each device the role its access row holds (the desktop shows it, not CONTROL for an
/// admin): an admin, a control and an observe device logging in get their own role back, with who.
#[test]
fn each_device_is_told_its_own_role() {
    use std::net::TcpStream;

    use vike_hub_protocol::auth::Scope;
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_tradehub_client::proto::NODE_PROTO_VERSION;

    let devices: Vec<(DeviceKey, &str)> = ["admin", "control", "observe"]
        .into_iter()
        .map(|role| (DeviceKey::generate().expect("a device key").0, role))
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
    let keys = HubKeys::new(b"observe-key".to_vec(), Vec::new()).with_access(list);
    let (_mount, addr) = spawn_maker_hub_with_keys("ACCESS_ROLE_TOKEN", keys, false, None);
    for (device, role) in &devices {
        let mut stream = TcpStream::connect(addr).expect("connect");
        write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
            .expect("hello");
        let nonce = match read_frame::<_, HandshakeReply>(&mut stream).expect("welcome") {
            HandshakeReply::Welcome { nonce: Some(nonce), .. } => nonce,
            other => panic!("expected a keyed Welcome, got {other:?}"),
        };
        let domain = vike_tradehub_client::auth::DOMAIN;
        let sig = device.sign(domain, &nonce, NODE_PROTO_VERSION, Scope::Read);
        let auth = HandshakeFrame::PersonAuth { key_id: device.key_id(), scope: Scope::Read, sig };
        write_frame(&mut stream, &auth).expect("person auth");
        match read_frame::<_, HandshakeReply>(&mut stream).expect("the answer") {
            HandshakeReply::AuthOk { scope: Scope::Read, admitted: Some(admitted) } => {
                assert_eq!(admitted.role, *role, "{admitted:?}");
                assert_eq!(
                    (admitted.person.as_str(), admitted.device),
                    ("alice", format!("pc-{role}"))
                );
            }
            other => panic!("{role}: {other:?}"),
        }
    }
}
