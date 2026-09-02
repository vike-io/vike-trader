//! `kind=cohort` HistStore series — the graded positioning panel, one row per (hour, asset, axis,
//! label). Mirrors the `kind=chain` / `kind=funding` precedents (`chain_series.rs`,
//! `funding_series.rs`) with the cohort kind, split by feature the same way.
//!
//! Two tests here carry weight nothing else in the tree does.
//!
//! * [`datafusion_tests::run_maintenance_handles_cohort_series`] proves at RUNTIME that the
//!   `compact_roundtrip` dispatch has a working `"cohort"` arm — `store_kind_gate.rs` is text-only,
//!   so it can see the arm's spelling and never its behaviour.
//!
//!   ⚠ It was, for a fortnight, the only thing that could see the arm's ABSENCE either.
//!   `store_kind_gate.rs`'s `every_compaction_arm_has_a_row` scanned ONE region spanning BOTH
//!   kind→codec dispatches (`compact_roundtrip` and its source-ranked twin), so it went green when
//!   EITHER arm existed. MEASURED on the CI box: deleting the plain arm alone left that gate at
//!   26 passed / 0 failed and reddened this test. Fixed 2026-08-24 — that gate now checks each
//!   dispatch separately, and `crates/vike-data/tests/store_kind_gate.rs`'s `dispatch_region`
//!   carries both measurements — so the two guards overlap on existence and this one is alone only
//!   on behaviour.
//!
//!   ⚠ **And the symptom is NOT an error.** Every sibling guard used to say a missing arm "errors
//!   the WHOLE store"; that was true before `DataFusionHist::run_maintenance` grew per-series
//!   isolation, and is stale for every kind. What actually happens, measured on the same run:
//!   `compact_roundtrip` returns its `unknown series kind` `Err`, `run_maintenance` catches it PER
//!   SERIES, pushes a `MaintenanceReport::failed` row, logs one `warn!` and returns `Ok` — so the
//!   pass reports success while that series is never compacted again. That is WORSE than the claim
//!   it replaces, not milder: it is precisely the "parts piling up forever" failure the isolation's
//!   own comment describes, and it is why this test asserts on `failed` as well as on
//!   `parts_written` — the count alone says the work did not happen, the `failed` row says why and
//!   is the thing an operator would have had to read a log to see.
//! * [`datafusion_tests::two_gradings_of_one_hour_survive_as_distinct_rows`] proves through the
//!   REAL store the property the row's shape exists for: the three gradings produce
//!   shape-identical rows, so a series that did not store `grading`/`label_basis` as COLUMNS could
//!   not tell a re-graded fetch from the realized one it aliases.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_data::CohortRow;

const HOUR: i64 = 1_756_000_800_000;

/// One cohort marginal. `ts`/`cohort`/notionals vary per call; the three per-fetch dimensions
/// default to a realized size-axis fetch so the round-trip assertions read tersely.
fn cohort(ts: i64, label: &str, long: f64, total: f64) -> CohortRow {
    CohortRow {
        ts,
        asset: "BTC".into(),
        axis: "size".into(),
        cohort: label.into(),
        grading: "realized".into(),
        label_basis: "point_in_time".into(),
        long_usd: long,
        total_usd: total,
    }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::{cohort, HOUR};
    use vike_data::{
        CohortRow, CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange,
    };
    use vike_model::TradeTick;

    #[test]
    fn cohort_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // Many labels in ONE hour — the shape a real fetch has — then a second hour, so the scan
        // proves both the within-hour order and the ts ordering across hours.
        let rows = vec![
            cohort(HOUR, "4xWhale", 60.0, 100.0),
            cohort(HOUR, "Shrimp", 1.0, 3.0),
            cohort(HOUR + 3_600_000, "4xWhale", 61.5, 101.25),
        ];
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, rows, "ts-ascending; every string and both notionals preserved");

        // a range that doesn't overlap any appended ts -> empty, not an error
        let empty = store.scan_cohort("hyperliquid", "BTC", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty(), "non-overlapping range -> empty");
    }

    /// A total below the long side is a source contradiction, not something the codec may
    /// normalise: it must survive the Parquet round trip verbatim so the reader that finds it can
    /// still see what was recorded.
    #[test]
    fn a_negative_derived_short_survives_the_round_trip_unclamped() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let bad = cohort(HOUR, "4xWhale", 150.0, 100.0);
        store.append_cohort("hyperliquid", "BTC", std::slice::from_ref(&bad), Some("k")).unwrap();

        let got = store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, vec![bad]);
        assert_eq!(got[0].short_usd(), -50.0, "the contradiction is readable, not laundered");
    }

    /// THE reason `grading` and `label_basis` are columns. Both fetches cover the same hour, the
    /// same asset and the same label, and they land in ONE series — so nothing but these two
    /// columns can tell them apart. A store without them returns four rows a reader would sum.
    #[test]
    fn two_gradings_of_one_hour_survive_as_distinct_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        let realized = cohort(HOUR, "4xWhale", 60.0, 100.0);
        let unrealized =
            CohortRow { grading: "unrealized".into(), long_usd: 55.0, ..realized.clone() };
        let current = CohortRow { label_basis: "current".into(), ..realized.clone() };
        store
            .append_cohort("hyperliquid", "BTC", std::slice::from_ref(&realized), Some("r"))
            .unwrap();
        store
            .append_cohort("hyperliquid", "BTC", std::slice::from_ref(&unrealized), Some("u"))
            .unwrap();
        store
            .append_cohort("hyperliquid", "BTC", std::slice::from_ref(&current), Some("c"))
            .unwrap();

        let got = store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got.len(), 3, "three fetches, three rows — none overwrote another");
        for want in [&realized, &unrealized, &current] {
            assert!(got.contains(want), "{want:?} did not survive distinguishable");
        }
        // …and a consumer can actually separate them, which is the point of storing the columns.
        assert_eq!(got.iter().filter(|r| r.grading == "realized").count(), 2);
        assert_eq!(got.iter().filter(|r| r.label_basis == "point_in_time").count(), 2);
    }

    #[test]
    fn cohort_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![cohort(HOUR, "4xWhale", 60.0, 100.0), cohort(HOUR, "Shrimp", 1.0, 3.0)];
        // same key twice → second is a no-op; None key always appends.
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 2);
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 2);
        let more = [cohort(HOUR + 3_600_000, "4xWhale", 61.0, 101.0)];
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &more, None).unwrap(), 1);
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 3);
    }

    /// The namespace guard: a cohort panel (`kind=cohort`) and the asset's MARKET prints
    /// (`kind=trade`) share `(venue, symbol)` but live under distinct `kind=` roots — neither leaks
    /// into the other's scan. The collision is realistic here rather than theoretical: both are
    /// keyed by the venue's own coin spelling, so `hyperliquid`/`BTC` names both series.
    #[test]
    fn cohort_does_not_collide_with_market_trades() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        let panel = cohort(HOUR, "4xWhale", 60.0, 100.0);
        store.append_cohort("hyperliquid", "BTC", std::slice::from_ref(&panel), Some("c")).unwrap();
        let market = TradeTick {
            ts: HOUR,
            local_ts: 0,
            price: 60_000.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTC".to_string(),
        };
        store
            .append_trades("hyperliquid", "BTC", std::slice::from_ref(&market), Some("t"))
            .unwrap();

        // the cohort row is NOT visible as a market trade print …
        assert_eq!(store.scan_trades("hyperliquid", "BTC", TsRange::all()).unwrap(), vec![market]);
        // … and the market print is NOT visible as a cohort marginal.
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap(), vec![panel]);
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match. `min_parts: 1` forces the
    /// single appended fragment through `compact_roundtrip` (the default `min_parts: 4` would never
    /// compact one fragment, making this guard toothless).
    ///
    /// ⚠ This is the arm's only RUNTIME guard — `store_kind_gate.rs` reads the dispatch as text.
    /// See the module doc for the fortnight in which it was the only guard at all, and for the
    /// measured symptom this asserts against: a missing arm makes `run_maintenance` return **`Ok`**
    /// with the series in `MaintenanceReport::failed` and never compacted, not an `Err`. Both halves
    /// are asserted, because a future change to that isolation could move the failure between them
    /// and either assertion alone would then acquit it.
    #[test]
    fn run_maintenance_handles_cohort_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![cohort(HOUR, "4xWhale", 60.0, 100.0), cohort(HOUR, "Shrimp", 1.0, 3.0)];
        store.append_cohort("hyperliquid", "BTC", &rows, Some("kc")).unwrap();

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
            "the cohort series was SKIPPED by maintenance, and the pass still returned Ok: {:?}",
            report.failed
        );
        assert_eq!(report.compaction.parts_written, 1, "the cohort series was actually compacted");

        // Data survives the compaction round-trip: every column preserved through the `ctx=""`
        // re-encode, and the within-hour order kept by the stable `(ts, 0)` sort — which for this
        // kind means the labels of one hour do not shuffle when parts merge.
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::{cohort, HOUR};
    use vike_data::{HistStore, MemHistStore, TsRange};

    #[test]
    fn cohort_series_roundtrips_mem() {
        let store = MemHistStore::default();
        let rows = vec![cohort(HOUR, "4xWhale", 60.0, 100.0), cohort(HOUR, "Shrimp", 1.0, 3.0)];
        store.append_cohort("hyperliquid", "BTC", &rows, None).unwrap();
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);

        // non-overlapping range -> empty
        let empty = store.scan_cohort("hyperliquid", "BTC", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn cohort_commit_key_is_idempotent_mem() {
        let store = MemHistStore::default();
        let rows = vec![cohort(HOUR, "4xWhale", 60.0, 100.0)];
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_cohort("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_cohort("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 1);
    }

    /// The double must join the catalog like the real store, or a Data-Manager/Studio test written
    /// against it would report a series the store holds as absent.
    #[test]
    fn a_seeded_cohort_series_joins_the_mem_catalog() {
        let store = MemHistStore::default();
        store
            .append_cohort("hyperliquid", "BTC", &[cohort(HOUR, "4xWhale", 60.0, 100.0)], None)
            .unwrap();
        let ids = store.list_series().unwrap();
        let found = ids.iter().find(|s| s.kind == "cohort").expect("the cohort series is listed");
        assert_eq!(found.venue, "hyperliquid");
        assert_eq!(found.symbol, "BTC", "per-symbol: this kind has no grouped form");
        assert!(found.group.is_none());
    }
}
