//! Gated: these drive the venue-direct collectors, which a default build does not compile
//! (feature `venue-backfill`). Without it this file is an empty test binary — the same shape
//! `vike-tradehub`'s `telegram_control.rs` uses for its own off-by-default feature.
#![cfg(feature = "venue-backfill")]

//! Offline gate for the Bybit V5 klines → hist-store backfill. No network: a SMALL committed klines
//! JSON fixture (`fixtures/bybit_klines_btcusdt_1m.json`, in Bybit's real newest-first envelope
//! shape) is mapped by `vike_bybit::data::parse_bybit_klines` and asserted bit-exact
//! (`to_bits` on o/h/l/c/v, `ts` exact) AND ascending — i.e. the mapper's newest-first→ascending
//! reversal is verified. Then the mapped bars are `append_bars`'d into a `tempdir` `DataFusionHist`
//! and `load_bars` must round-trip them bit-for-bit, plus commit-key idempotency (same key twice →
//! 0). The one REAL network fetch is `#[ignore]`d — this file is deterministic + offline in CI.

use vike_backfill::bybit::{backfill_bybit_klines, klines_commit_key, VENUE};
use vike_bybit::data::parse_bybit_klines;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::Bar;

const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// The three bars the fixture must map to, field-for-field, ASCENDING by ts (the fixture lists them
/// newest-first, so this also pins the reversal). Literals mirror the JSON's decimal strings.
fn expected_bars() -> Vec<Bar> {
    let mk = |ts, o: f64, h: f64, l: f64, c: f64, v: f64| Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    vec![
        mk(1_700_000_000_000, 27000.10, 27050.50, 26980.00, 27010.25, 12.345678),
        mk(1_700_000_060_000, 27010.25, 27100.00, 27000.00, 27080.10, 8.10),
        mk(1_700_000_120_000, 27080.10, 27090.00, 27010.00, 27033.33, 5.555555),
    ]
}

fn assert_bars_bit_eq(a: &[Bar], b: &[Bar]) {
    assert_eq!(a.len(), b.len(), "bar count");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        for (name, xv, yv) in [
            ("open", x.open, y.open),
            ("high", x.high, y.high),
            ("low", x.low, y.low),
            ("close", x.close, y.close),
            ("volume", x.volume, y.volume),
        ] {
            assert_eq!(xv.to_bits(), yv.to_bits(), "{name}[{i}] ({xv} vs {yv})");
        }
        assert_eq!(x.funding.map(f64::to_bits), y.funding.map(f64::to_bits), "funding[{i}]");
    }
}

#[test]
fn parse_klines_fixture_maps_bars_exactly_and_ascending() {
    let body = include_str!("fixtures/bybit_klines_btcusdt_1m.json");
    let got = parse_bybit_klines(body).unwrap();
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "reversed to strictly ascending");
    assert_bars_bit_eq(&expected_bars(), &got);
}

#[test]
fn append_then_load_is_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let body = include_str!("fixtures/bybit_klines_btcusdt_1m.json");
    let bars = parse_bybit_klines(body).unwrap();

    let key = klines_commit_key(SYMBOL, INTERVAL, 1_700_000_000_000, 1_700_000_179_999);
    let n = hist.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some(&key)).unwrap();
    assert_eq!(n, bars.len());

    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
}

#[test]
fn append_is_idempotent_by_commit_key() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let body = include_str!("fixtures/bybit_klines_btcusdt_1m.json");
    let bars = parse_bybit_klines(body).unwrap();
    let key = klines_commit_key(SYMBOL, INTERVAL, 1_700_000_000_000, 1_700_000_179_999);

    let first = hist.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some(&key)).unwrap();
    assert_eq!(first, bars.len());
    let second = hist.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some(&key)).unwrap();
    assert_eq!(second, 0, "same commit key twice must be a no-op");
}

#[test]
fn commit_key_is_stable_and_windowed() {
    assert_eq!(klines_commit_key("BTCUSDT", "1m", 0, 179_999), "bybit:BTCUSDT:1m:0-179999");
}

/// LIVE: hits Bybit's public REST endpoint. Run explicitly:
/// `cargo test -p vike-backfill --test bybit_backfill -- --ignored --nocapture live_`
#[test]
#[ignore = "live: hits Bybit's public REST klines endpoint"]
fn live_backfill_btcusdt_1m() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    // 2024-01-02 00:00–02:00 UTC — a liquid, contiguous window.
    let start = 1_704_153_600_000i64; // 2024-01-02T00:00:00Z
    let end = start + 2 * 3_600_000 - 60_000; // last 1m bar opens at 01:59

    let n = backfill_bybit_klines(&hist, "BTCUSDT", "1m", start, end).expect("backfill");
    eprintln!("backfilled {n} BTCUSDT 1m bars");
    assert!(n > 0, "a liquid window should yield bars");

    // idempotent second run
    let again = backfill_bybit_klines(&hist, "BTCUSDT", "1m", start, end).expect("backfill#2");
    assert_eq!(again, 0, "same window is a no-op");

    let got = hist.load_bars("bybit", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), n, "loaded count matches appended count");
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "strictly ascending, no dup startTimes");
}
