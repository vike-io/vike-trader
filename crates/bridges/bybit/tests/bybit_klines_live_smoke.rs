//! Live smoke for [`vike_bybit::data::fetch_klines_range`] — connects to REAL Bybit mainnet public
//! REST (`/v5/market/kline`): **keyless**, read-only, no auth, no order, no account, so unlike the
//! exec smokes there is no credential gate to satisfy — only network. Every test is `#[ignore]`d so
//! CI never dials the venue.
//!
//! ```sh
//! cargo test -p vike-bybit --test bybit_klines_live_smoke -- --ignored --nocapture
//! ```
//!
//! The second test is the one that matters. Bybit's kline endpoint is **end-anchored**: once the
//! requested window exceeds `limit` it silently ignores `start` and answers the newest `limit` rows
//! with `retCode: 0`, which reads as a perfectly healthy success. MEASURED on the CI box 2026-08-04, the
//! pre-fix forward pager returned **1,001 rows for a 43,200-minute (30-day) request** — ~2% of the
//! data — for all six BTC/ETH/SOL spot+perp combos. The unit tests in `src/data.rs` model that
//! failure with a synthetic venue; this proves the backward walk against the real one.
//!
//! Set `VIKE_LOG_FILE_LEVEL=warn` for a long run: the file log layer defaults to `trace`.

use std::time::Duration;

use vike_bybit::data::fetch_klines_range;
use vike_model::{now_ms, Bar};

/// Bybit's hard cap on klines per response — the number a truncated fetch stops at.
const PAGE_CAP: usize = 1000;
/// Spot (bare) and linear-perp (`.P`-suffixed) BTC — both were measured truncated, and the suffix
/// also exercises the `category=linear` routing.
const SPOT: &str = "BTCUSDT";
const PERP: &str = "BTCUSDT.P";

/// Shared assertions on any returned window: ascending + de-duplicated, clipped to the request, and
/// carrying sane OHLCV (`volume` is BASE units, so it stays small — a `turnover`/`volume` mix-up
/// shows up here as a wildly large number).
fn assert_window(bars: &[Bar], start_ms: i64, end_ms: i64) {
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending by ts and de-duplicated");
    for b in bars {
        assert!(b.ts >= start_ms && b.ts <= end_ms, "bar {} outside the request window", b.ts);
        assert!(b.open > 0.0 && b.close > 0.0, "prices are positive");
        assert!(b.high >= b.low, "high/low are not swapped");
        assert!(b.high >= b.open && b.high >= b.close, "high bounds the bar");
        assert!(b.low <= b.open && b.low <= b.close, "low bounds the bar");
        assert!(b.volume >= 0.0 && b.volume.is_finite(), "volume is base units");
        assert!(b.symbol.is_none(), "the series key carries the symbol, not the bar");
    }
}

#[test]
#[ignore = "hits real Bybit mainnet public REST; run explicitly with --ignored"]
fn fetches_a_small_live_window() {
    vike_log::test_init();
    let end = now_ms();
    let start = end - 30 * 60_000; // the last 30 minutes of 1m klines — fits in one page
    let bars = fetch_klines_range(SPOT, "1m", start, end).expect("live 1m fetch");
    println!("bybit {SPOT} 1m: {} bars over 30m", bars.len());
    assert!(!bars.is_empty(), "a liquid spot pair has 1m klines in the last half hour");
    assert!(bars.len() >= 20, "expected ~31 bars, got {}", bars.len());
    assert_window(&bars, start, end);
}

/// The load-bearing live proof: a window WIDER than one page, on BOTH the spot and the linear-perp
/// category. The pre-fix pager answered exactly `PAGE_CAP` (+1) rows here and reported success, so
/// the row count is the assertion that matters — it must track the requested WINDOW, not the cap.
#[test]
#[ignore = "hits real Bybit mainnet public REST (several pages per symbol); run with --ignored"]
fn fetches_a_window_wider_than_one_page() {
    vike_log::test_init();
    let minutes = 2_500i64; // 2.5x the 1000-row cap, so the walk MUST page three times
    let slack = 30i64; // venue-side edges: a missing print minute, the forming candle, clock skew

    for symbol in [SPOT, PERP] {
        let end = now_ms();
        let start = end - minutes * 60_000;
        let t = std::time::Instant::now();
        let bars = fetch_klines_range(symbol, "1m", start, end).expect("live paged 1m fetch");
        println!("bybit {symbol} 1m: {} bars over {minutes}m in {:?}", bars.len(), t.elapsed());

        assert!(
            bars.len() > PAGE_CAP,
            "{symbol}: a single end-anchored page caps at {PAGE_CAP} rows; the backward walk must \
             exceed it (got {})",
            bars.len()
        );
        // The row count must match the WINDOW: ~one 1m kline per requested minute.
        let got = bars.len() as i64;
        assert!(
            (minutes - slack..=minutes + 2).contains(&got),
            "{symbol}: expected ~{minutes} 1m bars for a {minutes}-minute window, got {got}"
        );
        assert_window(&bars, start, end);

        // The oldest bar must sit near the REQUESTED start, not near the end — that gap IS the
        // truncation signature (30 days requested, oldest bar ~16.6 h before `end`).
        let oldest = bars.first().expect("non-empty").ts;
        assert!(
            oldest - start < slack * 60_000,
            "{symbol}: oldest bar {oldest} is {} min after the requested start — the older tail \
             was dropped",
            (oldest - start) / 60_000
        );
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "{symbol}: a 3-page walk should not take minutes"
        );
    }
}
