//! `kind=funding` HistStore series — the realized perp funding-payment log (Tier-2), NOT market
//! prints. Mirrors the `kind=exec_fill` precedent (`exec_log_series.rs`) with the funding kind.
//!
//! The namespace-guard test is the point: the store already holds `kind=trade` (market trade ticks).
//! It proves realized funding (`kind=funding`) and a symbol's market prints (`kind=trade`) NEVER
//! collide even when `(venue, symbol)` are identical — distinct `kind=` partition roots. Split by
//! feature exactly like `exec_log_series.rs` / `equity_series.rs`.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_data::FundingRow;

/// One funding-payment literal: `ts`/`usdc`/`szi` vary per call; rate/hash derive from `ts` so the
/// round-trip assertions read tersely (mirrors the exec tests' `fill`/`order` helpers).
fn funding(ts: i64, usdc: f64, szi: f64) -> FundingRow {
    FundingRow { ts, usdc, szi, funding_rate: 0.000_01, hash: format!("0x{ts:x}") }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::funding;
    use vike_data::{CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange};
    use vike_model::TradeTick;

    #[test]
    fn funding_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // signed usdc/szi both directions (paid/long, received/short) + a later ascending row
        let rows = vec![funding(1, -1.25, 0.5), funding(2, 0.75, -2.0), funding(3, -0.10, 0.5)];
        assert_eq!(store.append_funding("hyperliquid", "BTC", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, rows, "ts-ascending, every signed field + hash preserved");

        // a range that doesn't overlap any appended ts -> empty, not an error
        let empty = store.scan_funding("hyperliquid", "BTC", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty(), "non-overlapping range -> empty");
    }

    #[test]
    fn funding_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![funding(1, -1.0, 0.5), funding(2, 0.5, -1.0)];
        // same key twice → second is a no-op; None key always appends.
        assert_eq!(store.append_funding("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 2);
        assert_eq!(store.append_funding("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 2);
        assert_eq!(
            store.append_funding("hyperliquid", "BTC", &[funding(3, -0.2, 0.5)], None).unwrap(),
            1
        );
        assert_eq!(store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 3);
    }

    /// The namespace guard: realized funding (`kind=funding`) and a symbol's MARKET prints
    /// (`kind=trade`) share `(venue, symbol)` but live under distinct `kind=` roots — neither leaks
    /// into the other's scan.
    #[test]
    fn funding_does_not_collide_with_market_trades() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        store.append_funding("hyperliquid", "BTC", &[funding(1, -1.0, 0.5)], Some("f")).unwrap();
        let market = TradeTick {
            ts: 1,
            local_ts: 0,
            price: 60_000.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTC".to_string(),
        };
        store
            .append_trades("hyperliquid", "BTC", std::slice::from_ref(&market), Some("t"))
            .unwrap();

        // funding row is NOT visible as a market trade print …
        assert_eq!(store.scan_trades("hyperliquid", "BTC", TsRange::all()).unwrap(), vec![market]);
        // … and the market print is NOT visible as a funding payment.
        assert_eq!(
            store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap(),
            vec![funding(1, -1.0, 0.5)]
        );
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match. `min_parts: 1` forces the
    /// single appended fragment through `compact_roundtrip` (the default `min_parts: 4` would never
    /// compact one fragment, making this guard toothless).
    ///
    /// ⚠ **The symptom of a missing arm is NOT an error.** This doc said the first
    /// `run_maintenance()` over funding data "errors the WHOLE store" — true before
    /// `DataFusionHist::run_maintenance` grew per-series isolation, and stale for every kind since.
    /// What actually happens, measured through the `"cohort"` arm on the CI box and pinned generally by
    /// `crates/vike-data/tests/hist_datafusion.rs`'s
    /// `one_broken_series_does_not_abort_maintenance_for_the_others`:
    /// `compact_roundtrip` returns its `unknown series kind` `Err`, `run_maintenance` catches it PER
    /// SERIES, pushes a `MaintenanceReport::failed` row, logs one `warn!` and returns **`Ok`**.
    ///
    /// That is WORSE than the old claim, not milder. A store-wide `Err` stops the pass loudly and
    /// at once; this reports success while the funding series is never compacted again — the "parts
    /// piling up forever" failure the isolation's own comment describes, visible only to somebody
    /// who reads `report.failed` or greps a log. So both halves are asserted below: the part count
    /// says the work did not happen, the `failed` row says why.
    #[test]
    fn run_maintenance_handles_funding_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_funding("hyperliquid", "BTC", &[funding(1, -1.0, 0.5)], Some("kf")).unwrap();

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
            "the funding series was SKIPPED by maintenance, and the pass still returned Ok: {:?}",
            report.failed
        );
        assert_eq!(report.compaction.parts_written, 1, "the funding series was actually compacted");

        // data survives the compaction round-trip (every column preserved through the ctx="" re-encode)
        assert_eq!(
            store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap(),
            vec![funding(1, -1.0, 0.5)]
        );
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::funding;
    use vike_data::{HistStore, MemHistStore, TsRange};

    #[test]
    fn funding_series_roundtrips_mem() {
        let store = MemHistStore::default();
        let rows = vec![funding(1, -1.0, 0.5), funding(2, 0.5, -1.0)];
        store.append_funding("hyperliquid", "BTC", &rows, None).unwrap();
        assert_eq!(store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);

        // non-overlapping range -> empty
        let empty = store.scan_funding("hyperliquid", "BTC", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn funding_commit_key_is_idempotent_mem() {
        let store = MemHistStore::default();
        let rows = vec![funding(1, -1.0, 0.5)];
        assert_eq!(store.append_funding("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_funding("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_funding("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 1);
    }
}
