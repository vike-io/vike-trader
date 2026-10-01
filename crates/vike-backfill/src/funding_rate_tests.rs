use super::*;

// --- map + key ---
// (The registry test that stood here went with `source_by_name`/`SOURCES`: the funding roster is
// the datahub's `FUNDING_SOURCES` now, pinned by
// `crates/vike-datahub/tests/backfill_roundtrip.rs`'s
// `the_real_table_serves_funding_for_exactly_its_funding_sources` — docs/decisions/0094.)

/// The rate rides on `Bar.funding`; OHLCV/volume are zeroed and bid/ask/symbol absent.
#[test]
fn point_to_bar_rides_rate_on_funding_with_zeroed_ohlcv() {
    // On-the-hour stamp: unchanged by the floor.
    let b = point_to_bar(&FundingRatePoint {
        ts_ms: 1_699_999_200_000,
        rate: 0.0001,
        premium: Some(0.0002),
    });
    assert_eq!(b.ts, 1_699_999_200_000);
    // Exact-float checks compared bitwise (codebase idiom) to stay clear of float-eq lints.
    assert_eq!(b.funding.map(f64::to_bits), Some(0.0001_f64.to_bits()));
    for x in [b.open, b.high, b.low, b.close, b.volume] {
        assert_eq!(x.to_bits(), 0.0_f64.to_bits(), "OHLCV/volume are zeroed");
    }
    assert_eq!(b.bid, None);
    assert_eq!(b.ask, None);
    assert_eq!(b.symbol, None);
}

#[test]
fn commit_key_has_venue_symbol_and_window() {
    assert_eq!(
        funding_rate_commit_key("binance", "BTCUSDT", 1000, 2000),
        "funding_rate:binance:BTCUSDT:1000-2000"
    );
}

/// The two halves of one fetch take DIFFERENT keys. Sharing one would make the second append of
/// a window a silent no-op against the first — the hazard `perp_metrics_commit_key` documents.
#[test]
fn the_premium_commit_key_is_distinct_from_the_rate_one() {
    let rate = funding_rate_commit_key("hyperliquid", "BTC", 1000, 2000);
    let prem = perp_metrics_commit_key("hyperliquid", "BTC", 1000, 2000);
    assert_eq!(prem, "perp_metrics:hyperliquid:BTC:1000-2000");
    assert_ne!(rate, prem, "one window, two series, two keys");
}

// --- premium: the value that used to be parsed and dropped ---

/// A point with a premium becomes a row; one without becomes nothing. ⚠ A ZERO premium is a
/// REAL observation (the perp at its oracle) and must be kept — inventing a zero for an absent
/// premium is precisely what `point_to_perp_metric` refuses.
#[test]
fn point_to_perp_metric_keeps_a_zero_and_drops_an_absent_premium() {
    let with = point_to_perp_metric(&FundingRatePoint {
        ts_ms: 1_700_000_000_000,
        rate: 0.0001,
        premium: Some(0.0),
    })
    .expect("a zero premium is an observation, not an absence");
    assert_eq!(with.ts, 1_699_999_200_000, "the jittered stamp floors onto its hour");
    assert_eq!(with.premium.to_bits(), 0.0_f64.to_bits());

    assert_eq!(
        point_to_perp_metric(&FundingRatePoint {
            ts_ms: 1_700_000_000_000,
            rate: 0.0001,
            premium: None,
        }),
        None,
        "no premium reported → no row, never a zero-filled one"
    );
}

/// ⚠ The stamp jitter that made every stored hour UNMATCHABLE, pinned with the observed
/// values: Hyperliquid stamps funding at `…000006`/`…000030` — milliseconds past the hour —
/// while price bars sit exactly on it, so a raw stamp broke the row doc's "align by
/// timestamp" promise for every consumer doing an exact-ts join (measured: the ported cohort
/// study ran with `funding_z_24h`/`premium_z_24h` all-NaN and nothing said so).
#[test]
fn a_jittered_stamp_floors_onto_its_nominal_hour_in_both_series() {
    let p =
        FundingRatePoint { ts_ms: 1_775_538_000_006, rate: 6.365e-7, premium: Some(-4.949083e-4) };
    assert_eq!(point_to_bar(&p).ts, 1_775_538_000_000);
    assert_eq!(point_to_perp_metric(&p).unwrap().ts, 1_775_538_000_000);
}

/// The premium does NOT ride the funding bar: `Bar` has no field for it, and the funding rate
/// keeps its one home. This pins the split the module doc argues for.
#[test]
fn the_funding_bar_carries_the_rate_and_never_the_premium() {
    let b = point_to_bar(&FundingRatePoint {
        ts_ms: 1_700_000_000_000,
        rate: 0.0001,
        premium: Some(0.42),
    });
    assert_eq!(b.funding.map(f64::to_bits), Some(0.0001_f64.to_bits()), "the RATE, not 0.42");
}

// --- the ingest, observed on the store ---
//
// `backfill_funding_rate` itself, which is what the datahub's funding lane runs, driven with a FAKE
// source over a temp store — the venue is the only thing missing. Every test gets its own store: a
// shared one would turn a second call into a commit-key no-op for the wrong reason.

/// A source that answers every window with the same fixed points and touches no network.
struct FixedFunding(Vec<FundingRatePoint>);

impl FundingRateSource for FixedFunding {
    fn venue(&self) -> &str {
        "testvenue"
    }

    fn fetch(
        &self,
        _symbol: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, vike_data::source::SourceError> {
        Ok(self.0.clone())
    }
}

/// An on-the-hour stamp, so [`floor_to_hour`] leaves every point where it is and the read-back
/// compares exactly.
const T0: i64 = 1_699_999_200_000;
const HOUR_MS: i64 = 3_600_000;

/// Two consecutive points: one carrying a premium (Hyperliquid's shape), one without (Binance's).
fn two_points() -> Vec<FundingRatePoint> {
    vec![
        FundingRatePoint { ts_ms: T0, rate: 0.0001, premium: Some(-0.00025) },
        FundingRatePoint { ts_ms: T0 + HOUR_MS, rate: -0.00005, premium: None },
    ]
}

/// A store over its own temp dir, returned with the dir so the dir outlives it.
fn store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().expect("temp dir");
    let hist = DataFusionHist::open(dir.path()).expect("open temp store");
    (dir, hist)
}

/// One fetch fills BOTH series: two rate bars under the reserved `interval=funding` label, each
/// carrying its point's rate on `Bar::funding`, and ONE premium row — for the one point that had a
/// premium. A second identical call writes nothing to either: both window keys were spent.
#[test]
fn the_ingest_fills_both_series_and_spends_both_window_keys() {
    let (_d, hist) = store();
    let source = FixedFunding(two_points());

    let written = backfill_funding_rate(&hist, &source, "BTC", T0, T0 + HOUR_MS).unwrap();
    assert_eq!(written, FundingRateWritten { rate_rows: 2, premium_rows: 1 });

    let bars =
        hist.load_bars("testvenue", "BTC", FUNDING_INTERVAL, vike_data::TsRange::all()).unwrap();
    assert_eq!(bars.len(), 2, "one rate bar per point, under the reserved label");
    for (bar, point) in bars.iter().zip(two_points()) {
        assert_eq!(bar.ts, point.ts_ms);
        assert_eq!(
            bar.funding.map(f64::to_bits),
            Some(point.rate.to_bits()),
            "the rate rides on Bar::funding at {}",
            point.ts_ms
        );
    }
    let premiums = hist.scan_perp_metrics("testvenue", "BTC", vike_data::TsRange::all()).unwrap();
    assert_eq!(premiums.len(), 1, "only the point that carried a premium writes a row");
    assert_eq!(premiums[0].ts, T0);
    assert_eq!(premiums[0].premium.to_bits(), (-0.00025_f64).to_bits());

    let again = backfill_funding_rate(&hist, &source, "BTC", T0, T0 + HOUR_MS).unwrap();
    assert_eq!(
        again,
        FundingRateWritten::default(),
        "a re-run of the same window is a no-op in BOTH series — both commit keys are spent"
    );
    let bars =
        hist.load_bars("testvenue", "BTC", FUNDING_INTERVAL, vike_data::TsRange::all()).unwrap();
    assert_eq!(bars.len(), 2, "and nothing was appended twice");
}

/// An EMPTY fetch writes nothing and spends no key, so a later non-empty fetch of the SAME window
/// still writes — the promise [`backfill_funding_rate`]'s doc makes ("an empty fetch is NOT
/// committed, so a later re-run still retries it").
///
/// Pinned as the OBSERVABLE promise rather than as the early return that keeps it: the store keeps
/// it too (`vike_data::DataFusionHist` records no commit key for an empty batch), and whichever layer
/// holds it, this is what an operator re-running a window that came back empty relies on.
#[test]
fn an_empty_fetch_writes_nothing_and_leaves_the_window_retryable() {
    let (_d, hist) = store();

    let empty =
        backfill_funding_rate(&hist, &FixedFunding(Vec::new()), "BTC", T0, T0 + HOUR_MS).unwrap();
    assert_eq!(empty, FundingRateWritten::default());
    assert!(
        hist.load_bars("testvenue", "BTC", FUNDING_INTERVAL, vike_data::TsRange::all())
            .unwrap()
            .is_empty(),
        "an empty fetch writes no rate bar"
    );

    let later =
        backfill_funding_rate(&hist, &FixedFunding(two_points()), "BTC", T0, T0 + HOUR_MS).unwrap();
    assert_eq!(
        later,
        FundingRateWritten { rate_rows: 2, premium_rows: 1 },
        "the empty fetch spent the window's commit key, so the retry wrote nothing"
    );
}
