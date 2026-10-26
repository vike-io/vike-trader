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

/// What changing the cadence between two runs costs, measured through the real store.
///
/// The idempotency key is `chain:{venue}:{underlying}:{expiry}:{ts_ms / cadence_ms}` and does NOT
/// name the cadence, so a new cadence re-numbers the buckets. A snapshot is swallowed only when the
/// NEW run's key equals a key an OLD run committed for the same expiry, i.e. when
/// `ts_new / cadence_new` equals `ts_old / cadence_old` (floored). At present-day timestamps that is
/// ~10^12 / cadence, so two cadences that differ by an ordinary ratio put their buckets at
/// different magnitudes and never meet: a change can only ADD one snapshot, inside the wall-clock
/// period the old cadence had already covered. The pairs below are the row's range
/// (`config.datahub_chain_cadence_secs`: 12 s to an hour) walked in both directions from the
/// default, plus one a sixtieth apart; the next test builds the pair that does collide.
#[test]
fn a_cadence_change_can_add_a_snapshot_and_never_swallows_one_at_ordinary_ratios() {
    let ns = |ms: i64| ms * 1_000_000;
    let rows = |ms: i64| vec![row(ms, "BTC-X-100-C", 7, 100.0, Call)];
    // 2020-01-01T00:00:00Z is a multiple of every cadence below, so the +5 s / +35 s instants sit
    // inside one bucket of the OLD cadence and (for the shorter ones) in different ones of the new.
    let t = NS_2020_01_01 / 1_000_000;
    for (old_ms, new_ms) in [
        (60_000, 30_000),
        (30_000, 60_000),
        (60_000, 12_000),
        (12_000, 60_000),
        (60_000, 3_600_000),
        (3_600_000, 60_000),
        (60_000, 61_000),
    ] {
        let (store, _t) = tmp();
        ChainRecorder::with_cadence(store.clone(), true, old_ms).record(
            "deribit",
            "BTC",
            &rows(t + 5_000),
            ns(t + 5_000),
        );
        // The new run records the same expiry 30 s later. Under the OLD cadence that may have been
        // the same bucket; under the new key it is a bucket nothing has committed.
        ChainRecorder::with_cadence(store.clone(), true, new_ms).record(
            "deribit",
            "BTC",
            &rows(t + 35_000),
            ns(t + 35_000),
        );
        let stored = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
        assert_eq!(
            stored.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![t + 5_000, t + 35_000],
            "{old_ms} ms -> {new_ms} ms: both snapshots landed — a change adds, it never swallows"
        );
    }

    // ...and it adds AT MOST ONE per change: the same bucket under the new cadence is deduped again.
    let (store, _t) = tmp();
    let old = ChainRecorder::with_cadence(store.clone(), true, 60_000);
    let new = ChainRecorder::with_cadence(store.clone(), true, 30_000);
    old.record("deribit", "BTC", &rows(t + 5_000), ns(t + 5_000));
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 1);
    old.record("deribit", "BTC", &rows(t + 35_000), ns(t + 35_000));
    assert_eq!(
        store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(),
        1,
        "the unchanged cadence swallowed the same-minute snapshot, as it always did"
    );
    new.record("deribit", "BTC", &rows(t + 35_000), ns(t + 35_000));
    new.record("deribit", "BTC", &rows(t + 40_000), ns(t + 40_000));
    assert_eq!(
        store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(),
        2,
        "one extra snapshot for the change, and no more inside the new bucket"
    );
}

/// **The one way a cadence change CAN swallow a snapshot, built on purpose and held here** so the
/// claim in `vike_data::rec::chain_rec`'s module doc and on the operator page is a measurement and
/// not a hope.
///
/// Two cadences a fraction of a percent apart put the bucket numbers of different wall-clock
/// instants equal: `b * 3_600_000` and `b * 3_601_000` are `b` seconds apart, and at `b = 490_000`
/// that is ~5.7 days. A chain for the SAME expiry recorded at both instants (any expiry that stays
/// among the nearest polled for longer than that) writes the same key twice, and the store keeps
/// only the first.
/// Cadences that far apart in ratio never do this (the test above), which is why the row's doc says
/// "pick it once" rather than forbidding a change.
#[test]
fn a_near_equal_cadence_can_swallow_a_snapshot_written_days_earlier() {
    let (store, _t) = tmp();
    let ns = |ms: i64| ms * 1_000_000;
    let rows = |ms: i64| vec![row(ms, "BTC-X-100-C", 7, 100.0, Call)];
    let b: i64 = 490_000;
    let (old_ms, new_ms) = (3_600_000, 3_601_000);
    let (t1, t2) = (b * old_ms + 1_000, b * new_ms + 1_000);
    assert_eq!(t1.div_euclid(old_ms), t2.div_euclid(new_ms), "the same bucket number");
    assert_eq!(t2 - t1, b * 1_000);
    assert!(t2 - t1 > 5 * 86_400_000, "the two instants are days apart: {} ms", t2 - t1);

    ChainRecorder::with_cadence(store.clone(), true, old_ms).record(
        "deribit",
        "BTC",
        &rows(t1),
        ns(t1),
    );
    ChainRecorder::with_cadence(store.clone(), true, new_ms).record(
        "deribit",
        "BTC",
        &rows(t2),
        ns(t2),
    );
    let stored = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
    assert_eq!(
        stored.iter().map(|r| r.ts).collect::<Vec<_>>(),
        vec![t1],
        "the later snapshot reused the earlier one's key and was dropped by the store"
    );

    // A different expiry at the later instant is a different key, and lands: the loss is one
    // (underlying, expiry, bucket), never a pass.
    ChainRecorder::with_cadence(store.clone(), true, new_ms).record(
        "deribit",
        "BTC",
        &[row(t2, "BTC-Y-100-C", 8, 100.0, Call)],
        ns(t2),
    );
    assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
}
