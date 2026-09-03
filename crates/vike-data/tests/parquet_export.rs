//! `export_bars_parquet` and `append_bars_from_parquet` must be each other's inverse.
//!
//! # Why round-trip rather than "it wrote a file"
//!
//! The export exists so a slice can leave one store and arrive in another — a starter dataset
//! downloaded by a stranger, a reproducer attached to a bug report, a slice moved between boxes.
//! Every one of those is a round trip, and every way it can fail quietly is a mismatch between the
//! columns one side writes and the columns the other selects: a dropped `volume`, a re-ordered
//! schema, a timestamp that survives as a float. A test that only checked the file exists would
//! pass through all of them.
//!
//! So the assertion is on the BARS, compared by bit pattern, after a real write and a real read
//! into a store that never saw the originals.

#![cfg(feature = "hist-datafusion")]

use vike_data::hist::{HistStore, TsRange};
use vike_data::DataFusionHist;
use vike_model::Bar;

/// A deterministic slice with a distinct value in every column, so a column that is dropped or
/// swapped cannot coincide with another.
fn bars() -> Vec<Bar> {
    (0..250)
        .map(|i| {
            let t = f64::from(i);
            Bar {
                ts: 1_700_000_000_000 + i64::from(i) * 60_000,
                open: 100.0 + t,
                high: 100.0 + t + 3.25,
                low: 100.0 + t - 1.75,
                close: 100.0 + t + 0.5,
                volume: 7.0 + t * 0.125,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".to_string()),
            }
        })
        .collect()
}

fn scratch(tag: &str) -> vike_model::scratch::ScratchDir {
    vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), tag)
        .expect("create a scratch directory")
}

#[test]
fn an_exported_slice_loads_back_bit_for_bit_into_a_fresh_store() {
    let src_dir = scratch("pq-export-src");
    let dst_dir = scratch("pq-export-dst");
    let file_dir = scratch("pq-export-file");
    let file = file_dir.path().join("slice.parquet");

    let src = DataFusionHist::open(src_dir.path()).expect("open source store");
    let written = bars();
    assert_eq!(
        src.append_bars("demo", "BTCUSDT", "1m", &written, Some("k")).expect("seed"),
        written.len()
    );

    let exported =
        src.export_bars_parquet(&file, "demo", "BTCUSDT", "1m", TsRange::all()).expect("export");
    assert_eq!(exported, written.len(), "the export wrote a different number of rows");
    assert!(file.is_file(), "the export produced no file at {}", file.display());
    assert!(
        std::fs::metadata(&file).expect("stat").len() > 0,
        "the export produced an EMPTY file — a reader would see a valid file with no rows, which \
         is indistinguishable from an empty slice"
    );

    // A store that has never seen these bars, so nothing can be read from memory or a cache.
    let dst = DataFusionHist::open(dst_dir.path()).expect("open destination store");
    let loaded =
        dst.append_bars_from_parquet(&file, "demo", "BTCUSDT", "1m", Some("k2")).expect("import");
    assert_eq!(loaded, written.len(), "the import took a different number of rows");

    let back = dst.load_bars("demo", "BTCUSDT", "1m", TsRange::all()).expect("read back");
    assert_eq!(back.len(), written.len());
    for (a, b) in written.iter().zip(&back) {
        assert_eq!(a.ts, b.ts);
        // Bit patterns, not `==`: a column that survived a float round trip through the wrong
        // precision compares equal at three decimal places and is still the wrong number.
        assert_eq!(a.open.to_bits(), b.open.to_bits(), "open at {}", a.ts);
        assert_eq!(a.high.to_bits(), b.high.to_bits(), "high at {}", a.ts);
        assert_eq!(a.low.to_bits(), b.low.to_bits(), "low at {}", a.ts);
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "close at {}", a.ts);
        assert_eq!(a.volume.to_bits(), b.volume.to_bits(), "volume at {}", a.ts);
    }
}

/// A range the store holds nothing in must produce a valid, empty file — not an error.
///
/// The distinction matters to the one caller that cannot ask a human: a publishing script that
/// treats "empty slice" as a failure republishes nothing and says the export broke, while one that
/// treats a missing file as success publishes an asset that is not there.
#[test]
fn an_empty_range_writes_a_valid_empty_file_rather_than_failing() {
    let store_dir = scratch("pq-export-empty");
    let file_dir = scratch("pq-export-empty-file");
    let file = file_dir.path().join("none.parquet");

    let store = DataFusionHist::open(store_dir.path()).expect("open");
    store.append_bars("demo", "BTCUSDT", "1m", &bars(), Some("k")).expect("seed");

    let n = store
        .export_bars_parquet(
            &file,
            "demo",
            "BTCUSDT",
            "1m",
            TsRange { start: Some(1), end: Some(2) },
        )
        .expect("an empty range is not an error");
    assert_eq!(n, 0);
    assert!(file.is_file(), "no file was written for an empty range");

    let dst_dir = scratch("pq-export-empty-dst");
    let dst = DataFusionHist::open(dst_dir.path()).expect("open destination");
    assert_eq!(
        dst.append_bars_from_parquet(&file, "demo", "BTCUSDT", "1m", Some("k")).expect("import"),
        0,
        "the empty file must be readable and yield no rows"
    );
}

/// The export must not touch the store it read from. It writes a plain file, and a caller who
/// exported a slice should not discover afterwards that their store gained a part or spent a
/// commit key.
#[test]
fn exporting_changes_nothing_under_the_store_root() {
    let store_dir = scratch("pq-export-inert");
    let file_dir = scratch("pq-export-inert-file");
    let store = DataFusionHist::open(store_dir.path()).expect("open");
    store.append_bars("demo", "BTCUSDT", "1m", &bars(), Some("k")).expect("seed");

    let before = tree(store_dir.path());
    store
        .export_bars_parquet(
            &file_dir.path().join("s.parquet"),
            "demo",
            "BTCUSDT",
            "1m",
            TsRange::all(),
        )
        .expect("export");
    assert_eq!(before, tree(store_dir.path()), "the export modified the store it read from");
}

/// Every path under `root`, with its size — enough to catch a new part, a rewritten manifest or a
/// spent commit key.
fn tree(root: &std::path::Path) -> Vec<(String, u64)> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(m) = std::fs::metadata(&p) {
                out.push((
                    p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned(),
                    m.len(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}
