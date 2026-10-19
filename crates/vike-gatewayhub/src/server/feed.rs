//! The `feed` class: the Deribit options streams (`markprice.options` and the per-instrument `ticker`),
//! pushed to a client that says nothing.
//!
//! `OptionsFeedOpen` is a MODE SWITCH like `DataOpen`: after the `OptionsFeedOpened` reply the socket
//! carries `OptionsFeedDown` frames only. The focus set is named by the client and therefore CAPPED here
//! (`OptionsFeedSpec::refusal`: decision 0052's bound, now client-named), and the set is fixed for the
//! life of the socket; a different set is a new socket.
//!
//! # Overflow
//!
//! A feed row is a latest-wins snapshot per instrument, and the datahub's options book has no gap
//! concept (the in-process feeds reconnect silently too), so the queue here is a plain bounded FIFO that
//! drops its OLDEST rows when a laggard cannot keep up. It is bounded in frames AND in bytes (a
//! `Marks` frame is a whole venue batch, so a frame count alone would let a parked consumer hold a
//! queue of whole chains). ⚠ It is still never silent: the drops are
//! counted and logged, and a feed that keeps losing rows (more than the lapse budget inside the lapse
//! window) is ended with `ByeReason::TooSlow`, whose reconnect re-reads current state. A row with a
//! non-finite float is dropped and counted too: JSON writes it as `null`, which would reach the book as
//! an ABSENT value, a different fact from the one the venue sent.
//!
//! # Liveness
//!
//! As for a stream: no idle read timeout, a `Heartbeat` after the stated period of silence, the write
//! timeout for a peer that stopped reading, and the closing `Bye` on the daemon's stop.

use std::collections::VecDeque;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    FEATURE_GW_OPTIONS, GW_DOWN_FRAME_CEILING_BYTES, OptionsFeedDown, OptionsFeedSpec, Response,
};
use vike_market_stream::frame::write_capped;
use vike_market_stream::wire::ByeReason;
use vike_marketdata::OptionTicker;

use super::features::not_served;
use super::frame::{Ready, wait_readable};
use super::shared::{After, Conn};
use crate::rest::OptionsFeedSink;

/// How often the reader wakes to look at the flags when the (silent) client sends nothing.
const TICK: Duration = Duration::from_secs(1);

/// Whether every float of a row is finite.
fn ticker_is_finite(t: &OptionTicker) -> bool {
    [
        t.bid,
        t.ask,
        t.bid_size,
        t.ask_size,
        t.last,
        t.mark,
        t.mark_iv,
        t.bid_iv,
        t.ask_iv,
        t.delta,
        t.gamma,
        t.vega,
        t.theta,
        t.rho,
        t.open_interest,
        t.volume,
        t.underlying_price,
        t.index_price,
    ]
    .iter()
    .all(|v| v.is_none_or(f64::is_finite))
}

/// A rough per-frame fixed cost (the enum, the queue's bookkeeping) added to every frame's payload.
const FRAME_OVERHEAD: usize = 64;

/// What the queue's byte bound counts a frame as: its in-memory size (every row's struct plus its
/// instrument name), the way `vike_market_stream::queue::Payload::cost` counts a stream frame. A bound, not a measurement.
fn frame_cost(frame: &OptionsFeedDown) -> usize {
    let row = |t: &OptionTicker| std::mem::size_of::<OptionTicker>() + t.symbol.len();
    FRAME_OVERHEAD
        + match frame {
            OptionsFeedDown::Marks(rows) => rows.iter().map(row).sum::<usize>(),
            OptionsFeedDown::Ticker(t) => row(t),
            OptionsFeedDown::Heartbeat | OptionsFeedDown::Bye(_) => 0,
        }
}

/// Ends the feed's queue when the reader leaves by any path (see `stream`'s `EndOnDrop`).
struct EndFeedOnDrop(Arc<FeedQueue>);

impl Drop for EndFeedOnDrop {
    fn drop(&mut self) {
        self.0.finish(None);
    }
}

enum FeedNext {
    /// A frame, and whether rows were dropped since the previous one (a lapse).
    Frame(OptionsFeedDown, bool),
    Idle,
    End(Option<ByeReason>),
}

struct FeedState {
    /// Each frame with its counted cost (`frame_cost`).
    frames: VecDeque<(OptionsFeedDown, usize)>,
    /// The sum of the queued frames' costs.
    bytes: usize,
    ended: Option<Option<ByeReason>>,
    dropped_since: u64,
    waiting: bool,
}

/// The bounded FIFO between the lane's feed threads and the feed's writer.
pub struct FeedQueue {
    cap: usize,
    byte_cap: usize,
    state: Mutex<FeedState>,
    cv: Condvar,
    dropped: AtomicU64,
    refused: AtomicU64,
}

impl FeedQueue {
    /// An empty queue holding at most `cap` frames and about `byte_cap` bytes (`frame_cost`).
    pub fn new(cap: usize, byte_cap: usize) -> Arc<Self> {
        Arc::new(Self {
            cap,
            byte_cap,
            state: Mutex::new(FeedState {
                frames: VecDeque::new(),
                bytes: 0,
                ended: None,
                dropped_since: 0,
                waiting: false,
            }),
            cv: Condvar::new(),
            dropped: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue a frame, dropping the oldest while the queue is over either bound. Never blocks. A frame
    /// larger than the whole byte bound is admitted alone (everything older goes): it is bounded by the
    /// frame ceiling the writer enforces, and refusing it would be a silent loss.
    pub fn push(&self, frame: OptionsFeedDown) {
        let cost = frame_cost(&frame);
        let mut g = self.lock();
        if g.ended.is_some() {
            return;
        }
        while g.frames.len() >= self.cap
            || (!g.frames.is_empty() && g.bytes.saturating_add(cost) > self.byte_cap)
        {
            let Some((_, old)) = g.frames.pop_front() else { break };
            g.bytes = g.bytes.saturating_sub(old);
            g.dropped_since += 1;
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        g.bytes = g.bytes.saturating_add(cost);
        g.frames.push_back((frame, cost));
        let waiting = g.waiting;
        drop(g);
        if waiting {
            self.cv.notify_one();
        }
    }

    /// End the feed; the first reason stands and the backlog is discarded.
    pub fn finish(&self, reason: Option<ByeReason>) {
        let mut g = self.lock();
        if g.ended.is_none() {
            g.ended = Some(reason);
        }
        drop(g);
        self.cv.notify_all();
    }

    /// Whether the feed has been ended.
    pub fn is_ended(&self) -> bool {
        self.lock().ended.is_some()
    }

    /// Rows dropped for want of room.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Rows refused for a non-finite float.
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    fn next(&self, wait: Duration) -> FeedNext {
        let deadline = Instant::now() + wait;
        let mut g = self.lock();
        loop {
            if let Some(end) = g.ended {
                return FeedNext::End(end);
            }
            if let Some((frame, cost)) = g.frames.pop_front() {
                g.bytes = g.bytes.saturating_sub(cost);
                let lapsed = std::mem::take(&mut g.dropped_since) > 0;
                return FeedNext::Frame(frame, lapsed);
            }
            let now = Instant::now();
            if now >= deadline {
                return FeedNext::Idle;
            }
            g.waiting = true;
            let (guard, _) =
                self.cv.wait_timeout(g, deadline - now).unwrap_or_else(PoisonError::into_inner);
            g = guard;
            g.waiting = false;
        }
    }
}

/// The lane's end of the queue.
struct QueueSink(Arc<FeedQueue>);

impl OptionsFeedSink for QueueSink {
    fn marks(&self, rows: Vec<OptionTicker>) {
        if rows.iter().all(ticker_is_finite) {
            self.0.push(OptionsFeedDown::Marks(rows));
        } else {
            self.0.refused.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn ticker(&self, row: OptionTicker) {
        if ticker_is_finite(&row) {
            self.0.push(OptionsFeedDown::Ticker(Box::new(row)));
        } else {
            self.0.refused.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct FeedWriterConfig {
    heartbeat: Duration,
    lapse_budget: u64,
    lapse_window: Duration,
}

fn run_feed_writer(mut stream: TcpStream, queue: &FeedQueue, cfg: &FeedWriterConfig) {
    let mut last_write = Instant::now();
    let mut window_start = Instant::now();
    let mut lapses: u64 = 0;
    let emit = |stream: &mut TcpStream, frame: &OptionsFeedDown| {
        write_capped(stream, frame, GW_DOWN_FRAME_CEILING_BYTES)
    };
    loop {
        let wait = cfg.heartbeat.saturating_sub(last_write.elapsed());
        match queue.next(wait) {
            FeedNext::Frame(frame, lapsed) => {
                if lapsed {
                    if window_start.elapsed() >= cfg.lapse_window {
                        lapses = 0;
                        window_start = Instant::now();
                    }
                    lapses += 1;
                    if lapses > cfg.lapse_budget {
                        let _ =
                            emit(&mut stream, &OptionsFeedDown::Bye(ByeReason::TooSlow { lapses }));
                        break;
                    }
                }
                match emit(&mut stream, &frame) {
                    Ok(()) => last_write = Instant::now(),
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        tracing::error!(error = %e, "vike-gatewayhub feed: a row could not be framed; dropped");
                    }
                    Err(_) => break,
                }
            }
            FeedNext::Idle => {
                if last_write.elapsed() >= cfg.heartbeat {
                    if emit(&mut stream, &OptionsFeedDown::Heartbeat).is_err() {
                        break;
                    }
                    last_write = Instant::now();
                }
            }
            FeedNext::End(reason) => {
                if let Some(reason) = reason {
                    let _ = emit(&mut stream, &OptionsFeedDown::Bye(reason));
                }
                break;
            }
        }
    }
    queue.finish(None);
    let _ = stream.shutdown(Shutdown::Both);
}

/// `OptionsFeedOpen { feed }`: refuse, or open the feed and run it to its end.
pub fn serve(conn: &mut Conn, spec: &OptionsFeedSpec) -> After {
    let shared = Arc::clone(&conn.shared);
    if !shared.rest.options {
        return refuse(conn, not_served("OptionsFeedOpen", FEATURE_GW_OPTIONS));
    }
    if let Some(why) = spec.refusal() {
        return refuse(conn, why);
    }
    let cap = shared.config.limits.max_options_feeds;
    let Some(slot) = shared.stats.options_feeds.try_enter(cap) else {
        return refuse(
            conn,
            format!(
                "gatewayhub: already serving {cap} options feeds (the daemon's cap); transient, \
                 retried on the caller's next pass"
            ),
        );
    };
    let queue = FeedQueue::new(
        shared.config.limits.feed_queue_frames,
        shared.config.limits.feed_queue_bytes,
    );
    let _end = EndFeedOnDrop(Arc::clone(&queue));
    let sink: Arc<dyn OptionsFeedSink> = Arc::new(QueueSink(Arc::clone(&queue)));
    let mut handle = match shared.lanes.rest.open_options_feed(&conn.memo, spec, sink) {
        Ok(handle) => handle,
        Err(text) => return refuse(conn, text),
    };
    let writer_socket = match conn.stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            handle.shutdown();
            return refuse(conn, format!("gatewayhub: could not open the feed: {e}"));
        }
    };
    let heartbeat = shared.config.timings.stream_heartbeat;
    let opened = Response::OptionsFeedOpened {
        epoch: shared.stats.epoch,
        heartbeat_ms: u64::try_from(heartbeat.as_millis()).unwrap_or(u64::MAX),
    };
    if conn.reply(&opened).is_err() {
        handle.shutdown();
        return After::Close;
    }
    tracing::info!(peer = ?conn.peer, "vike-gatewayhub feed: opened");
    let cfg = FeedWriterConfig {
        heartbeat,
        lapse_budget: shared.config.limits.lapse_budget,
        lapse_window: shared.config.limits.lapse_window,
    };
    let writer_queue = Arc::clone(&queue);
    let writer = std::thread::Builder::new()
        .name("gwh-fd-wr".into())
        .spawn(move || run_feed_writer(writer_socket, &writer_queue, &cfg));
    let writer = match writer {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!(error = %e, "vike-gatewayhub feed: could not start the writer");
            handle.shutdown();
            return After::Close;
        }
    };
    let stop_queue = Arc::clone(&queue);
    conn.ctl.set_wake(move || stop_queue.finish(Some(ByeReason::ServerStopping)));

    // The client is silent: anything it sends, and its going away, end the feed.
    loop {
        if conn.ctl.is_stopping() || queue.is_ended() {
            break;
        }
        match wait_readable(&conn.stream, TICK) {
            Ok(Ready::Timeout) => {}
            Ok(Ready::Data) => {
                tracing::warn!(peer = ?conn.peer, "vike-gatewayhub feed: the client sent a frame on a push socket; closing");
                break;
            }
            Ok(Ready::Eof) | Err(_) => break,
        }
    }
    queue.finish(None);
    let _ = writer.join();
    handle.shutdown();
    tracing::info!(
        peer = ?conn.peer,
        dropped = queue.dropped(),
        refused_nonfinite = queue.refused(),
        "vike-gatewayhub feed: closed"
    );
    drop(slot);
    After::Close
}

fn refuse(conn: &mut Conn, text: String) -> After {
    match conn.reply_error(text) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod feed_tests;
