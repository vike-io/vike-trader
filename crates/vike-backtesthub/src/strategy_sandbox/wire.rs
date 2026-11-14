//! The frames on a sandboxed backtest's protocol channel: the node wire's big-endian `u32` length, then a
//! MSGPACK body ([`write_message`], [`read_message`]), at most `vike_hub_protocol::frame::MAX_FRAME_LEN`.
//! The live verb's wire is the same (`vike_tradehub_core::remote_strategy`'s, plan 2b F5), so the one
//! child binary speaks one framing. Msgpack, not JSON, because JSON writes a NaN as `null` and cannot read
//! it back: a stored row's non-finite price must reach the run as the in-process read gave it.
//!
//! In order: the parent writes ONE [`ChildRequest`] frame, the request as it arrived. The child then writes
//! [`ChildFrame`]s: each [`ChildFrame::Read`] is answered by the parent with [`ReadAnswerFrame::Rows`] parts
//! and ONE [`ReadAnswerFrame::Done`] (or one [`ReadAnswerFrame::Failed`]): rows in parts of at most
//! [`ROWS_PER_FRAME`], BARS as [`BarColumns`] of at most [`BARS_PER_FRAME`], a single value as one part.
//! The last child frame is ONE [`ChildFrame::Answer`]: the `Response` the in-process daemon would have
//! written. Only the READ verbs below exist on this wire, so a child can neither write nor delete. These
//! answer frames are the backtest data relay's own; the live verb's frames are F5's.

use std::borrow::Cow;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use vike_backtesthub_client::proto::Response;
use vike_data::{HistoryStoreError, SeriesId, TimestampRange};
use vike_marketdata::Bar;

/// Write `msg` as ONE frame: the length prefix and the msgpack body in a single write (the reason is
/// `vike_hub_protocol::frame::write_frame`'s). The frame is built in memory first, so a body over the cap is
/// refused ([`io::ErrorKind::InvalidData`]) with NOTHING written.
///
/// ⚠ A second spelling of `vike_tradehub_core::remote_strategy`'s `write_message` (F5), byte for byte: this
/// crate cannot name that one. One home below both is the integrator's call.
pub(crate) fn write_message<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let mut frame = Vec::new();
    frame_into(&mut frame, msg)?;
    w.write_all(&frame)?;
    w.flush()
}

/// `msg` as one whole frame (length prefix and msgpack body) in `frame`, emptied first so a caller writing
/// many frames reuses one buffer; refused over the cap. What [`write_message`] writes.
pub(crate) fn frame_into(frame: &mut Vec<u8>, msg: &impl Serialize) -> io::Result<()> {
    frame.clear();
    frame.extend_from_slice(&[0; 4]);
    rmp_serde::encode::write_named(frame, msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(frame.len() - 4)
        .ok()
        .filter(|l| *l <= vike_hub_protocol::frame::MAX_FRAME_LEN)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame over the cap"))?;
    frame[..4].copy_from_slice(&len.to_be_bytes());
    Ok(())
}

/// Read ONE frame and decode it: an end of stream is [`io::ErrorKind::UnexpectedEof`], a body that is not
/// a message (or a length over the cap) [`io::ErrorKind::InvalidData`].
pub(crate) fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = vike_hub_protocol::frame::read_frame_raw(r)?;
    rmp_serde::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Rows per [`ReadAnswerFrame::Rows`] part, `vike_datahub_client::HistoryStoreDatahub`'s page size: a ROW
/// count cannot bound bytes, so the parent halves a part that would exceed the frame cap.
pub(crate) const ROWS_PER_FRAME: usize = 10_000;

/// Bars per [`BarColumns`] part: about 1.2 MB of columns, small enough that the parent's one reused part
/// and frame buffer stay in cache.
pub(crate) const BARS_PER_FRAME: usize = 16_384;

/// One column's bytes as ONE msgpack `bin`: a column crosses as one copy, not one msgpack value per bar
/// (MEASURED on 1.05M bars: a column of msgpack floats encoded SLOWER than the named rows it replaced).
/// Read, it BORROWS the frame body it came from (no copy).
#[derive(Debug, Default)]
struct Bin<'a>(Cow<'a, [u8]>);

impl Bin<'_> {
    /// Append `bits` as eight little-endian bytes.
    fn put(&mut self, bits: u64) {
        self.0.to_mut().extend_from_slice(&bits.to_le_bytes());
    }

    /// The `i`th eight-byte word (the caller checked the length).
    fn word(&self, i: usize) -> u64 {
        let mut word = [0u8; 8];
        word.copy_from_slice(&self.0[i * 8..i * 8 + 8]);
        u64::from_le_bytes(word)
    }
}

impl Serialize for Bin<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Bin<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> serde::de::Visitor<'de> for Bytes {
            type Value = Bin<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a column of bytes")
            }
            fn visit_borrowed_bytes<E: serde::de::Error>(
                self,
                bytes: &'de [u8],
            ) -> Result<Bin<'de>, E> {
                Ok(Bin(Cow::Borrowed(bytes)))
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Bin<'de>, E> {
                Ok(Bin(Cow::Owned(bytes.to_vec())))
            }
        }
        d.deserialize_bytes(Bytes)
    }
}

/// A part of a BAR answer as COLUMNS, the backtest data relay's bulk frame (a run reads bars by the
/// million): each field one `bin` of little-endian words (an absent optional as a zero word, with its
/// presence in `present`: bit 0 funding, bit 1 bid, bit 2 ask), and the symbol as runs (`(count,
/// symbol)`), so a series of one symbol crosses one string. Every field keeps its bits ([`Self::fill`] and
/// [`Self::push_into`] are each other's inverse, NaN and infinity included).
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct BarColumns<'a> {
    rows: usize,
    #[serde(borrow)]
    timestamp: Bin<'a>,
    #[serde(borrow)]
    open: Bin<'a>,
    #[serde(borrow)]
    high: Bin<'a>,
    #[serde(borrow)]
    low: Bin<'a>,
    #[serde(borrow)]
    close: Bin<'a>,
    #[serde(borrow)]
    volume: Bin<'a>,
    #[serde(borrow)]
    funding: Bin<'a>,
    #[serde(borrow)]
    bid: Bin<'a>,
    #[serde(borrow)]
    ask: Bin<'a>,
    #[serde(borrow)]
    present: Bin<'a>,
    symbol: Vec<(usize, Option<String>)>,
}

impl BarColumns<'_> {
    /// These columns emptied and refilled with `rows`, keeping their buffers: the parent encodes every
    /// part of an answer through one `BarColumns`.
    pub(crate) fn fill(&mut self, rows: &[Bar]) {
        let c = self;
        c.rows = rows.len();
        let words = [
            &mut c.timestamp,
            &mut c.open,
            &mut c.high,
            &mut c.low,
            &mut c.close,
            &mut c.volume,
            &mut c.funding,
            &mut c.bid,
            &mut c.ask,
            &mut c.present,
        ];
        for column in words {
            column.0.to_mut().clear();
        }
        c.symbol.clear();
        for b in rows {
            c.timestamp.put(b.timestamp as u64);
            c.open.put(b.open.to_bits());
            c.high.put(b.high.to_bits());
            c.low.put(b.low.to_bits());
            c.close.put(b.close.to_bits());
            c.volume.put(b.volume.to_bits());
            c.funding.put(b.funding.map_or(0, f64::to_bits));
            c.bid.put(b.bid.map_or(0, f64::to_bits));
            c.ask.put(b.ask.map_or(0, f64::to_bits));
            let flags = u8::from(b.funding.is_some())
                | (u8::from(b.bid.is_some()) << 1)
                | (u8::from(b.ask.is_some()) << 2);
            c.present.0.to_mut().push(flags);
            match c.symbol.last_mut() {
                Some((count, symbol)) if *symbol == b.symbol => *count += 1,
                _ => c.symbol.push((1, b.symbol.clone())),
            }
        }
    }

    /// The rows back, appended to `out`; a part whose columns disagree in length is refused, so a
    /// truncated or garbled part never reads as fewer bars.
    pub(crate) fn push_into(self, out: &mut Vec<Bar>) -> Result<(), HistoryStoreError> {
        let n = self.rows;
        let runs: usize = self.symbol.iter().map(|(count, _)| count).sum();
        let words = [
            &self.timestamp,
            &self.open,
            &self.high,
            &self.low,
            &self.close,
            &self.volume,
            &self.funding,
            &self.bid,
            &self.ask,
        ];
        if words.iter().any(|c| c.0.len() != n * 8) || self.present.0.len() != n || runs != n {
            return Err(HistoryStoreError::Io(
                "the strategy sandbox read a bar part whose columns disagree".into(),
            ));
        }
        let optional = |column: &Bin<'_>, i: usize, bit: u8| {
            ((self.present.0[i] & bit) != 0).then(|| f64::from_bits(column.word(i)))
        };
        out.reserve(n);
        let mut symbols = self.symbol.iter().flat_map(|(count, s)| std::iter::repeat_n(s, *count));
        for i in 0..n {
            out.push(Bar {
                timestamp: self.timestamp.word(i) as i64,
                open: f64::from_bits(self.open.word(i)),
                high: f64::from_bits(self.high.word(i)),
                low: f64::from_bits(self.low.word(i)),
                close: f64::from_bits(self.close.word(i)),
                volume: f64::from_bits(self.volume.word(i)),
                funding: optional(&self.funding, i, 1),
                bid: optional(&self.bid, i, 2),
                ask: optional(&self.ask, i, 4),
                symbol: symbols.next().cloned().flatten(),
            });
        }
        Ok(())
    }
}

/// The PARENT's first frame: the request as it arrived, the sweep cap this daemon runs under
/// (`vike_backtest::harness::paramscan::threads::paramscan_threads`: the `preferences.sweep_threads` row the daemon
/// installed, which the child cannot read), so a sweep in the child spends what it would here, and the
/// strategy file's TEXT as the hub read it (the child reads no folder), and the datahub's KEPT instrument rows the
/// run reads (`super::kept_rows_of`: its parent store's answer, narrowed to the run's), which the child's piped
/// store answers back (`super::pipe_store::PipeHistStore`). `R`/`T` are `&Request`/`&str` when written and
/// `Request`/`String` when read.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChildRequest<R, T> {
    pub(crate) request: R,
    pub(crate) paramscan_threads: usize,
    pub(crate) text: T,
    pub(crate) kept: Result<Option<Vec<vike_model::Instrument>>, WireDataError>,
}

/// A frame the CHILD writes.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChildFrame {
    /// One history read the run makes, answered by [`ReadAnswerFrame`]s.
    Read(StoreRead),
    /// The finished answer, the last frame the child writes.
    Answer(Box<Response>),
}

/// The history reads a backtest makes (`vike_data::HistoryStore`'s read verbs on the run path), each with
/// its arguments. Every other store verb is refused inside the child and never crosses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoreRead {
    LoadBars {
        venue: String,
        symbol: String,
        interval: String,
        range: WireRange,
    },
    LoadBarsSourcedCapped {
        venue: String,
        source: String,
        symbol: String,
        interval: String,
        range: WireRange,
        budget: Option<usize>,
    },
    ScanQuotes {
        venue: String,
        symbol: String,
        range: WireRange,
    },
    ScanTrades {
        venue: String,
        symbol: String,
        range: WireRange,
    },
    ScanBookUpdates {
        venue: String,
        symbol: String,
        range: WireRange,
    },
    ScanDepth {
        venue: String,
        symbol: String,
        range: WireRange,
    },
    ScanCohort {
        venue: String,
        asset: String,
        range: WireRange,
    },
    ScanPerpMetrics {
        venue: String,
        symbol: String,
        range: WireRange,
    },
    PropertiesAsOf {
        venue: String,
        symbol: String,
        timestamp: i64,
    },
    ListSeries,
    Inventory,
    SeriesGaps(SeriesId),
    SeriesFacts(SeriesId),
}

/// A `vike_data::TimestampRange` as it crosses: its two optional bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct WireRange {
    pub(crate) start: Option<i64>,
    pub(crate) end: Option<i64>,
}

impl From<TimestampRange> for WireRange {
    fn from(r: TimestampRange) -> Self {
        Self { start: r.start, end: r.end }
    }
}

impl From<WireRange> for TimestampRange {
    fn from(r: WireRange) -> Self {
        Self { start: r.start, end: r.end }
    }
}

/// A frame the PARENT writes in answer to one [`ChildFrame::Read`]. `P` is the payload's type: a slice
/// of rows when written, a `Vec` (or the single value) when read.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ReadAnswerFrame<P> {
    /// A part of a row answer, or the whole of a single-value one.
    Rows(P),
    /// The row answer is complete.
    Done,
    /// The parent's store refused the read.
    Failed(WireDataError),
}

/// A `vike_data::HistoryStoreError` as it crosses, variant for variant, so the child's run sees the error the
/// parent's store gave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum WireDataError {
    Query(String),
    Io(String),
    Unsupported(String),
}

impl From<&HistoryStoreError> for WireDataError {
    fn from(e: &HistoryStoreError) -> Self {
        match e {
            HistoryStoreError::Query(m) => Self::Query(m.clone()),
            HistoryStoreError::Io(m) => Self::Io(m.clone()),
            HistoryStoreError::Unsupported(m) => Self::Unsupported(m.clone()),
        }
    }
}

impl From<WireDataError> for HistoryStoreError {
    fn from(e: WireDataError) -> Self {
        match e {
            WireDataError::Query(m) => Self::Query(m),
            WireDataError::Io(m) => Self::Io(m),
            WireDataError::Unsupported(m) => Self::Unsupported(m),
        }
    }
}
