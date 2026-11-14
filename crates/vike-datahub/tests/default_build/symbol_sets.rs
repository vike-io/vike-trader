//! The three symbol-list verbs on a real server over a real settings store: a list saved over the wire is a row the
//! next client reads, a delete removes it, a refused list writes nothing, and a server with no store says so in words.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::datasets::{SymbolSet, SymbolSetStore};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::DatahubClient;
use vike_secrets::{CredentialScope, CredentialStore, StoreLocation};

fn serve(settings_store: Option<CredentialStore>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { settings_store, ..ServerLanes::default() };
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            None,
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    addr
}

fn planted() -> (tempfile::TempDir, CredentialStore) {
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8"))).expect("an empty store");
    let handle = CredentialStore::open(Some(StoreLocation::new(dir.path())), CredentialScope::Full);
    (dir, handle)
}

fn alts() -> SymbolSet {
    SymbolSet {
        name: "My Alts".into(),
        symbols: vec!["SOLUSDT".into(), "AVAXUSDT".into()],
        provider: "Auto".into(),
        interval: "1h".into(),
        benchmark: "BTCUSDT".into(),
        user: true,
    }
}

#[test]
fn a_list_saved_over_the_wire_is_read_back_and_deleted_by_name() {
    let (_dir, handle) = planted();
    let addr = serve(Some(handle));
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_symbol_sets(), "{:?}", client.features());
    assert!(client.symbol_sets().expect("read").is_empty(), "the table starts empty");
    assert!(
        SymbolSetStore::from_server(Vec::new()).sets.is_empty(),
        "no list is compiled in, so the client shows none"
    );

    assert_eq!(client.set_symbol_set(alts()).expect("saved"), vec![alts()]);
    let mut fewer = alts();
    fewer.symbols.truncate(1);
    assert_eq!(client.set_symbol_set(fewer.clone()).expect("saved over"), vec![fewer.clone()]);

    let mut other = DatahubClient::connect(addr).expect("a second client");
    assert_eq!(other.symbol_sets().expect("read"), vec![fewer], "a row, not a connection's memory");
    assert!(other.delete_symbol_set("My Alts").expect("deleted").is_empty());
    assert!(other.delete_symbol_set("My Alts").expect("a name no list has").is_empty());
}

#[test]
fn a_list_without_a_name_is_refused_and_writes_nothing() {
    let (_dir, handle) = planted();
    let mut client = DatahubClient::connect(serve(Some(handle))).expect("handshake");
    let mut nameless = alts();
    nameless.name = "  ".into();
    let err = client.set_symbol_set(nameless).expect_err("refused");
    assert!(err.contains("needs a name"), "{err}");
    assert!(client.symbol_sets().expect("read").is_empty());
}

#[test]
fn a_server_with_no_settings_store_answers_in_words() {
    let mut client = DatahubClient::connect(serve(None)).expect("handshake");
    let err = client.symbol_sets().expect_err("no store");
    assert!(err.contains("no settings store"), "{err}");
}
