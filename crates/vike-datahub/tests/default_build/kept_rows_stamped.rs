//! **`KeptRowsOfVenues` on a real server, read through the real `HistoryStoreDatahub`** (the owner's "yes both" of
//! 2026-11-19): the binary, stamped rows are the JSON verb's rows byte for byte once encoded as a sandboxed child reads
//! them (several venues, their splits in the JSON answer's order, every float by its bits), and the typed read is the
//! JSON verb's too; the JSON verb an older backtesthub asks is still answered beside it; a second run of the same
//! venues moves its stamp alone over the wire (counted on a proxy), and a write moves the rows again.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use vike_catalog::VenueInstrumentList;
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::instrument_lists::write_list;
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::proto::kept_msgpack;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_model::{Contract, Instrument, StockSplit};
use vike_secrets::{CredentialScope, CredentialStore, StoreLocation};

struct Hub {
    settings: tempfile::TempDir,
    instruments: tempfile::TempDir,
    addr: SocketAddr,
}

/// A key-less datahub with a project's kept lists and a settings store.
fn serve() -> Hub {
    let settings = tempfile::tempdir().expect("temp");
    let instruments = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let settings_store =
        CredentialStore::open(Some(StoreLocation::new(settings.path())), CredentialScope::Full);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let lanes = ServerLanes {
        settings_store: Some(settings_store),
        instruments: Some(instruments.path().to_path_buf()),
        ..ServerLanes::default()
    };
    thread::spawn(move || {
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
    Hub { settings, instruments, addr }
}

/// `n` rows of `venue`, with floats a text form could round: a third, a sum that is not its decimal, the smallest
/// subnormal.
fn list(venue: &str, n: usize) -> VenueInstrumentList {
    let instruments = (0..n)
        .map(|i| {
            let mut row =
                Instrument::new(venue, format!("SYM{i}"), "BTC", "USDT", Contract::CryptoSpot);
            row.properties.tick_size = 0.1 + 0.2 * i as f64;
            row.venue_figures.insert("scalingFactor".into(), 1.0 / (3.0 + i as f64));
            row.venue_figures.insert("tiny".into(), 5e-324);
            row
        })
        .collect();
    VenueInstrumentList {
        venue: venue.into(),
        refreshed_ms: Some(1_794_124_800_000),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1_794_124_800_000),
        instruments,
        added_by_operator: Vec::new(),
    }
}

fn names(venues: &[&str]) -> Vec<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

#[test]
fn the_stamped_rows_are_the_json_verbs_rows_byte_for_byte() {
    let hub = serve();
    for (venue, n) in [("okx", 3), ("binance", 20), ("dukascopy", 2)] {
        write_list(hub.instruments.path(), &list(venue, n)).expect("a list");
    }
    for (venue, symbol) in
        [("okx", "SYM1"), ("binance", "SYM9"), ("dukascopy", "SYM1"), ("binance", "SYM0")]
    {
        let split = StockSplit::new(venue, symbol, "2020-08-31", 4.0).expect("a split");
        vike_secrets::stock_split::write_stock_split(hub.settings.path(), &split).expect("row");
    }
    let remote = HistoryStoreDatahub::new(hub.addr.to_string());
    let mut client = DatahubClient::connect(hub.addr).expect("handshake");
    assert!(client.serves_kept_rows_stamped(), "advertised: {:?}", client.features());
    let asks: [&[&str]; 5] = [
        &["binance"],
        &["okx", "binance", "dukascopy"],
        &["dukascopy", "okx", "okx"],
        &["deribit"],
        &["binance", "no-such-venue"],
    ];
    for asked in asks {
        let asked = names(asked);
        let json =
            client.instrument_lists_of_venues(&asked).expect("the JSON verb, still answered");
        let json_rows: Vec<Instrument> =
            json.venues.into_iter().flat_map(|l| l.instruments).collect();
        let encoded =
            remote.kept_instruments_and_splits_encoded_of(&asked).expect("served").expect("read");
        assert_eq!(encoded.rows, kept_msgpack(&json_rows).expect("rows"), "{asked:?}");
        assert_eq!(encoded.splits, kept_msgpack(&json.splits).expect("splits"), "{asked:?}");
        let (rows, splits) = remote.kept_instruments_and_splits_of(&asked);
        assert_eq!(rows.expect("rows"), Some(json_rows), "{asked:?}");
        assert_eq!(splits.expect("splits"), Some(json.splits), "{asked:?}");
    }
}

/// A proxy in front of `upstream` that counts the bytes it hands back.
fn counting_proxy(upstream: SocketAddr) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let down = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&down);
    thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { return };
            let mut server = TcpStream::connect(upstream).expect("upstream");
            let mut up = (client.try_clone().expect("clone"), server.try_clone().expect("clone"));
            thread::spawn(move || {
                let _ = std::io::copy(&mut up.0, &mut up.1);
            });
            let counted = Arc::clone(&counted);
            thread::spawn(move || {
                let mut buffer = [0u8; 65_536];
                while let Ok(n @ 1..) = server.read(&mut buffer) {
                    counted.fetch_add(n, Ordering::SeqCst);
                    if client.write_all(&buffer[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, down)
}

#[test]
fn a_second_run_moves_its_stamp_alone_and_a_write_moves_the_rows_again() {
    let hub = serve();
    write_list(hub.instruments.path(), &list("binance", 2_000)).expect("a list");
    let (proxy, down) = counting_proxy(hub.addr);
    let remote = HistoryStoreDatahub::new(proxy.to_string());
    let binance = names(&["binance"]);

    let first =
        remote.kept_instruments_and_splits_encoded_of(&binance).expect("served").expect("read");
    let after_first = down.load(Ordering::SeqCst);
    assert!(after_first > first.rows.len(), "the first run moves the rows: {after_first} bytes");

    let second =
        remote.kept_instruments_and_splits_encoded_of(&binance).expect("served").expect("read");
    let moved = down.load(Ordering::SeqCst) - after_first;
    assert_eq!(second, first, "the second run reads the same rows");
    assert!(
        moved < 200,
        "and moves its stamp alone: {moved} bytes, the rows are {}",
        first.rows.len()
    );
    let (rows, _) = remote.kept_instruments_and_splits_of(&binance);
    assert_eq!(rows.expect("rows").expect("kept").len(), 2_000, "a typed read, off the same bytes");

    write_list(hub.instruments.path(), &list("binance", 3)).expect("a refresh's write");
    let before = down.load(Ordering::SeqCst);
    let (rows, _) = remote.kept_instruments_and_splits_of(&binance);
    assert_eq!(rows.expect("rows").expect("kept").len(), 3, "the written rows, at the next run");
    assert!(down.load(Ordering::SeqCst) - before > 200, "moved again");
}
