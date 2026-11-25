//! [`PipeHistStore`]: the sandboxed child's history, read from the parent over the protocol channel.
//!
//! A READ-ONLY `HistoryStore`: each read verb on the run path crosses as one [`StoreRead`] and comes back as
//! the parent store's own answer (rows, value or `HistoryStoreError`, variant for variant), so the run inside the
//! child sees what it would have seen in the hub. Every other verb, writes and deletes included, is
//! refused here and never crosses (`Unsupported`, naming the sandbox). The defaults the trait builds on
//! the forwarded verbs (`bar_edges`, `load_bars_head`, the `_capped` reads) stay defaults. The kept
//! instrument rows and the operator's share splits are read like any other answer, for the venues the run names
//! (`HistoryStore::kept_instruments_and_splits_of`, one [`StoreRead::KeptInstruments`]), never sent whole.
//!
//! ⚠ **The answers are CACHED by request for the child's life** (one backtest request): a sweep's points
//! re-read their series per point (`SearchEvaluatorStore` does), and each would cross the pipe again. The cache
//! keeps an answer's ENCODED frames, not its rows: the first read decodes once and keeps the bodies it
//! read (no copy), and a repeat decodes them again, so no read pays a clone of a million bars. A failed
//! read is not cached: the next read of it asks again, and fails in its own words. ⚠ **A child that serves ONE run
//! keeps no BAR or TICK answer's frames** ([`reads_again`]): it reads each series once, so its columns frames (18 MB
//! for six months of 1m bars) are read into one reused buffer and dropped as they are decoded (a prefetched bar or tick
//! answer holds only its decoded rows, until its one reader takes them); every other answer is still kept. A bar or tick
//! answer's rows are sized ONCE, by the count the parent sends ahead of its parts, and each part is decoded straight
//! into them: the child holds one copy of them, never a page beside it nor a doubling `Vec`.
//!
//! ⚠ **Several reads in flight** (decision 0281's amendment of 2026-11-19). A read is NUMBERED as it is written,
//! the parent answers in that order, and each answer is read by its own reader in its turn, so the frames stay
//! positional with no tag. A run names its reads that wait on nothing ahead ([`HistoryStore::prefetch`]): they
//! are written back to back and their answers read by one thread of their own as they arrive (a bar or tick answer
//! decoded as it arrives, for its first reader to take), while the run's verbs ask in the run's own order and find
//! them waiting. A verb asking a read already in flight waits for it, never asks it twice. Before its last frames
//! the child reads every prefetched answer through ([`PipeHistStore::drain`]), so the parent never writes into a
//! child that reads no more.

use std::any::Any;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::summary::{CurvePageWire, ParamscanRowSummaryWire};
use vike_data::{
    BarDay, ChainRow, CohortRow, ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreError,
    PerpMetricRow, PlannedRead, SeriesCoverage, SeriesId, TimestampRange,
};
use vike_datahub_client::proto::{
    BOOK_UPDATE_COLUMNS, ColumnRow, ColumnsMagic, DEPTH_COLUMNS, QUOTE_COLUMNS, TRADE_COLUMNS,
};
use vike_hub_protocol::child_pipe;
use vike_hub_protocol::frame::read_frame_raw_into;
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, StoreKindId, SymbolProperties, Trade};

use super::wire::{
    ChildFrame, KeptAnswer, ROWS_PER_FRAME, ReadAnswerFrame, StoreRead, is_columns_part,
    push_bar_part, push_tick_part, row_count,
};
use crate::run_summary::Summary;

/// The channel's output half and how many reads were written through it: a read is written and numbered under
/// this one lock, so the numbers are the order the parent answers in.
struct Output {
    out: Box<dyn Write + Send>,
    asked: u64,
}

/// The channel's input half, the number of the answer it carries next, or why the channel was lost.
struct Input {
    input: Box<dyn Read + Send>,
    next: u64,
    lost: Option<HistoryStoreError>,
}

/// A read's place in the cache.
enum Slot {
    /// Asked, its answer not read yet: another reader of it waits ([`Shared::claim`]).
    Asked,
    /// Read.
    Landed(Landed),
}

/// A read answer as the cache keeps it: its frames (none for a columns answer a one-run child does not keep), and its
/// rows when a prefetch decoded them as they arrived ([`Decoding`]: a `Vec` of the read's row type), for the first
/// reader to take.
struct Landed {
    bodies: Option<Arc<Vec<Vec<u8>>>>,
    rows: Option<Box<dyn Any + Send>>,
}

/// What [`Shared::claim`] found for a read.
enum Claimed {
    /// The rows a prefetch decoded, taken by this first reader.
    Rows(Box<dyn Any + Send>),
    /// The answer's kept frames, to decode again.
    Frames(Arc<Vec<Vec<u8>>>),
    /// Nothing: the read is now ASKED by the caller, who [`Shared::settle`]s it.
    Mine,
}

/// What the store's readers share, the prefetch's thread included.
struct Shared {
    output: Mutex<Output>,
    input: Mutex<Input>,
    /// Signalled when an answer is read through ([`Input::next`] moves) or the channel is lost.
    turn: Condvar,
    // shortcut: unbounded for the one request a child serves (the encoded bodies of every distinct read).
    // Bound it if a measured run runs out of memory.
    cache: Mutex<HashMap<String, Slot>>,
    /// Signalled when an asked read lands or is forgotten.
    landed: Condvar,
}

/// See the module doc.
pub(crate) struct PipeHistStore {
    shared: Arc<Shared>,
    /// The threads reading prefetched answers, joined before the child's last frames ([`Self::drain`]).
    prefetching: Mutex<Vec<JoinHandle<()>>>,
    /// Whether a BAR or TICK answer's frames are kept for a repeat read ([`reads_again`]); every other answer's always
    /// are.
    keep_columns: bool,
}

/// Whether the child serving `request` may read one series twice, and so keeps each bar and tick answer's frames (the
/// module doc): a scan's points, a walk's windows and a permutation test's runs re-read their series. A SINGLE run
/// (a Studio slice, or a profile's `RunBacktest` with no `[permutation]`, either summarized) reads each one once.
/// A profile that does not parse keeps them, as every child did.
pub(super) fn reads_again(request: &Request) -> bool {
    match request {
        Request::Summarized(inner) => reads_again(inner),
        Request::RunSlice { .. } => false,
        Request::RunBacktest(profile_toml) => {
            !ProfileBacktest::from_toml_str_in(profile_toml, Path::new(""))
                .is_ok_and(|profile| profile.permutation.is_none())
        }
        _ => true,
    }
}

/// `mutex`'s guard, a poisoned one's too: nothing here leaves its data half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What reading one answer through came to, the channel still in step.
enum Through {
    /// Its frames, and the first refusal of the reader's decode (the answer's error).
    Read(Vec<Vec<u8>>, Option<HistoryStoreError>),
    /// The parent store's refusal.
    Failed(HistoryStoreError),
}

/// One answer read from `input` through `Done` (or its one `Failed`), each part body handed to `each` as it
/// arrives and kept when `keep` (an answer not kept is read into ONE buffer, reused part after part). `Err` is a
/// channel that can no longer be read in step: lost, or a frame that is no answer frame.
fn read_through(
    input: &mut impl Read,
    keep: bool,
    mut each: impl FnMut(&[u8]) -> Result<(), HistoryStoreError>,
) -> Result<Through, HistoryStoreError> {
    let (mut bodies, mut refused, mut body) = (Vec::new(), None, Vec::new());
    loop {
        read_frame_raw_into(input, &mut body).map_err(lost)?;
        // A bar or tick part is a columns body in place of a `Rows`; every other frame is msgpack.
        if !is_columns_part(&body) {
            match frame_of::<serde::de::IgnoredAny>(&body)? {
                ReadAnswerFrame::Rows(_) | ReadAnswerFrame::Count(_) => {}
                ReadAnswerFrame::Done => return Ok(Through::Read(bodies, refused)),
                ReadAnswerFrame::Failed(e) => return Ok(Through::Failed(e)),
            }
        }
        if refused.is_none() {
            refused = each(&body).err();
        }
        if keep {
            // Taken whole, so the next part is read into a fresh allocation.
            bodies.push(std::mem::take(&mut body));
        }
    }
}

impl Shared {
    /// Write `reads` back to back, numbered in order: the first one's number (the rest follow it), how many were
    /// written, and the fault that stopped the rest. A frame over the cap is refused with nothing written
    /// (`child_pipe::write_message`), so the channel stays in step; any other fault loses it for every reader.
    fn ask(&self, reads: &[StoreRead]) -> (u64, usize, Option<HistoryStoreError>) {
        let mut output = lock(&self.output);
        let first = output.asked;
        for (written, read) in reads.iter().enumerate() {
            if let Err(e) =
                child_pipe::write_message(&mut output.out, &ChildFrame::Read(read.clone()))
            {
                drop(output);
                let in_step = e.kind() == std::io::ErrorKind::InvalidData;
                let e = lost(e);
                if !in_step {
                    lock(&self.input).lost.get_or_insert_with(|| e.clone());
                    self.turn.notify_all();
                }
                return (first, written, Some(e));
            }
            output.asked += 1;
        }
        (first, reads.len(), None)
    }

    /// Answer number `number`, read once every answer before it is, each part body handed to `each` as it
    /// arrives: its frames (none kept unless `keep`), or the store's refusal, or the first refusal of `each` (the
    /// rest still read through `Done`, so the channel stays in step), or the lost channel.
    fn answer(
        &self,
        number: u64,
        keep: bool,
        each: impl FnMut(&[u8]) -> Result<(), HistoryStoreError>,
    ) -> Result<Vec<Vec<u8>>, HistoryStoreError> {
        let mut input = lock(&self.input);
        while input.next != number && input.lost.is_none() {
            input = self.turn.wait(input).unwrap_or_else(PoisonError::into_inner);
        }
        if let Some(e) = &input.lost {
            return Err(e.clone());
        }
        let through = read_through(&mut input.input, keep, each);
        match &through {
            Ok(_) => input.next += 1,
            Err(e) => input.lost = Some(e.clone()),
        }
        drop(input);
        self.turn.notify_all();
        match through? {
            Through::Read(bodies, None) => Ok(bodies),
            Through::Read(_, Some(e)) | Through::Failed(e) => Err(e),
        }
    }

    /// The cache's answer to `key`, waiting while another reader has it asked: the decoded rows no reader took yet,
    /// else the kept frames, else [`Claimed::Mine`] with `key` now ASKED by the caller. A slot whose frames were not
    /// kept is gone once its rows are taken.
    fn claim(&self, key: &str) -> Claimed {
        let mut cache = lock(&self.cache);
        loop {
            match cache.get_mut(key) {
                Some(Slot::Landed(landed)) => {
                    if let Some(rows) = landed.rows.take() {
                        if landed.bodies.is_none() {
                            cache.remove(key);
                        }
                        return Claimed::Rows(rows);
                    }
                    if let Some(bodies) = &landed.bodies {
                        return Claimed::Frames(Arc::clone(bodies));
                    }
                    cache.insert(key.to_string(), Slot::Asked);
                    return Claimed::Mine;
                }
                Some(Slot::Asked) => {
                    cache = self.landed.wait(cache).unwrap_or_else(PoisonError::into_inner);
                }
                None => {
                    cache.insert(key.to_string(), Slot::Asked);
                    return Claimed::Mine;
                }
            }
        }
    }

    /// `key` read: kept (`Some`: its frames and any bars decoded for its first reader), or forgotten, a failed read
    /// being cached by nobody (`None`). Every reader waiting on it wakes.
    fn settle(&self, key: String, landed: Option<Landed>) {
        let mut cache = lock(&self.cache);
        match landed {
            Some(landed) => {
                cache.insert(key, Slot::Landed(landed));
            }
            None => {
                cache.remove(&key);
            }
        }
        drop(cache);
        self.landed.notify_all();
    }
}

/// The cache key of `read`: the frame it crosses as, spelled.
fn key_of(read: &StoreRead) -> Result<String, HistoryStoreError> {
    serde_json::to_string(read).map_err(|e| HistoryStoreError::Io(e.to_string()))
}

/// One part of a BAR answer decoded into `rows`: the count the parent states first sizes them ONCE, then each
/// bar-columns part is appended as it arrives ([`push_bar_part`]).
fn bar_part_of(body: &[u8], rows: &mut Vec<Bar>) -> Result<(), HistoryStoreError> {
    match row_count(body) {
        Some(count) => {
            rows.reserve_exact(count);
            Ok(())
        }
        None => push_bar_part(body, rows),
    }
}

/// [`bar_part_of`] for a TICK answer of `magic` ([`push_tick_part`]).
fn tick_part_of<R: ColumnRow>(
    body: &[u8],
    magic: &ColumnsMagic,
    rows: &mut Vec<R>,
) -> Result<(), HistoryStoreError> {
    match row_count(body) {
        Some(count) => {
            rows.reserve_exact(count);
            Ok(())
        }
        None => push_tick_part(body, magic, rows),
    }
}

/// The rows a prefetch decodes a COLUMNS answer into as it arrives (a bar read's or a tick read's), so a child that
/// keeps no columns frame holds none of a prefetched answer either; every other answer is kept as its frames.
enum Decoding {
    Bars(Vec<Bar>),
    Quotes(Vec<TickQuote>),
    Trades(Vec<TickTrade>),
    Books(Vec<L2BookUpdate>),
}

impl Decoding {
    /// The decoding `read`'s answer takes, `None` for a read answered in msgpack.
    fn of(read: &StoreRead) -> Option<Self> {
        match read {
            StoreRead::LoadBars { .. } | StoreRead::LoadBarsSourcedCapped { .. } => {
                Some(Self::Bars(Vec::new()))
            }
            StoreRead::ScanQuotes { .. } => Some(Self::Quotes(Vec::new())),
            StoreRead::ScanTrades { .. } => Some(Self::Trades(Vec::new())),
            StoreRead::ScanBookUpdates { .. } => Some(Self::Books(Vec::new())),
            _ => None,
        }
    }

    /// One part of the answer, decoded into the rows.
    fn push(&mut self, body: &[u8]) -> Result<(), HistoryStoreError> {
        match self {
            Self::Bars(rows) => bar_part_of(body, rows),
            Self::Quotes(rows) => tick_part_of(body, &QUOTE_COLUMNS, rows),
            Self::Trades(rows) => tick_part_of(body, &TRADE_COLUMNS, rows),
            Self::Books(rows) => tick_part_of(body, &BOOK_UPDATE_COLUMNS, rows),
        }
    }

    /// The decoded rows, for their reader to take ([`taken`]).
    fn into_rows(self) -> Box<dyn Any + Send> {
        match self {
            Self::Bars(rows) => Box::new(rows),
            Self::Quotes(rows) => Box::new(rows),
            Self::Trades(rows) => Box::new(rows),
            Self::Books(rows) => Box::new(rows),
        }
    }
}

/// The rows a prefetch decoded for a read, as the `Vec` of the read's own row type ([`Decoding::of`]).
fn taken<R: 'static>(rows: Box<dyn Any + Send>) -> Vec<R> {
    *rows.downcast::<Vec<R>>().unwrap_or_else(|_| unreachable!("a read's rows are its row type"))
}

/// The words of a frame that is not the answer part it should be.
fn garbled(e: impl std::fmt::Display) -> HistoryStoreError {
    HistoryStoreError::Io(format!("the strategy sandbox read an answer it cannot decode: {e}"))
}

/// One msgpack answer frame's body, decoded.
fn frame_of<'a, P: Deserialize<'a>>(
    body: &'a [u8],
) -> Result<ReadAnswerFrame<P>, HistoryStoreError> {
    rmp_serde::from_slice(body).map_err(garbled)
}

/// The words of a lost channel: the parent is gone or cut the child off.
fn lost(e: std::io::Error) -> HistoryStoreError {
    HistoryStoreError::Io(format!("the strategy sandbox lost the channel to its parent: {e}"))
}

/// Every verb this store does not serve.
fn not_served(verb: &str) -> HistoryStoreError {
    HistoryStoreError::Unsupported(format!(
        "{verb} is not served to a strategy sandbox, which reads history only"
    ))
}

impl PipeHistStore {
    /// A store that keeps every answer for a repeat read.
    pub(crate) fn new(input: Box<dyn Read + Send>, output: Box<dyn Write + Send>) -> Self {
        let shared = Shared {
            output: Mutex::new(Output { out: output, asked: 0 }),
            input: Mutex::new(Input { input, next: 0, lost: None }),
            turn: Condvar::new(),
            cache: Mutex::new(HashMap::new()),
            landed: Condvar::new(),
        };
        Self { shared: Arc::new(shared), prefetching: Mutex::new(Vec::new()), keep_columns: true }
    }

    /// The store of the child serving `request`: [`Self::new`]'s, keeping no bar or tick answer's frames when the child
    /// reads each series once ([`reads_again`]).
    pub(crate) fn serving(
        request: &Request,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
    ) -> Self {
        Self { keep_columns: reads_again(request), ..Self::new(input, output) }
    }

    /// Read every prefetched answer through ([`HistoryStore::prefetch`]'s thread), so that nothing the parent still
    /// writes is left unread once the child's last frames go up and it stops reading.
    fn drain(&self) {
        for reader in std::mem::take(&mut *lock(&self.prefetching)) {
            let _ = reader.join();
        }
    }

    /// The output half for one of the child's LAST frames, every prefetched answer read first ([`Self::drain`]).
    fn last_frames(&self) -> MutexGuard<'_, Output> {
        self.drain();
        lock(&self.shared.output)
    }

    /// The parent store's kept rows and splits of `venues` (`None`: every venue's): one value read
    /// ([`StoreRead::KeptInstruments`]), cached as every read is, each half with its own error.
    #[expect(clippy::type_complexity, reason = "the trait's two answers, side by side")]
    fn kept(
        &self,
        venues: Option<Vec<String>>,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        match self.value::<KeptAnswer>(StoreRead::KeptInstruments { venues }) {
            Ok(kept) => kept,
            Err(e) => (Err(e.clone()), Err(e)),
        }
    }

    /// Write the finished answer: the last frame of the child, NAMED (`super::wire`'s module doc).
    pub(crate) fn send_answer(&self, response: Response) -> std::io::Result<()> {
        let answer = ChildFrame::Answer(Box::new(response));
        child_pipe::write_message_named(&mut self.last_frames().out, &answer)
    }

    /// Write the saved run a Studio single run left (`super::wire::ChildFrame::StudioRun`), ahead of the answer,
    /// NAMED (`super::wire`'s module doc).
    pub(crate) fn send_studio_run(
        &self,
        run: crate::backtesthub_server::saved_run::StudioRun,
    ) -> std::io::Result<()> {
        let run = ChildFrame::StudioRun(Box::new(run));
        child_pipe::write_message_named(&mut self.last_frames().out, &run)
    }

    /// Write one row of a summarized PROFILE search (`super::wire::ChildFrame::ParamscanRow`): its summary and its
    /// whole JSON in one frame, or, when that frame would pass the cap (which writes nothing), its summary alone, so the
    /// answer is whole and the parent saves no run.
    pub(crate) fn send_paramscan_row(
        &self,
        row: ParamscanRowSummaryWire,
        json: String,
    ) -> std::io::Result<()> {
        let mut output = self.last_frames();
        let whole = ChildFrame::ParamscanRow(Box::new(row.clone()), Some(json));
        // NAMED, as a summarized point is: a row's summary leaves fields out before its last (`wire`'s module doc).
        match child_pipe::write_message_named(&mut output.out, &whole) {
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                child_pipe::write_message_named(
                    &mut output.out,
                    &ChildFrame::ParamscanRow(Box::new(row), None),
                )
            }
            written => written,
        }
    }

    /// Write a summarized search's report with no rows (`super::wire::ChildFrame::ParamscanTail`), after its last row.
    pub(crate) fn send_paramscan_tail(&self, tail: String) -> std::io::Result<()> {
        child_pipe::write_message(&mut self.last_frames().out, &ChildFrame::ParamscanTail(tail))
    }

    /// Write what the parent holds of a SUMMARIZED run, point by point (each point's trades in parts of
    /// [`ROWS_PER_FRAME`], then the point), and hand back the answer to finish with: the summary itself for a
    /// single run (its trades sent, then its pinned event log as ONE `ChildFrame::HeldLog` when it carries one, else
    /// its whole curve in parts of [`ROWS_PER_FRAME`] steps, no point), the scan's answer for a scan. A log over the
    /// frame cap is refused with nothing written, and the curve goes up in its place.
    pub(crate) fn send_summary(&self, summary: Summary) -> std::io::Result<Response> {
        fn send_trades(out: &mut impl Write, trades: &[Trade]) -> std::io::Result<()> {
            for part in trades.chunks(ROWS_PER_FRAME) {
                child_pipe::write_message(out, &ChildFrame::HeldTrades(part.to_vec()))?;
            }
            Ok(())
        }
        let mut output = self.last_frames();
        match summary {
            Summary::Run(summary, trades, curve, log) => {
                send_trades(&mut output.out, &trades)?;
                if let Some(log) = log {
                    match child_pipe::write_message(&mut output.out, &ChildFrame::HeldLog(log)) {
                        Ok(()) => return Ok(Response::RunSummary(summary)),
                        // Over the cap, nothing was written: the curve goes up instead.
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {}
                        Err(e) => return Err(e),
                    }
                }
                // The whole curve in parts of `ROWS_PER_FRAME` steps, each cut as a curve page is.
                for part in 0..curve.equity.len().div_ceil(ROWS_PER_FRAME) {
                    let page =
                        CurvePageWire::of(&curve.equity, &curve.timestamp, part, ROWS_PER_FRAME);
                    child_pipe::write_message(&mut output.out, &ChildFrame::HeldCurve(page))?;
                }
                Ok(Response::RunSummary(summary))
            }
            Summary::Scan(scan, held) => {
                for (point, trades) in held {
                    send_trades(&mut output.out, &trades)?;
                    // Named: `RunSummaryWire::run` is left out before its last field.
                    let point = ChildFrame::HeldPoint(Box::new(point));
                    child_pipe::write_message_named(&mut output.out, &point)?;
                }
                Ok(Response::ParamscanSummary(scan))
            }
            Summary::Answer(answer) => Ok(*answer),
        }
    }

    /// `read`'s answer, each encoded `Rows` part body handed to `each` in order: from the cache (waiting while it is
    /// in flight), or asked and read from the parent until `Done` in its turn, then cached when `keep` (a `Failed`
    /// is the store's error, not cached). A body read from the parent is decoded AS IT ARRIVES, so the child decodes
    /// one part while the parent encodes the next; an answer not kept is read into ONE buffer, reused part after
    /// part. A part `each` refuses is the answer's error, returned once the rest is read through `Done` (the channel
    /// stays in step) and not cached. `Some(rows)` in place of `each`'s calls: the rows a prefetch decoded as they
    /// arrived ([`Decoding`]), taken by this first reader of them.
    fn parts(
        &self,
        read: StoreRead,
        keep: bool,
        mut each: impl FnMut(&[u8]) -> Result<(), HistoryStoreError>,
    ) -> Result<Option<Box<dyn Any + Send>>, HistoryStoreError> {
        let key = key_of(&read)?;
        match self.shared.claim(&key) {
            Claimed::Rows(rows) => return Ok(Some(rows)),
            Claimed::Frames(bodies) => {
                bodies.iter().try_for_each(|body| each(body))?;
                return Ok(None);
            }
            Claimed::Mine => {}
        }
        let answered = match self.shared.ask(std::slice::from_ref(&read)) {
            (_, _, Some(refused)) => Err(refused),
            (number, _, None) => self.shared.answer(number, keep, &mut each),
        };
        let (landed, outcome) = match answered {
            Ok(bodies) => {
                let kept = keep.then(|| Landed { bodies: Some(Arc::new(bodies)), rows: None });
                (kept, Ok(None))
            }
            Err(e) => (None, Err(e)),
        };
        self.shared.settle(key, landed);
        outcome
    }

    /// A row answer: its parts, appended.
    fn rows<T: DeserializeOwned>(&self, read: StoreRead) -> Result<Vec<T>, HistoryStoreError> {
        let mut rows = Vec::new();
        self.parts(read, true, |body| {
            rows.append(&mut part_of::<Vec<T>>(body)?);
            Ok(())
        })?;
        Ok(rows)
    }

    /// A BAR answer: the count the parent states first sizes the rows ONCE (a repeat read of a kept answer replays
    /// it from the cache), then each bar-columns part is decoded into them as it arrives ([`bar_part_of`]); or the
    /// rows a prefetch decoded the same way.
    fn bars(&self, read: StoreRead) -> Result<Vec<Bar>, HistoryStoreError> {
        let mut rows = Vec::new();
        let decoded = self.parts(read, self.keep_columns, |body| bar_part_of(body, &mut rows))?;
        Ok(decoded.map_or(rows, taken))
    }

    /// A TICK answer, as [`Self::bars`] reads a bar answer: the count the parent states first sizes the rows ONCE,
    /// then each `magic` columns part is decoded into them as it arrives ([`tick_part_of`]); or the rows a prefetch
    /// decoded the same way (a tick replay names its lane reads ahead).
    fn ticks<R: ColumnRow + Send>(
        &self,
        read: StoreRead,
        magic: &ColumnsMagic,
    ) -> Result<Vec<R>, HistoryStoreError> {
        let mut rows = Vec::new();
        let decoded =
            self.parts(read, self.keep_columns, |body| tick_part_of(body, magic, &mut rows))?;
        Ok(decoded.map_or(rows, taken))
    }

    /// [`HistoryStore::prefetch`]: each of `reads` the cache neither holds nor has in flight, written back to back
    /// and its answer read, in its turn, by ONE thread of its own as it arrives (a bar or tick answer decoded as it
    /// arrives, [`Decoding`], for its first reader to take), then cached as a verb's read is (a failed one forgotten,
    /// so its verb asks it again). A thread that cannot start asks nothing, and every verb asks for itself.
    fn ask_ahead(&self, reads: &[PlannedRead]) {
        let (mut asked, mut keys) = (Vec::new(), Vec::new());
        {
            let mut cache = lock(&self.shared.cache);
            for read in reads.iter().map(StoreRead::from) {
                let Ok(key) = key_of(&read) else { continue };
                if !cache.contains_key(&key) {
                    cache.insert(key.clone(), Slot::Asked);
                    keys.push((key, Decoding::of(&read)));
                    asked.push(read);
                }
            }
        }
        if asked.is_empty() {
            return;
        }
        let (to_reader, numbered) = mpsc::channel::<(u64, Vec<(String, Option<Decoding>)>)>();
        let (shared, keep_columns) = (Arc::clone(&self.shared), self.keep_columns);
        let reader = thread::Builder::new().name("pipe-prefetch".into()).spawn(move || {
            let Ok((first, landing)) = numbered.recv() else { return };
            for ((key, mut decoding), number) in landing.into_iter().zip(first..) {
                let keep = decoding.is_none() || keep_columns;
                let answered = shared.answer(number, keep, |body| match decoding.as_mut() {
                    Some(rows) => rows.push(body),
                    None => Ok(()),
                });
                let landed = answered.ok().map(|bodies| Landed {
                    bodies: keep.then(|| Arc::new(bodies)),
                    rows: decoding.map(Decoding::into_rows),
                });
                shared.settle(key, landed);
            }
        });
        let Ok(reader) = reader else {
            keys.into_iter().for_each(|(key, _)| self.shared.settle(key, None));
            return;
        };
        lock(&self.prefetching).push(reader);
        let (first, written, _) = self.shared.ask(&asked);
        // The reads that were not written are forgotten, so each verb asks for itself.
        let unasked = keys.split_off(written);
        let _ = to_reader.send((first, keys));
        unasked.into_iter().for_each(|(key, _)| self.shared.settle(key, None));
    }

    /// A single-value answer: its one part.
    fn value<V: DeserializeOwned>(&self, read: StoreRead) -> Result<V, HistoryStoreError> {
        let mut value = None;
        self.parts(read, true, |body| {
            if value.is_none() {
                value = Some(part_of::<V>(body)?);
            }
            Ok(())
        })?;
        value.ok_or_else(|| {
            HistoryStoreError::Io(
                "the strategy sandbox's parent answered a value with no value".into(),
            )
        })
    }
}

/// A `Rows` part's payload, decoded.
fn part_of<'a, P: Deserialize<'a>>(body: &'a [u8]) -> Result<P, HistoryStoreError> {
    match frame_of::<P>(body)? {
        ReadAnswerFrame::Rows(part) => Ok(part),
        _ => Err(garbled("an answer part is not a part")),
    }
}

impl HistoryStore for PipeHistStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars(StoreRead::LoadBars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
        })
    }

    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars(StoreRead::LoadBarsSourcedCapped {
            venue: venue.into(),
            source: source.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
            budget,
        })
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.ticks(
            StoreRead::ScanQuotes { venue: venue.into(), symbol: symbol.into(), range },
            &QUOTE_COLUMNS,
        )
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.ticks(
            StoreRead::ScanTrades { venue: venue.into(), symbol: symbol.into(), range },
            &TRADE_COLUMNS,
        )
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.ticks(
            StoreRead::ScanBookUpdates { venue: venue.into(), symbol: symbol.into(), range },
            &BOOK_UPDATE_COLUMNS,
        )
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.ticks(
            StoreRead::ScanDepth { venue: venue.into(), symbol: symbol.into(), range },
            &DEPTH_COLUMNS,
        )
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.rows(StoreRead::ScanCohort { venue: venue.into(), asset: asset.into(), range })
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.rows(StoreRead::ScanPerpMetrics { venue: venue.into(), symbol: symbol.into(), range })
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.rows(StoreRead::ScanChain {
            venue: venue.into(),
            underlying: underlying.into(),
            range,
        })
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        self.value(StoreRead::PropertiesAsOf {
            venue: venue.into(),
            symbol: symbol.into(),
            timestamp: ts,
        })
    }

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        self.rows(StoreRead::ListSeries)
    }

    fn prefetch(&self, reads: &[PlannedRead]) {
        self.ask_ahead(reads);
    }

    /// The parent store's answer for a BAR read: the pin check `super::pinned_log` makes, one value read
    /// ([`StoreRead::BarDays`]). No other kind is asked through the pipe (a child stamps only the bars its run
    /// marked), so any other is unknown here, the trait's own answer.
    fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        let (StoreKindId::Bar, Some(interval)) = (kind, interval) else { return Ok(None) };
        self.value(StoreRead::BarDays {
            venue: venue.into(),
            source: source.map(str::to_string),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
        })
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.rows(StoreRead::Inventory)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        self.rows(StoreRead::SeriesGaps(id.clone()))
    }

    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        self.value(StoreRead::SeriesFacts(id.clone()))
    }

    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.kept(None).0
    }

    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        self.kept(None).1
    }

    fn kept_instruments_and_splits(
        &self,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        self.kept(None)
    }

    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        self.kept(Some(venues.to_vec()))
    }

    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        Err(not_served("scan_symbol_properties"))
    }

    fn scan_equity(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        Err(not_served("scan_equity"))
    }

    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        Err(not_served("scan_exec_fills"))
    }

    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        Err(not_served("scan_exec_orders"))
    }

    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_bars"))
    }

    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[TickQuote],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_quotes"))
    }

    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TickTrade],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_trades"))
    }

    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[L2BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_exec_orders"))
    }

    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("resample_trades_to_bars"))
    }
}
