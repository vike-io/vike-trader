//! `kind=chain` HistStore series — the point-in-time option-chain snapshot log. Mirrors the
//! `kind=funding` precedent (`funding_series.rs`) with the chain kind, plus the `chain_as_of`
//! PIT read (the `properties_as_of` analog: per-instrument latest row at or before ts).
//!
//! The namespace-guard test is the point: the store already holds `kind=trade` (market trade
//! ticks). It proves chain snapshots (`kind=chain`) and an underlying's market prints
//! (`kind=trade`) NEVER collide even when `(venue, symbol)` are identical — distinct `kind=`
//! partition roots. Split by feature exactly like `funding_series.rs` / `equity_series.rs`.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_data::ChainRow;

/// One chain-row literal: identity + ts vary per call; the quote fields derive from `strike` so
/// the round-trip assertions read tersely (mirrors the funding test's `funding` helper). The put
/// side is left sparse (all-None quote fields) so NULL columns are exercised on every test.
fn row(ts: i64, instrument: &str, expiry_ms: i64, strike: f64, is_call: bool) -> ChainRow {
    let quoted = is_call.then_some(strike);
    ChainRow {
        ts,
        underlying: "BTC".into(),
        instrument: instrument.into(),
        expiry_ms,
        strike,
        is_call,
        bid: quoted.map(|s| s * 0.05),
        ask: quoted.map(|s| s * 0.06),
        mark: quoted.map(|s| s * 0.055),
        iv: quoted.map(|_| 0.625),
        open_interest: quoted.map(|_| 120.0),
        volume: quoted.map(|_| 8.0),
        delta: quoted.map(|_| 0.55),
        gamma: quoted.map(|_| 0.000_01),
        theta: quoted.map(|_| -45.2),
        vega: quoted.map(|_| 210.0),
    }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::row;
    use vike_data::{CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange};
    use vike_model::TradeTick;

    #[test]
    fn chain_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // one snapshot: a populated call + a sparse put (every NULLable column exercised)
        let rows = vec![
            row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
            row(1_000, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
        ];
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &rows, Some("k1")).unwrap(), 2);

        let got = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, rows, "ts-ascending, every column (incl. absent quote fields) preserved");

        // a range that doesn't overlap any appended ts -> empty, not an error
        let empty = store.scan_chain("deribit", "BTC", TsRange::of(10_000, 20_000)).unwrap();
        assert!(empty.is_empty(), "non-overlapping range -> empty");
    }

    #[test]
    fn chain_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        // same key twice → second is a no-op; None key always appends.
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 1);
        assert_eq!(
            store
                .append_chain_snapshot(
                    "deribit",
                    "BTC",
                    &[row(2_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)],
                    None
                )
                .unwrap(),
            1
        );
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 2);
    }

    /// `chain_as_of` — per-instrument latest at or before ts, deterministic order.
    #[test]
    fn chain_as_of_returns_per_instrument_latest() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // snapshot A (ts=1000): call+put at 100k, JUN expiry
        let snap_a = vec![
            row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
            row(1_000, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
        ];
        // snapshot B (ts=2000): the call re-observed (fresher) + a SEP expiry instrument
        let snap_b = vec![
            row(2_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
            row(2_000, "BTC-25SEP26-120000-C", 900_000, 120_000.0, true),
        ];
        // snapshot C (ts=3000): AFTER the as-of point — must not surface
        let snap_c = vec![row(3_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        store.append_chain_snapshot("deribit", "BTC", &snap_a, Some("a")).unwrap();
        store.append_chain_snapshot("deribit", "BTC", &snap_b, Some("b")).unwrap();
        store.append_chain_snapshot("deribit", "BTC", &snap_c, Some("c")).unwrap();

        let asof = store.chain_as_of("deribit", "BTC", 2_500).unwrap();
        // per-instrument latest <= 2500: call from B (ts=2000), put from A (ts=1000), SEP from B —
        // sorted (expiry_ms, strike, instrument)
        assert_eq!(asof.len(), 3);
        assert_eq!(asof[0].instrument, "BTC-27JUN26-100000-C");
        assert_eq!(asof[0].ts, 2_000, "fresher observation wins");
        assert_eq!(asof[1].instrument, "BTC-27JUN26-100000-P");
        assert_eq!(asof[1].ts, 1_000, "instrument absent from later snapshots keeps its last row");
        assert_eq!(asof[2].instrument, "BTC-25SEP26-120000-C");

        // before anything was recorded -> empty
        assert!(store.chain_as_of("deribit", "BTC", 500).unwrap().is_empty());
    }

    /// `chain_as_of` across REAL UTC-day partition boundaries. Every other test here writes rows
    /// inside one `date=1970-01-01` part, so the multi-part read path (parts scanned + concatenated,
    /// then the ts-ascending stable sort the per-instrument fold depends on) was never exercised.
    /// Three days, interleaved instruments, appended OUT of day order to make the sort load-bearing.
    #[test]
    fn chain_as_of_folds_across_utc_day_partitions() {
        const DAY: i64 = 86_400_000;
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // day 2 written FIRST (so part order != ts order), then day 0, then day 1.
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[row(2 * DAY, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)],
                Some("d2"),
            )
            .unwrap();
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[
                    row(0, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
                    row(0, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
                ],
                Some("d0"),
            )
            .unwrap();
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[row(DAY, "BTC-25SEP26-120000-C", 900_000, 120_000.0, true)],
                Some("d1"),
            )
            .unwrap();

        // The scan itself must come back ts-ascending across parts — that ordering IS the fold rule.
        let scanned = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
        let ts: Vec<i64> = scanned.iter().map(|r| r.ts).collect();
        assert_eq!(ts, [0, 0, DAY, 2 * DAY], "parts merged in ts order, not write order");

        // As-of the last day: the call's day-2 row wins over its day-0 row; the put (day 0 only)
        // and the SEP call (day 1) keep their last observations. Sorted (expiry, strike, id).
        let asof = store.chain_as_of("deribit", "BTC", 2 * DAY).unwrap();
        assert_eq!(asof.len(), 3);
        assert_eq!(asof[0].instrument, "BTC-27JUN26-100000-C");
        assert_eq!(asof[0].ts, 2 * DAY, "freshest row wins across a partition boundary");
        assert_eq!(asof[1].instrument, "BTC-27JUN26-100000-P");
        assert_eq!(asof[1].ts, 0);
        assert_eq!(asof[2].instrument, "BTC-25SEP26-120000-C");
        assert_eq!(asof[2].ts, DAY);

        // As-of mid-archive: only what existed by then, and the call reverts to its day-0 row.
        let mid = store.chain_as_of("deribit", "BTC", DAY).unwrap();
        assert_eq!(mid.len(), 3);
        assert_eq!(mid[0].ts, 0, "day-2 observation is in the future at this as-of point");
    }

    /// The bounded PIT read: same semantics as `chain_as_of`, but the scan window (not the whole
    /// archive) sets the cost — so an instrument not re-observed inside the window drops out.
    #[test]
    fn chain_as_of_within_bounds_the_lookback() {
        const DAY: i64 = 86_400_000;
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // A stale instrument seen only on day 0, and a live one re-observed on day 2.
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[
                    row(0, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
                    row(0, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
                ],
                Some("d0"),
            )
            .unwrap();
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[row(2 * DAY, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)],
                Some("d2"),
            )
            .unwrap();

        // A 1-hour window at day 2 sees ONLY the re-observed call — the day-0 put is out of window.
        let tight = store.chain_as_of_within("deribit", "BTC", 2 * DAY, 3_600_000).unwrap();
        assert_eq!(tight.len(), 1);
        assert_eq!(tight[0].instrument, "BTC-27JUN26-100000-C");
        assert_eq!(tight[0].ts, 2 * DAY);

        // A window wide enough to cover the archive matches the unbounded read exactly.
        let wide = store.chain_as_of_within("deribit", "BTC", 2 * DAY, 10 * DAY).unwrap();
        assert_eq!(wide, store.chain_as_of("deribit", "BTC", 2 * DAY).unwrap());
        assert_eq!(wide.len(), 2);

        // Degenerate inputs stay total: a saturating lookback can't overflow into an empty range,
        // and a non-positive lookback narrows to the `ts` instant rather than inverting the range.
        assert_eq!(
            store.chain_as_of_within("deribit", "BTC", 2 * DAY, i64::MAX).unwrap(),
            store.chain_as_of("deribit", "BTC", 2 * DAY).unwrap(),
            "huge lookback degrades to the unbounded read"
        );
        let instant = store.chain_as_of_within("deribit", "BTC", 2 * DAY, -1).unwrap();
        assert_eq!(instant.len(), 1, "ts-instant only");
        assert_eq!(instant[0].ts, 2 * DAY);
    }

    /// An EMPTY batch must not burn its commit key — the later real append under the same key still
    /// has to land. Pins the `DataFusionHist` side of the contract the `MemHistStore` double mirrors
    /// (its twin lives in `mem_tests` below); the two diverging is exactly the fidelity gap the
    /// double exists to avoid.
    #[test]
    fn empty_chain_batch_does_not_consume_the_commit_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &[], Some("k")).unwrap(), 0);
        let rows = vec![row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        assert_eq!(
            store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(),
            1,
            "the key was still free after the empty batch"
        );
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);
    }

    /// The namespace guard: chain snapshots (`kind=chain`) and an underlying's MARKET prints
    /// (`kind=trade`) share `(venue, symbol)` but live under distinct `kind=` roots — neither
    /// leaks into the other's scan.
    #[test]
    fn chain_does_not_collide_with_market_trades() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        let snap = vec![row(1, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        store.append_chain_snapshot("deribit", "BTC", &snap, Some("c")).unwrap();
        let market = TradeTick {
            ts: 1,
            local_ts: 0,
            price: 60_000.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTC".to_string(),
        };
        store.append_trades("deribit", "BTC", std::slice::from_ref(&market), Some("t")).unwrap();

        // the chain row is NOT visible as a market trade print …
        assert_eq!(store.scan_trades("deribit", "BTC", TsRange::all()).unwrap(), vec![market]);
        // … and the market print is NOT visible as a chain row.
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), snap);
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match. `min_parts: 1` forces the
    /// single appended fragment through `compact_roundtrip` (the default `min_parts: 4` would never
    /// compact one fragment, making this guard toothless).
    ///
    /// ⚠ **The symptom of a missing arm is NOT an error.** This doc said the first
    /// `run_maintenance()` over chain data "errors the WHOLE store" — true before
    /// `DataFusionHist::run_maintenance` grew per-series isolation, and stale for every kind since.
    /// What actually happens, measured through the `"cohort"` arm on the CI box and pinned generally by
    /// `crates/vike-data/tests/hist_datafusion.rs`'s
    /// `one_broken_series_does_not_abort_maintenance_for_the_others`:
    /// `compact_roundtrip` returns its `unknown series kind` `Err`, `run_maintenance` catches it PER
    /// SERIES, pushes a `MaintenanceReport::failed` row, logs one `warn!` and returns **`Ok`**.
    ///
    /// That is WORSE than the old claim, not milder. A store-wide `Err` stops the pass loudly and
    /// at once; this reports success while the chain series is never compacted again — the "parts
    /// piling up forever" failure the isolation's own comment describes, visible only to somebody
    /// who reads `report.failed` or greps a log. So both halves are asserted below: the part count
    /// says the work did not happen, the `failed` row says why.
    #[test]
    fn run_maintenance_handles_chain_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![
            row(1, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
            row(1, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
        ];
        store.append_chain_snapshot("deribit", "BTC", &rows, Some("kc")).unwrap();

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
            "the chain series was SKIPPED by maintenance, and the pass still returned Ok: {:?}",
            report.failed
        );
        assert_eq!(report.compaction.parts_written, 1, "the chain series was actually compacted");

        // data survives the compaction round-trip (every column preserved through the ctx=""
        // re-encode — underlying/instrument included, NULLs stay NULL)
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::row;
    use vike_data::{HistStore, MemHistStore, TsRange};

    #[test]
    fn chain_series_roundtrips_mem() {
        let store = MemHistStore::default();
        let rows = vec![
            row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
            row(1_000, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
        ];
        store.append_chain_snapshot("deribit", "BTC", &rows, None).unwrap();
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);

        // non-overlapping range -> empty
        let empty = store.scan_chain("deribit", "BTC", TsRange::of(10_000, 20_000)).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn chain_commit_key_is_idempotent_mem() {
        let store = MemHistStore::default();
        let rows = vec![row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().len(), 1);
    }

    /// The defaulted `chain_as_of` runs over the double's real `scan_chain` too — per-instrument
    /// latest, exactly like the DataFusion twin.
    #[test]
    fn chain_as_of_per_instrument_latest_mem() {
        let store = MemHistStore::default();
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[
                    row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true),
                    row(1_000, "BTC-27JUN26-100000-P", 500_000, 100_000.0, false),
                ],
                None,
            )
            .unwrap();
        store
            .append_chain_snapshot(
                "deribit",
                "BTC",
                &[row(2_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)],
                None,
            )
            .unwrap();
        let asof = store.chain_as_of("deribit", "BTC", 2_000).unwrap();
        assert_eq!(asof.len(), 2);
        assert_eq!(asof[0].instrument, "BTC-27JUN26-100000-C");
        assert_eq!(asof[0].ts, 2_000);
        assert_eq!(asof[1].instrument, "BTC-27JUN26-100000-P");
        assert_eq!(asof[1].ts, 1_000);

        // The bounded variant shares the fold, so the double honors it identically: a 500ms window
        // at ts=2000 excludes the ts=1000 put.
        let tight = store.chain_as_of_within("deribit", "BTC", 2_000, 500).unwrap();
        assert_eq!(tight.len(), 1);
        assert_eq!(tight[0].instrument, "BTC-27JUN26-100000-C");
    }

    /// Empty-batch fidelity: the double must NOT burn the commit key on a zero-row append, because
    /// `DataFusionHist::commit_rows` doesn't (it returns `Ok(0)` on `ts.is_empty()` before
    /// registering the key). Twin of `empty_chain_batch_does_not_consume_the_commit_key` in
    /// `datafusion_tests` — the pair is what keeps the double honest.
    #[test]
    fn empty_chain_batch_does_not_consume_the_commit_key_mem() {
        let store = MemHistStore::default();
        assert_eq!(store.append_chain_snapshot("deribit", "BTC", &[], Some("k")).unwrap(), 0);
        let rows = vec![row(1_000, "BTC-27JUN26-100000-C", 500_000, 100_000.0, true)];
        assert_eq!(
            store.append_chain_snapshot("deribit", "BTC", &rows, Some("k")).unwrap(),
            1,
            "the key was still free after the empty batch"
        );
        assert_eq!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap(), rows);
    }
}
