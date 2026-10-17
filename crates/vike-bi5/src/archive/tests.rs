//! The `archive` module's unit tests: the shared fixtures here, one child per role. The live price-scale
//! probe needs the bridge's catalog and fetch, so it is
//! `crates/bridges/dukascopy/tests/bi5_agrees_with_the_bridge.rs`'s.

use chrono::NaiveDate;

use super::decode::decompress_bounded;
use super::limits::{Bi5Limits, MIB};
use super::path::Bi5Day;
use super::refusal::Bi5Refusal;
use crate::price_scale::{Bi5PriceScale, bi5_price_scale};
use crate::tick::REC_LEN;

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
