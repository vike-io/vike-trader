//! The `GatewayLinks` verb on a real server, over a real settings store, with no gatewayhub anywhere: a row the writer
//! committed reaches a real client, and a store that will not read is never an empty history.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::DatahubClient;
use vike_secrets::link_sessions::{LinkEvent, LinkKey, PLANE_TRADE, write_link_events};
use vike_secrets::{AccountEdit, CredentialScope, CredentialStore, StoreLocation};

fn serve(settings_store: Option<CredentialStore>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { settings_store, ..ServerLanes::default() };
        let timings = ServerTimings::PRODUCTION;
        let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, timings);
    });
    addr
}

fn handle(dir: &std::path::Path) -> CredentialStore {
    CredentialStore::open(Some(StoreLocation::new(dir)), CredentialScope::NoVenueKeys)
}

fn key(route: &str) -> LinkKey {
    LinkKey {
        venue: "aster".to_string(),
        route: route.to_string(),
        plane: PLANE_TRADE,
        lane: "aster-userdata".to_string(),
    }
}

/// A real store, whose `venue` rows the first account's write topped up.
fn planted() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8"))).expect("an empty store");
    vike_secrets::edit_account_in_unjournalled(
        dir.path(),
        AccountEdit::Create { venue: "aster", tier: vike_model::AccountTier::Demo, label: None },
    )
    .expect("an account");
    dir
}

#[test]
fn a_committed_session_reaches_a_client_with_no_gatewayhub_anywhere_under_the_hubs_narrowest_scope()
{
    let dir = planted();
    write_link_events(
        dir.path(),
        &[
            LinkEvent::Opened { key: key("aster"), at_ms: 1_000 },
            LinkEvent::Closed {
                key: key("aster"),
                at_ms: 907_000,
                how:
                    "wss://fstream.asterdex.com/ws/pqia91ma19a5s61cv6a81va65sdf19v8a65a1a5s closed"
                        .to_string(),
                ours: false,
            },
            LinkEvent::Opened { key: key("aster"), at_ms: 908_500 },
            LinkEvent::Opened { key: key("aster#OTHER"), at_ms: 2_000 },
        ],
    )
    .expect("write");

    let mut client = DatahubClient::connect(serve(Some(handle(dir.path())))).expect("handshake");
    assert!(client.serves_gateway_links(), "{:?}", client.features());
    let reply = client.gateway_links(0, Some("aster".to_string()), None).expect("answered");
    assert!(!reply.truncated);
    assert_eq!(reply.rows.len(), 2, "{:?}", reply.rows);
    let first = &reply.rows[1];
    assert_eq!((first.lived_s, first.next_open_s), (Some(906.0), Some(1.5)));
    let how = first.how.as_deref().expect("how");
    assert!(!how.contains("://") && !how.contains("pqia91"), "{how}");
    assert_eq!(client.gateway_links(0, None, None).expect("answered").rows.len(), 3);
    assert_eq!(
        client.gateway_links(0, None, Some("aster".to_string())).expect("answered").rows.len(),
        3,
        "a bare venue is every book of it"
    );
    assert_eq!(
        client.gateway_links(1_000_000, None, None).expect("answered").rows.len(),
        2,
        "a session that ended before the window goes, and one with no end stays"
    );
}

#[test]
fn a_store_with_no_table_yet_is_an_empty_answer_and_a_broken_one_is_an_error_never_an_empty_history()
 {
    let dir = planted();
    let mut client = DatahubClient::connect(serve(Some(handle(dir.path())))).expect("handshake");
    assert!(client.gateway_links(0, None, None).expect("answered").rows.is_empty());

    let broken = tempfile::tempdir().expect("temp");
    let db = vike_secrets::db_path_in(broken.path());
    std::fs::create_dir_all(db.parent().expect("a parent")).expect("dir");
    std::fs::write(&db, b"not a database, and long enough to be read as a header").expect("plant");
    let mut client = DatahubClient::connect(serve(Some(handle(broken.path())))).expect("handshake");
    let why = client.gateway_links(0, None, None).expect_err("an error in words");
    assert!(why.contains("could not be read"), "{why}");

    let mut none = DatahubClient::connect(serve(None)).expect("handshake");
    assert!(none.gateway_links(0, None, None).expect_err("words").contains("no settings store"));
}
