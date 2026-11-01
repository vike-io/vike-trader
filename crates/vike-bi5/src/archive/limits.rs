//! The bounds (§5) and the header: the caps a `.bi5` file is decoded under, and what its header says.

use super::refusal::Bi5Refusal;
use crate::tick::REC_LEN;

/// The LZMA-alone header: a properties byte, the dictionary size as a little-endian `u32`, and the
/// unpacked size as a little-endian `u64`.
const HEADER_LEN: usize = 13;
/// The unpacked-size field's "unknown — read to the end-of-stream marker" value.
pub(super) const UNKNOWN_SIZE: u64 = u64::MAX;
/// One past the largest valid properties byte, `lc + 9 * (lp + 5 * pb)` with `lc <= 8`, `lp <= 4`
/// and `pb <= 4`. `lzma-rs` refuses the same bound; checking it here lets a plan that reads only
/// headers refuse it too.
pub(super) const PROPS_LIMIT: u8 = 225;
pub(super) const MIB: usize = 1024 * 1024;

// ── the bounds (§5) and the header ──────────────────────────────────────────────────────────────

/// The resource caps one `.bi5` file is decoded under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bi5Limits {
    /// The largest FILE accepted, in bytes. It bounds the read buffer, so a reader takes at most
    /// one byte more than this before refusing.
    pub max_compressed_bytes: usize,
    /// The largest DECODED payload accepted, in bytes — the header's declared size, the LZMA
    /// dictionary window and the decoder's output are each held to it.
    pub max_decoded_bytes: usize,
}

impl Bi5Limits {
    /// The design's §5 caps — DEFAULTS, not measurements; the acceptance run (T7) measures the
    /// busiest real days before they are fixed.
    ///
    /// - **compressed, 16 MiB.** EURUSD averages ~100 KB per object (the vendor's own listing), so
    ///   this is ~170 times that; at the ~4.8 compressed bytes a tick measured on one real file it
    ///   is ~3.5 M ticks, the same day as the decoded cap.
    /// - **decoded, 64 MiB** — 3,355,443 whole records. The one liquid hour measured held 4,201
    ///   ticks, so a day of such hours is ~0.1 M, about thirty times under the cap.
    pub const DEFAULT: Bi5Limits =
        Bi5Limits { max_compressed_bytes: 16 * MIB, max_decoded_bytes: 64 * MIB };
}

/// What a `.bi5` file's 13-byte LZMA-alone header says, read without decoding anything — what a
/// plan that reads only headers can know about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bi5Header {
    /// The decoded size the header DECLARES, or `None` for an unknown-size stream (one that runs to
    /// its end-of-stream marker). One real feed file was measured to declare it.
    pub declared_bytes: Option<u64>,
}

impl Bi5Header {
    /// How many ticks the declared size holds — exact, since [`read_bi5_header`] refuses a declared
    /// size that is not a whole number of records.
    pub fn declared_ticks(&self) -> Option<u64> {
        self.declared_bytes.map(|bytes| bytes / REC_LEN as u64)
    }
}

/// Read and check a `.bi5` file's header from its first bytes (`prefix` may be the whole file).
///
/// Refuses [`Bi5Refusal::Truncated`] for a prefix shorter than the 13-byte header,
/// [`Bi5Refusal::Corrupt`] for an impossible properties byte, [`Bi5Refusal::DeclaredTooLarge`] for a
/// declared size over `limits.max_decoded_bytes`, and [`Bi5Refusal::PartialRecord`] for a declared
/// size that is not a whole number of 20-byte records. An EMPTY prefix — a zero-byte file — is not
/// a stream at all: it is an empty payload, read as declaring zero bytes.
pub fn read_bi5_header(prefix: &[u8], limits: &Bi5Limits) -> Result<Bi5Header, Bi5Refusal> {
    if prefix.is_empty() {
        return Ok(Bi5Header { declared_bytes: Some(0) });
    }
    let header = prefix.get(..HEADER_LEN).ok_or(Bi5Refusal::Truncated)?;
    if header[0] >= PROPS_LIMIT {
        return Err(Bi5Refusal::Corrupt {
            reason: "the LZMA properties byte is out of range".to_string(),
        });
    }
    let mut size = [0u8; 8];
    size.copy_from_slice(&header[5..HEADER_LEN]);
    let declared = u64::from_le_bytes(size);
    if declared == UNKNOWN_SIZE {
        return Ok(Bi5Header { declared_bytes: None });
    }
    let cap = limits.max_decoded_bytes;
    if declared > cap as u64 {
        return Err(Bi5Refusal::DeclaredTooLarge { declared, cap });
    }
    if !declared.is_multiple_of(REC_LEN as u64) {
        return Err(Bi5Refusal::PartialRecord { len: declared });
    }
    Ok(Bi5Header { declared_bytes: Some(declared) })
}
