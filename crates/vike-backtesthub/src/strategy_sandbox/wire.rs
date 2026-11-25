//! The frames on a sandboxed backtest's protocol channel: the CHILD PIPE's codec (`vike_hub_protocol::child_pipe`,
//! which the live verb's pipe speaks too, so the one child binary speaks one framing): the node wire's
//! big-endian `u32` length, then a body of at most `vike_hub_protocol::frame::MAX_FRAME_LEN`. Every body is
//! MSGPACK but a BAR part's, which is the datahub's own binary bar-columns body
//! (`vike_datahub_client::proto::BAR_COLUMNS`, the layout `HistoryStoreDatahub` reads the bars in: decision
//! 0281's amendment of 2026-11-18, the owner's ruling that bulk numbers do not cross as re-encoded msgpack), and a
//! TICK part's (quotes, trades, book updates, depth), which is the same codec's columns body of that read
//! (under the read's own magic, [`TICK_COLUMNS`]). A part is SPLICED from bodies already encoded
//! ([`columns_part_into`]: the daemon's cache keeps its days so, and any other answer is encoded once).
//! Msgpack, not JSON, because JSON writes a NaN as `null` and cannot read it back: a stored row's non-finite price
//! must reach the run as the in-process read gave it (a bar column keeps every bit of a price; an OPTIONAL that is
//! not finite crosses absent, the bar-columns rule, which is how the datahub hands it to this process in the first
//! place).
//!
//! **The bodies are POSITIONAL** (`child_pipe::write_message`: a struct is an array of its fields), behind the one
//! same-build handshake: each side's FIRST frame is its build (`vike_buildinfo::summary`, which the binary hands in:
//! `strategy_sandbox::use_build` on the hub's side, `serve_child`'s `identity` on the child's), and each refuses another
//! build in words before it reads a message (`child_pipe::check_build`; the hub and its child are one executable,
//! so they differ only when a deploy replaced it under a running hub). ⚠ **The frames whose payload type leaves a
//! field out before its last one cross NAMED** (`child_pipe::write_message_named`), because positional would read
//! every later field into the wrong slot: [`ChildRequest`] (`Request`'s point override), the single-value answers
//! (`SymbolProperties::tick_scheme`, `Instrument`'s optional tail), [`ChildFrame::HeldPoint`] (`RunSummaryWire::run`),
//! [`ChildFrame::ParamscanRow`] (a summarized search's row, a point's summary), [`ChildFrame::StudioRun`] and
//! [`ChildFrame::Answer`] (a `Response` reaches every report type). They are one frame each a run or a point; the
//! bulk (a run's trades, its curve) is positional.
//! `strategy_sandbox_tests.rs`'s `every_frame_crosses_positional_as_it_crosses_named` holds both lists.
//!
//! In order: each side writes its build, then the parent writes ONE [`ChildRequest`] frame, the request as it
//! arrived. The child then writes [`ChildFrame`]s: each [`ChildFrame::Read`] is answered by the parent with
//! [`ReadAnswerFrame::Rows`] parts and ONE [`ReadAnswerFrame::Done`] (or one [`ReadAnswerFrame::Failed`]): rows in
//! parts of at most [`ROWS_PER_FRAME`], BARS as ONE [`ReadAnswerFrame::Count`] (how many bars follow, so the child
//! sizes its rows once) then bar-columns parts of at most [`BARS_PER_FRAME`], TICKS the same way in tick-columns
//! parts of at most [`TICKS_PER_FRAME`] (a part in place of a `Rows`, told apart by its magic, [`is_columns_part`]),
//! a single value as one part. The child may have SEVERAL reads in flight (it writes them back to back, the parent
//! fetches up to `super::relay`'s `READ_WORKERS` of them at once), and the parent writes the answers in REQUEST
//! order, each whole before the next (one it cannot write whole is ended by a `Failed`), so the
//! frames stay in order: no answer carries a tag (decision 0281's amendment of 2026-11-19, "Several reads in
//! flight"). The last child frame is ONE
//! [`ChildFrame::Answer`]: the `Response` the in-process daemon would have written. A SUMMARIZED run's child
//! writes, before it, each point's [`ChildFrame::HeldTrades`] parts and [`ChildFrame::HeldPoint`] (a single
//! run's trades parts, then its [`ChildFrame::HeldCurve`] parts or, when the store pins its bars, ONE
//! [`ChildFrame::HeldLog`] in their place, and no point), which the parent holds; a summarized PROFILE search
//! writes one [`ChildFrame::ParamscanRow`] a row and then ONE [`ChildFrame::ParamscanTail`]. A Studio
//! single run of the file writes ONE [`ChildFrame::StudioRun`] before its answer, which the parent saves. Only
//! the READ verbs below exist on this wire, so a child can neither write nor delete. These answer frames are the
//! backtest data relay's own; the live verb's frames are F5's.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use vike_backtesthub_client::proto::Response;
use vike_data::{HistoryStoreError, PlannedRead, SeriesId, TimestampRange};
use vike_datahub_client::proto::{
    BAR_COLUMNS_MAGIC, BOOK_UPDATE_COLUMNS, ColumnRow, ColumnsMagic, DEPTH_COLUMNS, QUOTE_COLUMNS,
    TRADE_COLUMNS, decode_bar_columns_into, decode_columns_into, splice_columns_within,
};
use vike_hub_protocol::frame::MAX_FRAME_LEN;
use vike_marketdata::Bar;

/// The frame `vike_hub_protocol::child_pipe::write_message_named` writes for `ReadAnswerFrame::Rows(&kept)` when both halves of the [`KeptAnswer`] are
/// `Ok(Some(..))`, built from the halves' msgpack arrays as the store holds them (`vike_data::EncodedKept`, the datahub
/// handle's stamped cache): the enum wrappers `rmp_serde` writes around them (`{"Rows": [{"Ok": rows}, {"Ok":
/// splits}]}`: a newtype variant is a one-entry map, `Some` is its value), then the arrays copied in. No row is decoded
/// or encoded; the bytes are that writer's (a kept answer crosses NAMED: this module's doc), which `strategy_sandbox_tests.rs` holds equal.
pub(crate) fn write_kept_frame<W: Write>(
    w: &mut W,
    kept: &vike_data::EncodedKept,
) -> io::Result<()> {
    const ROWS: &[u8] = &[0x81, 0xa4, b'R', b'o', b'w', b's', 0x92];
    const OK: &[u8] = &[0x81, 0xa2, b'O', b'k'];
    let (rows, splits) = (kept.rows.as_slice(), kept.splits.as_slice());
    let mut frame = Vec::with_capacity(4 + ROWS.len() + 2 * OK.len() + rows.len() + splits.len());
    frame.extend_from_slice(&[0; 4]);
    for part in [ROWS, OK, rows, OK, splits] {
        frame.extend_from_slice(part);
    }
    seal(&mut frame)?;
    w.write_all(&frame)?;
    w.flush()
}

/// The length prefix written over the four bytes `frame` opens with, or the body refused over the cap.
fn seal(frame: &mut [u8]) -> io::Result<()> {
    let len = u32::try_from(frame.len() - 4)
        .ok()
        .filter(|l| *l <= MAX_FRAME_LEN)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame over the cap"))?;
    frame[..4].copy_from_slice(&len.to_be_bytes());
    Ok(())
}

/// Rows per [`ReadAnswerFrame::Rows`] part, `vike_datahub_client::HistoryStoreDatahub`'s page size: a ROW
/// count cannot bound bytes, so the parent halves a part that would exceed the frame cap.
pub(crate) const ROWS_PER_FRAME: usize = 10_000;

/// Bars per bar-columns part ([`columns_part_into`]): about 1.2 MB of columns, small enough that the parent's one
/// reused frame stays in cache, and far under the frame cap, so no part is ever halved.
pub(crate) const BARS_PER_FRAME: usize = 16_384;

/// Ticks per tick-columns part ([`columns_part_into`]): a quote row is 49 bytes of columns and a trade 33, so a part
/// is under 1 MB, as a bar part is. A book row carries its levels, so a part of deep books may pass the frame cap: the
/// parent halves it before a byte of it is written.
pub(crate) const TICKS_PER_FRAME: usize = 16_384;

/// The rows of `runs` (each the rows `cut` of one plain `magic` body of `R`: an answer's kept or encoded bodies), read
/// as one, as ONE frame of the relay in `frame` (emptied first, so one buffer serves every part): the length prefix,
/// then the datahub's own `magic` columns body SPLICED from the runs
/// (`vike_datahub_client::proto::splice_columns_within`: each column's stretch copied, no row decoded or encoded, byte
/// for byte the body their rows encode to), no compression (a pipe on one box is no thin link). A part over the
/// frame cap is refused ([`io::ErrorKind::InvalidData`]) from its counted length, before a byte of it is built, so the
/// caller halves it and `frame` never grows past the cap; a run that is not such a body fails with any other kind.
pub(crate) fn columns_part_into<R: ColumnRow>(
    frame: &mut Vec<u8>,
    magic: &ColumnsMagic,
    runs: &[(&[u8], std::ops::Range<usize>)],
) -> io::Result<()> {
    frame.clear();
    frame.extend_from_slice(&[0; 4]);
    let room = MAX_FRAME_LEN as usize;
    if !splice_columns_within::<R>(frame, magic, runs, room).map_err(io::Error::other)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame over the cap"));
    }
    seal(frame)
}

/// The frame of a bar part ENCODED from `rows`: the bytes the relay wrote before it spliced kept bodies, which the
/// tests hold [`columns_part_into`] to.
#[cfg(test)]
pub(crate) fn bar_part_into(frame: &mut Vec<u8>, rows: &[Bar]) -> io::Result<()> {
    tick_part_into(frame, &vike_datahub_client::proto::BAR_COLUMNS, rows)
}

/// The tick reads this pipe answers in columns, each by its own reply's magic (`vike_datahub_client::proto`'s table):
/// `ScanQuotes`, `ScanTrades`, `ScanBookUpdates`, `ScanDepth`. `ScanPerpMetrics` is not one: its optional
/// `open_interest` would cross absent where it is not finite (the codec's rule), and msgpack keeps it.
pub(crate) const TICK_COLUMNS: [ColumnsMagic; 4] =
    [QUOTE_COLUMNS, TRADE_COLUMNS, BOOK_UPDATE_COLUMNS, DEPTH_COLUMNS];

/// `rows` as ONE frame of the tick relay in `frame` (emptied first), ENCODED from them
/// (`vike_datahub_client::proto::encode_columns_into`): the bytes the relay wrote before it spliced kept bodies, which
/// the tests hold [`columns_part_into`] to. A part over the frame cap is refused ([`io::ErrorKind::InvalidData`])
/// before it is encoded.
#[cfg(test)]
pub(crate) fn tick_part_into<R: ColumnRow>(
    frame: &mut Vec<u8>,
    magic: &ColumnsMagic,
    rows: &[R],
) -> io::Result<()> {
    if vike_datahub_client::proto::columns_len(rows) > MAX_FRAME_LEN as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame over the cap"));
    }
    frame.clear();
    frame.extend_from_slice(&[0; 4]);
    vike_datahub_client::proto::encode_columns_into(frame, magic, rows);
    seal(frame)
}

/// Whether `body` is a columns part (a bar part, `BAR_COLUMNS_MAGIC`, or a tick part, [`TICK_COLUMNS`]) rather than
/// a msgpack frame: every magic opens with a NUL, the integer 0, which no [`ReadAnswerFrame`] is.
pub(crate) fn is_columns_part(body: &[u8]) -> bool {
    body.starts_with(&BAR_COLUMNS_MAGIC) || TICK_COLUMNS.iter().any(|m| body.starts_with(&m.plain))
}

/// One bar part's frame `body` back as its rows, appended straight to `out`
/// (`vike_datahub_client::proto::decode_bar_columns_into`: no part is built apart and copied after). REFUSED,
/// never read short, unless it is exactly one bar-columns body (the codec checks every length, the presence
/// bits, the symbol runs against the rows and each symbol's UTF-8 before it appends a bar): a msgpack part (a
/// parent of an older build) or a cut or garbled body fails, saying what is wrong, and appends nothing.
pub(crate) fn push_bar_part(body: &[u8], out: &mut Vec<Bar>) -> Result<(), HistoryStoreError> {
    let refused = |why: &str| {
        HistoryStoreError::Io(format!(
            "the strategy sandbox read a bar part it cannot decode: {why}"
        ))
    };
    if !body.starts_with(&BAR_COLUMNS_MAGIC) {
        return Err(refused("it is not a bar-columns body"));
    }
    decode_bar_columns_into(body, out).map(|_| ()).map_err(|e| refused(&e))
}

/// One tick part's frame `body` back as its rows, appended straight to `out`, [`push_bar_part`]'s rule for a
/// `magic` read (`vike_datahub_client::proto::decode_columns_into`): REFUSED, never read short, unless it is exactly
/// one plain `magic` columns body; a body of another magic, a msgpack part, or a cut or garbled one fails, saying
/// what is wrong, and appends nothing.
pub(crate) fn push_tick_part<R: ColumnRow>(
    body: &[u8],
    magic: &ColumnsMagic,
    out: &mut Vec<R>,
) -> Result<(), HistoryStoreError> {
    let refused = |why: &str| {
        HistoryStoreError::Io(format!(
            "the strategy sandbox read a {} part it cannot decode: {why}",
            magic.name
        ))
    };
    if !body.starts_with(&magic.plain) {
        return Err(refused(&format!("it is not a {}-columns body", magic.name)));
    }
    decode_columns_into(body, magic, out).map(|_| ()).map_err(|e| refused(&e))
}

/// The row count a [`ReadAnswerFrame::Count`] `body` states; `None` for any other body (a columns part, a `Rows`).
pub(crate) fn row_count(body: &[u8]) -> Option<usize> {
    if is_columns_part(body) {
        return None;
    }
    match rmp_serde::from_slice::<ReadAnswerFrame<serde::de::IgnoredAny>>(body) {
        Ok(ReadAnswerFrame::Count(count)) => Some(count),
        _ => None,
    }
}

/// The PARENT's first frame: the request as it arrived, the sweep cap this daemon runs under
/// (`vike_backtest::search::paramscan::threads::paramscan_threads`: the `preferences.sweep_threads` row the daemon
/// installed, which the child cannot read), so a sweep in the child spends what it would here, and per `.rs` MOUNT in
/// mount order (one for a `[strategy]` run, a portfolio's each: `super::resolve`), the strategy file's TEXT as the hub
/// read it (the child reads no folder) and the model TEXTS the hub read for the file's `type = "model"` keys, by run
/// id (`super::model_texts_of`; empty for a file with none). The child loaded one artifact per mount, in the same
/// order, and refuses a frame whose mount count is not its artifact count. The datahub's KEPT instrument rows are not
/// in it: the child asks for the venues it reads ([`StoreRead::KeptInstruments`]). `R`/`T`/`M` are
/// `&Request`/`&str`/`&BTreeMap` when written and `Request`/`String`/`BTreeMap` when read.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChildRequest<R, T, M> {
    pub(crate) request: R,
    pub(crate) paramscan_threads: usize,
    pub(crate) mounts: Vec<(T, M)>,
}

/// A frame the CHILD writes.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChildFrame {
    /// One history read the run makes, answered by [`ReadAnswerFrame`]s.
    Read(StoreRead),
    /// A part of a SUMMARIZED run's trades (`crate::run_summary`), at most [`ROWS_PER_FRAME`] of them: the
    /// trades of the point the next [`ChildFrame::HeldPoint`] closes, or, with no point after them, of the
    /// single run the answer summarizes. The parent holds them for paging.
    HeldTrades(Vec<vike_model::Trade>),
    /// One point of a summarized param scan, closing the trades sent since the last point.
    HeldPoint(Box<vike_backtesthub_client::summary::RunSummaryWire>),
    /// A part of a summarized SINGLE run's whole equity curve, at most [`ROWS_PER_FRAME`] steps of it, in step
    /// order (decision 0078's amendment of 2026-11-18): the parent joins them and holds the curve for
    /// `Request::ReadCurve`.
    HeldCurve(vike_backtesthub_client::summary::CurvePageWire),
    /// A summarized SINGLE bar run's event log IN PLACE of its [`ChildFrame::HeldCurve`] parts (decision 0078's
    /// amendment of 2026-11-19 on what the hub holds), sent only when the parent's store pinned every bar the run
    /// marked ([`StoreRead::BarDays`], `super::pinned_log`): the parent holds it, rebuilds the curve from it on
    /// demand, and saves it.
    HeldLog(Box<crate::run_summary::HeldLog>),
    /// The saved run a Studio single run of the file left, ahead of its answer: the parent saves it, no client
    /// reads it.
    StudioRun(Box<crate::backtesthub_server::saved_run::StudioRun>),
    /// One row of a summarized PROFILE search, best first (decision 0078's amendment of 2026-11-19): its summary,
    /// which the parent holds, and its whole JSON, which the parent writes into the saved run; `None` when the row
    /// with its JSON would pass the frame cap (the parent then saves no run).
    ParamscanRow(Box<vike_backtesthub_client::summary::ParamscanRowSummaryWire>, Option<String>),
    /// The summarized search's whole report with no rows, as JSON, after its last row: what closes the saved run's
    /// `report.json`.
    ParamscanTail(String),
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
        range: TimestampRange,
    },
    LoadBarsSourcedCapped {
        venue: String,
        source: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
        budget: Option<usize>,
    },
    ScanQuotes {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanTrades {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanBookUpdates {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanDepth {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanCohort {
        venue: String,
        asset: String,
        range: TimestampRange,
    },
    ScanPerpMetrics {
        venue: String,
        symbol: String,
        range: TimestampRange,
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
    /// The option-chain rows a profile's `[data.options]` legs are priced from.
    ScanChain {
        venue: String,
        underlying: String,
        range: TimestampRange,
    },
    /// The kept instrument rows and share splits of `venues` (`None`: every venue's), answered as ONE value
    /// ([`KeptAnswer`]): `HistoryStore::kept_instruments_and_splits_of`, asked once the run knows the venues it reads.
    KeptInstruments {
        venues: Option<Vec<String>>,
    },
    /// The store's day stamps of a series' bars over `range` (`HistoryStore::series_days` of a bar read, the datahub's `SeriesDays`),
    /// answered as ONE value: the pin check a summarized single run's child makes before it sends its event log in
    /// its curve's place (`super::pinned_log`).
    BarDays {
        venue: String,
        source: Option<String>,
        symbol: String,
        interval: String,
        range: TimestampRange,
    },
}

impl From<&PlannedRead> for StoreRead {
    /// A read named ahead (`vike_data::HistoryStore::prefetch`), as its verb crosses: the same frame, so the same
    /// cache key.
    fn from(read: &PlannedRead) -> Self {
        match read.clone() {
            PlannedRead::Bars { venue, symbol, interval, range } => {
                Self::LoadBars { venue, symbol, interval, range }
            }
            PlannedRead::BarsSourcedCapped { venue, source, symbol, interval, range, budget } => {
                Self::LoadBarsSourcedCapped { venue, source, symbol, interval, range, budget }
            }
            PlannedRead::ListSeries => Self::ListSeries,
            PlannedRead::Quotes { venue, symbol, range } => {
                Self::ScanQuotes { venue, symbol, range }
            }
            PlannedRead::Trades { venue, symbol, range } => {
                Self::ScanTrades { venue, symbol, range }
            }
            PlannedRead::BookUpdates { venue, symbol, range } => {
                Self::ScanBookUpdates { venue, symbol, range }
            }
        }
    }
}

/// A [`StoreRead::KeptInstruments`] answer: the parent store's two answers, each with its own error, as the trait
/// gives them.
pub(crate) type KeptAnswer = (
    Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
    Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
);

/// A frame the PARENT writes in answer to one [`ChildFrame::Read`]. `P` is the payload's type: a slice
/// of rows when written, a `Vec` (or the single value) when read.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ReadAnswerFrame<P> {
    /// A part of a row answer, or the whole of a single-value one.
    Rows(P),
    /// The row answer is complete.
    Done,
    /// The parent's store refused the read: its error itself, variant for variant (`vike_data::HistoryStoreError`'s
    /// serde form), so the child's run sees the error the parent's store gave.
    Failed(HistoryStoreError),
    /// How many rows the parts of a BAR or TICK answer hold, ahead of them (decision 0281's amendments of
    /// 2026-11-19): the child reserves its rows once ([`row_count`]).
    Count(usize),
}
