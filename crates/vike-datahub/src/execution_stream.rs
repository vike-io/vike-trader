//! **The EXECUTION STREAM's datahub half: the FIX drop copy's receiver.** A tradehub opens one stream per journal
//! (`vike_datahub_client::proto::Request::ExecutionStreamOpen`) and sends every execution and order-lifecycle record
//! its OMS journaled, each self-contained and numbered by its journal sequence; this module stores each one
//! idempotently, keeps ONE cursor per journal in `vike.db` and asks "everything since N" after a gap. It replaces the
//! journal materializer: the datahub reads no journal file, needs no access to the tradehub's directory, and holds
//! nothing per order (a window's buffer is all the state). The grammar is
//! `vike_datahub_client::execution_stream`'s module doc.
//!
//! # A window, and what a crash anywhere in it leaves
//!
//! The session buffers records and FLUSHES the window `(S, W]` at [`limits::FLUSH_RECORDS`] records,
//! [`limits::FLUSH_EVERY`] after its first record, or on a `Heartbeat`:
//!
//! 1. in `vike.db`, `writing_through = W` (`vike_secrets::execution_streams::begin_execution_window`);
//! 2. the records become rows, appended per series under keys that name `(S, W]` (`sink`);
//! 3. the window's unverified gaps are recorded, then `stored_through = W` with the record's time
//!    (`end_execution_window`);
//! 4. `Stored { through_sequence: W }` tells the tradehub its new journal floor.
//!
//! A crash before 1 wrote nothing. A crash after 1 leaves `writing_through > stored_through`, and the next open
//! PINS its first window to exactly `(S, W]`: the same keys, so every series that landed is a store no-op, and the
//! rest are written. A failed step retries the SAME window every [`limits::RETRY_EVERY`] and reads no frame meanwhile.
//!
//! # Sequences
//!
//! Every record names the record sent before it (`previous_sequence`). A record that does not chain from the last one
//! received is the first gap: the session stores the contiguous records it holds (`Stored` up to the highest
//! contiguous sequence) and answers `Resend { after_sequence }` from there, dropping the rest of the frame and every
//! record still in flight until one chains from it. A `Gap` (the journal pruned what the datahub lacks) is recorded
//! at ERROR and moves the cursor past itself.
//!
//! # One stream per journal
//!
//! A second open of a journal WINS: the older socket is shut and the newer open waits, at most
//! [`limits::SUPERSEDE_WAIT`], for the older session to end, so two sessions never write one cursor (a half-open TCP
//! must not hold the stream, and must not race the stream that replaced it).

pub(crate) mod cursor;
mod failures;
pub mod limits;
pub(crate) mod sink;

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use vike_data::HistoryStore;
use vike_datahub_client::execution_stream::{
    ExecutionAccountPositionsWire, ExecutionRecordWire, ExecutionStreamDown,
    ExecutionStreamGapWire, ExecutionStreamStatusWire, ExecutionStreamUp, validate_journal_id,
};
use vike_datahub_client::proto::{
    EXECUTION_STREAM_FRAME_CEILING_BYTES, EXECUTION_STREAM_HEARTBEAT,
    EXECUTION_STREAM_RECORDS_PER_FRAME, EXECUTION_STREAM_STRING_CEILING_BYTES, Response,
};
use vike_hub_protocol::frame::{BodyCodec, decode_body, read_frame_raw_capped, write_reply_as};
use vike_secrets::execution_streams::{
    ExecutionStreamGapRow, ExecutionStreamIdentity, GAP_PRUNED, begin_execution_window,
    end_execution_window, read_account_positions, read_execution_streams, record_account_positions,
    record_execution_gap,
};

use self::cursor::StreamCursor;
use self::failures::{FlushFailures, FlushStage};
use self::limits::{FLUSH_EVERY, FLUSH_RECORDS, RETRY_EVERY, SUPERSEDE_WAIT, WRITE_TIMEOUT};

/// The refusal of a datahub that mounts no execution stream: no settings database to keep a cursor in.
pub const NO_EXECUTION_STREAM: &str = "this datahub records no live executions: it keeps no settings database, \
     so it has nowhere to keep an execution stream's cursor (`vike-cli secrets init` creates one, and a restart of \
     the datahub mounts the stream). Nothing was stored and this connection is unchanged.";

/// A point a test may "crash" the datahub at: the session stops there, writing nothing more, as a killed process
/// would. Production never names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStreamFaultPoint {
    /// `writing_through` is written and nothing of the window is appended.
    AfterWritingThrough,
    /// `landed` series of the window are appended (fills, then forecasts, then orders, one per series).
    AfterSeries {
        /// How many series have landed.
        landed: usize,
    },
    /// Every series landed and `stored_through` is not written.
    BeforeStoredThrough,
}

/// A test's crash points: `true` at a point is "the process died here".
pub type ExecutionStreamFaults = Arc<dyn Fn(ExecutionStreamFaultPoint) -> bool + Send + Sync>;

/// The free-space floor the forecast rows ask (`Some(why)` while there is no room).
pub type ExecutionStreamForecastFloor = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// **The lane**: where cursors live, which journals stream now, and the forecast floor. Mounted by the datahub when
/// it keeps a settings database; its absence answers [`NO_EXECUTION_STREAM`].
pub struct ExecutionStreamLane {
    settings_dir: PathBuf,
    forecast_floor: Option<ExecutionStreamForecastFloor>,
    faults: Option<ExecutionStreamFaults>,
    live: Mutex<HashMap<String, Arc<LiveStream>>>,
    generation: AtomicU64,
}

/// One journal's running session, as the lane holds it: how a newer open ends it.
struct LiveStream {
    generation: u64,
    socket: TcpStream,
    superseded: AtomicBool,
    ended: (Mutex<bool>, Condvar),
}

impl LiveStream {
    fn end(&self) {
        let (ended, signal) = &self.ended;
        *ended.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        signal.notify_all();
    }

    /// Wait at most `within` for the session to end; `true` when it did.
    fn wait_ended(&self, within: Duration) -> bool {
        let (ended, signal) = &self.ended;
        let guard = ended.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, _) = signal
            .wait_timeout_while(guard, within, |ended| !*ended)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

impl std::fmt::Debug for ExecutionStreamLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionStreamLane")
            .field("settings_dir", &self.settings_dir)
            .field("forecast_floor", &self.forecast_floor.as_ref().map(|_| "<check>"))
            .finish_non_exhaustive()
    }
}

impl ExecutionStreamLane {
    /// A lane whose cursors live in `settings_dir`'s `vike.db`; `forecast_floor` is the optional writers' free-space
    /// floor over the store's root.
    pub fn new(
        settings_dir: PathBuf,
        forecast_floor: Option<ExecutionStreamForecastFloor>,
    ) -> Self {
        ExecutionStreamLane {
            settings_dir,
            forecast_floor,
            faults: None,
            live: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    /// The same lane with a test's crash points.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_fault_points(mut self, faults: ExecutionStreamFaults) -> Self {
        self.faults = Some(faults);
        self
    }

    fn fault(&self, point: ExecutionStreamFaultPoint) -> bool {
        self.faults.as_ref().is_some_and(|f| f(point))
    }

    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveStream>>> {
        self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take `journal_id` for a new session on `socket`: an older session of it is superseded (its socket shut) and
    /// waited for. `Err` when the older session did not end within [`SUPERSEDE_WAIT`].
    fn claim(&self, journal_id: &str, socket: TcpStream) -> Result<Arc<LiveStream>, String> {
        let deadline = Instant::now() + SUPERSEDE_WAIT;
        loop {
            let older = {
                let mut live = self.live();
                match live.get(journal_id) {
                    None => {
                        let mine = Arc::new(LiveStream {
                            generation: self.generation.fetch_add(1, Ordering::Relaxed),
                            socket,
                            superseded: AtomicBool::new(false),
                            ended: (Mutex::new(false), Condvar::new()),
                        });
                        live.insert(journal_id.to_string(), Arc::clone(&mine));
                        return Ok(mine);
                    }
                    Some(older) => Arc::clone(older),
                }
            };
            older.superseded.store(true, Ordering::Relaxed);
            let _ = older.socket.shutdown(std::net::Shutdown::Both);
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !older.wait_ended(left) {
                return Err(format!(
                    "the previous stream of journal {journal_id} is still ending (a window of it is being written); \
                     retry the open. Nothing was stored and this connection is unchanged."
                ));
            }
        }
    }

    /// Release `journal_id`, when `mine` still holds it.
    fn release(&self, journal_id: &str, mine: &LiveStream) {
        let mut live = self.live();
        if live.get(journal_id).is_some_and(|held| held.generation == mine.generation) {
            live.remove(journal_id);
        }
        drop(live);
        mine.end();
    }

    /// **`Request::ExecutionStreams`**: every journal's cursor, whether a tradehub streams it now, and its gaps. A
    /// settings database that will not read is the refusal in words, never an empty list.
    pub fn statuses(&self) -> Result<Vec<ExecutionStreamStatusWire>, String> {
        let streams = read_execution_streams(&self.settings_dir).map_err(|e| e.to_string())?;
        let live = self.live();
        Ok(streams
            .into_iter()
            .map(|(row, gaps)| ExecutionStreamStatusWire {
                connected: live.contains_key(&row.journal_id),
                journal_id: row.journal_id,
                tradehub: row.tradehub,
                stored_through: row.stored_through,
                last_stored_ms: row.last_stored_ms,
                gaps: gaps
                    .into_iter()
                    .map(|g| ExecutionStreamGapWire {
                        kind: g.kind,
                        after_sequence: g.after_sequence,
                        resumes_at_sequence: g.resumes_at_sequence,
                        lost_from_ms: g.lost_from_ms,
                        lost_to_ms: g.lost_to_ms,
                        noted_ms: g.noted_ms,
                        reason: g.reason,
                        order_client_ids: g.order_client_ids,
                    })
                    .collect(),
            })
            .collect())
    }

    /// **`Request::ExecutionAccountPositions`**: every account's latest position snapshot at or before
    /// `at_or_before_ms`. A settings database that will not read is the refusal in words, never "no snapshot".
    pub fn account_positions(
        &self,
        at_or_before_ms: i64,
    ) -> Result<Vec<ExecutionAccountPositionsWire>, String> {
        let rows = read_account_positions(&self.settings_dir, at_or_before_ms)
            .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(|r| ExecutionAccountPositionsWire {
                generation: r.generation,
                sequence: r.sequence,
                as_of_ms: r.as_of_ms,
                venue: r.venue,
                account: r.account,
                positions: r.positions,
                unknown: r.unknown,
            })
            .collect())
    }
}

/// `Request::ExecutionAccountPositions`, answered.
pub(crate) fn account_positions_verb(
    lane: Option<&ExecutionStreamLane>,
    at_or_before_ms: i64,
) -> Response {
    match lane.map(|l| l.account_positions(at_or_before_ms)) {
        None => Response::Error(NO_EXECUTION_STREAM.to_string()),
        Some(Ok(rows)) => Response::ExecutionAccountPositions(rows),
        Some(Err(why)) => {
            Response::Error(format!("the accounts' position snapshots could not be read: {why}"))
        }
    }
}

/// `Request::ExecutionStreams`, answered.
pub(crate) fn execution_streams_verb(lane: Option<&ExecutionStreamLane>) -> Response {
    match lane.map(ExecutionStreamLane::statuses) {
        None => Response::Error(NO_EXECUTION_STREAM.to_string()),
        Some(Ok(streams)) => Response::ExecutionStreams(streams),
        Some(Err(why)) => {
            Response::Error(format!("the execution streams could not be read: {why}"))
        }
    }
}

/// What a tradehub's open asked: `Request::ExecutionStreamOpen`'s fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStreamOpening {
    /// The journal directory's id.
    pub journal_id: String,
    /// The tradehub's identity line.
    pub tradehub: String,
    /// The oldest record its journal holds.
    pub oldest_sequence: u64,
    /// Its newest durable record.
    pub head_sequence: u64,
}

/// How an open ended, for the connection loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionStreamEnd {
    /// Refused positionally (`Response::Error` written): the connection stays positional.
    Refused,
    /// The session ran (or the reply could not be written): the connection is over.
    Closed,
}

/// **Serve one open**: refuse it positionally, or answer `ExecutionStreamOpened` (the LAST positional frame) and run
/// the session on this socket until the tradehub leaves, a newer open of its journal supersedes it, or a frame breaks
/// the grammar. `key_id` is the key the connection authenticated with; `idle` the server's idle read timeout;
/// `codec` the form the open came in, which every down frame is written in (an up frame is read in any form).
pub(crate) fn serve(
    stream: &mut TcpStream,
    lane: &ExecutionStreamLane,
    store: &(dyn HistoryStore + Send + Sync),
    opening: ExecutionStreamOpening,
    key_id: Option<&str>,
    peer: Option<SocketAddr>,
    idle: Duration,
    codec: BodyCodec,
) -> ExecutionStreamEnd {
    let refuse = |stream: &mut TcpStream, why: String| {
        let _ = write_reply_as(stream, codec, &Response::Error(why));
        ExecutionStreamEnd::Refused
    };
    if let Err(why) = validate_journal_id(&opening.journal_id) {
        return refuse(stream, format!("the execution stream was not opened: {why}"));
    }
    if opening.tradehub.len() > EXECUTION_STREAM_STRING_CEILING_BYTES {
        return refuse(
            stream,
            format!(
                "the execution stream was not opened: the tradehub's identity line is {} bytes, over \
                 {EXECUTION_STREAM_STRING_CEILING_BYTES}",
                opening.tradehub.len()
            ),
        );
    }
    let socket = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => return refuse(stream, format!("the execution stream was not opened: {e}")),
    };
    let live = match lane.claim(&opening.journal_id, socket) {
        Ok(live) => live,
        Err(why) => return refuse(stream, why),
    };
    let end = open_and_run(stream, lane, store, &opening, key_id, peer, idle, &live, codec);
    lane.release(&opening.journal_id, &live);
    end
}

fn open_and_run(
    stream: &mut TcpStream,
    lane: &ExecutionStreamLane,
    store: &(dyn HistoryStore + Send + Sync),
    opening: &ExecutionStreamOpening,
    key_id: Option<&str>,
    peer: Option<SocketAddr>,
    idle: Duration,
    live: &LiveStream,
    codec: BodyCodec,
) -> ExecutionStreamEnd {
    let identity = ExecutionStreamIdentity {
        journal_id: opening.journal_id.clone(),
        tradehub: opening.tradehub.clone(),
        key_id: key_id.map(str::to_string),
    };
    let cursor = match StreamCursor::open(&lane.settings_dir, &identity, opening.head_sequence) {
        Ok(c) => c,
        Err(why) => {
            tracing::error!(journal_id = %opening.journal_id, error = %why, "vike-datahub: an execution stream's cursor could not be read");
            let _ = write_reply_as(
                stream,
                codec,
                &Response::Error(format!(
                    "the execution stream was not opened: this datahub's cursor of journal {} could not be read \
                     ({why}). Nothing was stored and this connection is unchanged.",
                    opening.journal_id
                )),
            );
            return ExecutionStreamEnd::Refused;
        }
    };
    let reply = Response::ExecutionStreamOpened {
        stored_through: cursor.stored_through,
        stored_through_recorded_ms: cursor.recorded_ms,
        heartbeat_ms: u64::try_from(EXECUTION_STREAM_HEARTBEAT.as_millis()).unwrap_or(u64::MAX),
    };
    // THE LAST POSITIONAL FRAME.
    if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
        || write_reply_as(stream, codec, &reply).is_err()
    {
        return ExecutionStreamEnd::Closed;
    }
    tracing::info!(
        journal_id = %opening.journal_id,
        tradehub = %opening.tradehub.escape_debug(),
        ?peer,
        stored_through = ?cursor.stored_through,
        oldest_sequence = opening.oldest_sequence,
        head_sequence = opening.head_sequence,
        first_contact_through = cursor.first_contact_through,
        pinned = ?cursor.pinned,
        "vike-datahub: execution stream opened"
    );
    let mut session = Session {
        lane,
        store,
        stream,
        codec,
        live,
        identity,
        expected_previous: cursor.stored_through,
        cursor,
        awaiting_resend: false,
        buffer: Vec::new(),
        first_buffered_at: None,
        failures: FlushFailures::default(),
        forecasts_held_back: false,
    };
    let why = session.run(idle);
    tracing::info!(
        journal_id = %session.identity.journal_id,
        ?peer,
        stored_through = ?session.cursor.stored_through,
        why,
        "vike-datahub: execution stream closed"
    );
    ExecutionStreamEnd::Closed
}

/// The session must end now, and why: a test's crash point, a newer open, a socket that will not take a write.
struct Halt(&'static str);

/// Why one try of a flush step failed.
enum StepError {
    /// Retried after [`RETRY_EVERY`], logged through [`FlushFailures`]' bound.
    Failed(FlushStage, String),
    /// A test's crash point fired: nothing more is written.
    Died,
}

struct Session<'a> {
    lane: &'a ExecutionStreamLane,
    store: &'a (dyn HistoryStore + Send + Sync),
    stream: &'a mut TcpStream,
    /// The form of the open, every down frame's.
    codec: BodyCodec,
    live: &'a LiveStream,
    identity: ExecutionStreamIdentity,
    cursor: StreamCursor,
    /// The `previous_sequence` the next record must carry: `None` (the journal's start) only while nothing is stored
    /// and nothing is buffered.
    expected_previous: Option<u64>,
    /// A `Resend` is out: records that do not chain from `expected_previous` are in flight and dropped unanswered.
    awaiting_resend: bool,
    buffer: Vec<ExecutionRecordWire>,
    first_buffered_at: Option<Instant>,
    failures: FlushFailures,
    forecasts_held_back: bool,
}

impl Session<'_> {
    /// The frame loop. Returns why it ended.
    fn run(&mut self, idle: Duration) -> &'static str {
        let mut last_frame = Instant::now();
        loop {
            if self.live.superseded.load(Ordering::Relaxed) {
                return "a newer open of this journal superseded it";
            }
            // Wait for the next frame WITHOUT reading it, so a timeout never cuts a frame in half: a peek returns as
            // soon as a byte is there, and the whole frame is then read under the idle bound.
            let wait = match (self.first_buffered_at, self.cursor.pinned) {
                (Some(at), None) => FLUSH_EVERY.saturating_sub(at.elapsed()),
                _ => FLUSH_EVERY,
            };
            if self.stream.set_read_timeout(Some(wait.max(Duration::from_millis(1)))).is_err() {
                return "the socket refused a read timeout";
            }
            match self.stream.peek(&mut [0u8; 1]) {
                Ok(0) => {
                    // The tradehub left: what it sent is contiguous and stored before the session ends.
                    let _ = self.flush_contiguous();
                    return "the tradehub closed the stream";
                }
                Ok(_) => {}
                Err(e)
                    if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
                {
                    if let Err(Halt(why)) = self.tick() {
                        return why;
                    }
                    if last_frame.elapsed() > idle {
                        let _ = self.flush_contiguous();
                        return "no frame within the idle read timeout";
                    }
                    continue;
                }
                Err(_) => return "the socket failed",
            }
            if self.stream.set_read_timeout(Some(idle)).is_err() {
                return "the socket refused a read timeout";
            }
            let body = match read_frame_raw_capped(
                &mut *self.stream,
                EXECUTION_STREAM_FRAME_CEILING_BYTES,
            ) {
                Ok(body) => body,
                Err(_) => {
                    return "a frame could not be read (over the ceiling, cut, or the socket failed)";
                }
            };
            last_frame = Instant::now();
            let up = match decode_body::<ExecutionStreamUp>(&body) {
                Ok(up) => up,
                Err(_) => return "a frame that is no execution-stream frame",
            };
            let handled = match up {
                ExecutionStreamUp::Records(records) => {
                    if records.len() > EXECUTION_STREAM_RECORDS_PER_FRAME {
                        return "a frame over EXECUTION_STREAM_RECORDS_PER_FRAME records";
                    }
                    self.records(records)
                }
                ExecutionStreamUp::Gap {
                    after_sequence,
                    resumes_at_sequence,
                    lost_from_ms,
                    lost_to_ms,
                } => self.gap(after_sequence, resumes_at_sequence, lost_from_ms, lost_to_ms),
                ExecutionStreamUp::Heartbeat { head_sequence } => self.heartbeat(head_sequence),
            };
            if let Err(Halt(why)) = handled {
                return why;
            }
        }
    }

    /// No frame arrived: a buffered window is due once its first record is [`FLUSH_EVERY`] old.
    fn tick(&mut self) -> Result<(), Halt> {
        let due = self.first_buffered_at.is_some_and(|at| at.elapsed() >= FLUSH_EVERY);
        if due && self.cursor.pinned.is_none() { self.flush_all() } else { Ok(()) }
    }

    fn records(&mut self, records: Vec<ExecutionRecordWire>) -> Result<(), Halt> {
        for record in records {
            if self.awaiting_resend {
                if record.previous_sequence != self.expected_previous {
                    continue; // sent before the tradehub read the Resend
                }
                self.awaiting_resend = false;
            }
            // A `None` chains only from `None`: the start, taken while nothing of the journal is stored.
            if record.previous_sequence != self.expected_previous
                || record.previous_sequence.is_some_and(|p| record.sequence <= p)
            {
                return self.resend();
            }
            if let Some(pin) = self.cursor.pinned
                && record.sequence > pin.through
            {
                self.flush_pinned()?;
            }
            self.expected_previous = Some(record.sequence);
            let ends_the_pin = self.cursor.pinned.is_some_and(|p| record.sequence == p.through);
            self.buffer.push(record);
            self.first_buffered_at.get_or_insert_with(Instant::now);
            if ends_the_pin {
                self.flush_pinned()?;
            } else if self.buffer.len() >= FLUSH_RECORDS {
                if self.cursor.pinned.take().is_some() {
                    tracing::warn!(
                        journal_id = %self.identity.journal_id,
                        "vike-datahub: the window a crash left half written is not being resent as it was (this \
                         journal is not the one it was written from); it is written as a new window"
                    );
                }
                self.flush_all()?;
            }
        }
        Ok(())
    }

    /// The first gap in the chain: store what is contiguous, then "everything since" the cursor.
    fn resend(&mut self) -> Result<(), Halt> {
        self.flush_contiguous()?;
        self.expected_previous = self.cursor.stored_through;
        self.awaiting_resend = true;
        tracing::info!(
            journal_id = %self.identity.journal_id,
            after_sequence = ?self.cursor.stored_through,
            "vike-datahub: a streamed record does not chain from the last one received: asking for everything since"
        );
        self.send(ExecutionStreamDown::Resend { after_sequence: self.cursor.stored_through })
    }

    fn heartbeat(&mut self, head_sequence: u64) -> Result<(), Halt> {
        if self.awaiting_resend {
            return self
                .send(ExecutionStreamDown::Resend { after_sequence: self.expected_previous });
        }
        // An idle tradehub has sent everything durable: a pinned window whose last record never came is not this
        // journal's to rewrite.
        if self.cursor.pinned.take().is_some() {
            tracing::warn!(
                journal_id = %self.identity.journal_id,
                head_sequence,
                "vike-datahub: the window a crash left half written was not resent as it was (this journal is not \
                 the one it was written from); what was resent is written as a new window"
            );
        }
        self.flush_all()
    }

    fn gap(
        &mut self,
        after_sequence: u64,
        resumes_at_sequence: u64,
        lost_from_ms: Option<i64>,
        lost_to_ms: Option<i64>,
    ) -> Result<(), Halt> {
        if self.awaiting_resend && Some(after_sequence) != self.expected_previous {
            return Ok(());
        }
        if Some(after_sequence) != self.expected_previous || resumes_at_sequence <= after_sequence {
            return self.resend();
        }
        self.awaiting_resend = false;
        self.flush_contiguous()?;
        tracing::error!(
            journal_id = %self.identity.journal_id,
            after_sequence,
            resumes_at_sequence,
            ?lost_from_ms,
            ?lost_to_ms,
            "vike-datahub: the tradehub's journal no longer holds executions this datahub never stored (pruned past \
             its hold): they are LOST from the store, and the gap is recorded"
        );
        let row = ExecutionStreamGapRow {
            journal_id: self.identity.journal_id.clone(),
            kind: GAP_PRUNED.to_string(),
            after_sequence,
            resumes_at_sequence,
            lost_from_ms,
            lost_to_ms,
            noted_ms: vike_model::now_ms(),
            reason: None,
            order_client_ids: Vec::new(),
        };
        let gap_ends_the_pin = self.cursor.pinned.is_some_and(|p| resumes_at_sequence > p.through);
        if gap_ends_the_pin {
            self.cursor.pinned = None;
        }
        let past = resumes_at_sequence - 1;
        let moves = self.cursor.pinned.is_none() && Some(past) > self.cursor.stored_through;
        self.retrying(|session| {
            record_execution_gap(&session.lane.settings_dir, &row)
                .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            if moves {
                end_execution_window(
                    &session.lane.settings_dir,
                    &session.identity,
                    past,
                    None,
                    vike_model::now_ms(),
                )
                .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            }
            Ok(())
        })?;
        if moves {
            self.cursor.stored_through = Some(past);
            self.cursor.recorded_ms = None;
            return self.send(ExecutionStreamDown::Stored { through_sequence: past });
        }
        Ok(())
    }

    /// Store what the buffer holds when it is a window of its own: a pinned window is written only whole.
    fn flush_contiguous(&mut self) -> Result<(), Halt> {
        match self.cursor.pinned {
            None => self.flush_all(),
            Some(_) => {
                self.buffer.clear();
                self.first_buffered_at = None;
                Ok(())
            }
        }
    }

    fn flush_pinned(&mut self) -> Result<(), Halt> {
        match self.cursor.pinned.map(|p| p.through).or(self.cursor.stored_through) {
            Some(through) => self.flush_window(through),
            None => Ok(()), // nothing pinned and nothing stored: no window ends here
        }
    }

    fn flush_all(&mut self) -> Result<(), Halt> {
        match self.buffer.last().map(|r| r.sequence) {
            Some(through) => self.flush_window(through),
            None => Ok(()),
        }
    }

    /// Write the window `(window_after, through]` of the buffer, retried until it lands, then acknowledge it.
    fn flush_window(&mut self, through: u64) -> Result<(), Halt> {
        let take = self.buffer.iter().take_while(|r| r.sequence <= through).count();
        let records: Vec<ExecutionRecordWire> = self.buffer.drain(..take).collect();
        self.first_buffered_at = (!self.buffer.is_empty()).then(Instant::now);
        let after = self.cursor.window_after();
        if after.is_some_and(|a| through <= a) {
            self.cursor.pinned = None;
            return Ok(());
        }
        let recorded_ms = records.last().filter(|r| r.sequence == through).map(|r| r.recorded_ms);
        let (first_contact_through, generation) =
            (self.cursor.first_contact_through, self.cursor.generation);
        let mut written = None;
        self.retrying(|session| {
            let lane = session.lane;
            let now = vike_model::now_ms();
            begin_execution_window(&lane.settings_dir, &session.identity, through)
                .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            if lane.fault(ExecutionStreamFaultPoint::AfterWritingThrough) {
                return Err(StepError::Died);
            }
            let window = sink::Window {
                journal_id: &session.identity.journal_id,
                after,
                through,
                records: &records,
                first_contact_through,
                generation,
            };
            let floor = lane.forecast_floor.as_deref();
            let fault = |p: ExecutionStreamFaultPoint| lane.fault(p);
            let out = sink::write_window(session.store, &window, now, floor, &fault).map_err(
                |e| match e {
                    sink::SinkError::Store(e) => {
                        StepError::Failed(FlushStage::Append, e.to_string())
                    }
                    sink::SinkError::Died => StepError::Died,
                },
            )?;
            for gap in &out.unverified {
                record_execution_gap(&lane.settings_dir, gap)
                    .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            }
            for positions in &out.account_positions {
                record_account_positions(&lane.settings_dir, positions)
                    .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            }
            if lane.fault(ExecutionStreamFaultPoint::BeforeStoredThrough) {
                return Err(StepError::Died);
            }
            end_execution_window(&lane.settings_dir, &session.identity, through, recorded_ms, now)
                .map_err(|e| StepError::Failed(FlushStage::Cursor, e.to_string()))?;
            written = Some(out);
            Ok(())
        })?;
        let out = written.unwrap_or_default();
        self.note(&out);
        self.cursor.stored_through = Some(through);
        self.cursor.recorded_ms = recorded_ms;
        self.cursor.pinned = None;
        self.send(ExecutionStreamDown::Stored { through_sequence: through })
    }

    /// Run `step` until it succeeds, every [`RETRY_EVERY`], reading no frame meanwhile; a crash point or a newer open
    /// ends it.
    fn retrying(
        &mut self,
        mut step: impl FnMut(&mut Self) -> Result<(), StepError>,
    ) -> Result<(), Halt> {
        loop {
            match step(self) {
                Ok(()) => {
                    if let Some(event) = self.failures.landed(Instant::now()) {
                        failures::log(&self.identity.journal_id, event, "");
                    }
                    return Ok(());
                }
                Err(StepError::Died) => return Err(Halt("a test's crash point")),
                Err(StepError::Failed(stage, error)) => {
                    if let Some(event) = self.failures.failed(stage, Instant::now()) {
                        failures::log(&self.identity.journal_id, event, &error);
                    }
                    std::thread::sleep(RETRY_EVERY);
                    if self.live.superseded.load(Ordering::Relaxed) {
                        return Err(Halt("a newer open of this journal superseded it"));
                    }
                }
            }
        }
    }

    /// What a window skipped by rule, said once per window that skipped something, and the floor's two edges.
    fn note(&mut self, out: &sink::WindowWritten) {
        let c = out.counts;
        if c != sink::SinkCounts::default() {
            tracing::info!(
                journal_id = %self.identity.journal_id,
                unnamed = c.unnamed,
                already_stored = c.already_stored,
                venue_order_id_unknown = c.venue_order_id_unknown,
                forecasts_held_back = c.forecasts_held_back,
                "vike-datahub: an execution-stream window skipped records by rule"
            );
        }
        match (&out.forecasts_held_back, self.forecasts_held_back) {
            (Some(why), false) => tracing::warn!(
                journal_id = %self.identity.journal_id,
                "vike-datahub: the live forecasts are NOT written into kind=exec_forecast (fills and orders are): {why}"
            ),
            (None, true) if c.forecasts_held_back == 0 => tracing::info!(
                journal_id = %self.identity.journal_id,
                "vike-datahub: forecasts are written again"
            ),
            _ => {}
        }
        self.forecasts_held_back = out.forecasts_held_back.is_some();
    }

    fn send(&mut self, down: ExecutionStreamDown) -> Result<(), Halt> {
        write_reply_as(&mut *self.stream, self.codec, &down)
            .map_err(|_| Halt("a down frame could not be written"))
    }
}
