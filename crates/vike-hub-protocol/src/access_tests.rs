//! The one check: a row, a role, a revoke and a signature, each refused by name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::auth::DATAHUB_DOMAIN;

const NONCE: [u8; 32] = [7u8; 32];

/// A list over a map the test edits between calls, as a store is edited between reads.
fn list_of(rows: Arc<Mutex<HashMap<String, AccessEntry>>>) -> AccessList {
    AccessList::new(move |key_id| rows.lock().unwrap().get(key_id).cloned())
}

fn row(key: &DeviceKey, role: &str) -> AccessEntry {
    AccessEntry {
        person: "alice".into(),
        device: "macbook".into(),
        role: role.into(),
        public_key: key.public_key().to_vec(),
        revoked: false,
    }
}

#[test]
fn a_role_admits_every_scope_up_to_its_own_and_never_trade() {
    assert!(role_admits("observe", Scope::Read));
    assert!(!role_admits("observe", Scope::Write));
    assert!(role_admits("control", Scope::Write));
    assert!(!role_admits("control", Scope::Account));
    assert!(role_admits("admin", Scope::Account));
    for role in ["observe", "control", "admin", "trade", ""] {
        assert!(!role_admits(role, Scope::Trade), "{role} must not admit Trade");
    }
    assert!(!role_admits("Admin", Scope::Read), "a role is its exact word");
    assert_eq!(ROLES.map(Scope::name), ["observe", "control", "admin"]);
}

#[test]
fn a_device_signature_verifies_only_for_its_own_row_scope_nonce_and_domain() {
    let (key, document) = DeviceKey::generate().expect("generate");
    assert_eq!(DeviceKey::from_pkcs8(&document).expect("reload").key_id(), key.key_id());
    let rows = Arc::new(Mutex::new(HashMap::from([(key.key_id(), row(&key, "control"))])));
    let list = list_of(Arc::clone(&rows));
    let sig = key.sign(DATAHUB_DOMAIN, &NONCE, 7, Scope::Write);

    let person =
        verify_person(&list, DATAHUB_DOMAIN, &NONCE, 7, &key.key_id(), Scope::Write, &sig).unwrap();
    assert_eq!((person.person.as_str(), person.device.as_str()), ("alice", "macbook"));

    let refused = |nonce: &[u8; 32], version, scope, domain| {
        verify_person(&list, domain, nonce, version, &key.key_id(), scope, &sig).unwrap_err()
    };
    assert_eq!(refused(&[8u8; 32], 7, Scope::Write, DATAHUB_DOMAIN), AccessRefusal::Signature);
    assert_eq!(refused(&NONCE, 8, Scope::Write, DATAHUB_DOMAIN), AccessRefusal::Signature);
    assert_eq!(refused(&NONCE, 7, Scope::Read, DATAHUB_DOMAIN), AccessRefusal::Signature);
    assert_eq!(refused(&NONCE, 7, Scope::Write, INVITE_DOMAIN), AccessRefusal::Signature);
    assert_eq!(refused(&NONCE, 7, Scope::Account, DATAHUB_DOMAIN), AccessRefusal::Role);

    // Another device's signature under this row's id.
    let (other, _) = DeviceKey::generate().unwrap();
    let forged = other.sign(DATAHUB_DOMAIN, &NONCE, 7, Scope::Write);
    assert_eq!(
        verify_person(&list, DATAHUB_DOMAIN, &NONCE, 7, &key.key_id(), Scope::Write, &forged),
        Err(AccessRefusal::Signature)
    );
    // A row whose public key is not the one its id names.
    rows.lock().unwrap().insert(other.key_id(), row(&key, "control"));
    assert_eq!(
        verify_person(&list, DATAHUB_DOMAIN, &NONCE, 7, &other.key_id(), Scope::Write, &sig),
        Err(AccessRefusal::Signature)
    );
    assert_eq!(
        verify_person(&list, DATAHUB_DOMAIN, &NONCE, 7, "nk-0000000000000000", Scope::Read, &sig),
        Err(AccessRefusal::Unknown)
    );
}

#[test]
fn a_revoke_or_a_lowered_role_refuses_the_next_check_at_once() {
    let (key, _) = DeviceKey::generate().unwrap();
    let rows = Arc::new(Mutex::new(HashMap::from([(key.key_id(), row(&key, "control"))])));
    let list = list_of(Arc::clone(&rows));
    assert!(admit(&list, &key.key_id(), Scope::Write).is_ok());

    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().role = "observe".into();
    assert_eq!(admit(&list, &key.key_id(), Scope::Write), Err(AccessRefusal::Role));
    assert!(admit(&list, &key.key_id(), Scope::Read).is_ok());

    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().revoked = true;
    assert_eq!(admit(&list, &key.key_id(), Scope::Read), Err(AccessRefusal::Revoked));
}

/// **The store policy, pinned once for every hub**: a row is read afresh at each call, a store that fails a lookup
/// reaches the daemon's hook with its error and admits nobody, a failed `held` counts as held, and a list without a
/// store holds nothing.
#[test]
fn a_store_list_reads_afresh_an_unreadable_store_admits_nobody_and_a_failed_held_counts_as_held() {
    let (key, _) = DeviceKey::generate().unwrap();
    let rows = Arc::new(Mutex::new(HashMap::from([(key.key_id(), row(&key, "control"))])));
    let broken = Arc::new(Mutex::new(false));
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let list = {
        let (rows, find_broken, held_broken) =
            (Arc::clone(&rows), Arc::clone(&broken), Arc::clone(&broken));
        let heard = Arc::clone(&heard);
        AccessList::from_store(
            move |key_id| {
                if *find_broken.lock().unwrap() {
                    return Err("database is locked");
                }
                Ok(rows.lock().unwrap().get(key_id).cloned())
            },
            move || {
                if *held_broken.lock().unwrap() { Err("database is locked") } else { Ok(false) }
            },
            move |e| heard.lock().unwrap().push(e.to_string()),
        )
    };
    assert!(admit(&list, &key.key_id(), Scope::Write).is_ok());
    assert!(!list.held(), "a readable `held` answers for itself");
    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().revoked = true;
    assert_eq!(
        admit(&list, &key.key_id(), Scope::Write),
        Err(AccessRefusal::Revoked),
        "read afresh"
    );
    assert!(heard.lock().unwrap().is_empty(), "nothing failed yet");

    *broken.lock().unwrap() = true;
    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().revoked = false;
    assert_eq!(
        admit(&list, &key.key_id(), Scope::Read),
        Err(AccessRefusal::Unknown),
        "admits nobody"
    );
    assert!(list.held(), "a failed `held` counts as held, so the refusal is the lookup's");
    assert_eq!(
        *heard.lock().unwrap(),
        ["database is locked"],
        "the hook hears the find error, once"
    );

    let none = AccessList::without_store();
    assert_eq!(admit(&none, &key.key_id(), Scope::Read), Err(AccessRefusal::Unknown));
    assert!(!none.held(), "no store holds no row: NO_DEVICE_REGISTERED");
}

/// The server box's own devices: valid names, distinct rows, a role of [`ROLES`] each (never `Trade`), and the
/// `admin` row first.
#[test]
fn the_server_devices_are_valid_distinct_rows_with_the_admin_row_first() {
    assert_eq!(SERVER_DEVICES[0], OWNER_SERVER_DEVICE);
    let words: Vec<_> =
        SERVER_DEVICES.iter().map(|d| (d.person, d.device, d.role.name())).collect();
    assert_eq!(
        words,
        [
            ("owner", "server", "admin"),
            ("server", "backtesthub", "observe"),
            ("owner", "telegram", "control"),
        ]
    );
    for (i, d) in SERVER_DEVICES.iter().enumerate() {
        assert!(valid_name(d.person) && valid_name(d.device), "{d:?}");
        assert!(valid_role(d.role.name()) && ROLES.contains(&d.role), "{d:?}");
        assert!(
            SERVER_DEVICES[..i].iter().all(|o| (o.person, o.device) != (d.person, d.device)),
            "{d:?}"
        );
    }
}

/// The per-write re-check: a person's row decides afresh, a shared-key connection has none to read, and a person on a
/// hub holding no list is refused.
#[test]
fn the_write_recheck_reads_the_row_again_and_passes_a_shared_key() {
    let (key, _) = DeviceKey::generate().unwrap();
    let rows = Arc::new(Mutex::new(HashMap::from([(key.key_id(), row(&key, "control"))])));
    let list = list_of(Arc::clone(&rows));
    let alice = admit(&list, &key.key_id(), Scope::Write).unwrap();

    assert_eq!(recheck(Some(&list), Some(&alice), Scope::Write), Ok(()));
    assert_eq!(recheck(None, None, Scope::Write), Ok(()), "a shared key has no row");
    assert_eq!(recheck(None, Some(&alice), Scope::Write), Err(AccessRefusal::Unknown));

    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().role = "observe".into();
    assert_eq!(recheck(Some(&list), Some(&alice), Scope::Write), Err(AccessRefusal::Role));
    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().revoked = true;
    assert_eq!(recheck(Some(&list), Some(&alice), Scope::Write), Err(AccessRefusal::Revoked));
}

#[test]
fn an_invitation_proof_binds_its_secret_and_nonce_and_is_no_auth_tag() {
    let stored = invite_auth_secret("s3cret");
    let proof = invite_proof("s3cret", &NONCE, 9);
    assert!(invite_proof_verifies(&stored, &NONCE, 9, &proof));
    assert!(!invite_proof_verifies(&stored, &[1u8; 32], 9, &proof));
    assert!(!invite_proof_verifies(&invite_auth_secret("other"), &NONCE, 9, &proof));
    assert!(!invite_proof_verifies(
        &[],
        &NONCE,
        9,
        &sign(INVITE_DOMAIN, &[], &NONCE, 9, Scope::Read)
    ));
    // The same key under the hubs' auth domain is a different tag.
    assert!(!verify(DATAHUB_DOMAIN, &stored, &NONCE, 9, Scope::Read, &proof));
}

#[test]
fn names_and_ssh_keys_are_refused_unless_exactly_their_shape() {
    for good in ["alice", "a", "0pc", "macbook-pro_2", "x".repeat(32).as_str()] {
        assert!(valid_name(good), "{good}");
    }
    for bad in ["", "Alice", "-a", "_a", "a b", "a\"b", "a\n", "x".repeat(33).as_str(), "ä"] {
        assert!(!valid_name(bad), "{bad:?}");
    }
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBeY8sZr3o6R0x8T1v9Kc2QmN4yXh7pJwLfGd5uVbE0a";
    assert!(valid_ssh_key(key));
    for bad in [
        format!("{key} alice@pc"),
        format!("{key}\n"),
        key.replace("ssh-ed25519", "ssh-rsa"),
        format!("command=\"sh\" {key}"),
        key.replacen('B', "\"", 1),
        key[..key.len() - 1].to_string(),
    ] {
        assert!(!valid_ssh_key(&bad), "{bad:?}");
    }
}

/// Each owned signer lends the matching [`Signer`]; `Debug` names the kind and the key id, never a byte.
#[test]
fn a_hub_signer_lends_its_kind_and_its_debug_shows_no_byte() {
    let (key, _) = DeviceKey::generate().expect("a key");
    let device = HubSigner::Device(key.clone());
    assert!(matches!(Signer::from(&device), Signer::Device(k) if k.key_id() == key.key_id()));
    assert_eq!(format!("{device:?}"), format!("HubSigner::Device({})", key.key_id()));
    let shared = HubSigner::Shared(b"shared-secret".to_vec());
    assert!(matches!(Signer::from(&shared), Signer::Shared(b"shared-secret")));
    assert_eq!(format!("{shared:?}"), "HubSigner::Shared");
}
