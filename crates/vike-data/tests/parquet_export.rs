//! `write_bars_parquet` and `append_bars_from_parquet` must be each other's inverse.
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
//!
//! ⚠ **The encoder is fed bars that never saw a store**, and that is the shape it ships in: since
//! 2026-09-26 `backtest data export` asks a DATAHUB for the bars (decision 0084's amendment) and
//! hands them to `write_bars_parquet` from a plain `Vec`. The store's own `export_bars_parquet` —
//! `load_bars` followed by this encoder — was deleted with that change, having no production caller
//! left, and two of this file's tests went with it: one held the two spellings byte-equal, which
//! means nothing once there is one, and one asserted an export changed nothing under the store root
//! it read, which the encoder now cannot do BY SIGNATURE — it takes no store. Local-vs-wire parity
//! is a different property and lives where a wire exists:
//! `crates/vike-backtest/tests/optimizer_cli.rs`'s
//! `data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds`.

#![cfg(feature = "hist-datafusion")]

use vike_data::DataFusionHist;
use vike_data::hist::{HistStore, TsRange};
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
    let dst_dir = scratch("pq-export-dst");
    let file_dir = scratch("pq-export-file");
    let file = file_dir.path().join("slice.parquet");

    // The bars in hand, exactly as `bars()` built them — no store, no manifest, no read: the shape
    // `backtest data export` hands the encoder once the datahub has answered.
    let written = bars();
    let exported = vike_data::write_bars_parquet(&file, &written).expect("export");
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

/// A slice with nothing in it must produce a valid, empty file — not an error.
///
/// The distinction matters to the one caller that cannot ask a human: a publishing script that
/// treats "empty slice" as a failure republishes nothing and says the export broke, while one that
/// treats a missing file as success publishes an asset that is not there. An export over a range
/// the datahub holds nothing in reaches the encoder as exactly this: an empty `Vec`.
#[test]
fn an_empty_slice_writes_a_valid_empty_file_rather_than_failing() {
    let file_dir = scratch("pq-export-empty-file");
    let file = file_dir.path().join("none.parquet");

    let n = vike_data::write_bars_parquet(&file, &[]).expect("an empty slice is not an error");
    assert_eq!(n, 0);
    assert!(file.is_file(), "no file was written for an empty slice");

    let dst_dir = scratch("pq-export-empty-dst");
    let dst = DataFusionHist::open(dst_dir.path()).expect("open destination");
    assert_eq!(
        dst.append_bars_from_parquet(&file, "demo", "BTCUSDT", "1m", Some("k")).expect("import"),
        0,
        "the empty file must be readable and yield no rows"
    );
}
