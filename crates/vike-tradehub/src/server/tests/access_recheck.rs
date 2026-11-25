//! **The per-write re-check and the journal's person** (decision 0288 items 4 and 5), on a real paper node's
//! session over a real loopback socket: a device connection's row is read again before every write, so a lowered
//! role or a revoke refuses the next write at once while its reads go on, and an accepted command lands in the
//! change journal naming the person and the device.
//!
//! The session is entered with its person already in hand (`serve_session`'s `authed`), as the handshake hands it
//! over: what is under test is the session's write path, not the signature, which `vike_hub_protocol::access`'s
//! own tests hold.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_hub_protocol::access::{ACCESS_DENIED, AccessEntry, AccessList, Person};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_tradehub_client::proto::{Request, Response};
use vike_tradehub_client::wire::{AccountRequestWire, AccountVerbWire};
use vike_tradehub_core::StrategyCorePaper;

use super::control::ControlLimitsConfig;
use super::settings::SettingsShowSource;
use super::*;
use crate::publish;

const TOKEN: &str = "ACCESS_RECHECK_TOKEN";
const RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;
const KEY_ID: &str = "nk-5e44000000000001";

type Rows = Arc<Mutex<HashMap<String, AccessEntry>>>;

fn alice() -> Person {
    Person {
        key_id: KEY_ID.into(),
        person: "alice".into(),
        device: "macbook".into(),
        role: "control".into(),
    }
}

fn set_role(rows: &Rows, role: &str) {
    rows.lock().unwrap().get_mut(KEY_ID).unwrap().role = role.into();
}

/// A paper node whose session is entered as alice's device at `scope`, over an access list the test edits, with a
/// settings source whose change journal lives in `settings_dir`.
fn session_as_alice(
    scope: Scope,
    role: &str,
    settings_dir: &std::path::Path,
) -> (StrategyCorePaper, Rows, TcpStream) {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TIMESTAMP),
    );
    let rows: Rows = Arc::new(Mutex::new(HashMap::from([(
        KEY_ID.to_string(),
        AccessEntry {
            person: "alice".into(),
            device: "macbook".into(),
            role: role.into(),
            public_key: vec![0; 32],
            revoked: false,
        },
    )])));
    let list = {
        let rows = Arc::clone(&rows);
        AccessList::new(move |key_id| rows.lock().unwrap().get(key_id).cloned())
    };
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let settings =
        Arc::new(SettingsShowSource { settings_dir: Some(settings_dir.to_path_buf()), hot: None });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr: SocketAddr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let (stream, peer) = listener.accept().expect("accept");
        serve_session(
            stream,
            publisher,
            Some(&list),
            Some(KEY_ID.to_string()),
            Some(alice()),
            commands,
            ControlLimitsConfig::default(),
            Some(settings),
            None,
            scope,
            Some(peer),
            LinkPolicy::PRODUCTION,
        );
    });
    (mount, rows, TcpStream::connect(addr).expect("connect"))
}

fn ask(stream: &mut TcpStream, request: &Request) -> Response {
    write_frame(stream, request).expect("send");
    read_frame::<_, Response>(stream).expect("the node answers")
}

fn submit(order_client_id: &str) -> Request {
    Request::Command {
        command: vike_tradehub_client::test_support::commands::resting_submit(
            order_client_id,
            TOKEN,
        ),
        reason: Some("checking the door".into()),
    }
}

fn is_access_denied(response: &Response) -> bool {
    matches!(response, Response::AuthDenied { reason } if reason == ACCESS_DENIED)
}

/// Every journal line under `<settings>/state/changes`.
fn journal_lines(settings_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let dir = settings_dir.join("state").join("changes");
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the change journal exists") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            let text = std::fs::read_to_string(&path).expect("read the journal");
            lines.extend(text.lines().map(|l| serde_json::from_str(l).expect("one JSON object")));
        }
    }
    lines
}

/// An accepted command is journalled under alice's device; her role lowered to `observe`, the next command is
/// refused while a snapshot and a ping are still answered; revoked, the same; and no refused command is journalled.
#[test]
fn a_lowered_role_or_a_revoke_refuses_the_next_write_and_reads_go_on() {
    vike_log::test_init();
    let dir = tempfile::tempdir().expect("settings dir");
    let (_mount, rows, mut stream) = session_as_alice(Scope::Write, "control", dir.path());

    match ask(&mut stream, &submit("recheck-1")) {
        Response::Ack { order_client_id, .. } => assert_eq!(order_client_id, "recheck-1"),
        other => panic!("a live control device's command must ack, got {other:?}"),
    }
    let lines = journal_lines(dir.path());
    let accepted: Vec<_> = lines.iter().filter(|v| v["kind"] == "wire_command").collect();
    assert_eq!(accepted.len(), 1, "{lines:#?}");
    let record = accepted[0];
    assert_eq!(record["target"]["verb"], "submit");
    assert_eq!(record["target"]["order_client_id"], "recheck-1");
    assert_eq!(record["reason"], "checking the door");
    assert_eq!(record["actor"]["origin"], "wire");
    assert_eq!(record["actor"]["scope"], "control");
    assert_eq!(record["actor"]["key_id"], KEY_ID);
    assert_eq!(record["actor"]["person"], "alice");
    assert_eq!(record["actor"]["device"], "macbook");

    set_role(&rows, "observe");
    assert!(
        is_access_denied(&ask(&mut stream, &submit("recheck-2"))),
        "a lowered role refuses the write"
    );
    assert!(
        matches!(ask(&mut stream, &Request::Snapshot), Response::SnapshotFrame(_)),
        "reads go on"
    );
    assert!(matches!(ask(&mut stream, &Request::Ping), Response::Pong));

    set_role(&rows, "control");
    rows.lock().unwrap().get_mut(KEY_ID).unwrap().revoked = true;
    assert!(
        is_access_denied(&ask(&mut stream, &submit("recheck-3"))),
        "a revoke refuses the write"
    );
    assert!(
        matches!(ask(&mut stream, &Request::Snapshot), Response::SnapshotFrame(_)),
        "reads go on"
    );

    let journalled =
        journal_lines(dir.path()).iter().filter(|v| v["kind"] == "wire_command").count();
    assert_eq!(journalled, 1, "a refused command is journalled nowhere");
}

/// An `admin` device's account verb passes the re-check to the account plane's own admission (this node declares
/// no barrier, so that refusal is the plane's words); its role lowered to `control`, the verb is refused at the
/// re-check instead.
#[test]
fn an_admin_lowered_to_control_is_refused_the_account_plane() {
    vike_log::test_init();
    let dir = tempfile::tempdir().expect("settings dir");
    let (_mount, rows, mut stream) = session_as_alice(Scope::Account, "admin", dir.path());
    let list = || {
        Request::Account(Box::new(AccountRequestWire {
            verb: AccountVerbWire::List,
            confirm: None,
        }))
    };

    match ask(&mut stream, &list()) {
        Response::Error(reason) => {
            assert_ne!(reason, ACCESS_DENIED, "the admission's words, not the re-check's")
        }
        other => panic!("an undeclared box refuses the account plane, got {other:?}"),
    }
    set_role(&rows, "control");
    match ask(&mut stream, &list()) {
        Response::Error(reason) => assert_eq!(reason, ACCESS_DENIED),
        other => panic!("a lowered admin is refused at the re-check, got {other:?}"),
    }
}
