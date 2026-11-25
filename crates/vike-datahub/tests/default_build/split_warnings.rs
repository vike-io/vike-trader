//! The split warnings verb on a real server: a warning the watch found is answered over the wire until a split row of
//! the settings store covers it, then dropped at the next ask with no restart; a server whose store is not watched
//! answers none. The detection itself is `crates/vike-datahub/src/split_watch_tests.rs`'.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::split_watch::{SplitWatch, WrittenSeries};
use vike_datahub_client::DatahubClient;
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_secrets::{CredentialScope, CredentialStore, StoreLocation};

fn serve(split_watch: Option<Arc<SplitWatch>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { split_watch, ..ServerLanes::default() };
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            None,
            None,
            None,
            lanes,
            None,
            ServerTimings::PRODUCTION,
        );
    });
    addr
}

#[test]
fn a_warning_is_served_until_a_split_row_covers_it() {
    vike_datahub::test_support::hold_history_lists();
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8"))).expect("an empty store");
    let settings =
        CredentialStore::open(Some(StoreLocation::new(dir.path())), CredentialScope::Full);
    let watch = Arc::new(SplitWatch::new(Some(settings)));
    // Apple's 4-for-1 of 2020 as Dukascopy's daily bars hold it (decision 0289): Friday stored, then Monday written.
    let friday = 86_400_000_i64 * vike_utility::calendar::days_from_civil(2020, 8, 28);
    let store = HistoryStoreMemory::new();
    let stored = [flat_bar_unit_volume(friday, 499.19)];
    store.append_bars("dukascopy", "AAPL.CFD", "1d", &stored, None).expect("Friday");
    let series = WrittenSeries {
        venue: "dukascopy",
        source: None,
        symbol: "AAPL.CFD",
        interval: Some("1d"),
    };
    watch.observe(&store, series, &[flat_bar_unit_volume(friday + 3 * 86_400_000, 127.64)]);

    let mut client = DatahubClient::connect(serve(Some(Arc::clone(&watch)))).expect("handshake");
    assert!(client.serves_split_warnings(), "{:?}", client.features());
    let open = client.split_warnings().expect("read");
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(
        open[0].message(),
        "AAPL.CFD: looks like a 4-for-1 split on 2020-08-31 (price 499.19 → 127.64); run vike-cli data catalog \
         refresh --venue dukascopy"
    );

    let split =
        vike_model::StockSplit::new("dukascopy", "AAPL.CFD", "2020-08-31", 4.0).expect("a split");
    vike_secrets::stock_split::write_stock_split(dir.path(), &split).expect("the refresh's row");
    assert!(client.split_warnings().expect("read").is_empty(), "dropped once a row covers it");
}

#[test]
fn a_server_whose_store_is_not_watched_answers_none() {
    let mut client = DatahubClient::connect(serve(None)).expect("handshake");
    assert!(client.split_warnings().expect("read").is_empty());
}
