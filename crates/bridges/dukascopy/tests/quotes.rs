//! Offline + live gate for the bridge's tick→quote map and quote fetch (docs/decisions/0094 —
//! moved from `crates/vike-backfill/tests/dukascopy_backfill.rs`, since the
//! fetch half is this crate's now). `tick_to_quote_maps_fields_exactly` is the offline bit-exact
//! field-mapping pin; the live fetch is `#[ignore]`d.

use vike_dukascopy::{Tick, tick_to_quote};

const SYMBOL: &str = "EURUSD";

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

/// LIVE: hits Dukascopy's public datafeed CDN. Run explicitly:
/// `cargo test -p vike-dukascopy --test quotes -- --ignored --nocapture live_`
#[test]
#[ignore = "live: hits Dukascopy's public datafeed CDN"]
fn live_fetch_quotes_eurusd_hour() {
    // 2024-01-03 (Wed) 14:00–15:00 UTC — a liquid London/NY-overlap hour.
    let start = 1_704_290_400_000i64;
    let quotes =
        vike_dukascopy::fetch_quotes_range("EURUSD", start, start + 3_600_000 - 1).expect("fetch");
    assert!(!quotes.is_empty(), "a liquid hour should yield quotes");
    assert!(quotes.windows(2).all(|w| w[0].ts <= w[1].ts));
}
