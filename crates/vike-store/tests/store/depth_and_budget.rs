//! The conflating `kind=depth` lane, and the row budget of a capped book scan.

use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind};
use vike_store::HistoryStoreDataFusion;

use crate::common::bu;

// ---- the CONFLATING depth lane (kind=depth) ----------------------------------------------------
//
// Binance/bybit/okx declare `book: false, depth: true` and refuse `subscribe_book`, so for those
// venues `l2_snapshot` IS the L2. It used to hit the trait's default no-op and vanish.

/// A depth snapshot round-trips through its OWN series and does not touch `kind=book`.
///
/// The separation is the feature: a consumer asking for `book` must never silently receive
/// conflated data, because the book lane promises a losslessness depth cannot honour.
#[test]
fn depth_lands_in_its_own_series_and_never_in_the_book_lane() {
    use std::sync::Arc;
    use vike_data::DataClientSink;
    use vike_store::{RecorderConfig, RecorderSink};

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
        let (sink, handle) = RecorderSink::spawn(store, RecorderConfig::default()).unwrap();
        sink.l2_snapshot(
            "binance",
            "BTCUSDT",
            0.01,
            vec![L2BookLevel::new(100.0, 2.0)],
            vec![L2BookLevel::new(101.0, 3.0)],
            1_000,
        );
        sink.l2_snapshot(
            "binance",
            "BTCUSDT",
            0.01,
            vec![L2BookLevel::new(100.5, 1.0)],
            vec![L2BookLevel::new(101.5, 4.0)],
            1_100,
        );
        handle.shutdown();
    }

    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let got = store.scan_depth("binance", "BTCUSDT", TimestampRange::all()).unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].timestamp, 1_000);
    assert_eq!(got[0].bids, vec![L2BookLevel::new(100.0, 2.0)]);
    assert_eq!(got[0].asks, vec![L2BookLevel::new(101.0, 3.0)]);
    assert_eq!(got[1].timestamp, 1_100);

    // Every row is a full-state anchor — which is what a conflated depth frame IS.
    assert!(got.iter().all(|u| u.kind == vike_marketdata::L2BookUpdateKind::Snapshot));
    // Seqs are monotonic-DISTINCT, not the venue's update id and not a constant. A constant folds
    // every snapshot into one event: `BookCodec` treats `(seq, kind)` as frame identity.
    assert!(
        got[0].sequence < got[1].sequence,
        "distinct and ordered: {:?}",
        (got[0].sequence, got[1].sequence)
    );

    // THE POINT: the book lane is untouched.
    assert!(
        store.scan_book_updates("binance", "BTCUSDT", TimestampRange::all()).unwrap().is_empty()
    );
    assert!(!dir.path().join("kind=book").exists(), "no book series was created at all");
    assert!(dir.path().join("kind=depth").exists());
}

/// A snapshot folds into an `L2Book` exactly as the book lane's own anchors do — no delta is lost,
/// because a conflated feed has none to lose. What it cannot support is queue modelling, which is a
/// property of the DATA, not of the fold.
#[test]
fn a_recorded_depth_snapshot_folds_into_a_book() {
    use std::sync::Arc;
    use vike_data::DataClientSink;
    use vike_marketdata::L2Book;
    use vike_store::{RecorderConfig, RecorderSink};

    let dir = tempfile::tempdir().unwrap();
    {
        let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
        let (sink, handle) = RecorderSink::spawn(store, RecorderConfig::default()).unwrap();
        sink.l2_snapshot(
            "okx",
            "BTC-USDT",
            0.1,
            vec![L2BookLevel::new(100.0, 2.0), L2BookLevel::new(99.0, 5.0)],
            vec![L2BookLevel::new(101.0, 3.0)],
            7,
        );
        handle.shutdown();
    }

    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let got = store.scan_depth("okx", "BTC-USDT", TimestampRange::all()).unwrap();
    let mut book = L2Book::new(0.1);
    book.apply_snapshot(got[0].sequence, &got[0].bids, &got[0].asks);
    assert_eq!(book.best_bid().map(|l| l.price), Some(100.0));
    assert_eq!(book.best_ask().map(|l| l.price), Some(101.0));
}

/// **"I serve no depth" and "I hold no depth" are DIFFERENT answers**, and the point of this test
/// is that one call site can now tell them apart.
///
/// Both halves of the seam default to refusing, so a store with no depth lane can neither swallow a
/// recorder's writes and report success nor hand a reader a fabricated empty. The real backend,
/// asked for a series it genuinely does not hold, still answers with an honest empty `Ok` — it read
/// its own lane and found nothing. Before the read half was fixed both stores returned the same
/// `Ok(vec![])` and the assertion below could not be written at all.
///
/// Always runs here: `HistoryStoreMemory` lives behind vike-data's `test-support`, and this crate's
/// dev-dependency on vike-data enables it. The always-compiled twin is
/// `crates/vike-ops/tests/venues/store_kind_gate/contradictions.rs`'s `both_depth_defaults_refuse`.
#[test]
fn a_store_without_a_depth_lane_refuses_both_halves_while_a_real_store_reads_empty() {
    use vike_data::{HistoryStore, HistoryStoreMemory};

    let m = HistoryStoreMemory::default();
    assert!(m.append_depth("binance", "BTCUSDT", &[], Some("k")).is_err());
    let refused = m.scan_depth("binance", "BTCUSDT", TimestampRange::all()).unwrap_err();
    assert!(
        refused.to_string().contains("scan_depth"),
        "the refusal names the verb that could not be served: {refused}"
    );

    // ...and the store that DOES serve the lane answers the same question with a real empty read.
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert!(store.scan_depth("binance", "BTCUSDT", TimestampRange::all()).unwrap().is_empty());
}

/// **A seeded `HistoryStoreMemory` lists the series it actually holds** — the catalog twin of the depth
/// test above, proving the two facts the enumeration defaults used to conflate now come from real
/// folds on both stores that serve the verbs.
///
/// Under the trait's OLD inherited default this double answered `Ok(vec![])` for `list_series`
/// WHILE HOLDING a seeded properties series — the fabrication itself, and the red state this test
/// was run against before the fix. The two controls pin the other half of the contract: a store
/// that ANSWERED must still read as empty, never as a failure — a fresh `HistoryStoreMemory` folds its
/// real (empty) catalog, and a real `HistoryStoreDataFusion` over an empty root walks its real (empty)
/// manifest tree.
///
/// Always runs here, like its depth twin: this crate's dev-dependency on vike-data enables
/// `test-support`, which holds `HistoryStoreMemory`. The always-compiled twin is
/// `crates/vike-ops/tests/venues/store_kind_gate/contradictions.rs`'s `both_catalog_defaults_refuse`.
#[test]
fn a_seeded_memory_store_lists_its_catalog_while_an_empty_store_still_reads_empty() {
    use vike_data::{HistoryStore, HistoryStoreMemory, SeriesId, StoreKindId};
    use vike_model::SymbolProperties;

    // control: an empty double ANSWERS (a real fold over genuinely nothing), never refuses.
    let m = HistoryStoreMemory::default();
    assert!(m.list_series().expect("an empty mem store still answers").is_empty());
    assert!(m.inventory().expect("an empty mem store still answers").is_empty());

    // seeded: the catalog is what the double holds, with the fold's true coverage.
    let rows = [(1_000, SymbolProperties::default()), (2_000, SymbolProperties::default())];
    m.append_symbol_properties("binance", "BTCUSDT", &rows, Some("k")).unwrap();
    let series = m.list_series().unwrap();
    assert_eq!(
        series,
        vec![SeriesId::per_symbol(StoreKindId::Properties, "binance", "BTCUSDT", None)],
        "a seeded double lists the series it holds"
    );
    let inventory = m.inventory().unwrap();
    assert_eq!(inventory.len(), 1, "one held series, one coverage row");
    assert_eq!(inventory[0].0, series[0], "inventory and listing name the same series");
    assert_eq!(inventory[0].1.rows, 2, "rows is the held row count");
    assert_eq!(inventory[0].1.first_timestamp, 1_000, "first_timestamp is the earliest held ts");
    assert_eq!(inventory[0].1.last_timestamp, 2_000, "last_timestamp is the latest held ts");
    assert_eq!(inventory[0].1.bytes, 0, "an in-memory store truly occupies zero bytes on disk");

    // control: the real backend over an empty root answers the same questions with real empty reads.
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert!(store.list_series().unwrap().is_empty());
    assert!(store.inventory().unwrap().is_empty());
}

// ---- the ROW BUDGET ----------------------------------------------------------------------------

/// ⚠ **A capped read must be COMPLETE for a shorter span — never a sample of the asked-for one.**
///
/// That is the property the whole seam rests on: `HistoryStoreDatahub` pages by continuing past the
/// last row's `ts`, so a gap INSIDE the answered span is lost in silence — no error, no short
/// count a caller could notice, just missing rows. A test that only asserted "fewer rows came back"
/// would pass against exactly that bug.
///
/// The completeness half is checked by asking the store AGAIN, uncapped, for the span the capped
/// answer actually covered, and requiring the two to be equal.
#[test]
fn a_budgeted_book_scan_is_complete_for_a_shorter_span() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();

    // THREE parts: one append per commit key, each its own manifest entry, so the budget has
    // something to select BETWEEN. A single part could only ever be taken whole.
    for (i, base) in [1_000i64, 2_000, 3_000].iter().enumerate() {
        let events: Vec<L2BookUpdate> = (0..4)
            .map(|j| {
                bu(
                    base + j,
                    (i * 4 + j as usize) as u64,
                    L2BookUpdateKind::Delta,
                    vec![L2BookLevel::new(1.0 + j as f64, 10.0)],
                    vec![L2BookLevel::new(2.0 + j as f64, 20.0)],
                )
            })
            .collect();
        store
            .append_book_updates("binance", "BUDGETUSDT", &events, Some(&format!("part{i}")))
            .unwrap();
    }

    let all = store.scan_book_updates("binance", "BUDGETUSDT", TimestampRange::all()).unwrap();
    assert_eq!(all.len(), 12, "the seed lands as twelve events");

    // A budget smaller than the whole series must narrow the read.
    let capped = store
        .scan_book_updates_capped("binance", "BUDGETUSDT", TimestampRange::all(), Some(1))
        .unwrap();
    assert!(!capped.is_empty(), "progress outranks the budget — a capped read is never empty");
    assert!(
        capped.len() < all.len(),
        "a budget of one row must not return the whole series: {} of {}",
        capped.len(),
        all.len()
    );

    // ⚠ THE ASSERTION THAT MATTERS: what came back is everything in its own span.
    let covered = capped.last().unwrap().timestamp;
    let same_span = store
        .scan_book_updates(
            "binance",
            "BUDGETUSDT",
            TimestampRange { start: None, end: Some(covered) },
        )
        .unwrap();
    assert_eq!(
        capped.len(),
        same_span.len(),
        "the capped answer must hold every row up to its own last ts — a shorter answer is only \
         honest if it is COMPLETE for a shorter span"
    );
    for (a, b) in capped.iter().zip(&same_span) {
        assert_eq!(a.timestamp, b.timestamp);
        assert_eq!(a.sequence, b.sequence);
    }

    // No budget is the unnarrowed read, byte-for-byte the old behaviour.
    let uncapped = store
        .scan_book_updates_capped("binance", "BUDGETUSDT", TimestampRange::all(), None)
        .unwrap();
    assert_eq!(uncapped.len(), all.len(), "`None` narrows nothing");
}
