//! The refusal: why a `.bi5` file is refused, and the class a report names it by.

use std::fmt;

#[cfg(doc)]
use super::limits::Bi5Limits;

// ── the refusal ─────────────────────────────────────────────────────────────────────────────────

/// Why a `.bi5` file is refused. A refused file stores nothing and spends no key.
///
/// The variants carry sizes, caps and a record INDEX — never payload bytes. A report that must name
/// only a class sends [`Bi5Refusal::class`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bi5Refusal {
    /// The file is larger than [`Bi5Limits::max_compressed_bytes`].
    CompressedTooLarge { len: usize, cap: usize },
    /// The header DECLARES a decoded size over [`Bi5Limits::max_decoded_bytes`] — refused before
    /// anything is decoded.
    DeclaredTooLarge { declared: u64, cap: usize },
    /// Decoding passed [`Bi5Limits::max_decoded_bytes`] — a stream whose header did not say how big
    /// it is, and that would not stop.
    DecodedTooLarge { cap: usize },
    /// The stream ends before its header, or before the data it promises.
    Truncated,
    /// Not a decodable LZMA-alone stream. `reason` is the decoder's own diagnostic.
    Corrupt { reason: String },
    /// `len` decoded (or declared) bytes are not a whole number of 20-byte records.
    PartialRecord { len: u64 },
    /// Record `record`'s millisecond offset is at or past 24 h: the content is not a daily file.
    OutOfRange { record: usize },
    /// Record `record`'s offset is earlier than the one before it — hour-relative offsets
    /// restarting at 0.
    NotMonotonic { record: usize },
    /// Every tick lies in the day's first hour: one hour-relative payload filed under a day name
    /// would look exactly like it, and nothing in it says which hour it is.
    AmbiguousTimeBase,
}

impl Bi5Refusal {
    /// The refusal's class — the variant's name, `"AmbiguousTimeBase"` — for a report line that
    /// carries a class and never the file's content.
    pub fn class(&self) -> &'static str {
        match self {
            Bi5Refusal::CompressedTooLarge { .. } => "CompressedTooLarge",
            Bi5Refusal::DeclaredTooLarge { .. } => "DeclaredTooLarge",
            Bi5Refusal::DecodedTooLarge { .. } => "DecodedTooLarge",
            Bi5Refusal::Truncated => "Truncated",
            Bi5Refusal::Corrupt { .. } => "Corrupt",
            Bi5Refusal::PartialRecord { .. } => "PartialRecord",
            Bi5Refusal::OutOfRange { .. } => "OutOfRange",
            Bi5Refusal::NotMonotonic { .. } => "NotMonotonic",
            Bi5Refusal::AmbiguousTimeBase => "AmbiguousTimeBase",
        }
    }
}

impl fmt::Display for Bi5Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = self.class();
        match self {
            Bi5Refusal::CompressedTooLarge { len, cap } => {
                write!(f, "{class}: the file is {len} bytes, over the {cap}-byte cap")
            }
            Bi5Refusal::DeclaredTooLarge { declared, cap } => write!(
                f,
                "{class}: the header declares {declared} decoded bytes, over the {cap}-byte cap — \
                 refused before decoding"
            ),
            Bi5Refusal::DecodedTooLarge { cap } => {
                write!(f, "{class}: decoding passed the {cap}-byte cap")
            }
            Bi5Refusal::Truncated => {
                write!(f, "{class}: the stream ends before its header or its data does")
            }
            Bi5Refusal::Corrupt { reason } => {
                write!(f, "{class}: not a decodable LZMA-alone stream ({reason})")
            }
            Bi5Refusal::PartialRecord { len } => {
                write!(f, "{class}: {len} bytes are not a whole number of 20-byte records")
            }
            Bi5Refusal::OutOfRange { record } => write!(
                f,
                "{class}: record {record} is at or past 24 h from the day's start — not a daily file"
            ),
            Bi5Refusal::NotMonotonic { record } => write!(
                f,
                "{class}: record {record} is earlier than the one before it — the signature of \
                 hour-relative offsets"
            ),
            Bi5Refusal::AmbiguousTimeBase => write!(
                f,
                "{class}: every tick lies in the day's first hour, which is what one hour-relative \
                 payload filed under a day looks like — it cannot be placed"
            ),
        }
    }
}

impl std::error::Error for Bi5Refusal {}
