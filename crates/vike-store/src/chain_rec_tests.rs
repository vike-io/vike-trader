//! `vike_data::ChainRecorder` against a real store: the recorder holds only an
//! `Arc<dyn HistStore>`, so its round-trip tests, which open a `DataFusionHist`, live with the
//! engine.

use std::sync::Arc;

use crate::DataFusionHist;
use vike_data::{ChainRecorder, ChainRow, HistStore, TsRange};
use vike_marketdata::OptionKind::{self, Call, Put};

const NS_2020_01_01: i64 = 1_577_836_800_000_000_000; // 2020-01-01T00:00:00Z in ns

fn tmp() -> (Arc<dyn HistStore + Send + Sync>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    (store as Arc<dyn HistStore + Send + Sync>, dir)
}

fn row(ts: i64, instrument: &str, expiry_ms: i64, strike: f64, kind: OptionKind) -> ChainRow {
    ChainRow {
        ts,
        underlying: "BTC".into(),
        instrument: instrument.into(),
        expiry_ms,
        strike,
        kind,
        bid: Some(1.0),
        ask: Some(2.0),
        mark: Some(1.5),
        iv: Some(0.6),
        open_interest: None,
        volume: None,
        delta: Some(0.5),
        gamma: None,
        theta: None,
        vega: None,
    }
}

#[test]
fn disabled_recorder_writes_nothing() {
    let (store, _t) = tmp();
    let rec = ChainRecorder::new(store.clone(), false);
    let ts_ms = NS_2020_01_01 / 1_000_000;
    rec.record("deribit", "BTC", &[row(ts_ms, "BTC-X-100-C", 1, 100.0, Call)], NS_2020_01_01);
    assert!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().is_empty());
}

#[test]
fn same_bucket_twice_is_one_snapshot() {
    let (store, _t) = tmp();
    let rec = ChainRecorder::new(store.clone(), true); // 1min cadence
    let ts_ms = NS_2020_01_01 / 1_000_000;
    let rows =
        vec![row(ts_ms, "BTC-X-100-C", 7, 100.0, Call), row(ts_ms, "BTC-X-100-P", 7, 100.0, Put)];
    rec.record("deribit", "BTC", &rows, NS_2020_01_01);
    // 30s later, same minute bucket → dropped by the commit key
    rec.record("deribit", "BTC", &rows, NS_2020_01_01 + 30_000_000_000);
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);
    // next minute → a second snapshot lands
    let later = ts_ms + 60_000;
    rec.record(
        "deribit",
        "BTC",
        &[row(later, "BTC-X-100-C", 7, 100.0, Call)],
        NS_2020_01_01 + 60_000_000_000,
    );
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 3);
}

#[test]
fn distinct_expiries_in_one_bucket_both_land() {
    // A full-surface pass records one chain per expiry back to back — the expiry in the commit
    // key keeps the second from being swallowed by the first's bucket.
    let (store, _t) = tmp();
    let rec = ChainRecorder::new(store.clone(), true);
    let ts_ms = NS_2020_01_01 / 1_000_000;
    rec.record("deribit", "BTC", &[row(ts_ms, "BTC-JUN-100-C", 100, 100.0, Call)], NS_2020_01_01);
    rec.record(
        "deribit",
        "BTC",
        &[row(ts_ms, "BTC-SEP-100-C", 200, 100.0, Call)],
        NS_2020_01_01 + 1_000_000_000, // +1s: same minute bucket, different expiry
    );
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
}

/// The bucketing arithmetic itself, driven through the explicit-cadence constructor. Guards the
/// interaction the review called out: the non-positive→1ms clamp feeding `div_euclid`.
#[test]
fn cadence_controls_the_idempotency_bucket() {
    let ts_ms = NS_2020_01_01 / 1_000_000;
    let ns = |ms: i64| ms * 1_000_000;
    let rows = |ms: i64| vec![row(ms, "BTC-X-100-C", 7, 100.0, Call)];

    // 10s cadence: +5s is the SAME bucket (dropped), +10s is the next one (lands).
    let (store, _t) = tmp();
    let rec = ChainRecorder::with_cadence(store.clone(), true, 10_000);
    assert_eq!(rec.cadence_ms(), 10_000);
    rec.record("deribit", "BTC", &rows(ts_ms), ns(ts_ms));
    rec.record("deribit", "BTC", &rows(ts_ms + 5_000), ns(ts_ms + 5_000));
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 1);
    rec.record("deribit", "BTC", &rows(ts_ms + 10_000), ns(ts_ms + 10_000));
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);

    // Non-positive clamps to 1ms — every distinct ms is its own bucket, nothing is deduped
    // (the clamp exists so `div_euclid` can never divide by zero).
    let (store2, _t2) = tmp();
    let rec2 = ChainRecorder::with_cadence(store2.clone(), true, 0);
    assert_eq!(rec2.cadence_ms(), 1, "non-positive cadence clamps to 1ms");
    rec2.record("deribit", "BTC", &rows(ts_ms), ns(ts_ms));
    rec2.record("deribit", "BTC", &rows(ts_ms + 1), ns(ts_ms + 1));
    assert_eq!(store2.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
    rec2.record("deribit", "BTC", &rows(ts_ms + 1), ns(ts_ms + 1)); // same ms → same bucket
    assert_eq!(store2.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
}

/// A mid-bucket restart must not re-record: commit keys live in the series manifest, so they
/// survive dropping and re-opening the store. Proven for other kinds by the store's own tests;
/// pinned here for `kind=chain` specifically (the recorder's whole dedupe story depends on it).
#[test]
fn commit_keys_survive_a_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let ts_ms = NS_2020_01_01 / 1_000_000;
    let rows = vec![row(ts_ms, "BTC-X-100-C", 7, 100.0, Call)];
    {
        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(DataFusionHist::open(dir.path()).unwrap());
        ChainRecorder::new(store, true).record("deribit", "BTC", &rows, NS_2020_01_01);
    } // store dropped — "app restart"
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());
    // Same bucket after the restart → the manifest's committed key still wins.
    ChainRecorder::new(store.clone(), true).record("deribit", "BTC", &rows, NS_2020_01_01);
    assert_eq!(
        store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(),
        rows,
        "restart inside one bucket re-records nothing"
    );
}
