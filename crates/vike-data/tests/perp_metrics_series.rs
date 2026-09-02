//! `kind=perp_metrics` HistStore series — the venue's own per-interval market context for a perp,
//! which today is one number: the funding PREMIUM. Mirrors the `kind=cohort` / `kind=funding`
//! precedents (`cohort_series.rs`, `funding_series.rs`), split by feature the same way.
//!
//! Three tests here carry weight nothing else in the tree does.
//!
//! * [`datafusion_tests::run_maintenance_handles_perp_metrics_series`] proves at RUNTIME that the
//!   `compact_roundtrip` dispatch has a working `"perp_metrics"` arm. `store_kind_gate.rs` is
//!   text-only, so it can see the arm's spelling and never its behaviour — and the measured symptom
//!   of a missing arm is NOT an error: `run_maintenance` catches it per series, returns **`Ok`**
//!   with a `MaintenanceReport::failed` row, and never compacts that series again.
//!   `cohort_series.rs`'s module doc carries the measurement and the fortnight behind it.
//! * [`datafusion_tests::the_premium_series_does_not_collide_with_the_funding_rate_bars`] proves
//!   through the REAL store the property this kind exists for: the premium and the funding rate
//!   arrive in ONE Hyperliquid response at the SAME `ts` for the SAME `(venue, symbol)`, and they
//!   are stored as two series. If they ever aliased, the rate would silently acquire a second home
//!   that could disagree with `vike_model::Bar::funding` — the defect
//!   `vike_data::perp_metrics_log::PerpMetricRow`'s doc argues against.
//! * [`datafusion_tests::a_zero_premium_survives_as_an_observation`] pins the one value most likely
//!   to be optimised into an absence. Zero is a REAL premium (the perp trading exactly at its
//!   oracle), so it must round-trip as a stored row rather than becoming a gap.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_data::PerpMetricRow;

/// A Hyperliquid funding hour. The venue's cadence is 1h, so the siblings below step by that.
const HOUR: i64 = 1_756_000_800_000;
const HOUR_MS: i64 = 3_600_000;

/// One perp market-context observation.
fn metric(ts: i64, premium: f64) -> PerpMetricRow {
    PerpMetricRow { ts, premium, open_interest: None }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::{metric, HOUR, HOUR_MS};
    use vike_data::{
        CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, PerpMetricRow, TsRange,
    };
    use vike_model::Bar;

    #[test]
    fn perp_metrics_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // A real HL premium magnitude (~3e-4), its negative twin, and the boundary value.
        let rows = vec![
            metric(HOUR, 0.000_335_403_7),
            metric(HOUR + HOUR_MS, -0.000_623_610_9),
            metric(HOUR + 2 * HOUR_MS, 0.0),
        ];
        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, rows, "ts-ascending; the sign and the full precision preserved");

        // a range that doesn't overlap any appended ts -> empty, not an error
        let empty =
            store.scan_perp_metrics("hyperliquid", "BTC", TsRange::of(1_000, 2_000)).unwrap();
        assert!(empty.is_empty(), "non-overlapping range -> empty");
    }

    /// ⚠ Zero is an OBSERVATION — the perp trading exactly at its oracle — not a missing value.
    /// The producer refuses to invent one for an absent premium precisely so a stored zero can be
    /// trusted, which only holds if a stored zero survives the round trip as a row.
    #[test]
    fn a_zero_premium_survives_as_an_observation() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let flat = metric(HOUR, 0.0);
        store
            .append_perp_metrics("hyperliquid", "BTC", std::slice::from_ref(&flat), Some("k"))
            .unwrap();

        let got = store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap();
        assert_eq!(got, vec![flat], "one row, not zero rows");
        assert_eq!(got[0].premium.to_bits(), 0.0_f64.to_bits(), "a stored zero, bit for bit");
    }

    /// ⚠ THE namespace guard this kind exists for. The premium and the funding RATE come out of one
    /// `fundingHistory` row, at one `ts`, for one `(venue, symbol)` — and the rate's home is
    /// `Bar::funding` under the reserved `interval=funding` label. Neither may leak into the
    /// other's scan, because a rate readable from two places is a rate that can disagree with
    /// itself.
    #[test]
    fn the_premium_series_does_not_collide_with_the_funding_rate_bars() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        let premium = metric(HOUR, 0.000_335_403_7);
        store
            .append_perp_metrics("hyperliquid", "BTC", std::slice::from_ref(&premium), Some("p"))
            .unwrap();
        // The funding-rate half of the SAME venue response, stored the way it always has been.
        let rate_bar = Bar {
            ts: HOUR,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
            funding: Some(0.000_012_5),
            bid: None,
            ask: None,
            symbol: None,
        };
        store
            .append_bars(
                "hyperliquid",
                "BTC",
                "funding",
                std::slice::from_ref(&rate_bar),
                Some("r"),
            )
            .unwrap();

        // the premium is NOT visible as a funding bar …
        let bars = store.load_bars("hyperliquid", "BTC", "funding", TsRange::all()).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(
            bars[0].funding.map(f64::to_bits),
            Some(0.000_012_5_f64.to_bits()),
            "the RATE, never the premium"
        );
        // … and the funding bar is NOT visible as a perp metric.
        assert_eq!(
            store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap(),
            vec![premium]
        );
    }

    /// Batch-level idempotency: a re-run of the same window under the same key is a no-op, and a
    /// DIFFERENT key over the same rows appends again (the store never does per-row value dedup).
    #[test]
    fn perp_metrics_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![metric(HOUR, 0.0003), metric(HOUR + HOUR_MS, 0.0004)];

        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("w1")).unwrap(), 2);
        assert_eq!(
            store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("w1")).unwrap(),
            0,
            "the same window twice is a no-op"
        );
        assert_eq!(store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap().len(), 2);
    }

    /// ⚠ The premium key must NOT be the funding-rate key. One fetch fills two series, so a shared
    /// key would make the second append a silent no-op against the first — and a store whose
    /// funding bars predate this kind would then refuse the premium rows for every window it
    /// already held. This proves the two key-spaces are independent in the real store.
    #[test]
    fn the_rate_key_does_not_burn_the_premium_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let key = "funding_rate:hyperliquid:BTC:1-2";

        let bar = Bar {
            ts: HOUR,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
            funding: Some(0.000_012_5),
            bid: None,
            ask: None,
            symbol: None,
        };
        store
            .append_bars("hyperliquid", "BTC", "funding", std::slice::from_ref(&bar), Some(key))
            .unwrap();

        // A DIFFERENT key for the premium half — as the producer builds it — still writes.
        let premium = metric(HOUR, 0.0003);
        assert_eq!(
            store
                .append_perp_metrics(
                    "hyperliquid",
                    "BTC",
                    std::slice::from_ref(&premium),
                    Some("perp_metrics:hyperliquid:BTC:1-2"),
                )
                .unwrap(),
            1,
            "the rate's key must not have consumed the premium's window"
        );
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match. `min_parts: 1` forces the
    /// single appended fragment through it (the default `min_parts: 4` would never compact one
    /// fragment, making this guard toothless).
    ///
    /// ⚠ This is the arm's only RUNTIME guard — `store_kind_gate.rs` reads the dispatch as text.
    /// A missing arm makes `run_maintenance` return **`Ok`** with the series in
    /// `MaintenanceReport::failed` and never compacted, not an `Err`, so BOTH halves are asserted:
    /// the count alone says the work did not happen, the `failed` row says why.
    #[test]
    fn run_maintenance_handles_perp_metrics_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows: Vec<PerpMetricRow> = vec![metric(HOUR, 0.0003), metric(HOUR + HOUR_MS, -0.000_2)];
        store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("kp")).unwrap();

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
            "the perp_metrics series was SKIPPED by maintenance, and the pass still returned Ok: \
             {:?}",
            report.failed
        );
        assert_eq!(
            report.compaction.parts_written, 1,
            "the perp_metrics series was actually compacted"
        );

        // Data survives the compaction round-trip: every column preserved through the `ctx=""`
        // re-encode, with the stable `(ts, 0)` sort keeping order across the merge.
        assert_eq!(store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::{metric, HOUR, HOUR_MS};
    use vike_data::{HistStore, MemHistStore, TsRange};

    #[test]
    fn perp_metrics_series_roundtrips_mem() {
        let store = MemHistStore::new();
        let rows = vec![metric(HOUR, 0.0003), metric(HOUR + HOUR_MS, -0.000_2)];
        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 2);
        assert_eq!(store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);
        assert!(store.scan_perp_metrics("hyperliquid", "ETH", TsRange::all()).unwrap().is_empty());
    }

    /// The double honours the same batch-level guard the real store does — including the
    /// empty-batch case, which must NOT burn the key or a later real append vanishes here while
    /// the durable store accepts it.
    #[test]
    fn perp_metrics_commit_key_is_idempotent_mem() {
        let store = MemHistStore::new();
        let rows = vec![metric(HOUR, 0.0003)];
        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &[], Some("k")).unwrap(), 0);
        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_perp_metrics("hyperliquid", "BTC", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_perp_metrics("hyperliquid", "BTC", TsRange::all()).unwrap(), rows);
    }

    /// A seeded series shows up in the double's inventory under its own kind — the fold every
    /// other kind is listed in, so an inventory view does not silently omit this one.
    #[test]
    fn a_seeded_perp_metrics_series_joins_the_mem_catalog() {
        let store = MemHistStore::new();
        store
            .append_perp_metrics("hyperliquid", "BTC", &[metric(HOUR, 0.0003)], Some("k"))
            .unwrap();
        let ids = store.list_series().unwrap();
        let found = ids
            .iter()
            .find(|s| s.kind == "perp_metrics")
            .expect("the perp_metrics series is listed");
        assert_eq!(found.venue, "hyperliquid");
        assert_eq!(found.symbol, "BTC", "per-symbol: this kind has no grouped form");
        assert!(found.group.is_none());
    }
}
