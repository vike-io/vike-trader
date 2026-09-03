//! Gated: these drive the venue-direct collectors, which a default build does not compile
//! (feature `venue-backfill`). Without it this file is an empty test binary — the same shape
//! `vike-tradehub`'s `telegram_control.rs` uses for its own off-by-default feature.
#![cfg(feature = "venue-backfill")]

//! Offline gate for the Dukascopy → hist-store backfill (slice 6). No network, no fixture file:
//! build a synthetic `Vec<Tick>` directly, map via `tick_to_quote`, ingest into a `tempdir`
//! `DataFusionHist`, and assert bit-exact round-trips (`to_bits`). Then resample and assert the
//! stored bars match `vike_model::consolidate_quotes` bit-exactly, plus commit-key idempotency.
//! The one REAL network fetch is `#[ignore]`d — this file is deterministic + offline in CI.

use vike_backfill::dukascopy::{
    backfill_dukascopy_quotes, resample_and_store_bars, tick_to_quote, VENUE,
};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_dukascopy::Tick;
use vike_model::{consolidate_quotes, QuoteTick};

const SYMBOL: &str = "EURUSD";
const STEP_MS: i64 = 60_000;
const INTERVAL: &str = "1m";

/// A deterministic synthetic tick stream (ascending ts) spanning two 1-minute buckets, so the
/// resample yields >1 bar. Values are arbitrary — the assertions compare bit patterns of whatever
/// we ingest, so no "exact f64 repr" contrivance is needed.
fn synthetic_ticks() -> Vec<Tick> {
    vec![
        Tick { ts: 0, bid: 1.10001, ask: 1.10003, bid_vol: 1.5, ask_vol: 2.25 },
        Tick { ts: 1_000, bid: 1.10010, ask: 1.10012, bid_vol: 0.5, ask_vol: 3.0 },
        Tick { ts: 2_000, bid: 1.09990, ask: 1.09992, bid_vol: 4.0, ask_vol: 1.0 },
        Tick { ts: 60_000, bid: 1.10100, ask: 1.10103, bid_vol: 2.0, ask_vol: 2.0 },
        Tick { ts: 61_500, bid: 1.10080, ask: 1.10082, bid_vol: 1.0, ask_vol: 1.25 },
    ]
}

fn quotes_from(ticks: &[Tick]) -> Vec<QuoteTick> {
    ticks.iter().map(|t| tick_to_quote(t, SYMBOL)).collect()
}

#[test]
fn tick_to_quote_maps_fields_exactly() {
    let t = Tick { ts: 42, bid: 1.2345, ask: 1.2347, bid_vol: 7.0, ask_vol: 9.0 };
    let q = tick_to_quote(&t, SYMBOL);
    assert_eq!(q.ts, 42);
    assert_eq!(q.bid.to_bits(), 1.2345f64.to_bits());
    assert_eq!(q.ask.to_bits(), 1.2347f64.to_bits());
    assert_eq!(q.bid_size.to_bits(), 7.0f64.to_bits(), "bid_vol -> bid_size");
    assert_eq!(q.ask_size.to_bits(), 9.0f64.to_bits(), "ask_vol -> ask_size");
    assert_eq!(q.symbol, SYMBOL);
}

#[test]
fn append_then_scan_is_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = quotes_from(&synthetic_ticks());

    let n = hist.append_quotes(VENUE, SYMBOL, &quotes, Some("k1")).unwrap();
    assert_eq!(n, quotes.len());

    let got = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(got.len(), quotes.len());
    for (a, b) in quotes.iter().zip(got.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.bid.to_bits(), b.bid.to_bits());
        assert_eq!(a.ask.to_bits(), b.ask.to_bits());
        assert_eq!(a.bid_size.to_bits(), b.bid_size.to_bits());
        assert_eq!(a.ask_size.to_bits(), b.ask_size.to_bits());
        assert_eq!(a.symbol, b.symbol);
    }
}

#[test]
fn resample_matches_consolidate_quotes_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = quotes_from(&synthetic_ticks());
    hist.append_quotes(VENUE, SYMBOL, &quotes, Some("k1")).unwrap();

    let n = resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    let expect = consolidate_quotes(&quotes, STEP_MS);
    assert_eq!(n, expect.len());
    assert!(expect.len() >= 2, "two buckets should produce >=2 bars");

    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(got.len(), expect.len());
    // bid/ask are intentionally NOT persisted by the bar store (schema = OHLCV + funding), so they
    // read back as None; compare the STORED fields bit-exactly instead of a whole-Bar equality.
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.open.to_bits(), g.open.to_bits());
        assert_eq!(e.high.to_bits(), g.high.to_bits());
        assert_eq!(e.low.to_bits(), g.low.to_bits());
        assert_eq!(e.close.to_bits(), g.close.to_bits());
        assert_eq!(e.volume.to_bits(), g.volume.to_bits());
    }
}

#[test]
fn append_is_idempotent_by_commit_key() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = quotes_from(&synthetic_ticks());
    let key = "dukascopy:EURUSD:0-61500";

    let first = hist.append_quotes(VENUE, SYMBOL, &quotes, Some(key)).unwrap();
    assert_eq!(first, quotes.len());
    let second = hist.append_quotes(VENUE, SYMBOL, &quotes, Some(key)).unwrap();
    assert_eq!(second, 0, "same commit key twice must be a no-op");
}

/// LIVE: hits Dukascopy's public datafeed CDN. Run explicitly:
/// `cargo test -p vike-backfill --test dukascopy_backfill -- --ignored --nocapture live_`
#[test]
#[ignore = "live: hits Dukascopy's public datafeed CDN"]
fn live_backfill_eurusd_hour() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    // 2024-01-03 (Wed) 14:00–15:00 UTC — a liquid London/NY-overlap hour.
    let start = 1_704_290_400_000i64;
    let end = start + 3_600_000 - 1;

    let n = backfill_dukascopy_quotes(&hist, "EURUSD", start, end).expect("backfill");
    eprintln!("backfilled {n} EURUSD quote rows");
    assert!(n > 0, "a liquid hour should yield quotes");

    // idempotent second run
    let again = backfill_dukascopy_quotes(&hist, "EURUSD", start, end).expect("backfill#2");
    assert_eq!(again, 0, "same window is a no-op");

    let bars =
        resample_and_store_bars(&hist, "EURUSD", "1m", TsRange::of(start, end)).expect("resample");
    eprintln!("resampled {bars} 1m bars");
    assert!(bars > 0);
}
