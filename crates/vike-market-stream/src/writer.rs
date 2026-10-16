//! The stream session's WRITER thread: the only thread that writes to the session's socket after the
//! `DataOpened` reply, and the only one that serializes a frame.
//!
//! It drains `crate::sink::WireSink::next`: frames in emission order, the `Gap`s the queue owes
//! in front of them, a `Heartbeat` after the stated period of silence or on a client `Ping`, and the
//! closing `Bye`. A write that blocks past the socket's write timeout (a peer that has stopped reading)
//! or fails ends the session; nothing on this thread is ever waited for by a venue pump.
//!
//! # Lapses
//!
//! A LAPSE is one overflow EPISODE the reader lost frames in, not one `Gap` frame: ONE overflow holes
//! every busy `(key, verb)` at once (three hot symbols times three lossless verbs is nine `Gap`s), and
//! the queue discloses each at the first survivor of its own lane, so the `Gap`s of one overflow arrive
//! as a burst. An EPISODE is the run of disclosures that start inside `lapse_episode` of the FIRST one
//! (an anchored window, not a sliding one: a laggard that never recovers still opens a new episode
//! every `lapse_episode`). Every `Gap` is still written, and so is its `GapStart` at the receiver;
//! only the COUNT changes. More than `lapse_budget` episodes inside `lapse_window` ends the session
//! with `ByeReason::TooSlow` (after the gap that opened the episode that crossed the budget), and the
//! datahub's client reconnects and re-seeds. A session that keeps losing frames is being lied to more
//! slowly, so it is cut off rather than served an ever-staler stream. (Counting per `Gap` made one
//! overflow across three symbols a session close: `lapses=9` against a budget of 8.)
//!
//! # Batching
//!
//! The writer encodes frames into ONE buffer and writes the buffer with ONE `write_all` (one `sendto`)
//! instead of a write per frame: on the 2026-10-14 the CI box box the per-frame syscall was 54% of the
//! tube's CPU. The bytes are the same length-prefixed frames in the same order, `Gap`s and the closing
//! `Bye` included. A batch ends when the queue runs DRY (the writer never waits for more frames: the
//! last frame queued is written at once), or at `BATCH_FRAMES` frames, `BATCH_BYTES` bytes, or when its
//! oldest frame has waited `BATCH_AGE`; so a frame is delayed by at most the time to encode the frames in
//! front of it in its batch. A write fault now loses the batch, not one frame: the session ends either
//! way and the receiver discloses every held key as a `GapStart`.
//!
//! # A frame the writer cannot send
//!
//! A frame is serialized HERE, so this is where a frame over `GW_DOWN_FRAME_CEILING_BYTES` is found.
//! The client would treat a longer frame as a framing fault that kills the session, so it is not sent:
//! a lossless frame is taken back from the delivery books and becomes a hole (`Queue::note_unsendable`),
//! disclosed like any other; the incident is logged at `error`.

use std::io::{self, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use serde::Serialize;
use vike_marketdata::L2Book;

use crate::frame::encode_capped_into;
use crate::queue::{Item, Payload, Taken};
use crate::sink::{Next, WireSink};
use crate::wire::{ByeReason, StreamDown};

/// What the writer is configured with.
#[derive(Debug, Clone, Copy)]
pub struct WriterConfig {
    /// The heartbeat period stated in `DataOpened`: a `Heartbeat` after this much silence.
    pub heartbeat: Duration,
    /// Overflow EPISODES allowed inside `lapse_window` (see the module doc).
    pub lapse_budget: u64,
    /// The window the budget is counted in.
    pub lapse_window: Duration,
    /// How long after the first `Gap` of an episode further `Gap`s still belong to it.
    pub lapse_episode: Duration,
    /// The longest frame the peer reads (the gatewayhub's `GW_DOWN_FRAME_CEILING_BYTES`): a frame over it
    /// is not sent.
    pub down_frame_ceiling_bytes: u32,
}

/// The most frames one batch carries before it is written.
const BATCH_FRAMES: usize = 64;
/// The most bytes one batch carries before it is written (a frame that crosses it ends the batch).
const BATCH_BYTES: usize = 64 * 1024;
/// The longest the first frame of a batch waits for the batch to be written, however many frames are
/// still queued behind it.
const BATCH_AGE: Duration = Duration::from_micros(250);
/// A buffer that a very large frame grew past this is replaced by a small one after the write, so
/// an idle session does not pin the biggest frame it ever sent.
const BATCH_KEEP_BYTES: usize = 4 * BATCH_BYTES;

/// The frames the writer has encoded and not yet written: ONE `write_all` per batch instead of one
/// per frame. See the module doc's "Batching".
struct Batch<W: Write> {
    out: W,
    buf: Vec<u8>,
    frames: usize,
    /// When the oldest buffered frame was encoded.
    since: Option<Instant>,
    ceiling: u32,
}

impl<W: Write> Batch<W> {
    fn new(out: W, ceiling: u32) -> Self {
        Self { out, buf: Vec::with_capacity(BATCH_BYTES + 4096), frames: 0, since: None, ceiling }
    }

    fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Encode `frame` after the buffered ones. `InvalidData` (over the ceiling) adds nothing.
    fn put(&mut self, frame: &StreamDown) -> io::Result<()> {
        self.put_encoded(frame)
    }

    /// A folded book as the `StreamDown::Book` frame it is, serialized from the queue's `Arc` itself
    /// ([`BookRef`]): `Item::into_frame` deep-copies the whole book into a `Box` just to serialize it.
    fn put_book(&mut self, k: u32, seq: u64, book: &L2Book) -> io::Result<()> {
        self.put_encoded(&BookRef::Book { k, seq, book })
    }

    fn put_encoded(&mut self, frame: &impl Serialize) -> io::Result<()> {
        encode_capped_into(&mut self.buf, frame, self.ceiling)?;
        self.frames += 1;
        self.since.get_or_insert_with(Instant::now);
        Ok(())
    }

    /// Whether the batch must be written now, though more may be queued: it is as big as it may be, or
    /// its oldest frame has waited `BATCH_AGE`.
    fn full(&self, now: Instant) -> bool {
        self.frames >= BATCH_FRAMES
            || self.buf.len() >= BATCH_BYTES
            || self.since.is_some_and(|s| now.saturating_duration_since(s) >= BATCH_AGE)
    }

    /// Write what is buffered, in one `write_all`. The buffer is emptied whatever the outcome: after a
    /// fault the session is over and a retry could only write half a batch twice.
    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let result = self.out.write_all(&self.buf).and_then(|()| self.out.flush());
        self.buf.clear();
        if self.buf.capacity() > BATCH_KEEP_BYTES {
            self.buf = Vec::with_capacity(BATCH_BYTES + 4096);
        }
        self.frames = 0;
        self.since = None;
        result
    }
}

/// `StreamDown::Book` over a borrowed book: the same externally tagged JSON, byte for byte
/// (`writer_tests.rs`'s `a_book_frame_serialized_by_reference_is_the_stream_down_book_byte_for_byte`),
/// so a rename in `StreamDown::Book` fails that test and not a client.
#[derive(Serialize)]
enum BookRef<'a> {
    Book { k: u32, seq: u64, book: &'a L2Book },
}

/// Write the batch and stamp the write; `false` (after the log line) when the session must end.
fn flush<W: Write>(out: &mut Batch<W>, last_write: &mut Instant, session: u64) -> bool {
    match out.flush() {
        Ok(()) => {
            *last_write = Instant::now();
            true
        }
        Err(e) => {
            tracing::info!(session, error = %e, "vike-gatewayhub stream: write fault; closing");
            false
        }
    }
}

/// The writer loop. Returns when the session is over; the socket is shut down on the way out, which is
/// what wakes the reader thread parked on it.
pub fn run_writer(mut stream: TcpStream, sink: &WireSink, cfg: WriterConfig, session: u64) {
    drive(&mut stream, sink, cfg, session);
    // However the loop ended, nothing more is queued and the reader must wake.
    sink.finish(None);
    let _ = stream.shutdown(Shutdown::Both);
}

/// The loop of [`run_writer`] over any sink of bytes (the tests count its writes).
fn drive<W: Write>(w: W, sink: &WireSink, cfg: WriterConfig, session: u64) {
    let mut out = Batch::new(w, cfg.down_frame_ceiling_bytes);
    let mut last_write = Instant::now();
    let mut lapses = Lapses::new(Instant::now());
    'session: loop {
        // With frames buffered the writer never waits: it takes what is queued NOW and writes the moment
        // the queue is dry, so batching adds no latency to a frame that is the last one queued.
        let wait = if out.is_empty() {
            cfg.heartbeat.saturating_sub(last_write.elapsed())
        } else {
            Duration::ZERO
        };
        match sink.next(wait) {
            Next::Frame(taken) => {
                let Taken { gaps, item, undo } = *taken;
                if !gaps.is_empty() && !lapse(&mut out, &gaps, &mut lapses, &cfg) {
                    break 'session;
                }
                let put = match item {
                    Item::Data { k, seq, payload: Payload::Book(book) } => {
                        out.put_book(k, seq, &book)
                    }
                    item => out.put(&item.into_frame()),
                };
                match put {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                        // Refused before its first byte: the stream is in step, the frame is a hole.
                        tracing::error!(
                            session,
                            error = %e,
                            "vike-gatewayhub stream: a frame could not be sent and was dropped; \
                             a lossless one is disclosed as a Gap"
                        );
                        if let Some(undo) = undo {
                            sink.note_unsendable(undo);
                        }
                    }
                    Err(e) => {
                        tracing::info!(session, error = %e, "vike-gatewayhub stream: write fault; closing");
                        break 'session;
                    }
                }
                if out.full(Instant::now()) && !flush(&mut out, &mut last_write, session) {
                    break 'session;
                }
            }
            Next::Gaps(gaps) => {
                if !lapse(&mut out, &gaps, &mut lapses, &cfg)
                    || !flush(&mut out, &mut last_write, session)
                {
                    break 'session;
                }
            }
            Next::Heartbeat => {
                if out.put(&StreamDown::Heartbeat).is_err()
                    || !flush(&mut out, &mut last_write, session)
                {
                    break 'session;
                }
            }
            Next::Idle => {
                // The queue is dry: whatever is buffered goes out now.
                if !out.is_empty() {
                    if !flush(&mut out, &mut last_write, session) {
                        break 'session;
                    }
                } else if last_write.elapsed() >= cfg.heartbeat
                    && (out.put(&StreamDown::Heartbeat).is_err()
                        || !flush(&mut out, &mut last_write, session))
                {
                    break 'session;
                }
            }
            Next::End(reason) => {
                if let Some(reason) = reason {
                    let _ = out.put(&StreamDown::Bye(reason));
                }
                break 'session;
            }
        }
    }
    // Every way out of the loop but a write fault leaves frames the peer is owed (the `Bye`, the frames
    // before a refused `Gap`): they go out, in order. After a fault the buffer is already empty.
    let _ = out.flush();
}

/// The lapse counter: overflow EPISODES inside the budget window (see the module doc).
#[derive(Debug)]
struct Lapses {
    /// Where the current budget window began.
    window_start: Instant,
    /// Where the current episode began; `None` before the first disclosure of the window.
    episode_start: Option<Instant>,
    /// Episodes counted in the current window.
    count: u64,
}

impl Lapses {
    fn new(now: Instant) -> Self {
        Self { window_start: now, episode_start: None, count: 0 }
    }

    /// A disclosure begins at `now`: the episodes counted so far INCLUDING it. A disclosure inside
    /// `cfg.lapse_episode` of its episode's first one is the same episode and adds nothing.
    fn note(&mut self, now: Instant, cfg: &WriterConfig) -> u64 {
        if now.saturating_duration_since(self.window_start) >= cfg.lapse_window {
            *self = Self::new(now);
        }
        let same_episode = self
            .episode_start
            .is_some_and(|s| now.saturating_duration_since(s) < cfg.lapse_episode);
        if !same_episode {
            self.episode_start = Some(now);
            self.count = self.count.saturating_add(1);
        }
        self.count
    }
}

/// Buffer the gaps of one disclosure and count it (it adds a lapse only when it opens an episode).
/// `false` when the session must end: a gap that cannot be encoded, or the lapse budget crossed (the
/// `TooSlow` bye is buffered and written here, after the gaps that precede it).
fn lapse<W: Write>(
    out: &mut Batch<W>,
    gaps: &[StreamDown],
    lapses: &mut Lapses,
    cfg: &WriterConfig,
) -> bool {
    let count = lapses.note(Instant::now(), cfg);
    for gap in gaps {
        if out.put(gap).is_err() {
            return false;
        }
    }
    if count > cfg.lapse_budget {
        let _ = out.put(&StreamDown::Bye(ByeReason::TooSlow { lapses: count }));
        let _ = out.flush();
        tracing::info!(
            lapses = count,
            "vike-gatewayhub stream: closed (too slow - gap disclosures over budget)"
        );
        return false;
    }
    true
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod writer_tests;
