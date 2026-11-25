//! The `.bi5` byte builders the format's tests share: this crate's archive tests and the
//! dukascopy bridge's (`tests/bi5_agrees_with_the_bridge.rs`, `tests/decoder_never_panics.rs`),
//! which reach them through a dev edge with `test-support`. Gated
//! `#[cfg(any(test, feature = "test-support"))]`, so no shipped build compiles it.

use crate::tick::REC_LEN;

/// One 20-byte `>IIIff` record.
pub fn record(offset: u32, ask: u32, bid: u32, ask_vol: f32, bid_vol: f32) -> Vec<u8> {
    let mut r = Vec::with_capacity(REC_LEN);
    r.extend_from_slice(&offset.to_be_bytes());
    r.extend_from_slice(&ask.to_be_bytes());
    r.extend_from_slice(&bid.to_be_bytes());
    r.extend_from_slice(&ask_vol.to_be_bytes());
    r.extend_from_slice(&bid_vol.to_be_bytes());
    r
}

/// `raw` LZMA-compressed by `lzma-rs` itself, size UNKNOWN in the header (an end-marker stream).
pub fn lzma(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::lzma_compress(&mut &raw[..], &mut out).expect("compress");
    out
}

/// `raw` LZMA-compressed by `lzma-rs`, its size DECLARED in the header.
pub fn lzma_declared(raw: &[u8]) -> Vec<u8> {
    let options = lzma_rs::compress::Options {
        unpacked_size: lzma_rs::compress::UnpackedSize::WriteToHeader(Some(raw.len() as u64)),
    };
    let mut out = Vec::new();
    lzma_rs::lzma_compress_with_options(&mut &raw[..], &mut out, &options).expect("compress");
    out
}
