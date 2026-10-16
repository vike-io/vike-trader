//! The `.bi5` tick RECORD and the HOURLY file's decoder — the format half of Dukascopy's keyless
//! per-UTC-hour tick archive, the file the HTTP datafeed serves (`crates/bridges/dukascopy/src/data.rs`'s
//! `hour_url` names it, and its `fetch_hour` downloads it).
//!
//! File format (one hour per file): LZMA-compressed 20-byte big-endian records `>3i2f` =
//! (ms-offset-into-hour, ask_points, bid_points, ask_vol, bid_vol); prices are integer "points",
//! divide by 10^digits (5 for most pairs, 3 for JPY-quoted). Everything here is a pure function over
//! bytes: no network, no store. The DAILY layout the archive import lane reads shares the record
//! and decodes into the same [`Tick`] (`crate::archive`).

use vike_marketdata::QuoteTick;

/// One UTC hour in milliseconds — the span one hourly file covers.
pub const HOUR_MS: i64 = 3_600_000;
/// One tick record's length, in bytes — the hourly AND the daily layout's (`crate::archive`).
pub const REC_LEN: usize = 20; // >3i2f

/// One decoded tick (`ts` = epoch-ms UTC).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub ts: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_vol: f64,
    pub ask_vol: f64,
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

/// Pure map: one Dukascopy [`Tick`] → a [`QuoteTick`]. Bid/ask pass through unchanged; the venue's
/// per-side tick *volumes* become the L1 *sizes* (bid_vol→bid_size, ask_vol→ask_size). No rounding
/// or arithmetic — the hist store's `to_bits()` parity gate must hold bit-exactly across this hop.
pub fn tick_to_quote(tick: &Tick, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts: tick.ts,
        local_ts: 0,
        bid: tick.bid,
        ask: tick.ask,
        bid_size: tick.bid_vol,
        ask_size: tick.ask_vol,
        symbol: symbol.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
