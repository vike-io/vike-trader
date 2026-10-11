//! Dukascopy's DAILY `.bi5` tick files — the archive import lane's format half: the path grammar,
//! bounded decompression, and a daily decoder that REFUSES rather than guesses.
//!
//! `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §3.1–§3.4 and §5 are the design
//! (task T1 of its §9). Everything here is a pure function over path segments and bytes: no
//! filesystem walk, no store, no network. The walk, the confined open and the read through
//! `take(cap + 1)` belong to the datahub that mounts the lane; the archive day key, the day
//! classification and the append belong to the store half. Both consume this module.
//!
//! # The layout (§3.1)
//!
//! The vendor's Requester-Pays bucket holds `SYMBOL/YEAR/MONTH/DAY_ticks.bi5`, one file per UTC day,
//! and its MONTH IS ZERO-BASED (January = `00`). [`classify_bi5_path`] reads a path RELATIVE to the
//! dataset directory:
//!
//! - `<YYYY>/<MM>/<DD>_ticks.bi5` is a [`Bi5Path::Daily`] file — `YYYY` 1970 to the current year,
//!   `MM` `00`–`11`, `DD` `01`–`31`, and the triple a real calendar date (`2024/01/30`, 30 February,
//!   is not one). The day starts at UTC midnight of that date. These are imported.
//! - `<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5` is a [`Bi5Path::Hourly`] file — the HTTP datafeed's own
//!   layout (`crates/bridges/dukascopy/src/data.rs`'s `hour_url`) and the likeliest shape of the
//!   vendor's "legacy hourly files". Recognised and reported by day, NOT imported in v1 (Q6). A day
//!   holding both a daily file and hourly files is refused by the plan, which sees every path of a
//!   day; this function classifies one.
//! - Everything else, at any depth, is [`Bi5Path::Other`]: counted, never opened.
//!
//! [`is_bi5_layout_dir`] is the same grammar for the DIRECTORIES a walk may descend into.
//!
//! # The record, and how this decoder differs from the hourly one (§3.2)
//!
//! A record is 20 bytes, big-endian `>IIIff`: the millisecond offset, ask points, bid points, ask
//! volume, bid volume. `crates/bridges/dukascopy/src/data.rs`'s `decode_ticks` (the HTTP lane's)
//! shares the layout and differs in four ways, each deliberate:
//!
//! 1. **The base is the PATH's UTC midnight** ([`Bi5Day`]), never anything in the content.
//! 2. **The fields are UNSIGNED**, as the vendor documents; that decoder reads them signed (the
//!    design's F3). The two agree below 2^31 — any FX-scaled price under 21,474.84 — so they part
//!    only on an instrument the scale table does not admit yet.
//! 3. **A payload that is not a whole number of records is REFUSED as corrupt**, where that decoder's
//!    `chunks_exact` drops the trailing partial record without a word.
//! 4. **The content is checked against its time base (below) and decompression is bounded (§5)**;
//!    that decoder checks nothing, and its `decompress` has no output bound.
//!
//! It produces the same [`Tick`] that decoder does, for `crates/bridges/dukascopy/src/data.rs`'s
//! `tick_to_quote` to map: an identical record decodes to bit-identical `f64`s whichever lane read
//! it, which is what makes the lanes' one-owner-per-day rule honest (a test pins it).
//!
//! **Volumes** are passed through raw — the record's `f32`, widened — exactly as the HTTP lane
//! stores them. Their unit, per the vendor page, is **millions of base currency units**; neither
//! lane converts them.
//!
//! # The time base — decided by the path, checked on the content, refused on contradiction (§3.3)
//!
//! The vendor says older files "may use ms since start of hour instead of ms since start of day"
//! and that neither of its own decoders detects it. This one does, and answers ambiguity with
//! refusal — it never repairs an order, re-bases an offset or guesses an hour:
//!
//! 1. every offset is below 86,400,000, or the file is [`Bi5Refusal::OutOfRange`];
//! 2. offsets never DECREASE — equal ones are fine, several ticks can share a millisecond — and a
//!    decrease, the signature of hour-relative offsets restarting at 0, is
//!    [`Bi5Refusal::NotMonotonic`];
//! 3. a file whose EVERY tick lies in the day's first hour is [`Bi5Refusal::AmbiguousTimeBase`]:
//!    exactly what one hour-relative payload filed under a day name looks like, and nothing in it
//!    says WHICH hour it is. A genuine FX day almost never looks like it (Sunday's ticks start in
//!    the evening, UTC), and a false positive costs one refused, reported day;
//! 4. an EMPTY payload decodes to no ticks — it stores nothing and must spend no key.
//!
//! # The bounds (§5)
//!
//! [`Bi5Limits`] carries the caps, and every one of them is a DEFAULT until the acceptance run (T7)
//! measures the busiest real day. The decoded cap is enforced twice, because `lzma-rs`'s own
//! `memlimit` bounds only its DICTIONARY window and never its output: the header's declared size is
//! refused before anything is decoded, the dictionary window is held to the cap, and the output goes
//! through a writer that refuses the write that would pass the cap — so nothing is allocated past it.

use std::fmt;
use std::io;

use chrono::{Datelike, NaiveDate};

use crate::data::{HOUR_MS, REC_LEN, Tick};
use crate::price_scale::Bi5PriceScale;

/// One UTC day, in milliseconds — and the bound every offset of a daily file sits below.
const DAY_MS: i64 = 86_400_000;
/// A daily file's name after its two-digit day.
const DAILY_SUFFIX: &str = "_ticks.bi5";
/// An hourly-layout file's name after its two-digit hour.
const HOURLY_SUFFIX: &str = "h_ticks.bi5";
/// The LZMA-alone header: a properties byte, the dictionary size as a little-endian `u32`, and the
/// unpacked size as a little-endian `u64`.
const HEADER_LEN: usize = 13;
/// The unpacked-size field's "unknown — read to the end-of-stream marker" value.
const UNKNOWN_SIZE: u64 = u64::MAX;
/// One past the largest valid properties byte, `lc + 9 * (lp + 5 * pb)` with `lc <= 8`, `lp <= 4`
/// and `pb <= 4`. `lzma-rs` refuses the same bound; checking it here lets a plan that reads only
/// headers refuse it too.
const PROPS_LIMIT: u8 = 225;
const MIB: usize = 1024 * 1024;
/// How `lzma-rs` 0.3 words the error its `memlimit` raises — the one decode failure that means "the
/// output passed the decoded cap" rather than "the stream is bad". It is matched by PREFIX, and
/// `a_bomb_claiming_a_4_gib_dictionary_is_refused_at_the_cap` fails if a version bump rewords it
/// (the refusal would then read `Corrupt` — still a refusal, but the wrong class).
const LZMA_RS_MEMLIMIT: &str = "exceeded memory limit";

// ── the path grammar (§3.1) ─────────────────────────────────────────────────────────────────────

/// A UTC day as the archive's PATH names it — the time base of every tick in that day's file.
///
/// Always a UTC midnight on or after 1970-01-01: built by [`classify_bi5_path`] or checked by
/// [`Bi5Day::from_start_ms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bi5Day {
    start_ms: i64,
}

impl Bi5Day {
    /// The day starting at `start_ms`, or `None` when that is not a UTC midnight on or after the
    /// epoch.
    pub fn from_start_ms(start_ms: i64) -> Option<Bi5Day> {
        let midnight = start_ms >= 0 && start_ms % DAY_MS == 0;
        (midnight && chrono::DateTime::from_timestamp_millis(start_ms).is_some())
            .then_some(Bi5Day { start_ms })
    }

    /// Epoch-ms of the day's first millisecond (its UTC midnight).
    pub fn start_ms(self) -> i64 {
        self.start_ms
    }
}

impl fmt::Display for Bi5Day {
    /// The day as a calendar date, `2024-01-15` — the label a plan reports a day by.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match chrono::DateTime::from_timestamp_millis(self.start_ms) {
            Some(dt) => write!(f, "{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()),
            // unreachable: `from_start_ms` refuses what chrono cannot represent
            None => write!(f, "{} ms", self.start_ms),
        }
    }
}

/// What one path under a dataset directory is, by the layout grammar alone. Nothing is opened to
/// decide it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bi5Path {
    /// `<YYYY>/<MM>/<DD>_ticks.bi5` — a daily tick file, imported.
    Daily(Bi5Day),
    /// `<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5` — the HTTP datafeed's hourly layout: recognised and
    /// reported, NOT imported in v1.
    Hourly { day: Bi5Day, hour: u32 },
    /// Anything else — counted with its bytes, never opened.
    Other,
}

/// Classify one FILE path under a dataset directory. `rel` is the path's segments RELATIVE to the
/// dataset (`["2024", "00", "15_ticks.bi5"]`); `now_ms` bounds the year, which may not be later
/// than the current one. Names are matched exactly: `15_TICKS.BI5`, a one-digit day or month, or a
/// day file one level too deep is [`Bi5Path::Other`].
pub fn classify_bi5_path(rel: &[&str], now_ms: i64) -> Bi5Path {
    match rel {
        [year, month, file] => file
            .strip_suffix(DAILY_SUFFIX)
            .and_then(|day| layout_day(year, month, day, now_ms))
            .map_or(Bi5Path::Other, Bi5Path::Daily),
        [year, month, day, file] => {
            let hour = file.strip_suffix(HOURLY_SUFFIX).and_then(layout_hour);
            match (layout_day(year, month, day, now_ms), hour) {
                (Some(day), Some(hour)) => Bi5Path::Hourly { day, hour },
                _ => Bi5Path::Other,
            }
        }
        _ => Bi5Path::Other,
    }
}

/// Whether a DIRECTORY at `rel` (segments relative to the dataset, at least one) belongs to the
/// layout, so a walk may descend into it: a year, a year's month, or — the hourly layout's — a real
/// calendar day of that month. Any other directory is an unexpected name or depth, and a walk
/// skips it rather than reading below it.
pub fn is_bi5_layout_dir(rel: &[&str], now_ms: i64) -> bool {
    match rel {
        [year] => layout_year(year, now_ms).is_some(),
        [year, month] => layout_year(year, now_ms).is_some() && layout_month0(month).is_some(),
        [year, month, day] => layout_day(year, month, day, now_ms).is_some(),
        _ => false,
    }
}

/// `s` as a number, when it is exactly `width` ASCII digits.
fn fixed_digits(s: &str, width: usize) -> Option<u32> {
    if s.len() == width && s.bytes().all(|b| b.is_ascii_digit()) { s.parse().ok() } else { None }
}

/// A four-digit year from 1970 to `now_ms`'s year.
fn layout_year(s: &str, now_ms: i64) -> Option<i32> {
    let year = i32::try_from(fixed_digits(s, 4)?).ok()?;
    let current = chrono::DateTime::from_timestamp_millis(now_ms)?.year();
    (1970..=current).contains(&year).then_some(year)
}

/// A two-digit ZERO-BASED month, `00` (January) to `11` (December).
fn layout_month0(s: &str) -> Option<u32> {
    fixed_digits(s, 2).filter(|month0| *month0 <= 11)
}

/// A two-digit hour of the day, `00` to `23`.
fn layout_hour(s: &str) -> Option<u32> {
    fixed_digits(s, 2).filter(|hour| *hour <= 23)
}

/// The day a year, zero-based month and two-digit day name — when they form a real date.
fn layout_day(year: &str, month0: &str, day: &str, now_ms: i64) -> Option<Bi5Day> {
    let date = NaiveDate::from_ymd_opt(
        layout_year(year, now_ms)?,
        layout_month0(month0)? + 1,
        fixed_digits(day, 2)?,
    )?;
    Bi5Day::from_start_ms(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

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
/// decode whose dictionary window (`memlimit`) and output ([`CappedSink`]) are both held to the
/// decoded cap. An empty `blob` is an empty payload.
fn decompress_bounded(blob: &[u8], limits: &Bi5Limits) -> Result<Vec<u8>, Bi5Refusal> {
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
struct CappedSink {
    buf: Vec<u8>,
    cap: usize,
    /// Set when a write was refused — the one way the decode's error means "too large".
    refused: bool,
}

impl CappedSink {
    /// A sink for at most `cap` bytes, sized up front to a DECLARED size (already checked against
    /// the cap by [`read_bi5_header`]).
    fn new(cap: usize, declared: Option<u64>) -> CappedSink {
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
fn decode_daily_records(
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
        if i64::from(offset) >= DAY_MS {
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::data::{decode_ticks, point_divisor, tick_to_quote};
    use crate::price_scale::bi5_price_scale;

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
        Bi5Day::from_start_ms(
            date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis(),
        )
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

    // ── a minimal LZMA ENCODER, for the fixtures `lzma-rs`'s cannot produce ───────────────────────
    //
    // `lzma-rs`'s encoder emits LITERALS only — 70 MiB of zeros comes out at megabytes, not the few
    // KB a real decompression bomb is — and it always writes an 8 MiB dictionary, so it cannot make
    // a stream whose dictionary window flushes to the sink before a small cap.
    // This one writes `lc = lp = pb = 0` streams of literals followed by rep0 matches that repeat
    // the LAST byte (distance 1), then — unless the size is declared — the end-of-stream marker.
    // It is the exact inverse of `lzma-rs`'s range DECODER, and
    // `the_run_encoder_writes_streams_lzma_rs_itself_decodes` checks it against that decoder before
    // any other test leans on it.

    /// How a run stream ends.
    #[derive(Clone, Copy, Debug)]
    enum Ending {
        /// The header declares the size; no end marker.
        Declared,
        /// Unknown size in the header, then the end-of-stream marker.
        EndMarker,
        /// `EndMarker`, then the stream's last five bytes cut off — a decoder that reaches the end
        /// fails there, so a refusal of another class proves it stopped earlier.
        CutShort,
    }

    /// The LZMA SDK's range encoder.
    struct RangeEnc {
        out: Vec<u8>,
        low: u64,
        range: u32,
        cache: u8,
        pending: u64,
    }

    impl RangeEnc {
        fn bit(&mut self, prob: &mut u16, bit: bool) {
            let bound = (self.range >> 11) * u32::from(*prob);
            if bit {
                *prob -= *prob >> 5;
                self.low += u64::from(bound);
                self.range -= bound;
            } else {
                *prob += (0x800 - *prob) >> 5;
                self.range = bound;
            }
            self.normalize();
        }

        /// Fixed-probability bits, most significant first (a match distance's direct bits).
        fn direct(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                self.range >>= 1;
                if (value >> i) & 1 == 1 {
                    self.low += u64::from(self.range);
                }
                self.normalize();
            }
        }

        /// A bit tree, most significant bit first.
        fn tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
            let mut node = 1usize;
            for i in (0..bits).rev() {
                let bit = (value >> i) & 1 == 1;
                self.bit(&mut probs[node], bit);
                node = (node << 1) | usize::from(bit);
            }
        }

        /// A bit tree, least significant bit first (the distance's align bits).
        fn reverse_tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
            let mut node = 1usize;
            for i in 0..bits {
                let bit = (value >> i) & 1 == 1;
                self.bit(&mut probs[node], bit);
                node = (node << 1) | usize::from(bit);
            }
        }

        fn normalize(&mut self) {
            while self.range < 1 << 24 {
                self.range <<= 8;
                self.shift_low();
            }
        }

        fn shift_low(&mut self) {
            if self.low < 0xFF00_0000 || self.low > 0xFFFF_FFFF {
                let carry = (self.low >> 32) as u8;
                let mut byte = self.cache;
                loop {
                    self.out.push(byte.wrapping_add(carry));
                    byte = 0xFF;
                    self.pending -= 1;
                    if self.pending == 0 {
                        break;
                    }
                }
                self.cache = (self.low >> 24) as u8;
            }
            self.pending += 1;
            self.low = (self.low & 0x00FF_FFFF) << 8;
        }

        fn finish(mut self) -> Vec<u8> {
            for _ in 0..5 {
                self.shift_low();
            }
            self.out
        }
    }

    const HALF: u16 = 0x400;

    /// One length coder's probabilities (the decoder keeps two: rep matches and new matches).
    struct LenProbs {
        choice: u16,
        choice2: u16,
        low: [u16; 8],
        mid: [u16; 8],
        high: [u16; 256],
    }

    impl LenProbs {
        fn new() -> LenProbs {
            LenProbs {
                choice: HALF,
                choice2: HALF,
                low: [HALF; 8],
                mid: [HALF; 8],
                high: [HALF; 256],
            }
        }

        /// `v` = match length − 2, at position state 0.
        fn encode(&mut self, rc: &mut RangeEnc, v: u32) {
            if v < 8 {
                rc.bit(&mut self.choice, false);
                rc.tree(&mut self.low, 3, v);
            } else if v < 16 {
                rc.bit(&mut self.choice, true);
                rc.bit(&mut self.choice2, false);
                rc.tree(&mut self.mid, 3, v - 8);
            } else {
                rc.bit(&mut self.choice, true);
                rc.bit(&mut self.choice2, true);
                rc.tree(&mut self.high, 8, v - 16);
            }
        }
    }

    /// The encoder's model: the decoder's probability slots for state × position state 0.
    struct RunEncoder {
        rc: RangeEnc,
        state: usize,
        is_match: [u16; 12],
        is_rep: [u16; 12],
        is_rep_g0: [u16; 12],
        is_rep0_long: [u16; 12],
        literal: [u16; 0x100],
        rep_len: LenProbs,
        match_len: LenProbs,
        pos_slot: [u16; 64],
        align: [u16; 16],
    }

    impl RunEncoder {
        fn new(dict: u32, declared: Option<u64>) -> RunEncoder {
            let mut out = vec![0u8]; // lc = lp = pb = 0
            out.extend_from_slice(&dict.to_le_bytes());
            out.extend_from_slice(&declared.unwrap_or(UNKNOWN_SIZE).to_le_bytes());
            RunEncoder {
                rc: RangeEnc { out, low: 0, range: u32::MAX, cache: 0, pending: 1 },
                state: 0,
                is_match: [HALF; 12],
                is_rep: [HALF; 12],
                is_rep_g0: [HALF; 12],
                is_rep0_long: [HALF; 12],
                literal: [HALF; 0x100],
                rep_len: LenProbs::new(),
                match_len: LenProbs::new(),
                pos_slot: [HALF; 64],
                align: [HALF; 16],
            }
        }

        fn literal(&mut self, byte: u8) {
            let s = self.state;
            assert!(s < 7, "only a literal after a literal is modelled (no matched literal)");
            self.rc.bit(&mut self.is_match[s], false);
            self.rc.tree(&mut self.literal, 8, u32::from(byte));
            self.state = if s < 4 { 0 } else { s - 3 };
        }

        /// A rep0 match of `len` (1 = a short rep), repeating at the last distance — 1 here.
        fn rep0(&mut self, len: u32) {
            assert!((1..=273).contains(&len), "an LZMA match is 1..=273 bytes");
            let s = self.state;
            self.rc.bit(&mut self.is_match[s], true);
            self.rc.bit(&mut self.is_rep[s], true);
            self.rc.bit(&mut self.is_rep_g0[s], false);
            if len == 1 {
                self.rc.bit(&mut self.is_rep0_long[s], false);
                self.state = if s < 7 { 9 } else { 11 };
            } else {
                self.rc.bit(&mut self.is_rep0_long[s], true);
                self.rep_len.encode(&mut self.rc, len - 2);
                self.state = if s < 7 { 8 } else { 11 };
            }
        }

        /// The end-of-stream marker: a new match of length 2 at distance `0xFFFF_FFFF` — pos slot
        /// 63, then 26 direct bits and 4 align bits, all ones.
        fn end_marker(&mut self) {
            let s = self.state;
            self.rc.bit(&mut self.is_match[s], true);
            self.rc.bit(&mut self.is_rep[s], false);
            self.match_len.encode(&mut self.rc, 0);
            self.rc.tree(&mut self.pos_slot, 6, 63);
            self.rc.direct(0x03FF_FFFF, 26);
            self.rc.reverse_tree(&mut self.align, 4, 0xF);
        }
    }

    /// `prefix` as literals, then its last byte repeated until the stream decodes to `total` bytes.
    fn run(prefix: &[u8], total: u64, dict: u32, ending: Ending) -> Vec<u8> {
        assert!(!prefix.is_empty() && prefix.len() as u64 <= total);
        let declared = matches!(ending, Ending::Declared).then_some(total);
        let mut e = RunEncoder::new(dict, declared);
        for &b in prefix {
            e.literal(b);
        }
        let mut left = total - prefix.len() as u64;
        while left > 0 {
            let len = left.min(273);
            e.rep0(len as u32);
            left -= len;
        }
        if !matches!(ending, Ending::Declared) {
            e.end_marker();
        }
        let mut out = e.rc.finish();
        if matches!(ending, Ending::CutShort) {
            out.truncate(out.len() - 5);
        }
        out
    }

    #[test]
    fn the_run_encoder_writes_streams_lzma_rs_itself_decodes() {
        let cases: [(&[u8], u64); 7] = [
            (&[7], 1),
            (&[1, 2, 3], 3),
            (&[0], 2),
            (&[9], 275),
            (&[0], 5_000),
            (&[4, 5], 8_193),
            (&[3], 300_000),
        ];
        for (prefix, total) in cases {
            let last = *prefix.last().expect("a prefix");
            let mut want = prefix.to_vec();
            want.resize(total as usize, last);
            for ending in [Ending::Declared, Ending::EndMarker] {
                for dict in [4_096, 4 * MIB as u32, u32::MAX] {
                    let stream = run(prefix, total, dict, ending);
                    let mut got = Vec::new();
                    lzma_rs::lzma_decompress(&mut &stream[..], &mut got).unwrap_or_else(|e| {
                        panic!("{ending:?} {total} bytes, dict {dict}: lzma-rs refused it: {e}")
                    });
                    assert_eq!(got, want, "{ending:?} {total} bytes, dict {dict}");
                }
            }
            let cut = run(prefix, total, 4_096, Ending::CutShort);
            let mut got = Vec::new();
            assert!(
                lzma_rs::lzma_decompress(&mut &cut[..], &mut got).is_err(),
                "a stream cut short must not decode cleanly ({total} bytes)"
            );
        }
    }

    // ── the path grammar (§3.1) ─────────────────────────────────────────────────────────────────

    #[test]
    fn the_month_is_zero_based() {
        let jan_15 = classify_bi5_path(&["2024", "00", "15_ticks.bi5"], now());
        assert_eq!(jan_15, Bi5Path::Daily(day(2024, 1, 15)));
        let Bi5Path::Daily(d) = jan_15 else { unreachable!() };
        assert_eq!(d.start_ms(), 1_705_276_800_000, "2024-01-15T00:00:00Z");
        assert_eq!(d.to_string(), "2024-01-15");
        assert_eq!(
            classify_bi5_path(&["2024", "11", "31_ticks.bi5"], now()),
            Bi5Path::Daily(day(2024, 12, 31)),
            "11 is December"
        );
    }

    #[test]
    fn month_12_is_refused() {
        assert_eq!(classify_bi5_path(&["2024", "12", "01_ticks.bi5"], now()), Bi5Path::Other);
        assert!(!is_bi5_layout_dir(&["2024", "12"], now()));
    }

    #[test]
    fn day_00_day_32_and_30_february_are_refused() {
        for (month0, dom) in [("00", "00"), ("00", "32"), ("01", "30"), ("01", "31"), ("03", "31")]
        {
            let file = format!("{dom}{DAILY_SUFFIX}");
            assert_eq!(
                classify_bi5_path(&["2024", month0, file.as_str()], now()),
                Bi5Path::Other,
                "2024/{month0}/{file}"
            );
            assert!(!is_bi5_layout_dir(&["2024", month0, dom], now()), "2024/{month0}/{dom}");
        }
        // 29 February exists in 2024 and not in 2023
        assert_eq!(
            classify_bi5_path(&["2024", "01", "29_ticks.bi5"], now()),
            Bi5Path::Daily(day(2024, 2, 29))
        );
        assert_eq!(classify_bi5_path(&["2023", "01", "29_ticks.bi5"], now()), Bi5Path::Other);
    }

    #[test]
    fn the_year_runs_from_1970_to_the_current_year() {
        let at = |year: &str| classify_bi5_path(&[year, "00", "02_ticks.bi5"], now());
        assert_eq!(at("1969"), Bi5Path::Other);
        assert_eq!(at("1970"), Bi5Path::Daily(day(1970, 1, 2)));
        assert_eq!(at("2026"), Bi5Path::Daily(day(2026, 1, 2)), "the current year");
        assert_eq!(at("2027"), Bi5Path::Other, "a year after now");
        assert_eq!(at("24"), Bi5Path::Other, "four digits, exactly");
        assert_eq!(at("02024"), Bi5Path::Other);
        assert!(is_bi5_layout_dir(&["1970"], now()));
        assert!(!is_bi5_layout_dir(&["1969"], now()));
        assert!(!is_bi5_layout_dir(&["2027"], now()));
    }

    #[test]
    fn the_hourly_layout_is_recognised_not_imported() {
        assert_eq!(
            classify_bi5_path(&["2024", "00", "15", "10h_ticks.bi5"], now()),
            Bi5Path::Hourly { day: day(2024, 1, 15), hour: 10 }
        );
        assert_eq!(
            classify_bi5_path(&["2024", "00", "15", "00h_ticks.bi5"], now()),
            Bi5Path::Hourly { day: day(2024, 1, 15), hour: 0 }
        );
        for file in ["24h_ticks.bi5", "10_ticks.bi5", "1h_ticks.bi5", "10H_ticks.bi5"] {
            assert_eq!(
                classify_bi5_path(&["2024", "00", "15", file], now()),
                Bi5Path::Other,
                "{file}"
            );
        }
        // the hourly layout under an impossible day is not a layout at all
        assert_eq!(
            classify_bi5_path(&["2024", "01", "30", "10h_ticks.bi5"], now()),
            Bi5Path::Other
        );
        assert!(is_bi5_layout_dir(&["2024", "00", "15"], now()));
    }

    #[test]
    fn candle_and_other_names_are_other() {
        let others: [&[&str]; 13] = [
            &["2024", "00", "15", "BID_candles_min_1.bi5"],
            &["2024", "00", "15", "ASK_candles_min_1.bi5"],
            &["2024", "00", "BID_candles_hour_1.bi5"],
            &["2024", "BID_candles_day_1.bi5"],
            &["2024", "00", "15_ticks.bi5.part"],
            &["2024", "00", "15_TICKS.BI5"],
            &["2024", "00", "5_ticks.bi5"],
            &["2024", "0", "15_ticks.bi5"],
            &["2024", "00", "15_ticks.bi5", "x"],
            &["2024", "00", "15", "10h_ticks.bi5", "x"],
            &["README.md"],
            &["15_ticks.bi5"],
            &[],
        ];
        for rel in others {
            assert_eq!(classify_bi5_path(rel, now()), Bi5Path::Other, "{rel:?}");
        }
    }

    #[test]
    fn a_walk_descends_only_into_layout_directories() {
        assert!(is_bi5_layout_dir(&["2024"], now()));
        assert!(is_bi5_layout_dir(&["2024", "00"], now()));
        assert!(is_bi5_layout_dir(&["2024", "11"], now()));
        assert!(is_bi5_layout_dir(&["2024", "00", "15"], now()));
        for rel in [
            &[][..],
            &["candles"][..],
            &["2024", "Jan"][..],
            &["2024", "00", "15", "10"][..],
            &["2024", "00", "15_ticks.bi5"][..],
        ] {
            assert!(!is_bi5_layout_dir(rel, now()), "{rel:?}");
        }
    }

    #[test]
    fn a_day_is_a_utc_midnight_on_or_after_the_epoch() {
        assert_eq!(Bi5Day::from_start_ms(0).map(|d| d.to_string()), Some("1970-01-01".to_string()));
        assert_eq!(Bi5Day::from_start_ms(DAY_MS - 1), None);
        assert_eq!(Bi5Day::from_start_ms(DAY_MS + 1), None);
        assert_eq!(Bi5Day::from_start_ms(-DAY_MS), None);
        assert_eq!(Bi5Day::from_start_ms(1_705_276_800_000), Some(day(2024, 1, 15)));
    }

    // ── the daily decoder (§3.2, §3.3) ──────────────────────────────────────────────────────────

    #[test]
    fn a_price_at_or_above_2_31_decodes_positive() {
        let raw = record(43_200_000, 0x8000_0000, u32::MAX, 1.0, 1.0);
        let ticks = decode_daily_records(&raw, day(2024, 1, 15), eurusd()).expect("one tick");
        assert_eq!(ticks[0].ask, 2_147_483_648.0 / 1e5);
        assert_eq!(ticks[0].bid, 4_294_967_295.0 / 1e5);
        assert!(ticks[0].ask > 0.0 && ticks[0].bid > 0.0);
        // ...where the hourly decoder, reading SIGNED (the design's F3), turns both negative
        let hourly = decode_ticks(&raw, 0, 1e5);
        assert!(hourly[0].ask < 0.0 && hourly[0].bid < 0.0);
    }

    #[test]
    fn a_payload_not_a_multiple_of_20_is_refused() {
        let mut raw = payload(&[43_200_000, 43_200_001]);
        raw.push(0);
        let refusal = Bi5Refusal::PartialRecord { len: 41 };
        assert_eq!(decode_daily_records(&raw, day(2024, 1, 15), eurusd()), Err(refusal.clone()));
        assert_eq!(
            decode_daily_file(&lzma(&raw), day(2024, 1, 15), eurusd(), &Bi5Limits::DEFAULT),
            Err(refusal.clone()),
            "an unknown-size stream is refused once decoded"
        );
        assert_eq!(
            read_bi5_header(&lzma_declared(&raw), &Bi5Limits::DEFAULT),
            Err(refusal),
            "a declared size is refused from the header, before decoding"
        );
        // the whole records alone decode
        raw.pop();
        assert_eq!(decode_daily_records(&raw, day(2024, 1, 15), eurusd()).map(|t| t.len()), Ok(2));
    }

    #[test]
    fn an_offset_of_a_full_day_is_refused_as_out_of_range() {
        let d = day(2024, 1, 15);
        assert_eq!(
            decode_daily_records(&payload(&[43_200_000, 86_400_000]), d, eurusd()),
            Err(Bi5Refusal::OutOfRange { record: 1 })
        );
        assert_eq!(
            decode_daily_records(&payload(&[u32::MAX]), d, eurusd()),
            Err(Bi5Refusal::OutOfRange { record: 0 })
        );
        // the last millisecond of the day is inside it
        let ticks = decode_daily_records(&payload(&[43_200_000, 86_399_999]), d, eurusd())
            .expect("the bound is inside the day");
        assert_eq!(ticks[1].ts, d.start_ms() + 86_399_999);
    }

    /// The design's kill-proof fixture: two hour-relative payloads filed as one day, the second
    /// restarting at 0.
    #[test]
    fn an_hour_concatenation_is_refused_as_not_monotonic() {
        let concatenated = payload(&[1_000, 1_800_000, 3_500_000, 500, 2_000_000]);
        assert_eq!(
            decode_daily_records(&concatenated, day(2024, 1, 15), eurusd()),
            Err(Bi5Refusal::NotMonotonic { record: 3 })
        );
    }

    /// A decrease AFTER ticks past the first hour: no other check sees this file, so without the
    /// monotonicity check it would decode — with a tick at 00:00:01 stored after one at 10:08.
    #[test]
    fn a_restart_after_day_relative_ticks_is_refused_as_not_monotonic() {
        let restarted = payload(&[36_000_000, 36_500_000, 1_000, 2_000]);
        assert_eq!(
            decode_daily_records(&restarted, day(2024, 1, 15), eurusd()),
            Err(Bi5Refusal::NotMonotonic { record: 2 })
        );
    }

    #[test]
    fn an_all_first_hour_file_is_refused_as_ambiguous() {
        let d = day(2024, 1, 15);
        assert_eq!(
            decode_daily_records(&payload(&[0, 1_000, 3_599_999]), d, eurusd()),
            Err(Bi5Refusal::AmbiguousTimeBase)
        );
        // one tick at the first hour's end places the file
        assert_eq!(
            decode_daily_records(&payload(&[0, 1_000, 3_600_000]), d, eurusd()).map(|t| t.len()),
            Ok(3)
        );
    }

    #[test]
    fn equal_offsets_are_accepted() {
        let d = day(2024, 1, 15);
        let ticks =
            decode_daily_records(&payload(&[43_200_000, 43_200_000, 43_200_001]), d, eurusd())
                .expect("several ticks may share a millisecond");
        let ts: Vec<i64> = ticks.iter().map(|t| t.ts - d.start_ms()).collect();
        assert_eq!(ts, [43_200_000, 43_200_000, 43_200_001]);
    }

    /// An empty payload decodes to NO ticks — never a refusal — which is what tells the store half
    /// to spend no key on it.
    #[test]
    fn an_empty_payload_spends_nothing() {
        let d = day(2024, 1, 15);
        for blob in [Vec::new(), lzma(&[]), lzma_declared(&[])] {
            assert_eq!(
                decode_daily_file(&blob, d, eurusd(), &Bi5Limits::DEFAULT),
                Ok(Vec::new()),
                "{} bytes",
                blob.len()
            );
        }
        let empty_file = read_bi5_header(&[], &Bi5Limits::DEFAULT).expect("an empty payload");
        assert_eq!(empty_file.declared_ticks(), Some(0));
    }

    /// One record, decoded by BOTH lanes' decoders at the same instant: the same `Tick`, bit for
    /// bit, and so the same stored quote.
    #[test]
    fn a_daily_tick_is_bit_identical_to_the_hourly_decoders() {
        let d = day(2024, 1, 15);
        let hour = 10u32;
        let in_hour = 1_500u32;
        let at = |offset: u32| record(offset, 109_469, 109_465, 4.05, 3.6);
        let daily =
            decode_daily_records(&at(hour * 3_600_000 + in_hour), d, eurusd()).expect("daily");
        let hourly = decode_ticks(
            &at(in_hour),
            d.start_ms() + i64::from(hour) * HOUR_MS,
            point_divisor("EURUSD"),
        );
        assert_eq!(daily.len(), 1);
        let (a, b) = (daily[0], hourly[0]);
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.ask.to_bits(), b.ask.to_bits());
        assert_eq!(a.bid.to_bits(), b.bid.to_bits());
        assert_eq!(a.ask_vol.to_bits(), b.ask_vol.to_bits());
        assert_eq!(a.bid_vol.to_bits(), b.bid_vol.to_bits());
        assert_eq!(tick_to_quote(&a, "EURUSD"), tick_to_quote(&b, "EURUSD"));
    }

    #[test]
    fn a_good_day_decodes_end_to_end_under_either_header() {
        let d = day(2024, 1, 15);
        let raw: Vec<u8> = [
            record(36_000_229, 109_469, 109_465, 4.05, 3.6),
            record(36_000_400, 109_470, 109_466, 1.0, 2.5),
        ]
        .concat();
        for blob in [lzma(&raw), lzma_declared(&raw)] {
            let ticks =
                decode_daily_file(&blob, d, eurusd(), &Bi5Limits::DEFAULT).expect("a good day");
            assert_eq!(ticks.len(), 2);
            assert_eq!(ticks[0].ts, d.start_ms() + 36_000_229);
            assert_eq!(ticks[0].ask, 109_469.0 / 1e5);
            assert_eq!(ticks[0].bid, 109_465.0 / 1e5);
            // volumes pass through raw — the record's f32, widened — in millions of base units
            assert_eq!(ticks[0].ask_vol.to_bits(), f64::from(4.05f32).to_bits());
            assert_eq!(ticks[0].bid_vol.to_bits(), f64::from(3.6f32).to_bits());
            assert_eq!(ticks[1].ts, d.start_ms() + 36_000_400);
        }
        assert_eq!(
            read_bi5_header(&lzma_declared(&raw), &Bi5Limits::DEFAULT).map(|h| h.declared_ticks()),
            Ok(Some(2))
        );
        assert_eq!(
            read_bi5_header(&lzma(&raw), &Bi5Limits::DEFAULT).map(|h| h.declared_ticks()),
            Ok(None)
        );
        // a JPY pair's row scales by 1,000
        let usdjpy = bi5_price_scale("USDJPY").expect("USDJPY is admitted");
        let jpy = decode_daily_file(
            &lzma(&record(36_000_000, 148_123, 148_120, 1.0, 1.0)),
            d,
            usdjpy,
            &Bi5Limits::DEFAULT,
        )
        .expect("a JPY day");
        assert_eq!(jpy[0].ask, 148_123.0 / 1e3);
    }

    // ── the bounds (§5) ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_default_limits_are_the_designs() {
        assert_eq!(Bi5Limits::DEFAULT.max_compressed_bytes, 16 * 1024 * 1024);
        assert_eq!(Bi5Limits::DEFAULT.max_decoded_bytes, 64 * 1024 * 1024);
        assert_eq!(Bi5Limits::DEFAULT.max_decoded_bytes / REC_LEN, 3_355_443, "whole ticks");
    }

    /// At the DEFAULT: a file of exactly 16 MiB passes the compressed gate (and is then refused by
    /// its header, for another reason), one byte more does not.
    #[test]
    fn the_compressed_cap_admits_its_bound_and_refuses_one_byte_past_it() {
        let cap = Bi5Limits::DEFAULT.max_compressed_bytes;
        let over_declared = 64 * MIB as u64 + 20;
        let mut blob = header_then(over_declared, &[]);
        blob.resize(cap, 0);
        assert_eq!(
            inflate(&blob, Bi5Limits::DEFAULT),
            Err(Bi5Refusal::DeclaredTooLarge { declared: over_declared, cap: 64 * MIB })
        );
        blob.push(0);
        assert_eq!(
            inflate(&blob, Bi5Limits::DEFAULT),
            Err(Bi5Refusal::CompressedTooLarge { len: cap + 1, cap })
        );
        // and a real stream, at a cap of exactly its own length and one byte under it
        let stream = lzma(&payload(&[43_200_000]));
        assert_eq!(inflate(&stream, limits(stream.len(), 1_000)).map(|r| r.len()), Ok(20));
        assert_eq!(
            inflate(&stream, limits(stream.len() - 1, 1_000)),
            Err(Bi5Refusal::CompressedTooLarge { len: stream.len(), cap: stream.len() - 1 })
        );
    }

    /// The header's size is checked BEFORE decoding: the body behind the over-cap header is not a
    /// stream at all, and decoding it would have refused it as something else.
    #[test]
    fn a_declared_size_over_the_cap_is_refused_before_decoding() {
        let garbage = [0xA5u8; 64];
        assert_eq!(
            inflate(&header_then(1_020, &garbage), limits(MIB, 1_000)),
            Err(Bi5Refusal::DeclaredTooLarge { declared: 1_020, cap: 1_000 })
        );
        // at the bound, a real stream declaring exactly the cap decodes
        let zeros = vec![0u8; 1_000];
        assert_eq!(inflate(&lzma_declared(&zeros), limits(MIB, 1_000)), Ok(zeros));
        // and at the DEFAULT, the header alone: the largest whole-record size under 64 MiB is
        // admitted, the next whole-record size is not
        let largest = 3_355_443 * 20;
        assert_eq!(
            read_bi5_header(&header_then(largest, &[]), &Bi5Limits::DEFAULT),
            Ok(Bi5Header { declared_bytes: Some(largest) })
        );
        assert_eq!(
            read_bi5_header(&header_then(largest + 20, &[]), &Bi5Limits::DEFAULT),
            Err(Bi5Refusal::DeclaredTooLarge { declared: largest + 20, cap: 64 * MIB })
        );
    }

    /// `lzma-rs`'s encoder writes an 8 MiB dictionary, so under a small cap the DICTIONARY window
    /// (`memlimit`) is what refuses the byte past it.
    #[test]
    fn the_decoded_cap_holds_at_its_bound_through_the_dictionary_window() {
        let cap = 1_000;
        let exact = vec![7u8; cap];
        assert_eq!(inflate(&lzma(&exact), limits(MIB, cap)), Ok(exact));
        assert_eq!(
            inflate(&lzma(&[7u8; 1_001]), limits(MIB, cap)),
            Err(Bi5Refusal::DecodedTooLarge { cap })
        );
    }

    /// A 4 KiB dictionary flushes to the sink every 4 KiB, so under a larger cap the SINK is what
    /// refuses the byte past it.
    #[test]
    fn the_decoded_cap_holds_at_its_bound_through_the_sink() {
        let cap = 8_192;
        assert_eq!(
            inflate(&run(&[0], 8_192, 4_096, Ending::EndMarker), limits(MIB, cap)).map(|r| r.len()),
            Ok(cap)
        );
        assert_eq!(
            inflate(&run(&[0], 8_193, 4_096, Ending::EndMarker), limits(MIB, cap)),
            Err(Bi5Refusal::DecodedTooLarge { cap })
        );
    }

    /// At the DEFAULT: an unknown-size stream of exactly 64 MiB is admitted.
    #[test]
    fn the_default_decoded_cap_admits_exactly_64_mib() {
        let cap = Bi5Limits::DEFAULT.max_decoded_bytes;
        let stream = run(&[0], cap as u64, 4 * MIB as u32, Ending::EndMarker);
        assert_eq!(inflate(&stream, Bi5Limits::DEFAULT).map(|r| r.len()), Ok(cap));
    }

    /// 70 MiB of zeros in a few KB, its size not declared, under the measured 4 MiB dictionary. The
    /// stream is CUT SHORT at its end, so a decoder that read past the cap would reach the cut and
    /// say `Truncated` — `DecodedTooLarge` proves it stopped within one window of the cap.
    #[test]
    fn a_bomb_is_refused_at_the_cap() {
        let bomb = run(&[0], 70 * MIB as u64, 4 * MIB as u32, Ending::CutShort);
        assert!(bomb.len() < 64 * 1024, "a bomb is a few KB, this one is {} bytes", bomb.len());
        assert_eq!(
            inflate(&bomb, Bi5Limits::DEFAULT),
            Err(Bi5Refusal::DecodedTooLarge { cap: Bi5Limits::DEFAULT.max_decoded_bytes })
        );
    }

    /// The same bomb with a header claiming a 4 GiB dictionary: the window is held to the cap
    /// (`memlimit`), so it never grows past it, and the refusal is again `DecodedTooLarge` rather
    /// than the cut's `Truncated`.
    #[test]
    fn a_bomb_claiming_a_4_gib_dictionary_is_refused_at_the_cap() {
        let bomb = run(&[0], 70 * MIB as u64, u32::MAX, Ending::CutShort);
        assert_eq!(
            inflate(&bomb, Bi5Limits::DEFAULT),
            Err(Bi5Refusal::DecodedTooLarge { cap: Bi5Limits::DEFAULT.max_decoded_bytes })
        );
    }

    #[test]
    fn the_capped_sink_never_allocates_past_its_cap() {
        let mut sink = CappedSink::new(100, None);
        sink.write_all(&[1; 60]).expect("under the cap");
        sink.write_all(&[2; 40]).expect("exactly the cap");
        assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());
        assert!(sink.write_all(&[3]).is_err(), "one byte past the cap");
        assert!(sink.refused);
        assert_eq!(sink.buf.len(), 100);
        assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());

        // a write that would cross the cap is refused WHOLE — nothing of it is kept
        let mut sink = CappedSink::new(100, None);
        sink.write_all(&[1; 60]).expect("under the cap");
        assert!(sink.write_all(&[2; 41]).is_err());
        assert_eq!(sink.buf.len(), 60);
        assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());

        // a declared size sizes it up front, never past the cap
        assert_eq!(CappedSink::new(100, Some(80)).buf.capacity(), 80);
        assert!(CappedSink::new(100, Some(u64::MAX)).buf.capacity() <= 100);
    }

    #[test]
    fn a_truncated_stream_is_refused() {
        let raw = payload(&[36_000_000, 36_000_001, 36_000_002]);
        for stream in [lzma(&raw), lzma_declared(&raw)] {
            let cut = &stream[..stream.len() - 6];
            assert_eq!(inflate(cut, Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
            assert_eq!(
                decode_daily_file(cut, day(2024, 1, 15), eurusd(), &Bi5Limits::DEFAULT),
                Err(Bi5Refusal::Truncated)
            );
        }
        // inside the header itself
        let stream = lzma(&raw);
        assert_eq!(inflate(&stream[..10], Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
        assert_eq!(read_bi5_header(&stream[..12], &Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
    }

    #[test]
    fn an_impossible_properties_byte_is_refused_from_the_header() {
        let mut stream = lzma(&payload(&[36_000_000]));
        stream[0] = PROPS_LIMIT;
        let refused = read_bi5_header(&stream, &Bi5Limits::DEFAULT).err();
        assert_eq!(refused.as_ref().map(Bi5Refusal::class), Some("Corrupt"));
        assert_eq!(inflate(&stream, Bi5Limits::DEFAULT).err(), refused);
    }

    /// The classes are what a report carries; a renamed class is a changed report.
    #[test]
    fn every_refusal_names_its_class() {
        let all = [
            Bi5Refusal::CompressedTooLarge { len: 2, cap: 1 },
            Bi5Refusal::DeclaredTooLarge { declared: 2, cap: 1 },
            Bi5Refusal::DecodedTooLarge { cap: 1 },
            Bi5Refusal::Truncated,
            Bi5Refusal::Corrupt { reason: "r".to_string() },
            Bi5Refusal::PartialRecord { len: 21 },
            Bi5Refusal::OutOfRange { record: 0 },
            Bi5Refusal::NotMonotonic { record: 3 },
            Bi5Refusal::AmbiguousTimeBase,
        ];
        let classes: Vec<&str> = all.iter().map(Bi5Refusal::class).collect();
        assert_eq!(
            classes,
            [
                "CompressedTooLarge",
                "DeclaredTooLarge",
                "DecodedTooLarge",
                "Truncated",
                "Corrupt",
                "PartialRecord",
                "OutOfRange",
                "NotMonotonic",
                "AmbiguousTimeBase",
            ]
        );
        for refusal in &all {
            assert!(refusal.to_string().starts_with(refusal.class()), "{refusal}");
        }
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
            let at =
                |point_value: f64| (f64::from(ask) / point_value, f64::from(bid) / point_value);
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
}
