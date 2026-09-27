//! Dukascopy FX source — keyless deep tick history (bid/ask, back to ~2003), history-only (T+1 lag).
//!
//! Port of the Python `dukascopy_source`. Downloads the public per-UTC-hour `.bi5` tick archives (no
//! key), LZMA-decodes them, and aggregates ticks to [`Bar`] OHLC at any step. File format (one hour
//! per file): LZMA-compressed 20-byte big-endian records `>3i2f` =
//! (ms-offset-into-hour, ask_points, bid_points, ask_vol, bid_vol); prices are integer "points",
//! divide by 10^digits (5 for most pairs, 3 for JPY-quoted). OHLC uses the bid/ask **mid**; `volume`
//! is the tick count (FX has no true volume). The pure parts are unit-tested; only `fetch_hour` does
//! network I/O.

use std::io::Read;

use vike_model::Bar;

const DATAFEED: &str = "https://datafeed.dukascopy.com/datafeed";
const HOUR_MS: i64 = 3_600_000;
const REC_LEN: usize = 20; // >3i2f

/// One decoded tick (`ts` = epoch-ms UTC).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub ts: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_vol: f64,
    pub ask_vol: f64,
}

/// Price scale for a pair: `1e3` for JPY-quoted (3 digits), else `1e5` (5 digits).
pub fn point_divisor(symbol: &str) -> f64 {
    if symbol.to_ascii_uppercase().ends_with("JPY") { 1e3 } else { 1e5 }
}

/// LZMA-decompress a `.bi5` payload (legacy `.lzma`/ALONE stream, as Dukascopy serves).
pub fn decompress(blob: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    lzma_rs::lzma_decompress(&mut std::io::Cursor::new(blob), &mut out)
        .map_err(|e| format!("bi5 lzma decode: {e}"))?;
    Ok(out)
}

/// Decode decompressed bytes into ticks, anchoring each timestamp to `hour_start_ms`.
pub fn decode_ticks(raw: &[u8], hour_start_ms: i64, divisor: f64) -> Vec<Tick> {
    raw.chunks_exact(REC_LEN)
        .map(|c| {
            let be_i32 = |i: usize| i32::from_be_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
            let be_f32 = |i: usize| f32::from_be_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]) as f64;
            Tick {
                ts: hour_start_ms + be_i32(0) as i64,
                ask: be_i32(4) as f64 / divisor,
                bid: be_i32(8) as f64 / divisor,
                ask_vol: be_f32(12),
                bid_vol: be_f32(16),
            }
        })
        .collect()
}

/// Aggregate ticks into OHLC bars of `step_ms`, using the bid/ask mid. `volume` = tick count in the
/// bucket. Assumes ticks ascending by `ts`; emits bars ascending.
pub fn ticks_to_bars(ticks: &[Tick], step_ms: i64) -> Vec<Bar> {
    use std::collections::BTreeMap;
    // per bucket: (open, high, low, close, count)
    let mut buckets: BTreeMap<i64, (f64, f64, f64, f64, f64)> = BTreeMap::new();
    for t in ticks {
        let bucket = t.ts.div_euclid(step_ms) * step_ms;
        let mid = (t.bid + t.ask) / 2.0;
        buckets
            .entry(bucket)
            .and_modify(|b| {
                b.1 = b.1.max(mid);
                b.2 = b.2.min(mid);
                b.3 = mid;
                b.4 += 1.0;
            })
            .or_insert((mid, mid, mid, mid, 1.0));
    }
    buckets
        .into_iter()
        .map(|(ts, (open, high, low, close, count))| Bar {
            ts,
            open,
            high,
            low,
            close,
            volume: count,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// Build the `.bi5` URL for a UTC hour. Month is 0-indexed (January = 00), matching Dukascopy.
pub fn hour_url(symbol: &str, hour_start_ms: i64) -> String {
    use chrono::{Datelike, Timelike};
    let dt = chrono::DateTime::from_timestamp_millis(hour_start_ms).unwrap_or_default();
    format!(
        "{DATAFEED}/{}/{:04}/{:02}/{:02}/{:02}h_ticks.bi5",
        symbol.to_ascii_uppercase(),
        dt.year(),
        dt.month0(),
        dt.day(),
        dt.hour(),
    )
}

/// Download one hour's `.bi5` — `Ok(None)` when the hour is absent (404 = weekend / pre-listing /
/// not-yet-published). Retries transient CDN failures (429/5xx/network) a few times.
pub fn fetch_hour(symbol: &str, hour_start_ms: i64) -> Result<Option<Vec<u8>>, String> {
    let url = hour_url(symbol, hour_start_ms);
    let agent = vike_bridge_core::http::blocking_agent();
    let mut last_err = String::new();
    for attempt in 0..4 {
        match agent
            .get(&url)
            // Dukascopy's CDN prefers a browser-ish UA
            .header("User-Agent", "Mozilla/5.0 (vike-trader-rust forex source)")
            .call()
        {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                if status == 404 {
                    return Ok(None);
                }
                if (200..300).contains(&status) {
                    let mut buf = Vec::new();
                    resp.body_mut()
                        .as_reader()
                        .read_to_end(&mut buf)
                        .map_err(|e| format!("read: {e}"))?;
                    return Ok(Some(buf));
                }
                if !matches!(status, 429 | 500 | 502 | 503 | 504) {
                    return Err(format!("dukascopy {status}"));
                }
                last_err = format!("dukascopy {status}");
            }
            Err(e) => last_err = format!("network: {e}"),
        }
        // transient — back off (100ms, 200ms, 400ms) and retry
        std::thread::sleep(std::time::Duration::from_millis(100 << attempt));
    }
    Err(format!("dukascopy: {last_err} (after retries)"))
}

/// Fetch tick history for `[start_ms, end_ms]` (hour by hour, skipping absent/empty hours).
pub fn fetch_ticks_range(symbol: &str, start_ms: i64, end_ms: i64) -> Result<Vec<Tick>, String> {
    let divisor = point_divisor(symbol);
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(HOUR_MS) * HOUR_MS;
    while hour <= end_ms {
        if let Some(blob) = fetch_hour(symbol, hour)?
            && !blob.is_empty()
        {
            for t in decode_ticks(&decompress(&blob)?, hour, divisor) {
                if start_ms <= t.ts && t.ts <= end_ms {
                    out.push(t);
                }
            }
        }
        hour += HOUR_MS;
    }
    Ok(out)
}

/// Fetch tick history and aggregate to `step_ms` bars. History-only (T+1 publish lag).
pub fn fetch_bars_range(
    symbol: &str,
    step_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    Ok(ticks_to_bars(&fetch_ticks_range(symbol, start_ms, end_ms)?, step_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divisor_jpy_vs_others() {
        assert_eq!(point_divisor("USDJPY"), 1e3);
        assert_eq!(point_divisor("eurjpy"), 1e3);
        assert_eq!(point_divisor("EURUSD"), 1e5);
        assert_eq!(point_divisor("gbpusd"), 1e5);
    }

    #[test]
    fn hour_url_is_zero_indexed_month() {
        // 2020-01-02 03:00 UTC → epoch ms. January is month 00.
        let ms = 1_577_934_000_000; // 2020-01-02T03:00:00Z
        let u = hour_url("eurusd", ms);
        assert_eq!(u, "https://datafeed.dukascopy.com/datafeed/EURUSD/2020/00/02/03h_ticks.bi5");
    }

    #[test]
    fn decode_one_tick_record() {
        // one 20-byte >3i2f record: ms=1500, ask=110123 pts, bid=110120 pts, askVol=1.5, bidVol=2.0
        let mut raw = Vec::new();
        raw.extend_from_slice(&1500i32.to_be_bytes());
        raw.extend_from_slice(&110_123i32.to_be_bytes());
        raw.extend_from_slice(&110_120i32.to_be_bytes());
        raw.extend_from_slice(&1.5f32.to_be_bytes());
        raw.extend_from_slice(&2.0f32.to_be_bytes());
        let ticks = decode_ticks(&raw, 1_000_000, 1e5);
        assert_eq!(ticks.len(), 1);
        let t = ticks[0];
        assert_eq!(t.ts, 1_001_500);
        assert!((t.ask - 1.10123).abs() < 1e-9);
        assert!((t.bid - 1.10120).abs() < 1e-9);
        assert_eq!(t.ask_vol, 1.5);
        assert_eq!(t.bid_vol, 2.0);
    }

    #[test]
    fn ticks_aggregate_to_ohlc_on_mid() {
        // three ticks in the same 60s bucket, one in the next.
        let mk = |ts, bid, ask| Tick { ts, bid, ask, bid_vol: 0.0, ask_vol: 0.0 };
        let ticks = vec![
            mk(0, 1.0, 1.2),      // mid 1.1 (open)
            mk(1_000, 1.2, 1.4),  // mid 1.3 (high)
            mk(2_000, 0.9, 1.1),  // mid 1.0 (low, close of bucket 0)
            mk(60_000, 2.0, 2.0), // mid 2.0 (next bucket)
        ];
        let bars = ticks_to_bars(&ticks, 60_000);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].ts, 0);
        assert!((bars[0].open - 1.1).abs() < 1e-9);
        assert!((bars[0].high - 1.3).abs() < 1e-9);
        assert!((bars[0].low - 1.0).abs() < 1e-9);
        assert!((bars[0].close - 1.0).abs() < 1e-9);
        assert_eq!(bars[0].volume, 3.0); // tick count
        assert_eq!(bars[1].ts, 60_000);
        assert_eq!(bars[1].volume, 1.0);
    }

    #[test]
    fn decompress_round_trips() {
        // compress a synthetic tick payload with lzma-rs, then our decompress must recover it.
        let mut raw = Vec::new();
        for i in 0..5i32 {
            raw.extend_from_slice(&(i * 1000).to_be_bytes());
            raw.extend_from_slice(&110_000i32.to_be_bytes());
            raw.extend_from_slice(&109_998i32.to_be_bytes());
            raw.extend_from_slice(&1.0f32.to_be_bytes());
            raw.extend_from_slice(&1.0f32.to_be_bytes());
        }
        let mut compressed = Vec::new();
        lzma_rs::lzma_compress(&mut std::io::Cursor::new(&raw), &mut compressed).unwrap();
        assert_eq!(decompress(&compressed).unwrap(), raw);
    }

    /// LIVE: fetch one real EURUSD hour from Dukascopy and aggregate to 1-minute bars. A weekday
    /// mid-session hour has ticks; run explicitly:
    /// `cargo test -p vike-dukascopy --lib data::tests::live_fetch -- --ignored --nocapture`
    #[test]
    #[ignore = "live: hits Dukascopy's public datafeed CDN"]
    fn live_fetch_eurusd_hour() {
        vike_log::test_init();
        // 2024-01-03 (Wed) 14:00–15:00 UTC — a liquid London/NY-overlap hour.
        let start = 1_704_290_400_000; // 2024-01-03T14:00:00Z
        let end = start + HOUR_MS - 1;
        let bars = fetch_bars_range("EURUSD", 60_000, start, end).expect("fetch");
        tracing::info!(target: "vike_dukascopy::data", "EURUSD 2024-01-03 14:00Z → {} one-minute bars", bars.len());
        assert!(!bars.is_empty(), "a liquid hour should yield bars");
        assert!(bars.len() <= 60, "≤ 60 one-minute bars in an hour");
        for b in &bars {
            assert!(b.low <= b.open && b.open <= b.high);
            assert!(b.low <= b.close && b.close <= b.high);
            assert!((1.0..1.2).contains(&b.close), "EURUSD ~1.09 in Jan-2024, got {}", b.close);
        }
        tracing::debug!(target: "vike_dukascopy::data", "first bar: {:?}", bars.first());
    }
}
