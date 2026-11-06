//! A `source=` lane over the real wire: a real `DataFusionHist` holding the archive and the recorder's tape of one
//! series, served on loopback and read through the client (decision 0193's "Built" section).
#![cfg(feature = "datafusion-serve")]

use std::sync::Arc;

use tempfile::TempDir;
use vike_data::{HistStore, TsRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::{DatahubClient, FEATURE_SOURCE, RemoteHistStore};
use vike_marketdata::QuoteTick;
use vike_store::DataFusionHist;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "EURUSD";
const LANE: &str = "recorder";

fn quote(ts: i64, bid: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask: bid + 0.0001,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    }
}

/// The archive's one quote and the tape's two, served.
fn served() -> (TempDir, std::net::SocketAddr) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes(VENUE, SYMBOL, &[quote(1_000, 1.1)], Some("dukascopy:EURUSD:1-2")).unwrap();
    let tape = [quote(1_000, 1.2), quote(2_000, 1.3)];
    store.append_quotes_sourced(VENUE, LANE, SYMBOL, &tape, Some("live-1")).unwrap();
    (dir, spawn_server(Arc::new(store)))
}

fn bids(quotes: &[QuoteTick]) -> Vec<f64> {
    quotes.iter().map(|q| q.bid).collect()
}

/// **A read that names the lane gets the tape, with the range the sibling read takes, and a read that names none
/// gets the archive**; a word that is no lane is the store's refusal, in its words.
#[test]
fn a_read_that_names_the_lane_gets_the_tape_and_every_other_gets_the_archive() {
    let (_dir, addr) = served();
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.features().iter().any(|f| f == FEATURE_SOURCE), "{:?}", client.features());

    let tape = client.scan_quotes_sourced_ms(VENUE, LANE, SYMBOL, None, None, None).unwrap();
    assert_eq!(bids(&tape), [1.2, 1.3]);
    let window =
        client.scan_quotes_sourced_ms(VENUE, LANE, SYMBOL, Some(2_000), Some(2_000), None).unwrap();
    assert_eq!(bids(&window), [1.3]);
    let archive = client.scan_quotes_ms(VENUE, SYMBOL, None, None, None).unwrap();
    assert_eq!(bids(&archive), [1.1]);

    let why = client.scan_quotes_sourced_ms(VENUE, "../up", SYMBOL, None, None, None).unwrap_err();
    assert!(why.contains("not a lane the store declares"), "{why}");
}

/// **A backtest's read over the wire is the archive's**: `RemoteHistStore`, what every reader off the box uses,
/// answers `scan_quotes` from the sourceless series, and it has no way into a lane.
#[test]
fn the_remote_store_every_reader_uses_reads_the_archive() {
    let (_dir, addr) = served();
    let remote = RemoteHistStore::new(addr.to_string());
    assert_eq!(bids(&remote.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap()), [1.1]);
    assert!(
        remote.scan_quotes_sourced_capped(VENUE, LANE, SYMBOL, TsRange::all(), None).is_err(),
        "the trait's refusal: this seam does not read a lane"
    );
}
