//! The WIRE SINK: a `vike_data::DataClientSink` whose every verb becomes one frame of ONE stream session's
//! bounded queue (`crate::queue`), and the writer-side API the session's writer thread drains.
//!
//! This is the object that makes "a venue pump never blocks" true. A feed thread calling `quote` takes
//! the session's lock for a few hash operations, assigns the frame its `seq`, queues it (or evicts, or
//! supersedes, per the queue's policy) and returns; nothing on that path serializes a frame, touches a
//! socket or waits for the consumer. The writer thread owns all of that, so a consumer parked in the
//! middle of a frame costs the pump nothing but a growing disclosed `Gap`.
//!
//! # Keys
//!
//! Frames carry a session-assigned key id `k` instead of the venue and symbol (a polymarket token id is
//! 78 digits). One `k` per SYMBOL per session; every frame of every subscription on the symbol shares it,
//! and the `Gap` names the lane. The reader thread binds a key when it starts a subscription
//! ([`DataClientSinkStream::open_sub`]) and announces it in the `SubscribeOk`; a feed that emits a symbol it was not asked
//! for gets a key too, announced by a `Key` frame ahead of the first frame that needs it (at most
//! [`StreamSinkLimits::max_unsolicited`] of them; the rest are dropped and COUNTED). Keys are never reused
//! within a session, and a released key's numbers are freed once its queued frames have drained.
//!
//! # `SubscribeOk` before any frame carrying `k`
//!
//! A venue client may emit a seed from INSIDE `subscribe_*`, before the call returns and the `SubscribeOk` is
//! queued. A key a `Subscribe` is still starting is therefore HELD ([`DataClientSinkStream::open_sub`] holds it): its
//! frames are queued and counted like any other, but the writer does not deliver them until it has
//! dequeued that key's `SubscribeOk`. If the subscription is refused instead ([`DataClientSinkStream::sub_failed`]) the
//! held frames are purged, because the client never learned the key.
//!
//! # The class of a subscription
//!
//! A subscriber may raise the delivery class of a key's lane ([`DataClientSinkStream::set_delivery`]); the policy
//! that makes a raised lane lossless, and why, is `crate::queue`'s module doc. The sink only routes the
//! change in: it applies the floor to the queue under the same lock a push takes and queues the
//! announcement ([`StreamDown::Delivery`]) in the control lane's emission order.
//!
//! # What the sink is not
//!
//! It is not a broker: it makes no decision about WHO wants a frame. The datahub's `DataClientSinkRouting` fans
//! frames to its holders after the hop; this sink carries everything the venue client emits for the
//! symbols the session holds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_data::DataClientSink;
use vike_data::delivery::DataClientSinkDelivery;
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};

use crate::queue::{
    StreamQueue, StreamQueueControl, StreamQueueLimits, StreamQueuePayload, StreamQueueStatistics,
    StreamQueueTake, StreamQueueUndo, StreamQueueVerb,
};
use crate::wire::{StreamByeReason, StreamDown, StreamSubscriptionErrorKind, StreamWordWire};

/// The caps of one sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSinkLimits {
    /// The queue's bounds.
    pub queue: StreamQueueLimits,
    /// The most symbols the session binds at once (subscriptions plus unsolicited keys count against
    /// the first; see `max_unsolicited` for the second).
    pub max_keys: usize,
    /// The most symbols bound WITHOUT a subscription.
    pub max_unsolicited: usize,
}

/// How a key came to be bound for a `Subscribe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSinkOpenKind {
    /// A new key, held until its `SubscribeOk` is dequeued.
    Fresh,
    /// The symbol was already bound by a live subscription.
    Existing,
    /// The symbol was bound by a `Key` frame (a feed emitted it unasked); the subscription adopts it.
    Adopted,
}

/// A key bound for one `Subscribe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSinkOpen {
    /// The key.
    pub k: u32,
    /// How it came to be.
    pub kind: StreamSinkOpenKind,
}

/// What the writer thread gets.
#[derive(Debug)]
pub enum StreamSinkNext {
    /// A frame to write, with the `Gap`s that precede it.
    Frame(Box<StreamQueueTake>),
    /// `Gap`s for holes no queued frame will carry.
    Gaps(Vec<StreamDown>),
    /// Write a `Heartbeat` now (a client `Ping`).
    Heartbeat,
    /// The session is over: write the `Bye` (if any) and stop.
    End(Option<StreamByeReason>),
    /// The wait elapsed with nothing to write.
    Idle,
}

/// What a session did, for its closing log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSinkSummary {
    /// The queue's own counters.
    pub queue: StreamQueueStatistics,
    /// Frames for a symbol the session could not bind (past the unsolicited cap), dropped.
    pub unbound_dropped: u64,
}

struct KeyEntry {
    k: u32,
    /// Bound by a subscription (as opposed to a `Key` frame for an unasked symbol).
    solicited: bool,
    /// Live subscriptions on the symbol.
    subs: usize,
}

struct State {
    queue: StreamQueue,
    keys: HashMap<String, KeyEntry>,
    next_k: u32,
    unsolicited: usize,
    ended: Option<Option<StreamByeReason>>,
    heartbeat_requested: bool,
    writer_waiting: bool,
    unbound_dropped: u64,
}

impl State {
    fn end(&mut self, reason: Option<StreamByeReason>) {
        if self.ended.is_none() {
            self.ended = Some(reason);
        }
    }

    fn alloc_k(&mut self) -> Option<u32> {
        let k = self.next_k;
        self.next_k = self.next_k.checked_add(1)?;
        Some(k)
    }

    /// Queue a control frame, ending the session if the reserved lane is full.
    fn push_control(&mut self, control: StreamQueueControl) {
        if !self.queue.push_control(control) {
            self.end(Some(StreamByeReason::ControlLaneOverflow));
        }
    }
}

struct Shared {
    venue: String,
    limits: StreamSinkLimits,
    state: Mutex<State>,
    cv: Condvar,
    status_tap: OnceLock<StreamSinkStatusTap>,
    /// The subscriber asked for funding frames ([`DataClientSinkStream::carry_funding`]); until then `funding` is
    /// dropped here, so a subscriber that cannot decode the frame is never sent one.
    funding: AtomicBool,
    /// The subscriber asked for binary tick bodies ([`DataClientSinkStream::carry_binary_ticks`]); the writer
    /// reads it per frame, so a session that never asks is written JSON only.
    binary_ticks: AtomicBool,
    /// The subscriber reads the refusal classes ([`DataClientSinkStream::carry_refusal_kinds`]); until then a
    /// not-now or a cap goes out as the `Subscribe` an older subscriber classes by its words (`crate::legacy`).
    refusal_kinds: AtomicBool,
}

/// What [`DataClientSinkStream::tap_status`] takes: the symbol, and the lane and status it was told (`None`: its last subscription ended).
pub type StreamSinkStatusTap = Box<dyn Fn(&str, Option<(&str, StreamStatus)>) + Send + Sync>;

/// The wire sink. Cheap to clone (an `Arc`); the clones are one sink.
#[derive(Clone)]
pub struct DataClientSinkStream {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for DataClientSinkStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataClientSinkStream")
            .field("venue", &self.shared.venue)
            .finish_non_exhaustive()
    }
}

/// What [`DataClientSinkStream::end_on_drop`] returns: ends its session silently when it drops.
#[derive(Debug)]
pub struct EndOnDrop(DataClientSinkStream);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        self.0.finish(None);
    }
}

impl DataClientSinkStream {
    /// A sink for one session of `venue`.
    pub fn new(venue: &str, limits: StreamSinkLimits) -> Self {
        Self {
            shared: Arc::new(Shared {
                venue: venue.to_string(),
                limits,
                state: Mutex::new(State {
                    queue: StreamQueue::new(limits.queue),
                    keys: HashMap::new(),
                    next_k: 1,
                    unsolicited: 0,
                    ended: None,
                    heartbeat_requested: false,
                    writer_waiting: false,
                    unbound_dropped: 0,
                }),
                cv: Condvar::new(),
                status_tap: OnceLock::new(),
                funding: AtomicBool::new(false),
                binary_ticks: AtomicBool::new(false),
                refusal_kinds: AtomicBool::new(false),
            }),
        }
    }

    /// The subscriber reads the refusal classes (`StreamSubscriptionErrorKind::NotNow` and `Cap`): its connection
    /// said it reads the refusal frame (`vike_hub_protocol::handshake::HandshakeFrame::Reads`), and the host calls
    /// this before the session's first subscription. Idempotent; a session that never says so is sent each as the
    /// `Subscribe` its words class (`crate::legacy`).
    pub fn carry_refusal_kinds(&self) {
        self.shared.refusal_kinds.store(true, Ordering::Relaxed);
    }

    /// `err` as this session's subscriber reads it ([`DataClientSinkStream::carry_refusal_kinds`]).
    fn as_read(&self, err: StreamSubscriptionErrorKind) -> StreamSubscriptionErrorKind {
        if self.shared.refusal_kinds.load(Ordering::Relaxed) {
            err
        } else {
            crate::legacy::as_older_kind(err)
        }
    }

    /// The subscriber decodes binary tick bodies (its `StreamUp::BinaryTicks`): from here on the writer writes
    /// the per-tick frames as `crate::binary_ticks` bodies. Idempotent; a session that never says so stays JSON.
    pub fn carry_binary_ticks(&self) {
        self.shared.binary_ticks.store(true, Ordering::Relaxed);
    }

    /// Whether [`DataClientSinkStream::carry_binary_ticks`] was called: the writer's per-frame choice.
    pub fn binary_ticks(&self) -> bool {
        self.shared.binary_ticks.load(Ordering::Relaxed)
    }

    /// The subscriber decodes funding frames (its `StreamUp::Funding`): from here on `funding` is queued
    /// like `mark_tick`. Idempotent; a session that never says so is sent none.
    pub fn carry_funding(&self) {
        self.shared.funding.store(true, Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wake the writer, if it is waiting; drops the guard first.
    fn wake(&self, guard: MutexGuard<'_, State>) {
        let waiting = guard.writer_waiting;
        drop(guard);
        if waiting {
            self.shared.cv.notify_one();
        }
    }

    // ---- The reader side: a subscription's life -------------------------------------------------

    /// Bind (or find) the key for `symbol` ahead of a `subscribe_*` call. A fresh key is HELD.
    pub fn open_sub(&self, symbol: &str) -> Result<StreamSinkOpen, StreamSubscriptionErrorKind> {
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return Err(StreamSubscriptionErrorKind::Draining);
        }
        if let Some(entry) = st.keys.get_mut(symbol) {
            let kind = if entry.solicited {
                StreamSinkOpenKind::Existing
            } else {
                entry.solicited = true;
                st.unsolicited = st.unsolicited.saturating_sub(1);
                StreamSinkOpenKind::Adopted
            };
            return Ok(StreamSinkOpen { k: entry.k, kind });
        }
        let cap = self.shared.limits.max_keys;
        if st.keys.len() >= cap {
            let held = u32::try_from(st.keys.len()).unwrap_or(u32::MAX);
            return Err(StreamSubscriptionErrorKind::Budget {
                scope: "symbols per session".to_string(),
                held,
                cap: u32::try_from(cap).unwrap_or(u32::MAX),
            });
        }
        let Some(k) = st.alloc_k() else {
            return Err(StreamSubscriptionErrorKind::Budget {
                scope: "key ids per session".to_string(),
                held: u32::MAX,
                cap: u32::MAX,
            });
        };
        st.keys.insert(symbol.to_string(), KeyEntry { k, solicited: true, subs: 0 });
        st.queue.hold(k);
        Ok(StreamSinkOpen { k, kind: StreamSinkOpenKind::Fresh })
    }

    /// The in-process `subscribe_*` returned `Ok`: queue the `SubscribeOk`. Dequeuing it releases a held key.
    pub fn sub_ok(&self, sid: u64, symbol: &str, opened: StreamSinkOpen) {
        let mut g = self.lock();
        let st = &mut *g;
        if let Some(entry) = st.keys.get_mut(symbol)
            && entry.k == opened.k
        {
            entry.subs += 1;
        }
        st.push_control(StreamQueueControl::SubscribeOk { sid, k: opened.k });
        self.wake(g);
    }

    /// The in-process `subscribe_*` returned `Err`: queue the `SubscribeError`, and forget a key this call
    /// created (its held frames are purged: the client never learned it).
    pub fn sub_failed(
        &self,
        sid: u64,
        symbol: &str,
        opened: StreamSinkOpen,
        err: StreamSubscriptionErrorKind,
    ) {
        let mut g = self.lock();
        let st = &mut *g;
        match opened.kind {
            StreamSinkOpenKind::Fresh => {
                st.keys.remove(symbol);
                st.queue.purge(opened.k);
            }
            StreamSinkOpenKind::Adopted => {
                if let Some(entry) = st.keys.get_mut(symbol)
                    && entry.k == opened.k
                    && entry.subs == 0
                {
                    entry.solicited = false;
                    st.unsolicited += 1;
                }
            }
            StreamSinkOpenKind::Existing => {}
        }
        let err = self.as_read(err);
        st.push_control(StreamQueueControl::SubscribeError { sid, err });
        self.wake(g);
    }

    /// Queue the refusal of a `Subscribe` that never got a key (a spelling the daemon will not carry, a
    /// subscription cap).
    pub fn refuse(&self, sid: u64, err: StreamSubscriptionErrorKind) {
        let err = self.as_read(err);
        let mut g = self.lock();
        g.push_control(StreamQueueControl::SubscribeError { sid, err });
        self.wake(g);
    }

    /// Answer a repeated `Subscribe` with the key it already has: no new subscription is counted.
    pub fn re_ack(&self, sid: u64, k: u32) {
        let mut g = self.lock();
        g.push_control(StreamQueueControl::SubscribeOk { sid, k });
        self.wake(g);
    }

    /// A subscription on `symbol` ended (its `unsubscribe` returned, so the feed emits nothing more for
    /// it). When it was the symbol's last, the key is released.
    pub fn sub_ended(&self, symbol: &str) {
        let mut g = self.lock();
        let st = &mut *g;
        let release = match st.keys.get_mut(symbol) {
            Some(entry) => {
                entry.subs = entry.subs.saturating_sub(1);
                (entry.subs == 0 && entry.solicited).then_some(entry.k)
            }
            None => None,
        };
        if let Some(k) = release {
            st.keys.remove(symbol);
            st.queue.release_key(k);
            drop(g);
            if let Some(tap) = self.shared.status_tap.get() {
                tap(symbol, None);
            }
        }
    }

    /// **Let the session's host watch its status words**: `tap` hears every `stream_status` as `Some((lane, status))`, and
    /// `None` when a symbol's last subscription ended. Set once. It runs on the feed's thread with no lock of this sink
    /// held, so it stays as cheap as a sink call must (the crate page's rule) and hears nothing per data frame.
    pub fn tap_status(&self, tap: StreamSinkStatusTap) {
        let _ = self.shared.status_tap.set(tap);
    }

    /// How many BOOK frames (an anchor or a delta) of `symbol`'s key this session has lost since the key
    /// was bound: evicted from the bounded queue, refused as non-finite or taken back by the writer as
    /// unsendable. A figure that only grows while the key lives (a key given back and bound again starts
    /// from zero, so compare for DIFFERENCE, not for more), `0` for a symbol with no key.
    ///
    /// A lost book frame is disclosed to the receiver as a `Gap`, but the receiver has no way to ask for a
    /// resync (the tube has no such frame). This is the SENDER's half of the cure, for a sender that
    /// diffs (`vike_gatewayhub`'s `FeedTickBookEncoder`): when the figure moved, the receiver's book is no longer
    /// the one the sender last encoded, so the next frame must be an anchor, not another delta.
    pub fn book_losses(&self, symbol: &str) -> u64 {
        let g = self.lock();
        let Some(entry) = g.keys.get(symbol) else { return 0 };
        g.queue.lost(entry.k, StreamQueueVerb::Book)
            + g.queue.lost(entry.k, StreamQueueVerb::BookDelta)
    }

    /// Set the delivery class FLOOR of `lane` on key `k` (a subscriber's `Subscribe.class` or `Delivery` frame;
    /// [`DataClientSinkDelivery::Conflatable`] clears it). Applies to the frames pushed from here on; the ones already
    /// queued keep the rule they entered under. A change of the lane's class is announced to the
    /// receiver in frame order ([`StreamDown::Delivery`]); a floor that changes nothing (a lane of verbs
    /// that are already lossless, a bar lane) says nothing. Returns the lane's previous floor, so a
    /// caller whose subscription then failed can put it back.
    pub fn set_delivery(
        &self,
        k: u32,
        lane: StreamLabel,
        floor: DataClientSinkDelivery,
    ) -> DataClientSinkDelivery {
        let mut g = self.lock();
        let st = &mut *g;
        let previous = st.queue.floor_of(k, lane);
        if st.ended.is_some() {
            return previous;
        }
        if let Some(class) = st.queue.set_floor(k, lane, floor) {
            st.push_control(StreamQueueControl::Delivery { k, lane, class });
        }
        self.wake(g);
        previous
    }

    /// Answer a client `Ping`: the writer writes a `Heartbeat` at once.
    pub fn request_heartbeat(&self) {
        let mut g = self.lock();
        g.heartbeat_requested = true;
        self.wake(g);
    }

    /// End the session. `Some(reason)` is written as the last frame (`Bye`); `None` ends it silently
    /// (the socket is already gone). Idempotent: the first reason stands. The backlog is discarded.
    pub fn finish(&self, reason: Option<StreamByeReason>) {
        let mut g = self.lock();
        g.end(reason);
        drop(g);
        self.shared.cv.notify_all();
    }

    /// A guard that ends the session silently ([`DataClientSinkStream::finish`] with `None`) when it drops: the
    /// reader holds it, so the session's queue ends when the reader leaves by ANY path, a panic
    /// included. The writer thread holds a clone of the socket, so without it a reader that unwound
    /// would leave a writer heartbeating on an open socket for a session nobody serves: a zombie the
    /// subscriber would believe in.
    #[must_use = "the session ends when the guard drops: bind it for the reader's life"]
    pub fn end_on_drop(&self) -> EndOnDrop {
        EndOnDrop(self.clone())
    }

    /// Whether the session has been ended.
    pub fn is_ended(&self) -> bool {
        self.lock().ended.is_some()
    }

    /// What the session did.
    pub fn summary(&self) -> StreamSinkSummary {
        let g = self.lock();
        StreamSinkSummary { queue: g.queue.stats(), unbound_dropped: g.unbound_dropped }
    }

    /// How many data frames and bytes are queued right now (the tests' view of the bounds).
    pub fn queued(&self) -> (usize, usize) {
        let g = self.lock();
        (g.queue.data_frames(), g.queue.data_bytes())
    }

    /// Whether the writer is parked: queue dry, every owed `Gap` disclosed, its batch written.
    #[cfg(test)]
    pub(crate) fn writer_parked(&self) -> bool {
        self.lock().writer_waiting
    }

    // ---- The writer side ------------------------------------------------------------------------

    /// The next thing the writer should do, waiting up to `wait` for one.
    pub fn next(&self, wait: Duration) -> StreamSinkNext {
        let deadline = Instant::now() + wait;
        let mut g = self.lock();
        loop {
            if let Some(end) = g.ended {
                return StreamSinkNext::End(end);
            }
            if g.heartbeat_requested {
                g.heartbeat_requested = false;
                return StreamSinkNext::Heartbeat;
            }
            if let Some(taken) = g.queue.take() {
                return StreamSinkNext::Frame(Box::new(taken));
            }
            let gaps = g.queue.tail_gaps();
            if !gaps.is_empty() {
                return StreamSinkNext::Gaps(gaps);
            }
            let now = Instant::now();
            if now >= deadline {
                return StreamSinkNext::Idle;
            }
            g.writer_waiting = true;
            let (guard, _) = self
                .shared
                .cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            g = guard;
            g.writer_waiting = false;
        }
    }

    /// The writer found a lossless frame it could not send (over the frame ceiling): it is a hole.
    pub fn note_unsendable(&self, undo: StreamQueueUndo) {
        self.lock().queue.note_unsendable(undo);
    }

    // ---- The pump side --------------------------------------------------------------------------

    /// The key for `symbol`, binding an unasked one (and queuing its `Key` frame) if there is room.
    fn bind(&self, st: &mut State, symbol: &str) -> Option<u32> {
        if let Some(entry) = st.keys.get(symbol) {
            return Some(entry.k);
        }
        if st.unsolicited >= self.shared.limits.max_unsolicited {
            return None;
        }
        let k = st.alloc_k()?;
        st.keys.insert(symbol.to_string(), KeyEntry { k, solicited: false, subs: 0 });
        st.unsolicited += 1;
        st.push_control(StreamQueueControl::Key { k, symbol: symbol.to_string() });
        Some(k)
    }

    fn push_data(&self, symbol: &str, payload: StreamQueuePayload) {
        // The finite scan of a folded `Book` walks every level: done here, outside the session lock the
        // pump and the writer share.
        let finite = payload.is_finite();
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return;
        }
        let Some(k) = self.bind(st, symbol) else {
            st.unbound_dropped += 1;
            return;
        };
        st.queue.push_data_checked(k, payload, finite);
        self.wake(g);
    }

    fn push_status(&self, symbol: &str, control: impl FnOnce(u32) -> StreamQueueControl) {
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return;
        }
        let Some(k) = self.bind(st, symbol) else {
            st.unbound_dropped += 1;
            return;
        };
        st.push_control(control(k));
        self.wake(g);
    }
}

impl DataClientSink for DataClientSinkStream {
    fn seed_bars(&self, _venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push_data(symbol, StreamQueuePayload::Seed { interval: interval.to_string(), bars });
    }

    fn close_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push_data(
            symbol,
            StreamQueuePayload::BarClose { interval: interval.to_string(), bar },
        );
    }

    fn forming_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push_data(symbol, StreamQueuePayload::Forming { interval: interval.to_string(), bar });
    }

    fn mark_tick(&self, _venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push_data(symbol, StreamQueuePayload::Mark { price: px, timestamp: ts });
    }

    fn bar_close_tick(&self, _venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push_data(symbol, StreamQueuePayload::BarCloseTick { price: px, timestamp: ts });
    }

    fn l2_snapshot(
        &self,
        _venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        self.push_data(symbol, StreamQueuePayload::Depth { tick_size, bids, asks, timestamp: ts });
    }

    fn quote(&self, _venue: &str, symbol: &str, quote: TickQuote) {
        self.push_data(symbol, StreamQueuePayload::Quote(quote));
    }

    fn trade(&self, _venue: &str, symbol: &str, trade: TickTrade) {
        self.push_data(symbol, StreamQueuePayload::Trade(trade));
    }

    fn book(&self, _venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.push_data(symbol, StreamQueuePayload::Book(book));
    }

    fn book_update(&self, _venue: &str, symbol: &str, update: L2BookUpdate) {
        self.push_data(symbol, StreamQueuePayload::BookDelta(update));
    }

    fn stream_status(&self, _venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if let Some(tap) = self.shared.status_tap.get() {
            tap(symbol, Some((stream, status)));
        }
        let stream = StreamWordWire::from_word(stream);
        self.push_status(symbol, move |k| StreamQueueControl::Status { k, stream, status });
    }

    fn data_delay(&self, _venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        let stream = StreamWordWire::from_word(stream);
        self.push_status(symbol, move |k| StreamQueueControl::Delay { k, stream, delay });
    }

    fn funding(&self, _venue: &str, symbol: &str, funding: FundingRate) {
        if self.shared.funding.load(Ordering::Relaxed) {
            self.push_data(symbol, StreamQueuePayload::Funding(funding));
        }
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod sink_tests;
