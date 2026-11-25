//! **The access list, this device's key and the invitations** (decision 0288 items 2 and 7), through the public
//! writers and readers: a grant, its idempotent re-run, a role, a revoke that is never undone, an invitation that
//! registers ONCE and only before it expires, the swap, and a box with no store refused.

use vike_secrets::StoreErrorKind;
use vike_secrets::access::{
    AccessAdded, AccessKeyRow, DeviceAdded, RegisterOutcome, SwapOutcome, access_list_held,
    add_access_invite, add_access_key, device_key_grant, device_key_insert, device_key_load,
    device_key_named, find_access_key, list_access_invites, list_access_keys, register_access,
    revoke_access, set_access_role, swap_access_invite,
};

use crate::support::Fixture;

const NOW: i64 = 1_800_000_000_000;
const SECRET: &[u8] = b"the invitation's auth secret";
const SSH: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

fn row(key_id: &str, person: &str, device: &str, role: &str) -> AccessKeyRow {
    AccessKeyRow {
        key_id: key_id.into(),
        person: person.into(),
        device: device.into(),
        role: role.into(),
        public_key: vec![7; 32],
        granted_ms: NOW,
        revoked_ms: None,
    }
}

#[test]
fn a_grant_is_idempotent_for_its_own_key_and_refused_for_another() {
    let fx = Fixture::initialised();
    let pc = row("nk-1", "owner", "pc", "admin");
    assert_eq!(add_access_key(fx.dir(), &pc).expect("add"), AccessAdded::Added);
    assert_eq!(add_access_key(fx.dir(), &pc).expect("again"), AccessAdded::Held, "a re-connect");
    assert_eq!(
        add_access_key(fx.dir(), &row("nk-2", "owner", "pc", "admin")).expect("another key"),
        AccessAdded::DeviceTaken
    );
    assert_eq!(
        add_access_key(fx.dir(), &row("nk-1", "owner", "laptop", "admin")).expect("same key"),
        AccessAdded::KeyTaken
    );
    assert_eq!(find_access_key(fx.dir(), "nk-1").expect("find"), Some(pc));
    assert_eq!(find_access_key(fx.dir(), "nk-9").expect("find"), None);
}

/// What the hubs ask before a device's handshake (decision 0288's cutover): a list with no row admits nobody, and a
/// revoked row still makes the list HELD (the person was registered; the row refuses them by itself). A named
/// device's own key is read by (person, device), never by position.
#[test]
fn the_list_is_held_from_its_first_row_and_a_named_device_key_is_its_own() {
    let fx = Fixture::initialised();
    assert!(!access_list_held(fx.dir()).expect("an empty list reads"), "no row: not held");
    assert_eq!(
        add_access_key(fx.dir(), &row("nk-1", "owner", "pc", "admin")).expect("add"),
        AccessAdded::Added
    );
    assert!(access_list_held(fx.dir()).expect("read"), "one row: held");
    revoke_access(fx.dir(), "owner", None, NOW).expect("revoke");
    assert!(access_list_held(fx.dir()).expect("read"), "a revoked row still holds the list");

    assert_eq!(
        device_key_insert(fx.dir(), "nk-s", "owner", "server", b"pkcs8-s").expect("s"),
        DeviceAdded::Added
    );
    assert_eq!(
        device_key_insert(fx.dir(), "nk-t", "owner", "telegram", b"pkcs8-t").expect("t"),
        DeviceAdded::Added
    );
    let telegram = device_key_named(fx.dir(), "owner", "telegram").expect("read").expect("held");
    assert_eq!(
        (telegram.key_id.as_str(), telegram.private_key.as_slice()),
        ("nk-t", &b"pkcs8-t"[..])
    );
    assert_eq!(
        device_key_load(fx.dir()).expect("read").expect("held").key_id,
        "nk-s",
        "the box's first"
    );
    assert!(device_key_named(fx.dir(), "owner", "laptop").expect("read").is_none());
}

#[test]
fn a_role_moves_live_rows_and_a_revoke_is_never_undone() {
    let fx = Fixture::initialised();
    for (id, device) in [("nk-1", "macbook"), ("nk-2", "phone")] {
        add_access_key(fx.dir(), &row(id, "alice", device, "observe")).expect("add");
    }
    add_access_key(fx.dir(), &row("nk-3", "bob", "pc", "observe")).expect("add");

    assert_eq!(set_access_role(fx.dir(), "alice", Some("phone"), "control").expect("role"), 1);
    assert_eq!(find_access_key(fx.dir(), "nk-2").expect("find").expect("row").role, "control");
    assert_eq!(find_access_key(fx.dir(), "nk-1").expect("find").expect("row").role, "observe");

    assert_eq!(revoke_access(fx.dir(), "alice", Some("phone"), NOW + 1).expect("one"), 1);
    assert_eq!(revoke_access(fx.dir(), "alice", None, NOW + 2).expect("the rest"), 1);
    assert_eq!(revoke_access(fx.dir(), "alice", None, NOW + 3).expect("none left"), 0);
    assert_eq!(set_access_role(fx.dir(), "alice", None, "admin").expect("no live row"), 0);
    let revoked = find_access_key(fx.dir(), "nk-2").expect("find").expect("kept");
    assert_eq!((revoked.revoked_ms, revoked.role.as_str()), (Some(NOW + 1), "control"));
    assert_eq!(
        add_access_key(fx.dir(), &row("nk-2", "alice", "phone", "control")).expect("re-add"),
        AccessAdded::KeyTaken,
        "a revoked key is never re-granted"
    );
    assert_eq!(find_access_key(fx.dir(), "nk-3").expect("find").expect("bob").revoked_ms, None);
    assert_eq!(list_access_keys(fx.dir()).expect("list").len(), 3);
}

#[test]
fn a_role_outside_the_three_is_refused_by_the_table() {
    let fx = Fixture::initialised();
    assert!(add_access_key(fx.dir(), &row("nk-1", "owner", "pc", "trade")).is_err());
    assert!(list_access_keys(fx.dir()).expect("list").is_empty());
}

#[test]
fn an_invitation_registers_once_before_it_expires_and_a_refusal_writes_nothing() {
    let fx = Fixture::initialised();
    let expires = NOW + 15 * 60 * 1000;
    assert!(
        add_access_invite(fx.dir(), "0a1b2c3d", "alice", "observe", SECRET, expires).expect("mint")
    );
    assert!(
        !add_access_invite(fx.dir(), "0a1b2c3d", "bob", "admin", SECRET, expires)
            .expect("taken id"),
        "an id is never minted twice"
    );
    let register = |id: &str, device: &str, key_id: &str, now: i64, proof: bool| {
        register_access(fx.dir(), id, device, &[9; 32], key_id, SSH, now, |held| {
            assert_eq!(held, SECRET, "the proof is checked under the row's auth secret");
            proof
        })
        .expect("register")
    };

    assert_eq!(register("ffffffff", "macbook", "nk-a", NOW, true), RegisterOutcome::Unknown);
    assert_eq!(register("0a1b2c3d", "macbook", "nk-a", NOW, false), RegisterOutcome::WrongProof);
    assert_eq!(register("0a1b2c3d", "macbook", "nk-a", expires, true), RegisterOutcome::Expired);
    assert!(list_access_keys(fx.dir()).expect("list").is_empty(), "no refusal wrote a row");

    add_access_key(fx.dir(), &row("nk-taken", "carol", "pc", "observe")).expect("add");
    assert_eq!(
        register("0a1b2c3d", "macbook", "nk-taken", NOW, true),
        RegisterOutcome::DeviceTaken
    );

    assert_eq!(
        register("0a1b2c3d", "macbook", "nk-a", expires - 1, true),
        RegisterOutcome::Registered { person: "alice".into(), role: "observe".into() }
    );
    let granted = find_access_key(fx.dir(), "nk-a").expect("find").expect("the row");
    assert_eq!((granted.person.as_str(), granted.device.as_str()), ("alice", "macbook"));
    assert_eq!((granted.public_key, granted.granted_ms), (vec![9; 32], expires - 1));
    assert_eq!(
        register("0a1b2c3d", "laptop", "nk-b", NOW, true),
        RegisterOutcome::AlreadyRegistered,
        "single use"
    );
    let invite = &list_access_invites(fx.dir()).expect("invites")[0];
    assert_eq!((invite.registered_ms, invite.key_id.as_deref()), (Some(expires - 1), Some("nk-a")));
}

#[test]
fn a_person_revoked_whole_loses_their_open_invitations() {
    let fx = Fixture::initialised();
    add_access_invite(fx.dir(), "0a1b2c3d", "alice", "observe", SECRET, NOW + 1000).expect("mint");
    add_access_invite(fx.dir(), "11111111", "bob", "observe", SECRET, NOW + 1000).expect("mint");
    revoke_access(fx.dir(), "alice", Some("macbook"), NOW).expect("one device");
    revoke_access(fx.dir(), "alice", None, NOW).expect("the person");
    let register = |id: &str| {
        register_access(
            fx.dir(),
            id,
            "macbook",
            &[9; 32],
            &format!("nk-{id}"),
            SSH,
            NOW + 1,
            |_| true,
        )
        .expect("register")
    };
    assert_eq!(register("0a1b2c3d"), RegisterOutcome::Expired);
    assert!(matches!(register("11111111"), RegisterOutcome::Registered { .. }), "bob's stays open");
}

#[test]
fn a_swap_needs_a_registration_and_happens_once() {
    let fx = Fixture::initialised();
    assert_eq!(swap_access_invite(fx.dir(), "0a1b2c3d", NOW).expect("swap"), SwapOutcome::Unknown);
    add_access_invite(fx.dir(), "0a1b2c3d", "alice", "control", SECRET, NOW + 1000).expect("mint");
    assert_eq!(
        swap_access_invite(fx.dir(), "0a1b2c3d", NOW).expect("swap"),
        SwapOutcome::NotRegistered
    );
    register_access(fx.dir(), "0a1b2c3d", "macbook", &[9; 32], "nk-a", SSH, NOW, |_| true)
        .expect("register");
    assert_eq!(
        swap_access_invite(fx.dir(), "0a1b2c3d", NOW + 1).expect("swap"),
        SwapOutcome::Swapped {
            person: "alice".into(),
            device: "macbook".into(),
            ssh_key: SSH.into()
        }
    );
    assert_eq!(
        swap_access_invite(fx.dir(), "0a1b2c3d", NOW + 2).expect("swap"),
        SwapOutcome::AlreadySwapped
    );
    assert_eq!(list_access_invites(fx.dir()).expect("invites")[0].swapped_ms, Some(NOW + 1));
}

/// A revoke that lands between the registration and the swap (of the device, or of the whole person) leaves the swap
/// nothing to hand out: refused, and the invitation stays unswapped.
#[test]
fn a_swap_after_a_revoke_owes_no_line() {
    for device in [Some("macbook"), None] {
        let fx = Fixture::initialised();
        add_access_invite(fx.dir(), "0a1b2c3d", "alice", "observe", SECRET, NOW + 1000)
            .expect("mint");
        register_access(fx.dir(), "0a1b2c3d", "macbook", &[9; 32], "nk-a", SSH, NOW, |_| true)
            .expect("register");
        assert_eq!(revoke_access(fx.dir(), "alice", device, NOW + 1).expect("revoke"), 1);
        assert_eq!(
            swap_access_invite(fx.dir(), "0a1b2c3d", NOW + 2).expect("swap"),
            SwapOutcome::Revoked,
            "{device:?}"
        );
        assert_eq!(list_access_invites(fx.dir()).expect("invites")[0].swapped_ms, None);
    }
}

#[test]
fn a_device_key_is_stored_once_and_a_revoked_grant_is_never_renewed() {
    let fx = Fixture::initialised();
    assert_eq!(device_key_load(fx.dir()).expect("load"), None);
    let server = row("nk-s", "owner", "server", "admin");
    assert_eq!(device_key_grant(fx.dir(), b"pkcs8-s", &server).expect("grant"), DeviceAdded::Added);
    assert_eq!(device_key_grant(fx.dir(), b"pkcs8-x", &server).expect("again"), DeviceAdded::Held);
    assert_eq!(
        device_key_insert(fx.dir(), "nk-b", "server", "backtesthub", b"pkcs8-b").expect("second"),
        DeviceAdded::Added
    );
    let first = device_key_load(fx.dir()).expect("load").expect("a key");
    assert_eq!((first.key_id.as_str(), first.private_key.as_slice()), ("nk-s", &b"pkcs8-s"[..]));
    assert!(!format!("{first:?}").contains("pkcs8"), "Debug shows no private byte: {first:?}");
    assert_eq!(find_access_key(fx.dir(), "nk-s").expect("find"), Some(server.clone()));

    revoke_access(fx.dir(), "owner", Some("server"), NOW + 1).expect("revoke");
    assert_eq!(
        device_key_grant(fx.dir(), b"pkcs8-s", &server).expect("start"),
        DeviceAdded::Revoked
    );

    let other = Fixture::initialised();
    add_access_key(other.dir(), &row("nk-other", "owner", "server", "admin")).expect("listed");
    assert_eq!(
        device_key_grant(other.dir(), b"pkcs8", &server).expect("grant"),
        DeviceAdded::DeviceTaken
    );
    revoke_access(other.dir(), "owner", None, NOW).expect("revoke");
    assert_eq!(
        device_key_grant(other.dir(), b"pkcs8", &server).expect("grant"),
        DeviceAdded::Revoked
    );
    assert_eq!(device_key_load(other.dir()).expect("load"), None, "nothing was stored");
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let refused = |e: vike_secrets::StoreError| {
        assert!(matches!(e.kind, StoreErrorKind::NoSettingsDatabase), "{e}");
    };
    refused(add_access_key(fx.dir(), &row("nk-1", "owner", "pc", "admin")).expect_err("no store"));
    refused(
        add_access_invite(fx.dir(), "0a1b2c3d", "a", "observe", SECRET, NOW).expect_err("no store"),
    );
    refused(device_key_insert(fx.dir(), "nk-1", "owner", "pc", b"k").expect_err("no store"));
    assert_eq!(
        register_access(fx.dir(), "0a1b2c3d", "pc", &[0; 32], "nk-1", SSH, NOW, |_| true)
            .expect("no store"),
        RegisterOutcome::Unknown
    );
    assert_eq!(find_access_key(fx.dir(), "nk-1").expect("no store reads none"), None);
    assert!(list_access_keys(fx.dir()).expect("no store").is_empty());
    assert_eq!(device_key_load(fx.dir()).expect("no store"), None);
    assert!(!fx.db().exists(), "nothing created the database");
}
