//! Decoding: one DAILY `.bi5` file into its ticks — bounded decompression, then the time-base checks.

use std::io;

// One UTC day in milliseconds — and the bound every offset of a daily file sits below.
use vike_model::MS_PER_DAY;

use super::limits::{Bi5Limits, read_bi5_header};
use super::path::Bi5Day;
use super::refusal::Bi5Refusal;
use crate::price_scale::Bi5PriceScale;
use crate::tick::{HOUR_MS, REC_LEN, Tick};

#[cfg(doc)]
use super::path::{Bi5Path, classify_bi5_path};

/// How `lzma-rs` 0.3 words the error its `memlimit` raises — the one decode failure that means "the
/// output passed the decoded cap" rather than "the stream is bad". It is matched by PREFIX, and
/// `a_bomb_claiming_a_4_gib_dictionary_is_refused_at_the_cap` fails if a version bump rewords it
/// (the refusal would then read `Corrupt` — still a refusal, but the wrong class).
const LZMA_RS_MEMLIMIT: &str = "exceeded memory limit";

// ── decoding ────────────────────────────────────────────────────────────────────────────────────

/// Decode one DAILY `.bi5` file into its ticks — bounded, validated, refused on any contradiction.
///
/// `day` is the day the file's PATH names ([`classify_bi5_path`]'s [`Bi5Path::Daily`]) and is the
/// only time base used. `scale` is the instrument's row of the price-scale table
/// (`crate::bi5_price_scale`), so an instrument nobody measured cannot reach this function. Prices
/// are record points divided by the scale's point value; volumes are the record's raw `f32`s, in
/// millions of base currency units, exactly as the HTTP lane stores them.
///
/// `Ok` with NO ticks is an empty payload — a zero-byte file, or a stream that decodes to nothing —
/// which stores nothing and must spend no key. Every [`Bi5Refusal`] is a whole-file refusal: no
/// partial day is ever returned.
pub fn decode_daily_file(
    blob: &[u8],
    day: Bi5Day,
    scale: &Bi5PriceScale,
    limits: &Bi5Limits,
) -> Result<Vec<Tick>, Bi5Refusal> {
    let raw = decompress_bounded(blob, limits)?;
    decode_daily_records(&raw, day, scale)
}

/// LZMA-decode `blob` under `limits`: the compressed cap, the header ([`read_bi5_header`]), then a
/// decode whose dictionary window (`memlimit`) and output (`CappedSink`) are both held to the
/// decoded cap. An empty `blob` is an empty payload.
pub fn decompress_bounded(blob: &[u8], limits: &Bi5Limits) -> Result<Vec<u8>, Bi5Refusal> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    if blob.len() > limits.max_compressed_bytes {
        return Err(Bi5Refusal::CompressedTooLarge {
            len: blob.len(),
            cap: limits.max_compressed_bytes,
        });
    }
    let header = read_bi5_header(blob, limits)?;
    let cap = limits.max_decoded_bytes;
    let mut sink = CappedSink::new(cap, header.declared_bytes);
    let options = lzma_rs::decompress::Options { memlimit: Some(cap), ..Default::default() };
    match lzma_rs::lzma_decompress_with_options(&mut &blob[..], &mut sink, &options) {
        Ok(()) => Ok(sink.buf),
        Err(_) if sink.refused => Err(Bi5Refusal::DecodedTooLarge { cap }),
        Err(lzma_rs::error::Error::LzmaError(m)) if m.starts_with(LZMA_RS_MEMLIMIT) => {
            Err(Bi5Refusal::DecodedTooLarge { cap })
        }
        Err(lzma_rs::error::Error::IoError(e) | lzma_rs::error::Error::HeaderTooShort(e))
            if e.kind() == io::ErrorKind::UnexpectedEof =>
        {
            Err(Bi5Refusal::Truncated)
        }
        Err(e) => Err(Bi5Refusal::Corrupt { reason: e.to_string() }),
    }
}

/// The output side of a bounded decode. It accepts bytes up to `cap` and refuses — WHOLE — the write
/// that would pass it, so it never holds more than `cap`; and it grows geometrically but never
/// reserves past `cap`, so it never ALLOCATES more either. `lzma-rs` flushes its dictionary window
/// here a window at a time, so an oversized stream is refused within one window of the cap.
pub(super) struct CappedSink {
    pub(super) buf: Vec<u8>,
    cap: usize,
    /// Set when a write was refused — the one way the decode's error means "too large".
    pub(super) refused: bool,
}

impl CappedSink {
    /// A sink for at most `cap` bytes, sized up front to a DECLARED size (already checked against
    /// the cap by [`read_bi5_header`]).
    pub(super) fn new(cap: usize, declared: Option<u64>) -> CappedSink {
        let expected = declared.map_or(0, |n| usize::try_from(n).unwrap_or(cap).min(cap));
        CappedSink { buf: Vec::with_capacity(expected), cap, refused: false }
    }
}

impl io::Write for CappedSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let held = self.buf.len();
        if data.len() > self.cap - held {
            self.refused = true;
            return Err(io::Error::other("the decoded-size cap"));
        }
        let needed = held + data.len();
        if needed > self.buf.capacity() {
            let grown = self.buf.capacity().saturating_mul(2).max(needed).min(self.cap);
            self.buf.reserve_exact(grown - held);
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decoded bytes into ticks, under §3.3's checks: whole records only, every offset inside the day,
/// never decreasing, and not all inside the first hour.
pub(super) fn decode_daily_records(
    raw: &[u8],
    day: Bi5Day,
    scale: &Bi5PriceScale,
) -> Result<Vec<Tick>, Bi5Refusal> {
    if !raw.len().is_multiple_of(REC_LEN) {
        return Err(Bi5Refusal::PartialRecord { len: raw.len() as u64 });
    }
    let divisor = f64::from(scale.point_value());
    let mut ticks = Vec::with_capacity(raw.len() / REC_LEN);
    let mut previous = 0u32;
    let mut past_first_hour = false;
    for (record, r) in raw.chunks_exact(REC_LEN).enumerate() {
        let be_u32 = |i: usize| u32::from_be_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
        let be_f32 = |i: usize| f64::from(f32::from_be_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]));
        let offset = be_u32(0);
        if i64::from(offset) >= MS_PER_DAY {
            return Err(Bi5Refusal::OutOfRange { record });
        }
        if offset < previous {
            return Err(Bi5Refusal::NotMonotonic { record });
        }
        previous = offset;
        past_first_hour |= i64::from(offset) >= HOUR_MS;
        ticks.push(Tick {
            ts: day.start_ms() + i64::from(offset),
            ask: f64::from(be_u32(4)) / divisor,
            bid: f64::from(be_u32(8)) / divisor,
            ask_vol: be_f32(12),
            bid_vol: be_f32(16),
        });
    }
    if !ticks.is_empty() && !past_first_hour {
        return Err(Bi5Refusal::AmbiguousTimeBase);
    }
    Ok(ticks)
}
