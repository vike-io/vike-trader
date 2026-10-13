//! The `archive` module's unit tests: the shared fixtures and the live probe here, one child per role.

use chrono::NaiveDate;

use super::decode::decompress_bounded;
use super::limits::{Bi5Limits, MIB};
use super::path::Bi5Day;
use super::refusal::Bi5Refusal;
use crate::data::REC_LEN;
use crate::price_scale::{Bi5PriceScale, bi5_price_scale};

#[cfg(test)]
mod bounds;
#[cfg(test)]
mod decode;
#[cfg(test)]
mod lzma_encoder;
#[cfg(test)]
mod path;

// ── fixtures ────────────────────────────────────────────────────────────────────────────────

/// "Now" for the grammar's year bound: 2026-09-30 12:00 UTC.
fn now() -> i64 {
    NaiveDate::from_ymd_opt(2026, 9, 30)
        .and_then(|d| d.and_hms_opt(12, 0, 0))
        .map(|dt| dt.and_utc().timestamp_millis())
        .expect("a real instant")
}

/// A calendar day (ONE-based month, as people write it).
fn day(year: i32, month: u32, dom: u32) -> Bi5Day {
    let date = NaiveDate::from_ymd_opt(year, month, dom).expect("a real date");
    Bi5Day::from_start_ms(date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis())
        .expect("a midnight")
}

fn eurusd() -> &'static Bi5PriceScale {
    bi5_price_scale("EURUSD").expect("EURUSD is admitted")
}

/// One 20-byte `>IIIff` record.
fn record(offset: u32, ask: u32, bid: u32, ask_vol: f32, bid_vol: f32) -> Vec<u8> {
    let mut r = Vec::with_capacity(REC_LEN);
    r.extend_from_slice(&offset.to_be_bytes());
    r.extend_from_slice(&ask.to_be_bytes());
    r.extend_from_slice(&bid.to_be_bytes());
    r.extend_from_slice(&ask_vol.to_be_bytes());
    r.extend_from_slice(&bid_vol.to_be_bytes());
    r
}

/// Records at these offsets, every one quoting 1.10123 / 1.10120.
fn payload(offsets: &[u32]) -> Vec<u8> {
    offsets.iter().flat_map(|&o| record(o, 110_123, 110_120, 1.5, 2.0)).collect()
}

/// `raw` LZMA-compressed by `lzma-rs` itself, size UNKNOWN in the header (an end-marker stream).
fn lzma(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::lzma_compress(&mut &raw[..], &mut out).expect("compress");
    out
}

/// `raw` LZMA-compressed by `lzma-rs`, its size DECLARED in the header.
fn lzma_declared(raw: &[u8]) -> Vec<u8> {
    let options = lzma_rs::compress::Options {
        unpacked_size: lzma_rs::compress::UnpackedSize::WriteToHeader(Some(raw.len() as u64)),
    };
    let mut out = Vec::new();
    lzma_rs::lzma_compress_with_options(&mut &raw[..], &mut out, &options).expect("compress");
    out
}

/// The decoded bytes alone, under `limits`.
fn inflate(blob: &[u8], limits: Bi5Limits) -> Result<Vec<u8>, Bi5Refusal> {
    decompress_bounded(blob, &limits)
}

fn limits(max_compressed_bytes: usize, max_decoded_bytes: usize) -> Bi5Limits {
    Bi5Limits { max_compressed_bytes, max_decoded_bytes }
}

/// A 13-byte header with an arbitrary declared size, in front of `body`.
fn header_then(declared: u64, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x5D]; // lc = 3, lp = 0, pb = 2 — the common properties byte
    out.extend_from_slice(&(4u32 * MIB as u32).to_le_bytes());
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(body);
    out
}

// ── the live probe (§3.4) ───────────────────────────────────────────────────────────────────

/// LIVE MEASUREMENT PROBE for a price-scale row — not a check of this module, and it asserts
/// nothing about any scale. For every instrument of `bundled_instruments` the archive lane does
/// NOT admit (the exotics and the metals), it downloads one liquid hour from the PUBLIC datafeed
/// (no credentials), decodes it through this module's bounded decompression, and prints its
/// first tick's RAW integer points, the UTC minute, and the price each candidate point value
/// would give. A human compares those with an independent quote for the same minute, and the
/// candidate that matches becomes the instrument's row, with that comparison as its evidence.
/// Run explicitly, never in CI:
/// `cargo test -p vike-dukascopy --lib archive::tests::live_probe -- --ignored --nocapture`
#[test]
#[ignore = "live: hits Dukascopy's public datafeed CDN — a MEASUREMENT for a scale row, run manually"]
fn live_probe_prints_raw_points_for_every_unscaled_instrument() {
    vike_log::test_init();
    // 2024-01-03 (Wed) 14:00 UTC — a liquid London/NY-overlap hour.
    let hour_start: i64 = 1_704_290_400_000;
    for instrument in crate::catalog::bundled_instruments() {
        let name = instrument.raw_symbol;
        if bi5_price_scale(&name).is_some() {
            continue;
        }
        let blob = match crate::data::fetch_hour(&name, hour_start) {
            Ok(Some(blob)) if !blob.is_empty() => blob,
            Ok(_) => {
                tracing::info!(target: "vike_dukascopy::archive", "{name}: no ticks in the probe hour");
                continue;
            }
            Err(e) => {
                tracing::warn!(target: "vike_dukascopy::archive", "{name}: fetch failed: {e}");
                continue;
            }
        };
        let raw = match decompress_bounded(&blob, &Bi5Limits::DEFAULT) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(target: "vike_dukascopy::archive", "{name}: {e}");
                continue;
            }
        };
        let Some(first) = raw.chunks_exact(REC_LEN).next() else {
            tracing::info!(target: "vike_dukascopy::archive", "{name}: an empty hour");
            continue;
        };
        let be_u32 =
            |i: usize| u32::from_be_bytes([first[i], first[i + 1], first[i + 2], first[i + 3]]);
        let (offset, ask, bid) = (be_u32(0), be_u32(4), be_u32(8));
        let minute = (hour_start + i64::from(offset)) / 60_000 * 60_000;
        let at = |point_value: f64| (f64::from(ask) / point_value, f64::from(bid) / point_value);
        tracing::info!(
            target: "vike_dukascopy::archive",
            "{name} at {}: ask {ask} / bid {bid} points — at 10: {:?}, 100: {:?}, 1,000: {:?}, \
             10,000: {:?}, 100,000: {:?}",
            chrono::DateTime::from_timestamp_millis(minute).map(|t| t.to_rfc3339()).unwrap_or_default(),
            at(10.0),
            at(100.0),
            at(1_000.0),
            at(10_000.0),
            at(100_000.0),
        );
    }
}
