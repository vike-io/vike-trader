//! Decision 0288 item 4 on the data plane: a connection a DEVICE key signed has its access row read again before
//! every WRITE verb, so a lowered role or a revoke refuses the next write with `access denied` while its reads go
//! on; a read is never re-checked. The two writes on the Read side (a CSV upload, a chart gap seed) are re-checked
//! too. And the login itself is bound to THIS hub: one signed for another hub is refused (0288's amendment).

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;
use vike_datahub::test_support::{device_stream, person_auth};
use vike_datahub_client::archive::ImportTarget;
use vike_datahub_client::proto::{request_kind, writes_under_read_scope};
use vike_datahub_client::upload::CsvUploadSpec;
use vike_hub_protocol::access::{
    ACCESS_DENIED, AccessEntry, AccessList, DeviceKey, NO_DEVICE_REGISTERED,
};
use vike_hub_protocol::auth::{BACKTESTHUB_DOMAIN, Domain};

type Rows = Arc<Mutex<HashMap<String, AccessEntry>>>;

/// One live row for `key` under `role`, and the list that reads it afresh at every call (as a hub's does).
fn listed(key: &DeviceKey, role: &str) -> (Rows, AccessList) {
    let rows: Rows = Arc::new(Mutex::new(HashMap::from([(
        key.key_id(),
        AccessEntry {
            person: "alice".into(),
            device: "macbook".into(),
            role: role.into(),
            public_key: key.public_key().to_vec(),
            revoked: false,
        },
    )])));
    let list = {
        let rows = Arc::clone(&rows);
        AccessList::new(move |key_id| rows.lock().unwrap().get(key_id).cloned())
    };
    (rows, list)
}

/// A dry-run removal: a WRITE verb (`Scope::Write`) this server answers without touching anything.
fn write_verb() -> Request {
    Request::DeleteSeries {
        selector: SeriesSelector::new(StoreKindId::Bar, "binance"),
        produced_by: Some("klines:".to_string()),
        dry_run: true,
    }
}

fn is_access_denied(response: &Response) -> bool {
    matches!(response, Response::Error(reason) if reason == ACCESS_DENIED)
}

#[test]
fn a_lowered_role_or_a_revoke_refuses_the_next_write_and_reads_go_on() {
    let (key, _) = DeviceKey::generate().expect("a device key");
    let (rows, list) = listed(&key, "control");
    let addr = spawn(Some(keys().with_access(list)));
    let mut s = device_stream(addr, &key, Scope::Write);

    let first = exchange(&mut s, &write_verb());
    assert!(!is_access_denied(&first), "a live control device's write is served: {first:?}");

    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().role = "observe".into();
    assert!(is_access_denied(&exchange(&mut s, &write_verb())), "a lowered role refuses the write");
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong), "reads go on");

    {
        let mut rows = rows.lock().unwrap();
        let row = rows.get_mut(&key.key_id()).unwrap();
        row.role = "control".into();
        row.revoked = true;
    }
    assert!(is_access_denied(&exchange(&mut s, &write_verb())), "a revoke refuses the write");
    assert!(
        !matches!(exchange(&mut s, &Request::ListSeries), Response::Error(_)),
        "reads go on after a revoke"
    );
}

/// Decision 0288 item 8 (the owner: never cut security): the SERVED datahub always authenticates, by the list over
/// its settings directory (`vike_datahub::server::access_list`). An EMPTY list refuses every device, saying how to
/// register the first; the list is read at every handshake, never decided once at boot, so the first row written
/// admits its device on the SAME running server.
#[test]
fn an_empty_access_list_refuses_every_device_until_its_first_row_is_written() {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 temp path")))
        .expect("an empty settings database");
    let list = vike_datahub::server::access_list(dir.path().to_path_buf());
    let addr = spawn(Some(HubKeys::new(Vec::new(), Vec::new()).with_access(list)));
    let (key, _) = DeviceKey::generate().expect("a device key");

    match person_auth(addr, &key, DATAHUB_DOMAIN, Scope::Read).1 {
        HandshakeReply::AuthDenied { reason } => assert_eq!(reason, NO_DEVICE_REGISTERED),
        other => panic!("an empty list admits nobody: {other:?}"),
    }

    vike_secrets::access::add_access_key(
        dir.path(),
        &vike_secrets::access::AccessKeyRow {
            key_id: key.key_id(),
            person: "owner".into(),
            device: "pc".into(),
            role: "observe".into(),
            public_key: key.public_key().to_vec(),
            granted_ms: 1,
            revoked_ms: None,
        },
    )
    .expect("the first row is written");
    let mut s = device_stream(addr, &key, Scope::Read);
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong), "the listed device reads");
}

/// **The writes on the Read side are re-checked too** (finding 6): an observe device's CSV upload and chart gap seed
/// reach their lanes (unmounted here, so each lane answers for itself), and once the device is revoked the SAME open
/// connection is refused both with `access denied`, before either lane is asked; its reads go on.
#[test]
fn a_revoke_refuses_the_next_upload_and_seed_on_the_open_connection() {
    let (key, _) = DeviceKey::generate().expect("a device key");
    let (rows, list) = listed(&key, "observe");
    let addr = spawn(Some(keys().with_access(list)));
    let mut s = device_stream(addr, &key, Scope::Read);

    let upload = Request::CsvUpload(CsvUploadSpec {
        target: ImportTarget {
            venue: "user".into(),
            symbol: "USDT-USD".into(),
            interval: "1d".into(),
            label: "rates".into(),
            file_format: Default::default(),
            columns: None,
            sparse: false,
            allow_coarse_grid: false,
        },
        bytes: 10,
        dry_run: true,
        head: false,
    });
    let mut writes: Vec<Request> =
        every_request().into_iter().filter(writes_under_read_scope).collect();
    writes.push(upload);
    let kinds: Vec<&str> = writes.iter().map(request_kind).collect();
    assert_eq!(kinds, ["SeedSeries", "CsvUpload"], "the sample set's read-side writes");

    for write in &writes {
        let answer = exchange(&mut s, write);
        assert!(!is_access_denied(&answer), "a live device reaches the lane: {answer:?}");
    }
    rows.lock().unwrap().get_mut(&key.key_id()).unwrap().revoked = true;
    for write in &writes {
        let answer = exchange(&mut s, write);
        assert!(is_access_denied(&answer), "a revoked device's {write:?} answered {answer:?}");
    }
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong), "reads go on");
}

/// **One signature domain per hub** (decision 0288's amendment): a device's login signed for the BACKTESTHUB or the
/// tradehub, over this server's own nonce and version, is refused here, so a client lured into signing a nonce
/// relayed from this hub cannot be replayed onto it. The same login signed for the datahub is admitted.
#[test]
fn a_login_signed_for_another_hub_is_refused_and_one_for_this_hub_admitted() {
    let (key, _) = DeviceKey::generate().expect("a device key");
    let (_rows, list) = listed(&key, "control");
    let addr = spawn(Some(keys().with_access(list)));
    // The tradehub's separator, spelled out: this crate takes no edge to that client.
    for other in [BACKTESTHUB_DOMAIN, Domain::new(b"vike-tradehub-auth\0")] {
        let (_, answer) = person_auth(addr, &key, other, Scope::Write);
        assert!(
            matches!(&answer, HandshakeReply::AuthDenied { reason } if reason == ACCESS_DENIED),
            "{other:?}: {answer:?}"
        );
    }
    let (_, answer) = person_auth(addr, &key, DATAHUB_DOMAIN, Scope::Write);
    assert!(matches!(answer, HandshakeReply::AuthOk { scope: Scope::Write, .. }), "{answer:?}");
}

/// **The desktop's dial, against the SERVED datahub** (the CI box, 2026-11-16): the keys a desktop signs every datahub
/// dial with are its device key alone (`vike_app_core::backend::registry::datahub_dial_keys`), and the
/// served datahub holds no shared key, only the list. That dial is admitted at `Read`, the scope every desktop
/// datahub path asks for. The dial a desktop made before its key existed carries none: it hangs up after the
/// `Welcome`, and is the refusal that line now names.
#[test]
fn the_desktops_device_key_dial_is_admitted_and_a_keyless_one_refused() {
    let (key, _) = DeviceKey::generate().expect("a device key");
    let (_rows, list) = listed(&key, "admin");
    let addr = spawn(Some(HubKeys::new(Vec::new(), Vec::new()).with_access(list)));

    let desktop = HubKeys::of_device(key);
    let client = DatahubClient::connect_with_keys(addr, Some(&desktop), Scope::Read)
        .expect("the desktop's device-key dial is admitted");
    assert_eq!(client.authenticated_scope(), Some(Scope::Read));

    let refused = DatahubClient::connect_with_keys(addr, None, Scope::Read)
        .expect_err("a key-less dial is refused by a keyed datahub");
    assert!(
        matches!(refused, vike_hub_protocol::call::HubCallError::AccessDenied(_)),
        "{refused:?}"
    );
}
