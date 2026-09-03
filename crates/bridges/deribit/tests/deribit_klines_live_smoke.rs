//! Live smoke for [`vike_deribit::data::fetch_klines_range`] — connects to REAL Deribit mainnet
//! public REST (`public/get_tradingview_chart_data`): **keyless**, read-only, no auth, no order, no
//! account, so unlike the exec smokes there is no credential gate to satisfy — only network. Both
//! tests are `#[ignore]`d so CI never dials the venue.
//!
//! ```sh
//! cargo test -p vike-deribit --test deribit_klines_live_smoke -- --ignored --nocapture
//! ```
//!
//! The second test is the one that matters: it asks for a window WIDER than the endpoint's
//! (undocumented, ~5001-row) cap. That is the case the endpoint answers `status:"ok"` to while
//! silently dropping the older tail — the fixture tests model it, this proves it against the real
//! venue.
//!
//! Set `VIKE_LOG_FILE_LEVEL=warn` for a long run: the file log layer defaults to `trace`.

use std::time::Duration;

use vike_deribit::data::fetch_klines_range;
use vike_model::{now_ms, Bar};

/// The venue's own perpetual — keyless, always listed, always liquid.
const SYMBOL: &str = "BTC-PERPETUAL";

/// Shared assertions on any returned window: ascending + de-duplicated, clipped to the request, and
/// carrying sane OHLCV (`volume` is BASE units, so it is small — a `cost`/`volume` mix-up shows up
/// as a wildly large number here).
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
#[ignore = "hits real Deribit mainnet public REST; run explicitly with --ignored"]
fn fetches_a_small_live_window() {
    vike_log::test_init();
    let end = now_ms();
    let start = end - 30 * 60_000; // the last 30 minutes of 1m candles — one page, no paging
    let bars = fetch_klines_range(SYMBOL, "1m", start, end).expect("live 1m fetch");
    println!("deribit {SYMBOL} 1m: {} bars over 30m", bars.len());
    assert!(!bars.is_empty(), "a liquid perp has 1m candles in the last half hour");
    assert!(bars.len() >= 20, "expected ~31 bars, got {}", bars.len());
    assert_window(&bars, start, end);
}

/// The load-bearing live proof: a window WIDER than one page. A forward/single-request
/// implementation returns `status:"ok"` with the newest ~5001 rows and looks perfectly healthy —
/// this asserts strictly more than that came back, and that the OLDEST bar is near the requested
/// start rather than near the end.
#[test]
#[ignore = "hits real Deribit mainnet public REST (several pages); run explicitly with --ignored"]
fn fetches_a_window_wider_than_one_page() {
    vike_log::test_init();
    let end = now_ms();
    let minutes = 8_000i64; // > the observed ~5001-row cap, so the walk MUST page
    let start = end - minutes * 60_000;
    let t = std::time::Instant::now();
    let bars = fetch_klines_range(SYMBOL, "1m", start, end).expect("live paged 1m fetch");
    println!("deribit {SYMBOL} 1m: {} bars over {minutes}m in {:?}", bars.len(), t.elapsed());

    assert!(
        bars.len() > 5_001,
        "a single end-anchored page caps out at ~5001 rows; the backward walk must exceed it (got {})",
        bars.len()
    );
    assert_window(&bars, start, end);

    // The oldest bar must sit near the REQUESTED start, not near the end (the truncation signature).
    let oldest = bars.first().expect("non-empty").ts;
    let slack = 60 * 60_000; // one hour of tolerance for venue-side history edges
    assert!(
        oldest - start < slack,
        "oldest bar {oldest} is {} min after the requested start — the older tail was dropped",
        (oldest - start) / 60_000
    );
    assert!(t.elapsed() < Duration::from_secs(120), "a 2-3 page walk should not take minutes");
}
