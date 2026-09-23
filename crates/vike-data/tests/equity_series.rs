//! `kind=equity` HistStore series — the durable store for the equity sampler's output
//! (portfolio-observer PR-3 Task 2). Mirrors the `kind=properties` PIT-series precedent
//! (`symbol_properties_round_trip_and_as_of` et al. in `hist_datafusion.rs`) with `kind="equity"`.
//!
//! Split by feature so each half stays testable in isolation, matching how [`vike_data::MemHistStore`]
//! is kept DataFusion-free: `datafusion_tests` (feature `hist-datafusion`) proves the real
//! Parquet-backed round-trip plus the `compact_roundtrip` "equity" arm (the regression guard for the
//! hardcoded-kind-match gotcha); `mem_tests` (feature `test-support`) proves `MemHistStore`'s real
//! (non-stub) append/scan behavior. Gating the whole file on `any(...)` of the two features (rather
//! than unconditionally, like `hist_datafusion.rs`) keeps a plain `cargo test -p vike-data` (no
//! features) compiling this file to nothing, and keeps `--features hist-datafusion` alone (the
//! justfile's own gate, no `test-support`) from needing the DataFusion-free test double at all.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_model::EquitySample;

/// One equity-sample literal: `ts`/`equity`/`missing_prices` vary per call; venue/realized/
/// unrealized stay fixed so the round-trip assertions read tersely (mirrors the properties tests' `f1`/
/// `f2` literals).
fn sample(ts: i64, equity: f64, missing_prices: u32) -> EquitySample {
    EquitySample {
        ts,
        venue: "binance".to_string(),
        equity,
        realized: 1.0,
        unrealized: 2.0,
        missing_prices,
    }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::sample;
    use vike_data::{CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange};

    #[test]
    fn equity_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![sample(1, 100.0, 0), sample(2, 101.0, 1), sample(3, 102.0, 0)];
        assert_eq!(store.append_equity("portfolio", "binance", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
        assert_eq!(got, rows, "ts-ascending, values preserved");

        // a range that doesn't overlap any appended ts -> empty, not an error
        let empty = store.scan_equity("portfolio", "binance", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty(), "non-overlapping range -> empty");
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match. `min_parts: 1` forces the
    /// single appended fragment through `compact_roundtrip` (the default `min_parts: 4` would never
    /// trigger compaction for one fragment, which would make this guard toothless — it would pass
    /// even without the arm).
    ///
    /// ⚠ **The symptom of a missing arm is NOT an error.** This doc said the first
    /// `run_maintenance()` over any equity data "errors the WHOLE store" — true before
    /// `DataFusionHist::run_maintenance` grew per-series isolation, and stale for every kind since.
    /// What actually happens, measured through the `"cohort"` arm on the CI box and pinned generally by
    /// `crates/vike-data/tests/hist_datafusion.rs`'s
    /// `one_broken_series_does_not_abort_maintenance_for_the_others`:
    /// `compact_roundtrip` returns `compaction: unknown series kind "equity"`, `run_maintenance`
    /// catches it PER SERIES, pushes a `MaintenanceReport::failed` row, logs one `warn!` and returns
    /// **`Ok`**.
    ///
    /// That is WORSE than the old claim, not milder. A store-wide `Err` stops the pass loudly and
    /// at once; this reports success while the equity series is never compacted again — the "parts
    /// piling up forever" failure the isolation's own comment describes, visible only to somebody
    /// who reads `report.failed` or greps a log. So both halves are asserted below: the part count
    /// says the work did not happen, the `failed` row says why.
    #[test]
    fn run_maintenance_handles_equity_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_equity("portfolio", "binance", &[sample(1, 100.0, 0)], Some("k")).unwrap();

        let cfg = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 1,
                ..Default::default()
            },
            retention: None,
        };
        let report = store.run_maintenance(&cfg).unwrap();
        assert!(
            report.failed.is_empty(),
            "the equity series was SKIPPED by maintenance, and the pass still returned Ok: {:?}",
            report.failed
        );
        assert_eq!(report.compaction.parts_written, 1, "the equity series was actually compacted");

        // data survives the compaction round-trip
        let got = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
        assert_eq!(got, vec![sample(1, 100.0, 0)]);
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::sample;
    use vike_data::{HistStore, MemHistStore, TsRange};

    #[test]
    fn equity_series_roundtrips_mem() {
        let store = MemHistStore::default();
        let rows = vec![sample(1, 100.0, 0), sample(2, 101.0, 2)];
        store.append_equity("portfolio", "TOTAL", &rows, None).unwrap();
        assert_eq!(store.scan_equity("portfolio", "TOTAL", TsRange::all()).unwrap(), rows);

        // non-overlapping range -> empty
        let empty = store.scan_equity("portfolio", "TOTAL", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn equity_series_commit_key_is_idempotent() {
        let store = MemHistStore::default();
        let rows = vec![sample(1, 100.0, 0)];
        assert_eq!(store.append_equity("portfolio", "binance", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_equity("portfolio", "binance", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_equity("portfolio", "binance", TsRange::all()).unwrap().len(), 1);
    }
}
