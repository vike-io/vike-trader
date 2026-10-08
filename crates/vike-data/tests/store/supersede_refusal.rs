//! The store's supersede REFUSAL, told from every other store error.
//!
//! `DataFusionHist`'s superseding verbs refuse — whole, writing nothing — when the key they were
//! asked to supersede has been folded by compaction into a multi-key part; `hist_datafusion.rs`
//! pins that half, this file the other. A caller that backfills a window chunk by chunk wants to
//! step over THAT refusal and go on, and over nothing else — so it must be able to ask "is this the
//! refusal?" of a [`DataError`]. The refusal has no variant of its own: it is a [`DataError::Query`]
//! with fixed opening words, and [`DataError::is_supersede_refusal`] is the one question that reads
//! them. This file is the pin between that predicate and the store's message, built from the REAL
//! refusal rather than a hand-typed copy of its text — a copy would go on passing while the store's
//! wording drifted away from it, which is the failure the pin is for. Only compiled/run with
//! `--features hist-datafusion`.
#![cfg(feature = "hist-datafusion")]

use vike_data::{CompactionConfig, DataError, DataFusionHist, HistStore, TsRange};

use crate::common::qt;

/// Merge every `date=` partition of the `("v", "S")` quote series that holds two parts or more — the
/// pass background maintenance runs on its own once a date has `min_parts` fragments, here spelled
/// with `min_parts: 2` so two small parts are enough. It is what folds a provisional part into a
/// neighbour's key set.
fn compact(store: &DataFusionHist) {
    store
        .compact_series(
            "quote",
            "v",
            "S",
            None,
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
}

/// The refusal in BOTH of the shapes `plan_supersede` words it, each checked to be the shape it
/// claims to be before the predicate is asked — a scenario that quietly produced the other shape
/// would leave one message unpinned and the test green.
///
/// 1. **No exact-match part exists at all**: the provisional key's only occurrence is folded into a
///    multi-key part (one same-day neighbour, `compact`ed together).
/// 2. **Exact-match parts exist too**: a provisional commit whose rows straddle a UTC midnight seals
///    one part per date, all under the one key; compaction folds the first date's part into a
///    neighbour and leaves the second date's alone (a date holding a single part is never merged),
///    so the key occurs both exactly and folded.
#[test]
fn the_store_refusing_a_supersede_is_recognised_in_both_of_its_message_shapes() {
    // Shape 1.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes("v", "S", &[qt(0, 1.0)], Some("k1")).unwrap();
    store
        .append_quotes_superseding("v", "S", &[qt(1_000, 1.1)], Some("provisional"), None)
        .unwrap();
    compact(&store);
    let refusal = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(2_000, 1.2)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap_err();
    assert!(
        refusal.to_string().contains("no exact-match part exists"),
        "this scenario is meant to be the FIRST shape: {refusal}"
    );
    assert!(refusal.is_supersede_refusal(), "the first shape was not recognised: {refusal}");

    // Shape 2.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes("v", "S", &[qt(500, 0.9)], Some("k1")).unwrap();
    store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(90_000_000, 1.1)],
            Some("provisional"),
            None,
        )
        .unwrap();
    compact(&store);
    let refusal = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(2_000, 1.2)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap_err();
    assert!(
        refusal.to_string().contains("exact-match part(s) exist"),
        "this scenario is meant to be the SECOND shape: {refusal}"
    );
    assert!(refusal.is_supersede_refusal(), "the second shape was not recognised: {refusal}");

    // A refusal writes nothing, and the store answers the same on a retry — the property that makes
    // stepping over it the right response, and stepping over ANYTHING ELSE the wrong one.
    let again = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(2_000, 1.2)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap_err();
    assert!(again.is_supersede_refusal(), "a retry refused differently: {again}");
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        3,
        "a refusal wrote nothing: the k1 row and the provisional rows, and no canonical one"
    );
}

/// Nothing but the refusal answers `true`. The predicate reads a message, so the two ways it could
/// go wrong are both pinned: a `Query` that is not the refusal must not be taken for it — the store
/// has other `Query` errors, and a real one is built here — and a variant that is not `Query` must
/// not be, even when its text quotes the refusal's own words, because the caller acts on the
/// PREDICATE and a disk error that merely quotes a message is still a disk error.
#[test]
fn nothing_but_the_refusal_is_recognised_as_one() {
    // A real `Query` from the store, and the closest neighbour there is: it comes from the same write
    // verbs, before a byte is written, and is not a refusal to supersede.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let hostile = store
        .append_quotes_superseding("v", "EUR/USD", &[qt(0, 1.0)], Some("canonical"), Some("p"))
        .unwrap_err();
    assert!(
        matches!(hostile, DataError::Query(_)),
        "a path-hostile symbol answers a Query: {hostile}"
    );
    assert!(!hostile.is_supersede_refusal(), "a Query that is not the refusal: {hostile}");

    let quoting = "cannot supersede commit key \"k\": a disk that quotes the words";
    assert!(!DataError::Io(quoting.to_string()).is_supersede_refusal());
    assert!(!DataError::Unsupported(quoting.to_string()).is_supersede_refusal());
    assert!(!DataError::Query("hist query blew up".to_string()).is_supersede_refusal());
    assert!(
        DataError::Query(quoting.to_string()).is_supersede_refusal(),
        "the words alone, in a Query, are what the predicate reads — the real-refusal test above \
         is what ties them to the store"
    );
}
