//! **One CONNECTION of the execution stream**: dial the datahub, open one journal's stream, check the datahub's
//! cursor against the journal (a copy, a gap), then send what is durable and fold the acknowledgements until the stop,
//! a drained moved-aside journal, or an error, which drops the connection (the caller redials).
//!
//! - **Sequences.** Each record carries its journal sequence and the sequence of the record sent before it; the first
//!   after the open (or after a `Resend { after_sequence: N }`) chains from the datahub's `N`. While the datahub holds
//!   nothing of the journal its `N` is `None`: the read cursor is `None` and the first record sent, a fresh journal's
//!   record 0 included, chains from `None`, the chain's start (`vike_datahub_client::execution_stream`'s module doc).
//! - **Bounded ahead.** At most [`MAX_UNACKNOWLEDGED`] records are out without an acknowledgement, so a datahub that
//!   reads and never acknowledges is sent no more, and the memory of what is in flight stays bounded.
//! - **One frame at a time, each under the ceiling.** A `Records` frame ends at
//!   `EXECUTION_STREAM_RECORDS_PER_FRAME` records or before the record that would cross
//!   `EXECUTION_STREAM_FRAME_CEILING_BYTES`. A record that alone would cross it cannot be sent at all (the datahub
//!   closes the connection on it): it is said at ERROR and left out of the chain.

use std::collections::VecDeque;
use std::io;
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use vike_datahub_client::DatahubClient;
use vike_datahub_client::execution_stream::{
    ExecutionRecordWire, ExecutionStreamDown, ExecutionStreamUp,
};
use vike_datahub_client::proto::{
    EXECUTION_STREAM_FRAME_CEILING_BYTES, EXECUTION_STREAM_HEARTBEAT,
    EXECUTION_STREAM_RECORDS_PER_FRAME,
};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::frame::{BodyCodec, encode_frame, read_frame, write_frame_as};
use vike_journal::{JournalExecutionStream, record_now_ms, record_sequence};

use super::Publisher;
use super::read::{JournalRecords, READ_LIMIT, body_of};

/// How long the open's one reply may take (the datahub reads one cursor row).
const OPEN_REPLY: Duration = Duration::from_secs(30);

/// The most records sent and not yet acknowledged. The datahub stores a window at 1024 records or a second after its
/// first, so four windows in flight keep it busy.
const MAX_UNACKNOWLEDGED: usize = 4096;

/// How often the floor in the stream file is rewritten while acknowledgements arrive.
const FLOOR_EVERY: Duration = Duration::from_secs(1);

/// How often the journal's sync thread is asked to sync while the publisher has read all that is durable.
const SYNC_REQUEST_EVERY: Duration = Duration::from_secs(1);

/// What a `Records` frame keeps for its envelope under the ceiling.
const FRAME_ENVELOPE_BYTES: usize = 4096;

/// How long a down frame the datahub has started to send may take to arrive whole.
const DOWN_FRAME_READ: Duration = Duration::from_secs(10);

/// How a stream ended without an error.
pub(super) enum Ended {
    /// The stop was raised (after the final send).
    Stopped,
    /// A moved-aside journal: the datahub holds all of it.
    Drained,
}

/// Why a pass stopped short of the stop.
pub(super) enum Pause {
    /// The datahub does not advertise the stream: asked again after `unsupported_wait`.
    Unsupported(String),
    /// The datahub follows a copy of the journal, which has just taken a new identity: reopen at once.
    Diverged,
    /// Anything else, in words; `sent` when this connection got records to the datahub (the backoff starts over).
    Failed { why: String, sent: bool },
}

impl Publisher<'_> {
    /// Connect to the datahub under the Trade key: the first step of every connection, taken BEFORE any journal is
    /// read, so a datahub that is down costs no read.
    pub(super) fn dial(&self) -> Result<DatahubClient, Pause> {
        let addr = self.source.datahub_addr.as_str();
        let keys = (self.source.keys)();
        DatahubClient::connect_with_keys(addr, keys.as_ref(), Scope::Trade)
            .map_err(|e| Pause::Failed { why: format!("datahub {addr}: {e}"), sent: false })
    }

    /// Stream `journal` under `journal_id` over the connection `client` [dialled](Self::dial). `file` is the open
    /// journal's stream file (its floor is written, its identity checked); `None` for a moved-aside journal, which
    /// returns [`Ended::Drained`] once the datahub holds its last record.
    pub(super) fn stream(
        &mut self,
        client: DatahubClient,
        journal: &JournalRecords<'_>,
        journal_id: &str,
        mut file: Option<&mut JournalExecutionStream>,
    ) -> Result<Ended, Pause> {
        let source = self.source;
        let addr = source.datahub_addr.as_str();
        let lost =
            |e: io::Error, sent: bool| Pause::Failed { why: format!("datahub {addr}: {e}"), sent };
        let unreadable = |e: io::Error| Pause::Failed {
            why: format!("the journal of execution stream {journal_id} does not read: {e}"),
            sent: false,
        };
        let live = journal.end().is_none();
        let oldest = journal.oldest().map_err(unreadable)?;
        let head = journal
            .head(if live { self.head } else { None }, &mut self.sessions)
            .map_err(unreadable)?;
        if live {
            self.head = head;
        }
        let (info, mut socket) = client
            .open_execution_stream(
                journal_id,
                &source.tradehub,
                oldest.map_or(0, |(s, _)| s),
                head.unwrap_or(0),
                OPEN_REPLY,
                source.timings.write_timeout,
            )
            .map_err(|e| match e {
                HubCallError::NotOffered { .. } => {
                    Pause::Unsupported(format!("datahub {addr}: {e}"))
                }
                _ => Pause::Failed { why: format!("datahub {addr}: {e}"), sent: false },
            })?;
        let stored = info.stored_through;
        // Every up frame in the form this connection negotiated, as the datahub writes the down ones.
        let codec = info.codec;
        if let Some(file) = file.as_deref_mut()
            && diverged(journal, stored, info.stored_through_recorded_ms, head)
                .map_err(unreadable)?
        {
            let dir = source.journal_dir.as_path();
            let fresh = JournalExecutionStream::mint_journal_id();
            tracing::error!(
                old_journal_id = %journal_id,
                journal_id = %fresh,
                stored_through = ?stored,
                recorded_ms = ?info.stored_through_recorded_ms,
                head = ?head,
                "execution stream: the datahub follows ANOTHER copy of this journal (its cursor is not this journal's \
                 record), so this journal takes a new identity and is sent again from its first record; the datahub \
                 drops what it already holds"
            );
            file.journal_id = fresh;
            file.acknowledged_sequence = None;
            file.acknowledged_ms = None;
            file.store(dir).map_err(|e| Pause::Failed {
                why: format!("the journal's stream file in {}: {e}", dir.display()),
                sent: false,
            })?;
            return Err(Pause::Diverged);
        }
        if self.last_failure.take().is_some() {
            tracing::info!(datahub = %addr, "execution stream: the datahub is reached again");
        }
        tracing::info!(
            journal_id,
            stored_through = ?stored,
            oldest = ?oldest.map(|(s, _)| s),
            head = ?head,
            "execution stream: opened"
        );
        // a gap follows something stored; from nothing stored the stream starts at the oldest record it holds
        if let Some(stored) = stored
            && let Some((oldest_sequence, oldest_ms)) = oldest
            && stored + 1 < oldest_sequence
        {
            tracing::error!(
                journal_id,
                after_sequence = stored,
                resumes_at_sequence = oldest_sequence,
                "execution stream: the journal no longer holds executions the datahub never stored (pruned past the \
                 hold): they are LOST from the datahub, and the gap is sent"
            );
            let gap = ExecutionStreamUp::Gap {
                after_sequence: stored,
                resumes_at_sequence: oldest_sequence,
                lost_from_ms: info.stored_through_recorded_ms,
                lost_to_ms: Some(oldest_ms),
            };
            write_frame_as(&mut socket, codec, &gap).map_err(|e| lost(e, false))?;
        }

        let timings = source.timings;
        let heartbeat = match info.heartbeat_ms {
            0 => EXECUTION_STREAM_HEARTBEAT,
            ms => Duration::from_millis(ms),
        };
        // the read position, and the sequence the next record chains from
        let mut cursor = stored;
        let mut previous = stored;
        let mut acknowledged = cursor;
        let mut acknowledged_ms = info.stored_through_recorded_ms;
        let mut floor_dirty =
            file.as_deref().is_some_and(|f| f.acknowledged_sequence != acknowledged);
        let mut floor_written = Instant::now();
        let mut floor_failed_said = false;
        let mut unacknowledged: VecDeque<(u64, i64)> = VecDeque::new();
        let mut sent = false;
        let mut last_up = Instant::now();
        let mut sync_asked: Option<Instant> = None;
        let mut stopping: Option<Instant> = None;
        loop {
            if stopping.is_none() && self.stopped() {
                let _ = socket.set_write_timeout(Some(timings.final_send));
                stopping = Some(Instant::now() + timings.final_send);
            }
            // 1. send what is durable, up to the unacknowledged bound
            let room = MAX_UNACKNOWLEDGED.saturating_sub(unacknowledged.len()).min(READ_LIMIT);
            let mut more = false;
            if room > 0 {
                let batch = journal.read(cursor, room).map_err(unreadable)?;
                more = batch.len() == room;
                let mut records = Vec::new();
                for record in batch.iter() {
                    self.sessions.note(record);
                    let sequence = record_sequence(record);
                    cursor = Some(sequence);
                    let Some(body) = body_of(record, &mut self.mounts)
                        .map_err(|why| Pause::Failed { why, sent })?
                    else {
                        continue;
                    };
                    let recorded_ms = record_now_ms(record);
                    let wire = ExecutionRecordWire {
                        sequence,
                        previous_sequence: previous,
                        recorded_ms,
                        body,
                    };
                    if !fits_a_frame(&wire, codec) {
                        tracing::error!(
                            journal_id,
                            sequence,
                            "execution stream: a record too large for any frame is left out of the stream"
                        );
                        continue;
                    }
                    unacknowledged.push_back((sequence, recorded_ms));
                    previous = Some(sequence);
                    records.push(wire);
                }
                if live {
                    self.head = self.head.max(cursor);
                }
                if !records.is_empty() {
                    send_records(&mut socket, codec, records).map_err(|e| lost(e, sent))?;
                    sent = true;
                    last_up = Instant::now();
                }
            }
            // 2. the datahub's answers
            let wait = if more { Duration::from_millis(1) } else { timings.ack_poll };
            for down in read_down(&mut socket, wait).map_err(|e| lost(e, sent))? {
                match down {
                    ExecutionStreamDown::Stored { through_sequence } => {
                        while let Some(&(s, ms)) = unacknowledged.front()
                            && s <= through_sequence
                        {
                            unacknowledged.pop_front();
                            acknowledged_ms = Some(ms);
                        }
                        if acknowledged.is_none_or(|a| through_sequence > a) {
                            acknowledged = Some(through_sequence);
                            floor_dirty = true;
                        }
                    }
                    ExecutionStreamDown::Resend { after_sequence } => {
                        cursor = after_sequence;
                        previous = after_sequence;
                        unacknowledged.clear();
                    }
                }
            }
            // 3. the floor of the open journal
            if let Some(file) = file.as_deref_mut()
                && floor_dirty
                && (floor_written.elapsed() >= FLOOR_EVERY || stopping.is_some())
            {
                match store_floor(file, &source.journal_dir, acknowledged, acknowledged_ms) {
                    Ok(()) => floor_dirty = false,
                    Err(e) if !floor_failed_said => {
                        floor_failed_said = true;
                        tracing::warn!(
                            dir = %source.journal_dir.display(),
                            error = %e,
                            "execution stream: the journal's floor could not be written; the journal keeps more \
                             than it needs to until it is"
                        );
                    }
                    Err(_) => {}
                }
                floor_written = Instant::now();
            }
            if live {
                self.position = if unacknowledged.is_empty() { cursor } else { acknowledged };
                self.observe_lag();
            }
            // 4. a moved-aside journal is drained once the datahub holds its last record
            if let Some(end) = journal.end()
                && unacknowledged.is_empty()
                && end.is_none_or(|e| cursor.is_some_and(|c| c >= e))
            {
                return Ok(Ended::Drained);
            }
            // 5. all that is durable is read: ask for a sync, and keep the datahub's read alive
            if !more {
                if let Some(watch) = journal.watch()
                    && sync_asked.is_none_or(|t| t.elapsed() >= SYNC_REQUEST_EVERY)
                {
                    watch.request_sync();
                    sync_asked = Some(Instant::now());
                }
                if last_up.elapsed() >= heartbeat {
                    let beat = ExecutionStreamUp::Heartbeat { head_sequence: cursor.unwrap_or(0) };
                    write_frame_as(&mut socket, codec, &beat).map_err(|e| lost(e, sent))?;
                    last_up = Instant::now();
                }
            }
            // 6. the stop: what was durable is sent and stored, or the final send's bound is spent
            if let Some(deadline) = stopping
                && ((!more && unacknowledged.is_empty()) || Instant::now() >= deadline)
            {
                if let Some(file) = file.as_deref_mut()
                    && floor_dirty
                {
                    let _ = store_floor(file, &source.journal_dir, acknowledged, acknowledged_ms);
                }
                return Ok(Ended::Stopped);
            }
        }
    }
}

/// **Does the datahub follow a COPY of this journal?** Its cursor is past the journal's newest record, or the
/// journal's record at the cursor was recorded at another time than the datahub stored (a directory restored from a
/// copy, the rollback runbook's `cp -a`, carries the id and diverges). A cursor the journal has pruned is no copy (that
/// is the gap).
fn diverged(
    journal: &JournalRecords<'_>,
    stored: Option<u64>,
    recorded_ms: Option<i64>,
    head: Option<u64>,
) -> io::Result<bool> {
    let Some(stored) = stored else { return Ok(false) };
    if head.is_none_or(|h| stored > h) {
        return Ok(true);
    }
    // the record AT `stored`: everything after the one before it (record 0 has none before it)
    let at = journal.read(stored.checked_sub(1), 1)?;
    Ok(match (at.first(), recorded_ms) {
        (Some(record), Some(ms)) if record_sequence(record) == stored => {
            record_now_ms(record) != ms
        }
        _ => false,
    })
}

/// The bytes one `Records` frame may hold.
const FRAME_BUDGET: usize = EXECUTION_STREAM_FRAME_CEILING_BYTES as usize;

/// `record`'s bytes in the form it is written in, counted plainly (an LZ4 body is its msgpack compressed, within
/// the envelope's slack): a short decimal is longer in msgpack than in JSON. `None`: it does not encode.
fn body_len(record: &ExecutionRecordWire, codec: BodyCodec) -> Option<usize> {
    let plain = if codec == BodyCodec::Json { BodyCodec::Json } else { BodyCodec::Msgpack };
    encode_frame(plain, record).ok().map(|frame| frame.len() - 4)
}

fn fits_a_frame(record: &ExecutionRecordWire, codec: BodyCodec) -> bool {
    body_len(record, codec).is_some_and(|n| n + FRAME_ENVELOPE_BYTES <= FRAME_BUDGET)
}

/// `records` as `Records` frames, each at most `EXECUTION_STREAM_RECORDS_PER_FRAME` records and under the ceiling.
fn send_records(
    socket: &mut TcpStream,
    codec: BodyCodec,
    records: Vec<ExecutionRecordWire>,
) -> io::Result<()> {
    let mut frame: Vec<ExecutionRecordWire> = Vec::new();
    let mut bytes = FRAME_ENVELOPE_BYTES;
    for record in records {
        // one comma between records in JSON; at most that per element in msgpack
        let size = body_len(&record, codec).unwrap_or(usize::MAX).saturating_add(1);
        if !frame.is_empty()
            && (frame.len() == EXECUTION_STREAM_RECORDS_PER_FRAME || bytes + size > FRAME_BUDGET)
        {
            write_frame_as(socket, codec, &ExecutionStreamUp::Records(std::mem::take(&mut frame)))?;
            bytes = FRAME_ENVELOPE_BYTES;
        }
        bytes += size;
        frame.push(record);
    }
    if frame.is_empty() {
        return Ok(());
    }
    write_frame_as(socket, codec, &ExecutionStreamUp::Records(frame))
}

/// Every down frame the datahub has sent, waiting up to `wait` for the first. A frame is read only once a byte of it
/// is there, so a timeout never cuts one in half.
fn read_down(socket: &mut TcpStream, wait: Duration) -> io::Result<Vec<ExecutionStreamDown>> {
    let mut downs = Vec::new();
    let mut wait = wait.max(Duration::from_millis(1));
    loop {
        socket.set_read_timeout(Some(wait))?;
        match socket.peek(&mut [0u8; 1]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the datahub closed the execution stream",
                ));
            }
            Ok(_) => {
                socket.set_read_timeout(Some(DOWN_FRAME_READ))?;
                downs.push(read_frame(socket)?);
                wait = Duration::from_millis(1);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Ok(downs);
            }
            Err(e) => return Err(e),
        }
    }
}

/// The datahub's acknowledgement as the journal's prune floor (`crates/vike-journal/src/writer/offline.rs`'s
/// `prune_before_latest_snap` keeps every segment past it), written only when it moved.
fn store_floor(
    file: &mut JournalExecutionStream,
    dir: &Path,
    sequence: Option<u64>,
    ms: Option<i64>,
) -> io::Result<()> {
    if file.acknowledged_sequence == sequence {
        return Ok(());
    }
    file.acknowledged_sequence = sequence;
    file.acknowledged_ms = ms;
    file.store(dir)
}
